use crate::config::OpsConfig;
use crate::topology::FleetFile;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct JobState {
    pub id: u64,
    pub kind: String,
    pub target: String,
    pub running: bool,
    pub ok: Option<bool>,
    pub log: String,
}

#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<Option<JobState>>>,
}

impl Jobs {
    pub async fn snapshot(&self) -> Option<JobState> {
        self.inner.lock().await.clone()
    }

    async fn begin(&self, kind: &str, target: &str) -> Result<u64> {
        let mut g = self.inner.lock().await;
        if g.as_ref().map(|j| j.running).unwrap_or(false) {
            return Err(anyhow!("a job is already running"));
        }
        let id = g.as_ref().map(|j| j.id + 1).unwrap_or(1);
        *g = Some(JobState {
            id,
            kind: kind.into(),
            target: target.into(),
            running: true,
            ok: None,
            log: String::new(),
        });
        Ok(id)
    }

    async fn finish(&self, result: Result<String>) {
        let mut g = self.inner.lock().await;
        if let Some(j) = g.as_mut() {
            j.running = false;
            match result {
                Ok(msg) => {
                    j.ok = Some(true);
                    if !msg.is_empty() {
                        if !j.log.ends_with('\n') && !j.log.is_empty() {
                            j.log.push('\n');
                        }
                        j.log.push_str(&msg);
                        if !j.log.ends_with('\n') {
                            j.log.push('\n');
                        }
                    }
                    j.log.push_str("OK\n");
                }
                Err(e) => {
                    j.ok = Some(false);
                    j.log.push_str(&format!("ERROR {e:#}\n"));
                }
            }
        }
    }

    pub async fn start_blocking<F>(&self, kind: &str, target: &str, f: F) -> Result<u64>
    where
        F: FnOnce() -> Result<String> + Send + 'static,
    {
        let id = self.begin(kind, target).await?;
        let jobs = self.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(f)
                .await
                .unwrap_or_else(|e| Err(anyhow!("job join: {e}")));
            jobs.finish(result).await;
        });
        Ok(id)
    }

    pub async fn start_async<F, Fut>(&self, kind: &str, target: &str, f: F) -> Result<u64>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<String>> + Send,
    {
        let id = self.begin(kind, target).await?;
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.finish(f().await).await;
        });
        Ok(id)
    }

    pub async fn start_reclaim(
        &self,
        cfg: OpsConfig,
        fleet: FleetFile,
        fqdn: String,
        ip: Option<String>,
    ) -> Result<u64> {
        let mut g = self.inner.lock().await;
        if g.as_ref().map(|j| j.running).unwrap_or(false) {
            return Err(anyhow!("a job is already running"));
        }
        let id = g.as_ref().map(|j| j.id + 1).unwrap_or(1);
        *g = Some(JobState {
            id,
            kind: "reclaim".into(),
            target: fqdn.clone(),
            running: true,
            ok: None,
            log: String::new(),
        });
        drop(g);
        let jobs = self.clone();
        tokio::spawn(async move {
            let result = run_reclaim(&cfg, &fleet, &fqdn, ip.as_deref(), &jobs).await;
            jobs.finish(result).await;
        });
        Ok(id)
    }

    async fn append(&self, line: &str) {
        let mut g = self.inner.lock().await;
        if let Some(j) = g.as_mut() {
            j.log.push_str(line);
            if !line.ends_with('\n') {
                j.log.push('\n');
            }
        }
    }
}

async fn run_reclaim(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    fqdn: &str,
    ip: Option<&str>,
    jobs: &Jobs,
) -> Result<String> {
    let node = fleet
        .node(fqdn)
        .ok_or_else(|| anyhow!("unknown node {fqdn}"))?;
    let target = node
        .deploy_target
        .clone()
        .ok_or_else(|| anyhow!("node {fqdn} has no deployTarget in ops/fleet.json"))?;
    let checkout = cfg
        .checkout_path()
        .ok_or_else(|| anyhow!("no gateway checkout (set gateway_checkout)"))?;
    let script = checkout.join("scripts/relay-master-deploy.sh");
    if !script.is_file() {
        anyhow::bail!("missing {}", script.display());
    }
    jobs.append(&format!("reclaim {fqdn} target={target}"))
        .await;
    let mut cmd = Command::new("bash");
    cmd.arg(script.as_os_str())
        .arg(format!("--target={target}"))
        .arg("--install-only")
        .arg("--build")
        .current_dir(&checkout)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(ip) = ip {
        let slug = fqdn.split('.').next().unwrap_or("node").to_uppercase();
        cmd.env(format!("SPOT_{slug}_IP"), ip);
        cmd.env("RELAY_OPS_REPAIR_IP", ip);
    }
    if let Some(key) = cfg.identity_path() {
        cmd.env("RELAY_SSH_IDENTITY", key);
    }
    if let Some(user) = node.ssh_user.as_ref().or(cfg.ssh_user.as_ref()) {
        cmd.env("RELAY_SSH_USER", user);
    }
    let mut child = cmd.spawn()?;
    if let Some(stdout) = child.stdout.take() {
        let jobs2 = jobs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                jobs2.append(&line).await;
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let jobs2 = jobs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                jobs2.append(&line).await;
            }
        });
    }
    let status = child.wait().await?;
    if status.success() {
        Ok("deploy finished".into())
    } else {
        Err(anyhow!("deploy exited {status}"))
    }
}

pub fn reset_known_hosts(host: &str, ip: Option<&str>) -> Result<String> {
    let mut log = Vec::new();
    for target in [Some(host), ip].into_iter().flatten() {
        let out = std::process::Command::new("ssh-keygen")
            .args(["-R", target])
            .output();
        match out {
            Ok(o) => {
                log.push(format!(
                    "ssh-keygen -R {target}: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            Err(e) => log.push(format!("ssh-keygen -R {target}: {e}")),
        }
    }
    Ok(log.join("\n"))
}

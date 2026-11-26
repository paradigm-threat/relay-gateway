use crate::config::OpsConfig;
use crate::topology::FleetFile;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
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

    /// Run a short local action and keep the result in the job log (so the UI
    /// snapshot does not clobber it with a previous Repair/SSH failure).
    pub async fn record_finished(
        &self,
        kind: &str,
        target: &str,
        result: Result<String>,
    ) -> Result<u64> {
        let id = self.begin(kind, target).await?;
        self.finish(result).await;
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

pub fn reset_known_hosts(host: &str, ips: &[String]) -> Result<String> {
    reset_known_hosts_file(&user_known_hosts_path()?, host, ips)
}

fn user_known_hosts_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot locate home directory"))?;
    Ok(home.join(".ssh/known_hosts"))
}

fn known_hosts_targets(host: &str, ips: &[String]) -> Result<Vec<String>> {
    let host = host.trim();
    if host.is_empty() {
        anyhow::bail!("fqdn required");
    }
    let mut targets = vec![host.to_string()];
    for ip in ips {
        let ip = ip.trim();
        if ip.is_empty() || targets.iter().any(|t| t == ip) {
            continue;
        }
        targets.push(ip.to_string());
    }
    Ok(targets)
}

fn reset_known_hosts_file(path: &Path, host: &str, ips: &[String]) -> Result<String> {
    let targets = known_hosts_targets(host, ips)?;
    let mut log = vec![format!(
        "known_hosts {} (user file; ssh-keygen -R, no sudo)",
        path.display()
    )];
    for target in &targets {
        let out = std::process::Command::new("ssh-keygen")
            .args(["-f", &path.display().to_string(), "-R", target])
            .output();
        match out {
            Ok(o) => {
                let detail = String::from_utf8_lossy(&o.stderr);
                let detail = detail.trim();
                if !o.status.success() {
                    let hint = if detail.to_ascii_lowercase().contains("permission denied") {
                        format!(
                            "\n{} is not writable. This is your user known_hosts — do not sudo. Fix ownership (chown \"$USER\" '{}') and retry.",
                            path.display(),
                            path.display()
                        )
                    } else {
                        String::new()
                    };
                    anyhow::bail!("ssh-keygen -R {target} failed: {detail}{hint}");
                }
                log.push(format!("ssh-keygen -R {target}: {detail}"));
            }
            Err(e) => anyhow::bail!("ssh-keygen -R {target}: {e}"),
        }
    }
    Ok(log.join("\n"))
}

/// Reset FQDN + IPs for every fleet node. Continues after a per-node failure so
/// the job log still lists the rest.
pub fn reset_known_hosts_many(nodes: &[(String, Vec<String>)]) -> Result<String> {
    if nodes.is_empty() {
        anyhow::bail!("no fleet nodes");
    }
    let mut logs = Vec::new();
    let mut failed = false;
    for (fqdn, ips) in nodes {
        match reset_known_hosts(fqdn, ips) {
            Ok(m) => logs.push(m),
            Err(e) => {
                failed = true;
                logs.push(format!("{fqdn}: ERROR {e:#}"));
            }
        }
    }
    let text = logs.join("\n");
    if failed {
        anyhow::bail!("{text}");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn known_hosts_targets_dedupes_and_skips_blank() {
        let ips = vec![
            "  ".into(),
            "10.0.0.1".into(),
            "node.example".into(),
            "10.0.0.1".into(),
            "10.0.0.2".into(),
        ];
        let t = known_hosts_targets("node.example", &ips).unwrap();
        assert_eq!(
            t,
            vec![
                "node.example".to_string(),
                "10.0.0.1".to_string(),
                "10.0.0.2".to_string()
            ]
        );
    }

    #[test]
    fn reset_known_hosts_file_removes_fqdn_and_ip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            "old.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyForUnitTestingAAAA\n\
             192.0.2.9 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyForUnitTestingBBBB\n\
             keep.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustAFakeKeyForUnitTestingCCCC"
        )
        .unwrap();
        drop(f);
        let ips = vec!["192.0.2.9".into()];
        reset_known_hosts_file(&path, "old.example", &ips).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("old.example"), "{body}");
        assert!(!body.contains("192.0.2.9"), "{body}");
        assert!(body.contains("keep.example"), "{body}");
    }
}

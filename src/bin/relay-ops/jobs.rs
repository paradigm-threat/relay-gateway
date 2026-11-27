use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

#[derive(Default)]
struct JobsInner {
    next_id: u64,
    /// Active + recently finished jobs (newest retained for the UI log).
    by_id: HashMap<u64, JobState>,
}

#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<JobsInner>>,
}

impl Jobs {
    /// Most recent job (running preferred, else highest id) — back-compat for `snapshot.job`.
    pub async fn snapshot(&self) -> Option<JobState> {
        let g = self.inner.lock().await;
        g.by_id
            .values()
            .filter(|j| j.running)
            .max_by_key(|j| j.id)
            .cloned()
            .or_else(|| g.by_id.values().max_by_key(|j| j.id).cloned())
    }

    /// All jobs, newest first (for multi-job UI log).
    pub async fn snapshot_all(&self) -> Vec<JobState> {
        let g = self.inner.lock().await;
        let mut v: Vec<_> = g.by_id.values().cloned().collect();
        v.sort_by(|a, b| b.id.cmp(&a.id));
        v
    }

    async fn begin(&self, kind: &str, target: &str) -> Result<u64> {
        let mut g = self.inner.lock().await;
        let target_key = target.trim();
        if let Some(existing) = g
            .by_id
            .values()
            .find(|j| j.running && j.target.trim() == target_key)
        {
            return Err(anyhow!(
                "a job is already running for {} (#{}, {})",
                existing.target,
                existing.id,
                existing.kind
            ));
        }
        g.next_id = g.next_id.saturating_add(1).max(1);
        let id = g.next_id;
        g.by_id.insert(
            id,
            JobState {
                id,
                kind: kind.into(),
                target: target.into(),
                running: true,
                ok: None,
                log: String::new(),
            },
        );
        // Bound retention so the map cannot grow without limit.
        const KEEP: usize = 40;
        if g.by_id.len() > KEEP {
            let mut finished: Vec<u64> = g
                .by_id
                .values()
                .filter(|j| !j.running)
                .map(|j| j.id)
                .collect();
            finished.sort_unstable();
            let drop_n = g.by_id.len() - KEEP;
            for id in finished.into_iter().take(drop_n) {
                g.by_id.remove(&id);
            }
        }
        Ok(id)
    }

    async fn finish(&self, id: u64, result: Result<String>) {
        let mut g = self.inner.lock().await;
        if let Some(j) = g.by_id.get_mut(&id) {
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
            jobs.finish(id, result).await;
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
            jobs.finish(id, f().await).await;
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
        self.finish(id, result).await;
        Ok(id)
    }

    #[allow(dead_code)]
    async fn append(&self, id: u64, line: &str) {
        let mut g = self.inner.lock().await;
        if let Some(j) = g.by_id.get_mut(&id) {
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
    use std::time::Duration;

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

    #[tokio::test]
    async fn concurrent_jobs_allowed_for_different_targets() {
        let jobs = Jobs::default();
        let a = jobs
            .start_async("sync", "mesh1.example", || async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok("a".into())
            })
            .await
            .unwrap();
        let b = jobs
            .start_async("sync", "mesh2.example", || async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok("b".into())
            })
            .await
            .unwrap();
        assert_ne!(a, b);
        let all = jobs.snapshot_all().await;
        assert_eq!(all.iter().filter(|j| j.running).count(), 2);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let all = jobs.snapshot_all().await;
        assert!(all.iter().all(|j| !j.running));
        assert!(all.iter().all(|j| j.ok == Some(true)));
    }

    #[tokio::test]
    async fn same_target_rejects_second_running_job() {
        let jobs = Jobs::default();
        jobs.start_async("sync", "mesh1.example", || async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok("a".into())
        })
        .await
        .unwrap();
        let err = jobs
            .start_async("sync", "mesh1.example", || async { Ok("b".into()) })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "unexpected: {err:#}"
        );
    }
}

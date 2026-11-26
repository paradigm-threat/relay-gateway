use crate::config::OpsConfig;
use crate::inventory::Inventory;
use crate::topology::FleetFile;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Output, Stdio};

enum HostKeyMode {
    AcceptNew,
    Strict,
}

#[derive(Debug, Clone, Default)]
pub struct SshDiag {
    pub ssh_ok: Option<bool>,
    pub host_key_changed: bool,
    pub install_present: Option<bool>,
}

pub struct SshCtx<'a> {
    pub cfg: &'a OpsConfig,
    pub fleet: &'a FleetFile,
    pub interactive: bool,
}

impl SshCtx<'_> {
    pub fn require_admin(&self) -> Result<()> {
        let inv = Inventory::collect(self.cfg);
        if !inv.admin_ready {
            anyhow::bail!("needs ssh + identity (Admin inventory)");
        }
        Ok(())
    }

    fn identity(&self) -> Option<std::path::PathBuf> {
        Inventory::collect(self.cfg)
            .identity_path
            .map(std::path::PathBuf::from)
            .or_else(|| self.cfg.identity_path())
    }

    fn ssh_base_opts(
        &self,
        fqdn: &str,
        host_key: HostKeyMode,
        connect_timeout: u32,
    ) -> Result<Command> {
        self.require_admin()?;
        let user = self.fleet.ssh_user(fqdn, self.cfg.ssh_user.as_deref());
        let mut c = Command::new("ssh");
        if let Some(key) = self.identity() {
            if key.is_file() {
                c.arg("-i").arg(&key);
                c.arg("-o").arg("IdentitiesOnly=yes");
            }
        }
        c.arg("-o").arg(format!("ConnectTimeout={connect_timeout}"));
        if !self.interactive {
            c.arg("-o").arg("BatchMode=yes");
        }
        match host_key {
            HostKeyMode::AcceptNew => {
                c.arg("-o").arg("StrictHostKeyChecking=accept-new");
            }
            HostKeyMode::Strict => {
                c.arg("-o").arg("StrictHostKeyChecking=yes");
                c.arg("-o").arg("UpdateHostKeys=no");
            }
        }
        c.arg(format!("{user}@{fqdn}"));
        Ok(c)
    }

    pub fn ssh_base(&self, fqdn: &str) -> Result<Command> {
        self.ssh_base_opts(fqdn, HostKeyMode::AcceptNew, 20)
    }

    /// Strict host-key check + `/opt/relay` presence. Used to pick Repair vs Reset known_hosts.
    pub fn diagnose(&self, fqdn: &str) -> SshDiag {
        let mut diag = SshDiag::default();
        let mut c = match self.ssh_base_opts(fqdn, HostKeyMode::Strict, 8) {
            Ok(c) => c,
            Err(_) => return diag,
        };
        c.arg("test -d /opt/relay && echo RELAY_INSTALL=1 || echo RELAY_INSTALL=0");
        match c.output() {
            Ok(out) => {
                let text = output_text(&out);
                if host_key_changed_from_ssh_text(&text) {
                    diag.host_key_changed = true;
                    diag.ssh_ok = Some(false);
                    return diag;
                }
                if out.status.success() {
                    diag.ssh_ok = Some(true);
                    diag.install_present = Some(text.contains("RELAY_INSTALL=1"));
                } else {
                    diag.ssh_ok = Some(false);
                }
            }
            Err(_) => {
                diag.ssh_ok = Some(false);
            }
        }
        diag
    }

    pub fn run(&self, fqdn: &str, remote: &str) -> Result<Output> {
        let mut c = self.ssh_base(fqdn)?;
        c.arg(remote);
        c.output().with_context(|| format!("ssh {fqdn}"))
    }

    pub fn run_bash_s(&self, fqdn: &str, script: &str, args: &[&str]) -> Result<Output> {
        let mut c = self.ssh_base(fqdn)?;
        // `bash -s -- args` so extras land in $1…; without `--`, the first arg is $0.
        c.arg("bash").arg("-s").arg("--").args(args);
        // Pipe all three: inherited stdout would print to the ops journal and leave
        // `Output.stdout` empty (ownership probe / job logs would silently miss lines).
        c.stdin(Stdio::piped());
        c.stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        let mut child = c.spawn().with_context(|| format!("ssh {fqdn}"))?;
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes())?;
        }
        child
            .wait_with_output()
            .with_context(|| format!("ssh {fqdn} bash -s"))
    }

    pub fn scp_to(&self, fqdn: &str, local: &Path, remote: &str) -> Result<()> {
        self.require_admin()?;
        let user = self.fleet.ssh_user(fqdn, self.cfg.ssh_user.as_deref());
        let mut c = Command::new("scp");
        if let Some(key) = self.identity() {
            if key.is_file() {
                c.arg("-i").arg(&key);
                c.arg("-o").arg("IdentitiesOnly=yes");
            }
        }
        if !self.interactive {
            c.arg("-o").arg("BatchMode=yes");
        }
        c.arg("-o").arg("StrictHostKeyChecking=accept-new");
        c.arg(local).arg(format!("{user}@{fqdn}:{remote}"));
        let out = c.output()?;
        if !out.status.success() {
            anyhow::bail!(
                "scp {}: {}",
                fqdn,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

pub fn checkout(cfg: &OpsConfig) -> Result<std::path::PathBuf> {
    cfg.checkout_path().ok_or_else(|| {
        anyhow!("no gateway checkout (clone the GitLab project or set gateway_checkout)")
    })
}

pub fn output_text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !s.ends_with('\n') && !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&err);
    }
    s
}

/// True only for a *changed* key already in known_hosts (not a merely unknown host).
pub fn host_key_changed_from_ssh_text(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("remote host identification has changed")
        || t.contains("someone could be eavesdropping")
}

/// SSH diagnose for unhealthy nodes with TCP 22 open.
pub async fn attach_diags(cfg: &OpsConfig, fleet: &FleetFile, nodes: &mut [crate::probe::NodeProbe]) {
    let want: Vec<String> = nodes
        .iter()
        .filter(|n| !n.healthy && n.tcp22)
        .map(|n| n.fqdn.clone())
        .collect();
    if want.is_empty() {
        return;
    }
    let mut set = tokio::task::JoinSet::new();
    for fqdn in want {
        let cfg = cfg.clone();
        let fleet = fleet.clone();
        set.spawn_blocking(move || {
            let ssh = SshCtx {
                cfg: &cfg,
                fleet: &fleet,
                interactive: false,
            };
            (fqdn.clone(), ssh.diagnose(&fqdn))
        });
    }
    while let Some(res) = set.join_next().await {
        let Ok((fqdn, diag)) = res else {
            continue;
        };
        if let Some(n) = nodes.iter_mut().find(|n| n.fqdn == fqdn) {
            n.ssh_ok = diag.ssh_ok;
            n.host_key_changed = diag.host_key_changed;
            n.install_present = diag.install_present;
        }
    }
}

/// List bare repos under `data_root` whose tree is not fully owned by `relay`.
/// Healthy nodes with TCP 22 (gateway can warm/index as `relay` only when ownership is correct).
pub fn probe_repo_ownership(ssh: &SshCtx<'_>, fqdn: &str, data_root: &str) -> Vec<String> {
    let script = r#"set -eu
DATA="$1"
[[ -d "$DATA" ]] || exit 0
shopt -s nullglob
for bare in "$DATA"/*.git; do
  [[ -d "$bare" ]] || continue
  owner=$(stat -c %U "$bare" 2>/dev/null || echo unknown)
  stray=$(find "$bare" -not -user relay -print -quit 2>/dev/null || true)
  if [[ "$owner" != "relay" || -n "$stray" ]]; then
    basename "$bare" .git
  fi
done
"#;
    let out = match ssh.run_bash_s(fqdn, script, &[data_root]) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Attach ownership drift for healthy nodes.
pub async fn attach_repo_ownership(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    nodes: &mut [crate::probe::NodeProbe],
) {
    let data_root = fleet.data_root().to_string();
    let want: Vec<String> = nodes
        .iter()
        .filter(|n| n.healthy && n.tcp22)
        .map(|n| n.fqdn.clone())
        .collect();
    if want.is_empty() {
        return;
    }
    let mut set = tokio::task::JoinSet::new();
    for fqdn in want {
        let cfg = cfg.clone();
        let fleet = fleet.clone();
        let data_root = data_root.clone();
        set.spawn_blocking(move || {
            let ssh = SshCtx {
                cfg: &cfg,
                fleet: &fleet,
                interactive: false,
            };
            let bad = probe_repo_ownership(&ssh, &fqdn, &data_root);
            (fqdn, bad)
        });
    }
    while let Some(res) = set.join_next().await {
        let Ok((fqdn, bad)) = res else {
            continue;
        };
        if let Some(n) = nodes.iter_mut().find(|n| n.fqdn == fqdn) {
            n.ownership_bad_repos = bad;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_changed_host_key_not_unknown() {
        let changed = "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n\
             IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
             Host key verification failed.\n";
        assert!(host_key_changed_from_ssh_text(changed));
        assert!(host_key_changed_from_ssh_text(
            "WARNING: Someone could be eavesdropping on you right now"
        ));
        assert!(!host_key_changed_from_ssh_text(
            "Host key verification failed.\nNo ECDSA host key is known for lon1.example."
        ));
        assert!(!host_key_changed_from_ssh_text(
            "Permission denied (publickey)."
        ));
    }
}

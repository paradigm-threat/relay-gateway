use crate::config::OpsConfig;
use crate::inventory::Inventory;
use crate::topology::FleetFile;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Output, Stdio};

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

    pub fn ssh_base(&self, fqdn: &str) -> Result<Command> {
        self.require_admin()?;
        let user = self.fleet.ssh_user(fqdn, self.cfg.ssh_user.as_deref());
        let mut c = Command::new("ssh");
        if let Some(key) = self.identity() {
            if key.is_file() {
                c.arg("-i").arg(&key);
                c.arg("-o").arg("IdentitiesOnly=yes");
            }
        }
        c.arg("-o").arg("ConnectTimeout=20");
        if !self.interactive {
            c.arg("-o").arg("BatchMode=yes");
        }
        c.arg("-o").arg("StrictHostKeyChecking=accept-new");
        c.arg(format!("{user}@{fqdn}"));
        Ok(c)
    }

    pub fn run(&self, fqdn: &str, remote: &str) -> Result<Output> {
        let mut c = self.ssh_base(fqdn)?;
        c.arg(remote);
        c.output().with_context(|| format!("ssh {fqdn}"))
    }

    pub fn run_bash_s(&self, fqdn: &str, script: &str, args: &[&str]) -> Result<Output> {
        let mut c = self.ssh_base(fqdn)?;
        c.arg("bash").arg("-s").args(args);
        c.stdin(Stdio::piped());
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

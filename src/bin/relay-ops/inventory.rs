use crate::config::OpsConfig;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    pub ssh_binary: bool,
    pub ssh_path: Option<String>,
    pub ssh_agent: bool,
    pub identity_file: bool,
    pub identity_path: Option<String>,
    pub admin_ready: bool,
    pub vercel_token: bool,
    pub slack_webhook: bool,
    pub gitlab_token: bool,
    pub op_cli: bool,
    pub git_binary: bool,
    pub gateway_checkout: bool,
    pub checkout_path: Option<String>,
}

impl Inventory {
    pub fn collect(cfg: &OpsConfig) -> Self {
        let ssh_path = which("ssh");
        let ssh_binary = ssh_path.is_some();
        let ssh_agent = std::env::var_os("SSH_AUTH_SOCK")
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let id_path = cfg.identity_path();
        let identity_file = id_path.as_ref().map(|p| p.is_file()).unwrap_or(false);
        let admin_ready = ssh_binary && (ssh_agent || identity_file);
        let checkout = cfg
            .checkout_path()
            .filter(|p| p.join("scripts/relay-master-deploy.sh").is_file());
        Inventory {
            ssh_binary,
            ssh_path,
            ssh_agent,
            identity_file,
            identity_path: id_path.map(|p| p.display().to_string()),
            admin_ready,
            vercel_token: cfg
                .vercel_token
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_some(),
            slack_webhook: cfg
                .slack_webhook
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_some(),
            gitlab_token: cfg
                .gitlab_token
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_some(),
            op_cli: which("op").is_some(),
            git_binary: which("git").is_some(),
            gateway_checkout: checkout.is_some(),
            checkout_path: checkout.map(|p| p.display().to_string()),
        }
    }
}

pub fn which(name: &str) -> Option<String> {
    let exe = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(&exe);
            if cand.is_file() {
                return Some(cand.display().to_string());
            }
        }
    }
    let fallback = PathBuf::from(&exe);
    if fallback.is_file() {
        return Some(fallback.display().to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_does_not_require_live_ssh() {
        let cfg = OpsConfig::default();
        let inv = Inventory::collect(&cfg);
        // Must not panic; admin_ready is local-only.
        let _ = inv.admin_ready;
    }
}

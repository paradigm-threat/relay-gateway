use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpsConfig {
    pub gitlab_url: Option<String>,
    pub gitlab_token: Option<String>,
    pub gitlab_ref: Option<String>,
    pub identity_file: Option<String>,
    pub ssh_user: Option<String>,
    pub vercel_token: Option<String>,
    pub vercel_team_id: Option<String>,
    pub spot_token: Option<String>,
    pub slack_webhook: Option<String>,
    pub gateway_checkout: Option<String>,
    pub bind: Option<String>,
    pub probe_interval_secs: Option<u64>,
    /// Fast retry while any node is down or DNS-empty. Default 10s.
    pub probe_interval_down_secs: Option<u64>,
    pub auto_drop_dead_fleet_a: Option<bool>,
}

impl OpsConfig {
    pub fn load() -> Self {
        let mut cfg = Self::from_file().unwrap_or_default();
        cfg.merge_env();
        cfg.resolve_op_refs();
        cfg
    }

    fn from_file() -> Option<Self> {
        let path = config_path();
        let raw = fs::read_to_string(&path).ok()?;
        toml::from_str(&raw).ok()
    }

    fn merge_env(&mut self) {
        if let Ok(v) = std::env::var("RELAY_OPS_GITLAB_URL") {
            self.gitlab_url = Some(v);
        }
        if let Ok(v) = std::env::var("GITLAB_TOKEN") {
            self.gitlab_token = Some(v);
        } else if let Ok(v) = std::env::var("PRIVATE_TOKEN") {
            self.gitlab_token = Some(v);
        }
        if let Ok(v) = std::env::var("RELAY_SSH_IDENTITY") {
            self.identity_file = Some(v);
        }
        if let Ok(v) = std::env::var("RELAY_SSH_USER") {
            self.ssh_user = Some(v);
        }
        if let Ok(v) = std::env::var("VERCEL_API_TOKEN").or_else(|_| std::env::var("VERCEL_TOKEN"))
        {
            self.vercel_token = Some(v);
        }
        if let Ok(v) = std::env::var("VERCEL_TEAM_ID") {
            self.vercel_team_id = Some(v);
        }
        if let Ok(v) = std::env::var("RXTSPOT_TOKEN") {
            self.spot_token = Some(v);
        }
        if let Ok(v) = std::env::var("RELAY_NOTIFY_SLACK_WEBHOOK_URL") {
            self.slack_webhook = Some(v);
        }
        if let Ok(v) = std::env::var("RELAY_OPS_CHECKOUT") {
            self.gateway_checkout = Some(v);
        }
        merge_relay_env_file(self);
    }

    fn resolve_op_refs(&mut self) {
        for slot in [
            &mut self.gitlab_token,
            &mut self.vercel_token,
            &mut self.spot_token,
            &mut self.slack_webhook,
            &mut self.identity_file,
        ] {
            if let Some(v) = slot.as_ref() {
                if let Some(resolved) = resolve_secret(v) {
                    *slot = Some(resolved);
                }
            }
        }
    }

    pub fn identity_path(&self) -> Option<PathBuf> {
        self.identity_file
            .as_ref()
            .map(|p| expand_home(p))
            .or_else(|| {
                dirs::home_dir()
                    .map(|h| h.join(".ssh/id_ed25519_relay_gateway"))
                    .filter(|p| p.is_file())
            })
    }

    /// Bootstrap URL: config/env, else a GitLab-looking git remote on the checkout.
    pub fn gitlab_url_resolved(&self) -> Option<String> {
        self.gitlab_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(detect_gitlab_remote)
    }

    pub fn checkout_path(&self) -> Option<PathBuf> {
        self.gateway_checkout
            .as_ref()
            .map(|p| expand_home(p))
            .or_else(detect_checkout)
    }

    pub fn bind_addr(&self) -> String {
        self.bind
            .clone()
            .unwrap_or_else(|| "127.0.0.1:7780".to_string())
    }

    pub fn probe_interval(&self) -> u64 {
        self.probe_interval_secs.unwrap_or(300).max(30)
    }

    /// Cheap retry while a node is unreachable (DNS/TCP only until 443 opens).
    pub fn probe_interval_down(&self) -> u64 {
        self.probe_interval_down_secs.unwrap_or(10).clamp(3, 60)
    }

    pub fn auto_drop(&self) -> bool {
        self.auto_drop_dead_fleet_a.unwrap_or(true)
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("relay-ops")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("relay-ops")
}

pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

pub fn resolve_secret(value: &str) -> Option<String> {
    let value = value.trim();
    if !value.starts_with("op://") {
        return Some(value.to_string());
    }
    let out = Command::new("op").args(["read", value]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn detect_checkout() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    if cwd.join("scripts/relay-master-deploy.sh").is_file() {
        return Some(cwd);
    }
    if cwd.join("ops/fleet.json").is_file() {
        return Some(cwd);
    }
    None
}

fn detect_gitlab_remote() -> Option<String> {
    let root = detect_checkout()?;
    let out = Command::new("git")
        .args(["-C"])
        .arg(&root)
        .args(["remote", "-v"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut origin = None;
    let mut named = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(url) = parts.next() else { continue };
        if name == "gitlab" {
            named = Some(url.to_string());
        } else if name == "origin" {
            origin = Some(url.to_string());
        } else if named.is_none() && (url.contains("gitlab") || url.contains("/-/")) {
            named = Some(url.to_string());
        }
    }
    named.or(origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_url_resolved_prefers_explicit() {
        let cfg = OpsConfig {
            gitlab_url: Some("https://gitlab.example/g/p".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.gitlab_url_resolved().as_deref(),
            Some("https://gitlab.example/g/p")
        );
    }
}

fn merge_relay_env_file(cfg: &mut OpsConfig) {
    let candidates = [
        std::env::var("RELAY_FLEET_ENV_FILE")
            .ok()
            .map(PathBuf::from),
        detect_checkout().map(|p| p.join("relay.env")),
    ];
    for path in candidates.into_iter().flatten() {
        if let Ok(raw) = fs::read_to_string(&path) {
            apply_env_kv(cfg, &raw);
        }
    }
}

fn apply_env_kv(cfg: &mut OpsConfig, raw: &str) {
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "VERCEL_API_TOKEN" | "VERCEL_TOKEN" if cfg.vercel_token.is_none() => {
                cfg.vercel_token = Some(v)
            }
            "VERCEL_TEAM_ID" if cfg.vercel_team_id.is_none() => cfg.vercel_team_id = Some(v),
            "RXTSPOT_TOKEN" if cfg.spot_token.is_none() => cfg.spot_token = Some(v),
            "RELAY_SSH_IDENTITY" if cfg.identity_file.is_none() => cfg.identity_file = Some(v),
            "RELAY_SSH_USER" if cfg.ssh_user.is_none() => cfg.ssh_user = Some(v),
            "GITLAB_TOKEN" if cfg.gitlab_token.is_none() => cfg.gitlab_token = Some(v),
            "RELAY_NOTIFY_SLACK_WEBHOOK_URL" if cfg.slack_webhook.is_none() => {
                cfg.slack_webhook = Some(v)
            }
            _ => {}
        }
    }
}

pub fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display()))
}

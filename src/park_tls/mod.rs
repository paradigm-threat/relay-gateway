//! Opt-in parked-domain TLS plugin (`server.tls.plugin: park`).
//!
//! Coordinates HTTP-01 challenges and certificate fan-out across fleet masters so
//! multi-A / `fleet.relaygateway.net` ingress can renew parked names. Repos without
//! the plugin never enter this path; node/`{repo}.{node}` Caddy on-demand is unchanged.

mod acme;
mod caddy;
mod fanout;
mod store;

pub use acme::ensure_certs_for_domains;
pub use caddy::write_parked_caddyfile;
pub use fanout::{
    authorize_park_tls_peer, post_park_tls_hook, spawn_fanout_cert, spawn_fanout_challenge,
};
pub use store::{
    challenge_body, park_tls_root, persist_cert_pair, persist_challenge, read_cert_expiry_days,
    remove_challenge,
};

use crate::catalog_policy::{
    normalize_domain, parked_domains_allowlist_path, parked_domains_map_path,
};
use crate::git::repo::{open_repo, read_relay_config};
use crate::types::AppState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const PLUGIN_ID: &str = "park";

/// Filter a parked domain→repo map to hosts whose repo tip enables `server.tls.plugin: park`.
pub fn filter_plugin_enabled_domains(
    repo_path: &Path,
    map: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut enabled_repos: HashSet<String> = HashSet::new();
    let mut disabled_repos: HashSet<String> = HashSet::new();
    let mut out = HashMap::new();
    for (domain, repo) in map {
        if enabled_repos.contains(repo) {
            out.insert(domain.clone(), repo.clone());
            continue;
        }
        if disabled_repos.contains(repo) {
            continue;
        }
        if repo_has_park_plugin(repo_path, repo) {
            enabled_repos.insert(repo.clone());
            out.insert(domain.clone(), repo.clone());
        } else {
            disabled_repos.insert(repo.clone());
        }
    }
    out
}

fn repo_has_park_plugin(repo_path: &Path, repo: &str) -> bool {
    let Some(git) = open_repo(&repo_path.to_path_buf(), repo) else {
        return false;
    };
    let branch = crate::git::repo::default_branch_from_bare_head(&repo_path.to_path_buf(), repo)
        .unwrap_or_else(|| "main".into());
    read_relay_config(&git, &branch)
        .map(|c| c.has_park_tls_plugin())
        .unwrap_or(false)
}

/// Materialize TLS allowlist (plugin-filtered) used by `caddy-ask.py`.
/// Full Host→repo map remains in `parked-domains.json` for HTTP routing.
pub fn write_tls_allowlist(repo_path: &Path, map: &HashMap<String, String>) {
    let tls_map = filter_plugin_enabled_domains(repo_path, map);
    let path = parked_domains_allowlist_path();
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            return;
        }
    }
    let mut domains: Vec<&String> = tls_map.keys().collect();
    domains.sort();
    let mut body =
        String::from("# relay parked TLS domains (plugin=park only); managed by relay-gateway\n");
    for d in domains {
        body.push_str(d);
        body.push('\n');
    }
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write parked TLS allowlist");
    }
    // Keep full routing map on disk.
    let map_path = parked_domains_map_path();
    match serde_json::to_string_pretty(map) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&map_path, format!("{json}\n")) {
                tracing::warn!(error = %e, path = %map_path.display(), "failed to write parked-domains map");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialize parked-domains map"),
    }
    let _ = write_parked_caddyfile(&tls_map);
}

/// Domains that currently need park-plugin TLS on this node.
pub fn plugin_domains_for_state(state: &AppState) -> HashMap<String, String> {
    let Some(map) = state.parked_domains.as_ref() else {
        return HashMap::new();
    };
    filter_plugin_enabled_domains(&state.repo_path, map)
}

/// Background renewal / issuance worker.
pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        // Small delay so HTTP listener is up before first ACME HTTP-01 probe.
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        loop {
            if let Err(e) = run_once(&state).await {
                tracing::warn!(error = %e, "park_tls worker iteration failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
}

fn this_node_is_issuer(state: &AppState) -> bool {
    if std::env::var("RELAY_PARK_TLS_ISSUER").ok().as_deref() == Some("1") {
        return true;
    }
    if std::env::var("RELAY_PARK_TLS_ISSUER").ok().as_deref() == Some("0") {
        return false;
    }
    let self_fqdn = state
        .node_fqdn
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if self_fqdn.is_empty() {
        return false;
    }
    let raw = std::env::var("RELAY_MASTER_PEER_LIST")
        .or_else(|_| std::env::var("RELAY_OPERATOR_FLEET"))
        .unwrap_or_else(|_| self_fqdn.clone());
    let mut peers: Vec<String> = raw
        .split(|c| c == ';' || c == ',')
        .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if peers.is_empty() {
        peers.push(self_fqdn.clone());
    }
    peers.sort();
    peers.first().map(|s| s == &self_fqdn).unwrap_or(false)
}

async fn run_once(state: &AppState) -> anyhow::Result<()> {
    let domains = plugin_domains_for_state(state);
    if domains.is_empty() {
        tracing::debug!("park_tls: no plugin-enabled parked domains");
        return Ok(());
    }
    let _ = write_parked_caddyfile(&domains);
    if !this_node_is_issuer(state) {
        tracing::debug!("park_tls: not the elected issuer; waiting for fan-out");
        return Ok(());
    }
    let mut need: Vec<String> = Vec::new();
    for d in domains.keys() {
        match read_cert_expiry_days(d) {
            Some(days) if days > 30.0 => {}
            _ => need.push(d.clone()),
        }
    }
    if need.is_empty() {
        tracing::debug!(count = domains.len(), "park_tls: certs fresh");
        return Ok(());
    }
    tracing::info!(?need, "park_tls: issuing/renewing certificates");
    ensure_certs_for_domains(state, &need).await?;
    let _ = write_parked_caddyfile(&domains);
    Ok(())
}

/// Serve HTTP-01 from the park-tls challenge store, falling back to `RELAY_ACME_DIR`.
pub async fn serve_challenge(legacy_dir: &str, token: &str) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let token = token.trim_matches('/');
    if token.is_empty() || token.contains("..") || token.contains('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(body) = challenge_body(token) {
        return (StatusCode::OK, [("Content-Type", "text/plain")], body).into_response();
    }
    let path = PathBuf::from(legacy_dir).join(token);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (StatusCode::OK, [("Content-Type", "text/plain")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Normalize a Host for challenge/cert keys.
pub fn norm_host(host: &str) -> String {
    normalize_domain(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RelayConfig, ServerConfig, TlsConfig};

    #[test]
    fn park_plugin_detection() {
        let mut cfg = RelayConfig::default();
        assert!(!cfg.has_park_tls_plugin());
        cfg.server = Some(ServerConfig {
            tls: Some(TlsConfig {
                plugin: Some("park".into()),
            }),
            ..Default::default()
        });
        assert!(cfg.has_park_tls_plugin());
    }
}

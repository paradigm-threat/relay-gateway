//! Tip-notify: `POST /hooks/peer` to mesh peers after validated push or hop-forward.

use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};

use crate::git::origins;
use crate::mesh_events;
use crate::mesh_role;
use crate::types::{GitConfig, RelayConfig};

#[derive(Debug, Clone, Serialize)]
struct PeerNotifyBody {
    repo: String,
    peer: String,
    branch: String,
    new_sha: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    via: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hops_remaining: Option<u32>,
}

fn notify_scheme() -> &'static str {
    match std::env::var("RELAY_PEER_NOTIFY_SCHEME")
        .ok()
        .or_else(|| std::env::var("RELAY_MESH_EVENT_SCHEME").ok())
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("http") => "http",
        _ => "https",
    }
}

fn notify_port(scheme: &str) -> u16 {
    if scheme == "https" {
        std::env::var("RELAY_HTTPS_PORT")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(443)
    } else {
        std::env::var("RELAY_HTTP_PORT")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(8080)
    }
}

fn peer_notify_url(host: &str) -> String {
    let scheme = notify_scheme();
    let port = notify_port(scheme);
    if (scheme == "https" && port == 443) || (scheme == "http" && port == 8080) {
        format!("{scheme}://{host}/hooks/peer")
    } else {
        format!("{scheme}://{host}:{port}/hooks/peer")
    }
}

fn outbound_secret(url: &str) -> Option<String> {
    mesh_events::outbound_mesh_secret_for_post_url(url)
}

/// Blocking tip-notify (post-receive / catalog apply in hook-handler).
pub fn notify_peers_blocking(
    repo: &str,
    branch: &str,
    new_sha: &str,
    git_cfg: &GitConfig,
    via: &[String],
    hops_remaining: Option<u32>,
) {
    if std::env::var("RELAY_SYNC_IN_PROGRESS").is_ok() {
        debug!("peer notify skipped: RELAY_SYNC_IN_PROGRESS");
        return;
    }
    let self_host = mesh_role::node_fqdn_normalized().unwrap_or_else(|| "localhost".to_string());
    let targets: Vec<String> = if let Some(hops) = hops_remaining {
        if hops == 0 {
            return;
        }
        if let Some((next, _)) =
            origins::hop_forward_target(git_cfg, Some(&self_host), via, Some(hops))
        {
            vec![next]
        } else {
            return;
        }
    } else {
        origins::tip_notify_targets(git_cfg, Some(&self_host))
    };

    let ring_hops = if hops_remaining.is_none()
        && matches!(git_cfg.notify_mode, crate::types::NotifyMode::Next)
    {
        let n = origins::relay_ring_hosts(git_cfg).len();
        if n > 1 {
            Some((n - 1) as u32)
        } else if n == 1 {
            Some(1)
        } else {
            None
        }
    } else {
        hops_remaining
    };

    if targets.is_empty() {
        debug!(repo, "peer notify: no targets");
        return;
    }

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error=%e, "peer notify: client build failed");
            return;
        }
    };

    for target in targets {
        let url = peer_notify_url(&target);
        let Some(sec) = outbound_secret(&url) else {
            warn!(%url, "peer notify: no outbound secret");
            continue;
        };
        let body = PeerNotifyBody {
            repo: repo.to_string(),
            peer: self_host.clone(),
            branch: branch.to_string(),
            new_sha: new_sha.to_string(),
            via: via.to_vec(),
            hops_remaining: ring_hops,
        };
        match client
            .post(&url)
            .header("X-Relay-Peer-Secret", &sec)
            .json(&body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                info!(%url, repo, "peer notify delivered");
            }
            Ok(resp) => {
                warn!(%url, status=%resp.status(), repo, "peer notify rejected");
            }
            Err(e) => {
                warn!(%url, error=%e, repo, "peer notify HTTP failed");
            }
        }
    }
}

/// After validated push: read tip `.relay.yaml` and notify mesh peers.
pub fn notify_after_push(repo_slug: &str, branch: &str, new_sha: &str, relay_cfg: &RelayConfig) {
    let Some(ref git) = relay_cfg.git else {
        notify_peers_blocking(repo_slug, branch, new_sha, &Default::default(), &[], None);
        return;
    };
    notify_peers_blocking(repo_slug, branch, new_sha, git, &[], None);
}

/// Hop-forward after peer-fetch tip change (async, non-blocking).
pub fn spawn_hop_forward(
    repo_slug: String,
    branch: String,
    new_sha: String,
    relay_cfg: GitConfig,
    via: Vec<String>,
    hops_remaining: Option<u32>,
) {
    std::thread::spawn(move || {
        notify_peers_blocking(
            &repo_slug,
            &branch,
            &new_sha,
            &relay_cfg,
            &via,
            hops_remaining,
        );
    });
}

/// Load `git` config from bare tip for hop-forward decisions.
pub fn git_config_from_bare_tip(
    repo_path: &std::path::Path,
    _branch: &str,
    tip_sha: &str,
) -> Option<GitConfig> {
    let repo = git2::Repository::open_bare(repo_path).ok()?;
    let cfg = crate::git::read_relay_config(&repo, tip_sha)?;
    cfg.git
}

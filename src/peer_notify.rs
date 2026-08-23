//! Tip-notify: `POST /hooks/peer` to mesh peers after validated push or hop-forward.

use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};

use crate::git::origins;
use crate::mesh_events;
use crate::mesh_role;
use crate::post_push_sync::{write_post_push_sync, PeerNotifyOutcome, PostPushSyncRecord};
use crate::types::{GitConfig, NotifyMode, RelayConfig};

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

fn peer_notify_timeout() -> Duration {
    let secs = std::env::var("RELAY_PEER_NOTIFY_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(12);
    Duration::from_secs(secs)
}

fn post_peer_notify(
    client: &reqwest::blocking::Client,
    self_host: &str,
    target: &str,
    repo: &str,
    branch: &str,
    new_sha: &str,
    via: &[String],
    hops_remaining: Option<u32>,
) -> PeerNotifyOutcome {
    let url = peer_notify_url(target);
    let Some(sec) = outbound_secret(&url) else {
        warn!(%url, "peer notify: no outbound secret");
        return PeerNotifyOutcome {
            target: target.to_string(),
            ok: false,
            http_status: None,
            error: Some("no outbound mesh secret configured".into()),
        };
    };
    let body = PeerNotifyBody {
        repo: repo.to_string(),
        peer: self_host.to_string(),
        branch: branch.to_string(),
        new_sha: new_sha.to_string(),
        via: via.to_vec(),
        hops_remaining,
    };
    match client
        .post(&url)
        .header("X-Relay-Peer-Secret", &sec)
        .json(&body)
        .send()
    {
        Ok(resp) if resp.status().is_success() => {
            info!(%url, repo, "peer notify delivered");
            eprintln!("[relay-hook-handler] peer notify delivered {url} repo={repo}");
            PeerNotifyOutcome {
                target: target.to_string(),
                ok: true,
                http_status: Some(resp.status().as_u16()),
                error: None,
            }
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            let detail = resp.text().unwrap_or_default();
            let err = if detail.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {}", detail.chars().take(500).collect::<String>())
            };
            warn!(%url, status, repo, "peer notify rejected");
            eprintln!(
                "[relay-hook-handler] peer notify rejected {url} status={status} repo={repo} {err}"
            );
            PeerNotifyOutcome {
                target: target.to_string(),
                ok: false,
                http_status: Some(status),
                error: Some(err),
            }
        }
        Err(e) => {
            warn!(%url, error=%e, repo, "peer notify HTTP failed");
            eprintln!("[relay-hook-handler] peer notify HTTP failed {url} repo={repo} error={e}");
            PeerNotifyOutcome {
                target: target.to_string(),
                ok: false,
                http_status: None,
                error: Some(e.to_string()),
            }
        }
    }
}

/// Blocking tip-notify (post-receive / catalog apply in hook-handler).
pub fn notify_peers_blocking(
    repo: &str,
    branch: &str,
    new_sha: &str,
    git_cfg: &GitConfig,
    via: &[String],
    hops_remaining: Option<u32>,
) -> Vec<PeerNotifyOutcome> {
    if std::env::var("RELAY_SYNC_IN_PROGRESS").is_ok() {
        debug!("peer notify skipped: RELAY_SYNC_IN_PROGRESS");
        return Vec::new();
    }
    let self_host = mesh_role::node_fqdn_normalized().unwrap_or_else(|| "localhost".to_string());
    let notify_mode = if hops_remaining.is_none() {
        origins::initial_notify_mode(git_cfg)
    } else {
        git_cfg.notify_mode
    };

    let ring_hops = if hops_remaining.is_none() && matches!(notify_mode, NotifyMode::Next) {
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

    let client = match reqwest::blocking::Client::builder()
        .timeout(peer_notify_timeout())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error=%e, "peer notify: client build failed");
            return Vec::new();
        }
    };

    let mut outcomes: Vec<PeerNotifyOutcome> = Vec::new();

    // Hop-forward path: single next target from hop_forward_target.
    if let Some(hops) = hops_remaining {
        if hops == 0 {
            return outcomes;
        }
        let Some((next, next_hops)) =
            origins::hop_forward_target(git_cfg, Some(&self_host), via, Some(hops))
        else {
            return outcomes;
        };
        let o = post_peer_notify(
            &client,
            &self_host,
            &next,
            repo,
            branch,
            new_sha,
            via,
            Some(next_hops),
        );
        if o.ok {
            outcomes.push(o);
            persist_notify_outcomes(repo, branch, new_sha, &self_host, &outcomes);
            return outcomes;
        }
        outcomes.push(o);
        // Dead next hop: try next+1 once (exclude failed host).
        let mut via2 = via.to_vec();
        via2.push(next.clone());
        let ring = origins::relay_ring_hosts(git_cfg);
        if let Some(alt) = origins::next_hop_after_excluding(&ring, &self_host, &via2) {
            warn!(
                failed=%next,
                retry=%alt,
                repo,
                "peer notify: next hop failed; trying next+1"
            );
            let alt_hops = hops.saturating_sub(1);
            outcomes.push(post_peer_notify(
                &client,
                &self_host,
                &alt,
                repo,
                branch,
                new_sha,
                &via2,
                Some(alt_hops),
            ));
        }
        persist_notify_outcomes(repo, branch, new_sha, &self_host, &outcomes);
        return outcomes;
    }

    // Initial tip-notify after push.
    match notify_mode {
        NotifyMode::All => {
            let mut cfg = git_cfg.clone();
            cfg.notify_mode = NotifyMode::All;
            let targets = origins::tip_notify_targets(&cfg, Some(&self_host));
            if targets.is_empty() {
                debug!(repo, "peer notify: no targets");
                persist_notify_outcomes(repo, branch, new_sha, &self_host, &outcomes);
                return outcomes;
            }
            for target in targets {
                outcomes.push(post_peer_notify(
                    &client, &self_host, &target, repo, branch, new_sha, via, ring_hops,
                ));
            }
        }
        NotifyMode::Next => {
            let ring = origins::relay_ring_hosts(git_cfg);
            let Some(first) = origins::next_hop_after(&ring, &self_host) else {
                debug!(repo, "peer notify: no targets");
                persist_notify_outcomes(repo, branch, new_sha, &self_host, &outcomes);
                return outcomes;
            };
            let o = post_peer_notify(
                &client, &self_host, &first, repo, branch, new_sha, via, ring_hops,
            );
            if o.ok {
                outcomes.push(o);
                persist_notify_outcomes(repo, branch, new_sha, &self_host, &outcomes);
                return outcomes;
            }
            outcomes.push(o);
            // One dead Spot must not stall the ring — try next+1 once.
            let exclude = [first.clone()];
            if let Some(alt) = origins::next_hop_after_excluding(&ring, &self_host, &exclude) {
                warn!(
                    failed=%first,
                    retry=%alt,
                    repo,
                    "peer notify: next hop failed; trying next+1"
                );
                let via2 = vec![first];
                outcomes.push(post_peer_notify(
                    &client, &self_host, &alt, repo, branch, new_sha, &via2, ring_hops,
                ));
            }
        }
    }
    persist_notify_outcomes(repo, branch, new_sha, &self_host, &outcomes);
    outcomes
}

fn persist_notify_outcomes(
    repo: &str,
    branch: &str,
    new_sha: &str,
    publisher_fqdn: &str,
    outcomes: &[PeerNotifyOutcome],
) {
    let repo_path = std::env::var("RELAY_REPO_PATH")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/opt/relay/data"));
    let ok_count = outcomes.iter().filter(|o| o.ok).count() as u32;
    let fail_count = outcomes.len() as u32 - ok_count;
    let finished_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    write_post_push_sync(
        &repo_path,
        &PostPushSyncRecord {
            repo: repo.to_string(),
            branch: branch.to_string(),
            pushed_sha: new_sha.to_string(),
            publisher_fqdn: publisher_fqdn.to_string(),
            finished_at_unix,
            notify_ok_count: ok_count,
            notify_fail_count: fail_count,
            peers: outcomes.to_vec(),
        },
    );
    if fail_count > 0 {
        eprintln!(
            "[relay-hook-handler] mesh notify partial/fail repo={repo} ok={ok_count} fail={fail_count} (see state/post_push_sync/{repo}.json)"
        );
    }
}

/// After validated push: read tip `.relay.yaml` and notify mesh peers.
pub fn notify_after_push(repo_slug: &str, branch: &str, new_sha: &str, relay_cfg: &RelayConfig) {
    let Some(ref git) = relay_cfg.git else {
        let _ = notify_peers_blocking(repo_slug, branch, new_sha, &Default::default(), &[], None);
        return;
    };
    let _ = notify_peers_blocking(repo_slug, branch, new_sha, git, &[], None);
}

/// Non-blocking tip-notify for `post-receive` (hook-handler has no Tokio runtime).
pub fn spawn_notify_after_push(
    repo_slug: String,
    branch: String,
    new_sha: String,
    relay_cfg: RelayConfig,
) {
    std::thread::spawn(move || {
        notify_after_push(&repo_slug, &branch, &new_sha, &relay_cfg);
    });
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
        let _ = notify_peers_blocking(
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

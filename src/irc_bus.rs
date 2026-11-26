//! Optional ops-bus emit to the local Flashback/Relay IRC agent (`POST /emit`).
//!
//! Enable with **`RELAY_IRC_EVENTS=1`**. The agent (nick = short hostname) publishes
//! into Ergo channels such as `#logs` / `#relay`. Does not replace peer tip notify
//! or `relay-ops` probe — IRC is a human + low-rate pubsub plane only.

use std::time::Duration;

use serde_json::json;
use tracing::{debug, warn};

/// True when **`RELAY_IRC_EVENTS`** is `1` / `true` / `yes` / `on`.
pub fn irc_events_enabled() -> bool {
    std::env::var("RELAY_IRC_EVENTS")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Channel from **`RELAY_MESH_EVENT_CHANNEL`**, default `#relay`.
pub fn mesh_event_channel() -> String {
    std::env::var("RELAY_MESH_EVENT_CHANNEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "#relay".into())
}

fn emit_url() -> String {
    std::env::var("RELAY_IRC_EMIT_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8099/emit".into())
}

/// Fire-and-forget POST to the local irc-agent emit API.
pub fn spawn_emit(channel: &str, level: &str, component: &str, text: &str) {
    if !irc_events_enabled() {
        return;
    }
    let url = emit_url();
    let body = json!({
        "channel": channel,
        "level": level,
        "component": component,
        "text": text,
    });
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(error=%e, "irc_bus: client build failed");
                    return;
                }
            };
            match client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!(%url, "irc_bus: emit ok");
                }
                Ok(resp) => {
                    warn!(%url, status=%resp.status(), "irc_bus: emit rejected");
                }
                Err(e) => {
                    debug!(%url, error=%e, "irc_bus: emit failed (agent down?)");
                }
            }
        });
    } else {
        std::thread::spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
            {
                Ok(c) => c,
                Err(_) => return,
            };
            let _ = client.post(&url).json(&body).send();
        });
    }
}

/// Publish a mesh-event style line into **`RELAY_MESH_EVENT_CHANNEL`** (default `#relay`).
pub fn spawn_emit_mesh_event(
    event_type: &str,
    source: &str,
    repo: &str,
    branch: &str,
    error: &str,
    node_fqdn: Option<&str>,
) {
    if !irc_events_enabled() {
        return;
    }
    let ch = mesh_event_channel();
    let node = node_fqdn.unwrap_or("node");
    let err: String = error.chars().take(280).collect();
    let text = format!("{event_type} source={source} repo={repo}@{branch} node={node} {err}");
    spawn_emit(&ch, "warn", "mesh", &text);
}

/// Short repo → ops log channel (`#logs-fb`, `#logs-ct`, …). See docs/RELAY_CHAT.md.
pub fn repo_logs_channel(repo: &str) -> String {
    let r = repo.trim().to_ascii_lowercase();
    let prefix: String = match r.as_str() {
        "fan-edits" => "fb".into(),
        "clevertree-site" => "ct".into(),
        "paradigm-threat-files" => "pt".into(),
        "paradigm-threat-timeline" => "ptt".into(),
        "relay-master" => "rm".into(),
        "relay-web-assets" => "rwa".into(),
        other => {
            let mut s: String = other
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(6)
                .collect();
            if s.len() < 2 {
                s = "repo".into();
            }
            s
        }
    };
    format!("#logs-{prefix}")
}

/// Tip apply announcement (successful peer fetch that moved the tip).
/// Low-rate mesh visibility on `#relay` only — never `#logs*` (Ergo chat history).
pub fn spawn_emit_tip_apply(repo: &str, branch: &str, sha: &str, node_fqdn: Option<&str>) {
    if !irc_events_enabled() {
        return;
    }
    let short: String = sha.chars().take(12).collect();
    let node = node_fqdn.unwrap_or("node");
    let text = format!("tip apply {repo}@{branch} → {short} on {node}");
    spawn_emit("#relay", "info", "peer_hook", &text);
}

/// Park TLS Caddyfile rewrite notice (warn → `#logs`; info no longer fills chat history).
pub fn spawn_emit_park_rewrite(host_count: usize) {
    if !irc_events_enabled() {
        return;
    }
    let text = format!("park Caddyfile regenerated ({host_count} hosts)");
    spawn_emit("#logs", "warn", "park_tls", &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_default() {
        std::env::remove_var("RELAY_MESH_EVENT_CHANNEL");
        assert_eq!(mesh_event_channel(), "#relay");
    }

    #[test]
    fn repo_logs_channel_prefixes() {
        assert_eq!(repo_logs_channel("fan-edits"), "#logs-fb");
        assert_eq!(repo_logs_channel("clevertree-site"), "#logs-ct");
        assert_eq!(repo_logs_channel("paradigm-threat-files"), "#logs-pt");
    }
}

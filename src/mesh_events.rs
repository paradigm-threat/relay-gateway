//! HTTP fan-out for mesh-visible operational events (e.g. pull / anchor validation failures).
//! Receivers: `POST /hooks/mesh-event`. Future: IRC-style rooms can subscribe using the same payload shape
//! (`channel` field, from **`RELAY_MESH_EVENT_CHANNEL`**).
//!
//! **Masters** fan out to peers; **edges** do not publish mesh HTTP events (**`crate::mesh_role`**).

use std::time::Duration;

use axum::Json;
use serde::Serialize;
use tracing::{debug, warn};

use crate::mesh_role::{self, RelayNodeRole};
use crate::notify;

/// Shared secret for mesh event HTTP hooks. Prefer **`RELAY_MESH_EVENT_SECRET`**; falls back to **`RELAY_PEER_SECRET`**.
pub fn mesh_event_secret() -> Option<String> {
    std::env::var("RELAY_MESH_EVENT_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("RELAY_PEER_SECRET")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

fn master_mesh_secret_trimmed() -> Option<String> {
    std::env::var("RELAY_MASTER_MESH_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Host part of `http(s)://host:port/...` (no brackets / IPv6 support).
fn mesh_post_url_host(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let end = rest.find('/').unwrap_or(rest.len());
    let authority = rest.get(..end)?;
    let host = match authority.rsplit_once(':') {
        Some((h, port))
            if !h.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            h
        }
        _ => authority,
    };
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!h.is_empty()).then_some(h)
}

/// Secret to send in **`x-relay-mesh-secret`** for a given target URL.
/// Targets whose host appears in **`RELAY_MASTER_PEER_LIST`** use **`RELAY_MASTER_MESH_SECRET`** when set;
/// otherwise **`mesh_event_secret()`** (legacy).
pub fn outbound_mesh_secret_for_post_url(url: &str) -> Option<String> {
    let list_csv = std::env::var("RELAY_MASTER_PEER_LIST").unwrap_or_default();
    let Some(host) = mesh_post_url_host(url) else {
        return mesh_event_secret();
    };
    let target_is_master = mesh_role::host_in_master_peer_list(&host, &list_csv);
    if target_is_master {
        if let Some(s) = master_mesh_secret_trimmed() {
            return Some(s);
        }
    }
    mesh_event_secret()
}

fn mesh_http_port() -> u16 {
    std::env::var("RELAY_HTTP_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(8080)
}

fn mesh_event_scheme() -> &'static str {
    match std::env::var("RELAY_MESH_EVENT_SCHEME")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("https") => "https",
        _ => "http",
    }
}

/// Explicit URLs (semicolon-separated) override derived URLs from **`RELAY_MASTER_PEER_LIST`**.
pub fn mesh_event_target_urls(self_fqdn: Option<&str>) -> Vec<String> {
    let explicit = std::env::var("RELAY_MESH_EVENT_URLS").unwrap_or_default();
    let items: Vec<String> = explicit
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !items.is_empty() {
        return items;
    }

    let raw = std::env::var("RELAY_MASTER_PEER_LIST").unwrap_or_default();
    mesh_event_urls_for_peers(self_fqdn, &raw, mesh_http_port(), mesh_event_scheme())
}

#[derive(Debug, Clone, Serialize)]
pub struct MeshEventPayload {
    /// e.g. **`pull_validation_failed`**
    pub event_type: String,
    pub source: String,
    pub repo: String,
    pub branch: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_fqdn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_tip: Option<String>,
    /// For **`peer_hook`**: hostname we fetched from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_peer: Option<String>,
    /// Future IRC / chat routing; set **`RELAY_MESH_EVENT_CHANNEL`** (e.g. **`#relay-ops`**).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PullValidationPublishCtx {
    /// **`git_pull`** | **`peer_hook`**
    pub source: String,
    pub repo: String,
    pub branch: String,
    pub error: String,
    pub relay_server_id: Option<String>,
    pub node_fqdn: Option<String>,
    pub after_tip: Option<String>,
    pub trigger_peer: Option<String>,
}

fn slack_text_for_pull_validation(p: &MeshEventPayload) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "[Relay] {} — pull validation failed",
        p.event_type
    ));
    lines.push(format!("source: {}", p.source));
    lines.push(format!("repo: {}  branch: {}", p.repo, p.branch));
    if let Some(ref id) = p.relay_server_id {
        lines.push(format!("relay_server_id: {}", id));
    }
    if let Some(ref fqdn) = p.node_fqdn {
        lines.push(format!("node: {}", fqdn));
    }
    if let Some(ref tip) = p.after_tip {
        lines.push(format!("tip_after: {}", tip));
    }
    if let Some(ref peer) = p.trigger_peer {
        lines.push(format!("trigger_peer: {}", peer));
    }
    if let Some(ref ch) = p.channel {
        lines.push(format!("channel_hint: {}", ch));
    }
    let err: String = p.error.chars().take(1800).collect();
    lines.push(format!("error: {}", err));
    lines.join("\n")
}

/// Fire-and-forget POST to peer **`/hooks/mesh-event`** endpoints and optional Slack Incoming Webhook (does not block the HTTP response path for long).
pub fn spawn_publish_pull_validation_failure(ctx: PullValidationPublishCtx) {
    let channel = std::env::var("RELAY_MESH_EVENT_CHANNEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let payload = MeshEventPayload {
        event_type: "pull_validation_failed".into(),
        source: ctx.source,
        repo: ctx.repo,
        branch: ctx.branch,
        error: ctx.error,
        relay_server_id: ctx.relay_server_id,
        node_fqdn: ctx.node_fqdn,
        after_tip: ctx.after_tip,
        trigger_peer: ctx.trigger_peer,
        channel,
    };

    let urls = mesh_event_target_urls(payload.node_fqdn.as_deref());
    let slack_url = notify::slack_webhook_url();
    let publish_mesh_http = mesh_role::should_publish_mesh_http_events();

    if !publish_mesh_http {
        debug!("mesh event: skip HTTP fan-out (this node is an edge — masters only)");
    } else if urls.is_empty() {
        debug!("mesh event: no target URLs (RELAY_MESH_EVENT_URLS or RELAY_MASTER_PEER_LIST)");
    }

    if !publish_mesh_http && slack_url.is_none() {
        debug!("mesh event: no mesh HTTP fan-out and no RELAY_NOTIFY_SLACK_WEBHOOK_URL — nothing to publish");
        return;
    }

    if publish_mesh_http && urls.is_empty() && slack_url.is_none() {
        debug!("mesh event: master node but no targets and no Slack — nothing to publish");
        return;
    }

    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(error=%e, "mesh event: reqwest client build failed");
                return;
            }
        };

        if publish_mesh_http {
            for url in urls {
                let Some(sec) = outbound_mesh_secret_for_post_url(&url) else {
                    warn!(%url, "mesh event: no outbound secret for target (set RELAY_MASTER_MESH_SECRET and/or RELAY_MESH_EVENT_SECRET / RELAY_PEER_SECRET)");
                    continue;
                };
                match client
                    .post(&url)
                    .header("x-relay-mesh-secret", &sec)
                    .json(&payload)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        tracing::info!(%url, "mesh event delivered");
                    }
                    Ok(resp) => {
                        warn!(%url, status=%resp.status(), "mesh event receiver rejected");
                    }
                    Err(e) => {
                        warn!(%url, error=%e, "mesh event HTTP failed");
                    }
                }
            }
        }

        if let Some(ref su) = slack_url {
            let text = slack_text_for_pull_validation(&payload);
            match notify::post_slack_webhook(&client, su, &text).await {
                Ok(()) => tracing::info!("Slack notify: pull_validation_failed delivered"),
                Err(e) => warn!(err=%e, "Slack notify failed"),
            }
        }
    });
}

fn mesh_header_secret_ok(provided: Option<&str>, expected: &str) -> bool {
    let Some(p) = provided.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let ab = p.as_bytes();
    let bb = expected.as_bytes();
    if ab.len() != bb.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in ab.iter().zip(bb.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// `POST /hooks/mesh-event` — ingest mesh events from peers (structured log today; IRC/chat later).
#[derive(serde::Serialize)]
pub struct MeshEventIngestResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn post_mesh_event(
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let role = mesh_role::relay_node_role();
    let master_sec = master_mesh_secret_trimmed();
    let provided = headers
        .get("x-relay-mesh-secret")
        .and_then(|v| v.to_str().ok());

    let authorized = match role {
        RelayNodeRole::Master => {
            if let Some(ref ms) = master_sec {
                mesh_header_secret_ok(provided, ms)
            } else {
                mesh_event_secret()
                    .as_ref()
                    .is_some_and(|s| mesh_header_secret_ok(provided, s.trim()))
            }
        }
        RelayNodeRole::Edge => mesh_event_secret()
            .as_ref()
            .is_some_and(|s| mesh_header_secret_ok(provided, s.trim())),
    };

    if !authorized {
        let hook_disabled = match role {
            RelayNodeRole::Master => master_sec.is_none() && mesh_event_secret().is_none(),
            RelayNodeRole::Edge => mesh_event_secret().is_none(),
        };
        if hook_disabled {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(MeshEventIngestResponse {
                    ok: false,
                    error: Some(
                        "mesh-event hook disabled (set RELAY_MASTER_MESH_SECRET on masters, or RELAY_MESH_EVENT_SECRET / RELAY_PEER_SECRET)"
                            .into(),
                    ),
                }),
            )
                .into_response();
        }
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(MeshEventIngestResponse {
                ok: false,
                error: Some("invalid X-Relay-Mesh-Secret".into()),
            }),
        )
            .into_response();
    }

    tracing::warn!(
        target: "relay_mesh_event",
        payload = %body.to_string(),
        "mesh event received"
    );

    (
        axum::http::StatusCode::OK,
        axum::Json(MeshEventIngestResponse {
            ok: true,
            error: None,
        }),
    )
        .into_response()
}

/// Test helper: same derivation as [`mesh_event_target_urls`] without reading global env for peer list.
pub fn mesh_event_urls_for_peers(
    self_fqdn: Option<&str>,
    peer_list_csv: &str,
    port: u16,
    scheme: &str,
) -> Vec<String> {
    let self_norm = self_fqdn
        .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let mut out = Vec::new();
    for p in peer_list_csv.split(';') {
        let host = p.trim();
        if host.is_empty() {
            continue;
        }
        let host_lc = host.to_ascii_lowercase();
        if self_norm.as_ref().is_some_and(|s| s == &host_lc) {
            continue;
        }
        out.push(format!(
            "{}://{}:{}/hooks/mesh-event",
            scheme, host_lc, port
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn mesh_post_url_host_parses_http_port() {
        assert_eq!(
            mesh_post_url_host("http://atlanta3.example.net:8080/hooks/mesh-event").as_deref(),
            Some("atlanta3.example.net")
        );
        assert_eq!(
            mesh_post_url_host("https://x.example/hooks/mesh-event").as_deref(),
            Some("x.example")
        );
    }

    #[test]
    #[serial]
    fn outbound_secret_uses_master_mesh_for_listed_host() {
        std::env::set_var("RELAY_MASTER_PEER_LIST", "m1.example;m2.example");
        std::env::set_var("RELAY_MASTER_MESH_SECRET", "master-only");
        std::env::remove_var("RELAY_MESH_EVENT_SECRET");
        std::env::set_var("RELAY_PEER_SECRET", "peer-fallback");
        assert_eq!(
            outbound_mesh_secret_for_post_url("http://m1.example:8080/hooks/mesh-event").as_deref(),
            Some("master-only")
        );
        assert_eq!(
            outbound_mesh_secret_for_post_url("http://edge.example:8080/hooks/mesh-event").as_deref(),
            Some("peer-fallback")
        );
        std::env::remove_var("RELAY_MASTER_MESH_SECRET");
        assert_eq!(
            outbound_mesh_secret_for_post_url("http://m1.example:8080/hooks/mesh-event").as_deref(),
            Some("peer-fallback")
        );
        std::env::remove_var("RELAY_MASTER_PEER_LIST");
        std::env::remove_var("RELAY_PEER_SECRET");
    }

    #[test]
    fn slack_text_includes_repo_and_error() {
        let p = MeshEventPayload {
            event_type: "pull_validation_failed".into(),
            source: "peer_hook".into(),
            repo: "relay-master".into(),
            branch: "main".into(),
            error: "trust anchor deadbeef not ancestor".into(),
            relay_server_id: Some("n1".into()),
            node_fqdn: Some("relay.example.net".into()),
            after_tip: Some("abc".into()),
            trigger_peer: Some("peer.example.net".into()),
            channel: Some("#relay-ops".into()),
        };
        let t = slack_text_for_pull_validation(&p);
        assert!(t.contains("relay-master"));
        assert!(t.contains("trust anchor"));
        assert!(t.contains("peer.example.net"));
    }

    #[test]
    fn target_urls_for_peers_skips_self() {
        let u = mesh_event_urls_for_peers(
            Some("jp-osa.example.net"),
            "atlanta3.example.net;jp-osa.example.net",
            8080,
            "http",
        );
        assert_eq!(
            u,
            vec!["http://atlanta3.example.net:8080/hooks/mesh-event"]
        );
    }
}

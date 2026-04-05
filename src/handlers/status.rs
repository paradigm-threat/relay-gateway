//! `POST /api/status` — node and configured master-peer reachability for operators and automation.
//! (**GET** is not used: repo paths could shadow **`/api/*`** on wildcard routes.)
//!
//! - Query **`format=json`** or **`format=html`** overrides **`Accept`** (default JSON when ambiguous).
//! - Query **`strict=1`**: HTTP **503** if any peer is unreachable (otherwise **200** with `healthy: false`).
//! - Env **`RELAY_STATUS_PEER_URL_TEMPLATE`**: default **`https://{host}/api/config`** (probed with **POST**).
//! - Env **`RELAY_STATUS_PROBE_TIMEOUT_SECS`**: default **3**.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::git;
use crate::mesh_role::{self, RelayNodeRole};
use crate::types::AppState;

use super::general::{read_startup_catalog_sync_status, summarize_features};

const DEFAULT_PEER_URL_TEMPLATE: &str = "https://{host}/api/config";

fn peer_probe_url(host: &str) -> String {
    let template = std::env::var("RELAY_STATUS_PEER_URL_TEMPLATE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PEER_URL_TEMPLATE.to_string());
    template.replace("{host}", host.trim())
}

fn master_peer_hosts() -> Vec<String> {
    std::env::var("RELAY_MASTER_PEER_LIST")
        .unwrap_or_default()
        .split(';')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn tls_enabled_runtime() -> bool {
    let c = std::env::var("RELAY_TLS_CERT").ok();
    let k = std::env::var("RELAY_TLS_KEY").ok();
    c.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
        && k.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
}

fn probe_timeout() -> Duration {
    let secs = std::env::var("RELAY_STATUS_PROBE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0 && n <= 120)
        .unwrap_or(3);
    Duration::from_secs(secs)
}

pub(crate) fn wants_html(headers: &HeaderMap, query_format: Option<&str>) -> bool {
    match query_format.map(str::trim).filter(|s| !s.is_empty()) {
        Some("html") | Some("text/html") => return true,
        Some("json") | Some("application/json") => return false,
        _ => {}
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("application/json") {
        return false;
    }
    accept.contains("text/html")
}

#[derive(Serialize)]
struct RemotePeerConfigSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    relay_server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_fqdn: Option<String>,
    repo_count: usize,
}

#[derive(Serialize)]
struct PeerProbe {
    host: String,
    url: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_self: bool,
    reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote: Option<RemotePeerConfigSummary>,
}

#[derive(Serialize)]
struct StatusSelf {
    gateway_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_fqdn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relay_server_id: Option<String>,
    mesh_role: &'static str,
    mesh_http_publish: bool,
    tls_enabled: bool,
    repo_count: usize,
    master_peers_configured: Vec<String>,
    git_ensure_bare_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_features_summary: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup_catalog_sync: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct StatusBody {
    schema_version: u32,
    /// `ok` if every configured peer is reachable or skipped as self; `degraded` otherwise.
    status: &'static str,
    healthy: bool,
    generated_at_unix_secs: u64,
    #[serde(rename = "self")]
    self_: StatusSelf,
    peers: Vec<PeerProbe>,
}

async fn probe_one_peer(
    client: &reqwest::Client,
    host: &str,
    self_fqdn: Option<&str>,
) -> PeerProbe {
    let host_norm = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let url = peer_probe_url(&host_norm);

    let is_self = self_fqdn
        .map(|me| {
            me.trim()
                .trim_end_matches('.')
                .eq_ignore_ascii_case(&host_norm)
        })
        .unwrap_or(false);

    if is_self {
        return PeerProbe {
            host: host_norm.clone(),
            url,
            is_self: true,
            reachable: true,
            http_status: None,
            latency_ms: None,
            error: None,
            remote: None,
        };
    }

    let started = Instant::now();
    match client
        .post(&url)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(resp) => {
            let http_status = resp.status().as_u16();
            let latency_ms = started.elapsed().as_millis() as u64;
            if !resp.status().is_success() {
                return PeerProbe {
                    host: host_norm,
                    url,
                    is_self: false,
                    reachable: false,
                    http_status: Some(http_status),
                    latency_ms: Some(latency_ms),
                    error: Some(format!("HTTP {}", http_status)),
                    remote: None,
                };
            }
            let remote = match resp.json::<serde_json::Value>().await {
                Ok(j) => {
                    let repos = j
                        .get("repos")
                        .and_then(|r| r.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    Some(RemotePeerConfigSummary {
                        relay_server_id: j
                            .get("relay_server_id")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        node_fqdn: j
                            .get("node_fqdn")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        repo_count: repos,
                    })
                }
                Err(e) => {
                    return PeerProbe {
                        host: host_norm,
                        url,
                        is_self: false,
                        reachable: false,
                        http_status: Some(http_status),
                        latency_ms: Some(latency_ms),
                        error: Some(format!("invalid JSON: {}", e)),
                        remote: None,
                    };
                }
            };
            PeerProbe {
                host: host_norm,
                url,
                is_self: false,
                reachable: true,
                http_status: Some(http_status),
                latency_ms: Some(latency_ms),
                error: None,
                remote,
            }
        }
        Err(e) => PeerProbe {
            host: host_norm,
            url,
            is_self: false,
            reachable: false,
            http_status: None,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: Some(e.to_string()),
            remote: None,
        },
    }
}

fn mesh_role_str(r: RelayNodeRole) -> &'static str {
    match r {
        RelayNodeRole::Master => "master",
        RelayNodeRole::Edge => "edge",
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn render_html(body: &StatusBody) -> String {
    let overall = if body.healthy { "OK" } else { "DEGRADED" };
    let overall_class = if body.healthy { "ok" } else { "bad" };
    let s = &body.self_;
    let json_pretty = serde_json::to_string_pretty(body).unwrap_or_else(|_| "{}".into());

    let mut peer_rows = String::new();
    for p in &body.peers {
        let st = if p.reachable { "OK" } else { "DOWN" };
        let cls = if p.reachable { "ok" } else { "bad" };
        let detail = p
            .error
            .as_deref()
            .or_else(|| p.http_status.map(|_| "see http_status"))
            .unwrap_or("");
        let remote = p
            .remote
            .as_ref()
            .map(|r| {
                format!(
                    "{} repos, id={:?}",
                    r.repo_count,
                    r.relay_server_id.as_deref().unwrap_or("—")
                )
            })
            .unwrap_or_else(|| "—".into());
        peer_rows.push_str(&format!(
            r#"<tr><td>{}</td><td class="{}">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>"#,
            html_escape(&p.host),
            cls,
            st,
            html_escape(&p.url),
            p.latency_ms.map(|n| n.to_string()).unwrap_or_else(|| "—".into()),
            html_escape(&remote),
            html_escape(detail),
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <title>Relay status</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 1.5rem; max-width: 56rem; }}
    h1 {{ font-size: 1.25rem; }}
    .ok {{ color: #0a0; }}
    .bad {{ color: #a00; }}
    table {{ border-collapse: collapse; width: 100%; margin: 1rem 0; font-size: 0.9rem; }}
    th, td {{ border: 1px solid #ccc; padding: 0.35rem 0.5rem; text-align: left; }}
    th {{ background: #f4f4f4; }}
    pre {{ background: #f8f8f8; padding: 1rem; overflow: auto; font-size: 0.75rem; }}
    .meta {{ color: #555; font-size: 0.85rem; }}
  </style>
</head>
<body>
  <h1>Relay status — <span class="{}">{}</span></h1>
  <p class="meta">generated_at_unix_secs={} · schema_version={} · <code>POST /api/status</code> (add <code>?format=json</code> for JSON)</p>
  <h2>This node</h2>
  <table>
    <tr><th>gateway_version</th><td>{}</td></tr>
    <tr><th>node_fqdn</th><td>{}</td></tr>
    <tr><th>relay_server_id</th><td>{}</td></tr>
    <tr><th>mesh_role</th><td>{}</td></tr>
    <tr><th>mesh_http_publish</th><td>{}</td></tr>
    <tr><th>tls_enabled</th><td>{}</td></tr>
    <tr><th>repo_count</th><td>{}</td></tr>
    <tr><th>git_ensure_bare_enabled</th><td>{}</td></tr>
  </table>
  <h2>Peers (from RELAY_MASTER_PEER_LIST)</h2>
  <table>
    <tr><th>Host</th><th>Status</th><th>URL</th><th>Latency ms</th><th>Remote</th><th>Error</th></tr>
    {}
  </table>
  <h2>Raw JSON</h2>
  <pre>{}</pre>
</body>
</html>"#,
        overall_class,
        overall,
        body.generated_at_unix_secs,
        body.schema_version,
        html_escape(s.gateway_version),
        html_escape(s.node_fqdn.as_deref().unwrap_or("—")),
        html_escape(s.relay_server_id.as_deref().unwrap_or("—")),
        html_escape(s.mesh_role),
        s.mesh_http_publish,
        s.tls_enabled,
        s.repo_count,
        s.git_ensure_bare_enabled,
        peer_rows,
        html_escape(&json_pretty),
    )
}

/// POST /api/status
pub async fn post_api_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let format_q = query.get("format").map(|s| s.as_str());
    let html = wants_html(&headers, format_q);
    let strict = query
        .get("strict")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let peer_hosts = master_peer_hosts();
    let repos = git::bare_repo_names(&state.repo_path);
    let role = mesh_role::relay_node_role();
    let mesh_http = mesh_role::should_publish_mesh_http_events();

    let self_fqdn = state
        .node_fqdn
        .clone()
        .or_else(mesh_role::node_fqdn_normalized);

    let client = match reqwest::Client::builder()
        .timeout(probe_timeout())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("failed to build HTTP client: {}", e),
            )
                .into_response();
        }
    };

    let mut peers = Vec::new();
    for h in &peer_hosts {
        peers.push(probe_one_peer(&client, h, self_fqdn.as_deref()).await);
    }

    let healthy = peers.iter().all(|p| p.reachable);
    let status = if healthy { "ok" } else { "degraded" };

    let body = StatusBody {
        schema_version: 1,
        status,
        healthy,
        generated_at_unix_secs: now_unix_secs(),
        self_: StatusSelf {
            gateway_version: env!("CARGO_PKG_VERSION"),
            node_fqdn: state.node_fqdn.clone(),
            relay_server_id: state.relay_server_id.clone(),
            mesh_role: mesh_role_str(role),
            mesh_http_publish: mesh_http,
            tls_enabled: tls_enabled_runtime(),
            repo_count: repos.len(),
            master_peers_configured: peer_hosts,
            git_ensure_bare_enabled: state.git_ensure_bare_enabled,
            installed_features_summary: state
                .features_manifest
                .as_ref()
                .map(|m| summarize_features(m)),
            startup_catalog_sync: read_startup_catalog_sync_status(&state.repo_path),
        },
        peers,
    };

    let code = if strict && !body.healthy {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    let overall = if body.healthy { "ok" } else { "degraded" };
    let overall_name = HeaderName::from_static("x-relay-overall-status");
    let overall_value =
        HeaderValue::from_str(overall).unwrap_or_else(|_| HeaderValue::from_static("unknown"));

    let mut resp: Response = if html {
        Html(render_html(&body)).into_response()
    } else {
        Json(body).into_response()
    };
    *resp.status_mut() = code;
    resp.headers_mut().insert(overall_name, overall_value);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hdr_accept(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::ACCEPT,
            HeaderValue::from_str(v).expect("header value"),
        );
        h
    }

    #[test]
    fn wants_html_query_overrides() {
        assert!(wants_html(&HeaderMap::new(), Some("html")));
        assert!(!wants_html(&HeaderMap::new(), Some("json")));
        assert!(wants_html(&hdr_accept("*/*"), Some("html")));
    }

    #[test]
    fn wants_html_accept_prefers_json_when_both_mentioned() {
        let h = hdr_accept("text/html, application/json;q=0.9");
        assert!(!wants_html(&h, None));
    }

    #[test]
    fn wants_html_accept_html_only() {
        let h = hdr_accept("text/html");
        assert!(wants_html(&h, None));
    }

    #[test]
    fn wants_html_default_json() {
        assert!(!wants_html(&HeaderMap::new(), None));
        assert!(!wants_html(&hdr_accept("*/*"), None));
    }
}

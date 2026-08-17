//! `GET`/`HEAD`/`POST /api/status` — node, per-repo git tips, IPFS seed summary, and
//! configured master-peer reachability.
//!
//! Operator routes are bound to the **node FQDN** (and loopback). Parked domains and
//! `{repo}.{node}` Hosts keep `/api/*` as site files / GET hooks.
//!
//! Per-repo rows are read from **local** bares (libgit2), not `git ls-remote`, so one
//! public request can list every catalog + on-disk repo without using git-daemon slots.
//!
//! - Query **`format=json`** or **`format=html`** overrides **`Accept`** (default JSON when ambiguous).
//! - Query **`strict=1`**: HTTP **503** if any peer is unreachable (otherwise **200** with `healthy: false`).
//!   Missing catalog bares set `repos_ok: false` but do **not** change `healthy` / `strict`.
//!   IPFS daemon-down sets `ipfs_ok: false` and does **not** affect `healthy` / `strict`.
//! - Query **`light=1`** (or **`peers=0`**): skip remote peer probes and the IPFS daemon check so
//!   clients can measure their own RTT. `healthy` is then `true` (no peers were probed).
//! - Env **`RELAY_STATUS_PEER_URL_TEMPLATE`**: default **`https://{host}/api/config`** (probed with **POST**).
//! - Env **`RELAY_STATUS_PROBE_TIMEOUT_SECS`**: default **3**. Peer probes run **in parallel**.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use crate::types::{AppState, ServeContext};

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

fn wants_light(query: &HashMap<String, String>) -> bool {
    query
        .get("light")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || query
            .get("peers")
            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
            .unwrap_or(false)
}

async fn probe_peers_parallel(
    client: &reqwest::Client,
    hosts: &[String],
    self_fqdn: Option<&str>,
) -> Vec<PeerProbe> {
    if hosts.is_empty() {
        return Vec::new();
    }
    let mut set = tokio::task::JoinSet::new();
    for (i, h) in hosts.iter().cloned().enumerate() {
        let c = client.clone();
        let me = self_fqdn.map(str::to_string);
        set.spawn(async move {
            let p = probe_one_peer(&c, &h, me.as_deref()).await;
            (i, p)
        });
    }
    let mut indexed: Vec<(usize, PeerProbe)> = Vec::with_capacity(hosts.len());
    while let Some(joined) = set.join_next().await {
        if let Ok(pair) = joined {
            indexed.push(pair);
        }
    }
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, p)| p).collect()
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
    /// Provenance from pack-linode-deploy.sh (`deploy-manifest.json`).
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy_manifest: Option<serde_json::Value>,
    ipfs: IpfsSelfStatus,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
struct IpfsSelfStatus {
    enabled: bool,
    ok: bool,
    api_configured: bool,
    api_reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_id: Option<String>,
    gateway: String,
    cache_root: String,
    cache_cid_count: usize,
    in_flight_count: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoIpfsStatus {
    pub thin: bool,
    pub cid_count: usize,
    pub cached_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cids: Vec<String>,
}

const MAX_PUBLIC_CIDS: usize = 8;

/// One catalog and/or on-disk repo, from local git (not git-daemon).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoStatus {
    pub name: String,
    pub on_disk: bool,
    pub in_catalog: bool,
    /// Catalog repo: on disk and `publish_branch` tip exists. Extra (non-catalog): on disk.
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_sha: Option<String>,
    /// Catalog public blurb (`catalog/repos/*.json` `description`). Independent of on-disk state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub branch_count: usize,
    /// Present when the publish/HEAD tip references IPFS pointers or a manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipfs: Option<RepoIpfsStatus>,
}

#[derive(Serialize)]
struct StatusBody {
    schema_version: u32,
    /// `ok` if every configured peer is reachable or skipped as self; `degraded` otherwise.
    status: &'static str,
    healthy: bool,
    /// False when Kubo is configured but not reachable. Does **not** affect `healthy` / `strict`.
    ipfs_ok: bool,
    /// False when any **catalog** repo is missing on disk or has no `publish_branch` tip.
    repos_ok: bool,
    generated_at_unix_secs: u64,
    #[serde(rename = "self")]
    self_: StatusSelf,
    peers: Vec<PeerProbe>,
    repos: Vec<RepoStatus>,
}

/// Public status lists at most this many repos (catalog ∪ on-disk).
const MAX_PUBLIC_REPOS: usize = 512;

fn is_safe_repo_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.contains(['\0', '\n', '\r', '/', '\\', '<', '>'])
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn is_safe_branch(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('/')
        && !s.contains(['\0', '\n', '\r', ' ', '<', '>'])
}

fn is_hex_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn tip_sha(repo: &git2::Repository, branch: &str) -> Option<String> {
    if !is_safe_branch(branch) {
        return None;
    }
    let sha = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .ok()?
        .peel_to_commit()
        .ok()
        .map(|c| c.id().to_string())?;
    is_hex_oid(&sha).then_some(sha)
}

/// Union of on-disk bares and catalog `authorized_repos`, with HEAD / publish tips from libgit2.
pub(crate) fn collect_repo_status(state: &AppState) -> Vec<RepoStatus> {
    let on_disk: BTreeSet<String> = git::repo_names_for_state(state).into_iter().collect();
    let catalog: BTreeMap<String, (String, Option<String>)> = state
        .authorized_repos
        .as_ref()
        .map(|a| {
            a.repos
                .iter()
                .map(|(k, v)| (k.clone(), (v.branch.clone(), v.description.clone())))
                .collect()
        })
        .unwrap_or_default();

    let mut names = on_disk.clone();
    names.extend(catalog.keys().cloned());

    names
        .into_iter()
        .filter(|name| is_safe_repo_name(name))
        .take(MAX_PUBLIC_REPOS)
        .map(|name| {
            let in_catalog = catalog.contains_key(&name);
            let present = on_disk.contains(&name);
            let (catalog_branch, description) = match catalog.get(&name) {
                Some((b, d)) => (Some(b.clone()), d.clone()),
                None => (None, None),
            };

            let (head_branch, head_sha, publish_sha, branch_count, ipfs) = if present {
                if let Some(repo) = git::open_repo_for_state(state, &name) {
                    let branch_count = git::list_branches(&repo).len();
                    let head_branch = match &state.serve_context {
                        ServeContext::LocalWorktree {
                            repo_name,
                            checked_out_branch,
                            ..
                        } if repo_name == &name => Some(checked_out_branch.clone()),
                        _ => git::default_branch_from_bare_head(&state.repo_path, &name),
                    };
                    let head_branch = head_branch.filter(|b| is_safe_branch(b));
                    let head_sha = head_branch.as_deref().and_then(|b| tip_sha(&repo, b));
                    let publish_ref = catalog_branch
                        .as_deref()
                        .filter(|b| is_safe_branch(b))
                        .or(head_branch.as_deref());
                    let publish_sha = publish_ref.and_then(|b| tip_sha(&repo, b));
                    let ipfs = publish_ref.and_then(|b| repo_ipfs_status(&repo, b));
                    (head_branch, head_sha, publish_sha, branch_count, ipfs)
                } else {
                    (None, None, None, 0, None)
                }
            } else {
                (None, None, None, 0, None)
            };

            let publish_branch = catalog_branch
                .filter(|b| is_safe_branch(b))
                .or_else(|| head_branch.clone());
            let ok = if in_catalog {
                present && publish_sha.is_some()
            } else {
                present
            };

            RepoStatus {
                name,
                on_disk: present,
                in_catalog,
                ok,
                head_branch,
                head_sha,
                publish_branch,
                publish_sha,
                description,
                branch_count,
                ipfs,
            }
        })
        .collect()
}

fn repo_ipfs_status(repo: &git2::Repository, branch: &str) -> Option<RepoIpfsStatus> {
    let cids = git::ipfs::collect_cids_from_repo(repo, branch);
    if cids.is_empty() {
        return None;
    }
    let cached_count = cids.iter().filter(|c| git::ipfs::cid_is_cached(c)).count();
    Some(RepoIpfsStatus {
        thin: true,
        cid_count: cids.len(),
        cached_count,
        cids: cids.into_iter().take(MAX_PUBLIC_CIDS).collect(),
    })
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

async fn collect_ipfs_self() -> IpfsSelfStatus {
    let api = std::env::var("RELAY_IPFS_API")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());
    let enabled = api.is_some();
    let mut api_reachable = false;
    let mut peer_id = None;
    if let Some(ref base) = api {
        if let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
        {
            let url = format!("{base}/api/v0/id");
            if let Ok(resp) = client.post(&url).send().await {
                api_reachable = resp.status().is_success();
                if let Ok(j) = resp.json::<serde_json::Value>().await {
                    peer_id = j
                        .get("ID")
                        .or_else(|| j.get("id"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
        }
    }
    IpfsSelfStatus {
        enabled,
        ok: !enabled || api_reachable,
        api_configured: enabled,
        api_reachable,
        peer_id,
        gateway: git::ipfs::gateway_base_from_env(),
        cache_root: git::ipfs::cache_root_from_env().display().to_string(),
        cache_cid_count: git::ipfs::cache_cid_count(),
        in_flight_count: git::ipfs::in_flight_count(),
    }
}

/// No daemon round-trip. Used by `light=1` so client RTT is not the IPFS probe.
fn collect_ipfs_self_skipped() -> IpfsSelfStatus {
    let enabled = std::env::var("RELAY_IPFS_API")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .is_some();
    IpfsSelfStatus {
        enabled,
        ok: true,
        api_configured: enabled,
        api_reachable: false,
        peer_id: None,
        gateway: git::ipfs::gateway_base_from_env(),
        cache_root: git::ipfs::cache_root_from_env().display().to_string(),
        cache_cid_count: git::ipfs::cache_cid_count(),
        in_flight_count: git::ipfs::in_flight_count(),
    }
}

fn read_deploy_manifest() -> Option<serde_json::Value> {
    let root = std::env::var("RELAY_INSTALL_ROOT").unwrap_or_else(|_| "/opt/relay".into());
    let path = std::path::Path::new(&root).join("state/deploy-manifest.json");
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
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

fn short_sha(sha: Option<&str>) -> String {
    match sha {
        Some(s) if s.len() > 12 => s[..12].to_string(),
        Some(s) => s.to_string(),
        None => "—".into(),
    }
}

fn render_html(body: &StatusBody) -> String {
    let overall = if body.healthy { "OK" } else { "DEGRADED" };
    let overall_class = if body.healthy { "ok" } else { "bad" };
    let repos_label = if body.repos_ok { "OK" } else { "SYNC" };
    let repos_class = if body.repos_ok { "ok" } else { "warn" };
    let s = &body.self_;
    let json_pretty = serde_json::to_string_pretty(body).unwrap_or_else(|_| "{}".into());

    let mut repo_rows = String::new();
    for r in &body.repos {
        let st = if r.ok {
            if r.in_catalog {
                "OK"
            } else {
                "extra"
            }
        } else if !r.on_disk {
            "missing"
        } else {
            "no tip"
        };
        let cls = if r.ok {
            if r.in_catalog {
                "ok"
            } else {
                "warn"
            }
        } else {
            "bad"
        };
        repo_rows.push_str(&format!(
            r#"<tr><td>{}</td><td class="{}">{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>"#,
            html_escape(&r.name),
            cls,
            st,
            if r.in_catalog { "yes" } else { "no" },
            if r.on_disk { "yes" } else { "no" },
            html_escape(r.publish_branch.as_deref().unwrap_or("—")),
            html_escape(&short_sha(r.publish_sha.as_deref())),
            r.branch_count,
            html_escape(&short_sha(r.head_sha.as_deref())),
            r.ipfs
                .as_ref()
                .map(|i| {
                    let links: Vec<String> = i
                        .cids
                        .iter()
                        .map(|c| {
                            let short = if c.len() > 12 { &c[..12] } else { c.as_str() };
                            format!(
                                r#"<a href="/ipfs/{}?relayAssetStatus=1"><code>{}</code></a>"#,
                                html_escape(c),
                                html_escape(short)
                            )
                        })
                        .collect();
                    let extra = if links.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", links.join(" "))
                    };
                    format!(
                        "IPFS seed {}/{} cached{}",
                        i.cached_count, i.cid_count, extra
                    )
                })
                .unwrap_or_else(|| "—".into()),
        ));
    }
    if repo_rows.is_empty() {
        repo_rows.push_str(r#"<tr><td colspan="9">No catalog or on-disk repos</td></tr>"#);
    }

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

    let ipfs_label = if body.ipfs_ok { "OK" } else { "DOWN" };
    let ipfs_class = if body.ipfs_ok { "ok" } else { "warn" };
    let ipfs = &s.ipfs;
    let ipfs_cids: String = ipfs
        .peer_id
        .as_deref()
        .map(|p| html_escape(p))
        .unwrap_or_else(|| "—".into());

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <title>Relay status</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 1.5rem; max-width: 72rem; }}
    h1 {{ font-size: 1.25rem; }}
    .ok {{ color: #0a0; }}
    .warn {{ color: #a60; }}
    .bad {{ color: #a00; }}
    table {{ border-collapse: collapse; width: 100%; margin: 1rem 0; font-size: 0.9rem; }}
    th, td {{ border: 1px solid #ccc; padding: 0.35rem 0.5rem; text-align: left; }}
    th {{ background: #f4f4f4; }}
    pre {{ background: #f8f8f8; padding: 1rem; overflow: auto; font-size: 0.75rem; }}
    .meta {{ color: #555; font-size: 0.85rem; }}
  </style>
</head>
<body>
  <h1>Relay status — <span class="{}">{}</span> · repos <span class="{}">{}</span> · IPFS seed <span class="{}">{}</span></h1>
  <p class="meta">generated_at_unix_secs={} · schema_version={} · <code>GET /api/status</code> on this node FQDN (add <code>?format=json</code> for JSON). Parked Hosts serve site files at this path.</p>
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
  <h2>IPFS seed (Kubo + disk cache)</h2>
  <p class="meta">Git-ops <code>ensure-bare</code> seed is unrelated. This is CID availability on this node.</p>
  <table>
    <tr><th>ok</th><td class="{}">{}</td></tr>
    <tr><th>enabled</th><td>{}</td></tr>
    <tr><th>api_reachable</th><td>{}</td></tr>
    <tr><th>peer_id</th><td><code>{}</code></td></tr>
    <tr><th>gateway</th><td><code>{}</code></td></tr>
    <tr><th>cache_root</th><td><code>{}</code></td></tr>
    <tr><th>cache_cid_count</th><td>{}</td></tr>
    <tr><th>in_flight_count</th><td>{}</td></tr>
  </table>
  <h2>Repos (local git, not git-daemon)</h2>
  <table>
    <tr><th>Repo</th><th>Status</th><th>Catalog</th><th>On disk</th><th>Publish</th><th>SHA</th><th>Branches</th><th>HEAD</th><th>IPFS seed</th></tr>
    {}
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
        repos_class,
        repos_label,
        ipfs_class,
        ipfs_label,
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
        ipfs_class,
        if ipfs.ok { "OK" } else { "DOWN" },
        ipfs.enabled,
        ipfs.api_reachable,
        ipfs_cids,
        html_escape(&ipfs.gateway),
        html_escape(&ipfs.cache_root),
        ipfs.cache_cid_count,
        ipfs.in_flight_count,
        repo_rows,
        peer_rows,
        html_escape(&json_pretty),
    )
}

/// GET/HEAD/POST `/api/status` on the node FQDN (or loopback). Other Hosts: site files or 404.
pub async fn api_status(
    State(state): State<AppState>,
    method: axum::http::Method,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    if let Some(resp) = super::node_api::if_not_node_ops(
        state.clone(),
        method.clone(),
        headers.clone(),
        "api/status".into(),
        Some(Query(query.clone())),
        body,
    )
    .await
    {
        return resp;
    }
    if method == axum::http::Method::PUT || method == axum::http::Method::DELETE {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD, POST")],
            "Use GET, HEAD, or POST for /api/status on the node Host",
        )
            .into_response();
    }
    let resp = post_api_status(State(state), headers, Query(query))
        .await
        .into_response();
    if method == axum::http::Method::HEAD {
        return super::node_api::empty_head(resp);
    }
    resp
}

/// POST /api/status (also used as the inner body for GET/HEAD on the node Host).
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
    let light = wants_light(&query);

    let peer_hosts = master_peer_hosts();
    let repo_rows = collect_repo_status(&state);
    let repo_count = repo_rows.iter().filter(|r| r.on_disk).count();
    let repos_ok = repo_rows.iter().filter(|r| r.in_catalog).all(|r| r.ok);
    let role = mesh_role::relay_node_role();
    let mesh_http = mesh_role::should_publish_mesh_http_events();

    let self_fqdn = state
        .node_fqdn
        .clone()
        .or_else(mesh_role::node_fqdn_normalized);

    let (peers, ipfs_self) = if light {
        (Vec::new(), collect_ipfs_self_skipped())
    } else {
        let client = match reqwest::Client::builder().timeout(probe_timeout()).build() {
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
        tokio::join!(
            probe_peers_parallel(&client, &peer_hosts, self_fqdn.as_deref()),
            collect_ipfs_self()
        )
    };

    let healthy = peers.iter().all(|p| p.reachable);
    let status = if healthy { "ok" } else { "degraded" };
    let ipfs_ok = ipfs_self.ok;

    let body = StatusBody {
        schema_version: 3,
        status,
        healthy,
        ipfs_ok,
        repos_ok,
        generated_at_unix_secs: now_unix_secs(),
        self_: StatusSelf {
            gateway_version: env!("CARGO_PKG_VERSION"),
            node_fqdn: state.node_fqdn.clone(),
            relay_server_id: state.relay_server_id.clone(),
            mesh_role: mesh_role_str(role),
            mesh_http_publish: mesh_http,
            tls_enabled: tls_enabled_runtime(),
            repo_count,
            master_peers_configured: peer_hosts,
            git_ensure_bare_enabled: state.git_ensure_bare_enabled,
            installed_features_summary: state
                .features_manifest
                .as_ref()
                .map(|m| summarize_features(m)),
            startup_catalog_sync: read_startup_catalog_sync_status(&state.repo_path),
            deploy_manifest: read_deploy_manifest(),
            ipfs: ipfs_self,
        },
        peers,
        repos: repo_rows,
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
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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

    #[test]
    fn wants_light_query() {
        let mut q = HashMap::new();
        assert!(!wants_light(&q));
        q.insert("light".into(), "1".into());
        assert!(wants_light(&q));
        q.clear();
        q.insert("peers".into(), "0".into());
        assert!(wants_light(&q));
    }

    fn status_state(repo_path: std::path::PathBuf) -> AppState {
        AppState {
            repo_path,
            serve_context: ServeContext::BareFleet,
            static_paths: Vec::new(),
            node_fqdn: Some("test.local".to_string()),
            relay_server_id: None,
            authorized_repos: None,
            parked_domains: None,
            features_manifest: None,
            git_ensure_bare_enabled: false,
            web_assets_repo: None,
            web_assets_branch: "release".to_string(),
        }
    }

    fn init_bare_with_commit(root: &std::path::Path, name: &str, branch: &str) -> String {
        let path = root.join(format!("{name}.git"));
        let repo = git2::Repository::init_bare(&path).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let blob = repo.blob(b"hi").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("README", blob, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(
                Some(&format!("refs/heads/{branch}")),
                &sig,
                &sig,
                "init",
                &tree,
                &[],
            )
            .unwrap();
        repo.set_head(&format!("refs/heads/{branch}")).unwrap();
        oid.to_string()
    }

    #[test]
    fn collect_repo_status_lists_disk_and_catalog() {
        use crate::authorized_repos::{AuthorizedReposFile, RepoAnchor};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let sha = init_bare_with_commit(dir.path(), "present", "main");
        init_bare_with_commit(dir.path(), "extra", "main");

        let mut catalog = std::collections::HashMap::new();
        catalog.insert(
            "present".into(),
            RepoAnchor {
                anchor_commit: "a".repeat(40),
                branch: "main".into(),
                description: Some("Present on disk.".into()),
            },
        );
        catalog.insert(
            "missing".into(),
            RepoAnchor {
                anchor_commit: "b".repeat(40),
                branch: "main".into(),
                description: Some("Catalog only.".into()),
            },
        );

        let mut state = status_state(dir.path().to_path_buf());
        state.authorized_repos = Some(Arc::new(AuthorizedReposFile {
            relay_server_id: None,
            repos: catalog,
        }));

        let rows = collect_repo_status(&state);
        assert_eq!(rows.len(), 3);

        let present = rows.iter().find(|r| r.name == "present").unwrap();
        assert!(present.on_disk && present.in_catalog && present.ok);
        assert_eq!(present.publish_branch.as_deref(), Some("main"));
        assert_eq!(present.publish_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(present.description.as_deref(), Some("Present on disk."));
        assert_eq!(present.branch_count, 1);

        let missing = rows.iter().find(|r| r.name == "missing").unwrap();
        assert!(!missing.on_disk && missing.in_catalog && !missing.ok);
        assert_eq!(missing.description.as_deref(), Some("Catalog only."));

        let extra = rows.iter().find(|r| r.name == "extra").unwrap();
        assert!(extra.on_disk && !extra.in_catalog && extra.ok);

        assert!(!rows.iter().filter(|r| r.in_catalog).all(|r| r.ok));

        let html = render_html(&StatusBody {
            schema_version: 3,
            status: "ok",
            healthy: true,
            ipfs_ok: false,
            repos_ok: false,
            generated_at_unix_secs: 0,
            self_: StatusSelf {
                gateway_version: "0",
                node_fqdn: None,
                relay_server_id: None,
                mesh_role: "master",
                mesh_http_publish: false,
                tls_enabled: false,
                repo_count: 2,
                master_peers_configured: vec![],
                git_ensure_bare_enabled: false,
                installed_features_summary: None,
                startup_catalog_sync: None,
                deploy_manifest: None,
                ipfs: IpfsSelfStatus {
                    enabled: true,
                    ok: false,
                    api_configured: true,
                    api_reachable: false,
                    peer_id: None,
                    gateway: "http://127.0.0.1:8081".into(),
                    cache_root: "/tmp/ipfs-cache".into(),
                    cache_cid_count: 0,
                    in_flight_count: 0,
                },
            },
            peers: vec![],
            repos: rows,
        });
        assert!(html.contains("present"));
        assert!(html.contains("missing"));
        assert!(html.contains(">extra<"));
        assert!(html.contains("IPFS seed"));
        assert!(html.contains("Relay status —"));
        assert!(!html.contains("DEGRADED"));
        assert!(html.contains(">OK</span>"));
    }

    #[test]
    fn collect_repo_status_includes_ipfs_cids_from_manifest() {
        use crate::git::ipfs::{format_pointer_stub, IpfsPointer};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thin.git");
        let repo = git2::Repository::init_bare(&path).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let stub = format_pointer_stub(&IpfsPointer {
            cid: cid.into(),
            content_type: Some("image/png".into()),
            sha256: None,
        });
        let manifest = serde_json::json!({
            "schema": "relay.assets-manifest.v1",
            "assets": [{ "path": "cover.png", "ipfs": cid }]
        })
        .to_string();
        let blob_man = repo.blob(manifest.as_bytes()).unwrap();
        let blob_stub = repo.blob(stub.as_bytes()).unwrap();
        let mut assets_tb = repo.treebuilder(None).unwrap();
        assets_tb
            .insert("hero.png.ipfs", blob_stub, 0o100644)
            .unwrap();
        let assets_id = assets_tb.write().unwrap();
        let mut relay_tb = repo.treebuilder(None).unwrap();
        relay_tb.insert("assets", assets_id, 0o040000).unwrap();
        let relay_id = relay_tb.write().unwrap();
        let mut root = repo.treebuilder(None).unwrap();
        root.insert(".relay", relay_id, 0o040000).unwrap();
        root.insert("assets.manifest.json", blob_man, 0o100644)
            .unwrap();
        let tree_id = root.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "thin", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();

        let rows = collect_repo_status(&status_state(dir.path().to_path_buf()));
        let thin = rows.iter().find(|r| r.name == "thin").unwrap();
        let ipfs = thin.ipfs.as_ref().expect("ipfs summary");
        assert!(ipfs.thin);
        assert_eq!(ipfs.cid_count, 1);
        assert!(ipfs.cids.iter().any(|c| c == cid));

        let html = render_html(&StatusBody {
            schema_version: 3,
            status: "ok",
            healthy: true,
            ipfs_ok: true,
            repos_ok: true,
            generated_at_unix_secs: 0,
            self_: StatusSelf {
                gateway_version: "0",
                node_fqdn: None,
                relay_server_id: None,
                mesh_role: "master",
                mesh_http_publish: false,
                tls_enabled: false,
                repo_count: 1,
                master_peers_configured: vec![],
                git_ensure_bare_enabled: false,
                installed_features_summary: None,
                startup_catalog_sync: None,
                deploy_manifest: None,
                ipfs: IpfsSelfStatus {
                    enabled: true,
                    ok: true,
                    api_configured: true,
                    api_reachable: true,
                    peer_id: None,
                    gateway: "http://127.0.0.1:8081".into(),
                    cache_root: "/tmp/ipfs-cache".into(),
                    cache_cid_count: 0,
                    in_flight_count: 0,
                },
            },
            peers: vec![],
            repos: rows,
        });
        assert!(html.contains(&format!("/ipfs/{cid}?relayAssetStatus=1")));
    }

    #[test]
    fn collect_repo_status_skips_unsafe_names() {
        let dir = tempfile::tempdir().unwrap();
        init_bare_with_commit(dir.path(), "ok-repo", "main");
        let evil = dir.path().join("not a repo.git");
        std::fs::create_dir(&evil).unwrap();
        let rows = collect_repo_status(&status_state(dir.path().to_path_buf()));
        assert!(rows.iter().any(|r| r.name == "ok-repo"));
        assert!(rows.iter().all(|r| r.name != "not a repo"));
    }
}

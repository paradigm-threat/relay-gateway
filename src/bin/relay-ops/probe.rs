use crate::topology::{FleetFile, FleetNode};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::timeout;

/// Stay well under git daemon `--max-connections` (default 32) so probes do not
/// crowd out mesh fetch / receive-pack.
const LS_REMOTE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub struct NodeProbe {
    pub fqdn: String,
    pub dns: Vec<String>,
    pub tcp22: bool,
    pub tcp443: bool,
    pub tcp_git: bool,
    pub https_config: Option<ConfigProbe>,
    pub git_tips: Vec<GitTip>,
    pub healthy: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub missing_repos: Vec<String>,
    #[serde(default)]
    pub drift: Vec<RepoDrift>,
    /// Unhealthy publisher/replica ordering (not ordinary per-node drift).
    #[serde(default)]
    pub sync_anomalies: Vec<SyncAnomaly>,
    /// False when this up node is missing catalog repos or git tips disagree with the fleet.
    #[serde(default = "sync_ok_default")]
    pub sync_ok: bool,
    /// False when SearchStore heads lag git tips on any probed repo (schema ≥ 4 status).
    #[serde(default = "sync_ok_default")]
    pub index_ok: bool,
    #[serde(default)]
    pub index_stale_repos: Vec<IndexStaleRepo>,
    /// Repos with background `index_pending` from `/api/status`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_pending_repos: Vec<String>,
    /// Last SearchStore warm errors from status (`repo: error`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_errors: Vec<String>,
    /// `master_peers_configured` from this node's `/api/status` (ghost peers matter for publish).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_peers: Vec<String>,
    /// Configured peers that `/api/status` marked unreachable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreachable_peers: Vec<String>,
    /// Origin HTTP traffic aggregates from `/api/status` `http` (schema ≥ 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpTrafficProbe>,
    /// Optional 24h RUM p75 from `GET /api/rum` when enabled on the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rum: Option<RumProbe>,
    /// Kubo reachable (`ipfs_ok` on `/api/status`). `None` = status missing field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipfs_ok: Option<bool>,
    /// Per-repo thin IPFS seed lag from status `repos[].ipfs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ipfs_repos: Vec<RepoIpfsProbe>,
    /// Admin SSH probe (Observer leaves this unset).
    #[serde(default)]
    pub ssh_ok: Option<bool>,
    /// `~/.ssh/known_hosts` key does not match the live host.
    #[serde(default)]
    pub host_key_changed: bool,
    /// `Some(false)` = SSH worked and `/opt/relay` is missing (empty VM).
    #[serde(default)]
    pub install_present: Option<bool>,
    /// Bare repos under data root not owned by `relay` (SSH admin probe).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ownership_bad_repos: Vec<String>,
    /// Parked apex TLS via this node's IP (`--resolve` SNI). `None` = not probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_tls_ok: Option<bool>,
    /// Domain used for `park_tls_ok` (first `health.sslDomains` entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_tls_domain: Option<String>,
}

#[allow(dead_code)]
fn sync_ok_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoDrift {
    pub repo: String,
    pub branch: String,
    pub sha: Option<String>,
    pub expected_sha: String,
    /// How this drift was classified (publisher-aware mesh rules).
    #[serde(default)]
    pub kind: DriftKind,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// No `publish_origin`; compared against fleet majority tip.
    #[default]
    TipMismatch,
    /// Non-publisher replica behind its publish origin.
    ReplicaBehind,
    /// Non-publisher replica ahead of publish origin (mesh anomaly; do not sync down).
    ReplicaAhead,
}

/// Unhealthy mesh: publish origin tip is behind a non-publisher replica.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncAnomaly {
    pub kind: String,
    pub repo: String,
    pub branch: String,
    pub publisher_fqdn: String,
    pub publisher_sha: String,
    pub replica_fqdn: String,
    pub replica_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogRepoInfo {
    pub name: String,
    pub publish_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_origin: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitRef {
    pub name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigProbe {
    pub ok: bool,
    pub relay_server_id: Option<String>,
    pub peers: Vec<String>,
    pub status: u16,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub authorized_repos: Vec<String>,
    #[serde(default)]
    pub catalog_repos: Vec<CatalogRepoInfo>,
}

/// Subset of gateway `POST /api/status` used for local per-repo tips (schema ≥ 2).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusRepo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub on_disk: bool,
    #[serde(default)]
    pub in_catalog: bool,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub publish_branch: Option<String>,
    #[serde(default)]
    pub publish_sha: Option<String>,
    #[serde(default)]
    pub branch_count: usize,
    #[serde(default)]
    pub indexed_head_sha: Option<String>,
    #[serde(default)]
    pub index_fresh: Option<bool>,
    #[serde(default)]
    pub index_pending: Option<bool>,
    #[serde(default)]
    pub index_last_error: Option<String>,
    #[serde(default)]
    pub index_doc_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipfs: Option<RepoIpfsProbe>,
}

/// Thin IPFS seed summary for one catalog repo (`repos[].ipfs` on `/api/status`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RepoIpfsProbe {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub thin: bool,
    #[serde(default)]
    pub cid_count: usize,
    #[serde(default)]
    pub cached_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusPeerProbe {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub fqdn: String,
    #[serde(default)]
    pub reachable: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusProbe {
    pub ok: bool,
    pub schema_version: u32,
    pub repos_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipfs_ok: Option<bool>,
    pub repos: Vec<StatusRepo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpTrafficProbe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub master_peers_configured: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<StatusPeerProbe>,
}

/// Subset of gateway `/api/status` `http` object (schema ≥ 5).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpTrafficProbe {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub req_1m: u64,
    #[serde(default)]
    pub req_1h: u64,
    #[serde(default)]
    pub req_24h: u64,
    #[serde(default)]
    pub p50_ms: Option<u32>,
    #[serde(default)]
    pub p95_ms: Option<u32>,
    #[serde(default)]
    pub window_1h: HttpWindowProbe,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_hosts: Vec<HostCountProbe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostWindowProbe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_paths: Vec<PathCountProbe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpWindowProbe {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub status_2xx: u64,
    #[serde(default)]
    pub status_3xx: u64,
    #[serde(default)]
    pub status_4xx: u64,
    #[serde(default)]
    pub status_5xx: u64,
    #[serde(default)]
    pub api: u64,
    #[serde(default)]
    pub asset: u64,
    #[serde(default)]
    pub html: u64,
    #[serde(default)]
    pub other: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostCountProbe {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostWindowProbe {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub req_1h: u64,
    #[serde(default)]
    pub html: u64,
    #[serde(default)]
    pub api: u64,
    #[serde(default)]
    pub asset: u64,
    #[serde(default)]
    pub s5xx: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathCountProbe {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RumProbe {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub events: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p75_ttfc_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p75_lcp_ms: Option<f64>,
    /// Theater / client typed events (24h), when present in `/api/rum` summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theater24h: Option<TheaterRumSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TheaterRumSummary {
    #[serde(default)]
    pub major: u64,
    #[serde(default)]
    pub minor: u64,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub by_code: std::collections::BTreeMap<String, u64>,
    /// Nested `major` / `minor` → code → count (from `/api/rum` `bySeverityCode`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub by_severity_code: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>>,
}

/// SearchStore indexed head lags git tip for this repo on the node.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexStaleRepo {
    pub repo: String,
    pub branch: String,
    pub tip_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_head_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitTip {
    pub repo: String,
    pub sha: Option<String>,
    #[serde(default)]
    pub refs: Vec<GitRef>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngressProbe {
    pub fqdn: String,
    pub dns: Vec<String>,
}

pub async fn probe_fleet(
    fleet: &FleetFile,
    gitlab_url: Option<&str>,
) -> (Vec<NodeProbe>, Option<IngressProbe>, Vec<String>) {
    let git_port = fleet.git_port();
    let repos = fleet.git_repos.clone();
    let park_domain = fleet
        .health
        .ssl_domains
        .first()
        .cloned()
        .or_else(|| Some("paradigmthreat.net".into()));
    let mut set = tokio::task::JoinSet::new();
    for n in fleet.nodes.clone() {
        let repos = repos.clone();
        let park_domain = park_domain.clone();
        set.spawn(async move { probe_node(&n, git_port, &repos, park_domain.as_deref()).await });
    }
    let mut nodes = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(n) = res {
            nodes.push(n);
        }
    }
    nodes.sort_by(|a, b| a.fqdn.cmp(&b.fqdn));
    let forge_tips = probe_forge_tips(gitlab_url, &repos).await;
    apply_catalog_sync(&mut nodes, &repos, &forge_tips);
    let mut live_peers = Vec::new();
    for n in &nodes {
        if let Some(cfg) = &n.https_config {
            for p in &cfg.peers {
                if !live_peers
                    .iter()
                    .any(|x: &String| x.eq_ignore_ascii_case(p))
                {
                    live_peers.push(p.clone());
                }
            }
        }
    }
    let ingress = if let Some(fqdn) = fleet.fleet_ingress.clone() {
        Some(IngressProbe {
            dns: resolve_ips(&fqdn).await,
            fqdn,
        })
    } else {
        None
    };
    (nodes, ingress, live_peers)
}

/// Sleep this many seconds until the next background probe.
pub(crate) fn next_probe_secs(nodes: &[NodeProbe], up_secs: u64, down_secs: u64) -> u64 {
    if nodes.is_empty() || nodes.iter().any(|n| !n.healthy || n.dns.is_empty()) {
        down_secs
    } else {
        up_secs
    }
}

fn unreachable_probe(
    fqdn: String,
    dns: Vec<String>,
    tcp22: bool,
    tcp443: bool,
    tcp_git: bool,
) -> NodeProbe {
    NodeProbe {
        fqdn,
        dns,
        tcp22,
        tcp443,
        tcp_git,
        https_config: None,
        git_tips: Vec::new(),
        healthy: false,
        error: None,
        missing_repos: Vec::new(),
        drift: Vec::new(),
        sync_anomalies: Vec::new(),
        sync_ok: true,
        index_ok: true,
        index_stale_repos: Vec::new(),
        index_pending_repos: Vec::new(),
        index_errors: Vec::new(),
        configured_peers: Vec::new(),
        unreachable_peers: Vec::new(),
        http: None,
        rum: None,
        ipfs_ok: None,
        ipfs_repos: Vec::new(),
        ssh_ok: None,
        host_key_changed: false,
        install_present: None,
        ownership_bad_repos: Vec::new(),
        park_tls_ok: None,
        park_tls_domain: None,
    }
}

async fn probe_node(
    node: &FleetNode,
    git_port: u16,
    repos: &[String],
    park_domain: Option<&str>,
) -> NodeProbe {
    let fqdn = node.fqdn.clone();
    // DNS first: empty result is a laptop-resolver miss (boot race). No SYNs, no HTTPS.
    let dns = resolve_ips(&fqdn).await;
    if dns.is_empty() {
        return unreachable_probe(fqdn, dns, false, false, false);
    }
    let (tcp22, tcp443, tcp_git) = tokio::join!(
        tcp_open(&fqdn, 22),
        tcp_open(&fqdn, 443),
        tcp_open(&fqdn, git_port),
    );
    // 443 closed: TCP SYNs only. Skip HTTPS and git-daemon until the port opens.
    if !tcp443 {
        return unreachable_probe(fqdn, dns, tcp22, tcp443, tcp_git);
    }
    let (mut https_config, https_status, rum) = tokio::join!(
        https_api_config(&fqdn),
        https_api_status(&fqdn),
        https_api_rum(&fqdn),
    );
    if let (Some(cfg), Some(st)) = (https_config.as_mut(), https_status.as_ref()) {
        merge_status_into_config(cfg, st);
    }
    // Prefer local `POST /api/status` tips (no git-daemon). Fall back to bounded
    // ls-remote only on reachable old nodes (schema < 2). Never ls-remote a down host.
    let git_tips = match https_status.as_ref() {
        Some(s) if s.ok && s.schema_version >= 2 => git_tips_from_status(&s.repos),
        _ if https_config.as_ref().map(|c| c.ok).unwrap_or(false) => {
            ls_remote_tips(&fqdn, git_port, https_config.as_ref(), repos).await
        }
        _ => Vec::new(),
    };
    let healthy = https_config.as_ref().map(|c| c.ok).unwrap_or(false) && tcp_git;
    let (index_stale_repos, index_ok) = https_status
        .as_ref()
        .map(|st| index_stale_from_status(st))
        .unwrap_or((Vec::new(), true));
    let (index_pending_repos, index_errors) = https_status
        .as_ref()
        .map(index_pending_from_status)
        .unwrap_or_default();
    let configured_peers = https_status
        .as_ref()
        .map(|s| s.master_peers_configured.clone())
        .unwrap_or_default();
    let unreachable_peers = https_status
        .as_ref()
        .map(unreachable_peers_from_status)
        .unwrap_or_default();
    let (park_tls_ok, park_tls_domain) =
        park_tls_via_node_ip(park_domain, dns.first().map(|s| s.as_str())).await;
    let ipfs_ok = https_status.as_ref().and_then(|s| s.ipfs_ok);
    let ipfs_repos = https_status
        .as_ref()
        .map(ipfs_repos_from_status)
        .unwrap_or_default();
    NodeProbe {
        fqdn,
        dns,
        tcp22,
        tcp443,
        tcp_git,
        https_config,
        git_tips,
        healthy,
        error: None,
        missing_repos: Vec::new(),
        drift: Vec::new(),
        sync_anomalies: Vec::new(),
        sync_ok: true,
        index_ok,
        index_stale_repos,
        index_pending_repos,
        index_errors,
        configured_peers,
        unreachable_peers,
        http: https_status.as_ref().and_then(|s| s.http.clone()),
        rum,
        ipfs_ok,
        ipfs_repos,
        ssh_ok: None,
        host_key_changed: false,
        install_present: None,
        ownership_bad_repos: Vec::new(),
        park_tls_ok,
        park_tls_domain,
    }
}

/// HTTPS GET with SNI=`domain` forced to `ip` — catches missing park-tls on one master
/// while multi-A DNS still looks fine from other nodes.
async fn park_tls_via_node_ip(
    domain: Option<&str>,
    ip: Option<&str>,
) -> (Option<bool>, Option<String>) {
    let Some(domain) = domain.map(str::trim).filter(|d| !d.is_empty()) else {
        return (None, None);
    };
    let Some(ip) = ip.map(str::trim).filter(|d| !d.is_empty()) else {
        return (None, Some(domain.to_string()));
    };
    let resolve = format!("{domain}:443:{ip}");
    let url = format!("https://{domain}/");
    let out = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            "6",
            "--resolve",
            &resolve,
            &url,
        ])
        .output()
        .await;
    let ok = match out {
        Ok(o) if o.status.success() => {
            let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
            matches!(code.as_str(), "200" | "301" | "302" | "303" | "307" | "308")
        }
        _ => false,
    };
    (Some(ok), Some(domain.to_string()))
}

fn merge_status_into_config(cfg: &mut ConfigProbe, st: &StatusProbe) {
    if st.schema_version < 2 {
        return;
    }
    let on_disk: Vec<String> = st
        .repos
        .iter()
        .filter(|r| r.on_disk && !r.name.is_empty())
        .map(|r| r.name.clone())
        .collect();
    let catalog: Vec<String> = st
        .repos
        .iter()
        .filter(|r| r.in_catalog && !r.name.is_empty())
        .map(|r| r.name.clone())
        .collect();
    if !on_disk.is_empty() {
        cfg.repos = on_disk;
    }
    if !catalog.is_empty() {
        cfg.authorized_repos = catalog;
    }
}

pub(crate) fn git_tips_from_status(repos: &[StatusRepo]) -> Vec<GitTip> {
    let mut tips: Vec<GitTip> = repos
        .iter()
        .filter(|r| r.on_disk && !r.name.is_empty())
        .map(|r| {
            let mut refs = Vec::new();
            push_ref(
                &mut refs,
                r.publish_branch.as_deref(),
                r.publish_sha.as_deref(),
            );
            push_ref(&mut refs, r.head_branch.as_deref(), r.head_sha.as_deref());
            GitTip {
                repo: r.name.clone(),
                sha: r
                    .publish_sha
                    .clone()
                    .or_else(|| r.head_sha.clone())
                    .or_else(|| refs.first().map(|x| x.sha.clone())),
                refs,
                error: None,
            }
        })
        .collect();
    tips.sort_by(|a, b| a.repo.cmp(&b.repo));
    tips
}

fn push_ref(refs: &mut Vec<GitRef>, branch: Option<&str>, sha: Option<&str>) {
    let Some(name) = branch.filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(sha) = sha.filter(|s| !s.is_empty()) else {
        return;
    };
    if refs.iter().any(|r| r.name == name) {
        return;
    }
    refs.push(GitRef {
        name: name.to_string(),
        sha: sha.to_string(),
    });
}

pub(crate) fn index_stale_from_status(st: &StatusProbe) -> (Vec<IndexStaleRepo>, bool) {
    if st.schema_version < 4 {
        return (Vec::new(), true);
    }
    let mut stale = Vec::new();
    for r in &st.repos {
        if !r.on_disk || r.index_fresh != Some(false) {
            continue;
        }
        // Warming in flight is not "stale after failure" — UI shows pending separately.
        if r.index_pending == Some(true) {
            continue;
        }
        let tip = r
            .publish_sha
            .clone()
            .or_else(|| r.head_sha.clone())
            .unwrap_or_default();
        if tip.is_empty() {
            continue;
        }
        stale.push(IndexStaleRepo {
            repo: r.name.clone(),
            branch: r
                .publish_branch
                .clone()
                .or_else(|| r.head_branch.clone())
                .unwrap_or_else(|| "main".into()),
            tip_sha: tip,
            indexed_head_sha: r.indexed_head_sha.clone(),
        });
    }
    stale.sort_by(|a, b| a.repo.cmp(&b.repo));
    let ok = stale.is_empty();
    (stale, ok)
}

pub(crate) fn index_pending_from_status(st: &StatusProbe) -> (Vec<String>, Vec<String>) {
    let mut pending = Vec::new();
    let mut errors = Vec::new();
    for r in &st.repos {
        if !r.on_disk {
            continue;
        }
        if r.index_pending == Some(true) {
            pending.push(r.name.clone());
        }
        if let Some(err) = r.index_last_error.as_ref().filter(|e| !e.is_empty()) {
            errors.push(format!("{}: {err}", r.name));
        }
    }
    pending.sort();
    errors.sort();
    (pending, errors)
}

pub(crate) fn unreachable_peers_from_status(st: &StatusProbe) -> Vec<String> {
    let mut out = Vec::new();
    for p in &st.peers {
        if p.reachable {
            continue;
        }
        let host = if !p.host.is_empty() {
            p.host.clone()
        } else {
            p.fqdn.clone()
        };
        if !host.is_empty() {
            out.push(host);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Operator warnings for credit scale-down / dead mesh peers left in publisher peer lists.
pub fn fleet_operator_warnings(nodes: &[NodeProbe], fleet: &FleetFile) -> Vec<String> {
    let mut warnings = Vec::new();
    let down_mesh: Vec<&str> = fleet
        .nodes
        .iter()
        .filter(|n| n.role.as_deref().is_some_and(|r| r.eq_ignore_ascii_case("mesh")))
        .filter(|n| {
            nodes
                .iter()
                .find(|p| p.fqdn == n.fqdn)
                .is_none_or(|p| !p.healthy)
        })
        .map(|n| n.fqdn.as_str())
        .collect();
    if !down_mesh.is_empty() {
        warnings.push(format!(
            "Mesh node(s) down: {}. Before destroy, shrink RELAY_MASTER_PEER_LIST on publisher or publishes fail mesh-wait.",
            down_mesh.join(", ")
        ));
    }
    for n in nodes.iter().filter(|n| n.healthy && !n.unreachable_peers.is_empty()) {
        warnings.push(format!(
            "{} still peers unreachable hosts {:?} — remove them from RELAY_MASTER_PEER_LIST before scale-down.",
            n.fqdn, n.unreachable_peers
        ));
    }
    for n in nodes.iter().filter(|n| n.healthy && !n.index_errors.is_empty()) {
        warnings.push(format!(
            "{} SearchStore warm errors: {}",
            n.fqdn,
            n.index_errors.join("; ")
        ));
    }
    for n in nodes.iter().filter(|n| n.healthy && !n.index_pending_repos.is_empty()) {
        warnings.push(format!(
            "{} index warming: {}",
            n.fqdn,
            n.index_pending_repos.join(", ")
        ));
    }
    warnings
}

async fn ls_remote_tips(
    fqdn: &str,
    git_port: u16,
    https_config: Option<&ConfigProbe>,
    repos: &[String],
) -> Vec<GitTip> {
    // Only probe tips for bares that exist (config.repos). Catalog names not on
    // disk are "missing" without hitting git daemon. Unbounded parallel
    // ls-remote exceeds git daemon --max-connections (default 32).
    let mut want: Vec<String> = if let Some(cfg) = https_config {
        cfg.repos.clone()
    } else {
        repos.to_vec()
    };
    want.sort();
    want.dedup();
    let mut git_tips = Vec::new();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(LS_REMOTE_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for repo in want {
        let host = fqdn.to_string();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await;
            git_ls_remote(&host, git_port, &repo).await
        });
    }
    while let Some(res) = set.join_next().await {
        if let Ok(tip) = res {
            git_tips.push(tip);
        }
    }
    git_tips.sort_by(|a, b| a.repo.cmp(&b.repo));
    git_tips
}

pub async fn resolve_ips(host: &str) -> Vec<String> {
    let lookup = format!("{host}:0");
    match timeout(Duration::from_secs(4), tokio::net::lookup_host(lookup)).await {
        Ok(Ok(addrs)) => {
            let mut out: Vec<String> = addrs.map(|a: SocketAddr| a.ip().to_string()).collect();
            out.sort();
            out.dedup();
            out
        }
        _ => Vec::new(),
    }
}

async fn tcp_open(host: &str, port: u16) -> bool {
    let target = format!("{host}:{port}");
    timeout(Duration::from_secs(3), TcpStream::connect(&target))
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some()
}

async fn https_api_config(host: &str) -> Option<ConfigProbe> {
    let url = format!("https://{host}/api/config");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .danger_accept_invalid_certs(false)
        .build()
        .ok()?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .ok()?;
    let status = resp.status().as_u16();
    let ok = resp.status().is_success();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    let relay_server_id = body
        .get("relay_server_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let peers = json_string_vec(&body, "peers");
    let repos = json_string_vec(&body, "repos");
    let authorized_repos = json_string_vec(&body, "authorized_repos");
    let catalog_repos = body
        .get("catalog_repos")
        .and_then(|v| serde_json::from_value::<Vec<CatalogRepoInfo>>(v.clone()).ok())
        .unwrap_or_default();
    Some(ConfigProbe {
        ok,
        relay_server_id,
        peers,
        status,
        repos,
        authorized_repos,
        catalog_repos,
    })
}

async fn https_api_status(host: &str) -> Option<StatusProbe> {
    let url = format!("https://{host}/api/status");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .danger_accept_invalid_certs(false)
        .build()
        .ok()?;
    let resp = client
        .post(&url)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    let ok = resp.status().is_success();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    Some(parse_status_probe(ok, &body))
}

async fn https_api_rum(host: &str) -> Option<RumProbe> {
    let url = format!("https://{host}/api/rum");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .danger_accept_invalid_certs(false)
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Some(RumProbe {
            enabled: false,
            ..Default::default()
        });
    }
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    Some(parse_rum_probe(&body))
}

pub(crate) fn parse_rum_probe(body: &serde_json::Value) -> RumProbe {
    let p75 = body.get("p75");
    let num = |key: &str| -> Option<f64> {
        p75.and_then(|v| v.get(key)).and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_u64().map(|n| n as f64))
                .or_else(|| v.as_i64().map(|n| n as f64))
        })
    };
    let theater24h = body.get("theater24h").and_then(|t| {
        let major = t.get("major").and_then(|v| v.as_u64()).unwrap_or(0);
        let minor = t.get("minor").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut by_code = std::collections::BTreeMap::new();
        if let Some(obj) = t.get("byCode").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(n) = v.as_u64() {
                    by_code.insert(k.clone(), n);
                }
            }
        }
        let mut by_severity_code = std::collections::BTreeMap::new();
        if let Some(sev) = t.get("bySeverityCode").and_then(|v| v.as_object()) {
            for (sev_key, codes) in sev {
                let mut inner = std::collections::BTreeMap::new();
                if let Some(obj) = codes.as_object() {
                    for (k, v) in obj {
                        if let Some(n) = v.as_u64() {
                            inner.insert(k.clone(), n);
                        }
                    }
                }
                if !inner.is_empty() {
                    by_severity_code.insert(sev_key.clone(), inner);
                }
            }
        }
        Some(TheaterRumSummary {
            major,
            minor,
            by_code,
            by_severity_code,
        })
    });
    RumProbe {
        enabled: body
            .get("enabled")
            .and_then(|v| v.as_bool())
            .or_else(|| body.get("ok").and_then(|v| v.as_bool()))
            .unwrap_or(true),
        events: body.get("events").and_then(|v| v.as_u64()).unwrap_or(0),
        p75_ttfc_ms: num("ttfc_ms"),
        p75_lcp_ms: num("lcp_ms"),
        theater24h,
    }
}

pub(crate) fn parse_status_probe(ok: bool, body: &serde_json::Value) -> StatusProbe {
    let schema_version = body
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let repos_ok = body.get("repos_ok").and_then(|v| v.as_bool());
    let ipfs_ok = body.get("ipfs_ok").and_then(|v| v.as_bool());
    let mut repos = body
        .get("repos")
        .and_then(|v| serde_json::from_value::<Vec<StatusRepo>>(v.clone()).ok())
        .unwrap_or_default();
    for r in &mut repos {
        if let Some(ref mut ip) = r.ipfs {
            if ip.repo.is_empty() {
                ip.repo = r.name.clone();
            }
        }
    }
    let master_peers_configured = json_string_vec(body, "master_peers_configured");
    let peers = body
        .get("peers")
        .and_then(|v| serde_json::from_value::<Vec<StatusPeerProbe>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|mut p| {
            if p.host.is_empty() && !p.fqdn.is_empty() {
                p.host = p.fqdn.clone();
            }
            if p.fqdn.is_empty() && !p.host.is_empty() {
                p.fqdn = p.host.clone();
            }
            p
        })
        .collect();
    StatusProbe {
        ok,
        schema_version,
        repos_ok,
        ipfs_ok,
        repos,
        http: body
            .get("http")
            .and_then(|v| serde_json::from_value::<HttpTrafficProbe>(v.clone()).ok()),
        master_peers_configured,
        peers,
    }
}

fn ipfs_repos_from_status(st: &StatusProbe) -> Vec<RepoIpfsProbe> {
    st.repos
        .iter()
        .filter_map(|r| {
            r.ipfs.as_ref().map(|ip| RepoIpfsProbe {
                repo: if ip.repo.is_empty() {
                    r.name.clone()
                } else {
                    ip.repo.clone()
                },
                thin: ip.thin,
                cid_count: ip.cid_count,
                cached_count: ip.cached_count,
            })
        })
        .filter(|ip| ip.cid_count > 0 || ip.thin)
        .collect()
}

fn json_string_vec(body: &serde_json::Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

async fn git_ls_remote(host: &str, port: u16, repo: &str) -> GitTip {
    if which_git().is_none() {
        return GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some("git not installed".into()),
        };
    }
    let url = format!("git://{host}:{port}/{repo}.git");
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--heads", &url]);
    match timeout(Duration::from_secs(8), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let refs = crate::browse::parse_ls_remote_heads(&String::from_utf8_lossy(&out.stdout));
            let refs: Vec<GitRef> = refs
                .into_iter()
                .map(|b| GitRef {
                    name: b.name,
                    sha: b.sha,
                })
                .collect();
            let sha = preferred_sha(&refs);
            GitTip {
                repo: repo.into(),
                sha,
                refs,
                error: None,
            }
        }
        Ok(Ok(out)) => GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        },
        Ok(Err(e)) => GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some(e.to_string()),
        },
        Err(_) => GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some("timeout".into()),
        },
    }
}

fn preferred_sha(refs: &[GitRef]) -> Option<String> {
    for want in ["release", "main", "master"] {
        if let Some(r) = refs.iter().find(|r| r.name == want) {
            return Some(r.sha.clone());
        }
    }
    refs.first().map(|r| r.sha.clone())
}

/// Catalog repos any node advertised, plus topology `gitRepos`.
pub fn expected_catalog_repos(nodes: &[NodeProbe], extra: &[String]) -> Vec<String> {
    let mut set: Vec<String> = Vec::new();
    fn push(set: &mut Vec<String>, s: &str) {
        if !s.is_empty() && !set.iter().any(|x| x == s) {
            set.push(s.to_string());
        }
    }
    for n in nodes {
        if let Some(cfg) = &n.https_config {
            for r in &cfg.authorized_repos {
                push(&mut set, r);
            }
        }
    }
    if set.is_empty() {
        for n in nodes {
            if let Some(cfg) = &n.https_config {
                for r in &cfg.repos {
                    push(&mut set, r);
                }
            }
        }
    }
    for r in extra {
        push(&mut set, r);
    }
    set.sort();
    set
}

pub fn apply_catalog_sync(
    nodes: &mut [NodeProbe],
    extra_expected: &[String],
    forge_tips: &std::collections::BTreeMap<(String, String), String>,
) {
    let expected = expected_catalog_repos(nodes, extra_expected);
    let catalog_meta = collect_catalog_repos(nodes);
    let canonical = canonical_tips(nodes, &expected);
    let publisher_tips = build_publisher_tips(nodes, &catalog_meta, &expected);

    for n in nodes.iter_mut() {
        n.missing_repos.clear();
        n.drift.clear();
        n.sync_anomalies.clear();
        n.sync_ok = true;
        if !n.healthy {
            continue;
        }
        if n.git_tips
            .iter()
            .any(|t| t.error.as_deref() == Some("git not installed"))
        {
            continue;
        }
        let present: Vec<String> = n
            .https_config
            .as_ref()
            .map(|c| c.repos.clone())
            .unwrap_or_default();
        for repo in &expected {
            let on_disk = present.iter().any(|p| p == repo);
            if !on_disk {
                n.missing_repos.push(repo.clone());
                continue;
            }
            let Some(tip) = n
                .git_tips
                .iter()
                .find(|t| t.repo == *repo && t.error.is_none())
            else {
                continue;
            };

            let meta = catalog_meta.get(repo);
            let publish_branch = meta.map(|m| m.publish_branch.as_str());
            let publish_origin = meta.and_then(|m| m.publish_origin.as_deref());

            if let Some(_origin) = publish_origin.filter(|o| !o.is_empty()) {
                let branch = publish_branch.unwrap_or("main");
                let key = (repo.clone(), branch.to_string());
                let Some((expected_sha, origin_fqdn)) = publisher_tips.get(&key) else {
                    continue;
                };
                let is_publisher = fqdn_eq(&n.fqdn, origin_fqdn);
                if is_publisher {
                    continue;
                }
                let have = tip_sha_for_branch(tip, branch);
                if have.as_deref() != Some(expected_sha.as_str()) {
                    // Publisher tip is canonical. Majority vote treated two lagging
                    // replicas as "ahead" and hid Synchronize. Divergence still
                    // surfaces as ReplicaBehind; peer fetch is fast-forward only.
                    n.drift.push(RepoDrift {
                        repo: repo.clone(),
                        branch: branch.to_string(),
                        sha: have,
                        expected_sha: expected_sha.clone(),
                        kind: DriftKind::ReplicaBehind,
                    });
                }
            } else {
                for ((repo_key, branch), want_sha) in &canonical {
                    if repo_key != repo {
                        continue;
                    }
                    let have = tip
                        .refs
                        .iter()
                        .find(|r| r.name == *branch)
                        .map(|r| r.sha.as_str());
                    if have != Some(want_sha.as_str()) {
                        n.drift.push(RepoDrift {
                            repo: repo.clone(),
                            branch: branch.clone(),
                            sha: have.map(|s| s.to_string()),
                            expected_sha: want_sha.clone(),
                            kind: DriftKind::TipMismatch,
                        });
                    }
                }
            }
        }
        n.missing_repos.sort();
        n.drift
            .sort_by(|a, b| (&a.repo, &a.branch).cmp(&(&b.repo, &b.branch)));
        n.sync_ok = n.missing_repos.is_empty() && n.drift.is_empty();
    }

    attach_publisher_behind_anomalies(nodes, &publisher_tips, forge_tips);
}

/// When GitLab (forge) matches a replica but not the publish origin, the publisher
/// was not pushed yet — do not treat lagging replicas as "ahead" via majority vote.
fn attach_publisher_behind_anomalies(
    nodes: &mut [NodeProbe],
    publisher_tips: &std::collections::BTreeMap<(String, String), (String, String)>,
    forge_tips: &std::collections::BTreeMap<(String, String), String>,
) {
    for ((repo, branch), (pub_sha, origin_fqdn)) in publisher_tips {
        let key = (repo.clone(), branch.clone());
        let Some(forge_sha) = forge_tips.get(&key) else {
            continue;
        };
        if forge_sha == pub_sha {
            continue;
        }
        let Some(replica) = nodes.iter().find(|n| {
            n.healthy
                && !fqdn_eq(&n.fqdn, origin_fqdn)
                && n.git_tips.iter().any(|t| {
                    t.repo == *repo
                        && t.error.is_none()
                        && tip_sha_for_branch(t, branch).as_deref() == Some(forge_sha.as_str())
                })
        }) else {
            continue;
        };
        let replica_sha = replica
            .git_tips
            .iter()
            .find(|t| t.repo == *repo)
            .and_then(|t| tip_sha_for_branch(t, branch))
            .unwrap_or_default();
        let anomaly = SyncAnomaly {
            kind: "publisher_behind_forge".into(),
            repo: repo.clone(),
            branch: branch.clone(),
            publisher_fqdn: origin_fqdn.clone(),
            publisher_sha: pub_sha.clone(),
            replica_fqdn: replica.fqdn.clone(),
            replica_sha,
        };
        if let Some(publisher) = nodes
            .iter_mut()
            .find(|n| n.healthy && fqdn_eq(&n.fqdn, origin_fqdn))
        {
            publisher.sync_anomalies.push(anomaly);
            publisher.sync_ok = false;
        }
    }
}

async fn probe_forge_tips(
    gitlab_url: Option<&str>,
    expected: &[String],
) -> std::collections::BTreeMap<(String, String), String> {
    use std::collections::BTreeMap;
    let mut out = BTreeMap::new();
    let Some(base) = gitlab_url.and_then(forge_repo_base_from_gitlab_url) else {
        return out;
    };
    let mut set = tokio::task::JoinSet::new();
    for repo in expected {
        let url = format!("{base}/{repo}.git");
        let repo = repo.clone();
        set.spawn(async move {
            let tip = git_ls_remote_url(&url, &repo).await;
            (repo, tip)
        });
    }
    while let Some(res) = set.join_next().await {
        if let Ok((repo, tip)) = res {
            if tip.error.is_some() {
                continue;
            }
            for r in &tip.refs {
                out.insert((repo.clone(), r.name.clone()), r.sha.clone());
            }
            if let Some(sha) = tip.sha {
                out.entry((repo, "main".into())).or_insert(sha);
            }
        }
    }
    out
}

/// `https://gitlab.com/clevertree/relay-master` → `https://gitlab.com/clevertree`
fn forge_repo_base_from_gitlab_url(url: &str) -> Option<String> {
    let s = url.trim().trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = s.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            let ns = path.rsplit_once('/')?.0;
            return Some(format!("git@{host}:{ns}"));
        }
    }
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let ns = path.rsplit_once('/')?.0;
    Some(format!("https://{host}/{ns}"))
}

async fn git_ls_remote_url(url: &str, repo: &str) -> GitTip {
    if which_git().is_none() {
        return GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some("git not installed".into()),
        };
    }
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--heads", url]);
    match timeout(Duration::from_secs(12), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let refs = crate::browse::parse_ls_remote_heads(&String::from_utf8_lossy(&out.stdout));
            let refs: Vec<GitRef> = refs
                .into_iter()
                .map(|b| GitRef {
                    name: b.name,
                    sha: b.sha,
                })
                .collect();
            let sha = preferred_sha(&refs);
            GitTip {
                repo: repo.into(),
                sha,
                refs,
                error: None,
            }
        }
        Ok(Ok(out)) => GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        },
        Ok(Err(e)) => GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some(e.to_string()),
        },
        Err(_) => GitTip {
            repo: repo.into(),
            sha: None,
            refs: Vec::new(),
            error: Some("git ls-remote timeout".into()),
        },
    }
}

/// Merge `catalog_repos` from all healthy nodes (first wins per repo name).
pub fn collect_catalog_repos(nodes: &[NodeProbe]) -> std::collections::BTreeMap<String, CatalogRepoInfo> {
    let mut out = std::collections::BTreeMap::new();
    for n in nodes {
        if !n.healthy {
            continue;
        }
        let Some(cfg) = &n.https_config else {
            continue;
        };
        for row in &cfg.catalog_repos {
            if row.name.is_empty() {
                continue;
            }
            out.entry(row.name.clone()).or_insert_with(|| row.clone());
        }
    }
    out
}

fn build_publisher_tips(
    nodes: &[NodeProbe],
    catalog: &std::collections::BTreeMap<String, CatalogRepoInfo>,
    expected: &[String],
) -> std::collections::BTreeMap<(String, String), (String, String)> {
    let mut out = std::collections::BTreeMap::new();
    for repo in expected {
        let Some(meta) = catalog.get(repo) else {
            continue;
        };
        let Some(origin) = meta.publish_origin.as_deref().filter(|o| !o.is_empty()) else {
            continue;
        };
        let branch = meta.publish_branch.as_str();
        let Some(publisher) = nodes.iter().find(|n| n.healthy && fqdn_eq(&n.fqdn, origin)) else {
            continue;
        };
        let Some(tip) = publisher
            .git_tips
            .iter()
            .find(|t| t.repo == *repo && t.error.is_none())
        else {
            continue;
        };
        let Some(sha) = tip_sha_for_branch(tip, branch) else {
            continue;
        };
        out.insert((repo.clone(), branch.to_string()), (sha, origin.to_string()));
    }
    out
}

fn tip_sha_for_branch(tip: &GitTip, branch: &str) -> Option<String> {
    tip.refs
        .iter()
        .find(|r| r.name == branch)
        .map(|r| r.sha.clone())
        .or_else(|| {
            if tip.refs.is_empty() {
                tip.sha.clone()
            } else {
                None
            }
        })
}

pub(crate) fn fqdn_eq(a: &str, b: &str) -> bool {
    normalize_fqdn(a) == normalize_fqdn(b)
}

fn normalize_fqdn(host: &str) -> String {
    let h = host.trim();
    let h = h.split(':').next().unwrap_or(h);
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// Most common SHA per (repo, branch) among up nodes that advertised that ref.
fn canonical_tips(
    nodes: &[NodeProbe],
    expected: &[String],
) -> std::collections::BTreeMap<(String, String), String> {
    use std::collections::HashMap;
    let mut counts: HashMap<(String, String), HashMap<String, usize>> = HashMap::new();
    for n in nodes {
        if !n.healthy {
            continue;
        }
        for tip in &n.git_tips {
            if !expected.iter().any(|r| r == &tip.repo) || tip.error.is_some() {
                continue;
            }
            for r in &tip.refs {
                *counts
                    .entry((tip.repo.clone(), r.name.clone()))
                    .or_default()
                    .entry(r.sha.clone())
                    .or_default() += 1;
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    for (key, shas) in counts {
        let total: usize = shas.values().sum();
        if total < 2 {
            continue;
        }
        let Some((sha, _)) = shas
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)))
        else {
            continue;
        };
        out.insert(key, sha);
    }
    out
}

pub fn attention_nodes(nodes: &[NodeProbe]) -> usize {
    nodes
        .iter()
        .filter(|n| {
            !n.healthy
                || !n.sync_ok
                || !n.index_ok
                || !n.sync_anomalies.is_empty()
        })
        .count()
}

pub fn down_nodes(nodes: &[NodeProbe]) -> usize {
    nodes.iter().filter(|n| !n.healthy).count()
}

pub fn sync_issue_nodes(nodes: &[NodeProbe]) -> usize {
    nodes.iter().filter(|n| n.healthy && !n.sync_ok).count()
}

pub fn index_stale_nodes(nodes: &[NodeProbe]) -> usize {
    nodes.iter().filter(|n| n.healthy && !n.index_ok).count()
}

fn which_git() -> Option<()> {
    which::which_os("git")
}

mod which {
    pub fn which_os(name: &str) -> Option<()> {
        let exe = if cfg!(windows) && !name.ends_with(".exe") {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                if dir.join(&exe).is_file() {
                    return Some(());
                }
            }
        }
        None
    }
}

/// IPs of nodes that still look alive enough to keep on fleet ingress.
pub fn healthy_node_ips(nodes: &[NodeProbe]) -> Vec<String> {
    let mut ips = Vec::new();
    for n in nodes {
        if n.healthy || n.tcp443 {
            for ip in &n.dns {
                if !ips.contains(ip) {
                    ips.push(ip.clone());
                }
            }
        }
    }
    ips
}

/// Like [`healthy_node_ips`], but only nodes with `fleetDns` / non-mesh role in
/// topology (publisher fat ingress). Mesh-only nodes must not enter
/// `fleet.relaygateway.net` multi-A.
pub fn healthy_fleet_ingress_ips(nodes: &[NodeProbe], fleet: &crate::topology::FleetFile) -> Vec<String> {
    let allow: std::collections::HashSet<String> = fleet
        .nodes
        .iter()
        .filter(|n| n.participates_in_fleet_dns())
        .map(|n| n.fqdn.to_ascii_lowercase())
        .collect();
    let mut ips = Vec::new();
    for n in nodes {
        if !allow.contains(&n.fqdn.to_ascii_lowercase()) {
            continue;
        }
        if n.healthy || n.tcp443 {
            for ip in &n.dns {
                if !ips.contains(ip) {
                    ips.push(ip.clone());
                }
            }
        }
    }
    ips
}

pub fn dead_ingress_ips(ingress: &IngressProbe, keep: &[String]) -> Vec<String> {
    ingress
        .dns
        .iter()
        .filter(|ip| !keep.iter().any(|k| k == *ip))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_stale_ingress_ips() {
        let ingress = IngressProbe {
            fqdn: "fleet.example.net".into(),
            dns: vec!["1.1.1.1".into(), "2.2.2.2".into()],
        };
        let keep = vec!["1.1.1.1".to_string()];
        assert_eq!(
            dead_ingress_ips(&ingress, &keep),
            vec!["2.2.2.2".to_string()]
        );
    }

    #[test]
    fn config_probe_reads_repos() {
        let body: serde_json::Value = serde_json::json!({
            "relay_server_id": "relay-iad1",
            "peers": ["lon1.relaygateway.net"],
            "repos": ["relay-master", "paradigm-threat-files"],
            "authorized_repos": ["relay-master"]
        });
        let repos = json_string_vec(&body, "repos");
        let auth = json_string_vec(&body, "authorized_repos");
        assert_eq!(repos.len(), 2);
        assert_eq!(auth, vec!["relay-master"]);
    }

    fn up_node(
        fqdn: &str,
        repos: &[&str],
        authorized: &[&str],
        tips: Vec<GitTip>,
        catalog_repos: Vec<CatalogRepoInfo>,
    ) -> NodeProbe {
        NodeProbe {
            fqdn: fqdn.into(),
            dns: vec!["1.1.1.1".into()],
            tcp22: true,
            tcp443: true,
            tcp_git: true,
            https_config: Some(ConfigProbe {
                ok: true,
                relay_server_id: Some(fqdn.into()),
                peers: vec![],
                status: 200,
                repos: repos.iter().map(|s| s.to_string()).collect(),
                authorized_repos: authorized.iter().map(|s| s.to_string()).collect(),
                catalog_repos,
            }),
            git_tips: tips,
            healthy: true,
            error: None,
            missing_repos: vec![],
            drift: vec![],
            sync_anomalies: vec![],
            sync_ok: true,
            index_ok: true,
            index_stale_repos: vec![],
            index_pending_repos: vec![],
            index_errors: vec![],
            configured_peers: vec![],
            unreachable_peers: vec![],
            http: None,
            rum: None,
            ipfs_ok: Some(true),
            ipfs_repos: vec![],
            ssh_ok: None,
            host_key_changed: false,
            install_present: None,
            ownership_bad_repos: vec![],
            park_tls_ok: Some(true),
            park_tls_domain: Some("paradigmthreat.net".into()),
        }
    }

    fn catalog_row(name: &str, branch: &str, origin: &str) -> CatalogRepoInfo {
        CatalogRepoInfo {
            name: name.into(),
            publish_branch: branch.into(),
            publish_origin: Some(origin.into()),
        }
    }

    fn tip(repo: &str, branch: &str, sha: &str) -> GitTip {
        GitTip {
            repo: repo.into(),
            sha: Some(sha.into()),
            refs: vec![GitRef {
                name: branch.into(),
                sha: sha.into(),
            }],
            error: None,
        }
    }

    #[test]
    fn missing_catalog_repo_flags_sync() {
        let mut nodes = vec![
            up_node(
                "a.example",
                &["relay-master", "park"],
                &["relay-master", "park"],
                vec![
                    tip("relay-master", "main", "aaa1111"),
                    tip("park", "main", "bbb2222"),
                ],
                vec![],
            ),
            up_node(
                "b.example",
                &["relay-master"],
                &["relay-master", "park"],
                vec![tip("relay-master", "main", "aaa1111")],
                vec![],
            ),
        ];
        apply_catalog_sync(&mut nodes, &[], &std::collections::BTreeMap::new());
        assert!(nodes[0].sync_ok);
        assert!(!nodes[1].sync_ok);
        assert_eq!(nodes[1].missing_repos, vec!["park"]);
        assert_eq!(sync_issue_nodes(&nodes), 1);
        assert_eq!(attention_nodes(&nodes), 1);
    }

    #[test]
    fn sha_mismatch_flags_drift() {
        let mut nodes = vec![
            up_node(
                "a.example",
                &["park"],
                &["park"],
                vec![tip("park", "release", "aaa1111")],
                vec![],
            ),
            up_node(
                "b.example",
                &["park"],
                &["park"],
                vec![tip("park", "release", "bbb2222")],
                vec![],
            ),
            up_node(
                "c.example",
                &["park"],
                &["park"],
                vec![tip("park", "release", "aaa1111")],
                vec![],
            ),
        ];
        apply_catalog_sync(&mut nodes, &[], &std::collections::BTreeMap::new());
        assert!(nodes[0].sync_ok);
        assert!(!nodes[1].sync_ok);
        assert_eq!(nodes[1].drift[0].branch, "release");
        assert_eq!(nodes[1].drift[0].kind, DriftKind::TipMismatch);
        assert_eq!(nodes[1].drift[0].expected_sha, "aaa1111");
        assert!(nodes[2].sync_ok);
    }

    #[test]
    fn ls_remote_error_is_not_missing_repo() {
        let mut nodes = vec![
            up_node(
                "a.example",
                &["park"],
                &["park"],
                vec![tip("park", "main", "aaa1111")],
                vec![],
            ),
            up_node(
                "b.example",
                &["park"],
                &["park"],
                vec![GitTip {
                    repo: "park".into(),
                    sha: None,
                    refs: vec![],
                    error: Some("fatal: unable to connect".into()),
                }],
                vec![],
            ),
        ];
        apply_catalog_sync(&mut nodes, &[], &std::collections::BTreeMap::new());
        assert!(nodes[1].missing_repos.is_empty());
        assert!(nodes[1].drift.is_empty());
        assert!(nodes[1].sync_ok);
    }

    #[test]
    fn parse_status_v2_builds_tips_without_ls_remote() {
        let body = serde_json::json!({
            "schema_version": 2,
            "repos_ok": false,
            "repos": [
                {
                    "name": "park",
                    "on_disk": true,
                    "in_catalog": true,
                    "ok": true,
                    "publish_branch": "main",
                    "publish_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "head_branch": "main",
                    "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "branch_count": 1
                },
                {
                    "name": "ghost",
                    "on_disk": false,
                    "in_catalog": true,
                    "ok": false,
                    "publish_branch": "main"
                }
            ]
        });
        let st = parse_status_probe(true, &body);
        assert_eq!(st.schema_version, 2);
        assert_eq!(st.repos_ok, Some(false));
        let tips = git_tips_from_status(&st.repos);
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].repo, "park");
        assert_eq!(
            tips[0].sha.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let mut cfg = ConfigProbe {
            ok: true,
            relay_server_id: None,
            peers: vec![],
            status: 200,
            repos: vec![],
            authorized_repos: vec![],
            catalog_repos: vec![],
        };
        merge_status_into_config(&mut cfg, &st);
        assert_eq!(cfg.repos, vec!["park"]);
        assert_eq!(cfg.authorized_repos, vec!["park", "ghost"]);
    }

    #[test]
    fn next_probe_secs_is_fast_when_down_or_dns_empty() {
        let up = up_node("a.example", &["park"], &["park"], vec![], vec![]);
        assert_eq!(next_probe_secs(&[up.clone()], 300, 10), 300);
        let mut down = up.clone();
        down.healthy = false;
        assert_eq!(next_probe_secs(&[down], 300, 10), 10);
        let mut no_dns = up.clone();
        no_dns.dns.clear();
        assert_eq!(next_probe_secs(&[no_dns], 300, 10), 10);
        assert_eq!(next_probe_secs(&[], 300, 10), 10);
    }

    #[test]
    fn healthy_fleet_ingress_ips_skips_mesh() {
        let fleet = FleetFile {
            schema: "relay.ops.fleet.v1".into(),
            fleet_ingress: None,
            git_port: Some(9418),
            nodes: vec![
                FleetNode {
                    fqdn: "pub1.relaygateway.net".into(),
                    ssh_user: None,
                    deploy_target: None,
                    fleet_dns: true,
                    role: Some("publisher".into()),
                },
                FleetNode {
                    fqdn: "mesh1.relaygateway.net".into(),
                    ssh_user: None,
                    deploy_target: None,
                    fleet_dns: false,
                    role: Some("mesh".into()),
                },
            ],
            git_repos: vec![],
            health: Default::default(),
            catalog_path: None,
            data_root: None,
            mail: None,
        };
        let mut pub1 = up_node("pub1.relaygateway.net", &[], &[], vec![], vec![]);
        pub1.dns = vec!["172.234.148.70".into()];
        let mut mesh1 = up_node("mesh1.relaygateway.net", &[], &[], vec![], vec![]);
        mesh1.dns = vec!["172.237.134.22".into()];
        let ips = healthy_fleet_ingress_ips(&[pub1.clone(), mesh1.clone()], &fleet);
        assert_eq!(ips, vec!["172.234.148.70".to_string()]);
        assert_eq!(healthy_node_ips(&[pub1, mesh1]).len(), 2);
    }

    #[test]
    fn fleet_warnings_flag_unreachable_peers() {
        let fleet = FleetFile {
            schema: "relay.ops.fleet.v1".into(),
            fleet_ingress: None,
            git_port: Some(9418),
            nodes: vec![
                FleetNode {
                    fqdn: "pub1.relaygateway.net".into(),
                    ssh_user: None,
                    deploy_target: None,
                    fleet_dns: true,
                    role: Some("publisher".into()),
                },
                FleetNode {
                    fqdn: "mesh1.relaygateway.net".into(),
                    ssh_user: None,
                    deploy_target: None,
                    fleet_dns: false,
                    role: Some("mesh".into()),
                },
            ],
            git_repos: vec![],
            health: Default::default(),
            catalog_path: None,
            data_root: None,
            mail: None,
        };
        let mut pub1 = up_node("pub1.relaygateway.net", &[], &[], vec![], vec![]);
        pub1.unreachable_peers = vec!["mesh1.relaygateway.net".into()];
        let mesh1 = unreachable_probe("mesh1.relaygateway.net".into(), vec![], false, false, false);
        let w = fleet_operator_warnings(&[pub1, mesh1], &fleet);
        assert!(w.iter().any(|s| s.contains("Mesh node")));
        assert!(w.iter().any(|s| s.contains("unreachable")));
    }

    #[test]
    fn index_stale_from_status_v4_detects_lag() {
        let body = serde_json::json!({
            "schema_version": 4,
            "repos": [{
                "name": "paradigm-threat-files",
                "on_disk": true,
                "in_catalog": true,
                "ok": true,
                "publish_branch": "main",
                "publish_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "indexed_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "index_fresh": false
            }]
        });
        let st = parse_status_probe(true, &body);
        let (stale, ok) = index_stale_from_status(&st);
        assert!(!ok);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].repo, "paradigm-threat-files");
    }

    #[test]
    fn old_status_schema_is_not_treated_as_local_tips() {
        let st = parse_status_probe(
            true,
            &serde_json::json!({"schema_version": 1, "healthy": true}),
        );
        assert_eq!(st.schema_version, 1);
        assert!(st.repos.is_empty());
    }

    #[test]
    fn parse_status_v5_http_traffic() {
        let body = serde_json::json!({
            "schema_version": 5,
            "http": {
                "enabled": true,
                "req_1m": 12,
                "req_1h": 900,
                "req_24h": 20000,
                "p50_ms": 40,
                "p95_ms": 180,
                "window_1h": {
                    "requests": 900,
                    "status_2xx": 880,
                    "status_5xx": 2,
                    "html": 120,
                    "api": 700
                },
                "top_hosts": [
                    {"host": "paradigmthreat.net", "requests": 120}
                ],
                "hosts": [
                    {"host": "paradigmthreat.net", "req_1h": 120, "html": 100, "api": 10, "s5xx": 0}
                ],
                "top_paths": [
                    {"host": "paradigmthreat.net", "path": "/science/computing/crichtons-prophecy.md", "requests": 40}
                ]
            }
        });
        let st = parse_status_probe(true, &body);
        assert_eq!(st.schema_version, 5);
        let h = st.http.expect("http");
        assert!(h.enabled);
        assert_eq!(h.req_1h, 900);
        assert_eq!(h.p95_ms, Some(180));
        assert_eq!(h.window_1h.status_5xx, 2);
        assert_eq!(h.window_1h.html, 120);
        assert_eq!(h.top_hosts[0].host, "paradigmthreat.net");
        assert_eq!(h.hosts[0].html, 100);
        assert_eq!(h.top_paths[0].path, "/science/computing/crichtons-prophecy.md");
    }

    #[test]
    fn parse_rum_p75() {
        let rum = parse_rum_probe(&serde_json::json!({
            "ok": true,
            "enabled": true,
            "events": 12,
            "p75": { "ttfc_ms": 180.0, "lcp_ms": 900.0 },
            "theater24h": {
                "major": 5,
                "minor": 20,
                "byCode": { "wasm_hard_fail": 3, "js_error": 20 },
                "bySeverityCode": {
                    "major": { "wasm_hard_fail": 3 },
                    "minor": { "js_error": 20 }
                }
            }
        }));
        assert!(rum.enabled);
        assert_eq!(rum.events, 12);
        assert_eq!(rum.p75_ttfc_ms, Some(180.0));
        assert_eq!(rum.p75_lcp_ms, Some(900.0));
        let t = rum.theater24h.expect("theater");
        assert_eq!(t.major, 5);
        assert_eq!(t.by_code.get("wasm_hard_fail"), Some(&3));
        assert_eq!(
            t.by_severity_code
                .get("major")
                .and_then(|m| m.get("wasm_hard_fail")),
            Some(&3)
        );
    }

    #[test]
    fn parse_status_ipfs_repos() {
        let st = parse_status_probe(
            true,
            &serde_json::json!({
                "schema_version": 5,
                "ipfs_ok": true,
                "repos": [{
                    "name": "fan-edits",
                    "on_disk": true,
                    "in_catalog": true,
                    "ok": true,
                    "ipfs": { "thin": true, "cid_count": 10, "cached_count": 7 }
                }]
            }),
        );
        assert_eq!(st.ipfs_ok, Some(true));
        let ips = ipfs_repos_from_status(&st);
        assert_eq!(ips.len(), 1);
        assert_eq!(ips[0].repo, "fan-edits");
        assert_eq!(ips[0].cid_count, 10);
        assert_eq!(ips[0].cached_count, 7);
    }

    #[test]
    fn two_lagging_replicas_are_behind_publisher_not_ahead() {
        let site = catalog_row("paradigm-threat-files", "main", "iad1.example");
        let mut nodes = vec![
            up_node(
                "iad1.example",
                &["paradigm-threat-files"],
                &["paradigm-threat-files"],
                vec![tip("paradigm-threat-files", "main", "newsha1")],
                vec![site.clone()],
            ),
            up_node(
                "ord1.example",
                &["paradigm-threat-files"],
                &["paradigm-threat-files"],
                vec![tip("paradigm-threat-files", "main", "oldsha00")],
                vec![site.clone()],
            ),
            up_node(
                "hkg1.example",
                &["paradigm-threat-files"],
                &["paradigm-threat-files"],
                vec![tip("paradigm-threat-files", "main", "oldsha00")],
                vec![site],
            ),
        ];
        apply_catalog_sync(&mut nodes, &[], &std::collections::BTreeMap::new());
        assert!(nodes[0].sync_ok, "publisher should stay in sync");
        assert!(nodes[0].drift.is_empty());
        assert!(nodes[0].sync_anomalies.is_empty());
        assert!(!nodes[1].sync_ok);
        assert_eq!(nodes[1].drift[0].kind, DriftKind::ReplicaBehind);
        assert_eq!(nodes[1].drift[0].expected_sha, "newsha1");
        assert!(!nodes[2].sync_ok);
        assert_eq!(nodes[2].drift[0].kind, DriftKind::ReplicaBehind);
    }

    #[test]
    fn replicas_behind_publisher_are_flagged_not_publisher() {
        let site = catalog_row("paradigm-threat-web", "release", "iad1.example");
        let mut nodes = vec![
            up_node(
                "iad1.example",
                &["paradigm-threat-web"],
                &["paradigm-threat-web"],
                vec![tip("paradigm-threat-web", "release", "bbb2222")],
                vec![site.clone()],
            ),
            up_node(
                "iad1.example",
                &[],
                &["paradigm-threat-web"],
                vec![],
                vec![site.clone()],
            ),
            up_node(
                "hkg1.example",
                &["paradigm-threat-web"],
                &["paradigm-threat-web"],
                vec![tip("paradigm-threat-web", "release", "aaa1111")],
                vec![site],
            ),
        ];
        apply_catalog_sync(&mut nodes, &[], &std::collections::BTreeMap::new());
        assert!(nodes[0].sync_ok);
        assert!(nodes[0].drift.is_empty());
        assert!(nodes[0].sync_anomalies.is_empty());
        assert!(!nodes[1].sync_ok);
        assert_eq!(nodes[1].missing_repos, vec!["paradigm-threat-web"]);
        assert!(!nodes[2].sync_ok);
        assert_eq!(nodes[2].drift[0].kind, DriftKind::ReplicaBehind);
        assert_eq!(nodes[2].drift[0].expected_sha, "bbb2222");
    }

    #[test]
    fn publisher_origin_match_is_case_insensitive() {
        let site = catalog_row("clevertree-site", "release", "DFW1.example");
        let mut nodes = vec![
            up_node(
                "iad1.example",
                &["clevertree-site"],
                &["clevertree-site"],
                vec![tip("clevertree-site", "release", "aaa1111")],
                vec![site.clone()],
            ),
            up_node(
                "iad1.example",
                &["clevertree-site"],
                &["clevertree-site"],
                vec![tip("clevertree-site", "release", "bbb2222")],
                vec![site],
            ),
        ];
        apply_catalog_sync(&mut nodes, &[], &std::collections::BTreeMap::new());
        assert!(nodes[0].drift.is_empty());
    }

    #[test]
    fn publisher_behind_forge_flags_anomaly_not_replica_ahead() {
        let site = catalog_row("paradigm-threat-files", "main", "iad1.example");
        let mut nodes = vec![
            up_node(
                "iad1.example",
                &["paradigm-threat-files"],
                &["paradigm-threat-files"],
                vec![tip("paradigm-threat-files", "main", "pub00001")],
                vec![site.clone()],
            ),
            up_node(
                "hkg1.example",
                &["paradigm-threat-files"],
                &["paradigm-threat-files"],
                vec![tip("paradigm-threat-files", "main", "forge000")],
                vec![site.clone()],
            ),
            up_node(
                "ord1.example",
                &["paradigm-threat-files"],
                &["paradigm-threat-files"],
                vec![tip("paradigm-threat-files", "main", "forge000")],
                vec![site],
            ),
        ];
        let mut forge = std::collections::BTreeMap::new();
        forge.insert(
            ("paradigm-threat-files".into(), "main".into()),
            "forge000".into(),
        );
        apply_catalog_sync(&mut nodes, &[], &forge);
        assert!(nodes[0].sync_anomalies.len() == 1);
        assert_eq!(nodes[0].sync_anomalies[0].kind, "publisher_behind_forge");
        assert_eq!(nodes[1].drift[0].kind, DriftKind::ReplicaBehind);
        assert_ne!(nodes[1].drift[0].kind, DriftKind::ReplicaAhead);
    }
}

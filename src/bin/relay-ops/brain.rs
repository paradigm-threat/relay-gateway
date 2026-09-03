//! Local fleet brain: probe loop, JSONL feed, pending requests, loopback HTTP API.

use crate::config::OpsConfig;
use crate::gitlab;
use crate::probe::{probe_fleet, NodeProbe};
use crate::recommend::{self, RecommendInput};
use crate::scenarios::{self, ScenarioHit};
use crate::spot::SpotAssignment;
use crate::topology::FleetFile;
use anyhow::{anyhow, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid_ish::new_id;

/// NullScorer stub — exact YAML match only (plan: no ML in v1).
pub struct NullScorer;

impl NullScorer {
    pub fn suggest(_hit: Option<&ScenarioHit>) -> Option<String> {
        None
    }
}

mod uuid_ish {
    use std::time::{SystemTime, UNIX_EPOCH};
    pub fn new_id() -> String {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("req-{t:x}-{:04x}", (t ^ (t >> 7)) as u16)
    }
}

#[derive(Clone)]
pub struct BrainApp {
    pub inner: Arc<Mutex<BrainState>>,
    pub token: String,
    pub execute: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub feed_path: PathBuf,
    pub req_seq: Arc<AtomicU64>,
}

#[derive(Default)]
pub struct BrainState {
    pub online: bool,
    pub blocked: bool,
    pub last_scenario: Option<String>,
    pub last_fingerprint_summary: String,
    pub nodes: Vec<NodeClassification>,
    pub requests: HashMap<String, BrainRequest>,
    pub fleet: Option<FleetFile>,
    pub spot: Vec<SpotAssignment>,
    pub scenarios_source: String,
    pub last_error: Option<String>,
    /// Cooldown key `fqdn:scenario` → last auto-run attempt ms.
    pub auto_ran_ms: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeClassification {
    pub fqdn: String,
    pub scenario_id: Option<String>,
    pub message: Option<String>,
    pub blocked: bool,
    pub steps: Vec<String>,
    pub requires_confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainRequest {
    pub id: String,
    pub title: String,
    pub description: String,
    pub fqdn: Option<String>,
    pub scenario_id: Option<String>,
    pub proposed_steps: Vec<String>,
    pub severity: String,
    pub status: String,
    pub created_ms: u64,
}

#[derive(Serialize)]
pub struct BrainStatus {
    pub online: bool,
    pub observe_only: bool,
    pub paused: bool,
    pub blocked: bool,
    pub last_scenario: Option<String>,
    pub last_fingerprint_summary: String,
    pub pending_requests: usize,
    pub scenarios_source: String,
    pub feed_path: String,
    pub nodes: Vec<NodeClassification>,
    pub last_error: Option<String>,
}

fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("relay-brain")
}

pub fn default_feed_path() -> PathBuf {
    state_dir().join("feed.jsonl")
}

pub fn default_bind() -> String {
    std::env::var("RELAY_BRAIN_BIND").unwrap_or_else(|_| "127.0.0.1:7781".into())
}

pub fn resolve_token(cfg: &OpsConfig) -> String {
    if let Ok(t) = std::env::var("RELAY_BRAIN_TOKEN") {
        if !t.is_empty() {
            return t;
        }
    }
    // Optional field via env only in v1; generate sticky token file if missing
    let p = state_dir().join("token");
    if let Ok(t) = fs::read_to_string(&p) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let _ = cfg; // reserved for config.toml brain_token later
    let t = format!("local-{}", new_id());
    let _ = fs::create_dir_all(state_dir());
    let _ = fs::write(&p, &t);
    t
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

const AUTO_RUN_COOLDOWN_MS: u64 = 180_000;

fn append_feed(path: &std::path::Path, event: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{event}")?;
    Ok(())
}

fn check_token(headers: &HeaderMap, expected: &str) -> bool {
    if let Some(v) = headers.get("authorization").and_then(|h| h.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return t == expected;
        }
    }
    if let Some(v) = headers.get("x-relay-brain-token").and_then(|h| h.to_str().ok()) {
        return v == expected;
    }
    false
}

pub async fn run_serve(execute: bool) -> Result<()> {
    let cfg = OpsConfig::load();
    let bind = default_bind();
    let token = resolve_token(&cfg);
    let feed_path = default_feed_path();
    let _ = fs::create_dir_all(state_dir());
    let execute = execute || env_truthy("RELAY_BRAIN_EXECUTE");
    let app = BrainApp {
        inner: Arc::new(Mutex::new(BrainState {
            online: true,
            scenarios_source: scenarios::scenarios_dir(cfg.checkout_path().as_deref())
                .display()
                .to_string(),
            ..Default::default()
        })),
        token: token.clone(),
        execute: Arc::new(AtomicBool::new(execute)),
        paused: Arc::new(AtomicBool::new(false)),
        feed_path: feed_path.clone(),
        req_seq: Arc::new(AtomicU64::new(1)),
    };

    append_feed(
        &feed_path,
        &serde_json::json!({
            "type": "brain_started",
            "ts_ms": now_ms(),
            "observe_only": !execute,
            "bind": bind,
        }),
    )?;

    let loop_app = app.clone();
    tokio::spawn(async move {
        probe_loop(loop_app).await;
    });

    let router = Router::new()
        .route("/status", get(api_status))
        .route("/feed", get(api_feed))
        .route("/requests", get(api_requests))
        .route("/requests/:id/approve", post(api_approve))
        .route("/requests/:id/decline", post(api_decline))
        .route("/chat", post(api_chat))
        .route("/say", post(api_chat))
        .route("/resume", post(api_resume))
        .route("/pause", post(api_pause))
        .route("/ack/:id", post(api_ack))
        .with_state(app);

    tracing::info!(
        "relay-brain http://{bind} (token in {} or RELAY_BRAIN_TOKEN)",
        state_dir().join("token").display()
    );
    eprintln!(
        "relay-brain listening on http://{bind} — feed {}",
        feed_path.display()
    );
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    axum::serve(listener, router).await?;
    Ok(())
}

async fn probe_loop(app: BrainApp) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        if app.paused.load(Ordering::Relaxed) {
            continue;
        }
        if let Err(e) = classify_once(&app).await {
            let mut g = app.inner.lock().await;
            g.last_error = Some(format!("{e:#}"));
            let _ = append_feed(
                &app.feed_path,
                &serde_json::json!({
                    "type": "probe_error",
                    "ts_ms": now_ms(),
                    "error": format!("{e:#}"),
                }),
            );
        }
    }
}

async fn classify_once(app: &BrainApp) -> Result<()> {
    let cfg = OpsConfig::load();
    let gitlab = cfg
        .gitlab_url_resolved()
        .ok_or_else(|| anyhow!("set gitlab URL in relay-ops config"))?;
    let (fleet, _source) = gitlab::load_topology(&cfg, &gitlab).await?;
    let (nodes, _ingress, _peers) = probe_fleet(&fleet, Some(gitlab.as_str())).await;
    let spot = if let Some(tok) = cfg.spot_token.as_deref() {
        crate::spot::list_assignments(tok).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let store = scenarios::load_default(cfg.checkout_path().as_deref());
    let mut classifications = Vec::new();
    let mut any_blocked = false;
    let mut last_scenario = None;
    let mut summary_bits = Vec::new();

    for n in &nodes {
        let spot_ip = fleet.node(&n.fqdn).and_then(|m| {
            m.spot_cloudspace.as_ref().and_then(|cs| {
                spot.iter()
                    .find(|a| a.cloudspace == *cs)
                    .map(|a| a.ip.as_str())
            })
        });
        let input = input_for_node(n, spot_ip);
        let hit = store.match_input(&input);
        let class = match &hit {
            None => NodeClassification {
                fqdn: n.fqdn.clone(),
                scenario_id: None,
                message: None,
                blocked: false,
                steps: vec![],
                requires_confirm: false,
            },
            Some(h) => {
                any_blocked |= h.blocked;
                last_scenario = Some(h.id.clone());
                summary_bits.push(format!("{}:{}", n.fqdn, h.id));
                NodeClassification {
                    fqdn: n.fqdn.clone(),
                    scenario_id: Some(h.id.clone()),
                    message: Some(recommend::recommendation_from_hit(&input, h).message),
                    blocked: h.blocked,
                    steps: h.steps.clone(),
                    requires_confirm: h.requires_confirm,
                }
            }
        };

        match &hit {
            Some(h) => {
                resolve_obsolete_pending(app, &n.fqdn, Some(h)).await?;
                maybe_open_request(app, n, h, &class).await?;
                // Safe leaves (requires_confirm=false): auto-run when --execute / RELAY_BRAIN_EXECUTE.
                if app.execute.load(Ordering::Relaxed)
                    && !h.requires_confirm
                    && !h.blocked
                    && !h.steps.is_empty()
                {
                    let key = format!("{}:{}", n.fqdn, h.id);
                    let now = now_ms();
                    let should = {
                        let mut g = app.inner.lock().await;
                        if let Some(prev) = g.auto_ran_ms.get(&key).copied() {
                            if now.saturating_sub(prev) < AUTO_RUN_COOLDOWN_MS {
                                false
                            } else {
                                g.auto_ran_ms.insert(key.clone(), now);
                                true
                            }
                        } else {
                            g.auto_ran_ms.insert(key.clone(), now);
                            true
                        }
                    };
                    if should {
                        let app_c = app.clone();
                        let n_c = n.clone();
                        let h_c = h.clone();
                        let nodes_c = nodes.clone();
                        let fleet_c = fleet.clone();
                        let cfg_c = cfg.clone();
                        tokio::spawn(async move {
                            maybe_auto_run(&app_c, &n_c, &h_c, &nodes_c, &fleet_c, &cfg_c).await;
                        });
                    }
                }
            }
            None => {
                resolve_obsolete_pending(app, &n.fqdn, None).await?;
            }
        }
        classifications.push(class);
    }

    let summary = summary_bits.join(", ");
    {
        let mut g = app.inner.lock().await;
        // Preserve approved request allowlists across re-probes while the same
        // scenario is still active (do not stick steps onto healthy nodes).
        for req in g.requests.values().filter(|r| r.status == "approved") {
            if let Some(fqdn) = req.fqdn.as_deref() {
                if let Some(n) = classifications.iter_mut().find(|n| n.fqdn == fqdn) {
                    if req.scenario_id.is_some()
                        && req.scenario_id.as_deref() == n.scenario_id.as_deref()
                    {
                        n.steps = req.proposed_steps.clone();
                        n.requires_confirm = false;
                        n.blocked = false;
                    }
                }
            }
        }
        g.online = true;
        g.blocked = classifications.iter().any(|c| c.blocked);
        g.last_scenario = last_scenario.clone();
        g.last_fingerprint_summary = summary.clone();
        g.nodes = classifications;
        g.fleet = Some(fleet);
        g.spot = spot;
        g.scenarios_source = store.source.clone();
        g.last_error = None;
    }
    append_feed(
        &app.feed_path,
        &serde_json::json!({
            "type": "classify",
            "ts_ms": now_ms(),
            "blocked": any_blocked,
            "last_scenario": last_scenario,
            "summary": summary,
        }),
    )?;
    let _ = NullScorer::suggest(None);
    Ok(())
}

fn pick_sync_peer<'a>(nodes: &'a [NodeProbe], target_fqdn: &str) -> Option<&'a str> {
    nodes
        .iter()
        .find(|p| p.healthy && !p.fqdn.eq_ignore_ascii_case(target_fqdn))
        .map(|p| p.fqdn.as_str())
}

async fn maybe_auto_run(
    app: &BrainApp,
    n: &NodeProbe,
    hit: &ScenarioHit,
    nodes: &[NodeProbe],
    fleet: &FleetFile,
    cfg: &OpsConfig,
) {
    let peer = pick_sync_peer(nodes, &n.fqdn).map(|s| s.to_string());
    let _ = append_feed(
        &app.feed_path,
        &serde_json::json!({
            "type": "auto_run",
            "ts_ms": now_ms(),
            "fqdn": n.fqdn,
            "scenario_id": hit.id,
            "steps": hit.steps,
            "peer": peer,
        }),
    );

    for step in &hit.steps {
        let result = match step.as_str() {
            "sync" => {
                let Ok(fleet_one) = fleet.filtered(Some(&n.fqdn)) else {
                    let _ = append_feed(
                        &app.feed_path,
                        &serde_json::json!({
                            "type": "auto_run_result",
                            "ts_ms": now_ms(),
                            "fqdn": n.fqdn,
                            "step": step,
                            "ok": false,
                            "error": "fleet filter failed",
                        }),
                    );
                    continue;
                };
                let cfg = cfg.clone();
                let peer_c = peer.clone();
                tokio::task::spawn_blocking(move || {
                    crate::fleet_cmd::sync_node(&cfg, &fleet_one, false, peer_c.as_deref())
                })
                .await
                .unwrap_or_else(|e| Err(anyhow!("join: {e}")))
            }
            "index-warm" => {
                let Ok(fleet_one) = fleet.filtered(Some(&n.fqdn)) else {
                    let _ = append_feed(
                        &app.feed_path,
                        &serde_json::json!({
                            "type": "auto_run_result",
                            "ts_ms": now_ms(),
                            "fqdn": n.fqdn,
                            "step": step,
                            "ok": false,
                            "error": "fleet filter failed",
                        }),
                    );
                    continue;
                };
                crate::fleet_cmd::warm_stale_indexes(&fleet_one, Some(&n.fqdn), None).await
            }
            other => {
                let _ = append_feed(
                    &app.feed_path,
                    &serde_json::json!({
                        "type": "auto_run_skip",
                        "ts_ms": now_ms(),
                        "fqdn": n.fqdn,
                        "step": other,
                        "note": "step not in safe auto-run set",
                    }),
                );
                continue;
            }
        };
        match result {
            Ok(log) => {
                let _ = append_feed(
                    &app.feed_path,
                    &serde_json::json!({
                        "type": "auto_run_result",
                        "ts_ms": now_ms(),
                        "fqdn": n.fqdn,
                        "step": step,
                        "ok": true,
                        "log_tail": log.chars().rev().take(800).collect::<String>().chars().rev().collect::<String>(),
                    }),
                );
            }
            Err(e) => {
                let _ = append_feed(
                    &app.feed_path,
                    &serde_json::json!({
                        "type": "auto_run_result",
                        "ts_ms": now_ms(),
                        "fqdn": n.fqdn,
                        "step": step,
                        "ok": false,
                        "error": format!("{e:#}"),
                    }),
                );
            }
        }
    }
}

fn input_for_node<'a>(n: &'a NodeProbe, spot_ip: Option<&str>) -> RecommendInput<'a> {
    RecommendInput {
        healthy: n.healthy,
        sync_ok: n.sync_ok,
        missing_repos: &n.missing_repos,
        drift: &n.drift,
        sync_anomalies: &n.sync_anomalies,
        dns_mismatch: recommend::dns_mismatch(&n.dns, spot_ip),
        tcp22: n.tcp22,
        https_ok: n.https_config.as_ref().map(|c| c.ok).unwrap_or(false),
        ssh_ok: n.ssh_ok,
        host_key_changed: n.host_key_changed,
        install_present: n.install_present,
        has_spot_ip: spot_ip.is_some(),
        index_ok: n.index_ok,
        index_stale_count: n.index_stale_repos.len(),
    }
}

async fn resolve_obsolete_pending(
    app: &BrainApp,
    fqdn: &str,
    hit: Option<&ScenarioHit>,
) -> Result<()> {
    let auto_decline = match hit {
        None => true,
        Some(h) if !h.requires_confirm && h.id == "catalog_missing" => true,
        _ => false,
    };
    if !auto_decline {
        return Ok(());
    }
    let ids: Vec<String> = {
        let g = app.inner.lock().await;
        g.requests
            .iter()
            .filter(|(_, r)| {
                r.status == "pending"
                    && r.fqdn.as_deref() == Some(fqdn)
                    && matches!(
                        r.scenario_id.as_deref(),
                        Some("catalog_sync") | Some("catalog_missing")
                    )
            })
            .map(|(id, _)| id.clone())
            .collect()
    };
    for id in ids {
        let _ = append_feed(
            &app.feed_path,
            &serde_json::json!({
                "type": "request_resolved",
                "ts_ms": now_ms(),
                "status": "declined",
                "note": "obsolete — catalog_missing is auto-run or node is in sync",
                "request_id": id,
            }),
        );
        if let Some(r) = app.inner.lock().await.requests.get_mut(&id) {
            r.status = "declined".into();
        }
    }
    Ok(())
}

async fn maybe_open_request(
    app: &BrainApp,
    n: &NodeProbe,
    hit: &ScenarioHit,
    class: &NodeClassification,
) -> Result<()> {
    if !hit.requires_confirm && !hit.blocked {
        return Ok(());
    }
    // Dedupe: one pending request per fqdn+scenario; skip if already approved for this leaf
    {
        let g = app.inner.lock().await;
        let exists = g.requests.values().any(|r| {
            r.fqdn.as_deref() == Some(n.fqdn.as_str())
                && r.scenario_id.as_deref() == Some(hit.id.as_str())
                && (r.status == "pending" || r.status == "approved")
        });
        if exists {
            return Ok(());
        }
    }
    let id = new_id();
    let severity = if hit.blocked {
        "blocked"
    } else if hit.steps.iter().any(|s| s == "repair" || s == "reclaim") {
        "high"
    } else {
        "medium"
    };
    let req = BrainRequest {
        id: id.clone(),
        title: hit.title.clone(),
        description: class
            .message
            .clone()
            .unwrap_or_else(|| hit.message.clone()),
        fqdn: Some(n.fqdn.clone()),
        scenario_id: Some(hit.id.clone()),
        proposed_steps: hit.steps.clone(),
        severity: severity.into(),
        status: "pending".into(),
        created_ms: now_ms(),
    };
    append_feed(
        &app.feed_path,
        &serde_json::json!({
            "type": "request_opened",
            "ts_ms": now_ms(),
            "request": &req,
        }),
    )?;
    app.inner.lock().await.requests.insert(id, req);
    Ok(())
}

async fn api_status(State(app): State<BrainApp>) -> Json<BrainStatus> {
    let g = app.inner.lock().await;
    Json(BrainStatus {
        online: g.online,
        observe_only: !app.execute.load(Ordering::Relaxed),
        paused: app.paused.load(Ordering::Relaxed),
        blocked: g.blocked,
        last_scenario: g.last_scenario.clone(),
        last_fingerprint_summary: g.last_fingerprint_summary.clone(),
        pending_requests: g.requests.values().filter(|r| r.status == "pending").count(),
        scenarios_source: g.scenarios_source.clone(),
        feed_path: app.feed_path.display().to_string(),
        nodes: g.nodes.clone(),
        last_error: g.last_error.clone(),
    })
}

#[derive(Deserialize)]
struct FeedQuery {
    #[serde(default = "default_tail")]
    tail: usize,
}

fn default_tail() -> usize {
    100
}

async fn api_feed(State(app): State<BrainApp>, Query(q): Query<FeedQuery>) -> impl IntoResponse {
    match read_feed_tail(&app.feed_path, q.tail) {
        Ok(lines) => (StatusCode::OK, Json(serde_json::json!({ "lines": lines }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

fn read_feed_tail(path: &std::path::Path, tail: usize) -> Result<Vec<serde_json::Value>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = fs::read_to_string(path)?;
    let mut out: Vec<serde_json::Value> = raw
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if out.len() > tail {
        out = out.split_off(out.len() - tail);
    }
    Ok(out)
}

async fn api_requests(State(app): State<BrainApp>) -> Json<serde_json::Value> {
    let g = app.inner.lock().await;
    let mut pending: Vec<_> = g
        .requests
        .values()
        .filter(|r| r.status == "pending")
        .cloned()
        .collect();
    pending.sort_by_key(|r| r.created_ms);
    Json(serde_json::json!({ "pending": pending }))
}

async fn api_approve(
    State(app): State<BrainApp>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, "token required".into());
    }
    resolve_request(&app, &id, "approved").await
}

async fn api_decline(
    State(app): State<BrainApp>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, "token required".into());
    }
    resolve_request(&app, &id, "declined").await
}

async fn api_ack(
    State(app): State<BrainApp>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, "token required".into());
    }
    resolve_request(&app, &id, "acked").await
}

async fn resolve_request(app: &BrainApp, id: &str, status: &str) -> (StatusCode, String) {
    let mut g = app.inner.lock().await;
    let Some(req) = g.requests.get_mut(id) else {
        return (StatusCode::NOT_FOUND, "unknown request".into());
    };
    if req.status != "pending" {
        return (StatusCode::CONFLICT, format!("already {}", req.status));
    }
    req.status = status.into();
    let snap = req.clone();
    if status == "approved" {
        if let Some(fqdn) = snap.fqdn.as_deref() {
            if let Some(n) = g.nodes.iter_mut().find(|n| n.fqdn == fqdn) {
                n.steps = snap.proposed_steps.clone();
                n.requires_confirm = false;
                n.blocked = false;
            }
        }
    }
    drop(g);
    let _ = append_feed(
        &app.feed_path,
        &serde_json::json!({
            "type": "request_resolved",
            "ts_ms": now_ms(),
            "status": status,
            "request": snap,
        }),
    );
    if status == "approved" {
        let _ = append_feed(
            &app.feed_path,
            &serde_json::json!({
                "type": "execute_authorized",
                "ts_ms": now_ms(),
                "request_id": id,
                "steps": snap.proposed_steps,
                "fqdn": snap.fqdn,
                "note": "run via relay-ops job; brain allowlists these steps for this request",
            }),
        );
    }
    (StatusCode::OK, format!("{status} {id}"))
}

#[derive(Deserialize)]
struct ChatBody {
    text: String,
}

async fn api_chat(
    State(app): State<BrainApp>,
    headers: HeaderMap,
    Json(body): Json<ChatBody>,
) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, "token required".into());
    }
    let text = body.text.trim();
    let reply = match text {
        "status" => {
            let g = app.inner.lock().await;
            format!(
                "online={} blocked={} paused={} last={:?} pending={}",
                g.online,
                g.blocked,
                app.paused.load(Ordering::Relaxed),
                g.last_scenario,
                g.requests.values().filter(|r| r.status == "pending").count()
            )
        }
        "blocked" => {
            let g = app.inner.lock().await;
            if g.blocked {
                format!("BLOCKED — {}", g.last_fingerprint_summary)
            } else {
                "not blocked".into()
            }
        }
        "pause" => {
            app.paused.store(true, Ordering::Relaxed);
            "paused".into()
        }
        "resume" => {
            app.paused.store(false, Ordering::Relaxed);
            "resumed".into()
        }
        other => {
            let _ = append_feed(
                &app.feed_path,
                &serde_json::json!({
                    "type": "chat",
                    "ts_ms": now_ms(),
                    "text": other,
                }),
            );
            format!("logged: {other}")
        }
    };
    let _ = append_feed(
        &app.feed_path,
        &serde_json::json!({
            "type": "chat_reply",
            "ts_ms": now_ms(),
            "text": &reply,
        }),
    );
    (StatusCode::OK, reply)
}

async fn api_resume(State(app): State<BrainApp>, headers: HeaderMap) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, "token required".to_string());
    }
    app.paused.store(false, Ordering::Relaxed);
    app.execute.store(true, Ordering::Relaxed);
    (StatusCode::OK, "resumed (execute enabled)".to_string())
}

async fn api_pause(State(app): State<BrainApp>, headers: HeaderMap) -> impl IntoResponse {
    if !check_token(&headers, &app.token) {
        return (StatusCode::UNAUTHORIZED, "token required".to_string());
    }
    app.paused.store(true, Ordering::Relaxed);
    (StatusCode::OK, "paused".to_string())
}

/// Allowlisted job kinds for the currently approved request / classification.
pub async fn allowlisted_steps_for(app: &BrainApp, fqdn: &str) -> Vec<String> {
    let g = app.inner.lock().await;
    // Prefer approved request steps
    for r in g.requests.values() {
        if r.status == "approved" && r.fqdn.as_deref() == Some(fqdn) {
            return r.proposed_steps.clone();
        }
    }
    if let Some(n) = g.nodes.iter().find(|n| n.fqdn == fqdn) {
        if !n.requires_confirm && !n.blocked {
            return n.steps.clone();
        }
    }
    vec![]
}

pub fn handoff_prompt(req: &BrainRequest) -> String {
    format!(
        "Relay brain request {} needs attention.\nTitle: {}\nFQDN: {}\nScenario: {}\nSeverity: {}\nDescription: {}\nProposed steps: {:?}\n\nPlease inspect ops/scenarios/, extend YAML if BLOCKED/unknown, then `relay-ops brain approve {}` or decline.",
        req.id,
        req.title,
        req.fqdn.as_deref().unwrap_or("-"),
        req.scenario_id.as_deref().unwrap_or("-"),
        req.severity,
        req.description,
        req.proposed_steps,
        req.id
    )
}

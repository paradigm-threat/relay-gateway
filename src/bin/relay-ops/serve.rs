use crate::config::OpsConfig;
use crate::inventory::Inventory;
use crate::jobs::Jobs;
use crate::probe::{dead_ingress_ips, healthy_node_ips, probe_fleet, IngressProbe, NodeProbe};
use crate::spot::SpotAssignment;
use crate::topology::FleetFile;
use crate::vercel::Vercel;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct App {
    pub cfg: OpsConfig,
    pub gitlab_url: String,
    pub inner: Arc<Mutex<Inner>>,
    pub jobs: Jobs,
    #[cfg(unix)]
    pub tray_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::tray::TrayView>>,
}

pub struct Inner {
    pub fleet: FleetFile,
    pub topology_source: String,
    pub nodes: Vec<NodeProbe>,
    pub ingress: Option<IngressProbe>,
    pub live_peers: Vec<String>,
    pub spot: Vec<SpotAssignment>,
    pub last_error: Option<String>,
}

#[derive(Serialize)]
struct Snapshot {
    gitlab_url: String,
    topology_source: String,
    git_port: u16,
    inventory: Inventory,
    nodes: Vec<NodeJson>,
    ingress: Option<IngressProbe>,
    live_peers: Vec<String>,
    spot: Vec<SpotAssignment>,
    catalog_repos: Vec<String>,
    job: Option<crate::jobs::JobState>,
    last_error: Option<String>,
}

#[derive(Serialize)]
struct NodeJson {
    fqdn: String,
    dns: Vec<String>,
    tcp22: bool,
    tcp443: bool,
    tcp_git: bool,
    https_config: Option<crate::probe::ConfigProbe>,
    git_tips: Vec<crate::probe::GitTip>,
    healthy: bool,
    missing_repos: Vec<String>,
    drift: Vec<crate::probe::RepoDrift>,
    sync_anomalies: Vec<crate::probe::SyncAnomaly>,
    sync_ok: bool,
    index_ok: bool,
    index_stale_repos: Vec<crate::probe::IndexStaleRepo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http: Option<crate::probe::HttpTrafficProbe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rum: Option<crate::probe::RumProbe>,
    ssh_ok: Option<bool>,
    host_key_changed: bool,
    install_present: Option<bool>,
    recommendation: Option<crate::recommend::Recommendation>,
    visible_actions: crate::recommend::VisibleActions,
    spot_cloudspace: Option<String>,
    deploy_target: Option<String>,
}

pub async fn refresh_inner(app: &App) {
    let gitlab = app.gitlab_url.clone();
    let cfg = app.cfg.clone();
    match crate::gitlab::load_topology(&cfg, &gitlab).await {
        Ok((mut fleet, source)) => {
            let (mut nodes, ingress, live_peers) =
                probe_fleet(&fleet, Some(gitlab.as_str())).await;
            if Inventory::collect(&cfg).admin_ready {
                attach_ssh_diags(&cfg, &fleet, &mut nodes).await;
            }
            fleet.union_peers(&live_peers);
            let spot = if let Some(tok) = cfg.spot_token.as_deref() {
                crate::spot::list_assignments(tok).await.unwrap_or_default()
            } else {
                Vec::new()
            };
            if cfg.auto_drop() {
                if let (Some(ing), Some(tok)) = (ingress.as_ref(), cfg.vercel_token.as_deref()) {
                    let keep = healthy_node_ips(&nodes);
                    let stale = dead_ingress_ips(ing, &keep);
                    let missing = keep
                        .iter()
                        .filter(|ip| !ing.dns.iter().any(|d| d == *ip))
                        .cloned()
                        .collect::<Vec<_>>();
                    let needs_reconcile = !keep.is_empty() && (!stale.is_empty() || !missing.is_empty());
                    if needs_reconcile {
                        if let Ok(v) = Vercel::new(tok, cfg.vercel_team_id.as_deref()) {
                            match v.reconcile_fleet_a(&ing.fqdn, &keep).await {
                                Ok(msg) => {
                                    tracing::info!("{msg}");
                                    if !stale.is_empty() {
                                        crate::notify::notify(
                                            cfg.slack_webhook.as_deref(),
                                            &format!(
                                                "relay-ops fleet ingress sync: dropped stale A {stale:?}"
                                            ),
                                        )
                                        .await;
                                    }
                                    if !missing.is_empty() {
                                        crate::notify::notify(
                                            cfg.slack_webhook.as_deref(),
                                            &format!(
                                                "relay-ops fleet ingress sync: added missing A {missing:?}"
                                            ),
                                        )
                                        .await;
                                    }
                                }
                                Err(e) => tracing::warn!("auto fleet-dns: {e:#}"),
                            }
                        }
                    }
                }
            }
            let unhealthy = crate::probe::attention_nodes(&nodes);
            let nlen = nodes.len();
            let down = crate::probe::down_nodes(&nodes);
            let sync = crate::probe::sync_issue_nodes(&nodes);
            let index_stale = crate::probe::index_stale_nodes(&nodes);
            let summary = nodes
                .iter()
                .map(|n| {
                    let st = if !n.healthy {
                        "down"
                    } else if !n.sync_ok {
                        "sync"
                    } else if !n.index_ok {
                        "index"
                    } else {
                        "up"
                    };
                    format!("{} {st}", n.fqdn)
                })
                .collect::<Vec<_>>()
                .join("\n");
            #[cfg(unix)]
            let tray_view = {
                let inv = crate::inventory::Inventory::collect(&cfg);
                crate::tray::TrayView {
                    unhealthy,
                    down,
                    sync,
                    index_stale,
                    nodes: nlen,
                    summary: summary.clone(),
                    mode: if inv.admin_ready {
                        "Admin".into()
                    } else {
                        "Observer".into()
                    },
                    lines: crate::tray::menu_lines(&nodes, &fleet, &spot, ingress.as_ref()),
                }
            };
            let mut g = app.inner.lock().await;
            g.fleet = fleet;
            g.topology_source = source;
            g.nodes = nodes;
            g.ingress = ingress;
            g.live_peers = live_peers;
            g.spot = spot;
            g.last_error = None;
            drop(g);
            #[cfg(unix)]
            if let Some(tx) = &app.tray_tx {
                let _ = tx.send(tray_view);
            }
        }
        Err(e) => {
            app.inner.lock().await.last_error = Some(format!("{e:#}"));
        }
    }
}

async fn attach_ssh_diags(cfg: &OpsConfig, fleet: &FleetFile, nodes: &mut [NodeProbe]) {
    crate::ssh::attach_diags(cfg, fleet, nodes).await;
}

pub async fn serve(app: App, bind: &str) -> anyhow::Result<()> {
    refresh_inner(&app).await;
    let bg = app.clone();
    tokio::spawn(async move {
        loop {
            let secs = {
                let g = bg.inner.lock().await;
                crate::probe::next_probe_secs(
                    &g.nodes,
                    bg.cfg.probe_interval(),
                    bg.cfg.probe_interval_down(),
                )
            };
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            refresh_inner(&bg).await;
        }
    });
    let router = Router::new()
        .route("/", get(ui))
        .route("/icon.png", get(icon_png))
        .route("/favicon.png", get(favicon_png))
        .route("/favicon.ico", get(favicon_png))
        .route("/api/snapshot", get(snapshot))
        .route("/api/refresh", post(refresh))
        .route("/api/fleet-dns-drop", post(fleet_dns_drop))
        .route("/api/point-dns", post(point_dns))
        .route("/api/repair", post(reclaim))
        .route("/api/reclaim", post(reclaim))
        .route("/api/job", post(job))
        .route("/api/known-hosts-reset", post(known_hosts))
        .route("/api/git/refs", get(git_refs))
        .route("/api/git/tree", get(git_tree))
        .route("/api/git/file", get(git_file))
        .route("/api/mail/list", get(mail_list))
        .route("/api/mail/message", get(mail_message))
        .route("/api/mail/mark-read", post(mail_mark_read))
        .route("/api/brain/status", get(brain_proxy_get))
        .route("/api/brain/feed", get(brain_proxy_get))
        .route("/api/brain/requests", get(brain_proxy_get))
        .route("/api/brain/chat", post(brain_proxy_post))
        .route("/api/brain/resume", post(brain_proxy_post))
        .route("/api/brain/pause", post(brain_proxy_post))
        .route("/api/brain/requests/:id/approve", post(brain_proxy_post))
        .route("/api/brain/requests/:id/decline", post(brain_proxy_post))
        .route("/api/mcp-tools", get(mcp_tools))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("relay-ops UI http://{bind}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn ui() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn icon_png() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            ("content-type", "image/png"),
            ("cache-control", "public, max-age=86400"),
        ],
        include_bytes!("icon.png").as_slice(),
    )
}

async fn favicon_png() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            ("content-type", "image/png"),
            ("cache-control", "public, max-age=86400"),
        ],
        include_bytes!("favicon.png").as_slice(),
    )
}

async fn mcp_tools() -> impl IntoResponse {
    match std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ops/mcp-tools.json"),
    ) {
        Ok(s) => (StatusCode::OK, [("content-type", "application/json")], s).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!("ops/mcp-tools.json: {e}"),
        )
            .into_response(),
    }
}

fn brain_upstream_path(uri_path: &str) -> String {
    uri_path
        .strip_prefix("/api/brain")
        .unwrap_or(uri_path)
        .to_string()
}

async fn brain_proxy_get(
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
) -> impl IntoResponse {
    let path = brain_upstream_path(uri.path());
    let q = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    match crate::brain_client::get_json_async(&format!("{path}{q}")).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn brain_proxy_post(
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let path = brain_upstream_path(uri.path());
    let body = body.map(|Json(v)| v);
    match crate::brain_client::post_auth_async(&path, body).await {
        Ok(t) => (StatusCode::OK, t).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// When brain is reachable, refuse remediations that are not allowlisted /
/// approved for the target fqdn.
async fn brain_job_gate(kind: &str, fqdn: Option<&str>) -> Result<(), String> {
    const GATED: &[&str] = &[
        "repair",
        "reclaim",
        "sync",
        "point-dns",
        "index-warm",
        "update",
        "deploy-all",
        "reclone-repo",
    ];
    if !GATED.contains(&kind) {
        return Ok(());
    }
    let Ok(st) = crate::brain_client::get_json_async("/status").await else {
        return Ok(());
    };
    if !st.get("online").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(());
    }
    let Some(fqdn) = fqdn.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if let Some(nodes) = st.get("nodes").and_then(|n| n.as_array()) {
        for n in nodes {
            if n.get("fqdn").and_then(|x| x.as_str()) != Some(fqdn) {
                continue;
            }
            let blocked = n.get("blocked").and_then(|v| v.as_bool()).unwrap_or(false);
            let confirm = n
                .get("requires_confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let steps: Vec<&str> = n
                .get("steps")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            if blocked {
                return Err(format!(
                    "brain BLOCKED for {fqdn} — approve/decline the pending request or extend ops/scenarios"
                ));
            }
            if confirm {
                return Err(format!(
                    "brain requires confirmation for {fqdn} ({kind}) — open Brain tab and Approve the request first"
                ));
            }
            if !steps.is_empty() && !steps.contains(&kind) {
                return Err(format!(
                    "brain allowlist for {fqdn} is {steps:?}; refused {kind}"
                ));
            }
            return Ok(());
        }
    }
    Ok(())
}

async fn snapshot(State(app): State<App>) -> Json<Snapshot> {
    let g = app.inner.lock().await;
    let inv = Inventory::collect(&app.cfg);
    let nodes = g
        .nodes
        .iter()
        .map(|n| {
            let meta = g.fleet.node(&n.fqdn);
            let spot_ip = meta
                .and_then(|m| m.spot_cloudspace.as_ref())
                .and_then(|cs| {
                    g.spot
                        .iter()
                        .find(|a| a.cloudspace == *cs)
                        .map(|a| a.ip.as_str())
                });
            let (rec, vis) = crate::recommend::for_node(n, spot_ip);
            NodeJson {
                fqdn: n.fqdn.clone(),
                dns: n.dns.clone(),
                tcp22: n.tcp22,
                tcp443: n.tcp443,
                tcp_git: n.tcp_git,
                https_config: n.https_config.clone(),
                git_tips: n.git_tips.clone(),
                healthy: n.healthy,
                missing_repos: n.missing_repos.clone(),
                drift: n.drift.clone(),
                sync_anomalies: n.sync_anomalies.clone(),
                sync_ok: n.sync_ok,
                index_ok: n.index_ok,
                index_stale_repos: n.index_stale_repos.clone(),
                http: n.http.clone(),
                rum: n.rum.clone(),
                ssh_ok: n.ssh_ok,
                host_key_changed: n.host_key_changed,
                install_present: n.install_present,
                recommendation: rec,
                visible_actions: vis,
                spot_cloudspace: meta.and_then(|m| m.spot_cloudspace.clone()),
                deploy_target: meta.and_then(|m| m.deploy_target.clone()),
            }
        })
        .collect();
    Json(Snapshot {
        gitlab_url: app.gitlab_url.clone(),
        topology_source: g.topology_source.clone(),
        git_port: g.fleet.git_port(),
        inventory: inv,
        nodes,
        ingress: g.ingress.clone(),
        live_peers: g.live_peers.clone(),
        spot: g.spot.clone(),
        catalog_repos: crate::probe::expected_catalog_repos(&g.nodes, &g.fleet.git_repos),
        job: app.jobs.snapshot().await,
        last_error: g.last_error.clone(),
    })
}

async fn refresh(State(app): State<App>) -> impl IntoResponse {
    refresh_inner(&app).await;
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct FqdnIp {
    #[serde(default)]
    fqdn: String,
    ip: Option<String>,
    #[serde(default)]
    ips: Vec<String>,
}

async fn fleet_dns_drop(State(app): State<App>) -> impl IntoResponse {
    let inv = Inventory::collect(&app.cfg);
    if !inv.vercel_token {
        return (StatusCode::BAD_REQUEST, "needs Vercel token".to_string());
    }
    let g = app.inner.lock().await;
    let Some(ing) = g.ingress.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            "no fleetIngress in topology".into(),
        );
    };
    let keep = healthy_node_ips(&g.nodes);
    drop(g);
    if keep.is_empty() {
        return (
            StatusCode::CONFLICT,
            "all nodes unhealthy; refusing to empty fleet A set".into(),
        );
    }
    match Vercel::new(
        app.cfg.vercel_token.as_deref().unwrap(),
        app.cfg.vercel_team_id.as_deref(),
    ) {
        Ok(v) => match v.reconcile_fleet_a(&ing.fqdn, &keep).await {
            Ok(msg) => (StatusCode::OK, msg),
            Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")),
        },
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn point_dns(State(app): State<App>, Json(body): Json<FqdnIp>) -> impl IntoResponse {
    if !Inventory::collect(&app.cfg).vercel_token {
        return (StatusCode::BAD_REQUEST, "needs Vercel token".to_string());
    }
    let Some(ip) = body.ip.filter(|s| !s.is_empty()) else {
        return (StatusCode::BAD_REQUEST, "ip required".into());
    };
    match Vercel::new(
        app.cfg.vercel_token.as_deref().unwrap(),
        app.cfg.vercel_team_id.as_deref(),
    ) {
        Ok(v) => match v.point_a(&body.fqdn, &ip).await {
            Ok(msg) => (StatusCode::OK, msg),
            Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")),
        },
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn reclaim(State(app): State<App>, Json(body): Json<FqdnIp>) -> impl IntoResponse {
    if !Inventory::collect(&app.cfg).admin_ready {
        return (StatusCode::BAD_REQUEST, "needs ssh + identity".to_string());
    }
    let fleet = app.inner.lock().await.fleet.clone();
    match app
        .jobs
        .start_reclaim(app.cfg.clone(), fleet, body.fqdn, body.ip)
        .await
    {
        Ok(id) => (StatusCode::ACCEPTED, format!("job {id} started")),
        Err(e) => (StatusCode::CONFLICT, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct JobReq {
    kind: String,
    fqdn: Option<String>,
    ip: Option<String>,
    repo: Option<String>,
    peer: Option<String>,
    mcp: Option<bool>,
    piper: Option<String>,
    npm_pkgs: Option<String>,
    translation: Option<String>,
    translation_pkgs: Option<String>,
    libretranslate: Option<String>,
    inbound_mail: Option<String>,
    mail_domains: Option<String>,
    mail_user: Option<String>,
    mail_hostname: Option<String>,
    drop_unhealthy: Option<bool>,
    dry_run: Option<bool>,
}

async fn job(State(app): State<App>, Json(body): Json<JobReq>) -> impl IntoResponse {
    let inv = Inventory::collect(&app.cfg);
    let kind = body.kind.as_str();
    let needs_admin = !matches!(kind, "health" | "ssl" | "hosts");
    if needs_admin && !inv.admin_ready {
        return (StatusCode::BAD_REQUEST, "needs ssh + identity".to_string());
    }
    if matches!(kind, "fleet-dns" | "fleet-dns-drop") && !inv.vercel_token {
        return (StatusCode::BAD_REQUEST, "needs Vercel token".to_string());
    }
    if let Err(msg) = brain_job_gate(kind, body.fqdn.as_deref()).await {
        return (StatusCode::FORBIDDEN, msg);
    }
    let (fleet_all, source) = {
        let g = app.inner.lock().await;
        (g.fleet.clone(), g.topology_source.clone())
    };
    let fleet = match fleet_all.filtered(body.fqdn.as_deref()) {
        Ok(f) => f,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let cfg = app.cfg.clone();
    let target = body.fqdn.clone().unwrap_or_else(|| {
        fleet
            .nodes
            .iter()
            .map(|n| n.fqdn.clone())
            .collect::<Vec<_>>()
            .join(",")
    });
    let started = match kind {
        "hosts" => {
            app.jobs
                .start_blocking("hosts", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    let source = source.clone();
                    move || Ok(crate::fleet_cmd::hosts_text(&cfg, &fleet, &source))
                })
                .await
        }
        "health" => {
            let mcp = body.mcp.unwrap_or(false);
            app.jobs
                .start_async("health", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || async move { crate::fleet_cmd::health(&cfg, &fleet, mcp).await }
                })
                .await
        }
        "ssl" => {
            app.jobs
                .start_async("ssl", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || async move { crate::fleet_cmd::ssl_check(&cfg, &fleet).await }
                })
                .await
        }
        "ssh-check" => {
            app.jobs
                .start_blocking("ssh-check", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || crate::fleet_cmd::ssh_check(&cfg, &fleet, false)
                })
                .await
        }
        "drift" => {
            app.jobs
                .start_blocking("drift", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || crate::fleet_cmd::drift(&cfg, &fleet, false)
                })
                .await
        }
        "sync" => {
            let peer = body.peer.clone();
            app.jobs
                .start_blocking("sync", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || crate::fleet_cmd::sync_node(&cfg, &fleet, false, peer.as_deref())
                })
                .await
        }
        "repair" => {
            app.jobs
                .start_blocking("repair", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || crate::fleet_cmd::remote_install(&cfg, &fleet, false, "repair", None)
                })
                .await
        }
        "update" | "deploy-all" => {
            app.jobs
                .start_blocking("update", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || crate::fleet_cmd::update(&cfg, &fleet, false, true, false)
                })
                .await
        }
        "features-set" => {
            let env_body = crate::fleet_cmd::features_env(
                body.piper.as_deref(),
                body.npm_pkgs.as_deref(),
                body.translation.as_deref(),
                body.translation_pkgs.as_deref(),
                body.libretranslate.as_deref(),
                body.inbound_mail.as_deref(),
                body.mail_domains.as_deref(),
                body.mail_user.as_deref(),
                body.mail_hostname.as_deref(),
            );
            app.jobs
                .start_blocking("features-set", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || {
                        crate::fleet_cmd::remote_install(
                            &cfg,
                            &fleet,
                            false,
                            "reconfigure-features",
                            Some(&env_body),
                        )
                    }
                })
                .await
        }
        "reclone-repo" => {
            let Some(repo) = body.repo.clone() else {
                return (StatusCode::BAD_REQUEST, "repo required".into());
            };
            let peer = body.peer.clone();
            app.jobs
                .start_blocking("reclone-repo", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || {
                        crate::fleet_cmd::reclone_repo(&cfg, &fleet, false, &repo, peer.as_deref())
                    }
                })
                .await
        }
        "fix-bare-head" => {
            app.jobs
                .start_blocking("fix-bare-head", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || crate::fleet_cmd::fix_bare_head(&cfg, &fleet, false)
                })
                .await
        }
        "index-warm" => {
            let fqdn = body.fqdn.clone();
            let repo = body.repo.clone();
            let app2 = app.clone();
            app.jobs
                .start_async("index-warm", &target, {
                    let fleet = fleet.clone();
                    move || async move {
                        let result = crate::fleet_cmd::warm_stale_indexes(
                            &fleet,
                            fqdn.as_deref(),
                            repo.as_deref(),
                        )
                        .await;
                        refresh_inner(&app2).await;
                        result
                    }
                })
                .await
        }
        "fleet-dns" => {
            let drop = body.drop_unhealthy.unwrap_or(false);
            let dry = body.dry_run.unwrap_or(true);
            app.jobs
                .start_async("fleet-dns", &target, {
                    let cfg = cfg.clone();
                    let fleet = fleet.clone();
                    move || async move {
                        crate::fleet_cmd::fleet_dns(&cfg, &fleet, drop, dry, None).await
                    }
                })
                .await
        }
        "reclaim" => {
            let fqdn = body.fqdn.clone().unwrap_or_default();
            if fqdn.is_empty() {
                return (StatusCode::BAD_REQUEST, "fqdn required".into());
            }
            app.jobs.start_reclaim(cfg, fleet_all, fqdn, body.ip).await
        }
        other => return (StatusCode::BAD_REQUEST, format!("unknown job kind {other}")),
    };
    match started {
        Ok(id) => (StatusCode::ACCEPTED, format!("job {id} started")),
        Err(e) => (StatusCode::CONFLICT, format!("{e:#}")),
    }
}

async fn known_hosts(State(app): State<App>, Json(body): Json<FqdnIp>) -> impl IntoResponse {
    if !Inventory::collect(&app.cfg).admin_ready {
        return (StatusCode::BAD_REQUEST, "needs ssh + identity".to_string());
    }
    let (target, nodes) = {
        let g = app.inner.lock().await;
        if body.fqdn.trim().is_empty() {
            (
                "fleet".to_string(),
                fleet_known_hosts_targets(&g.nodes, &g.fleet, &g.spot),
            )
        } else {
            let mut ips = Vec::new();
            if let Some(ip) = body.ip.filter(|s| !s.trim().is_empty()) {
                ips.push(ip);
            }
            ips.extend(body.ips.into_iter().filter(|s| !s.trim().is_empty()));
            if let Some(n) = g.nodes.iter().find(|n| n.fqdn == body.fqdn) {
                ips.extend(n.dns.iter().cloned());
            }
            if let Some(meta) = g.fleet.node(&body.fqdn) {
                if let Some(cs) = &meta.spot_cloudspace {
                    if let Some(a) = g.spot.iter().find(|a| a.cloudspace == *cs) {
                        ips.push(a.ip.clone());
                    }
                }
            }
            (body.fqdn.clone(), vec![(body.fqdn.clone(), ips)])
        }
    };
    let result =
        tokio::task::spawn_blocking(move || crate::jobs::reset_known_hosts_many(&nodes)).await;
    let recorded = match result {
        Ok(inner) => inner,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")),
    };
    let ok = recorded.is_ok();
    let msg = match &recorded {
        Ok(m) => m.clone(),
        Err(e) => format!("{e:#}"),
    };
    let _ = app
        .jobs
        .record_finished("known-hosts-reset", &target, recorded)
        .await;
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        msg,
    )
}

fn fleet_known_hosts_targets(
    nodes: &[crate::probe::NodeProbe],
    fleet: &crate::topology::FleetFile,
    spot: &[crate::spot::SpotAssignment],
) -> Vec<(String, Vec<String>)> {
    nodes
        .iter()
        .map(|n| {
            let mut ips = n.dns.clone();
            if let Some(meta) = fleet.node(&n.fqdn) {
                if let Some(cs) = &meta.spot_cloudspace {
                    if let Some(a) = spot.iter().find(|a| a.cloudspace == *cs) {
                        ips.push(a.ip.clone());
                    }
                }
            }
            (n.fqdn.clone(), ips)
        })
        .collect()
}

#[derive(Deserialize)]
struct BrowseQ {
    fqdn: String,
    repo: String,
    #[serde(default)]
    git_ref: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

async fn browse_port(app: &App, fqdn: &str) -> Result<u16, String> {
    let g = app.inner.lock().await;
    if !g
        .fleet
        .nodes
        .iter()
        .any(|n| n.fqdn.eq_ignore_ascii_case(fqdn))
    {
        return Err("unknown fleet host".into());
    }
    Ok(g.fleet.git_port())
}

async fn git_refs(State(app): State<App>, Query(q): Query<BrowseQ>) -> Response {
    let port = match browse_port(&app, &q.fqdn).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };
    match crate::browse::list_heads(&q.fqdn, port, &q.repo).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

async fn git_tree(State(app): State<App>, Query(q): Query<BrowseQ>) -> Response {
    let port = match browse_port(&app, &q.fqdn).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };
    let git_ref = q.git_ref.as_deref().unwrap_or("main");
    let prefix = q.prefix.as_deref().unwrap_or("");
    match crate::browse::list_tree(&q.fqdn, port, &q.repo, git_ref, prefix).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

async fn git_file(State(app): State<App>, Query(q): Query<BrowseQ>) -> Response {
    let port = match browse_port(&app, &q.fqdn).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };
    let git_ref = q.git_ref.as_deref().unwrap_or("main");
    let Some(path) = q.path.filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "path required"})),
        )
            .into_response();
    };
    match crate::browse::show_file(&q.fqdn, port, &q.repo, git_ref, &path).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct MailIdQ {
    id: String,
}

async fn mail_list(State(app): State<App>) -> Response {
    let (cfg, fleet) = {
        let g = app.inner.lock().await;
        (app.cfg.clone(), g.fleet.clone())
    };
    let target = crate::mail::MailTarget::resolve(&cfg, &fleet);
    match tokio::task::spawn_blocking(move || crate::mail::list_messages(&cfg, &fleet)).await {
        Ok(Ok(items)) => Json(serde_json::json!({
            "host": target.host,
            "maildir": target.maildir,
            "mx": target.host,
            "messages": items,
        }))
        .into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

async fn mail_message(State(app): State<App>, Query(q): Query<MailIdQ>) -> Response {
    let (cfg, fleet) = {
        let g = app.inner.lock().await;
        (app.cfg.clone(), g.fleet.clone())
    };
    let id = q.id;
    match tokio::task::spawn_blocking(move || crate::mail::get_message(&cfg, &fleet, &id)).await {
        Ok(Ok(msg)) => Json(msg).into_response(),
        Ok(Err(e)) => {
            let code = if e.to_string().contains("invalid mail id") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            (code, Json(serde_json::json!({"error": format!("{e:#}")}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

async fn mail_mark_read(State(app): State<App>, Json(body): Json<MailIdQ>) -> Response {
    let (cfg, fleet) = {
        let g = app.inner.lock().await;
        (app.cfg.clone(), g.fleet.clone())
    };
    let id = body.id;
    match tokio::task::spawn_blocking(move || crate::mail::mark_read(&cfg, &fleet, &id)).await {
        Ok(Ok(item)) => Json(item).into_response(),
        Ok(Err(e)) => {
            let code = if e.to_string().contains("invalid mail id") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            (code, Json(serde_json::json!({"error": format!("{e:#}")}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

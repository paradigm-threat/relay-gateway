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
    sync_ok: bool,
    spot_cloudspace: Option<String>,
    deploy_target: Option<String>,
}

pub async fn refresh_inner(app: &App) {
    let gitlab = app.gitlab_url.clone();
    let cfg = app.cfg.clone();
    match crate::gitlab::load_topology(&cfg, &gitlab).await {
        Ok((mut fleet, source)) => {
            let (nodes, ingress, live_peers) = probe_fleet(&fleet).await;
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
                    if !stale.is_empty() && !keep.is_empty() {
                        if let Ok(v) = Vercel::new(tok, cfg.vercel_team_id.as_deref()) {
                            match v.reconcile_fleet_a(&ing.fqdn, &keep).await {
                                Ok(msg) => {
                                    tracing::info!("{msg}");
                                    crate::notify::notify(
                                        cfg.slack_webhook.as_deref(),
                                        &format!(
                                            "relay-ops dropped dead fleet A records: {stale:?}"
                                        ),
                                    )
                                    .await;
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
            let summary = nodes
                .iter()
                .map(|n| {
                    let st = if !n.healthy {
                        "down"
                    } else if !n.sync_ok {
                        "sync"
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
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("relay-ops UI http://{bind}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn ui() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn snapshot(State(app): State<App>) -> Json<Snapshot> {
    let g = app.inner.lock().await;
    let inv = Inventory::collect(&app.cfg);
    let nodes = g
        .nodes
        .iter()
        .map(|n| {
            let meta = g.fleet.node(&n.fqdn);
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
                sync_ok: n.sync_ok,
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
    fqdn: String,
    ip: Option<String>,
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
    match crate::jobs::reset_known_hosts(&body.fqdn, body.ip.as_deref()) {
        Ok(msg) => (StatusCode::OK, msg),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
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

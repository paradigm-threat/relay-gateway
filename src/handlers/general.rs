use std::collections::HashMap;
use std::path::PathBuf;

use axum::{
    extract::{Path as AxPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use git2::Repository;
use serde::Serialize;

use crate::{authorized_repos, git, helpers, notify, types::*};

/// Serve a minimal OpenAPI YAML specification (placeholder)
pub async fn get_openapi_yaml() -> impl IntoResponse {
    let yaml = r#"openapi: 3.0.0
info:
  title: Relay API
  version: 0.0.0
paths: {}
"#;
    (StatusCode::OK, [("Content-Type", "application/yaml")], yaml)
}

/// Serve Swagger UI HTML page
pub async fn get_swagger_ui() -> impl IntoResponse {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="description" content="SwaggerUI" />
    <title>SwaggerUI</title>
    <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5.11.0/swagger-ui.css" />
</head>
<body>
<div id="swagger-ui"></div>
<script src="https://unpkg.com/swagger-ui-dist@5.11.0/swagger-ui-bundle.js" crossorigin></script>
<script>
    window.onload = () => {
        window.ui = SwaggerUIBundle({
            url: '/openapi.yaml',
            dom_id: '#swagger-ui',
            deepLinking: true,
            presets: [
                SwaggerUIBundle.presets.apis,
                SwaggerUIBundle.presets.standalone
            ],
            plugins: [
                SwaggerUIBundle.plugins.DownloadUrl
            ],
            layout: "BaseLayout"
        });
    };
    </script>
</body>
</html>"#;
    (StatusCode::OK, [("Content-Type", "text/html")], html)
}

pub(crate) fn read_startup_catalog_sync_status(
    repo_path: &std::path::Path,
) -> Option<serde_json::Value> {
    let state_file = repo_path
        .parent()?
        .join("state")
        .join("startup_catalog_sync.json");
    let s = std::fs::read_to_string(&state_file).ok()?;
    serde_json::from_str(&s).ok()
}

/// POST /api/config — peers, repo list, optional `node_fqdn`, server id, authorized repo names
pub async fn post_api_config(State(state): State<AppState>) -> impl IntoResponse {
    #[derive(Serialize)]
    struct Config {
        peers: Vec<String>,
        repos: Vec<String>,
        /// Node hostname for HTTP: use **`{repo}.{node_fqdn}`** as `Host` (no `X-Relay-Repo`, no `?repo=`).
        #[serde(skip_serializing_if = "Option::is_none")]
        node_fqdn: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        relay_server_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        authorized_repos: Option<Vec<String>>,
        /// Install-time feature manifest (Piper, npm extensions, ports). See relay-install.sh.
        #[serde(skip_serializing_if = "Option::is_none")]
        installed_features: Option<serde_json::Value>,
        /// When true, `POST /api/git/ensure-bare/:repo` is allowed (no HTTP auth; protect at firewall if needed).
        git_ensure_bare_enabled: bool,
        /// Last `relay-startup-catalog-sync.sh` run (`state/startup_catalog_sync.json` next to `RELAY_REPO_PATH`).
        #[serde(skip_serializing_if = "Option::is_none")]
        startup_catalog_sync: Option<serde_json::Value>,
    }

    let peer_list = std::env::var("RELAY_MASTER_PEER_LIST")
        .unwrap_or_default()
        .split(';')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();

    let repos = git::bare_repo_names(&state.repo_path);

    let authorized_repos = state.authorized_repos.as_ref().map(|a| {
        a.repos
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    });

    let installed_features = state.features_manifest.as_ref().map(|m| {
        serde_json::json!({
            "manifest": m.as_ref(),
            "summary": summarize_features(m),
        })
    });

    let startup_catalog_sync = read_startup_catalog_sync_status(&state.repo_path);

    let config = Config {
        peers: peer_list,
        repos,
        node_fqdn: state.node_fqdn.clone(),
        relay_server_id: state.relay_server_id.clone(),
        authorized_repos,
        installed_features,
        git_ensure_bare_enabled: state.git_ensure_bare_enabled,
        startup_catalog_sync,
    };
    (StatusCode::OK, Json(config))
}

pub(crate) fn summarize_features(m: &std::sync::Arc<serde_json::Value>) -> serde_json::Value {
    let f = m.get("features").cloned().unwrap_or(serde_json::json!({}));
    let piper = f.get("piper_tts");
    let npm = f.get("npm_extensions");
    let translation = f.get("text_translation");
    let libre = f.get("libretranslate_api");
    let voice_count = piper
        .and_then(|p| p.get("voices"))
        .and_then(|v| v.as_array())
        .map(|a| a.len());
    let pair_count = translation
        .and_then(|p| p.get("language_pairs"))
        .and_then(|v| v.as_array())
        .map(|a| a.len());
    serde_json::json!({
        "http_port": m.get("http_port"),
        "git_port": m.get("git_port"),
        "piper_tts": {
            "enabled": piper.and_then(|p| p.get("enabled")),
            "http_port": piper.and_then(|p| p.get("http_port")),
            "voice_count": voice_count,
            "languages": piper.and_then(|p| p.get("languages")),
        },
        "npm_extensions": {
            "enabled": npm.and_then(|p| p.get("enabled")),
            "packages": npm.and_then(|p| p.get("packages")),
        },
        "text_translation": {
            "enabled": translation.and_then(|p| p.get("enabled")),
            "backend": translation.and_then(|p| p.get("backend")),
            "language_pair_count": pair_count,
            "from_languages": translation.and_then(|p| p.get("from_languages")),
            "to_languages": translation.and_then(|p| p.get("to_languages")),
        },
        "libretranslate_api": {
            "enabled": libre.and_then(|p| p.get("enabled")),
            "http_port": libre.and_then(|p| p.get("http_port")),
            "load_only": libre.and_then(|p| p.get("load_only")),
        },
    })
}

/// POST /git-pull — fetch from `origin` into a **bare** repo.
/// Target resolution: **`Host: {repo}.{node_fqdn}`** (same as HTTP file serving). No `?repo=` or `X-Relay-Repo`.
/// Legacy: if `RELAY_REPO_PATH` is itself a bare `*.git` directory and no per-repo dirs exist, that repo is used.
pub async fn post_git_pull(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    #[derive(Serialize)]
    struct GitPullResponse {
        success: bool,
        message: String,
        updated: bool,
        repo: Option<String>,
        before_commit: Option<String>,
        after_commit: Option<String>,
        error: Option<String>,
    }

    let names = git::repo_names_for_state(&state);

    let (bare_path, repo_label) = if let Some(n) =
        helpers::repo_from_host_or_default(&state, state.node_fqdn.as_deref(), &headers)
    {
        match &state.serve_context {
            crate::types::ServeContext::LocalWorktree { checkout, .. } => (checkout.clone(), n),
            crate::types::ServeContext::BareFleet => (state.repo_path.join(format!("{}.git", n)), n),
        }
    } else if names.is_empty() && Repository::open_bare(&state.repo_path).is_ok() {
        let stem = state
            .repo_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("repo")
            .to_string();
        (state.repo_path.clone(), stem)
    } else {
        let msg = if state.node_fqdn.is_none() {
            "No bare repository resolved. Set RELAY_PUBLIC_HOSTNAME to this node's FQDN and call with Host: {repo}.{that-fqdn}; repos live under RELAY_REPO_PATH as <repo>.git".to_string()
        } else {
            format!(
                "No bare repository resolved. Use Host header: {{repo}}.{}",
                state.node_fqdn.as_deref().unwrap_or("")
            )
        };
        return (
            StatusCode::BAD_REQUEST,
            Json(GitPullResponse {
                success: false,
                message: msg.clone(),
                updated: false,
                repo: None,
                before_commit: None,
                after_commit: None,
                error: Some(msg),
            }),
        );
    };

    if let Some(ref cfg) = state.authorized_repos {
        if let Err(msg) = authorized_repos::ensure_pull_allowed(
            cfg,
            state.relay_server_id.as_deref(),
            &repo_label,
        ) {
            tracing::warn!("git-pull denied: {}", msg);
            return (
                StatusCode::FORBIDDEN,
                Json(GitPullResponse {
                    success: false,
                    message: msg.clone(),
                    updated: false,
                    repo: Some(repo_label),
                    before_commit: None,
                    after_commit: None,
                    error: Some(msg),
                }),
            );
        }
    }

    let repo = match Repository::open_bare(&bare_path) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to open bare repo at {:?}: {}", bare_path, e);
            tracing::error!("git-pull: {}", msg);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(GitPullResponse {
                    success: false,
                    message: msg.clone(),
                    updated: false,
                    repo: Some(repo_label),
                    before_commit: None,
                    after_commit: None,
                    error: Some(msg),
                }),
            );
        }
    };

    let track_branch = state
        .authorized_repos
        .as_ref()
        .and_then(|c| c.repos.get(&repo_label))
        .map(|a| a.branch.clone())
        .unwrap_or_else(|| "main".to_string());

    let before_commit = repo
        .find_reference(&format!("refs/heads/{}", track_branch))
        .ok()
        .and_then(|r| r.target())
        .map(|o| o.to_string());

    let mut remote = match repo.find_remote("origin") {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Remote 'origin' not found: {}", e);
            return (
                StatusCode::OK,
                Json(GitPullResponse {
                    success: false,
                    message: msg.clone(),
                    updated: false,
                    repo: Some(repo_label),
                    before_commit,
                    after_commit: None,
                    error: Some(msg),
                }),
            );
        }
    };

    let fetch_specs = ["+refs/heads/*:refs/heads/*"];
    let fetch_res = remote.fetch(&fetch_specs, None, None);
    if let Err(e) = fetch_res {
        let fallback = remote.fetch(&["+refs/heads/main:refs/heads/main"], None, None);
        if let Err(e2) = fallback {
            let msg = format!("Fetch failed: {} (fallback main: {})", e, e2);
            tracing::warn!("git-pull: {}", msg);
            notify::spawn_slack_text(format!(
                "[Relay] git_pull_fetch_failed\nrepo: {}\nnode: {:?}\n{}",
                repo_label,
                state.node_fqdn.as_deref(),
                msg
            ));
            return (
                StatusCode::OK,
                Json(GitPullResponse {
                    success: false,
                    message: msg.clone(),
                    updated: false,
                    repo: Some(repo_label),
                    before_commit: before_commit.clone(),
                    after_commit: None,
                    error: Some(msg),
                }),
            );
        }
    }

    let after_commit = repo
        .find_reference(&format!("refs/heads/{}", track_branch))
        .ok()
        .and_then(|r| r.target())
        .map(|o| o.to_string());

    if let Some(ref cfg) = state.authorized_repos {
        if let Err(msg) = authorized_repos::validate_anchor(&repo, &repo_label, cfg) {
            tracing::warn!("git-pull trust validation failed: {}", msg);
            let after_tip = after_commit.clone();
            authorized_repos::rollback_branch(&repo, &track_branch, before_commit.as_deref());
            crate::mesh_events::spawn_publish_pull_validation_failure(
                crate::mesh_events::PullValidationPublishCtx {
                    source: "git_pull".into(),
                    repo: repo_label.clone(),
                    branch: track_branch.clone(),
                    error: msg.clone(),
                    relay_server_id: state.relay_server_id.clone(),
                    node_fqdn: state.node_fqdn.clone(),
                    after_tip,
                    trigger_peer: None,
                },
            );
            return (
                StatusCode::FORBIDDEN,
                Json(GitPullResponse {
                    success: false,
                    message: msg.clone(),
                    updated: false,
                    repo: Some(repo_label),
                    before_commit: before_commit.clone(),
                    after_commit: after_commit.clone(),
                    error: Some(msg),
                }),
            );
        }
    }

    let updated = before_commit != after_commit;
    let message = if updated {
        format!(
            "Fetched {}. {}: {:?} -> {:?}",
            repo_label, track_branch, before_commit, after_commit
        )
    } else {
        format!(
            "{} already up to date with origin ({})",
            repo_label, track_branch
        )
    };
    tracing::info!("git-pull: {}", message);

    if updated {
        if let Some(ref after) = after_commit {
            notify::spawn_commit_update_notify(
                "git_pull",
                repo_label.clone(),
                track_branch.clone(),
                after.clone(),
                state.relay_server_id.clone(),
                state.node_fqdn.clone(),
            );
        }
    }

    (
        StatusCode::OK,
        Json(GitPullResponse {
            success: true,
            message,
            updated,
            repo: Some(repo_label),
            before_commit,
            after_commit,
            error: None,
        }),
    )
}

/// Serve ACME HTTP-01 challenge files from a configured directory
pub async fn serve_acme_challenge(base_dir: &str, subpath: &str) -> impl IntoResponse {
    let rel = subpath
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect::<Vec<_>>()
        .join("/");
    if rel.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = PathBuf::from(base_dir).join(rel);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (StatusCode::OK, [("Content-Type", "text/plain")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// When `.relay.yaml` omits `client.hooks.get.path` (or there is no config), OPTIONS still
/// advertises the universal default so the web client can fetch `hooks/client/get.jsx`.
pub(crate) fn ensure_default_client_get_hook(body: &mut serde_json::Value) {
    const DEFAULT_GET: &str = "hooks/client/get.jsx";
    let Some(root) = body.as_object_mut() else {
        return;
    };
    let client_entry = root
        .entry("client")
        .or_insert_with(|| serde_json::json!({}));
    if !client_entry.is_object() {
        *client_entry = serde_json::json!({});
    }
    let client = client_entry.as_object_mut().unwrap();
    let hooks_entry = client
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks_entry.is_object() {
        *hooks_entry = serde_json::json!({});
    }
    let hooks = hooks_entry.as_object_mut().unwrap();
    let get_entry = hooks.entry("get").or_insert_with(|| serde_json::json!({}));
    if !get_entry.is_object() {
        *get_entry = serde_json::json!({});
    }
    let get = get_entry.as_object_mut().unwrap();
    let missing_or_empty = get
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    if missing_or_empty {
        get.insert("path".to_string(), serde_json::json!(DEFAULT_GET));
    }
}

/// OPTIONS handler — discovery: capabilities, branches, repos, current selections, client hooks
pub async fn options_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    _query: Option<Query<HashMap<String, String>>>,
) -> impl IntoResponse {
    let repo_name =
        helpers::repo_from_host_or_default(&state, state.node_fqdn.as_deref(), &headers);
    let branch = helpers::branch_for_request(&headers, &state, repo_name.as_deref());

    let repo_names = git::repo_names_for_state(&state);
    let mut repos_json: Vec<serde_json::Value> = Vec::new();
    let mut relay_config: Option<RelayConfig> = None;

    for name in &repo_names {
        if let Some(repo) = git::open_repo_for_state(&state, name) {
            let mut heads_map = serde_json::Map::new();
            let branches = helpers::list_branches(&repo);
            for b in &branches {
                if let Ok(reference) = repo.find_reference(&format!("refs/heads/{}", b)) {
                    if let Ok(commit) = reference.peel_to_commit() {
                        heads_map.insert(b.clone(), serde_json::json!(commit.id().to_string()));
                    }
                }
            }
            if Some(name) == repo_name.as_ref() {
                if relay_config.is_none() {
                    relay_config = git::read_relay_config(&repo, &branch);
                }
            }
            repos_json.push(serde_json::json!({
                "name": name,
                "branches": serde_json::Value::Object(heads_map),
            }));
        }
    }

    let allow = "GET, PUT, DELETE, OPTIONS, QUERY";
    let mut body = serde_json::json!({
        "ok": true,
        "capabilities": {"supports": ["GET","PUT","DELETE","OPTIONS","QUERY"]},
        "repos": repos_json,
        "currentBranch": branch,
        "currentRepo": repo_name.clone().unwrap_or_default(),
    });

    if let Some(config) = relay_config {
        if let Ok(config_json) = serde_json::to_value(&config) {
            if let Some(obj) = body.as_object_mut() {
                if let Some(config_obj) = config_json.as_object() {
                    for (key, value) in config_obj {
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    ensure_default_client_get_hook(&mut body);

    (
        StatusCode::OK,
        [
            ("Allow", allow.to_string()),
            ("Content-Type", "application/json".to_string()),
            (HEADER_BRANCH, branch),
            (HEADER_REPO, repo_name.unwrap_or_default()),
        ],
        Json(body),
    )
}

pub async fn get_root(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Option<Query<HashMap<String, String>>>,
) -> impl IntoResponse {
    let qmap = query.as_ref().map(|q| &q.0);
    // Repo subdomain (`{repo}.node`): prefer site shell via `server.hooks.get` +
    // `siteShellRequest` before the gateway SPA `index.html`.
    if let Some(repo_name) = helpers::repo_from_host(&state, state.node_fqdn.as_deref(), &headers)
    {
        let branch = helpers::branch_for_request(&headers, &state, Some(&repo_name));
        if let Some(resp) =
            git::try_site_shell_for_root(&state, &headers, &branch, &repo_name, qmap)
        {
            return resp;
        }
    }
    // Try serving SPA index.html from configured static paths
    if let Some(resp) = crate::handlers::try_static(&state, "index.html").await {
        return resp;
    }
    // Apex host (no `{repo}.` prefix): serve **`RELAY_DEFAULT_REPO`** (default `relay-master`) index if present
    if let Some(repo_name) = helpers::default_http_repo_from_apex(&state) {
        let branch = helpers::branch_for_request(&headers, &state, Some(&repo_name));
        if let GitResolveResult::Respond(resp) = git::git_resolve_and_respond(
            &state,
            &headers,
            &branch,
            &repo_name,
            "index.html",
            None,
        ) {
            return resp;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /hooks/github/{repo} — Triggered by GitHub webhooks to pull updates
pub async fn post_github_hook(
    State(state): State<AppState>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let repo_name = repo_name.trim().trim_end_matches(".git");
    if repo_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing repo name").into_response();
    }

    let full_repo_path = state.repo_path.join(format!("{}.git", repo_name));
    if !full_repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            format!("Repository {}.git not found", repo_name),
        )
            .into_response();
    }

    // Identify if GitHub hooks are enabled in .relay.yaml
    let repo = match git2::Repository::open_bare(&full_repo_path) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open repo: {}", e),
            )
                .into_response()
        }
    };

    let relay_config = match git::read_relay_config(&repo, "main") {
        Some(c) => c,
        None => return (StatusCode::FORBIDDEN, "No .relay.yaml found").into_response(),
    };

    let gh_config = match relay_config.git.and_then(|g| g.github) {
        Some(gh) if gh.enabled => gh,
        _ => {
            return (
                StatusCode::FORBIDDEN,
                "GitHub hooks not enabled in .relay.yaml",
            )
                .into_response()
        }
    };

    tracing::info!(
        "GitHub hook received for repo: {}, path: {}",
        repo_name,
        gh_config.path
    );

    // Trigger git pull (fetch + merge/reset)
    // For bare repos, we just fetch
    let res = match repo.find_remote("origin") {
        Ok(mut remote) => {
            if let Err(e) = remote.fetch(&["main"], None, None) {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Fetch failed: {}", e),
                )
                    .into_response()
            } else {
                (StatusCode::OK, "GitHub hook processed, repository fetched").into_response()
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Remote 'origin' not found: {}", e),
        )
            .into_response(),
    };
    res
}

/// Create `RELAY_REPO_PATH/<repo>.git` when missing so `git push git://host:9418/<repo>.git` is accepted.
/// Relay does **not** implement git-protocol authentication; optional policy lives in per-repo `.relay.yaml` / hooks.
pub async fn post_ensure_bare(
    State(state): State<AppState>,
    AxPath(repo): AxPath<String>,
) -> impl IntoResponse {
    #[derive(Serialize)]
    struct EnsureBareResponse {
        ok: bool,
        created: bool,
        repo: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    if !state.git_ensure_bare_enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(EnsureBareResponse {
                ok: false,
                created: false,
                repo: repo.clone(),
                path: String::new(),
                error: Some("RELAY_GIT_ENSURE_BARE_ENABLED is disabled for this node".to_string()),
            }),
        )
            .into_response();
    }

    let slug = match crate::peer_hook::validate_repo_slug(&repo) {
        Ok(s) => s,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(EnsureBareResponse {
                    ok: false,
                    created: false,
                    repo,
                    path: String::new(),
                    error: Some(msg.to_string()),
                }),
            )
                .into_response();
        }
    };

    let root = state.repo_path.clone();
    let slug_for_task = slug.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<(bool, PathBuf), String> {
        let dest = root.join(format!("{}.git", slug_for_task));
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        if dest.exists() {
            let r = Repository::open(&dest).map_err(|e| e.to_string())?;
            if !r.is_bare() {
                return Err("path exists but is not a bare git repository".to_string());
            }
            git::apply_bare_receive_security_at_path(&dest)?;
            return Ok((false, dest));
        }
        Repository::init_bare(&dest).map_err(|e| e.to_string())?;
        git::apply_bare_receive_security_at_path(&dest)?;
        Ok((true, dest))
    })
    .await;

    let Ok(task_result) = join else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(EnsureBareResponse {
                ok: false,
                created: false,
                repo: slug,
                path: String::new(),
                error: Some("internal task failed".to_string()),
            }),
        )
            .into_response();
    };

    match task_result {
        Ok((created, dest)) => {
            let path_str = dest.display().to_string();
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (
                status,
                Json(EnsureBareResponse {
                    ok: true,
                    created,
                    repo: slug,
                    path: path_str,
                    error: None,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(EnsureBareResponse {
                ok: false,
                created: false,
                repo: slug,
                path: String::new(),
                error: Some(e),
            }),
        )
            .into_response(),
    }
}

/// `POST /api/index/warm/:repo` — JIT-warm SQLite index for a bare repo branch (default `main`).
pub async fn post_index_warm(
    State(state): State<AppState>,
    AxPath(repo): AxPath<String>,
) -> impl IntoResponse {
    #[derive(Serialize)]
    struct WarmResponse {
        ok: bool,
        repo: String,
        branch: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tip: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    let slug = match crate::peer_hook::validate_repo_slug(&repo) {
        Ok(s) => s,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WarmResponse {
                    ok: false,
                    repo,
                    branch: crate::types::DEFAULT_BRANCH.to_string(),
                    tip: None,
                    error: Some(msg.to_string()),
                }),
            )
                .into_response();
        }
    };

    let branch = crate::types::DEFAULT_BRANCH.to_string();
    let root = state.repo_path.clone();
    let slug_out = slug.clone();
    let branch_out = branch.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let bare = root.join(format!("{}.git", slug));
        if !bare.is_dir() {
            return Err(format!("bare repository {}.git not found", slug));
        }
        let repo = git2::Repository::open_bare(&bare).map_err(|e| e.to_string())?;
        let refname = format!("refs/heads/{}", branch);
        let tip = repo
            .refname_to_id(&refname)
            .map_err(|_| format!("branch {} not found", branch))?
            .to_string();
        let ctx = git::HookContext {
            repo_path: bare,
            old_commit: String::new(),
            new_commit: tip.clone(),
            refname: format!("refs/heads/{}", branch),
            branch: branch.clone(),
            is_verified: true,
            files: Default::default(),
        };
        if let Some(cfg) = git::read_relay_config(&repo, &tip) {
            if git::has_index_hook(&cfg) {
                let lock = git::branch_operation_mutex(ctx.repo_path.clone(), branch.clone());
                let _guard = lock.lock().unwrap();
                git::ensure_indexed(&ctx).map_err(|e| e.to_string())?;
            } else {
                return Err("repo has no index hook".into());
            }
        } else {
            return Err("no .relay.yaml at tip".into());
        }
        Ok(tip)
    })
    .await;

    match join {
        Ok(Ok(tip)) => (
            StatusCode::OK,
            Json(WarmResponse {
                ok: true,
                repo: slug_out.clone(),
                branch: branch_out.clone(),
                tip: Some(tip),
                error: None,
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::CONFLICT,
            Json(WarmResponse {
                ok: false,
                repo: slug_out.clone(),
                branch: branch_out.clone(),
                tip: None,
                error: Some(e),
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(WarmResponse {
                ok: false,
                repo: slug_out.clone(),
                branch: branch_out.clone(),
                tip: None,
                error: Some("internal task join failed".into()),
            }),
        )
            .into_response(),
    }
}

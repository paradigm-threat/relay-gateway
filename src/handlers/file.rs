use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tracing::{error, info, warn};

use crate::{
    git, helpers, transpiler, AppState, GitResolveResult, DEFAULT_IPFS_CACHE_ROOT, HEADER_BRANCH,
    HEADER_REPO,
};

fn is_relay_runtime_static_path(path: &str) -> bool {
    let rel = path.trim_start_matches('/');
    let reserved = rel.starts_with("assets/") || rel.starts_with("wasm/");
    reserved
        && std::path::Path::new(rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// GET file handler — resolves from Git first, then `server.hooks.get` CleverScript (`.cs`), then static
pub async fn handle_get_file(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
    query: Option<Query<HashMap<String, String>>>,
) -> Response {
    info!(%path, "get_file called");
    let decoded = helpers::url_decode(&path).decode_utf8_lossy().to_string();
    info!(decoded = %decoded, "decoded path");

    let repo_name_opt =
        helpers::repo_from_host_or_default(&state, state.node_fqdn.as_deref(), &headers);
    let branch = helpers::branch_for_request(&headers, &state, repo_name_opt.as_deref());

    // Dynamic tip sitemap / robots (prefer over stale tip blobs).
    if crate::git::sitemap::is_seo_meta_path(&decoded) {
        if let Some(ref repo_name) = repo_name_opt {
            if let Some(resp) = crate::git::sitemap::try_seo_meta_response(
                &state, &headers, &branch, repo_name, &decoded,
            ) {
                return resp;
            }
        }
    }

    // Framework `/assets` + `/wasm` only when the site imports `@relay/web-assets` in get.cs.
    // Opt-in is a cached source scan — never runs CleverScript for chunk/WASM GETs.
    if is_relay_runtime_static_path(&decoded) {
        if let Some(ref repo_name) = repo_name_opt {
            if crate::git::web_assets::repo_opts_into_web_assets(&state, repo_name, &branch) {
                if let Some(resp) = crate::git::web_assets::try_pinned_web_asset(&state, &decoded) {
                    return resp;
                }
                if let Some(resp) = try_static(&state, &decoded).await {
                    return resp;
                }
            }
        } else if let Some(resp) = try_static(&state, &decoded).await {
            // No Host→repo: disk static only (no catalog capture).
            return resp;
        }
    }

    let repo_name: String;
    if repo_name_opt.is_none() {
        if let Some(resp) = try_static(&state, &decoded).await {
            return resp;
        }
        if let Some(resp) = node_repo_door_hint(&state, &headers, &decoded) {
            return resp;
        }
        let hint = state
            .node_fqdn
            .as_deref()
            .map(|n| {
                format!(
                    "Use Host: {{repo}}.{} (bare repos under RELAY_REPO_PATH as <repo>.git). ",
                    n
                )
            })
            .unwrap_or_else(|| {
                "Set RELAY_PUBLIC_HOSTNAME to this node's FQDN, then use Host: {repo}.{that-fqdn}. "
                    .to_string()
            });
        let error_msg = format!(
            "Not Found\n\nPath: {}\nBranch: {}\nRepo: (none — {})\nStatic dirs searched: {:?}\n\nNo file found in static directories.",
            decoded, branch, hint, state.static_paths
        );
        return (
            StatusCode::NOT_FOUND,
            [
                ("Content-Type", "text/plain".to_string()),
                (HEADER_BRANCH, branch.clone()),
                (HEADER_REPO, "".to_string()),
            ],
            error_msg,
        )
            .into_response();
    } else {
        repo_name = repo_name_opt.unwrap();
    }
    let normalized_path = decoded.trim_start_matches('/').to_string();

    let qmap = query.as_ref().map(|q| &q.0);

    if transpiler::helpers::should_transpile_request(&headers, &query)
        && transpiler::helpers::is_transpilable_hook_path(&normalized_path)
    {
        if let Some(transpiled) = transpiler::helpers::transpile_hook_file(
            &state.repo_path,
            &branch,
            &repo_name,
            &normalized_path,
        ) {
            return transpiled;
        }
    }

    info!(%branch, "resolved branch");

    let git_result = match super::run_off_accept(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            git::git_resolve_and_respond(&state, &headers, &branch, &repo_name, &decoded, qmap)
        }))
    }) {
        Ok(r) => r,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "non-string panic payload".to_string()
            };
            error!(
                target: "relay_ssr",
                panic = %msg,
                path = %decoded,
                repo = %repo_name,
                "GET resolve panicked (returning 500)"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [("Content-Type", "text/html; charset=utf-8")],
                r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>Site shell error</title></head>
<body>
<h1>Site shell / SSR failed</h1>
<p>This request panicked while rendering the parked site shell. Check server logs for <code>relay_ssr</code>.</p>
</body></html>"#,
            )
                .into_response();
        }
    };
    match git_result {
        GitResolveResult::Respond(resp) => return resp,
        GitResolveResult::NotFound(rel_missing) => {
            let hook_resp =
                run_get_clever_or_404(&state, &headers, &branch, &repo_name, &rel_missing, qmap)
                    .await;
            if hook_resp.status() != StatusCode::NOT_FOUND {
                return hook_resp;
            }
            if let Some(resp) = try_static(&state, &decoded).await {
                return resp;
            }
            return hook_resp;
        }
    }
}

#[cfg(test)]
mod static_path_tests {
    use super::is_relay_runtime_static_path;

    #[test]
    fn reserves_hydration_assets_without_allowing_traversal() {
        // Path shape used for opt-in framework capture (see `@relay/web-assets`).
        assert!(is_relay_runtime_static_path("assets/embed.js"));
        assert!(is_relay_runtime_static_path(
            "assets/c/0123456789abcdef0123456789abcdef01234567/embed.js"
        ));
        assert!(is_relay_runtime_static_path(
            "/wasm/relay_runtime_pkg/relay_runtime_bg.wasm"
        ));
        assert!(!is_relay_runtime_static_path("README.md"));
        assert!(!is_relay_runtime_static_path("assets/../relay.env"));
    }

    #[test]
    fn repo_door_sentence_names_git_port_and_files_host() {
        let s = super::repo_door_sentence("iad1.relaygateway.net", 9418, "paradigm-threat-files");
        assert_eq!(
            s,
            "Clone git://iad1.relaygateway.net:9418/paradigm-threat-files.git and open files at https://paradigm-threat-files.iad1.relaygateway.net/.\n"
        );
        assert!(!s.contains('\n') || s.matches('\n').count() == 1);
        assert!(!s.to_ascii_lowercase().contains("index of"));
    }
}

async fn run_get_clever_or_404(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    branch: &str,
    repo_name: &str,
    rel_missing: &str,
    query: Option<&HashMap<String, String>>,
) -> Response {
    let repo = match git::open_repo(&state.repo_path, repo_name) {
        Some(r) => r,
        None => return (StatusCode::INTERNAL_SERVER_ERROR, "Repository not found").into_response(),
    };
    let refname = format!("refs/heads/{}", branch);
    let commit = match repo
        .find_reference(&refname)
        .and_then(|r| r.peel_to_commit())
    {
        Ok(c) => c,
        Err(_) => {
            let error_msg = format!(
                "Not Found\n\nPath: {}\nBranch: {} (not found in repo)\nRepo: {}\n\nBranch does not exist in repository.",
                rel_missing, branch, repo_name
            );
            return (StatusCode::NOT_FOUND, error_msg).into_response();
        }
    };
    let head = commit.id().to_string();
    let relay_cfg = git::read_relay_config(&repo, &head).unwrap_or_default();
    let cs_path = match git::index_policy::clever_get_hook_path(&relay_cfg) {
        Some(p) => p,
        None => {
            let error_msg = format!(
                "Not Found\n\nPath: {}\nBranch: {}\nRepo: {}\n\nNo `server.hooks.get` CleverScript (.cs) configured in .relay.yaml for dynamic GET.",
                rel_missing, branch, repo_name
            );
            return (StatusCode::NOT_FOUND, error_msg).into_response();
        }
    };
    let repo_full = state.repo_path.join(format!("{}.git", repo_name));
    let source_bytes = match git::read_file_from_repo(&repo_full, branch, &cs_path) {
        Ok(b) => b,
        Err(_) => {
            let error_msg = format!(
                "Not Found\n\nPath: {}\nBranch: {}\nRepo: {}\n\nConfigured GET hook missing in tree: {}",
                rel_missing, branch, repo_name, cs_path
            );
            return (StatusCode::NOT_FOUND, error_msg).into_response();
        }
    };
    let source = String::from_utf8_lossy(&source_bytes);
    let content_type = mime_guess::from_path(rel_missing)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    let method = headers
        .get("x-relay-http-method")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("GET");
    let mut ctx = serde_json::json!({
        "relPath": rel_missing,
        "branch": branch,
        "repo": repo_name,
        "commitSha": head,
        "contentType": content_type,
        "httpMethod": method,
        "cacheRoot": std::env::var("RELAY_IPFS_CACHE_ROOT").unwrap_or_else(|_| DEFAULT_IPFS_CACHE_ROOT.to_string()),
    });
    if let Some(q) = query {
        if let Ok(obj) = serde_json::to_value(q) {
            ctx["query"] = obj;
        }
    }
    crate::git::clever_script_host::augment_get_context_headers(&mut ctx, headers);
    let val = match super::run_off_accept(|| {
        git::clever_hooks::run_clever_hook(
            &source,
            &ctx,
            repo_full.clone(),
            branch.to_string(),
            true,
        )
    }) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("ssrClientHookFromGetContext disabled") {
                tracing::debug!(%msg, "GET CleverScript hook skipped (ssrCache)");
            } else {
                error!(?e, "GET CleverScript hook failed");
            }
            return git::get_hook_http::get_hook_execution_error_response(branch, repo_name, &e);
        }
    };
    if let Some(resp) = git::get_hook_json_to_response(&val, branch, repo_name) {
        return resp;
    }
    (
        StatusCode::NOT_FOUND,
        [
            ("Content-Type", "text/plain".to_string()),
            (HEADER_BRANCH, branch.to_string()),
            (HEADER_REPO, repo_name.to_string()),
        ],
        "Not Found",
    )
        .into_response()
}

/// One sentence for `GET /{slug}` on the node apex when `{slug}.git` exists.
/// Not a directory listing. Git stays on the git port.
pub(crate) fn repo_door_sentence(node: &str, port: u16, slug: &str) -> String {
    format!("Clone git://{node}:{port}/{slug}.git and open files at https://{slug}.{node}/.\n")
}

fn node_repo_door_hint(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    decoded: &str,
) -> Option<Response> {
    if !helpers::host_is_node_apex(state.node_fqdn.as_deref(), headers) {
        return None;
    }
    let slug = decoded.trim().trim_matches('/');
    if slug.is_empty() || slug.contains('/') || slug.contains('\\') || slug.contains("..") {
        return None;
    }
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    if slug.is_empty()
        || slug.contains('.')
        || !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let bare = state.repo_path.join(format!("{slug}.git"));
    if !bare.is_dir() {
        return None;
    }
    let node = state.node_fqdn.clone().unwrap_or_else(|| {
        headers
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.split(':').next())
            .unwrap_or("localhost")
            .to_string()
    });
    let body = repo_door_sentence(&node, crate::post_push_sync::git_port(), slug);
    Some(
        (
            StatusCode::OK,
            [("Content-Type", "text/plain; charset=utf-8")],
            body,
        )
            .into_response(),
    )
}

/// Try to serve a file from static paths
pub async fn try_static(state: &AppState, rel: &str) -> Option<Response> {
    for base in &state.static_paths {
        let candidate = base.join(rel.trim_start_matches('/'));
        if candidate.is_file() {
            match tokio::fs::read(&candidate).await {
                Ok(bytes) => {
                    let ct = mime_guess::from_path(&candidate)
                        .first_or_octet_stream()
                        .essence_str()
                        .to_string();
                    let mut resp =
                        (StatusCode::OK, [("Content-Type", ct.clone())], bytes).into_response();
                    let headers = resp.headers_mut();
                    headers.insert(
                        axum::http::header::CACHE_CONTROL,
                        axum::http::HeaderValue::from_static("public, max-age=3600"),
                    );
                    headers.insert(
                        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                        axum::http::HeaderValue::from_static("*"),
                    );
                    return Some(resp);
                }
                Err(e) => {
                    warn!(?e, path=%candidate.to_string_lossy(), "Failed to read static file");
                }
            }
        }
    }
    None
}

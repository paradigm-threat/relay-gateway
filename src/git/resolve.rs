use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use git2::ObjectType;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::error;

use crate::catalog_policy::{self, CatalogRepoRecord};
use crate::git::clever_hooks::run_clever_hook;
use crate::git::clever_script_host::augment_get_context_headers;
use crate::git::embed_html::merge_vary_accept;
use crate::git::get_hook_http::{get_hook_execution_error_response, get_hook_json_to_response};
use crate::git::index_policy;
use crate::git::ipfs::{
    is_asset_like_path, manifest_cid_for_path, parse_manifest_bytes, parse_pointer_bytes,
    resolve_ipfs_pointer, AssetsManifest, IpfsResolveOptions, MANIFEST_PATH,
};
use crate::git::open_repo;
use crate::git::repo::{open_repo_for_state, read_file_from_repo, read_relay_config};
use crate::git::worktree::{read_worktree_file, worktree_file_response};
use crate::types::{
    AppState, GitResolveResult, ServeContext, HEADER_BRANCH, HEADER_REPO, HEADER_TREE,
};
use git2::Repository;

fn load_assets_manifest(repo: &Repository, commit: &git2::Commit) -> Option<AssetsManifest> {
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(std::path::Path::new(MANIFEST_PATH)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    parse_manifest_bytes(blob.content()).ok()
}

fn catalog_for_repo_name(repo_name: &str) -> Option<CatalogRepoRecord> {
    let path = catalog_policy::catalog_path_from_env()?;
    let index = catalog_policy::try_load_catalog_index(&path)?;
    catalog_policy::catalog_record_for_slug(&index, repo_name).cloned()
}

/// Run `server.hooks.get` (optional on-tree blob body).
fn try_server_get_hook(
    repo: &Repository,
    branch: &str,
    repo_name: &str,
    rel: &str,
    headers: &HeaderMap,
    commit: &git2::Commit,
    body_bytes: Option<&[u8]>,
    content_type: &str,
    blob_oid: Option<&str>,
    qmap: &HashMap<String, String>,
    extra_ctx: serde_json::Value,
) -> Option<axum::response::Response> {
    let cfg = read_relay_config(&repo, &commit.id().to_string())?;
    let cs_path = index_policy::clever_get_hook_path(&cfg)?;
    let repo_path = repo.path().to_path_buf();
    let source_bytes = read_file_from_repo(&repo_path, branch, &cs_path).ok()?;
    let source = String::from_utf8_lossy(&source_bytes);
    let mut ctx = serde_json::json!({
        "relPath": rel,
        "repo": repo_name,
        "branch": branch,
        "contentType": content_type,
        "commitSha": commit.id().to_string(),
        "query": qmap.clone(),
    });
    if let Some(oid) = blob_oid {
        ctx["blobOid"] = serde_json::json!(oid);
    }
    if let Some(bytes) = body_bytes {
        ctx["bodyBase64"] = serde_json::json!(B64.encode(bytes));
    }
    if let serde_json::Value::Object(ref mut map) = ctx {
        if let serde_json::Value::Object(extra) = extra_ctx {
            for (k, v) in extra {
                map.insert(k, v);
            }
        }
    }
    augment_get_context_headers(&mut ctx, headers);
    let val = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_clever_hook(&source, &ctx, repo_path.clone(), branch.to_string(), true)
    })) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            error!(?e, "GET CleverScript hook failed");
            return Some(get_hook_execution_error_response(branch, repo_name, &e));
        }
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
                repo = %repo_name,
                path = %rel,
                "GET CleverScript hook panicked"
            );
            return Some(get_hook_execution_error_response(
                branch,
                repo_name,
                &anyhow::anyhow!("GET CleverScript hook panicked: {msg}"),
            ));
        }
    };
    get_hook_json_to_response(&val, branch, repo_name)
}

/// Run `server.hooks.get` for an on-tree blob (IPFS pointer, raster resize, markdown HTML, embed).
fn try_server_get_hook_for_blob(
    repo: &Repository,
    branch: &str,
    repo_name: &str,
    rel: &str,
    headers: &HeaderMap,
    commit: &git2::Commit,
    entry: &git2::TreeEntry,
    body_bytes: &[u8],
    content_type: &str,
    qmap: &HashMap<String, String>,
    extra_ctx: serde_json::Value,
) -> Option<axum::response::Response> {
    try_server_get_hook(
        repo,
        branch,
        repo_name,
        rel,
        headers,
        commit,
        Some(body_bytes),
        content_type,
        Some(&entry.id().to_string()),
        qmap,
        extra_ctx,
    )
}

fn path_is_ipfs_asset(
    repo_path: &PathBuf,
    branch: &str,
    rel: &str,
    body_bytes: Option<&[u8]>,
    manifest: Option<&AssetsManifest>,
) -> bool {
    if body_bytes
        .map(|b| parse_pointer_bytes(b).is_some())
        .unwrap_or(false)
    {
        return true;
    }
    if manifest
        .and_then(|m| manifest_cid_for_path(m, rel))
        .is_some()
    {
        return true;
    }
    let repo_path = repo_path.clone();
    let branch = branch.to_string();
    let rel_owned = rel.to_string();
    resolve_ipfs_pointer(
        &IpfsResolveOptions::default(),
        &|path| read_file_from_repo(&repo_path, &branch, path).ok(),
        &rel_owned,
        body_bytes,
    )
    .is_some()
}

fn is_markdown_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.contains("markdown") || ct == "text/md" || ct == "text/x-markdown"
}

fn is_markdown_blob(content_type: &str, uri_path: &str) -> bool {
    is_markdown_content_type(content_type) || uri_path.to_ascii_lowercase().ends_with(".md")
}

fn accept_prefers_html(headers: &HeaderMap) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    accept.contains("text/html") || accept.contains("application/xhtml+xml")
}

fn embed_query_flag(q: &HashMap<String, String>) -> bool {
    q.get("embed")
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

/// Media type for explicit git tree listings (`X-Relay-Tree: 1`).
pub const RELAY_TREE_MEDIA_TYPE: &str = "application/vnd.relay.tree+json";

fn header_flag_enabled(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let v = v.trim();
            if v.is_empty() {
                return true;
            }
            let v = v.to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

/// Explicit file-tree listing: `X-Relay-Tree: 1`.
///
/// A request header (never sent by normal browser navigation) so listing cannot collide
/// with blobs (`index.html`, `index.json`) or the repo-host site shell on `GET /`.
pub fn wants_relay_tree(headers: &HeaderMap) -> bool {
    header_flag_enabled(headers, HEADER_TREE)
}

fn tree_entry_type(kind: Option<ObjectType>) -> &'static str {
    match kind {
        Some(ObjectType::Blob) => "file",
        Some(ObjectType::Tree) => "dir",
        Some(ObjectType::Commit) => "commit",
        _ => "unknown",
    }
}

fn relay_tree_json(
    repo_name: &str,
    branch: &str,
    commit: &git2::Commit,
    path: &str,
    dir_tree: &git2::Tree,
) -> serde_json::Value {
    let mut entries = Vec::new();
    for item in dir_tree.iter() {
        let Some(name) = item.name() else {
            continue;
        };
        let child_path = if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}/{name}")
        };
        entries.push(serde_json::json!({
            "name": name,
            "type": tree_entry_type(item.kind()),
            "path": child_path,
            "mode": format!("{:06o}", item.filemode()),
            "oid": item.id().to_string(),
        }));
    }
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    serde_json::json!({
        "schema": "relay.tree.v1",
        "repo": repo_name,
        "branch": branch,
        "commit": commit.id().to_string(),
        "path": path,
        "entries": entries,
    })
}

fn relay_tree_response(
    repo_name: &str,
    branch: &str,
    commit: &git2::Commit,
    path: &str,
    dir_tree: &git2::Tree,
) -> axum::response::Response {
    let body = serde_json::to_string(&relay_tree_json(repo_name, branch, commit, path, dir_tree))
        .unwrap_or_else(|_| "{}".to_string());
    (
        StatusCode::OK,
        [
            (
                "Content-Type",
                format!("{RELAY_TREE_MEDIA_TYPE}; charset=utf-8"),
            ),
            (HEADER_BRANCH, branch.to_string()),
            (HEADER_REPO, repo_name.to_string()),
        ],
        body,
    )
        .into_response()
}

fn want_static_markdown_html(
    headers: &HeaderMap,
    q: &HashMap<String, String>,
    content_type: &str,
    uri_path: &str,
) -> bool {
    is_markdown_blob(content_type, uri_path) && accept_prefers_html(headers) && !embed_query_flag(q)
}

/// Virtual SPA routes that have no tip blob but should stay HTTP 200 (indexable chrome).
fn is_spa_virtual_path(rel: &str) -> bool {
    let p = rel.trim().trim_matches('/');
    p.is_empty() || p.eq_ignore_ascii_case("search")
}

/// Missing `DIR/page.md` when `DIR` is a git Tree → directory auto-index (HTTP 200 shell).
fn auto_index_dir_if_tree(tree: &git2::Tree, rel: &str) -> Option<String> {
    let p = rel.trim().trim_start_matches('/');
    let dir = p.strip_suffix("/page.md")?;
    if dir.is_empty() {
        return None;
    }
    let entry = tree.get_path(std::path::Path::new(dir)).ok()?;
    if entry.kind() == Some(ObjectType::Tree) {
        Some(dir.to_string())
    } else {
        None
    }
}

fn mark_missing_shell_noindex(mut resp: axum::response::Response) -> axum::response::Response {
    *resp.status_mut() = StatusCode::NOT_FOUND;
    if let Ok(hv) = HeaderValue::try_from("noindex, follow") {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-robots-tag"), hv);
    }
    resp
}

/// Site shell (embed SSR / CSR): markdown blob and either `?embed=1` or `Accept: text/html`.
fn want_relay_embed_from_request(
    headers: &HeaderMap,
    q: &HashMap<String, String>,
    content_type: &str,
    uri_path: &str,
) -> bool {
    is_markdown_blob(content_type, uri_path)
        && (embed_query_flag(q) || accept_prefers_html(headers))
}

fn response_content_type_is_html(resp: &axum::response::Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            let ct = ct.to_ascii_lowercase();
            ct.contains("text/html") || ct.contains("application/xhtml")
        })
        .unwrap_or(false)
}

/// Repo-host `/`: run `server.hooks.get` with `siteShellRequest` so sites can SSR the client
/// hook without a markdown path or `?embed=1`. Returns HTML only when the hook opts in.
pub fn try_site_shell_for_root(
    state: &AppState,
    headers: &HeaderMap,
    branch: &str,
    repo_name: &str,
    query: Option<&HashMap<String, String>>,
) -> Option<axum::response::Response> {
    let repo = open_repo_for_state(state, repo_name)?;
    let refname = format!("refs/heads/{branch}");
    let reference = repo.find_reference(&refname).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    let cfg = read_relay_config(&repo, &commit.id().to_string())?;
    let _ = index_policy::clever_get_hook_path(&cfg)?;
    let qmap: HashMap<String, String> = query.cloned().unwrap_or_default();
    let resp = try_server_get_hook(
        &repo,
        branch,
        repo_name,
        "",
        headers,
        &commit,
        None,
        "text/html",
        None,
        &qmap,
        serde_json::json!({ "siteShellRequest": true }),
    )?;
    if response_content_type_is_html(&resp) {
        Some(merge_vary_accept(resp))
    } else {
        None
    }
}

pub fn git_resolve_and_respond(
    state: &AppState,
    headers: &HeaderMap,
    branch: &str,
    repo_name: &str,
    decoded: &str,
    query: Option<&HashMap<String, String>>,
) -> GitResolveResult {
    let repo = match open_repo_for_state(state, repo_name) {
        Some(r) => r,
        None => {
            error!("open repo error: repo not found");
            return GitResolveResult::Respond(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let refname = format!("refs/heads/{}", branch);
    let reference = match repo.find_reference(&refname) {
        Ok(r) => r,
        Err(_) => {
            return GitResolveResult::NotFound(decoded.to_string());
        }
    };
    let commit = match reference.peel_to_commit() {
        Ok(c) => c,
        Err(e) => {
            error!(?e, "peel to commit error");
            return GitResolveResult::Respond(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let tree = match commit.tree() {
        Ok(t) => t,
        Err(e) => {
            error!(?e, "tree error");
            return GitResolveResult::Respond(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };

    let rel = decoded.trim_matches('/');
    let wants_tree = wants_relay_tree(headers);

    // Empty path: `X-Relay-Tree: 1` lists the commit root; otherwise fall through to the site shell.
    if rel.is_empty() {
        if wants_tree {
            return GitResolveResult::Respond(relay_tree_response(
                repo_name, branch, &commit, "", &tree,
            ));
        }
        return GitResolveResult::NotFound(rel.to_string());
    }

    if let ServeContext::LocalWorktree {
        checkout,
        checked_out_branch,
        ..
    } = &state.serve_context
    {
        if branch == checked_out_branch {
            if let Some(bytes) = read_worktree_file(checkout, rel) {
                return GitResolveResult::Respond(worktree_file_response(
                    branch, repo_name, rel, bytes,
                ));
            }
        }
    }

    let path_obj = std::path::Path::new(rel);
    let entry = match tree.get_path(path_obj) {
        Ok(e) => e,
        Err(_) => {
            let qmap: HashMap<String, String> = query.cloned().unwrap_or_default();
            let has_get_hook = read_relay_config(&repo, &commit.id().to_string())
                .and_then(|cfg| index_policy::clever_get_hook_path(&cfg))
                .is_some();
            let thin = catalog_for_repo_name(repo_name)
                .map(|c| c.thin_ipfs_assets())
                .unwrap_or(false);
            let asset_like = is_asset_like_path(rel);
            if has_get_hook || (thin && asset_like) {
                let ct = mime_guess::from_path(rel)
                    .first_or_octet_stream()
                    .essence_str()
                    .to_string();
                // Multi-page CS shells: missing non-asset paths with HTML Accept / ?embed=1
                // get the same siteShellRequest opt-in as repo-host `/` (relPath → initialPath).
                // Soft-404 (404+noindex) unless DIR/page.md is missing but DIR is a git Tree
                // (directory auto-index — HTTP 200 shell; tip fills Categories/Documents at P2).
                let wants_shell = has_get_hook
                    && !asset_like
                    && (accept_prefers_html(headers) || embed_query_flag(&qmap));
                let spa_virtual = is_spa_virtual_path(rel);
                let auto_index_dir = auto_index_dir_if_tree(&tree, rel);
                let missing_for_seo =
                    wants_shell && !spa_virtual && auto_index_dir.is_none();
                let extra = if wants_shell {
                    let mut map = serde_json::Map::new();
                    map.insert("siteShellRequest".into(), serde_json::json!(true));
                    map.insert(
                        "siteShellMissingPath".into(),
                        serde_json::json!(missing_for_seo),
                    );
                    if let Some(ref d) = auto_index_dir {
                        map.insert(
                            "siteShellAutoIndexDir".into(),
                            serde_json::json!(d),
                        );
                    }
                    serde_json::Value::Object(map)
                } else {
                    serde_json::json!({})
                };
                if let Some(resp) = try_server_get_hook(
                    &repo, branch, repo_name, rel, headers, &commit, None, &ct, None, &qmap, extra,
                ) {
                    if wants_shell {
                        if response_content_type_is_html(&resp) {
                            let resp = merge_vary_accept(resp);
                            let resp = if missing_for_seo {
                                mark_missing_shell_noindex(resp)
                            } else {
                                resp
                            };
                            return GitResolveResult::Respond(resp);
                        }
                    } else {
                        return GitResolveResult::Respond(resp);
                    }
                }
            }
            return GitResolveResult::NotFound(rel.to_string());
        }
    };

    match entry.kind() {
        Some(ObjectType::Blob) => match repo.find_blob(entry.id()) {
            Ok(blob) => {
                let ct = mime_guess::from_path(rel)
                    .first_or_octet_stream()
                    .essence_str()
                    .to_string();

                let qmap: HashMap<String, String> = query.cloned().unwrap_or_default();
                let manifest = load_assets_manifest(&repo, &commit);
                let body_bytes = blob.content().to_vec();

                let repo_path = repo.path().to_path_buf();
                let ipfs_asset = path_is_ipfs_asset(
                    &repo_path,
                    branch,
                    rel,
                    Some(&body_bytes),
                    manifest.as_ref(),
                );
                if ipfs_asset {
                    if let Some(resp) = try_server_get_hook_for_blob(
                        &repo,
                        branch,
                        repo_name,
                        rel,
                        headers,
                        &commit,
                        &entry,
                        &body_bytes,
                        &ct,
                        &qmap,
                        serde_json::json!({}),
                    ) {
                        return GitResolveResult::Respond(resp);
                    }
                }

                // App shell first: `?embed=1` or browser Accept:text/html on markdown.
                // Hook may SSR via `@relay/ssr`. Explicit `?embed=1` falls back to the empty
                // CSR embed shell; Accept-only falls through to static `@relay/md` HTML.
                if want_relay_embed_from_request(headers, &qmap, &ct, rel) {
                    let initial_path = format!("/{}", rel.trim_start_matches('/'));

                    let cfg_opt = read_relay_config(&repo, &commit.id().to_string());
                    let client_hook_http = cfg_opt
                        .as_ref()
                        .and_then(|c| c.client.as_ref())
                        .and_then(|cl| cl.hooks.get.as_ref())
                        .map(|h| {
                            let p = h.path.trim_start_matches('/');
                            format!("/{}", p)
                        })
                        .unwrap_or_else(|| "/hooks/client/get.jsx".to_string());

                    if let Some(resp) = try_server_get_hook_for_blob(
                        &repo,
                        branch,
                        repo_name,
                        rel,
                        headers,
                        &commit,
                        &entry,
                        &body_bytes,
                        &ct,
                        &qmap,
                        serde_json::json!({ "embedMarkdownRequest": true }),
                    ) {
                        // Hooks often `return next()` (raw blob). Only honor custom HTML
                        // from the server GET hook; otherwise fall through.
                        if response_content_type_is_html(&resp) {
                            return GitResolveResult::Respond(merge_vary_accept(resp));
                        }
                    }

                    if embed_query_flag(&qmap) {
                        let assets_prefix =
                            crate::git::web_assets::pinned_assets_url_prefix_if_opted_in(
                                state, repo_name, branch,
                            );
                        return GitResolveResult::Respond(
                            crate::git::embed_html::fallback_embed_html_response_with_ssr_and_assets(
                                repo_name,
                                branch,
                                &initial_path,
                                &client_hook_http,
                                None,
                                assets_prefix.as_deref(),
                            ),
                        );
                    }
                }

                if want_static_markdown_html(headers, &qmap, &ct, rel) {
                    if let Some(resp) = try_server_get_hook_for_blob(
                        &repo,
                        branch,
                        repo_name,
                        rel,
                        headers,
                        &commit,
                        &entry,
                        &body_bytes,
                        &ct,
                        &qmap,
                        serde_json::json!({ "markdownHtmlRequest": true }),
                    ) {
                        return GitResolveResult::Respond(merge_vary_accept(resp));
                    }
                }

                if read_relay_config(&repo, &commit.id().to_string())
                    .and_then(|cfg| index_policy::clever_get_hook_path(&cfg))
                    .is_some()
                {
                    if let Some(resp) = try_server_get_hook_for_blob(
                        &repo,
                        branch,
                        repo_name,
                        rel,
                        headers,
                        &commit,
                        &entry,
                        &body_bytes,
                        &ct,
                        &qmap,
                        serde_json::json!({}),
                    ) {
                        return GitResolveResult::Respond(resp);
                    }
                }

                let resp = (
                    StatusCode::OK,
                    [
                        ("Content-Type", ct),
                        (HEADER_BRANCH, branch.to_string()),
                        (HEADER_REPO, repo_name.to_string()),
                    ],
                    body_bytes,
                )
                    .into_response();
                GitResolveResult::Respond(resp)
            }
            Err(e) => {
                error!(?e, "blob read error");
                GitResolveResult::Respond(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            }
        },
        Some(ObjectType::Tree) => match repo.find_tree(entry.id()) {
            Ok(dir_tree) => {
                if wants_tree {
                    return GitResolveResult::Respond(relay_tree_response(
                        repo_name, branch, &commit, rel, &dir_tree,
                    ));
                }
                let mut entries = serde_json::json!({});
                for item in dir_tree.iter() {
                    if let Some(name) = item.name() {
                        let kind = match item.kind() {
                            Some(ObjectType::Blob) => "file",
                            Some(ObjectType::Tree) => "dir",
                            _ => "unknown",
                        };
                        entries[name] = serde_json::json!({
                            "type": kind,
                            "path": format!("{}/{}", rel, name)
                        });
                    }
                }
                let resp = (
                    StatusCode::OK,
                    [
                        ("Content-Type", "application/json".to_string()),
                        (HEADER_BRANCH, branch.to_string()),
                        (HEADER_REPO, repo_name.to_string()),
                    ],
                    serde_json::to_string(&entries).unwrap_or_else(|_| "{}".to_string()),
                )
                    .into_response();
                GitResolveResult::Respond(resp)
            }
            Err(e) => {
                error!(?e, "tree read error");
                GitResolveResult::Respond(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            }
        },
        _ => GitResolveResult::NotFound(rel.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GitResolveResult;
    use axum::http::{HeaderName, HeaderValue};
    use git2::{Repository, Signature};
    use image::{ImageBuffer, ImageFormat, Rgba};
    use serial_test::serial;
    use std::io::Cursor;
    use tempfile::tempdir;

    fn bare_fleet_state(root: PathBuf) -> AppState {
        AppState {
            repo_path: root,
            serve_context: ServeContext::BareFleet,
            static_paths: Vec::new(),
            node_fqdn: None,
            relay_server_id: None,
            authorized_repos: None,
            parked_domains: None,
            features_manifest: None,
            git_ensure_bare_enabled: false,
            web_assets_repo: None,
            web_assets_branch: "release".to_string(),
        }
    }

    fn wide_png_bytes() -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(220, 2, Rgba([40u8, 2u8, 99u8, 255u8]));
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut c = Cursor::new(Vec::new());
        dyn_img.write_to(&mut c, ImageFormat::Png).unwrap();
        c.into_inner()
    }

    fn bare_img_repo(with_get_cs: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo_path = root.join("img.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let png_oid = repo.blob(&wide_png_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("pixel.png", png_oid, 0o100644).unwrap();
        if with_get_cs {
            let yaml = r#"
server:
  hooks:
    get:
      path: hooks/server/resize.cs
"#;
            let yaml_oid = repo.blob(yaml.as_bytes()).unwrap();
            let get_cs = r#"import { queryWidth, optimizeRasterFromGetContext } from '@relay/image';
export default function run(ctx) {
  var maxW = queryWidth(ctx);
  if (maxW !== null) {
    var webp = optimizeRasterFromGetContext(ctx, null, { width: maxW });
    if (webp !== null) { return webp; }
  }
  return null;
}"#;
            let cs_oid = repo.blob(get_cs.as_bytes()).unwrap();
            let mut tb_srv = repo.treebuilder(None).unwrap();
            tb_srv.insert("resize.cs", cs_oid, 0o100644).unwrap();
            let srv_tid = tb_srv.write().unwrap();
            let mut tb_hooks = repo.treebuilder(None).unwrap();
            tb_hooks.insert("server", srv_tid, 0o040000).unwrap();
            let hooks_tid = tb_hooks.write().unwrap();
            tb.insert(".relay.yaml", yaml_oid, 0o100644).unwrap();
            tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        }
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, root)
    }

    #[tokio::test]
    #[serial]
    async fn raster_with_w_uses_get_hook_webp_when_configured() {
        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IMAGE_CACHE_ROOT", cache_dir.path());
        let (_dir, root) = bare_img_repo(true);
        let mut q = HashMap::new();
        q.insert("w".into(), "64".into());
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "img", "pixel.png", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond, got non-Respond variant");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("webp"),
            "expected image/webp content-type, got {ct:?}"
        );
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert!(bytes.len() > 16);
        assert_eq!(&bytes[0..4], b"RIFF");
    }

    #[tokio::test]
    async fn raster_with_w_without_get_hook_serves_raw_png() {
        let (_dir, root) = bare_img_repo(false);
        let mut q = HashMap::new();
        q.insert("w".into(), "64".into());
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "img", "pixel.png", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("png"), "resize requires GET hook, got {ct:?}");
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
    }

    #[tokio::test]
    async fn raster_without_w_serves_raw_png() {
        let (_dir, root) = bare_img_repo(false);
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "img", "pixel.png", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("png"), "expected png, got {ct:?}");
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
    }

    #[tokio::test]
    async fn raster_zero_width_skips_resize_branch() {
        let (_dir, root) = bare_img_repo(true);
        let mut q = HashMap::new();
        q.insert("w".into(), "0".into());
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "img", "pixel.png", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("png"),
            "zero width should skip resize, got {ct:?}"
        );
        assert_eq!(parts.status, StatusCode::OK);
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
    }

    fn bare_md_repo(with_get_cs: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo_path = root.join("doc.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let md = "# Hello\n";
        let blob_oid = repo.blob(md.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("readme.md", blob_oid, 0o100644).unwrap();
        if with_get_cs {
            let yaml = r#"
server:
  hooks:
    get:
      path: hooks/server/get.cs
"#;
            let yaml_oid = repo.blob(yaml.as_bytes()).unwrap();
            let get_cs = r#"import { markdownHtmlFromGetContext } from '@relay/md';
export default function run(ctx, next) {
  var html = markdownHtmlFromGetContext(ctx);
  return html !== null ? html : next();
}"#;
            let cs_oid = repo.blob(get_cs.as_bytes()).unwrap();
            let mut tb_srv = repo.treebuilder(None).unwrap();
            tb_srv.insert("get.cs", cs_oid, 0o100644).unwrap();
            let srv_tid = tb_srv.write().unwrap();
            let mut tb_hooks = repo.treebuilder(None).unwrap();
            tb_hooks.insert("server", srv_tid, 0o040000).unwrap();
            let hooks_tid = tb_hooks.write().unwrap();
            tb.insert(".relay.yaml", yaml_oid, 0o100644).unwrap();
            tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        }
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, root)
    }

    fn bare_ssr_opt_in_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo_path = root.join("doc.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();

        let md_oid = repo.blob(b"# Hello\n").unwrap();
        let yaml_oid = repo
            .blob(
                br#"
client:
  hooks:
    get:
      path: hooks/client/get.jsx
server:
  hooks:
    get:
      path: hooks/server/get.cs
"#,
            )
            .unwrap();
        let get_cs_oid = repo
            .blob(
                br#"import { ssrClientHookFromGetContext } from '@relay/ssr';
export default function run(ctx, next) {
  var shell = false;
  try { if (ctx.embedMarkdownRequest === true) shell = true; } catch (e) {}
  try { if (ctx.siteShellRequest === true) shell = true; } catch (e) {}
  if (shell) {
    var ssr = ssrClientHookFromGetContext(ctx);
    if (ssr !== null) { return ssr; }
  }
  return next();
}"#,
            )
            .unwrap();
        let client_get_oid = repo
            .blob(
                br#"export default function App() {
  return <main><h1>Opt-in SSR</h1><button onClick={() => {}}>Go</button></main>;
}"#,
            )
            .unwrap();

        let mut server = repo.treebuilder(None).unwrap();
        server.insert("get.cs", get_cs_oid, 0o100644).unwrap();
        let server_id = server.write().unwrap();
        let mut client = repo.treebuilder(None).unwrap();
        client.insert("get.jsx", client_get_oid, 0o100644).unwrap();
        let client_id = client.write().unwrap();
        let mut hooks = repo.treebuilder(None).unwrap();
        hooks.insert("server", server_id, 0o040000).unwrap();
        hooks.insert("client", client_id, 0o040000).unwrap();
        let hooks_id = hooks.write().unwrap();

        let mut root_tree = repo.treebuilder(None).unwrap();
        root_tree.insert("readme.md", md_oid, 0o100644).unwrap();
        root_tree.insert(".relay.yaml", yaml_oid, 0o100644).unwrap();
        root_tree.insert("hooks", hooks_id, 0o040000).unwrap();
        let tree_id = root_tree.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, root)
    }

    #[tokio::test]
    async fn markdown_accept_html_without_hook_serves_raw() {
        let (_dir, root) = bare_md_repo(false);
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let text = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(text.starts_with("# Hello"));
    }

    #[tokio::test]
    #[serial]
    async fn markdown_accept_html_uses_get_hook_static_html() {
        let (_dir, root) = bare_md_repo(true);
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"), "expected text/html, got {ct:?}");
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("<article"));
        assert!(html.contains("Hello"));
        assert!(!html.contains("/assets/embed.js"));
        let vary = parts
            .headers
            .get(header::VARY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            vary.to_ascii_lowercase().contains("accept"),
            "Vary should mention Accept, got {vary:?}"
        );
    }

    #[tokio::test]
    async fn markdown_embed_query_returns_embed_fallback() {
        let (_dir, root) = bare_md_repo(false);
        let mut q = HashMap::new();
        q.insert("embed".into(), "1".into());
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("/assets/embed.js"));
        assert!(html.contains("relay-embed-config"));
        let vary = parts
            .headers
            .get(header::VARY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            vary.to_ascii_lowercase().contains("accept"),
            "Vary should mention Accept, got {vary:?}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn markdown_embed_ignores_server_hook_next_and_serves_shell() {
        // Production pattern: get.cs returns markdownHtml OR next(). For embed,
        // markdownHtml is null and next() would be raw markdown — must not short-circuit SSR.
        let (_dir, root) = bare_md_repo(true);
        let mut q = HashMap::new();
        q.insert("embed".into(), "1".into());
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"), "expected HTML embed, got {ct:?}");
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("/assets/embed.js"), "{html}");
        assert!(html.contains("relay-embed-config"), "{html}");
        assert!(
            !html.contains("data-relay-ssr=\"1\""),
            "repo did not opt in to SSR: {html}"
        );
        assert!(
            !html.starts_with("# Hello"),
            "must not return raw markdown from next()"
        );
    }

    #[tokio::test]
    #[serial]
    async fn markdown_embed_server_hook_explicitly_opts_into_ssr() {
        let (_dir, root) = bare_ssr_opt_in_repo();
        let mut q = HashMap::new();
        q.insert("embed".into(), "1".into());
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        headers.insert(header::HOST, HeaderValue::from_static("doc.example.test"));
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("data-relay-ssr=\"1\""), "{html}");
        assert!(html.contains("Opt-in SSR"), "{html}");
        assert!(html.contains("data-relay-hid=\"1\""), "{html}");
        assert!(html.contains("/assets/embed.js"), "{html}");
    }

    #[tokio::test]
    #[serial]
    async fn markdown_accept_html_without_embed_opts_into_ssr() {
        let (_dir, root) = bare_ssr_opt_in_repo();
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        headers.insert(header::HOST, HeaderValue::from_static("doc.example.test"));
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("data-relay-ssr=\"1\""), "{html}");
        assert!(html.contains("Opt-in SSR"), "{html}");
        assert!(html.contains("/assets/embed.js"), "{html}");
    }

    #[tokio::test]
    #[serial]
    async fn site_shell_root_opts_into_ssr() {
        let (_dir, root) = bare_ssr_opt_in_repo();
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        headers.insert(header::HOST, HeaderValue::from_static("doc.example.test"));
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        let state = bare_fleet_state(root);
        let resp = try_site_shell_for_root(&state, &headers, "main", "doc", None)
            .expect("site shell HTML");
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("data-relay-ssr=\"1\""), "{html}");
        assert!(html.contains("Opt-in SSR"), "{html}");
        assert!(html.contains("/assets/embed.js"), "{html}");
    }

    #[tokio::test]
    #[serial]
    async fn missing_path_accept_html_opts_into_site_shell_ssr() {
        let (_dir, root) = bare_ssr_opt_in_repo();
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        headers.insert(header::HOST, HeaderValue::from_static("doc.example.test"));
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "search", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond for missing SPA path");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("data-relay-ssr=\"1\""), "{html}");
        assert!(html.contains("Opt-in SSR"), "{html}");
        assert!(html.contains("/assets/embed.js"), "{html}");
        let vary = parts
            .headers
            .get(header::VARY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            vary.to_ascii_lowercase().contains("accept"),
            "missing-path shell must Vary: Accept, got {vary:?}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn missing_non_spa_path_accept_html_is_404_noindex() {
        let (_dir, root) = bare_ssr_opt_in_repo();
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        headers.insert(header::HOST, HeaderValue::from_static("doc.example.test"));
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(
            &state,
            &headers,
            "main",
            "doc",
            "does-not-exist-path",
            None,
        );
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond for missing non-SPA path");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::NOT_FOUND);
        let robots = parts
            .headers
            .get("x-robots-tag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            robots.to_ascii_lowercase().contains("noindex"),
            "expected X-Robots-Tag noindex, got {robots:?}"
        );
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("data-relay-ssr=\"1\""), "{html}");
        assert!(
            html.contains(r#"name="robots" content="noindex,follow""#),
            "{html}"
        );
    }

    #[tokio::test]
    async fn markdown_without_html_accept_serves_raw() {
        let (_dir, root) = bare_md_repo(false);
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("markdown") || ct.contains("text/plain"),
            "expected markdown-ish content-type, got {ct:?}"
        );
        let text = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(text.starts_with("# Hello"));
    }

    #[tokio::test]
    async fn markdown_embed_query_returns_shell_without_accept() {
        let (_dir, root) = bare_md_repo(false);
        let mut q = HashMap::new();
        q.insert("embed".into(), "1".into());
        let headers = HeaderMap::new();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &headers, "main", "doc", "readme.md", Some(&q));
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (_parts, body) = resp.into_response().into_parts();
        let html = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("/assets/embed.js"));
    }

    fn bare_site_with_indexes() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo_path = root.join("site.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let index_html = repo.blob(b"<html>home</html>").unwrap();
        let index_json = repo.blob(br#"{"ok":true}"#).unwrap();
        let readme = repo.blob(b"# site\n").unwrap();
        let blog_index = repo.blob(b"<html>blog</html>").unwrap();
        let post = repo.blob(b"# post\n").unwrap();
        let mut tb_blog = repo.treebuilder(None).unwrap();
        tb_blog.insert("index.html", blog_index, 0o100644).unwrap();
        tb_blog.insert("post.md", post, 0o100644).unwrap();
        let blog_tid = tb_blog.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("index.html", index_html, 0o100644).unwrap();
        tb.insert("index.json", index_json, 0o100644).unwrap();
        tb.insert("README.md", readme, 0o100644).unwrap();
        tb.insert("blog", blog_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, root)
    }

    fn tree_header() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(HEADER_TREE, HeaderValue::from_static("1"));
        h
    }

    async fn resolve_json(
        state: &AppState,
        headers: &HeaderMap,
        path: &str,
    ) -> (StatusCode, String, serde_json::Value) {
        let r = git_resolve_and_respond(state, headers, "main", "site", path, None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond for path {path}");
        };
        let (parts, body) = resp.into_response().into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!(null));
        (parts.status, ct, json)
    }

    #[tokio::test]
    async fn relay_tree_header_lists_root_despite_index_files() {
        let (_dir, root) = bare_site_with_indexes();
        let state = bare_fleet_state(root);
        let (status, ct, json) = resolve_json(&state, &tree_header(), "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains(RELAY_TREE_MEDIA_TYPE),
            "expected tree media type, got {ct:?}"
        );
        assert_eq!(json["schema"], "relay.tree.v1");
        assert_eq!(json["path"], "");
        let names: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"index.html"));
        assert!(names.contains(&"index.json"));
        assert!(names.contains(&"blog"));
        let blog = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "blog")
            .unwrap();
        assert_eq!(blog["type"], "dir");
    }

    #[tokio::test]
    async fn empty_path_without_tree_header_is_not_found() {
        let (_dir, root) = bare_site_with_indexes();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &HeaderMap::new(), "main", "site", "", None);
        assert!(matches!(r, GitResolveResult::NotFound(_)));
    }

    #[tokio::test]
    async fn relay_tree_header_lists_dir_despite_index_html() {
        let (_dir, root) = bare_site_with_indexes();
        let state = bare_fleet_state(root);
        let (status, ct, json) = resolve_json(&state, &tree_header(), "blog").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains(RELAY_TREE_MEDIA_TYPE));
        assert_eq!(json["path"], "blog");
        let names: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"index.html"));
        assert!(names.contains(&"post.md"));
    }

    #[tokio::test]
    async fn relay_tree_header_does_not_steal_blob_get() {
        let (_dir, root) = bare_site_with_indexes();
        let state = bare_fleet_state(root);
        let r = git_resolve_and_respond(&state, &tree_header(), "main", "site", "index.json", None);
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected Respond");
        };
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            !ct.contains(RELAY_TREE_MEDIA_TYPE),
            "blob GET must not become a tree listing, got {ct:?}"
        );
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(bytes.as_ref(), br#"{"ok":true}"#);
    }

    #[test]
    fn auto_index_dir_if_tree_when_page_md_missing() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("hub.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let md_oid = repo.blob(b"# A\n").unwrap();
        let mut tb_ff = repo.treebuilder(None).unwrap();
        tb_ff.insert("a.md", md_oid, 0o100644).unwrap();
        let ff_tid = tb_ff.write().unwrap();
        let mut tb_ev = repo.treebuilder(None).unwrap();
        tb_ev.insert("false_flag", ff_tid, 0o040000).unwrap();
        let ev_tid = tb_ev.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("events", ev_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        assert_eq!(
            auto_index_dir_if_tree(&tree, "events/false_flag/page.md").as_deref(),
            Some("events/false_flag")
        );
        assert_eq!(
            auto_index_dir_if_tree(&tree, "events/false_flag/a.md"),
            None,
            "existing blob path is not an auto-index hub"
        );
        assert_eq!(
            auto_index_dir_if_tree(&tree, "missing/page.md"),
            None,
            "missing parent tree → no auto-index"
        );
        assert_eq!(auto_index_dir_if_tree(&tree, "page.md"), None);
    }
}

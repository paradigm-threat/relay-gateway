//! Pre-rendered SSR fragment cache (`ssrCache` catalog field).
//!
//! When enabled for a content repo, parked Host HTML is composed from a mesh-synced
//! bare (`relay-ssr-cache` by default): one `_shell.html` + per-path `*.body.html`.
//! Cache miss → CSR cheap shell. Never runs live `ssrClientHookFromGetContext`.

use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use std::path::PathBuf;

use crate::catalog_policy::{self, CatalogRepoRecord, SsrCacheSpec};
use crate::git::embed_html::merge_vary_accept;
use crate::git::repo::{open_repo_for_state, read_file_from_repo};
use crate::git::ssr_html_cache::SSR_HTML_CACHE_CONTROL;
use crate::types::AppState;

const BODY_MARKER: &str = "<!--RELAY_SSR_BODY-->";
const DEFAULT_CACHE_BRANCH: &str = "main";

/// True when catalog says this content repo uses the static fragment cache.
pub fn catalog_ssr_cache_enabled(repo_name: &str) -> bool {
    catalog_record(repo_name)
        .as_ref()
        .is_some_and(|r| r.ssr_cache_enabled())
}

fn catalog_record(repo_name: &str) -> Option<CatalogRepoRecord> {
    let path = catalog_policy::catalog_path_from_env()?;
    let index = catalog_policy::try_load_catalog_index(&path)?;
    catalog_policy::catalog_record_for_slug(&index, repo_name).cloned()
}

fn cache_branch() -> String {
    std::env::var("RELAY_SSR_CACHE_BRANCH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CACHE_BRANCH.to_string())
}

fn normalize_prefix(prefix: &str) -> String {
    let mut p = prefix.trim().trim_matches('/').to_string();
    if !p.is_empty() {
        p.push('/');
    }
    p
}

/// Map request path → body blob under the site prefix.
/// `/` → `index.body.html`; `/events/foo` → `events/foo.body.html`;
/// `events/foo.md` → `events/foo.body.html`.
pub fn body_rel_under_prefix(prefix: &str, request_path: &str) -> String {
    let prefix = normalize_prefix(prefix);
    let mut rel = request_path.trim().trim_matches('/').to_string();
    if let Some(stripped) = rel.strip_suffix(".md") {
        rel = stripped.to_string();
    }
    if rel.is_empty() || rel == "index" || rel == "index.html" {
        return format!("{prefix}index.body.html");
    }
    format!("{prefix}{rel}.body.html")
}

fn shell_rel(spec: &SsrCacheSpec) -> String {
    spec.shell_path
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_start_matches('/')
        .to_string()
}

fn read_cache_utf8(state: &AppState, cache_repo: &str, branch: &str, path: &str) -> Option<String> {
    let repo = open_repo_for_state(state, cache_repo)?;
    let repo_path = PathBuf::from(repo.path());
    let bytes = read_file_from_repo(&repo_path, branch, path).ok()?;
    String::from_utf8(bytes).ok()
}

fn compose_html(shell: &str, body: &str) -> String {
    if shell.contains(BODY_MARKER) {
        shell.replacen(BODY_MARKER, body, 1)
    } else if let Some(idx) = shell.to_ascii_lowercase().find("</main>") {
        let mut out = String::with_capacity(shell.len() + body.len());
        out.push_str(&shell[..idx]);
        out.push_str(body);
        out.push_str(&shell[idx..]);
        out
    } else if let Some(idx) = shell.to_ascii_lowercase().find("</body>") {
        let mut out = String::with_capacity(shell.len() + body.len());
        out.push_str(&shell[..idx]);
        out.push_str(body);
        out.push_str(&shell[idx..]);
        out
    } else {
        format!("{shell}\n{body}")
    }
}

/// Public CSR-miss response when compose could not run (invalid catalog / missing bare).
pub fn html_response_csr_miss(content_repo: &str, request_path: &str) -> axum::response::Response {
    html_response(
        StatusCode::OK,
        csr_miss_html(content_repo, request_path),
        content_repo,
        "relay-ssr-cache",
        false,
    )
}

fn html_response(
    status: StatusCode,
    html: String,
    content_repo: &str,
    cache_repo: &str,
    hit: bool,
) -> axum::response::Response {
    let mut resp = (
        status,
        [
            (
                header::CONTENT_TYPE,
                "text/html; charset=utf-8".to_string(),
            ),
            (header::CACHE_CONTROL, SSR_HTML_CACHE_CONTROL.to_string()),
            (header::VARY, "Accept".to_string()),
            (
                HeaderName::from_static("x-relay-repo"),
                content_repo.to_string(),
            ),
            (
                HeaderName::from_static("x-relay-ssr-cache"),
                if hit { "hit" } else { "miss" }.to_string(),
            ),
            (
                HeaderName::from_static("x-relay-ssr-cache-repo"),
                cache_repo.to_string(),
            ),
        ],
        html,
    )
        .into_response();
    if let Ok(hv) = HeaderValue::from_str(if hit { "hit" } else { "miss" }) {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-relay-ssr-cache"), hv);
    }
    merge_vary_accept(resp)
}

/// Standard CSR shell when a body fragment is missing (no live SSR).
/// Same embed bootstrap the live SSR path leaves for hydrate — paints instantly;
/// client hook updates from local state after `embed.js` mounts.
pub fn csr_miss_html(content_repo: &str, request_path: &str) -> String {
    let path = if request_path.trim().is_empty() {
        "/".to_string()
    } else if request_path.starts_with('/') {
        request_path.to_string()
    } else {
        format!("/{request_path}")
    };
    let branch = catalog_record(content_repo)
        .map(|r| r.publish_branch)
        .unwrap_or_else(|| "main".to_string());
    let hook_rel = catalog_policy::resolve_client_hook_rel(content_repo, None);
    let hook_http = format!("/{hook_rel}");
    let mut html = crate::git::embed_html::embed_html_document_with_assets_and_seo_extras(
        content_repo,
        &branch,
        &path,
        &hook_http,
        None,
        None,
        None,
        None,
        None,
        None,
        crate::git::embed_html::EmbedSeoExtras {
            noindex: true,
            csr_placeholder: false,
            omit_embed_js: false,
            ..Default::default()
        },
    );
    // Marker for ops / tests: compose miss, not a baked shell+body hit.
    if let Some(idx) = html.find("<body>") {
        html.insert_str(
            idx + "<body>".len(),
            &format!(
                "\n  <!-- relay-ssr-cache:miss repo={} path={} -->",
                content_repo, path
            ),
        );
    }
    html
}

/// Compose shell+body for a parked content repo path, or CSR miss.
/// Returns `None` when `ssrCache` is not enabled / invalid for this repo.
pub fn try_compose_ssr_cache(
    state: &AppState,
    content_repo: &str,
    request_path: &str,
    _headers: &HeaderMap,
) -> Option<axum::response::Response> {
    let rec = catalog_record(content_repo)?;
    let spec = rec.ssr_cache_spec()?.clone();
    let cache_repo = rec.ssr_cache_repo_slug()?;
    let prefix = spec.prefix.as_deref()?.trim();
    let shell_path = shell_rel(&spec);
    if shell_path.is_empty() {
        return None;
    }
    let branch = cache_branch();
    let shell = match read_cache_utf8(state, &cache_repo, &branch, &shell_path) {
        Some(s) => s,
        None => {
            tracing::warn!(
                target: "relay_ssr_cache",
                repo = %content_repo,
                cache = %cache_repo,
                shell = %shell_path,
                "ssrCache shell missing; CSR miss"
            );
            return Some(html_response(
                StatusCode::OK,
                csr_miss_html(content_repo, request_path),
                content_repo,
                &cache_repo,
                false,
            ));
        }
    };
    let body_path = body_rel_under_prefix(prefix, request_path);
    match read_cache_utf8(state, &cache_repo, &branch, &body_path) {
        Some(body) => {
            // Normalize park-native host scripts on every hit so baked shells do not
            // keep retired hosts (LobbyAudio / Affirmation) after gateway upgrades.
            let html = crate::git::embed_html::inject_park_native_host_scripts(
                &compose_html(&shell, &body),
            );
            Some(html_response(
                StatusCode::OK,
                html,
                content_repo,
                &cache_repo,
                true,
            ))
        }
        None => {
            tracing::debug!(
                target: "relay_ssr_cache",
                repo = %content_repo,
                body = %body_path,
                "ssrCache body miss; CSR"
            );
            Some(html_response(
                StatusCode::OK,
                csr_miss_html(content_repo, request_path),
                content_repo,
                &cache_repo,
                false,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_rel_root_and_pages() {
        assert_eq!(
            body_rel_under_prefix("sites/paradigmthreat.net/", "/"),
            "sites/paradigmthreat.net/index.body.html"
        );
        assert_eq!(
            body_rel_under_prefix("sites/paradigmthreat.net/", "events/foo"),
            "sites/paradigmthreat.net/events/foo.body.html"
        );
        assert_eq!(
            body_rel_under_prefix("sites/paradigmthreat.net/", "events/foo.md"),
            "sites/paradigmthreat.net/events/foo.body.html"
        );
    }

    #[test]
    fn compose_inserts_marker() {
        let shell = format!("<html><body>{BODY_MARKER}</body></html>");
        let out = compose_html(&shell, "<p>hi</p>");
        assert!(out.contains("<p>hi</p>"));
        assert!(!out.contains(BODY_MARKER));
    }

    #[test]
    fn csr_miss_shell_bootstraps_client_hook() {
        let html = csr_miss_html("demo-park", "/articles/x");
        assert!(
            html.contains("relay-embed-config"),
            "missing embed config: {html}"
        );
        assert!(
            html.contains("embed.js"),
            "missing embed.js bootstrap: {html}"
        );
        assert!(
            html.contains("/hooks/client/get.jsx")
                || html.contains("\"clientHookPath\":\"/hooks/client/get.jsx\""),
            "missing client hook path: {html}"
        );
        assert!(
            html.contains("relay-ssr-cache:miss"),
            "missing miss marker: {html}"
        );
        assert!(html.contains("id=\"root\""), "missing #root: {html}");
    }
}

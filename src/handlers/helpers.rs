use std::path::PathBuf;

use axum::http::HeaderMap;
use git2::Repository;
use percent_encoding::percent_decode_str;

fn normalize_fqdn(s: &str) -> String {
    s.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Extract repo slug from `Host` when it matches `{repo}.{node_fqdn}`.
/// `repo` must be a single DNS label (no dots). Returns `None` if `host == node_fqdn` (no repo prefix).
pub fn repo_slug_from_host(
    host_header: &str,
    node_fqdn: &str,
    bare_names: &[String],
) -> Option<String> {
    let host = normalize_fqdn(host_header.split(':').next().unwrap_or(host_header));
    let node = normalize_fqdn(node_fqdn);
    if node.is_empty() {
        return None;
    }
    let suffix = format!(".{}", node);
    if host == node {
        return None;
    }
    if !host.ends_with(&suffix) {
        return None;
    }
    let slug = &host[..host.len() - suffix.len()];
    if slug.is_empty() || slug.contains('.') {
        return None;
    }
    if bare_names.iter().any(|n| n == slug) {
        Some(slug.to_string())
    } else {
        None
    }
}

/// Resolve bare repo name from request `Host` and configured node FQDN.
pub fn repo_from_host(
    state: &crate::types::AppState,
    node_fqdn: Option<&str>,
    headers: &HeaderMap,
) -> Option<String> {
    let names = crate::git::repo_names_for_state(state);
    if names.is_empty() {
        return None;
    }
    let node = node_fqdn?.trim();
    if node.is_empty() {
        return None;
    }
    let h = headers.get("host")?.to_str().ok()?;
    repo_slug_from_host(h, node, &names)
}

/// Bare name from **`RELAY_DEFAULT_REPO`** (default **`relay-master`**) if that repo exists.
/// Documented as the apex site when **`Host`** is the node FQDN only (no `{repo}.` prefix).
fn default_repo_name_for_apex() -> String {
    std::env::var("RELAY_DEFAULT_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "relay-master".to_string())
}

/// When **`Host`** is the node apex (no repo subdomain), serve this bare if present.
pub fn default_http_repo_from_apex(state: &crate::types::AppState) -> Option<String> {
    let name = default_repo_name_for_apex();
    let names = crate::git::repo_names_for_state(state);
    names.iter().find(|n| *n == &name).map(|_| name)
}

/// [`repo_from_host`], else apex default (**`RELAY_DEFAULT_REPO`** / **`relay-master`**) when that bare exists.
pub fn repo_from_host_or_default(
    state: &crate::types::AppState,
    node_fqdn: Option<&str>,
    headers: &HeaderMap,
) -> Option<String> {
    repo_from_host(state, node_fqdn, headers).or_else(|| default_http_repo_from_apex(state))
}

/// Resolve the branch name from `X-Relay-Branch` header only, defaults to **main**.
/// For HTTP handlers that know the repo, prefer [`branch_for_request`] so the bare repo's `HEAD` is used.
pub fn branch_from(headers: &HeaderMap) -> String {
    if let Some(h) = headers
        .get(crate::types::HEADER_BRANCH)
        .and_then(|v| v.to_str().ok())
    {
        if !h.is_empty() {
            return h.to_string();
        }
    }
    crate::types::DEFAULT_BRANCH.to_string()
}

/// Branch for a request: optional `X-Relay-Branch`, else repo default branch, else **`main`**.
pub fn branch_for_request(
    headers: &HeaderMap,
    state: &crate::types::AppState,
    repo_name: Option<&str>,
) -> String {
    if let Some(h) = headers
        .get(crate::types::HEADER_BRANCH)
        .and_then(|v| v.to_str().ok())
    {
        if !h.is_empty() {
            return h.to_string();
        }
    }
    if let Some(name) = repo_name {
        if let Some(b) = default_branch_for_repo(state, name) {
            return b;
        }
    }
    crate::types::DEFAULT_BRANCH.to_string()
}

fn default_branch_for_repo(state: &crate::types::AppState, name: &str) -> Option<String> {
    if let crate::types::ServeContext::LocalWorktree {
        checked_out_branch,
        repo_name,
        ..
    } = &state.serve_context
    {
        if repo_name == name {
            return Some(checked_out_branch.clone());
        }
    }
    crate::git::default_branch_from_bare_head(&state.repo_path, name)
}

/// Minimal URL percent-decoder wrapper used by handlers.
/// Returns a percent-decoder so callers can choose utf8 lossless decoding.
pub fn url_decode(input: &str) -> percent_encoding::PercentDecode<'_> {
    percent_decode_str(input)
}

/// List branch names from a repository
pub fn list_branches(repo: &Repository) -> Vec<String> {
    let mut out = vec![];
    if let Ok(mut iter) = repo.branches(None) {
        while let Some(Ok((b, _))) = iter.next() {
            if let Ok(name) = b.name() {
                if let Some(s) = name {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}

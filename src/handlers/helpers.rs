use axum::http::HeaderMap;
use git2::Repository;
use percent_encoding::percent_decode_str;

fn normalize_fqdn(s: &str) -> String {
    s.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Hostname from `Host` without port (`example:8080`, `[::1]:8080`).
pub fn host_without_port(host_header: &str) -> String {
    let h = host_header.trim();
    if let Some(rest) = h.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return normalize_fqdn(&rest[..end]);
        }
    }
    if let Some((host, port)) = h.rsplit_once(':') {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            return normalize_fqdn(host);
        }
    }
    normalize_fqdn(h)
}

fn host_is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// True when this `Host` may call node operator `/api/*` (apex FQDN or loopback).
pub fn host_is_node_ops_api(node_fqdn: Option<&str>, headers: &HeaderMap) -> bool {
    match headers.get("host").and_then(|v| v.to_str().ok()) {
        Some(h) => {
            let host = host_without_port(h);
            if host_is_loopback(&host) {
                return true;
            }
            let node = match node_fqdn.map(normalize_fqdn).filter(|s| !s.is_empty()) {
                Some(n) => n,
                None => return true,
            };
            host == node
        }
        None => true,
    }
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

/// Resolve a parked domain (`Host` matches a catalog `domains[]` entry) to its repo slug.
pub fn repo_from_parked_domain(
    state: &crate::types::AppState,
    headers: &HeaderMap,
) -> Option<String> {
    let map = state.parked_domains.as_ref()?;
    let host = headers.get("host")?.to_str().ok()?;
    crate::catalog_policy::repo_for_parked_host(map, host)
}

/// True when the request `Host` is the node's own apex FQDN (or when either is absent).
pub fn host_is_node_apex(node_fqdn: Option<&str>, headers: &HeaderMap) -> bool {
    let node = match node_fqdn.map(normalize_fqdn).filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => return true,
    };
    match headers.get("host").and_then(|v| v.to_str().ok()) {
        Some(h) => {
            let host = normalize_fqdn(h.split(':').next().unwrap_or(h));
            host == node
        }
        None => true,
    }
}

/// Repo for a request `Host`: `{repo}.{node}` subdomain, else a parked domain, else the apex
/// default (**`RELAY_DEFAULT_REPO`** / **`relay-master`**).
///
/// When parked domains are configured, an unknown `Host` (neither a known subdomain, parked
/// domain, nor the node apex) resolves to `None` instead of leaking the default apex repo.
pub fn repo_from_host_or_default(
    state: &crate::types::AppState,
    node_fqdn: Option<&str>,
    headers: &HeaderMap,
) -> Option<String> {
    if let Some(repo) = repo_from_host(state, node_fqdn, headers) {
        return Some(repo);
    }
    if let Some(repo) = repo_from_parked_domain(state, headers) {
        return Some(repo);
    }
    let has_parked = state
        .parked_domains
        .as_ref()
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if has_parked && !host_is_node_apex(node_fqdn, headers) {
        return None;
    }
    default_http_repo_from_apex(state)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn host(h: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert("host", HeaderValue::from_str(h).unwrap());
        m
    }

    #[test]
    fn host_without_port_strips_ipv4_and_bracket_v6() {
        assert_eq!(
            host_without_port("dfw1.relaygateway.net:8080"),
            "dfw1.relaygateway.net"
        );
        assert_eq!(host_without_port("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(host_without_port("[::1]:8080"), "::1");
        assert_eq!(host_without_port("localhost"), "localhost");
    }

    #[test]
    fn node_ops_api_allows_apex_and_loopback() {
        let node = Some("dfw1.relaygateway.net");
        assert!(host_is_node_ops_api(node, &host("dfw1.relaygateway.net")));
        assert!(host_is_node_ops_api(
            node,
            &host("dfw1.relaygateway.net:8080")
        ));
        assert!(host_is_node_ops_api(node, &host("127.0.0.1:8080")));
        assert!(host_is_node_ops_api(node, &host("localhost")));
        assert!(host_is_node_ops_api(node, &host("[::1]:8080")));
        assert!(host_is_node_ops_api(None, &host("anything.example")));
    }

    #[test]
    fn node_ops_api_rejects_parked_and_repo_subdomain() {
        let node = Some("dfw1.relaygateway.net");
        assert!(!host_is_node_ops_api(node, &host("clevertree.net")));
        assert!(!host_is_node_ops_api(
            node,
            &host("clevertree-site.dfw1.relaygateway.net")
        ));
    }
}

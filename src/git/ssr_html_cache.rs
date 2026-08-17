//! In-process LRU for site-shell SSR HTML documents (Phase D).
//!
//! Key: `(content bare path, branch, tip commit OID, initial path, client hook path, web-assets OID)`.
//! Headers on hit/miss: `public, s-maxage=60, stale-while-revalidate=3600`.
//! Disable: `RELAY_DISABLE_SSR_HTML_CACHE=1`.

use git2::Repository;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const MAX_ENTRIES: usize = 256;

/// Cache-Control for public park SSR HTML (CDN glue on a commit-keyed body).
pub const SSR_HTML_CACHE_CONTROL: &str = "public, s-maxage=60, stale-while-revalidate=3600";

#[derive(Clone, Hash, PartialEq, Eq)]
struct SsrHtmlKey {
    repo: String,
    branch: String,
    tip_commit: String,
    initial_path: String,
    client_hook: String,
    web_assets_oid: String,
}

struct CacheState {
    entries: HashMap<SsrHtmlKey, String>,
    order: Vec<SsrHtmlKey>,
}

fn cache_enabled() -> bool {
    std::env::var("RELAY_DISABLE_SSR_HTML_CACHE")
        .ok()
        .as_deref()
        != Some("1")
}

fn state() -> &'static Mutex<CacheState> {
    static STATE: OnceLock<Mutex<CacheState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(CacheState {
            entries: HashMap::new(),
            order: Vec::new(),
        })
    })
}

fn tip_commit_oid(repo_path: &Path, branch: &str) -> Option<String> {
    let repo = Repository::open(repo_path)
        .or_else(|_| Repository::open_bare(repo_path))
        .ok()?;
    if let Ok(reference) = repo.find_reference(&format!("refs/heads/{}", branch)) {
        if let Ok(commit) = reference.peel_to_commit() {
            return Some(commit.id().to_string());
        }
    }
    // Detached / default-branch mismatch in tests — fall back to HEAD.
    let head = repo.head().ok()?;
    let commit = head.peel_to_commit().ok()?;
    Some(commit.id().to_string())
}

/// Web-assets tip OID from the parent of a content bare (empty string when unset).
fn web_assets_oid_from_content_bare(content_bare: &Path) -> String {
    let Some(root) = content_bare.parent() else {
        return String::new();
    };
    let slug = std::env::var("RELAY_WEB_ASSETS_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "relay-web-assets".to_string());
    let branch = std::env::var("RELAY_WEB_ASSETS_BRANCH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "release".to_string());
    crate::git::web_assets::tip_commit_oid(root, &slug, &branch).unwrap_or_default()
}

fn make_key(
    repo_path: &Path,
    branch: &str,
    initial_path: &str,
    client_hook: &str,
) -> Option<SsrHtmlKey> {
    let tip = tip_commit_oid(repo_path, branch)?;
    Some(SsrHtmlKey {
        repo: repo_path.to_string_lossy().into_owned(),
        branch: branch.to_string(),
        tip_commit: tip,
        initial_path: initial_path.to_string(),
        client_hook: client_hook.trim_start_matches('/').to_string(),
        web_assets_oid: web_assets_oid_from_content_bare(repo_path),
    })
}

/// Look up a previously rendered SSR HTML document.
pub fn get(
    repo_path: &Path,
    branch: &str,
    initial_path: &str,
    client_hook: &str,
) -> Option<String> {
    if !cache_enabled() {
        return None;
    }
    let key = make_key(repo_path, branch, initial_path, client_hook)?;
    let Ok(guard) = state().lock() else {
        return None;
    };
    guard.entries.get(&key).cloned()
}

/// Store a rendered SSR HTML document (FIFO eviction when full).
pub fn put(repo_path: &Path, branch: &str, initial_path: &str, client_hook: &str, html: String) {
    if !cache_enabled() {
        return;
    }
    let Some(key) = make_key(repo_path, branch, initial_path, client_hook) else {
        return;
    };
    let Ok(mut guard) = state().lock() else {
        return;
    };
    if guard.entries.contains_key(&key) {
        guard.entries.insert(key, html);
        return;
    }
    while guard.order.len() >= MAX_ENTRIES {
        if let Some(old) = guard.order.first().cloned() {
            guard.order.remove(0);
            guard.entries.remove(&old);
        } else {
            break;
        }
    }
    guard.order.push(key.clone());
    guard.entries.insert(key, html);
}

/// Test helper: drop all entries.
#[cfg(test)]
pub fn clear_for_tests() {
    if let Ok(mut guard) = state().lock() {
        guard.entries.clear();
        guard.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    fn init_repo_with_branch(dir: &Path, branch: &str) -> String {
        let repo = Repository::init(dir).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            let _ = cfg.set_str("user.name", "test");
            let _ = cfg.set_str("user.email", "test@example.com");
        }
        std::fs::write(dir.join("README.md"), "# hi\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let oid = index.write_tree().unwrap();
        let tree = repo.find_tree(oid).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();
        let commit = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let commit_obj = repo.find_commit(commit).unwrap();
        repo.branch(branch, &commit_obj, true).unwrap();
        let mut checkout = repo
            .find_reference(&format!("refs/heads/{}", branch))
            .unwrap();
        repo.set_head(checkout.name().unwrap()).unwrap();
        let _ = checkout;
        commit.to_string()
    }

    #[test]
    fn roundtrip_put_get() {
        clear_for_tests();
        let dir = tempdir().unwrap();
        let oid = init_repo_with_branch(dir.path(), "main");
        assert!(!oid.is_empty());
        put(
            dir.path(),
            "main",
            "/",
            "hooks/client/get.jsx",
            "<html/>".into(),
        );
        let hit = get(dir.path(), "main", "/", "hooks/client/get.jsx");
        assert_eq!(hit.as_deref(), Some("<html/>"));
        let miss = get(dir.path(), "main", "/other.md", "hooks/client/get.jsx");
        assert!(miss.is_none());
    }
}

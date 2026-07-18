//! Commit-pinned web/CDN assets from a dedicated catalog bare repo (`relay-web-assets`).
//!
//! See `docs/RELAY_WEB_ASSETS.md`. URLs:
//! - `/assets/c/{40hex}/{rel}` → blob `assets/{rel}` at that commit
//! - `/wasm/c/{40hex}/{rel}` → blob `wasm/{rel}` at that commit

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use git2::Repository;
use std::path::{Path, PathBuf};

use crate::types::AppState;

const OID_LEN: usize = 40;

/// Parse `/assets/c/{oid}/…` or `/wasm/c/{oid}/…` into (kind, oid, tree-relative path).
pub fn parse_pinned_framework_path(path: &str) -> Option<PinnedFrameworkPath<'_>> {
    let rel = path.trim_start_matches('/');
    let (kind, rest) = if let Some(r) = rel.strip_prefix("assets/c/") {
        ("assets", r)
    } else if let Some(r) = rel.strip_prefix("wasm/c/") {
        ("wasm", r)
    } else {
        return None;
    };
    let (oid, file) = rest.split_once('/')?;
    if oid.len() != OID_LEN || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if file.is_empty()
        || Path::new(file)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(PinnedFrameworkPath {
        kind,
        commit_oid: oid,
        file_rel: file,
    })
}

pub struct PinnedFrameworkPath<'a> {
    pub kind: &'a str,
    pub commit_oid: &'a str,
    pub file_rel: &'a str,
}

impl PinnedFrameworkPath<'_> {
    pub fn tree_path(&self) -> String {
        format!("{}/{}", self.kind, self.file_rel)
    }
}

fn env_assets_repo() -> Option<String> {
    std::env::var("RELAY_WEB_ASSETS_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_assets_branch() -> String {
    std::env::var("RELAY_WEB_ASSETS_BRANCH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "release".to_string())
}

/// Bare-repos root + configured slug (from AppState or env).
pub fn assets_repo_config(state: &AppState) -> Option<(PathBuf, String, String)> {
    let slug = state
        .web_assets_repo
        .clone()
        .or_else(env_assets_repo)?;
    let branch = if state.web_assets_branch.is_empty() {
        env_assets_branch()
    } else {
        state.web_assets_branch.clone()
    };
    Some((state.repo_path.clone(), slug, branch))
}

fn bare_path(repos_root: &Path, slug: &str) -> PathBuf {
    repos_root.join(format!("{}.git", slug))
}

/// Tip commit OID for the web-assets publish branch, if the bare exists.
pub fn tip_commit_oid(repos_root: &Path, slug: &str, branch: &str) -> Option<String> {
    let path = bare_path(repos_root, slug);
    let repo = Repository::open_bare(&path).ok()?;
    let reference = repo.find_reference(&format!("refs/heads/{}", branch)).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    Some(commit.id().to_string())
}

/// `/assets/c/{oid}` prefix for HTML stamping, or `None` to use disk `/assets`.
pub fn pinned_assets_url_prefix(state: &AppState) -> Option<String> {
    let (root, slug, branch) = assets_repo_config(state)?;
    let oid = tip_commit_oid(&root, &slug, &branch)?;
    Some(format!("/assets/c/{}", oid))
}

/// Same as [`pinned_assets_url_prefix`], but repos root is the parent of a content bare
/// (CleverScript host only has the content repo path).
pub fn pinned_assets_url_prefix_from_content_bare(content_bare: &Path) -> Option<String> {
    let slug = env_assets_repo()?;
    let branch = env_assets_branch();
    let root = content_bare.parent()?;
    let oid = tip_commit_oid(root, &slug, &branch)?;
    Some(format!("/assets/c/{}", oid))
}

fn read_blob_at_commit(repos_root: &Path, slug: &str, commit: &str, tree_path: &str) -> Option<Vec<u8>> {
    let path = bare_path(repos_root, slug);
    let repo = Repository::open_bare(&path).ok()?;
    let oid = git2::Oid::from_str(commit).ok()?;
    let commit = repo.find_commit(oid).ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(tree_path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(blob.content().to_vec())
}

/// Serve a commit-pinned framework asset, or `None` if not a pinned path / not configured / missing.
pub fn try_pinned_web_asset(state: &AppState, request_path: &str) -> Option<Response> {
    let pinned = parse_pinned_framework_path(request_path)?;
    let (root, slug, _branch) = assets_repo_config(state)?;
    let bytes = read_blob_at_commit(&root, &slug, pinned.commit_oid, &pinned.tree_path())?;
    let ct = mime_guess::from_path(pinned.file_rel)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    let mut resp = (StatusCode::OK, [("Content-Type", ct)], bytes).into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    // Help operators / clients correlate loads.
    if let Ok(v) = HeaderValue::from_str(pinned.commit_oid) {
        headers.insert("X-Relay-Assets-Commit", v);
    }
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    #[test]
    fn parse_pinned_rejects_traversal_and_short_oid() {
        assert!(parse_pinned_framework_path("assets/c/abcd/embed.js").is_none());
        assert!(parse_pinned_framework_path(
            "assets/c/0123456789abcdef0123456789abcdef01234567/../etc/passwd"
        )
        .is_none());
        let p = parse_pinned_framework_path(
            "assets/c/0123456789abcdef0123456789abcdef01234567/embed.js",
        )
        .unwrap();
        assert_eq!(p.tree_path(), "assets/embed.js");
    }

    #[test]
    fn tip_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let bare = root.join("relay-web-assets.git");
        let repo = Repository::init_bare(&bare).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let blob = repo.blob(b"console.log(1)").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        let mut assets = repo.treebuilder(None).unwrap();
        assets.insert("embed.js", blob, 0o100644).unwrap();
        let assets_oid = assets.write().unwrap();
        tb.insert("assets", assets_oid, 0o040000).unwrap();
        let tree = tb.write().unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &repo.find_tree(tree).unwrap(), &[])
            .unwrap();
        let oid = commit.to_string();
        assert_eq!(tip_commit_oid(&root, "relay-web-assets", "main").as_deref(), Some(oid.as_str()));
        let bytes = read_blob_at_commit(&root, "relay-web-assets", &oid, "assets/embed.js").unwrap();
        assert_eq!(bytes, b"console.log(1)");
    }
}

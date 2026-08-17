//! Commit-pinned web/CDN assets from a dedicated catalog bare repo (`relay-web-assets`).
//!
//! See `docs/RELAY_WEB_ASSETS.md`. URLs:
//! - `/assets/c/{40hex}/{rel}` → blob `assets/{rel}` at that commit
//! - `/wasm/c/{40hex}/{rel}` → blob `wasm/{rel}` at that commit
//!
//! Per-repo opt-in: `server.hooks.get` CleverScript must `import … from '@relay/web-assets'`.
//! Route capture uses a cached source scan — never runs the GET hook for chunk/WASM GETs.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use git2::Repository;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::git::index_policy;
use crate::git::repo::read_relay_config;
use crate::types::AppState;

const OID_LEN: usize = 40;

/// Virtual module specifiers sites import to opt into framework `/assets` + `/wasm`.
pub const RELAY_WEB_ASSETS_MODULE: &str = "@relay/web-assets";

#[derive(Clone, Hash, PartialEq, Eq)]
struct OptInKey {
    repo: String,
    commit: String,
    hook_path: String,
}

fn opt_in_cache() -> &'static Mutex<HashMap<OptInKey, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<OptInKey, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_enabled() -> bool {
    std::env::var("RELAY_DISABLE_CONFIG_CACHE").ok().as_deref() != Some("1")
}

/// True when CleverScript source imports `@relay/web-assets` (static scan, not execution).
pub fn source_imports_web_assets(source: &str) -> bool {
    // Match common forms: from '@relay/web-assets' / from "@relay/web-assets"
    source.contains("'@relay/web-assets'") || source.contains("\"@relay/web-assets\"")
}

fn opt_in_from_bare(bare: &Path, branch: &str) -> bool {
    let repo = match Repository::open_bare(bare) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let refname = format!("refs/heads/{branch}");
    let commit = match repo
        .find_reference(&refname)
        .and_then(|r| r.peel_to_commit())
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let commit_oid = commit.id().to_string();
    let cfg = match read_relay_config(&repo, &commit_oid) {
        Some(c) => c,
        None => return false,
    };
    let Some(hook_path) = index_policy::clever_get_hook_path(&cfg) else {
        return false;
    };

    let key = OptInKey {
        repo: bare.to_string_lossy().into_owned(),
        commit: commit_oid,
        hook_path: hook_path.clone(),
    };
    if cache_enabled() {
        if let Ok(guard) = opt_in_cache().lock() {
            if let Some(hit) = guard.get(&key) {
                return *hit;
            }
        }
    }

    let source =
        match crate::git::relay_config_cache::read_hook_source_cached(bare, branch, &hook_path) {
            Ok(s) => s,
            Err(_) => return false,
        };
    let opted = source_imports_web_assets(&source);

    if cache_enabled() {
        if let Ok(mut guard) = opt_in_cache().lock() {
            if guard.len() >= 512 {
                guard.clear();
            }
            guard.insert(key, opted);
        }
    }
    opted
}

/// Whether `repo_name` at `branch` opts into framework `/assets` + `/wasm` capture.
pub fn repo_opts_into_web_assets(state: &AppState, repo_name: &str, branch: &str) -> bool {
    let bare = state.repo_path.join(format!("{}.git", repo_name));
    opt_in_from_bare(&bare, branch)
}

/// Same check when the CleverScript host only has the content bare path.
pub fn content_bare_opts_into_web_assets(content_bare: &Path, branch: &str) -> bool {
    opt_in_from_bare(content_bare, branch)
}

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
    let slug = state.web_assets_repo.clone().or_else(env_assets_repo)?;
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
    let reference = repo
        .find_reference(&format!("refs/heads/{}", branch))
        .ok()?;
    let commit = reference.peel_to_commit().ok()?;
    Some(commit.id().to_string())
}

/// `/assets/c/{oid}` prefix for HTML stamping, or `None` to use disk `/assets`.
pub fn pinned_assets_url_prefix(state: &AppState) -> Option<String> {
    let (root, slug, branch) = assets_repo_config(state)?;
    let oid = tip_commit_oid(&root, &slug, &branch)?;
    Some(format!("/assets/c/{}", oid))
}

/// `/wasm/c/{oid}` prefix (same catalog tip as assets).
pub fn pinned_wasm_url_prefix(state: &AppState) -> Option<String> {
    let (root, slug, branch) = assets_repo_config(state)?;
    let oid = tip_commit_oid(&root, &slug, &branch)?;
    Some(format!("/wasm/c/{}", oid))
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

/// Same as [`pinned_wasm_url_prefix`] from a content bare's parent repos root.
pub fn pinned_wasm_url_prefix_from_content_bare(content_bare: &Path) -> Option<String> {
    let slug = env_assets_repo()?;
    let branch = env_assets_branch();
    let root = content_bare.parent()?;
    let oid = tip_commit_oid(root, &slug, &branch)?;
    Some(format!("/wasm/c/{}", oid))
}

/// Prefix for HTML stamping only when the content repo imports `@relay/web-assets`.
pub fn pinned_assets_url_prefix_if_opted_in(
    state: &AppState,
    repo_name: &str,
    branch: &str,
) -> Option<String> {
    if !repo_opts_into_web_assets(state, repo_name, branch) {
        return None;
    }
    pinned_assets_url_prefix(state)
}

fn read_blob_at_commit(
    repos_root: &Path,
    slug: &str,
    commit: &str,
    tree_path: &str,
) -> Option<Vec<u8>> {
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
    fn source_imports_detects_common_forms() {
        assert!(source_imports_web_assets(
            "import { pinnedPrefix } from '@relay/web-assets';\n"
        ));
        assert!(source_imports_web_assets(
            "import { pinnedPrefix } from \"@relay/web-assets\";\n"
        ));
        assert!(!source_imports_web_assets(
            "import { ssrClientHookFromGetContext } from '@relay/ssr';\n"
        ));
    }

    fn bare_content_with_get_cs(root: &Path, slug: &str, get_cs: &str) {
        let bare = root.join(format!("{slug}.git"));
        let repo = Repository::init_bare(&bare).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let yaml = r#"
server:
  hooks:
    get:
      path: hooks/server/get.cs
"#;
        let yaml_oid = repo.blob(yaml.as_bytes()).unwrap();
        let cs_oid = repo.blob(get_cs.as_bytes()).unwrap();
        let mut tb_srv = repo.treebuilder(None).unwrap();
        tb_srv.insert("get.cs", cs_oid, 0o100644).unwrap();
        let srv_tid = tb_srv.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", srv_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", yaml_oid, 0o100644).unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree = tb.write().unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "init",
            &repo.find_tree(tree).unwrap(),
            &[],
        )
        .unwrap();
    }

    #[test]
    fn opt_in_requires_web_assets_import() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        bare_content_with_get_cs(
            root,
            "plain",
            "import { markdownHtmlFromGetContext } from '@relay/md';\nexport default function run(ctx, next) { return next(); }\n",
        );
        bare_content_with_get_cs(
            root,
            "embed-site",
            "import { pinnedPrefix } from '@relay/web-assets';\nexport default function run(ctx, next) { return next(); }\n",
        );
        assert!(!opt_in_from_bare(&root.join("plain.git"), "main"));
        assert!(opt_in_from_bare(&root.join("embed-site.git"), "main"));
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
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "init",
                &repo.find_tree(tree).unwrap(),
                &[],
            )
            .unwrap();
        let oid = commit.to_string();
        assert_eq!(
            tip_commit_oid(&root, "relay-web-assets", "main").as_deref(),
            Some(oid.as_str())
        );
        let bytes =
            read_blob_at_commit(&root, "relay-web-assets", &oid, "assets/embed.js").unwrap();
        assert_eq!(bytes, b"console.log(1)");
    }
}

//! Publisher pre-receive gate: CleverScript SSR + HTML QA on changed park paths.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::catalog_policy::{self, CatalogRepoRecord};
use crate::git::ssr_render::{
    deadline_exceeded, pre_receive_wall_budget, ssr_render_and_qa, SsrRenderError,
};

const MAX_PATHS: usize = 32;

fn env_qa_enabled() -> bool {
    match std::env::var("RELAY_SSR_PRE_RECEIVE_QA") {
        Ok(v) => {
            let v = v.trim();
            !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
        }
        Err(_) => true,
    }
}

fn catalog_record_for_bare(repo_path: &Path) -> Option<CatalogRepoRecord> {
    let slug = repo_path
        .file_name()
        .and_then(|s| s.to_str())?
        .trim_end_matches(".git");
    let cat = catalog_policy::catalog_path_from_env()?;
    let index = catalog_policy::try_load_catalog_index(&cat)?;
    catalog_policy::catalog_record_for_slug(&index, slug).cloned()
}

fn require_qa(rec: &CatalogRepoRecord) -> bool {
    let Some(c) = rec.ssr_cache.as_ref() else {
        return false;
    };
    c.enabled && c.require_pre_receive_qa
}

/// Paths to SSR from `old..new` (markdown / page routes). Cap + always include `/`.
pub fn changed_ssr_paths(repo_path: &Path, old_oid: &str, new_oid: &str) -> Vec<String> {
    let mut paths = Vec::new();
    if old_oid.chars().all(|c| c == '0') {
        // New branch: smoke home only + later sample from tree would be heavy; home is enough.
        paths.push("/".to_string());
        return paths;
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args([
            "diff",
            "--name-only",
            "--diff-filter=ACMR",
            &format!("{old_oid}..{new_oid}"),
        ])
        .output();
    let Ok(out) = out else {
        paths.push("/".to_string());
        return paths;
    };
    if !out.status.success() {
        paths.push("/".to_string());
        return paths;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(".relay/") || line.starts_with("hooks/") {
            // Hook / policy change → force home re-check.
            if !paths.iter().any(|p| p == "/") {
                paths.push("/".to_string());
            }
            continue;
        }
        if line.ends_with(".md") {
            let p = format!("/{}", line.trim_start_matches('/'));
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    if paths.is_empty() {
        // Non-md change (assets only): still verify home when hooks touched; else skip.
        return paths;
    }
    if !paths.iter().any(|p| p == "/") {
        paths.insert(0, "/".to_string());
    }
    if paths.len() > MAX_PATHS {
        paths.truncate(MAX_PATHS);
    }
    paths
}

/// Run SSR QA for a content push. `Ok(())` to allow; `Err` message rejects the push.
///
/// Publisher-only: skips when this node is not catalog `publish_origin` (replicas /
/// peer-fetch must not re-run CleverScript SSR on every FF).
pub fn enforce_ssr_pre_receive_qa(
    repo_path: &Path,
    branch: &str,
    old_oid: &str,
    new_oid: &str,
) -> Result<(), String> {
    if !env_qa_enabled() {
        tracing::warn!(
            target: "relay_ssr",
            "RELAY_SSR_PRE_RECEIVE_QA=0; skipping SSR pre-receive gate"
        );
        return Ok(());
    }
    let hostname = std::env::var("RELAY_PUBLIC_HOSTNAME")
        .or_else(|_| std::env::var("RELAY_NODE_FQDN"))
        .ok();
    if crate::git::publish_origin_push_gate(repo_path, branch, hostname.as_deref()).is_none() {
        return Ok(());
    }
    let Some(rec) = catalog_record_for_bare(repo_path) else {
        return Ok(());
    };
    if !require_qa(&rec) {
        return Ok(());
    }
    // Only gate the catalog publish branch.
    if branch != rec.publish_branch {
        return Ok(());
    }

    let paths = changed_ssr_paths(repo_path, old_oid, new_oid);
    if paths.is_empty() {
        return Ok(());
    }

    let budget = pre_receive_wall_budget();
    let started = Instant::now();
    let mut errors: Vec<SsrRenderError> = Vec::new();

    for path in &paths {
        if deadline_exceeded(started, budget) {
            return Err(format!(
                "SSR pre-receive QA exceeded wall budget ({}s) after {} path(s); last ok before timeout",
                budget.as_secs(),
                paths.len().saturating_sub(errors.len())
            ));
        }
        match ssr_render_and_qa(repo_path, new_oid, path, None) {
            Ok(_) => {
                tracing::info!(
                    target: "relay_ssr",
                    path = %path,
                    commit = %&new_oid[..new_oid.len().min(12)],
                    "SSR pre-receive QA ok"
                );
            }
            Err(e) => {
                tracing::error!(
                    target: "relay_ssr",
                    path = %e.path,
                    reason = %e.reason,
                    "SSR pre-receive QA failed"
                );
                errors.push(e);
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }
    let mut msg = String::from("SSR pre-receive QA failed:\n");
    for e in errors {
        msg.push_str(&format!("  - {}\n", e));
    }
    msg.push_str("Fix the client hook / content so CleverScript SSR produces readable HTML, then push again.\n");
    msg.push_str("Emergency bypass (publisher only): RELAY_SSR_PRE_RECEIVE_QA=0\n");
    Err(msg)
}

/// True when post-receive should enqueue an SSR bake for this bare.
pub fn should_enqueue_ssr_bake(repo_path: &Path, branch: &str) -> bool {
    let Some(rec) = catalog_record_for_bare(repo_path) else {
        return false;
    };
    if !rec.ssr_cache_enabled() {
        return false;
    }
    branch == rec.publish_branch
}

/// Write a bake job JSON for `relay-ssr-bake-queue.sh` (publisher only).
pub fn enqueue_ssr_bake_job(repo_path: &Path, branch: &str, new_oid: &str) -> Result<(), String> {
    if std::env::var("RELAY_NODE_ROLE")
        .ok()
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case("mesh"))
    {
        return Ok(());
    }
    let rec = catalog_record_for_bare(repo_path)
        .ok_or_else(|| "no catalog record for SSR bake enqueue".to_string())?;
    let spec = rec
        .ssr_cache_spec()
        .ok_or_else(|| "ssrCache not configured".to_string())?;
    let prefix = spec.prefix.clone().unwrap_or_default();
    let queue = std::env::var("RELAY_SSR_BAKE_QUEUE_DIR")
        .unwrap_or_else(|_| "/opt/relay/state/ssr-bake-queue".into());
    let cache_wt = std::env::var("RELAY_SSR_CACHE_WORKTREE").unwrap_or_default();
    if cache_wt.is_empty() {
        tracing::warn!(
            target: "relay_ssr",
            "RELAY_SSR_CACHE_WORKTREE unset; skip bake enqueue"
        );
        return Ok(());
    }
    let pending = Path::new(&queue).join("pending");
    std::fs::create_dir_all(&pending).map_err(|e| e.to_string())?;
    // Bake expects filesystem paths. Catalog stores hook-assets as a repo slug.
    let hook_assets_path = spec.hook_assets_repo.as_ref().map(|slug| {
        let data = std::env::var("RELAY_REPO_PATH").unwrap_or_else(|_| "/opt/relay/data".into());
        let bare = Path::new(&data).join(format!("{slug}.git"));
        if bare.is_dir() {
            bare.display().to_string()
        } else {
            // Fall back to slug; queue script may still resolve it.
            slug.clone()
        }
    });
    let job = serde_json::json!({
        "content": repo_path.display().to_string(),
        "cacheWorktree": cache_wt,
        "prefix": prefix,
        "siteName": prefix.trim_matches('/').rsplit('/').next().unwrap_or(""),
        "contentBranch": branch,
        "contentTip": new_oid,
        "full": false,
        "hookAssets": hook_assets_path,
        "hookAssetsBranch": spec.hook_assets_branch.clone().unwrap_or_else(|| "release".into()),
    });
    let name = format!(
        "{}-{}-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        std::process::id(),
        &new_oid[..new_oid.len().min(8)]
    );
    let path = pending.join(name);
    std::fs::write(&path, serde_json::to_string_pretty(&job).unwrap_or_default() + "\n")
        .map_err(|e| e.to_string())?;
    tracing::info!(
        target: "relay_ssr",
        job = %path.display(),
        "enqueued SSR bake job"
    );
    Ok(())
}

/// Non-blocking post-receive enqueue.
pub fn spawn_enqueue_ssr_bake(repo_path: PathBuf, branch: String, new_oid: String) {
    if !should_enqueue_ssr_bake(&repo_path, &branch) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = enqueue_ssr_bake_job(&repo_path, &branch, &new_oid) {
            tracing::warn!(target: "relay_ssr", error = %e, "SSR bake enqueue failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    fn commit_tree(repo: &Repository, files: &[(&str, &str)], parents: &[&git2::Commit]) -> git2::Oid {
        let sig = Signature::now("t", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        index.clear().unwrap();
        for (path, src) in files {
            let blob = repo.blob(src.as_bytes()).unwrap();
            let entry = git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: src.len() as u32,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            };
            index.add(&entry).unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "c",
            &tree,
            parents,
        )
        .unwrap()
    }

    #[test]
    fn empty_diff_returns_empty_without_zero_oid() {
        // Nonexistent repo → git fails → home fallback
        let p = Path::new("/tmp/relay-ssr-qa-no-such-repo.git");
        let paths = changed_ssr_paths(p, "a".repeat(40).as_str(), "b".repeat(40).as_str());
        assert_eq!(paths, vec!["/".to_string()]);
    }

    #[test]
    fn new_branch_only_home() {
        let paths = changed_ssr_paths(
            Path::new("/tmp"),
            &"0".repeat(40),
            &"a".repeat(40),
        );
        assert_eq!(paths, vec!["/".to_string()]);
    }

    #[test]
    fn changed_ssr_paths_lists_md_and_home() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        let repo = Repository::init_bare(&bare).unwrap();
        let old = commit_tree(&repo, &[("README.md", "# a\n")], &[]);
        let parent = repo.find_commit(old).unwrap();
        let new = commit_tree(
            &repo,
            &[
                ("README.md", "# a\n"),
                ("events/foo.md", "# foo\n"),
                ("assets/x.png", "PNG"),
            ],
            &[&parent],
        );
        let paths = changed_ssr_paths(&bare, &old.to_string(), &new.to_string());
        assert!(paths.iter().any(|p| p == "/"), "{paths:?}");
        assert!(
            paths.iter().any(|p| p == "/events/foo.md"),
            "{paths:?}"
        );
        assert!(!paths.iter().any(|p| p.contains("png")), "{paths:?}");
    }
}

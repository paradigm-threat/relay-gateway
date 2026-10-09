//! Reject raw binary blobs on push when catalog disallows them (thin IPFS repos).

use std::path::Path;
use std::process::Command;

use crate::catalog_policy::CatalogRepoRecord;
use crate::git::ipfs::{is_likely_binary_content, parse_pointer_bytes, MANIFEST_PATH};

pub fn allow_binary_files(catalog: Option<&CatalogRepoRecord>) -> bool {
    catalog.map(|c| c.allow_binary_files()).unwrap_or(true)
}

/// Paths touched in `commit` must not introduce disallowed binary blobs.
pub fn enforce_commit_no_disallowed_binaries(
    repo_path: &Path,
    commit: &str,
    allow_binary: bool,
) -> Result<(), String> {
    if allow_binary {
        return Ok(());
    }

    let paths = diff_added_or_modified_paths_for_commit(repo_path, commit)?;
    for path in paths {
        if should_skip_path(&path) {
            continue;
        }
        let bytes = read_blob_at_commit(repo_path, commit, &path)?;
        if bytes.is_empty() {
            continue;
        }
        if parse_pointer_bytes(&bytes).is_some() {
            continue;
        }
        if path == MANIFEST_PATH || path.ends_with(".md") || path.ends_with(".svg") {
            if !is_likely_binary_content(&bytes) {
                continue;
            }
        }
        if is_likely_binary_content(&bytes) {
            return Err(format!(
                "commit {}: binary file {:?} not allowed (use relay-ipfs-ingest; expected {} pointer)",
                commit, path, crate::git::ipfs::POINTER_MAGIC
            ));
        }
    }
    Ok(())
}

fn should_skip_path(path: &str) -> bool {
    if path.starts_with(".git/") {
        return true;
    }
    if path.starts_with(".relay/assets/") && path.ends_with(".ipfs") {
        return false;
    }
    if path.starts_with(".relay/") {
        return true;
    }
    // Flashback sideload APK + version manifest live under /downloads (git tip, not IPFS).
    if path.starts_with("downloads/") && (path.ends_with(".apk") || path.ends_with(".json")) {
        return true;
    }
    path.ends_with(".json") && path != MANIFEST_PATH
}

/// Added/modified paths only — deletions must not `git show new:path` (missing blob).
fn diff_added_or_modified_paths_for_commit(
    repo_path: &Path,
    commit: &str,
) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["diff-tree", "--no-commit-id", "--name-status", "-r", commit])
        .output()
        .map_err(|e| format!("git diff-tree: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git diff-tree failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or("");
        // D = deleted; skip. R100/C100 → take new path.
        if status.starts_with('D') {
            continue;
        }
        if status.starts_with('R') || status.starts_with('C') {
            let _old = parts.next();
            if let Some(new_path) = parts.next() {
                if !new_path.is_empty() {
                    paths.push(new_path.to_string());
                }
            }
            continue;
        }
        if let Some(path) = parts.next() {
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    }
    Ok(paths)
}

fn read_blob_at_commit(repo_path: &Path, commit: &str, path: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["show", &format!("{}:{}", commit, path)])
        .output()
        .map_err(|e| format!("git show: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git show {}:{} failed: {}",
            commit,
            path,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_manifest_json_false_positive() {
        assert!(!should_skip_path("assets.manifest.json"));
    }

    #[test]
    fn central_ipfs_stubs_are_checked() {
        assert!(!should_skip_path(".relay/assets/media/foo.png.ipfs"));
        assert!(should_skip_path(".relay/local-assets/x.png"));
    }

    #[test]
    fn flashback_downloads_apk_allowed_as_tip() {
        assert!(should_skip_path("downloads/flashback.apk"));
        assert!(should_skip_path("downloads/flashback.json"));
        assert!(!should_skip_path("FanMixes/foo.apk"));
    }
}

use git2::Repository;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::types::ReadError;

/// Fast-forward-only pushes: `git push --force` and non-FF updates are rejected by `receive-pack`.
pub fn apply_bare_receive_security(repo: &Repository) -> Result<(), git2::Error> {
    let mut cfg = repo.config()?;
    cfg.set_str("receive.denyNonFastForwards", "true")?;
    Ok(())
}

/// Apply [`apply_bare_receive_security`] when opening a bare repo by path (e.g. after `init_bare`).
pub fn apply_bare_receive_security_at_path(repo_path: &Path) -> Result<(), String> {
    let repo = Repository::open_bare(repo_path).map_err(|e| e.to_string())?;
    apply_bare_receive_security(&repo).map_err(|e| e.to_string())
}

/// Returns a sorted list of bare repository names (without .git suffix) in the given root directory
pub fn bare_repo_names(root: &PathBuf) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let p = e.path();
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if name.ends_with(".git") {
                        names.push(name.trim_end_matches(".git").to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names
}

/// Opens a bare repository by name (without .git suffix)
pub fn open_repo(root: &PathBuf, name: &str) -> Option<Repository> {
    let p = root.join(format!("{}.git", name));
    Repository::open_bare(p).ok()
}

/// Default branch name from the bare repo's symbolic `HEAD` (e.g. `refs/heads/master` → `master`).
/// Used when the client sends no branch header. Returns `None` if `HEAD` is missing or not symbolic.
pub fn default_branch_from_bare_head(root: &PathBuf, name: &str) -> Option<String> {
    let repo = open_repo(root, name)?;
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target()?;
    target
        .strip_prefix("refs/heads/")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read .relay.yaml configuration from git tree for the given revision (branch name or commit hash)
pub fn read_relay_config(repo: &Repository, rev: &str) -> Option<crate::types::RelayConfig> {
    let obj = repo.revparse_single(rev).ok()?;
    let commit = obj.as_commit()?;
    let tree = commit.tree().ok()?;

    let entry = tree.get_name(".relay.yaml")?;
    let obj = entry.to_object(repo).ok()?;
    let blob = obj.as_blob()?;
    let content = std::str::from_utf8(blob.content()).ok()?;
    serde_yaml::from_str(content).ok()
}

/// Read hooks/git.yaml configuration from git tree for the given revision
pub fn read_git_config(repo: &Repository, rev: &str) -> Option<crate::types::GitConfig> {
    read_relay_config(repo, rev).and_then(|c| c.git)
}

/// Read `git` config from `.relay.yaml` at `rev` using **`git show`** (inherits hook env).
///
/// During **`pre-receive`**, new objects live in a quarantine directory (`GIT_OBJECT_DIRECTORY` /
/// alternates). **libgit2** often cannot resolve those commits; the Git CLI can.
pub fn read_git_config_from_revision(repo_path: &Path, rev: &str) -> Option<crate::types::GitConfig> {
    let spec = format!("{}:.relay.yaml", rev);
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("show")
        .arg(&spec)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let content = std::str::from_utf8(&out.stdout).ok()?;
    let cfg: crate::types::RelayConfig = serde_yaml::from_str(content).ok()?;
    cfg.git
}

/// Get commit information for a branch
pub fn get_branch_commit_info(repo: &Repository, branch: &str) -> Option<(String, String, String)> {
    let refname = format!("refs/heads/{}", branch);
    let reference = repo.find_reference(&refname).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    Some((
        commit.id().to_string(),
        commit.summary().unwrap_or("").to_string(),
        commit.time().seconds().to_string(),
    ))
}

/// List all branches in a repository
pub fn list_branches(repo: &Repository) -> Vec<String> {
    let mut branches = Vec::new();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            if let Some(name) = r.name() {
                if let Some(branch_name) = name.strip_prefix("refs/heads/") {
                    branches.push(branch_name.to_string());
                }
            }
        }
    }
    branches.sort();
    branches
}

/// Read a file from a git repository
pub fn read_file_from_repo(
    repo_path: &PathBuf,
    branch: &str,
    path: &str,
) -> Result<Vec<u8>, ReadError> {
    let repo = Repository::open_bare(repo_path).map_err(|e| ReadError::Other(e.into()))?;
    let refname = format!("refs/heads/{}", branch);
    let reference = repo
        .find_reference(&refname)
        .map_err(|_| ReadError::NotFound)?;
    let commit = reference
        .peel_to_commit()
        .map_err(|_| ReadError::NotFound)?;
    let tree = commit.tree().map_err(|e| ReadError::Other(e.into()))?;
    let entry = tree
        .get_path(std::path::Path::new(path))
        .map_err(|_| ReadError::NotFound)?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|e| ReadError::Other(e.into()))?;
    Ok(blob.content().to_vec())
}

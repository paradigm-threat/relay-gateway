use crate::types::ReadError;
use git2::Repository;
use std::path::{Path, PathBuf};

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

/// Create or open `root/<slug>.git` as a bare repo with `receive.denyNonFastForwards`.
///
/// Returns `(created, dest)`. Same logic as `POST /api/git/ensure-bare/:repo` and first-push
/// provision on `git-receive-pack`. `slug` must already be validated.
pub fn ensure_bare_repo(root: &Path, slug: &str) -> Result<(bool, PathBuf), String> {
    let dest = root.join(format!("{slug}.git"));
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    if dest.exists() {
        return open_existing_bare(&dest).map(|()| (false, dest));
    }
    match Repository::init_bare(&dest) {
        Ok(_) => {
            apply_bare_receive_security_at_path(&dest)?;
            Ok((true, dest))
        }
        Err(e) => {
            // First-push race: another connection created the path first.
            if dest.exists() {
                open_existing_bare(&dest).map(|()| (false, dest))
            } else {
                Err(e.to_string())
            }
        }
    }
}

fn open_existing_bare(dest: &Path) -> Result<(), String> {
    let r = Repository::open(dest).map_err(|e| e.to_string())?;
    if !r.is_bare() {
        return Err("path exists but is not a bare git repository".to_string());
    }
    apply_bare_receive_security_at_path(dest)
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

/// Opens a repository for the given app state and repo name.
pub fn open_repo_for_state(state: &crate::types::AppState, name: &str) -> Option<Repository> {
    match &state.serve_context {
        crate::types::ServeContext::BareFleet => open_repo(&state.repo_path, name),
        crate::types::ServeContext::LocalWorktree {
            checkout,
            repo_name,
            ..
        } if repo_name == name => Repository::open(checkout).ok(),
        _ => None,
    }
}

/// Repo names visible to HTTP routing for this app state.
pub fn repo_names_for_state(state: &crate::types::AppState) -> Vec<String> {
    match &state.serve_context {
        crate::types::ServeContext::LocalWorktree { repo_name, .. } => vec![repo_name.clone()],
        crate::types::ServeContext::BareFleet => bare_repo_names(&state.repo_path),
    }
}

/// Opens a bare repository by name (without .git suffix)
pub fn open_repo(root: &PathBuf, name: &str) -> Option<Repository> {
    let p = root.join(format!("{}.git", name));
    Repository::open_bare(p).ok()
}

/// True when `refs/heads/{branch}` exists on the bare repo.
pub fn branch_ref_exists(repo: &Repository, branch: &str) -> bool {
    !branch.is_empty() && repo.find_reference(&format!("refs/heads/{branch}")).is_ok()
}

/// Default branch name from the bare repo's symbolic `HEAD` (e.g. `refs/heads/main` → `main`).
/// If `HEAD` points at a missing branch (stale `master` after rename), falls back to `main`,
/// then `master`, then the first existing branch. Returns `None` only when no branch refs exist.
pub fn default_branch_from_bare_head(root: &PathBuf, name: &str) -> Option<String> {
    let repo = open_repo(root, name)?;
    if let Ok(head) = repo.find_reference("HEAD") {
        if let Some(target) = head.symbolic_target() {
            if let Some(branch) = target.strip_prefix("refs/heads/").filter(|s| !s.is_empty()) {
                if branch_ref_exists(&repo, branch) {
                    return Some(branch.to_string());
                }
            }
        }
    }
    for fallback in [crate::types::DEFAULT_BRANCH, "master"] {
        if branch_ref_exists(&repo, fallback) {
            return Some(fallback.to_string());
        }
    }
    let mut names = bare_repo_branch_names(&repo);
    names.into_iter().next()
}

/// Sorted branch names (without `refs/heads/` prefix) for a bare repo.
fn bare_repo_branch_names(repo: &Repository) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            if let Some(name) = r.name() {
                if let Some(branch) = name.strip_prefix("refs/heads/") {
                    if !branch.is_empty() {
                        names.push(branch.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names
}

/// Read .relay.yaml configuration from git tree for the given revision (branch name or commit hash).
pub fn read_relay_config(repo: &Repository, rev: &str) -> Option<crate::types::RelayConfig> {
    super::relay_config_cache::read_relay_config_cached(repo, rev)
}

/// Read hooks/git.yaml configuration from git tree for the given revision
pub fn read_git_config(repo: &Repository, rev: &str) -> Option<crate::types::GitConfig> {
    read_relay_config(repo, rev).and_then(|c| c.git)
}

/// Read `git` config from `.relay.yaml` at `rev` using **`git show`** (inherits hook env).
///
/// During **`pre-receive`**, new objects live in a quarantine directory (`GIT_OBJECT_DIRECTORY` /
/// alternates). **libgit2** often cannot resolve those commits; the Git CLI can.
pub fn read_git_config_from_revision(
    repo_path: &Path,
    rev: &str,
) -> Option<crate::types::GitConfig> {
    super::relay_config_cache::read_relay_config_at_path_cached(repo_path, rev).and_then(|c| c.git)
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

/// Read a file at a specific revision (branch name or commit OID).
pub fn read_file_from_revision(
    repo_path: &Path,
    rev: &str,
    path: &str,
) -> Result<Vec<u8>, ReadError> {
    let repo = Repository::open_bare(repo_path).map_err(|e| ReadError::Other(e.into()))?;
    let obj = repo.revparse_single(rev).map_err(|_| ReadError::NotFound)?;
    let commit = obj.peel_to_commit().map_err(|_| ReadError::NotFound)?;
    let tree = commit.tree().map_err(|e| ReadError::Other(e.into()))?;
    let entry = tree
        .get_path(std::path::Path::new(path))
        .map_err(|_| ReadError::NotFound)?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|e| ReadError::Other(e.into()))?;
    Ok(blob.content().to_vec())
}

/// Read a file from a git repository at the branch tip.
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

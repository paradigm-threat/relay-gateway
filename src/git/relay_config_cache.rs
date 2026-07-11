//! In-process cache for parsed `.relay.yaml` and hook CleverScript source.
//!
//! Keys: `(canonical repo path, commit OID)`. Invalidates naturally when the tip moves to a new OID.
//! Disable with **`RELAY_DISABLE_CONFIG_CACHE=1`** (tests/debug).

use git2::Repository;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

const MAX_CONFIG_ENTRIES: usize = 512;
const MAX_HOOK_SOURCE_ENTRIES: usize = 256;

#[derive(Clone, Hash, PartialEq, Eq)]
struct ConfigKey {
    repo: String,
    commit: String,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct HookSourceKey {
    repo: String,
    commit: String,
    path: String,
}

struct CacheState {
    configs: HashMap<ConfigKey, Arc<crate::types::RelayConfig>>,
    config_order: Vec<ConfigKey>,
    hook_sources: HashMap<HookSourceKey, Arc<String>>,
    hook_source_order: Vec<HookSourceKey>,
}

fn cache_enabled() -> bool {
    std::env::var("RELAY_DISABLE_CONFIG_CACHE").ok().as_deref() != Some("1")
}

fn state() -> &'static Mutex<CacheState> {
    static STATE: OnceLock<Mutex<CacheState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(CacheState {
            configs: HashMap::new(),
            config_order: Vec::new(),
            hook_sources: HashMap::new(),
            hook_source_order: Vec::new(),
        })
    })
}

fn repo_key(repo_path: &Path) -> String {
    repo_path.to_string_lossy().into_owned()
}

fn resolve_commit_oid(repo: &Repository, rev: &str) -> Option<String> {
    let obj = repo.revparse_single(rev).ok()?;
    let commit = obj.peel_to_commit().ok()?;
    Some(commit.id().to_string())
}

/// Quarantine-safe OID resolve for `pre-receive` (inherits `GIT_QUARANTINE_PATH` / object dir).
fn resolve_commit_oid_git(repo_path: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--verify", &format!("{}^{{commit}}", rev)])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let oid = s.trim();
    if oid.len() == 40 && oid.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(oid.to_string())
    } else {
        None
    }
}

fn load_relay_config_from_tree(repo: &Repository, rev: &str) -> Option<crate::types::RelayConfig> {
    let obj = repo.revparse_single(rev).ok()?;
    let commit = obj.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_name(".relay.yaml")?;
    let obj = entry.to_object(repo).ok()?;
    let blob = obj.as_blob()?;
    let content = std::str::from_utf8(blob.content()).ok()?;
    serde_yaml::from_str(content).ok()
}

fn load_relay_config_git_show(repo_path: &Path, rev: &str) -> Option<crate::types::RelayConfig> {
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
    serde_yaml::from_str(content).ok()
}

/// Parsed `.relay.yaml` at `rev` (branch name or commit OID), cached by commit OID.
pub fn read_relay_config_cached(repo: &Repository, rev: &str) -> Option<crate::types::RelayConfig> {
    let commit = resolve_commit_oid(repo, rev)?;
    let key = ConfigKey {
        repo: repo_key(repo.path()),
        commit: commit.clone(),
    };

    if cache_enabled() {
        if let Some(hit) = state().lock().ok().and_then(|s| s.configs.get(&key).cloned()) {
            return Some((*hit).clone());
        }
    }

    let cfg = load_relay_config_from_tree(repo, rev)?;

    if cache_enabled() {
        if let Ok(mut s) = state().lock() {
            while s.configs.len() >= MAX_CONFIG_ENTRIES {
                if let Some(old) = s.config_order.first().cloned() {
                    s.config_order.remove(0);
                    s.configs.remove(&old);
                } else {
                    break;
                }
            }
            s.config_order.push(key.clone());
            s.configs.insert(key, Arc::new(cfg.clone()));
        }
    }

    Some(cfg)
}

/// Parsed `.relay.yaml` via `git show` (quarantine-safe during `pre-receive`), cached by commit OID.
///
/// Prefer `git` CLI over libgit2 for resolve + read: during `pre-receive`, new objects live in the
/// quarantine directory that subprocess `git` inherits, while `Repository::open_bare` often cannot
/// see them — which previously surfaced as a false "missing .relay.yaml" reject.
pub fn read_relay_config_at_path_cached(
    repo_path: &Path,
    rev: &str,
) -> Option<crate::types::RelayConfig> {
    let commit = resolve_commit_oid_git(repo_path, rev).or_else(|| {
        let repo = Repository::open_bare(repo_path).ok()?;
        resolve_commit_oid(&repo, rev)
    })?;
    let key = ConfigKey {
        repo: repo_key(repo_path),
        commit: commit.clone(),
    };

    if cache_enabled() {
        if let Some(hit) = state().lock().ok().and_then(|s| s.configs.get(&key).cloned()) {
            return Some((*hit).clone());
        }
    }

    let cfg = load_relay_config_git_show(repo_path, rev).or_else(|| {
        let repo = Repository::open_bare(repo_path).ok()?;
        load_relay_config_from_tree(&repo, rev)
    })?;

    if cache_enabled() {
        if let Ok(mut s) = state().lock() {
            while s.configs.len() >= MAX_CONFIG_ENTRIES {
                if let Some(old) = s.config_order.first().cloned() {
                    s.config_order.remove(0);
                    s.configs.remove(&old);
                } else {
                    break;
                }
            }
            s.config_order.push(key.clone());
            s.configs.insert(key, Arc::new(cfg.clone()));
        }
    }

    Some(cfg)
}

/// CleverScript hook source at `rev` + `path`, cached by commit OID.
pub fn read_hook_source_cached(
    repo_path: &Path,
    rev: &str,
    path: &str,
) -> Result<String, crate::types::ReadError> {
    let repo = Repository::open_bare(repo_path).map_err(|e| crate::types::ReadError::Other(e.into()))?;
    let commit = resolve_commit_oid(&repo, rev).ok_or(crate::types::ReadError::NotFound)?;
    let key = HookSourceKey {
        repo: repo_key(repo_path),
        commit: commit.clone(),
        path: path.to_string(),
    };

    if cache_enabled() {
        if let Some(hit) = state()
            .lock()
            .ok()
            .and_then(|s| s.hook_sources.get(&key).cloned())
        {
            return Ok((*hit).clone());
        }
    }

    let bytes = read_file_bytes_at_revision(repo_path, rev, path)?;
    let source = strip_shebang(String::from_utf8_lossy(&bytes).into_owned());

    if cache_enabled() {
        if let Ok(mut s) = state().lock() {
            while s.hook_sources.len() >= MAX_HOOK_SOURCE_ENTRIES {
                if let Some(old) = s.hook_source_order.first().cloned() {
                    s.hook_source_order.remove(0);
                    s.hook_sources.remove(&old);
                } else {
                    break;
                }
            }
            s.hook_source_order.push(key.clone());
            s.hook_sources
                .insert(key, Arc::new(source.clone()));
        }
    }

    Ok(source)
}

fn read_file_bytes_at_revision(
    repo_path: &Path,
    rev: &str,
    path: &str,
) -> Result<Vec<u8>, crate::types::ReadError> {
    match super::repo::read_file_from_revision(repo_path, rev, path) {
        Ok(b) => Ok(b),
        Err(_) => {
            let spec = format!("{}:{}", rev, path);
            let out = Command::new("git")
                .arg("-C")
                .arg(repo_path)
                .arg("show")
                .arg(&spec)
                .output()
                .map_err(|e| crate::types::ReadError::Other(e.into()))?;
            if !out.status.success() {
                return Err(crate::types::ReadError::NotFound);
            }
            Ok(out.stdout)
        }
    }
}

fn strip_shebang(mut source: String) -> String {
    if source.starts_with("#!") {
        if let Some(pos) = source.find('\n') {
            source = source[pos + 1..].to_string();
        } else {
            source.clear();
        }
    }
    source
}

#[cfg(test)]
pub fn clear_for_tests() {
    if let Ok(mut s) = state().lock() {
        s.configs.clear();
        s.config_order.clear();
        s.hook_sources.clear();
        s.hook_source_order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    #[test]
    fn relay_config_cache_returns_same_parse_twice() {
        clear_for_tests();
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let yaml = "name: cached\nserver:\n  hooks: {}\n";
        let oid = repo.blob(yaml.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", oid, 0o100644).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string();

        let key = ConfigKey {
            repo: repo_key(repo.path()),
            commit: commit.clone(),
        };
        let a = read_relay_config_cached(&repo, &commit).expect("first");
        let b = read_relay_config_cached(&repo, &commit).expect("second");
        assert_eq!(a.name.as_deref(), Some("cached"));
        assert_eq!(b.name.as_deref(), Some("cached"));
        assert!(state().lock().unwrap().configs.contains_key(&key));
    }

    #[test]
    fn hook_source_cache_returns_same_source_twice() {
        clear_for_tests();
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let hook = "export default function run(c) { return true; }\n";
        let hook_oid = repo.blob(hook.as_bytes()).unwrap();
        let mut tb_srv = repo.treebuilder(None).unwrap();
        tb_srv.insert("pre-commit.cs", hook_oid, 0o100644).unwrap();
        let srv_tid = tb_srv.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", srv_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string();

        let a = read_hook_source_cached(&repo_path, &commit, "hooks/server/pre-commit.cs")
            .expect("first");
        let b = read_hook_source_cached(&repo_path, &commit, "hooks/server/pre-commit.cs")
            .expect("second");
        assert_eq!(a, b);
        assert!(a.contains("return true"));
    }
}

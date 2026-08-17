//! Authorized repo list + anchor commit validation (pull today; push/planned).
//!
//! Two sources:
//! - **`RELAY_MASTER_CATALOG_PATH`**: open a **`relay-master`** worktree (`catalog/repos/*.json` or
//!   legacy `relay-catalog.json` + `HEAD`); optional **`RELAY_MASTER_TRUST_ROOT`** pins the catalog.
//! - **`RELAY_AUTHORIZED_REPOS_PATH`**: YAML file (e.g. emitted by **`sync-relay-catalog.sh`**).
//!
//! See `docs/RELAY_TRUST_AND_BOOTSTRAP.md` and `docs/RELAY_CATALOG.md`.
use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::catalog_policy::{self, CatalogIndex, CatalogRepoRecord};

/// `relay-catalog.json` at the root of **relay-master** (no git submodules).
#[derive(Debug, Deserialize)]
struct RelayCatalogFile {
    #[serde(default)]
    repos: HashMap<String, RelayCatalogRepo>,
}

#[derive(Debug, Deserialize)]
struct RelayCatalogRepo {
    #[serde(default)]
    #[allow(dead_code)]
    url: Option<String>,
    #[serde(rename = "relayRootCommit")]
    relay_root_commit: String,
    #[serde(default = "default_branch")]
    branch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizedReposFile {
    /// Must match RELAY_SERVER_ID when both are set.
    #[serde(default)]
    pub relay_server_id: Option<String>,
    #[serde(default)]
    pub repos: HashMap<String, RepoAnchor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoAnchor {
    /// Git object id; must be an ancestor of `branch` after every successful pull.
    pub anchor_commit: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Catalog public blurb; copied from `catalog/repos/*.json` `description`.
    #[serde(default)]
    pub description: Option<String>,
}

fn default_branch() -> String {
    "main".to_string()
}

fn nonempty_description(d: Option<String>) -> Option<String> {
    d.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn load_from_path(path: &Path) -> anyhow::Result<AuthorizedReposFile> {
    let s = std::fs::read_to_string(path)?;
    let cfg: AuthorizedReposFile = serde_yaml::from_str(&s)?;
    Ok(cfg)
}

/// Ensures `trust_root` is an ancestor of the catalog checkout `HEAD` (same lineage as
/// catalog `relayRootCommit` checks). Use the canonical **root** of `relay-master`:
/// `git rev-list --max-parents=0 main` (pick the policy root if there are several).
pub fn verify_master_catalog_trust_root(
    catalog_path: &Path,
    trust_root_hex: &str,
) -> anyhow::Result<()> {
    let repo = git2::Repository::open(catalog_path)
        .with_context(|| format!("open RELAY_MASTER_CATALOG_PATH {}", catalog_path.display()))?;
    let trust = git2::Oid::from_str(trust_root_hex.trim()).context("RELAY_MASTER_TRUST_ROOT")?;
    let head_oid = repo
        .head()
        .context("catalog repo HEAD")?
        .peel_to_commit()
        .context("catalog HEAD peel")?
        .id();
    let base = repo.merge_base(trust, head_oid).with_context(|| {
        format!(
            "merge-base(trust_root, HEAD) for {}",
            catalog_path.display()
        )
    })?;
    if base != trust {
        anyhow::bail!(
            "RELAY_MASTER_TRUST_ROOT {} is not an ancestor of catalog HEAD {} — wrong relay-master checkout?",
            trust_root_hex.trim(),
            head_oid
        );
    }
    Ok(())
}

/// In-memory catalog loaded from `catalog/repos/*.json` when present.
pub fn load_catalog_index_from_master(catalog_path: &Path) -> anyhow::Result<CatalogIndex> {
    catalog_policy::load_catalog_index(catalog_path)
}

/// Build allowlist from `relay-master` checkout: unified `catalog/repos/` or legacy `relay-catalog.json`.
pub fn load_from_master_catalog(
    catalog_path: &Path,
    relay_server_id: &str,
) -> anyhow::Result<AuthorizedReposFile> {
    let repo = git2::Repository::open(catalog_path)
        .with_context(|| format!("open catalog {}", catalog_path.display()))?;
    let head_ref = repo.head().context("catalog HEAD")?;
    let tip = head_ref
        .peel_to_commit()
        .context("catalog HEAD commit")?
        .id()
        .to_string();
    let branch = head_ref
        .symbolic_target()
        .map(|t| t.strip_prefix("refs/heads/").unwrap_or(t).to_string())
        .or_else(|| std::env::var("RELAY_MASTER_BRANCH").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_string());

    let mut repos = HashMap::new();
    repos.insert(
        "relay-master".to_string(),
        RepoAnchor {
            anchor_commit: tip,
            branch: branch.clone(),
            description: Some(
                "Canonical catalog for Relay deployments: which repositories a node may mirror, where to clone them, and the trusted root commit for each.".into(),
            ),
        },
    );

    if catalog_path.join("catalog/repos").is_dir() {
        let index = catalog_policy::load_catalog_index(catalog_path)?;
        for (name, entry) in index.repos {
            let root = entry.relay_root_commit.trim().to_string();
            if root.len() != 40 || !root.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!(
                    "catalog/repos/{nm}: relay_root_commit must be 40 hex chars",
                    nm = name
                );
            }
            repos.insert(
                name,
                RepoAnchor {
                    anchor_commit: root,
                    branch: entry.publish_branch,
                    description: nonempty_description(entry.description),
                },
            );
        }
        return Ok(AuthorizedReposFile {
            relay_server_id: Some(relay_server_id.to_string()),
            repos,
        });
    }

    let catalog_json = catalog_path.join("relay-catalog.json");
    if !catalog_json.is_file() {
        anyhow::bail!(
            "RELAY_MASTER_CATALOG_PATH {} has no catalog/repos/ or relay-catalog.json",
            catalog_path.display()
        );
    }

    let s = std::fs::read_to_string(&catalog_json).context("read relay-catalog.json")?;
    let cat: RelayCatalogFile = serde_json::from_str(&s).context("parse relay-catalog.json")?;

    for (name, entry) in cat.repos {
        let root = entry.relay_root_commit.trim().to_string();
        if root.len() != 40 || !root.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!(
                "relay-catalog.json repos.{nm}: relayRootCommit must be 40 hex chars",
                nm = name
            );
        }
        repos.insert(
            name,
            RepoAnchor {
                anchor_commit: root,
                branch: entry.branch,
                description: None,
            },
        );
    }

    Ok(AuthorizedReposFile {
        relay_server_id: Some(relay_server_id.to_string()),
        repos,
    })
}

/// Lookup per-repo catalog record (unified layout only).
pub fn catalog_repo_record(
    catalog_path: &Path,
    repo_slug: &str,
) -> anyhow::Result<Option<CatalogRepoRecord>> {
    if !catalog_path.join("catalog/repos").is_dir() {
        return Ok(None);
    }
    let index = catalog_policy::load_catalog_index(catalog_path)?;
    Ok(index.repos.get(repo_slug).cloned())
}

/// When allowlist is active: repo must be listed (server id checked at startup).
pub fn ensure_pull_allowed(
    cfg: &AuthorizedReposFile,
    _relay_server_id: Option<&str>,
    repo_name: &str,
) -> Result<(), String> {
    if cfg.repos.is_empty() {
        return Err(
            "authorized-repos file has no repos — configure at least one repo entry".into(),
        );
    }
    cfg.repos
        .get(repo_name)
        .ok_or_else(|| format!("repo '{}' is not in authorized-repos list", repo_name))?;
    Ok(())
}

/// After fetch: branch tip must contain anchor_commit in its history.
pub fn validate_anchor(
    repo: &git2::Repository,
    repo_name: &str,
    cfg: &AuthorizedReposFile,
) -> Result<(), String> {
    let anchor = cfg
        .repos
        .get(repo_name)
        .ok_or_else(|| format!("repo '{}' missing from allowlist", repo_name))?;
    let anchor_oid = git2::Oid::from_str(anchor.anchor_commit.trim())
        .map_err(|e| format!("bad anchor_commit: {}", e))?;
    let refname = format!("refs/heads/{}", anchor.branch);
    let head_oid = repo
        .refname_to_id(&refname)
        .map_err(|_| format!("{}:{} missing after pull", repo_name, anchor.branch))?;
    let base = repo
        .merge_base(anchor_oid, head_oid)
        .map_err(|e| format!("merge-base: {}", e))?;
    if base != anchor_oid {
        return Err(format!(
            "trust anchor {} not ancestor of {}:{} — pull rejected",
            anchor.anchor_commit, repo_name, anchor.branch
        ));
    }
    Ok(())
}

/// Roll back refs/heads/main to previous tip (best effort).
pub fn rollback_main(repo: &git2::Repository, prev: Option<&str>) {
    rollback_branch(repo, "main", prev);
}

/// Roll back `refs/heads/{branch}` to previous tip (best effort).
pub fn rollback_branch(repo: &git2::Repository, branch: &str, prev: Option<&str>) {
    let refname = format!("refs/heads/{}", branch);
    match prev {
        Some(s) => {
            if let Ok(oid) = git2::Oid::from_str(s.trim()) {
                let _ = repo.reference(&refname, oid, true, "relay trust rollback");
            }
        }
        None => {
            if let Ok(mut r) = repo.find_reference(&refname) {
                let _ = r.delete();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn anchor_must_be_ancestor_of_head() {
        let td = tempdir().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let t1 = tb.write().unwrap();
        let tree1 = repo.find_tree(t1).unwrap();
        let c1 = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "1", &tree1, &[])
            .unwrap();
        let p1 = repo.find_commit(c1).unwrap();
        let mut tb2 = repo.treebuilder(None).unwrap();
        let f = repo.blob(b"x").unwrap();
        tb2.insert("f", f, 0o100644).unwrap();
        let t2 = tb2.write().unwrap();
        let tree2 = repo.find_tree(t2).unwrap();
        let c2 = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "2", &tree2, &[&p1])
            .unwrap();

        let mut cfg = AuthorizedReposFile {
            relay_server_id: None,
            repos: HashMap::new(),
        };
        cfg.repos.insert(
            "r".into(),
            RepoAnchor {
                anchor_commit: c1.to_string(),
                branch: "main".into(),
                description: None,
            },
        );
        assert!(validate_anchor(&repo, "r", &cfg).is_ok());

        cfg.repos.get_mut("r").unwrap().anchor_commit = c2.to_string();
        assert!(validate_anchor(&repo, "r", &cfg).is_ok());

        let orphan = repo.blob(b"y").unwrap();
        let mut tb3 = repo.treebuilder(None).unwrap();
        tb3.insert("y", orphan, 0o100644).unwrap();
        let t3 = tb3.write().unwrap();
        let tree3 = repo.find_tree(t3).unwrap();
        let evil = repo.commit(None, &sig, &sig, "evil", &tree3, &[]).unwrap();
        repo.reference("refs/heads/main", evil, true, "m").unwrap();
        cfg.repos.get_mut("r").unwrap().anchor_commit = c1.to_string();
        assert!(validate_anchor(&repo, "r", &cfg).is_err());
    }

    #[test]
    fn master_catalog_trust_root_rejects_wrong_lineage() {
        let td = tempdir().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let t1 = tb.write().unwrap();
        let tree1 = repo.find_tree(t1).unwrap();
        let good_root = repo
            .commit(Some("refs/heads/master"), &sig, &sig, "root", &tree1, &[])
            .unwrap();

        std::fs::write(
            td.path().join("relay-catalog.json"),
            format!(
                r#"{{
  "repos": {{
    "child": {{
      "url": "https://example.com/child.git",
      "relayRootCommit": "{}",
      "branch": "master"
    }}
  }}
}}"#,
                good_root
            ),
        )
        .unwrap();

        let ar = load_from_master_catalog(td.path(), "relay-test").unwrap();
        assert!(ar.repos.contains_key("relay-master"));
        assert!(ar.repos.contains_key("child"));

        verify_master_catalog_trust_root(td.path(), &good_root.to_string()).unwrap();

        let tb2 = repo.treebuilder(None).unwrap();
        let t2 = tb2.write().unwrap();
        let tree2 = repo.find_tree(t2).unwrap();
        let other = repo
            .commit(None, &sig, &sig, "otherroot", &tree2, &[])
            .unwrap();
        assert!(verify_master_catalog_trust_root(td.path(), &other.to_string()).is_err());
    }

    #[test]
    fn load_from_unified_catalog_repos_dir() {
        let td = tempdir().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let t1 = tb.write().unwrap();
        let tree1 = repo.find_tree(t1).unwrap();
        repo.reference(
            "refs/heads/main",
            repo.commit(None, &sig, &sig, "tip", &tree1, &[]).unwrap(),
            true,
            "test",
        )
        .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap().id();

        let repos_dir = td.path().join("catalog/repos");
        std::fs::create_dir_all(&repos_dir).unwrap();
        std::fs::write(
            repos_dir.join("sample-repo.json"),
            format!(
                r#"{{
  "schema": "relay.catalog-repo.v1",
  "repo": "sample-repo",
  "tier": "primary",
  "forge_url": "https://example.com/sample.git",
  "relay_root_commit": "{head}",
  "publish_branch": "main",
  "publish_origin": "dfw1.relaygateway.net",
  "signing_keys_source": "repo",
  "description": "Sample allowlisted repo."
}}"#
            ),
        )
        .unwrap();

        let ar = load_from_master_catalog(td.path(), "relay-test").unwrap();
        assert!(ar.repos.contains_key("relay-master"));
        let sample = ar.repos.get("sample-repo").unwrap();
        assert_eq!(sample.anchor_commit, head.to_string());
        assert_eq!(sample.branch, "main");
        assert_eq!(
            sample.description.as_deref(),
            Some("Sample allowlisted repo.")
        );
    }
}

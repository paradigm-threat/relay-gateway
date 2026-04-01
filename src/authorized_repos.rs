//! Authorized repo list + anchor commit validation (pull today; push/planned).
//!
//! Two sources:
//! - **`RELAY_MASTER_CATALOG_PATH`**: open a **`relay-master`** worktree (`.gitmodules` + `HEAD`);
//!   optional **`RELAY_MASTER_TRUST_ROOT`** pins the catalog to an expected **root** commit so the
//!   wrong repository cannot be mounted at that path.
//! - **`RELAY_AUTHORIZED_REPOS_PATH`**: YAML file (e.g. emitted by **`sync-relay-catalog.sh`**).
//!
//! See `docs/RELAY_TRUST_AND_BOOTSTRAP.md`.
use anyhow::Context;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

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
}

fn default_branch() -> String {
    "main".to_string()
}

pub fn load_from_path(path: &Path) -> anyhow::Result<AuthorizedReposFile> {
    let s = std::fs::read_to_string(path)?;
    let cfg: AuthorizedReposFile = serde_yaml::from_str(&s)?;
    Ok(cfg)
}

/// Ensures `trust_root` is an ancestor of the catalog checkout `HEAD` (same lineage as
/// submodule `relayRootCommit` checks). Use the canonical **root** of `relay-master`:
/// `git rev-list --max-parents=0 main` (pick the policy root if there are several).
pub fn verify_master_catalog_trust_root(catalog_path: &Path, trust_root_hex: &str) -> anyhow::Result<()> {
    let repo = git2::Repository::open(catalog_path)
        .with_context(|| format!("open RELAY_MASTER_CATALOG_PATH {}", catalog_path.display()))?;
    let trust = git2::Oid::from_str(trust_root_hex.trim()).context("RELAY_MASTER_TRUST_ROOT")?;
    let head_oid = repo
        .head()
        .context("catalog repo HEAD")?
        .peel_to_commit()
        .context("catalog HEAD peel")?
        .id();
    let base = repo
        .merge_base(trust, head_oid)
        .with_context(|| format!("merge-base(trust_root, HEAD) for {}", catalog_path.display()))?;
    if base != trust {
        anyhow::bail!(
            "RELAY_MASTER_TRUST_ROOT {} is not an ancestor of catalog HEAD {} — wrong relay-master checkout?",
            trust_root_hex.trim(),
            head_oid
        );
    }
    Ok(())
}

/// Build allowlist from `relay-master` checkout: `.gitmodules` submodules + synthetic `relay-master` row from `HEAD`.
pub fn load_from_master_catalog(catalog_path: &Path, relay_server_id: &str) -> anyhow::Result<AuthorizedReposFile> {
    let gitmodules = catalog_path.join(".gitmodules");
    if !gitmodules.is_file() {
        anyhow::bail!(
            "RELAY_MASTER_CATALOG_PATH {} has no .gitmodules",
            catalog_path.display()
        );
    }

    let mut gmcfg = git2::Config::new()?;
    gmcfg.add_file(&gitmodules, git2::ConfigLevel::Local, false)?;

    let mut submodule_names = BTreeSet::new();
    let mut entries = gmcfg.entries(None).context("read .gitmodules")?;
    while let Some(entry) = entries.next() {
        let e = entry.context("gitmodules entry")?;
        let Some(name) = e.name() else { continue };
        let Some(rest) = name.strip_prefix("submodule.") else { continue };
        let Some(sub) = rest.strip_suffix(".path") else { continue };
        submodule_names.insert(sub.to_string());
    }

    let repo = git2::Repository::open(catalog_path)
        .with_context(|| format!("open catalog {}", catalog_path.display()))?;
    let head_ref = repo.head().context("catalog HEAD")?;
    let tip = head_ref.peel_to_commit().context("catalog HEAD commit")?.id().to_string();
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
        },
    );

    for name in submodule_names {
        let path_key = format!("submodule.{name}.path");
        let url_key = format!("submodule.{name}.url");
        let root_key = format!("submodule.{name}.relayRootCommit");
        let branch_key = format!("submodule.{name}.branch");

        let _path: String = gmcfg
            .get_string(&path_key)
            .with_context(|| format!("{path_key} missing in .gitmodules"))?;
        let _url: String = gmcfg
            .get_string(&url_key)
            .with_context(|| format!("{url_key} missing in .gitmodules"))?;
        let root: String = gmcfg
            .get_string(&root_key)
            .with_context(|| format!("{root_key} missing in .gitmodules (set relayRootCommit)"))?;
        let root = root.trim().to_string();
        if root.len() != 40 || !root.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("{root_key} must be 40 hex chars");
        }
        let sub_branch = gmcfg
            .get_string(&branch_key)
            .unwrap_or_else(|_| "main".to_string());

        repos.insert(
            name,
            RepoAnchor {
                anchor_commit: root,
                branch: sub_branch,
            },
        );
    }

    if repos.len() <= 1 {
        anyhow::bail!("catalog has no submodules in .gitmodules (only relay-master row)");
    }

    Ok(AuthorizedReposFile {
        relay_server_id: Some(relay_server_id.to_string()),
        repos,
    })
}

/// When allowlist is active: repo must be listed (server id checked at startup).
pub fn ensure_pull_allowed(cfg: &AuthorizedReposFile, _relay_server_id: Option<&str>, repo_name: &str) -> Result<(), String> {
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
pub fn validate_anchor(repo: &git2::Repository, repo_name: &str, cfg: &AuthorizedReposFile) -> Result<(), String> {
    let anchor = cfg
        .repos
        .get(repo_name)
        .ok_or_else(|| format!("repo '{}' missing from allowlist", repo_name))?;
    let anchor_oid =
        git2::Oid::from_str(anchor.anchor_commit.trim()).map_err(|e| format!("bad anchor_commit: {}", e))?;
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
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "2",
                &tree2,
                &[&p1],
            )
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
        let evil = repo
            .commit(None, &sig, &sig, "evil", &tree3, &[])
            .unwrap();
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
            td.path().join(".gitmodules"),
            format!(
                r#"[submodule "child"]
	path = repos/child
	url = https://example.com/child.git
	relayRootCommit = {}
"#,
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
}

//! Apply relay-master catalog tip changes: remove/reclone bare mirrors under RELAY_REPO_PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use tracing::{error, info, warn};

use crate::git::hooks::HookContext;
use crate::git::{branch_operation_mutex, ensure_indexed, has_index_hook, read_relay_config};
use crate::notify;
use crate::peer_notify;

const ZERO_OID: &str = "0000000000000000000000000000000000000000";
const CATALOG_RELAY_MASTER: &str = "relay-master";

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct CatalogRepoEntry {
    url: String,
    #[serde(rename = "relayRootCommit")]
    relay_root_commit: String,
    branch: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct CatalogFile {
    #[serde(default)]
    repos: std::collections::HashMap<String, CatalogRepoEntry>,
}

pub fn data_root() -> PathBuf {
    std::env::var("RELAY_REPO_PATH")
        .or_else(|_| std::env::var("RELAY_DATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/relay/data"))
}

fn read_catalog_at(repo_path: &Path, commit: &str) -> Option<CatalogFile> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("show")
        .arg(format!("{}:relay-catalog.json", commit))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn bare_path(root: &Path, slug: &str) -> PathBuf {
    root.join(format!("{}.git", slug))
}

fn remove_bare(root: &Path, slug: &str) {
    let p = bare_path(root, slug);
    if p.exists() {
        info!(path = %p.display(), "catalog apply: removing bare");
        if let Err(e) = std::fs::remove_dir_all(&p) {
            warn!(error = %e, path = %p.display(), "catalog apply: rm failed");
        }
    }
}

fn verify_commit_exists(bare: &Path, oid: &str) -> bool {
    Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .arg("cat-file")
        .arg("-e")
        .arg(format!("{}^{{commit}}", oid))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn clone_bare(url: &str, dest: &Path, depth: Option<u32>) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| format!("rm before clone: {}", e))?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--bare");
    if let Some(d) = depth.filter(|&x| x > 0) {
        cmd.arg("--depth").arg(d.to_string());
    }
    cmd.arg(url).arg(dest);
    let out = cmd.output().map_err(|e| format!("git clone: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn deepen_if_needed(bare: &Path, root_commit: &str) -> Result<(), String> {
    if verify_commit_exists(bare, root_commit) {
        return Ok(());
    }
    let deepen = std::env::var("RELAY_CATALOG_AUTO_DEEPEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256u32);
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .arg("fetch")
        .arg("--deepen")
        .arg(deepen.to_string())
        .arg("origin")
        .output()
        .map_err(|e| format!("deepen fetch: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "deepen fetch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if !verify_commit_exists(bare, root_commit) {
        return Err(format!(
            "relayRootCommit {} still missing after deepen",
            root_commit
        ));
    }
    Ok(())
}

fn set_branch_head(bare: &Path, branch: &str, tip: &str) -> Result<(), String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .arg("update-ref")
        .arg(format!("refs/heads/{}", branch))
        .arg(tip)
        .output()
        .map_err(|e| format!("update-ref: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "update-ref failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn warm_index_if_needed(repo_path: &PathBuf, branch: &str, tip: &str) {
    let ctx = HookContext {
        repo_path: repo_path.clone(),
        old_commit: ZERO_OID.to_string(),
        new_commit: tip.to_string(),
        refname: format!("refs/heads/{}", branch),
        branch: branch.to_string(),
        is_verified: true,
        files: Default::default(),
    };
    if let Ok(repo) = git2::Repository::open_bare(repo_path) {
        if let Some(cfg) = read_relay_config(&repo, tip) {
            if has_index_hook(&cfg) {
                let lock = branch_operation_mutex(repo_path.clone(), branch.to_string());
                let _guard = lock.lock().unwrap();
                if let Err(e) = ensure_indexed(&ctx) {
                    warn!(error = %e, "catalog apply: ensure_indexed failed");
                }
            }
        }
    }
}

/// Apply catalog diff after relay-master tip move. Returns slugs touched.
pub fn apply_catalog_tip_change(
    relay_master_bare: &Path,
    old_commit: &str,
    new_commit: &str,
    branch: &str,
) -> Result<Vec<String>, String> {
    if new_commit == ZERO_OID || new_commit.len() < 40 {
        return Ok(vec![]);
    }
    let old_cat = if old_commit == ZERO_OID || old_commit.len() < 40 {
        CatalogFile::default()
    } else {
        read_catalog_at(relay_master_bare, old_commit).unwrap_or_default()
    };
    let new_cat = read_catalog_at(relay_master_bare, new_commit)
        .ok_or_else(|| "relay-catalog.json missing at new tip".to_string())?;

    let root = data_root();
    let depth = std::env::var("RELAY_CATALOG_CLONE_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .or(Some(1u32));

    let mut touched = Vec::new();

    for slug in old_cat.repos.keys() {
        if !new_cat.repos.contains_key(slug) {
            remove_bare(&root, slug);
            touched.push(slug.clone());
        }
    }

    for (slug, entry) in &new_cat.repos {
        let needs_reclone = match old_cat.repos.get(slug) {
            None => true,
            Some(old) => old != entry,
        };
        if !needs_reclone {
            continue;
        }
        info!(%slug, url = %entry.url, "catalog apply: recloning bare");
        remove_bare(&root, slug);
        let dest = bare_path(&root, slug);
        clone_bare(&entry.url, &dest, depth)?;
        deepen_if_needed(&dest, &entry.relay_root_commit)?;
        set_branch_head(&dest, &entry.branch, &entry.relay_root_commit)?;
        warm_index_if_needed(&dest, &entry.branch, &entry.relay_root_commit);
        touched.push(slug.clone());
    }

    notify::spawn_slack_text(format!(
        "[Relay] catalog_apply\nbranch: {}\nold: {}\nnew: {}\ntouched: {}",
        branch,
        &old_commit[..12.min(old_commit.len())],
        &new_commit[..12.min(new_commit.len())],
        touched.join(", ")
    ));

    Ok(touched)
}

/// Post-receive path for relay-master.git: apply catalog + mesh notify relay-master tip.
pub fn handle_relay_master_post_receive(ctx: &HookContext) {
    if let Err(e) = apply_catalog_tip_change(
        &ctx.repo_path,
        &ctx.old_commit,
        &ctx.new_commit,
        &ctx.branch,
    ) {
        error!("catalog apply failed: {}", e);
        notify::spawn_slack_text(format!(
            "[Relay] catalog_apply FAILED\n{}\nold: {}\nnew: {}",
            e, ctx.old_commit, ctx.new_commit
        ));
    }

    if let Ok(repo) = git2::Repository::open_bare(&ctx.repo_path) {
        if let Some(cfg) = read_relay_config(&repo, &ctx.new_commit) {
            peer_notify::notify_after_push(CATALOG_RELAY_MASTER, &ctx.branch, &ctx.new_commit, &cfg);
        }
    }
}

/// After peer-fetch updated relay-master tip.
pub fn handle_relay_master_peer_fetch(
    relay_master_bare: &Path,
    before: Option<&str>,
    after: &str,
    branch: &str,
    via: &[String],
    hops_remaining: Option<u32>,
) {
    let old = before.unwrap_or(ZERO_OID);
    if let Err(e) = apply_catalog_tip_change(relay_master_bare, old, after, branch) {
        warn!(error = %e, "catalog apply after peer-fetch failed");
    }
    if let Some(git) = peer_notify::git_config_from_bare_tip(relay_master_bare, branch, after) {
        let mut next_via = via.to_vec();
        if let Some(self_host) = crate::mesh_role::node_fqdn_normalized() {
            next_via.push(self_host);
        }
        let next_hops = hops_remaining.map(|h| h.saturating_sub(1));
        if next_hops.unwrap_or(0) > 0 {
            peer_notify::spawn_hop_forward(
                CATALOG_RELAY_MASTER.to_string(),
                branch.to_string(),
                after.to_string(),
                git,
                next_via,
                next_hops,
            );
        }
    }
}

pub fn is_relay_master_slug(slug: &str) -> bool {
    slug == CATALOG_RELAY_MASTER || slug == "relay-master.git"
}

pub fn repo_slug_from_path(repo_path: &Path) -> String {
    repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .trim_end_matches(".git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use serial_test::serial;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn catalog_entry_equality() {
        let a = CatalogRepoEntry {
            url: "https://x.git".into(),
            relay_root_commit: "abc".into(),
            branch: "main".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn is_relay_master_slug_variants() {
        assert!(is_relay_master_slug("relay-master"));
        assert!(is_relay_master_slug("relay-master.git"));
        assert!(!is_relay_master_slug("paradigm-threat-files"));
    }

    fn commit_file(
        repo: &Repository,
        parent: Option<&str>,
        path: &str,
        content: &[u8],
        branch: &str,
        message: &str,
    ) -> String {
        let sig = Signature::now("test", "test@test").unwrap();
        let blob = repo.blob(content).unwrap();
        let parent_tree = parent.and_then(|p| {
            repo.revparse_single(p)
                .ok()?
                .peel_to_commit()
                .ok()
                .and_then(|c| c.tree().ok())
        });
        let mut tb = repo.treebuilder(parent_tree.as_ref()).unwrap();
        tb.insert(path, blob, 0o100644).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let parents: Vec<git2::Commit> = parent
            .and_then(|p| repo.revparse_single(p).ok()?.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let oid = repo
            .commit(
                Some(&format!("refs/heads/{branch}")),
                &sig,
                &sig,
                message,
                &tree,
                &parent_refs,
            )
            .unwrap();
        oid.to_string()
    }

    fn source_bare_with_main_commit(dir: &std::path::Path) -> (std::path::PathBuf, String) {
        let path = dir.join("source.git");
        let repo = Repository::init_bare(&path).unwrap();
        let tip = commit_file(
            &repo,
            None,
            "README.md",
            b"fixture",
            "main",
            "init",
        );
        (path, tip)
    }

    #[test]
    #[serial]
    fn apply_catalog_adds_and_removes_bare_repo() {
        let prev_repo_path = std::env::var("RELAY_REPO_PATH").ok();
        let prev_depth = std::env::var("RELAY_CATALOG_CLONE_DEPTH").ok();

        let workspace = tempdir().unwrap();
        let data_root = workspace.path().join("data");
        std::fs::create_dir_all(&data_root).unwrap();
        let (source_path, source_tip) = source_bare_with_main_commit(workspace.path());
        let source_url = format!("file://{}", source_path.display());

        let master_bare = workspace.path().join("relay-master.git");
        let master = Repository::init_bare(&master_bare).unwrap();

        let old_catalog = r#"{"repos":{}}"#;
        let old_tip = commit_file(
            &master,
            None,
            "relay-catalog.json",
            old_catalog.as_bytes(),
            "main",
            "empty catalog",
        );

        let new_catalog = format!(
            r#"{{
  "repos": {{
    "fixture-repo": {{
      "url": "{source_url}",
      "relayRootCommit": "{source_tip}",
      "branch": "main"
    }}
  }}
}}"#
        );
        let new_tip = commit_file(
            &master,
            Some(&old_tip),
            "relay-catalog.json",
            new_catalog.as_bytes(),
            "main",
            "add fixture-repo",
        );

        unsafe {
            std::env::set_var("RELAY_REPO_PATH", &data_root);
            std::env::set_var("RELAY_CATALOG_CLONE_DEPTH", "0");
        }

        let touched = apply_catalog_tip_change(&master_bare, &old_tip, &new_tip, "main")
            .expect("apply should succeed");
        assert_eq!(touched, vec!["fixture-repo".to_string()]);

        let dest = data_root.join("fixture-repo.git");
        assert!(dest.is_dir(), "bare mirror should exist");
        let tip_out = Command::new("git")
            .args([
                "--git-dir",
                dest.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ])
            .output()
            .unwrap();
        assert!(tip_out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&tip_out.stdout).trim(),
            source_tip
        );

        let removed_catalog = r#"{"repos":{}}"#;
        let removed_tip = commit_file(
            &master,
            Some(&new_tip),
            "relay-catalog.json",
            removed_catalog.as_bytes(),
            "main",
            "remove fixture-repo",
        );
        let touched2 =
            apply_catalog_tip_change(&master_bare, &new_tip, &removed_tip, "main").unwrap();
        assert_eq!(touched2, vec!["fixture-repo".to_string()]);
        assert!(!dest.exists(), "bare mirror should be removed");

        unsafe {
            match prev_repo_path {
                Some(v) => std::env::set_var("RELAY_REPO_PATH", v),
                None => std::env::remove_var("RELAY_REPO_PATH"),
            }
            match prev_depth {
                Some(v) => std::env::set_var("RELAY_CATALOG_CLONE_DEPTH", v),
                None => std::env::remove_var("RELAY_CATALOG_CLONE_DEPTH"),
            }
        }
    }
}

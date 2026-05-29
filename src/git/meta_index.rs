//! Rust-side `meta.yaml` / `meta.yml` indexing into `index.db.json` (replaces Node `index.mjs` upsert path).

use crate::git::hooks::HookContext;
use crate::git::branch_index_paths;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

fn git_paths_changed(repo: &Path, old: &str, new: &str) -> Result<Vec<String>, String> {
    let out = if old.is_empty() || old == "0000000000000000000000000000000000000000" {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("ls-tree")
            .arg("-r")
            .arg("--name-only")
            .arg(new)
            .output()
            .map_err(|e| e.to_string())?
    } else {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("diff")
            .arg("--name-only")
            .arg(format!("{}..{}", old, new))
            .output()
            .map_err(|e| e.to_string())?
    };
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

fn git_show(repo: &Path, commit: &str, path: &str) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{}:{}", commit, path))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// Sync `collections.index` from `meta.yaml` / `meta.yml` under `data/**` for commits in `HookContext`.
pub fn sync_meta_yaml_index(ctx: &HookContext) -> anyhow::Result<()> {
    let (db_path, _) = branch_index_paths(&ctx.repo_path, &ctx.branch);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut db: Value = if db_path.exists() {
        let t = std::fs::read_to_string(&db_path)?;
        serde_json::from_str(&t).unwrap_or_else(|_| {
            json!({
                "metadata": { "indexed_head": null },
                "collections": { "index": [] }
            })
        })
    } else {
        json!({
            "metadata": { "indexed_head": null },
            "collections": { "index": [] }
        })
    };

    let arr = db
        .pointer_mut("/collections/index")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("index.db.json missing collections.index array"))?;

    let paths = if !ctx.files.is_empty() {
        ctx.files.keys().cloned().collect::<Vec<_>>()
    } else {
        git_paths_changed(&ctx.repo_path, &ctx.old_commit, &ctx.new_commit)
            .map_err(|e| anyhow::anyhow!("git diff: {}", e))?
    };

    for path in paths {
        let is_meta = path.ends_with("meta.yaml") || path.ends_with("meta.yml");
        if !is_meta {
            continue;
        }
        if !path.starts_with("data/") {
            continue;
        }

        let meta_dir = Path::new(&path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".");

        // Remove existing docs for this meta dir
        arr.retain(|item| {
            item.get("_meta_dir")
                .and_then(|v| v.as_str())
                .map(|d| d != meta_dir)
                .unwrap_or(true)
        });

        if let Some(bytes) = git_show(&ctx.repo_path, &ctx.new_commit, &path) {
            let text = String::from_utf8_lossy(&bytes);
            let yaml_val: serde_yaml::Value =
                serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("yaml {}: {}", path, e))?;
            let mut doc = serde_json::to_value(&yaml_val)?;
            if let Value::Object(ref mut m) = doc {
                m.insert(
                    "_branch".to_string(),
                    Value::String(ctx.branch.clone()),
                );
                m.insert("_meta_dir".to_string(), Value::String(meta_dir.to_string()));
                m.insert(
                    "_updated_at".to_string(),
                    Value::String(chrono_now_iso()),
                );
            }
            arr.push(doc);
        }
    }

    if let Some(meta) = db.get_mut("metadata").and_then(|v| v.as_object_mut()) {
        meta.insert(
            "indexed_head".to_string(),
            Value::String(ctx.new_commit.clone()),
        );
    }

    std::fs::write(&db_path, serde_json::to_string_pretty(&db)?)?;
    Ok(())
}

fn chrono_now_iso() -> String {
    use std::time::SystemTime;
    let Ok(d) = SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return String::new();
    };
    format!("{}", d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn bare_repo_with_meta(yaml: &str) -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let meta_oid = repo.blob(yaml.as_bytes()).unwrap();
        let mut tb_item = repo.treebuilder(None).unwrap();
        tb_item.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let item_tid = tb_item.write().unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("item", item_tid, 0o040000).unwrap();
        let data_tid = tb_data.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("data", data_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, repo_path, oid.to_string())
    }

    #[test]
    fn sync_meta_writes_index_and_metadata() {
        let (dir, repo_path, commit) = bare_repo_with_meta("title: Indexed Title\n");
        let ctx = HookContext {
            repo_path: repo_path.clone(),
            old_commit: "0000000000000000000000000000000000000000".into(),
            new_commit: commit.clone(),
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        sync_meta_yaml_index(&ctx).unwrap();
        let (db_path, _) = branch_index_paths(&repo_path, "main");
        assert!(db_path.exists());
        let db: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&db_path).unwrap()).unwrap();
        assert_eq!(db["metadata"]["indexed_head"].as_str(), Some(commit.as_str()));
        let arr = db["collections"]["index"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["title"], "Indexed Title");
        assert_eq!(arr[0]["_meta_dir"], "data/item");
        assert_eq!(arr[0]["_branch"], "main");
        drop(dir);
    }

    #[test]
    fn sync_meta_skips_meta_outside_data() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let meta_oid = repo.blob(b"title: Root\n").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string();
        let ctx = HookContext {
            repo_path,
            old_commit: String::new(),
            new_commit: commit,
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        sync_meta_yaml_index(&ctx).unwrap();
        let (db_path, _) = branch_index_paths(&ctx.repo_path, "main");
        let db: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&db_path).unwrap()).unwrap();
        let arr = db["collections"]["index"].as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn sync_meta_invalid_yaml_errors() {
        let (dir, repo_path, commit) = bare_repo_with_meta("::: not yaml :::\n");
        let ctx = HookContext {
            repo_path,
            old_commit: String::new(),
            new_commit: commit,
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        let err = sync_meta_yaml_index(&ctx).unwrap_err();
        assert!(
            err.to_string().contains("yaml"),
            "expected yaml error, got {}",
            err
        );
        drop(dir);
    }

    #[test]
    fn sync_meta_rejects_malformed_index_db() {
        let (dir, repo_path, commit) = bare_repo_with_meta("title: A\n");
        let (db_path, _) = branch_index_paths(&repo_path, "main");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, r#"{"collections":{"index":"bad"}}"#).unwrap();
        let ctx = HookContext {
            repo_path,
            old_commit: String::new(),
            new_commit: commit,
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        let err = sync_meta_yaml_index(&ctx).unwrap_err();
        assert!(err.to_string().contains("collections.index"));
        drop(dir);
    }
}

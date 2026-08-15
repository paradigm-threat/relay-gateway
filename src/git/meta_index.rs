//! Rust-side `meta.yaml` / `meta.yml` indexing into `index.db.json` (replaces Node `index.mjs` upsert path).
//! Also ingests catalog `index.json` / timeline `data/events.json` into SQLite for the default search plugin.

use crate::git::hooks::HookContext;
use crate::git::branch_index_paths;
use crate::git::search_store::{store_for_bare_repo, store_key_for_bare, SearchStore, StoreKey};
use crate::git::sqlite_index;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

fn is_full_rebuild(old: &str) -> bool {
    old.is_empty() || old.chars().all(|c| c == '0') || old == ZERO_OID
}

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

    let store = store_for_bare_repo(&ctx.repo_path)?;
    let key = store_key_for_bare(&ctx.repo_path, &ctx.branch);
    if let Some(map) = db.get("collections").and_then(|c| c.as_object()) {
        for (name, val) in map {
            let docs = match val {
                Value::Array(a) => a.clone(),
                _ => continue,
            };
            store.replace_collection(&key, name, &docs, None, None, None)?;
        }
    }
    store.set_docs_indexed_head(&key, &ctx.new_commit)?;
    ingest_catalog_json_indexes(ctx, store.as_ref(), &key)?;

    Ok(())
}

fn apply_collection_docs(
    store: &dyn SearchStore,
    key: &StoreKey,
    collection: &str,
    docs: &[Value],
    commit: &str,
    mode: crate::git::search_config::IngestMode,
    id_f: Option<&[String]>,
    fts_f: Option<&[String]>,
) -> anyhow::Result<()> {
    use crate::git::search_config::IngestMode;
    if docs.is_empty() {
        return Ok(());
    }
    match mode {
        IngestMode::Merge => store.merge_collection(key, collection, docs, id_f, fts_f, Some(commit))?,
        IngestMode::Replace => {
            store.replace_collection(key, collection, docs, id_f, fts_f, Some(commit))?
        }
    }
    Ok(())
}

/// Ingest catalog sources from `search.yaml` (JSON blobs and/or tip-tree builders).
pub fn ingest_catalog_json_indexes(
    ctx: &HookContext,
    store: &dyn SearchStore,
    key: &StoreKey,
) -> anyhow::Result<()> {
    use crate::git::catalog_build::{
        build_content_events_docs, build_path_tree_docs_for_paths, build_path_tree_docs_scoped,
    };
    use crate::git::repo::read_relay_config;
    use crate::git::search_config::{self, SearchSource};

    let repo = git2::Repository::open_bare(&ctx.repo_path).ok();
    let relay = repo
        .as_ref()
        .and_then(|r| read_relay_config(r, &ctx.new_commit))
        .unwrap_or_default();
    let cfg = search_config::load_search_config(&ctx.repo_path, &ctx.new_commit, &relay, None);

    let incremental = !is_full_rebuild(&ctx.old_commit);
    let changed = if incremental {
        git_paths_changed(&ctx.repo_path, &ctx.old_commit, &ctx.new_commit).ok()
    } else {
        None
    };

    let mut any = false;
    for (coll_name, coll) in &cfg.collections {
        let id_f = coll.id_fields.as_deref();
        let fts_f = coll.fts_fields.as_deref();
        store.ensure_schema(
            key,
            coll_name,
            &crate::git::search_store::CollectionSchema {
                fts_fields: crate::git::search_store::effective_fts_fields(fts_f),
                filter_fields: crate::git::search_store::effective_filter_fields(
                    coll.filter_fields.as_deref(),
                ),
                sort_fields: crate::git::search_store::effective_sort_fields(
                    coll.sort_fields.as_deref(),
                ),
            },
        )?;
        for src in &coll.sources {
            match src {
                SearchSource::MetaYaml { .. } => {}
                SearchSource::PathIndex { path, mode } => {
                    if let Some(bytes) = git_show(&ctx.repo_path, &ctx.new_commit, path) {
                        let v: Value = serde_json::from_slice(&bytes)
                            .map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
                        let docs = sqlite_index::flatten_path_index(&v);
                        apply_collection_docs(
                            store, key, coll_name, &docs, &ctx.new_commit, *mode, id_f, fts_f,
                        )?;
                        any = true;
                    }
                }
                SearchSource::EventsJson { path, mode } => {
                    if let Some(bytes) = git_show(&ctx.repo_path, &ctx.new_commit, path) {
                        let v: Value = serde_json::from_slice(&bytes)
                            .map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
                        let docs = flatten_timeline_events(&v);
                        apply_collection_docs(
                            store, key, coll_name, &docs, &ctx.new_commit, *mode, id_f, fts_f,
                        )?;
                        any = true;
                    }
                }
                SearchSource::PathTree { root, mode } => {
                    if incremental {
                        if let Some(ref paths) = changed {
                            let docs = build_path_tree_docs_for_paths(
                                &ctx.repo_path,
                                &ctx.new_commit,
                                root.as_deref(),
                                paths,
                            )
                            .map_err(|e| anyhow::anyhow!("path_tree: {e}"))?;
                            store.merge_collection(
                                key,
                                coll_name,
                                &docs,
                                id_f,
                                fts_f,
                                Some(&ctx.new_commit),
                            )?;
                            let gone: Vec<String> = paths
                                .iter()
                                .filter(|p| git_show(&ctx.repo_path, &ctx.new_commit, p).is_none())
                                .cloned()
                                .collect();
                            if !gone.is_empty() {
                                store.delete_docs_by_paths(key, coll_name, &gone)?;
                            }
                            any = true;
                        }
                    } else {
                        let docs = build_path_tree_docs_scoped(
                            &ctx.repo_path,
                            &ctx.new_commit,
                            root.as_deref(),
                        )
                        .map_err(|e| anyhow::anyhow!("path_tree: {e}"))?;
                        apply_collection_docs(
                            store, key, coll_name, &docs, &ctx.new_commit, *mode, id_f, fts_f,
                        )?;
                        any = true;
                    }
                }
                SearchSource::ContentEvents { root: _, mode } => {
                    let docs = build_content_events_docs(&ctx.repo_path, &ctx.new_commit)
                        .map_err(|e| anyhow::anyhow!("content_events: {e}"))?;
                    apply_collection_docs(
                        store, key, coll_name, &docs, &ctx.new_commit, *mode, id_f, fts_f,
                    )?;
                    any = true;
                }
            }
        }
    }

    if !any {
        if let Some(bytes) = git_show(&ctx.repo_path, &ctx.new_commit, "index.json") {
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|e| anyhow::anyhow!("index.json: {}", e))?;
            let docs = sqlite_index::flatten_path_index(&v);
            if !docs.is_empty() {
                store.replace_collection(key, "index", &docs, None, None, Some(&ctx.new_commit))?;
            }
        }
        if let Some(bytes) = git_show(&ctx.repo_path, &ctx.new_commit, "data/events.json") {
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|e| anyhow::anyhow!("data/events.json: {}", e))?;
            let docs = flatten_timeline_events(&v);
            if !docs.is_empty() {
                store.replace_collection(key, "events", &docs, None, None, Some(&ctx.new_commit))?;
            }
        }
    }

    Ok(())
}

fn flatten_timeline_events(root: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    fn walk(node: &Value, out: &mut Vec<Value>) {
        match node {
            Value::Array(arr) => {
                for n in arr {
                    walk(n, out);
                }
            }
            Value::Object(obj) => {
                // Root wrappers: { entries: [...], meta, ... }
                if let Some(entries) = obj.get("entries") {
                    walk(entries, out);
                    return;
                }
                let has_id = obj.get("id").and_then(|v| v.as_str()).is_some();
                let has_title = obj
                    .get("title")
                    .or_else(|| obj.get("name"))
                    .and_then(|v| v.as_str())
                    .is_some();
                if has_id || has_title {
                    out.push(Value::Object(obj.clone()));
                }
                if let Some(children) = obj.get("children") {
                    walk(children, out);
                }
            }
            _ => {}
        }
    }
    walk(root, &mut out);
    out
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

    #[test]
    fn ingest_index_json_into_sqlite() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let index_json = br#"{"blog":{"post.md":{"_title":"Portal Research","_keywords":["portal","july"]}}}"#;
        let index_oid = repo.blob(index_json).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("index.json", index_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string();
        let ctx = HookContext {
            repo_path: repo_path.clone(),
            old_commit: String::new(),
            new_commit: commit.clone(),
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        sync_meta_yaml_index(&ctx).unwrap();
        let store = store_for_bare_repo(&repo_path).unwrap();
        let key = store_key_for_bare(&repo_path, "main");
        let hits = store
            .query(
                &key,
                "index",
                Some(&serde_json::Value::String("portal".into())),
                None,
                None,
                crate::git::sqlite_index::QueryOrder::default(),
                &crate::git::search_store::effective_filter_fields(None),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["path"], "blog/post.md");
        assert_eq!(
            store.docs_indexed_head(&key).unwrap().as_deref(),
            Some(commit.as_str())
        );
    }

    #[test]
    fn ingest_events_json_entries_wrapper() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let events = br#"{"entries":[{"id":"evt-1","title":"The Blip","dates":[{"start":-670,"end":1053}],"children":[{"id":"evt-2","title":"Mud Flood","dates":[{"start":1811}]}]}],"meta":{}}"#;
        let events_oid = repo.blob(events).unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("events.json", events_oid, 0o100644).unwrap();
        let data_tid = tb_data.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("data", data_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string();
        let ctx = HookContext {
            repo_path: repo_path.clone(),
            old_commit: String::new(),
            new_commit: commit,
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        sync_meta_yaml_index(&ctx).unwrap();
        let store = store_for_bare_repo(&repo_path).unwrap();
        let key = store_key_for_bare(&repo_path, "main");
        let hits = store
            .query(
                &key,
                "events",
                Some(&serde_json::Value::String("blip".into())),
                None,
                None,
                crate::git::sqlite_index::QueryOrder::default(),
                &crate::git::search_store::effective_filter_fields(None),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "evt-1");
    }
}

use crate::git::hooks::{execute_repo_hook, HookContext};
use crate::git::index_store;
use crate::git::meta_index;
use crate::git::sqlite_index;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{debug, info};

type RepoBranchLocks = Mutex<HashMap<(PathBuf, String), Arc<Mutex<()>>>>;

static BRANCH_LOCKS: OnceLock<RepoBranchLocks> = OnceLock::new();

/// Serialize QUERY / JIT indexing on the same `(bare repo path, branch)` to avoid torn reads.
pub fn branch_operation_mutex(repo: PathBuf, branch: String) -> Arc<Mutex<()>> {
    let mut map = BRANCH_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    map.entry((repo, branch))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Hex prefix used under `.relay_data/branches/<hex>/`.
pub fn branch_hash_short(branch: &str) -> String {
    let branch_bytes = if branch.is_empty() {
        "main".as_bytes()
    } else {
        branch.as_bytes()
    };
    let branch_hash = hex::encode(branch_bytes);
    if branch_hash.len() > 12 {
        branch_hash[..12].to_string()
    } else {
        branch_hash
    }
}

/// `.relay_data/branches/<hash>/` for this branch.
pub fn branch_data_dir(repo_path: &Path, branch: &str) -> PathBuf {
    repo_path
        .join(".relay_data")
        .join("branches")
        .join(branch_hash_short(branch))
}

pub fn branch_index_paths(repo_path: &Path, branch: &str) -> (PathBuf, PathBuf) {
    let d = branch_data_dir(repo_path, branch);
    (d.join("index.db.json"), d.join("index.redb"))
}

/// Default SQLite plugin DB path (`.relay_data/branches/<hash>/index.db`).
pub fn branch_sqlite_path(repo_path: &Path, branch: &str) -> PathBuf {
    sqlite_index::sqlite_db_path(repo_path, branch)
}

fn read_json_indexed_head(json_path: &Path) -> String {
    if !json_path.exists() {
        return String::new();
    }
    let Ok(content) = std::fs::read_to_string(json_path) else {
        return String::new();
    };
    let Ok(db) = serde_json::from_str::<serde_json::Value>(&content) else {
        return String::new();
    };
    db.get("metadata")
        .and_then(|m| m.get("indexed_head"))
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string()
}

/// Run index hook if `indexed_head` lags `HEAD`; then sync JSON → redb + SQLite.
/// Caller should hold [`branch_operation_mutex`] for this repo/branch when serving QUERY.
pub fn ensure_indexed(ctx: &HookContext) -> anyhow::Result<()> {
    let (json_path, redb_path) = branch_index_paths(&ctx.repo_path, &ctx.branch);
    let sqlite_path = branch_sqlite_path(&ctx.repo_path, &ctx.branch);

    let mut indexed_head = sqlite_index::read_indexed_head(&sqlite_path)?
        .unwrap_or_default();
    if indexed_head.is_empty() {
        indexed_head = read_json_indexed_head(&json_path);
    }

    if indexed_head == ctx.new_commit {
        debug!(
            "Branch {} is up to date (head: {})",
            ctx.branch, ctx.new_commit
        );
        // Skip JSON/catalog re-sync: path_tree ingest is expensive and must not
        // run on every QUERY when SQLite already matches tip.
        return Ok(());
    }

    info!(
        "Branch {} is stale ({} != {}). Running JIT indexing...",
        ctx.branch, indexed_head, ctx.new_commit
    );

    let result = execute_repo_hook(ctx, "index");
    result?;
    debug!("JIT indexing completed for branch {}", ctx.branch);

    if json_path.exists() {
        index_store::sync_json_to_redb(&json_path, &redb_path)?;
        // JSON may only contain meta.yaml rows; re-ingest catalog indexes after so
        // `index.json` / `events.json` are not wiped by an empty collections.index.
        sqlite_index::sync_json_to_sqlite(&json_path, &sqlite_path)?;
        meta_index::ingest_catalog_json_indexes(ctx, &sqlite_path)?;
    }

    Ok(())
}

/// Async index warm after peer-fetch / catalog apply (errors logged only).
pub fn spawn_ensure_indexed(repo_path: PathBuf, branch: String, tip_sha: String) {
    std::thread::spawn(move || {
        let ctx = HookContext {
            repo_path: repo_path.clone(),
            old_commit: String::new(),
            new_commit: tip_sha.clone(),
            refname: format!("refs/heads/{}", branch),
            branch: branch.clone(),
            is_verified: true,
            files: Default::default(),
        };
        if let Ok(repo) = git2::Repository::open_bare(&repo_path) {
            if let Some(cfg) = crate::git::read_relay_config(&repo, &tip_sha) {
                if crate::git::has_index_hook(&cfg) {
                    let lock = branch_operation_mutex(repo_path, branch);
                    let _guard = lock.lock().unwrap();
                    if let Err(e) = ensure_indexed(&ctx) {
                        tracing::warn!(error = %e, "spawn_ensure_indexed failed");
                    }
                }
            }
        }
    });
}

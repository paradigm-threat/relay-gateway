use crate::git::hooks::{execute_repo_hook, HookContext};
use crate::git::index_store;
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

/// Run index hook if `indexed_head` in JSON lags `HEAD`; then sync JSON → redb.
/// Caller should hold [`branch_operation_mutex`] for this repo/branch when serving QUERY.
pub fn ensure_indexed(ctx: &HookContext) -> anyhow::Result<()> {
    let (db_path, redb_path) = branch_index_paths(&ctx.repo_path, &ctx.branch);

    let mut indexed_head = String::new();
    if db_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&db_path) {
            if let Ok(db) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(meta) = db.get("metadata") {
                    if let Some(head) = meta.get("indexed_head") {
                        indexed_head = head.as_str().unwrap_or("").to_string();
                    }
                }
            }
        }
    }

    if indexed_head != ctx.new_commit {
        info!(
            "Branch {} is stale ({} != {}). Running JIT indexing...",
            ctx.branch, indexed_head, ctx.new_commit
        );

        let result = execute_repo_hook(ctx, "index");
        result?;
        debug!("JIT indexing completed for branch {}", ctx.branch);
    } else {
        debug!(
            "Branch {} is up to date (head: {})",
            ctx.branch, ctx.new_commit
        );
    }

    if db_path.exists() {
        index_store::sync_json_to_redb(&db_path, &redb_path)?;
    }

    Ok(())
}

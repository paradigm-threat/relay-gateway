use crate::git::chat_index;
use crate::git::hooks::{execute_repo_hook, HookContext};
use crate::git::index_store;
use crate::git::repo::read_relay_config;
use crate::git::search_config;
use crate::git::search_plugins::{self, PLUGIN_CHAT};
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

/// Run index hook if `indexed_head` lags `HEAD`; then sync JSON → redb.
/// SQLite sync + catalog ingest happen inside `execute_repo_hook("index")` →
/// `sync_meta_yaml_index` (do not duplicate here).
/// Caller should hold [`branch_operation_mutex`] for this repo/branch when serving QUERY.
pub fn ensure_indexed(ctx: &HookContext) -> anyhow::Result<()> {
    let (json_path, redb_path) = branch_index_paths(&ctx.repo_path, &ctx.branch);
    let sqlite_path = branch_sqlite_path(&ctx.repo_path, &ctx.branch);

    let mut indexed_head = sqlite_index::read_indexed_head(&sqlite_path)?
        .unwrap_or_default();
    if indexed_head.is_empty() {
        indexed_head = read_json_indexed_head(&json_path);
    }

    let relay = git2::Repository::open_bare(&ctx.repo_path)
        .ok()
        .and_then(|r| read_relay_config(&r, &ctx.new_commit))
        .unwrap_or_default();
    let search = search_config::load_search_config(
        &ctx.repo_path,
        &ctx.new_commit,
        &relay,
        None,
    );
    let plugin = search
        .plugin
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| search_plugins::default_plugin_id(&relay));
    let chat_root = search
        .root
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(chat_index::DEFAULT_CHAT_ROOT);

    if indexed_head == ctx.new_commit {
        if plugin == PLUGIN_CHAT {
            // Tip may already be marked indexed from path_tree; still ensure chat_comments.
            chat_index::ensure_chat_indexed(ctx, chat_root)?;
        }
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

    if plugin == PLUGIN_CHAT {
        // Thin CS index hook (may call `@relay/search/chat` index); skip path_tree.
        let _ = execute_repo_hook(ctx, "index");
        chat_index::ensure_chat_indexed(ctx, chat_root)?;
        debug!("JIT chat indexing completed for branch {}", ctx.branch);
        return Ok(());
    }

    let result = execute_repo_hook(ctx, "index");
    result?;
    debug!("JIT indexing completed for branch {}", ctx.branch);

    // `execute_repo_hook("index")` already runs `sync_meta_yaml_index` → SQLite sync +
    // catalog ingest. Do **not** repeat sync_json+ingest here (that wiped catalog rows
    // with an empty collections.index then rebuilt them twice).
    // Legacy redb is not updated inside sync_meta — keep a single JSON→redb pass.
    if json_path.exists() {
        index_store::sync_json_to_redb(&json_path, &redb_path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::hooks::HookContext;
    use git2::{Repository, Signature};
    use std::collections::HashMap;
    use tempfile::tempdir;

    /// Regression: `ensure_indexed` must not wipe+rebuild SQLite twice.
    /// After one warm, `indexed_head` matches tip and a second call is a no-op.
    #[test]
    fn ensure_indexed_once_sets_head_second_call_noop() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();

        let yaml = br#"
name: t
server:
  hooks:
    index:
      path: hooks/server/index.cs
"#;
        let yaml_oid = repo.blob(yaml).unwrap();
        let cs = b"export default function() { return true; }\n";
        let cs_oid = repo.blob(cs).unwrap();
        let meta = b"title: One\n";
        let meta_oid = repo.blob(meta).unwrap();

        let mut tb_hooks = repo.treebuilder(None).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server.insert("index.cs", cs_oid, 0o100644).unwrap();
        let server_tid = tb_server.write().unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();

        let mut tb_item = repo.treebuilder(None).unwrap();
        tb_item.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let item_tid = tb_item.write().unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("item", item_tid, 0o040000).unwrap();
        let data_tid = tb_data.write().unwrap();

        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", yaml_oid, 0o100644).unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
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
            new_commit: commit.clone(),
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };

        ensure_indexed(&ctx).unwrap();
        let sqlite = branch_sqlite_path(&repo_path, "main");
        let head1 = sqlite_index::read_indexed_head(&sqlite)
            .unwrap()
            .unwrap_or_default();
        assert_eq!(head1, commit);

        // Second call must early-return (same head) — would panic/loop if broken.
        ensure_indexed(&ctx).unwrap();
        let head2 = sqlite_index::read_indexed_head(&sqlite)
            .unwrap()
            .unwrap_or_default();
        assert_eq!(head2, commit);
    }
}

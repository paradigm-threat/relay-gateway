use crate::git::chat_index;
use crate::git::hooks::{execute_repo_hook, HookContext};
use crate::git::index_store;
use crate::git::repo::read_relay_config;
use crate::git::search_config;
use crate::git::search_plugins::{self, PLUGIN_CHAT};
use crate::git::search_store::{store_for_bare_repo, store_key_for_bare, SearchStore};
use crate::git::sqlite_index;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{debug, info};

type RepoBranchLocks = Mutex<HashMap<(PathBuf, String), Arc<Mutex<()>>>>;

static BRANCH_LOCKS: OnceLock<RepoBranchLocks> = OnceLock::new();

/// Serialize index writes on the same `(bare repo path, branch)` (on-commit / warm).
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

/// Run index hook if SearchStore `docs_indexed_head` lags `HEAD`; then sync JSON → redb.
/// SQLite sync + catalog ingest happen inside `execute_repo_hook("index")` →
/// `sync_meta_yaml_index` (or directly when no CS index hook is configured).
/// QUERY must not call this; writers (post-receive, peer-fetch, `/api/index/warm`) do.
///
/// Freshness is **SearchStore only**. Leftover `index.db.json` `indexed_head` must not
/// skip ingest (that left QUERY `not_indexed` after the node-wide DB cutover).
pub fn ensure_indexed(ctx: &HookContext) -> anyhow::Result<()> {
    ensure_indexed_impl(ctx, false)
}

/// Full SearchStore regen (HTTP `/api/index/warm`). Ignores a matching store head so
/// schema/filterField changes and empty catalogs can be rebuilt from git.
pub fn ensure_indexed_force(ctx: &HookContext) -> anyhow::Result<()> {
    ensure_indexed_impl(ctx, true)
}

fn ensure_indexed_impl(ctx: &HookContext, force: bool) -> anyhow::Result<()> {
    let (json_path, redb_path) = branch_index_paths(&ctx.repo_path, &ctx.branch);
    let store = store_for_bare_repo(&ctx.repo_path)?;
    let key = store_key_for_bare(&ctx.repo_path, &ctx.branch);
    let _store_lock = store.lock_branch(&key);
    let _guard = _store_lock.lock().unwrap();

    let indexed_head = store.docs_indexed_head(&key)?.unwrap_or_default();

    let relay = git2::Repository::open_bare(&ctx.repo_path)
        .ok()
        .and_then(|r| read_relay_config(&r, &ctx.new_commit))
        .unwrap_or_default();
    if !crate::git::index_policy::branch_allowed_for_index(&relay, &ctx.branch) {
        debug!(
            branch = %ctx.branch,
            "index skipped: branch not in server.hooks.index.branches"
        );
        return Ok(());
    }
    let search = search_config::load_search_config(&ctx.repo_path, &ctx.new_commit, &relay, None);
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

    if !force && indexed_head == ctx.new_commit {
        if plugin == PLUGIN_CHAT {
            // Tip may already be marked indexed from path_tree; still ensure chat_comments.
            chat_index::ensure_chat_indexed(ctx, chat_root)?;
            debug!(
                "Branch {} is up to date (head: {})",
                ctx.branch, ctx.new_commit
            );
            return Ok(());
        }
        // Head may match tip while SQLite rows were wiped (empty replace, failed ingest).
        let doc_count = store.count_docs(&key, "index").unwrap_or(0);
        if doc_count > 0 {
            debug!(
                "Branch {} is up to date (head: {}, docs={})",
                ctx.branch, ctx.new_commit, doc_count
            );
            return Ok(());
        }
        info!(
            "Branch {} head matches tip but SearchStore index is empty — rebuilding",
            ctx.branch
        );
    }

    if force {
        info!(
            "Forcing SearchStore regen for branch {} (store_head={} tip={})",
            ctx.branch, indexed_head, ctx.new_commit
        );
    } else {
        info!(
            "Branch {} is stale ({} != {}). Running on-commit indexing...",
            ctx.branch, indexed_head, ctx.new_commit
        );
    }

    if plugin == PLUGIN_CHAT {
        // Thin CS index hook (may call `@relay/search/chat` index); skip path_tree.
        if crate::git::has_index_hook(&relay) {
            let _ = execute_repo_hook(ctx, "index");
        }
        chat_index::ensure_chat_indexed(ctx, chat_root)?;
        debug!("chat indexing completed for branch {}", ctx.branch);
        return Ok(());
    }

    if crate::git::has_index_hook(&relay) {
        execute_repo_hook(ctx, "index")?;
    } else {
        crate::git::meta_index::sync_meta_yaml_index(ctx)?;
    }
    debug!("on-commit indexing completed for branch {}", ctx.branch);

    // `execute_repo_hook("index")` already runs `sync_meta_yaml_index` → SQLite sync +
    // catalog ingest. Do **not** repeat sync_json+ingest here (that wiped catalog rows
    // with an empty collections.index then rebuilt them twice).
    // Legacy redb is not updated inside sync_meta — keep a single JSON→redb pass.
    if json_path.exists() {
        index_store::sync_json_to_redb(&json_path, &redb_path)?;
    }

    let legacy = branch_sqlite_path(&ctx.repo_path, &ctx.branch);
    let _ = std::fs::remove_file(&legacy);
    let _ = std::fs::remove_file(path_wal_sidecar(&legacy, "-wal"));
    let _ = std::fs::remove_file(path_wal_sidecar(&legacy, "-shm"));

    Ok(())
}

fn path_wal_sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Async index warm after peer-fetch / catalog apply (errors logged only).
pub fn spawn_ensure_indexed(
    repo_path: PathBuf,
    branch: String,
    old_commit: String,
    tip_sha: String,
) {
    std::thread::spawn(move || {
        let ctx = HookContext {
            repo_path: repo_path.clone(),
            old_commit,
            new_commit: tip_sha.clone(),
            refname: format!("refs/heads/{}", branch),
            branch: branch.clone(),
            is_verified: true,
            files: Default::default(),
        };
        if let Err(e) = ensure_indexed(&ctx) {
            tracing::warn!(error = %e, "spawn_ensure_indexed failed");
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

    fn meta_index_bare_fixture() -> (tempfile::TempDir, PathBuf, String, HookContext) {
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
        (dir, repo_path, commit, ctx)
    }

    /// Regression: `ensure_indexed` must not wipe+rebuild SQLite twice.
    /// After one warm, `indexed_head` matches tip and a second call is a no-op.
    #[test]
    fn ensure_indexed_once_sets_head_second_call_noop() {
        let (_dir, repo_path, commit, ctx) = meta_index_bare_fixture();

        ensure_indexed(&ctx).unwrap();
        let store = store_for_bare_repo(&repo_path).unwrap();
        let key = store_key_for_bare(&repo_path, "main");
        let head1 = store.docs_indexed_head(&key).unwrap().unwrap_or_default();
        assert_eq!(head1, commit);

        // Second call must early-return (same head) — would panic/loop if broken.
        ensure_indexed(&ctx).unwrap();
        let head2 = store.docs_indexed_head(&key).unwrap().unwrap_or_default();
        assert_eq!(head2, commit);
    }

    /// Leftover `index.db.json` stamped at tip must not skip SearchStore ingest.
    #[test]
    fn ensure_indexed_ignores_json_head_when_store_empty() {
        let (_dir, repo_path, commit, ctx) = meta_index_bare_fixture();
        let (json_path, _) = branch_index_paths(&repo_path, "main");
        std::fs::create_dir_all(json_path.parent().unwrap()).unwrap();
        std::fs::write(
            &json_path,
            format!(r#"{{"metadata":{{"indexed_head":"{commit}"}},"collections":{{"index":[]}}}}"#),
        )
        .unwrap();

        ensure_indexed(&ctx).unwrap();
        let store = store_for_bare_repo(&repo_path).unwrap();
        let key = store_key_for_bare(&repo_path, "main");
        assert_eq!(
            store.docs_indexed_head(&key).unwrap().as_deref(),
            Some(commit.as_str())
        );
        assert!(
            store.count_docs(&key, "index").unwrap() > 0,
            "SearchStore must ingest meta.yaml even when JSON already has this tip"
        );
    }

    #[test]
    fn ensure_indexed_rebuilds_when_head_matches_but_store_empty() {
        let (_dir, repo_path, commit, ctx) = meta_index_bare_fixture();
        let store = store_for_bare_repo(&repo_path).unwrap();
        let key = store_key_for_bare(&repo_path, "main");
        ensure_indexed(&ctx).unwrap();
        store
            .replace_collection(&key, "index", &[], None, None, None)
            .unwrap();
        assert_eq!(store.count_docs(&key, "index").unwrap(), 0);
        ensure_indexed(&ctx).unwrap();
        assert!(
            store.count_docs(&key, "index").unwrap() > 0,
            "matching head with empty store must rebuild"
        );
    }

    #[test]
    fn ensure_indexed_force_rebuilds_when_store_already_current() {
        let (_dir, repo_path, commit, ctx) = meta_index_bare_fixture();
        ensure_indexed(&ctx).unwrap();
        let store = store_for_bare_repo(&repo_path).unwrap();
        let key = store_key_for_bare(&repo_path, "main");
        store
            .replace_collection(&key, "index", &[], None, None, None)
            .unwrap();
        assert_eq!(store.count_docs(&key, "index").unwrap(), 0);

        ensure_indexed_force(&ctx).unwrap();
        assert_eq!(
            store.docs_indexed_head(&key).unwrap().as_deref(),
            Some(commit.as_str())
        );
        assert!(
            store.count_docs(&key, "index").unwrap() > 0,
            "warm force must regen rows from git"
        );
    }
}

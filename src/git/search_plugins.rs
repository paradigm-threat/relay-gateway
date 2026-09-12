//! Named QUERY search plugins. CleverScript imports `@relay/search/*` (or returns a plugin id); Rust executes.

use crate::git::chat_index;
use crate::git::index_store;
use crate::git::indexing::branch_index_paths;
use crate::git::search_config;
use crate::git::search_store::{
    effective_filter_fields, effective_sort_fields, store_for_bare_repo, store_key_for_bare,
    SearchStore,
};
use crate::git::sqlite_index;
use crate::types::RelayConfig;
use serde_json::Value;
use std::path::Path;

pub const PLUGIN_SQLITE: &str = "sqlite";
pub const PLUGIN_REDB: &str = "redb";
pub const PLUGIN_CHAT: &str = "chat";

/// Resolve default plugin id from `.relay.yaml` `server.search.plugin` (default: sqlite).
pub fn default_plugin_id(cfg: &RelayConfig) -> &str {
    cfg.server
        .as_ref()
        .and_then(|s| s.search.as_ref())
        .and_then(|s| s.plugin.as_deref())
        .filter(|p| !p.is_empty())
        .unwrap_or(PLUGIN_SQLITE)
}

/// Run a search plugin. Unknown ids error.
pub fn run_plugin(
    plugin: &str,
    repo_full_path: &Path,
    branch: &str,
    collection: &str,
    query: Option<&Value>,
    fields: Option<&[String]>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    run_plugin_ordered(
        plugin,
        repo_full_path,
        branch,
        collection,
        query,
        fields,
        limit,
        sqlite_index::QueryOrder::default(),
    )
}

/// Like [`run_plugin`] with an explicit [`sqlite_index::QueryOrder`].
pub fn run_plugin_ordered(
    plugin: &str,
    repo_full_path: &Path,
    branch: &str,
    collection: &str,
    query: Option<&Value>,
    fields: Option<&[String]>,
    limit: Option<usize>,
    order: sqlite_index::QueryOrder,
) -> anyhow::Result<Vec<Value>> {
    match plugin {
        PLUGIN_SQLITE => {
            let store = store_for_bare_repo(repo_full_path)?;
            let key = store_key_for_bare(repo_full_path, branch);
            let head = git2::Repository::open_bare(repo_full_path)
                .ok()
                .and_then(|r| r.refname_to_id(&format!("refs/heads/{branch}")).ok())
                .map(|oid| oid.to_string())
                .unwrap_or_default();
            let relay = git2::Repository::open_bare(repo_full_path)
                .ok()
                .and_then(|r| crate::git::repo::read_relay_config(&r, &head))
                .unwrap_or_default();
            let cfg = search_config::load_search_config(repo_full_path, &head, &relay, None);
            let coll_cfg = cfg.collection(collection);
            let filter_fields = coll_cfg
                .map(|c| effective_filter_fields(c.filter_fields.as_deref()))
                .unwrap_or_else(|| effective_filter_fields(None));
            let sort_fields = coll_cfg
                .map(|c| effective_sort_fields(c.sort_fields.as_deref()))
                .unwrap_or_else(|| effective_sort_fields(None));
            if !order.allowed_for_sort_fields(&sort_fields) {
                anyhow::bail!(
                    "QUERY_BAD_ORDER: order {:?} is not in collection sortFields",
                    order.as_str()
                );
            }
            store.query(
                &key,
                collection,
                query,
                fields,
                limit,
                order,
                &filter_fields,
            )
        }
        PLUGIN_REDB => {
            let (json_path, redb_path) = branch_index_paths(repo_full_path, branch);
            if json_path.exists() {
                index_store::sync_json_to_redb(&json_path, &redb_path)?;
            }
            let mut results = index_store::read_collection(&redb_path, collection)?
                .unwrap_or(Value::Array(vec![]));
            let arr = results
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("collection {} is not an array", collection))?;
            filter_redb_array(arr, query);
            let lim = sqlite_index::clamp_query_limit(order, limit);
            let rows: Vec<Value> = arr.iter().take(lim).cloned().collect();
            Ok(project_rows(rows, fields))
        }
        PLUGIN_CHAT => {
            let _ = collection;
            let _ = order;
            chat_index::query_chat_for_bare(repo_full_path, branch, query, limit)
                .map(|rows| project_rows(rows, fields))
        }
        other => anyhow::bail!("unknown search plugin: {other}"),
    }
}

fn project_rows(rows: Vec<Value>, fields: Option<&[String]>) -> Vec<Value> {
    let Some(field_list) = fields.filter(|f| !f.is_empty()) else {
        return rows;
    };
    rows.into_iter()
        .map(|row| {
            let mut m = serde_json::Map::new();
            if let Some(obj) = row.as_object() {
                for k in field_list {
                    if let Some(v) = obj.get(k) {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
            serde_json::Value::Object(m)
        })
        .collect()
}

fn filter_redb_array(arr: &mut Vec<Value>, query: Option<&Value>) {
    let Some(query_val) = query else {
        return;
    };
    if let Some(q_str) = query_val.as_str() {
        if !q_str.is_empty() {
            let q_lower = q_str.to_lowercase();
            arr.retain(|item| {
                if let Some(obj) = item.as_object() {
                    for value in obj.values() {
                        if let Some(s) = value.as_str() {
                            if s.to_lowercase().contains(&q_lower) {
                                return true;
                            }
                        }
                    }
                }
                false
            });
        }
    } else if let Some(q_obj) = query_val.as_object() {
        arr.retain(|item| {
            if let Some(item_obj) = item.as_object() {
                for (k, v) in q_obj {
                    if item_obj.get(k) != Some(v) {
                        return false;
                    }
                }
                true
            } else {
                false
            }
        });
    }
}

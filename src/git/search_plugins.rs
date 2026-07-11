//! Named QUERY search plugins. CleverScript routes; Rust executes.

use crate::git::index_store;
use crate::git::indexing::{branch_index_paths, branch_sqlite_path};
use crate::git::sqlite_index;
use crate::types::RelayConfig;
use serde_json::Value;
use std::path::Path;

pub const PLUGIN_SQLITE: &str = "sqlite";
pub const PLUGIN_REDB: &str = "redb";

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
    match plugin {
        PLUGIN_SQLITE => {
            let db = branch_sqlite_path(repo_full_path, branch);
            sqlite_index::query_collection(&db, collection, query, fields, limit)
        }
        PLUGIN_REDB => {
            let (json_path, redb_path) = branch_index_paths(repo_full_path, branch);
            if json_path.exists() {
                index_store::sync_json_to_redb(&json_path, &redb_path)?;
            }
            let mut results =
                index_store::read_collection(&redb_path, collection)?.unwrap_or(Value::Array(vec![]));
            let arr = results
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("collection {} is not an array", collection))?;
            filter_redb_array(arr, query);
            let rows: Vec<Value> = arr.clone();
            Ok(project_rows(rows, fields))
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
            Value::Object(m)
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

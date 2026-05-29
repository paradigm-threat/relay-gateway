use crate::git;
use crate::git::hooks::HookContext;
use crate::git::index_policy;
use crate::git::index_store;
use crate::git::indexing::{branch_index_paths, branch_operation_mutex, ensure_indexed};
use crate::git::clever_script_host::CleverScriptServerHost;
use crate::git::repo::{read_file_from_repo, read_relay_config};
use relay_runtime::value::Value as CsValue;
use serde_json::{Map, Value};
use std::path::Path;

fn project_rows(rows: Vec<Value>, fields: Option<&[String]>) -> Vec<Value> {
    let Some(field_list) = fields.filter(|f| !f.is_empty()) else {
        return rows;
    };
    rows.into_iter()
        .map(|row| {
            let mut m = Map::new();
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

fn clever_query_result_to_rows(v: &CsValue) -> anyhow::Result<Vec<Value>> {
    match v {
        CsValue::Array(a) => Ok(a.iter().map(|x| x.to_json()).collect()),
        CsValue::Object(o) => {
            if let Some(CsValue::Array(rows)) = o.get("rows") {
                Ok(rows.iter().map(|x| x.to_json()).collect())
            } else {
                anyhow::bail!("CleverScript QUERY must return {{ rows: [...] }} or a JSON array");
            }
        }
        _ => anyhow::bail!("CleverScript QUERY returned an unsupported type"),
    }
}

/// Run JIT index if needed, load collection from redb (synced from hook JSON), filter, project fields from `.relay.yaml`.
/// When `server.hooks.query.path` ends with `.cs`, run CleverScript instead (`import … from '@relay/git'`; no redb required).
pub fn execute_query(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
) -> anyhow::Result<Vec<Value>> {
    let repo_full_path = repo_root.join(format!("{}.git", repo_name));
    let repo = git2::Repository::open_bare(&repo_full_path)?;
    let head = git::get_branch_commit_info(&repo, branch)
        .ok_or_else(|| anyhow::anyhow!("Branch {} not found", branch))?
        .0;

    let relay_cfg = read_relay_config(&repo, &head).unwrap_or_default();

    if !index_policy::branch_allowed_for_query_request(&relay_cfg, branch) {
        return Ok(vec![]);
    }

    let export_fields = index_policy::export_fields_for_collection(&relay_cfg, collection);

    let ctx = HookContext {
        repo_path: repo_full_path.clone(),
        old_commit: String::new(),
        new_commit: head,
        refname: format!("refs/heads/{}", branch),
        branch: branch.to_string(),
        is_verified: true,
        files: std::collections::HashMap::new(),
    };

    let lock = branch_operation_mutex(ctx.repo_path.clone(), ctx.branch.clone());
    let _guard = lock.lock().unwrap();

    if let Some(cs_path) = index_policy::clever_query_hook_path(&relay_cfg) {
        let source_bytes = read_file_from_repo(&repo_full_path, branch, &cs_path)?;
        let source = String::from_utf8_lossy(&source_bytes).into_owned();
        let qctx = serde_json::json!({
            "branch": branch,
            "collection": collection,
            "query": query,
            "repo": repo_name,
        });
        let host = CleverScriptServerHost::host_handle(repo_full_path.clone(), branch.to_string(), false);
        let raw = relay_runtime::run_query_script(&source, host, &qctx, None, None)
            .map_err(|e| anyhow::anyhow!("CleverScript QUERY: {}", e))?;
        let rows = clever_query_result_to_rows(&raw)?;
        return Ok(project_rows(rows, export_fields.as_deref()));
    }

    if index_policy::has_index_hook(&relay_cfg) {
        ensure_indexed(&ctx)?;
    }

    let (json_path, redb_path) = branch_index_paths(&repo_full_path, branch);

    if json_path.exists() {
        index_store::sync_json_to_redb(&json_path, &redb_path)?;
    }

    let mut results =
        index_store::read_collection(&redb_path, collection)?.unwrap_or(Value::Array(vec![]));

    let arr = results
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("collection {} is not an array", collection))?;

    if let Some(query_val) = query {
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

    let rows: Vec<Value> = arr.clone();
    Ok(project_rows(rows, export_fields.as_deref()))
}

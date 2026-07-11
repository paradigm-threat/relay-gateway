use crate::git;
use crate::git::hooks::HookContext;
use crate::git::index_policy;
use crate::git::indexing::{branch_operation_mutex, ensure_indexed};
use crate::git::search_plugins;
use crate::git::clever_script_host::CleverScriptServerHost;
use crate::git::repo::{read_file_from_repo, read_relay_config};
use relay_runtime::value::Value as CsValue;
use serde_json::Value;
use std::path::Path;

fn clever_query_result_to_rows(v: &CsValue) -> anyhow::Result<Option<Vec<Value>>> {
    match v {
        CsValue::Array(a) => Ok(Some(a.iter().map(|x| x.to_json()).collect())),
        CsValue::Object(o) => {
            if let Some(CsValue::Array(rows)) = o.get("rows") {
                return Ok(Some(rows.iter().map(|x| x.to_json()).collect()));
            }
            // Thin router: { plugin, collection?, query?, limit? } — no rows.
            if o.contains_key("plugin") {
                return Ok(None);
            }
            anyhow::bail!("CleverScript QUERY must return {{ rows: [...] }}, a JSON array, or {{ plugin: \"sqlite\"|\"redb\", ... }}");
        }
        CsValue::Null => Ok(None),
        _ => anyhow::bail!("CleverScript QUERY returned an unsupported type"),
    }
}

fn routing_from_cs(
    v: &CsValue,
    default_plugin: &str,
    default_collection: &str,
    default_query: Option<&Value>,
) -> (String, String, Option<Value>, Option<usize>) {
    let CsValue::Object(o) = v else {
        return (
            default_plugin.to_string(),
            default_collection.to_string(),
            default_query.cloned(),
            None,
        );
    };
    let plugin = o
        .get("plugin")
        .and_then(|p| match p {
            CsValue::String(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        })
        .unwrap_or(default_plugin)
        .to_string();
    let collection = o
        .get("collection")
        .and_then(|c| match c {
            CsValue::String(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        })
        .unwrap_or(default_collection)
        .to_string();
    let query = o
        .get("query")
        .map(|q| q.to_json())
        .or_else(|| default_query.cloned());
    let limit = o.get("limit").and_then(|l| match l {
        CsValue::Number(n) if *n > 0.0 => Some(*n as usize),
        _ => None,
    });
    (plugin, collection, query, limit)
}

/// Run JIT index if needed, then QUERY via search plugin (default **sqlite**).
///
/// When `server.hooks.query.path` ends with `.cs`:
/// - `{ rows: [...] }` / array — legacy full CS result (compat; prefer plugins).
/// - `{ plugin: "sqlite"|"redb", collection?, query?, limit? }` — thin router; Rust runs the plugin.
/// - `null` — fall through to default plugin.
pub fn execute_query(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
) -> anyhow::Result<Vec<Value>> {
    execute_query_limited(repo_root, repo_name, branch, query, collection, None)
}

/// Same as [`execute_query`] with an optional result cap (HTTP/MCP `limit`).
pub fn execute_query_limited(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
    limit: Option<usize>,
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
    let default_plugin = search_plugins::default_plugin_id(&relay_cfg);

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

    if index_policy::has_index_hook(&relay_cfg) {
        ensure_indexed(&ctx)?;
    }

    if let Some(cs_path) = index_policy::clever_query_hook_path(&relay_cfg) {
        let source_bytes = read_file_from_repo(&repo_full_path, branch, &cs_path)?;
        let source = String::from_utf8_lossy(&source_bytes).into_owned();
        let qctx = serde_json::json!({
            "branch": branch,
            "collection": collection,
            "query": query,
            "repo": repo_name,
            "defaultPlugin": default_plugin,
            "limit": limit,
        });
        let host = CleverScriptServerHost::host_handle(repo_full_path.clone(), branch.to_string(), false);
        let raw = relay_runtime::run_query_script(&source, host, &qctx, None, None)
            .map_err(|e| anyhow::anyhow!("CleverScript QUERY: {}", e))?;

        if let Some(rows) = clever_query_result_to_rows(&raw)? {
            let rows = project_export(rows, export_fields.as_deref());
            return Ok(apply_limit(rows, limit));
        }

        let (plugin, coll, q, cs_limit) =
            routing_from_cs(&raw, default_plugin, collection, query.as_ref());
        let effective_limit = cs_limit.or(limit);
        return search_plugins::run_plugin(
            &plugin,
            &repo_full_path,
            branch,
            &coll,
            q.as_ref(),
            export_fields.as_deref(),
            effective_limit,
        );
    }

    search_plugins::run_plugin(
        default_plugin,
        &repo_full_path,
        branch,
        collection,
        query.as_ref(),
        export_fields.as_deref(),
        limit,
    )
}

fn apply_limit(mut rows: Vec<Value>, limit: Option<usize>) -> Vec<Value> {
    if let Some(n) = limit {
        if rows.len() > n {
            rows.truncate(n);
        }
    }
    rows
}

fn project_export(rows: Vec<Value>, fields: Option<&[String]>) -> Vec<Value> {
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

/// Shared entry for MCP `search_site` / QUERY — always goes through [`execute_query_limited`].
pub fn execute_search(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
) -> anyhow::Result<Vec<Value>> {
    execute_query_limited(repo_root, repo_name, branch, query, collection, None)
}

pub fn execute_search_limited(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    execute_query_limited(repo_root, repo_name, branch, query, collection, limit)
}

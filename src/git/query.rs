use crate::git;
use crate::git::clever_script_host::CleverScriptServerHost;
use crate::git::index_policy;
use crate::git::search_plugins;
use crate::git::search_store::{store_for_bare_repo, store_key_for_bare, SearchStore};
use crate::git::sqlite_index::{self, QueryOrder};
use crate::git::repo::{read_file_from_repo, read_relay_config};
use relay_runtime::value::Value as CsValue;
use serde_json::Value;
use std::path::Path;
use std::time::Instant;

/// QUERY / MCP search result with index health metadata.
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub results: Vec<Value>,
    pub collection: String,
    pub query: Option<Value>,
    pub tip_head: String,
    pub indexed_head: Option<String>,
    pub index_fresh: bool,
    pub doc_count: u64,
    pub took_ms: u64,
    /// `"ok"` or `"not_indexed"` (DB missing / no indexed_head).
    pub status: &'static str,
}

impl QueryOutcome {
    pub fn to_json_envelope(&self) -> Value {
        serde_json::json!({
            "results": self.results,
            "collection": self.collection,
            "query": self.query,
            "count": self.results.len(),
            "docCount": self.doc_count,
            "indexedHead": self.indexed_head,
            "tipHead": self.tip_head,
            "indexFresh": self.index_fresh,
            "tookMs": self.took_ms,
            "status": self.status,
        })
    }
}

fn clever_query_result_to_rows(v: &CsValue) -> anyhow::Result<Option<Vec<Value>>> {
    match v {
        CsValue::Array(a) => Ok(Some(a.iter().map(|x| x.to_json()).collect())),
        CsValue::Object(o) => {
            if let Some(CsValue::Array(rows)) = o.get("rows") {
                return Ok(Some(rows.iter().map(|x| x.to_json()).collect()));
            }
            if o.contains_key("plugin") {
                return Ok(None);
            }
            anyhow::bail!("CleverScript QUERY must return {{ rows: [...] }}, a JSON array, {{ plugin: \"sqlite\"|\"redb\"|\"chat\", ... }}, or call search() from @relay/search/*");
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
    default_order: QueryOrder,
) -> (
    String,
    String,
    Option<Value>,
    Option<usize>,
    Option<String>,
    QueryOrder,
) {
    let CsValue::Object(o) = v else {
        return (
            default_plugin.to_string(),
            default_collection.to_string(),
            default_query.cloned(),
            None,
            None,
            default_order,
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
    let config = o.get("config").and_then(|c| match c {
        CsValue::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    });
    let order = o
        .get("order")
        .and_then(|ord| match ord {
            CsValue::String(s) => QueryOrder::parse(s).ok(),
            _ => None,
        })
        .unwrap_or(default_order);
    (plugin, collection, query, limit, config, order)
}

fn finish_outcome(
    _repo_full_path: &Path,
    _branch: &str,
    tip_head: &str,
    collection: &str,
    query: Option<Value>,
    results: Vec<Value>,
    started: Instant,
    indexed_head: Option<String>,
    doc_count: u64,
) -> anyhow::Result<QueryOutcome> {
    let index_fresh = indexed_head.as_deref() == Some(tip_head);
    let status = if indexed_head.is_none() {
        "not_indexed"
    } else {
        "ok"
    };
    Ok(QueryOutcome {
        results,
        collection: collection.to_string(),
        query,
        tip_head: tip_head.to_string(),
        indexed_head,
        index_fresh,
        doc_count,
        took_ms: started.elapsed().as_millis() as u64,
        status,
    })
}

/// QUERY via search plugin (default **sqlite**). Read-only: never rebuilds the index.
pub fn execute_query(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
) -> anyhow::Result<Vec<Value>> {
    Ok(
        execute_query_outcome(repo_root, repo_name, branch, query, collection, None, None)?
            .results,
    )
}

pub fn execute_query_limited(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    Ok(
        execute_query_outcome(
            repo_root,
            repo_name,
            branch,
            query,
            collection,
            limit,
            None,
        )?
        .results,
    )
}

/// Full QUERY with index health envelope (HTTP JSON + MCP).
pub fn execute_query_outcome(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
    limit: Option<usize>,
    order: Option<QueryOrder>,
) -> anyhow::Result<QueryOutcome> {
    let started = Instant::now();
    let order = order.unwrap_or_default();
    let repo_full_path = repo_root.join(format!("{}.git", repo_name));
    let repo = git2::Repository::open_bare(&repo_full_path)?;
    let head = git::get_branch_commit_info(&repo, branch)
        .ok_or_else(|| anyhow::anyhow!("Branch {} not found", branch))?
        .0;

    let cache_key = crate::git::query_cache::cache_key(
        repo_name,
        &head,
        collection,
        query.as_ref(),
        limit,
        order,
    );
    // Do not cache RANDOM() — each QUERY should reshuffle.
    let use_cache = order != QueryOrder::Random;
    if use_cache {
        if let Some(hit) = crate::git::query_cache::get(&cache_key) {
            return Ok(hit);
        }
    }

    let relay_cfg = read_relay_config(&repo, &head).unwrap_or_default();
    let store = store_for_bare_repo(&repo_full_path)?;
    let store_key = store_key_for_bare(&repo_full_path, branch);

    if !index_policy::branch_allowed_for_query_request(&relay_cfg, branch) {
        let indexed_head = store.docs_indexed_head(&store_key)?;
        let doc_count = store.count_docs(&store_key, collection)?;
        return finish_outcome(
            &repo_full_path,
            branch,
            &head,
            collection,
            query,
            vec![],
            started,
            indexed_head,
            doc_count,
        );
    }

    let export_fields = index_policy::export_fields_for_collection(&relay_cfg, collection);
    let default_plugin = search_plugins::default_plugin_id(&relay_cfg);

    let mut rows: Vec<Value>;
    let mut used_collection = collection.to_string();
    let mut used_query = query.clone();
    let mut used_plugin = default_plugin.to_string();

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
            "order": order.as_str(),
        });
        let host = CleverScriptServerHost::host_handle(repo_full_path.clone(), branch.to_string(), false);
        let raw = relay_runtime::run_query_script(&source, host, &qctx, None, None)
            .map_err(|e| anyhow::anyhow!("CleverScript QUERY: {}", e))?;

        if let Some(r) = clever_query_result_to_rows(&raw)? {
            rows = project_export(r, export_fields.as_deref());
            let lim = sqlite_index::clamp_query_limit(order, limit);
            rows = apply_limit(rows, Some(lim));
        } else {
            let (plugin, coll, q, cs_limit, _cfg, cs_order) =
                routing_from_cs(&raw, default_plugin, collection, query.as_ref(), order);
            used_plugin = plugin.clone();
            used_collection = coll.clone();
            used_query = q.clone();
            let effective_limit = cs_limit.or(limit);
            rows = search_plugins::run_plugin_ordered(
                &plugin,
                &repo_full_path,
                branch,
                &coll,
                q.as_ref(),
                export_fields.as_deref(),
                effective_limit,
                cs_order,
            )?;
        }
    } else {
        rows = search_plugins::run_plugin_ordered(
            default_plugin,
            &repo_full_path,
            branch,
            collection,
            query.as_ref(),
            export_fields.as_deref(),
            limit,
            order,
        )?;
    }

    let (indexed_head, doc_count) = if used_plugin == search_plugins::PLUGIN_CHAT {
        (
            store.chat_indexed_head(&store_key)?,
            store.chat_count(&store_key)? as u64,
        )
    } else {
        (
            store.docs_indexed_head(&store_key)?,
            store.count_docs(&store_key, &used_collection)?,
        )
    };
    let outcome = finish_outcome(
        &repo_full_path,
        branch,
        &head,
        &used_collection,
        used_query,
        rows,
        started,
        indexed_head,
        doc_count,
    )?;
    if use_cache {
        crate::git::query_cache::put(cache_key, outcome.clone());
    }
    Ok(outcome)
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

pub fn execute_search_outcome(
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
    query: Option<Value>,
    collection: &str,
    limit: Option<usize>,
) -> anyhow::Result<QueryOutcome> {
    execute_query_outcome(repo_root, repo_name, branch, query, collection, limit, None)
}

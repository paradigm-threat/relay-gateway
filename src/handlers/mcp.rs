//! Streamable HTTP / JSON-RPC MCP adapter.
//! Search tools call [`crate::git::query::execute_search_outcome`] — no duplicate search stack.

use crate::git::index_policy;
use crate::git::query::{execute_search_limited, execute_search_outcome};
use crate::git::repo::{read_file_from_repo, read_relay_config};
use crate::git::{get_branch_commit_info, open_repo};
use crate::types::AppState;
use crate::DEFAULT_BRANCH;
use axum::{
    extract::{Path as AxPath, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Phantom-millennium interval used by paradigm-threat chronology (signed years).
const BLIP_SIGNED_START: i64 = -670;
const BLIP_SIGNED_END: i64 = 1053;

#[derive(Debug, Deserialize, Default, Clone)]
pub struct McpManifest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tools: HashMap<String, McpToolDef>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct McpToolDef {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// `query` | `git_read` | `json_match` | `events_lookup` | `research_status`
    #[serde(default)]
    pub via: Option<String>,
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// Optional JSON Schema object override for `tools/list`.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

fn load_manifest(repo_path: &std::path::PathBuf, branch: &str) -> McpManifest {
    for candidate in ["mcp.yaml", "hooks/server/mcp.yaml"] {
        if let Ok(bytes) = read_file_from_repo(repo_path, branch, candidate) {
            if let Ok(m) = serde_yaml::from_slice::<McpManifest>(&bytes) {
                return m;
            }
        }
    }
    default_manifest()
}

fn default_manifest() -> McpManifest {
    let mut tools = HashMap::new();
    tools.insert(
        "search_site".into(),
        McpToolDef {
            description: Some("Keyword search via Relay QUERY / sqlite plugin".into()),
            aliases: vec!["search".into()],
            via: Some("query".into()),
            collection: Some("index".into()),
            ..Default::default()
        },
    );
    tools.insert(
        "read_article".into(),
        McpToolDef {
            description: Some("Read a git blob by path".into()),
            aliases: vec!["fetch".into()],
            via: Some("git_read".into()),
            ..Default::default()
        },
    );
    McpManifest {
        name: Some("relay-repo".into()),
        version: Some("0.1.0".into()),
        tools,
    }
}

fn resolve_tool<'a>(manifest: &'a McpManifest, name: &'a str) -> Option<(&'a str, &'a McpToolDef)> {
    if let Some(def) = manifest.tools.get(name) {
        return Some((name, def));
    }
    for (canonical, def) in &manifest.tools {
        if def.aliases.iter().any(|a| a == name) {
            return Some((canonical.as_str(), def));
        }
    }
    None
}

fn schema_for_via(via: &str) -> Value {
    match via {
        "git_read" => json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "description": "Repo-relative path" },
                "offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 40000 }
            }
        }),
        "json_match" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 2 },
                "id": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
            }
        }),
        "events_lookup" => json!({
            "type": "object",
            "properties": {
                "eventId": { "type": "string" },
                "titleQuery": { "type": "string" },
                "refYear": { "type": "integer", "description": "Reference CE year (default: current UTC year)" },
                "blipAdjust": { "type": "boolean", "default": true }
            }
        }),
        "research_status" => json!({
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": { "type": "string", "minLength": 2 },
                "query": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
            }
        }),
        _ => json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": { "type": "string", "minLength": 2, "description": "Space-separated AND terms" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 },
                "collection": { "type": "string" }
            }
        }),
    }
}

fn tools_list_payload(manifest: &McpManifest) -> Value {
    let mut tools = Vec::new();
    for (name, def) in &manifest.tools {
        let via = def.via.as_deref().unwrap_or("query");
        let schema = def
            .input_schema
            .clone()
            .unwrap_or_else(|| schema_for_via(via));
        let mut names = vec![name.clone()];
        names.extend(def.aliases.clone());
        for n in names {
            tools.push(json!({
                "name": n,
                "description": def.description.clone().unwrap_or_default(),
                "inputSchema": schema,
                "annotations": { "readOnlyHint": true }
            }));
        }
    }
    tools.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    json!({ "tools": tools })
}

fn json_tool_result(data: Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&data).unwrap_or_else(|_| "{}".into())
        }],
        "isError": false
    })
}

fn json_tool_error(message: impl AsRef<str>) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": message.as_ref()
        }],
        "isError": true
    })
}

fn safe_repo_rel_path(path: &str) -> anyhow::Result<&str> {
    let p = path.trim().trim_start_matches('/');
    if p.is_empty() {
        anyhow::bail!("path is required");
    }
    if p.contains('\0') || p.split('/').any(|s| s == "..") {
        anyhow::bail!("invalid path");
    }
    Ok(p)
}

fn calendar_years_elapsed(from: i64, to: i64) -> i64 {
    if from == to {
        return 0;
    }
    if from < 0 && to > 0 {
        (to - from) - 1
    } else {
        (to - from).abs()
    }
}

fn blip_overlap_years(from: i64, to: i64) -> i64 {
    let a = from.min(to);
    let b = from.max(to);
    let start = a.max(BLIP_SIGNED_START);
    let end = b.min(BLIP_SIGNED_END);
    if end <= start {
        0
    } else {
        calendar_years_elapsed(start, end)
    }
}

fn project_signed_year(event: &Value) -> Option<i64> {
    if let Some(n) = event.get("start").and_then(|v| v.as_i64()) {
        return Some(n);
    }
    if let Some(n) = event.get("start").and_then(|v| v.as_f64()) {
        return Some(n as i64);
    }
    let dates = event.get("dates")?.as_array()?;
    for d in dates {
        if let Some(n) = d.get("start").and_then(|v| v.as_i64()) {
            return Some(n);
        }
        if let Some(n) = d.get("start").and_then(|v| v.as_f64()) {
            return Some(n as i64);
        }
    }
    None
}

fn current_utc_year() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Approximate Gregorian year from unix days (good enough for MCP elapsed estimates).
    1970 + (secs / 31_557_600) as i64
}

fn load_json_entries(repo_full: &std::path::PathBuf, branch: &str, source: &str) -> anyhow::Result<Vec<Value>> {
    let bytes = read_file_from_repo(repo_full, branch, source)?;
    let root: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("{source}: {e}"))?;
    Ok(match root {
        Value::Array(a) => a,
        Value::Object(o) => {
            if let Some(Value::Array(a)) = o
                .get("claims")
                .or_else(|| o.get("entries"))
                .or_else(|| o.get("events"))
            {
                // Flatten nested timeline trees when present.
                let mut out = Vec::new();
                fn walk(nodes: &[Value], out: &mut Vec<Value>) {
                    for n in nodes {
                        if let Value::Object(obj) = n {
                            out.push(Value::Object(obj.clone()));
                            if let Some(Value::Array(ch)) = obj.get("children") {
                                walk(ch, out);
                            }
                        }
                    }
                }
                walk(a, &mut out);
                if out.is_empty() {
                    a.clone()
                } else {
                    out
                }
            } else {
                o.values().cloned().collect()
            }
        }
        other => vec![other],
    })
}

fn call_tool(
    state: &AppState,
    repo_name: &str,
    branch: &str,
    manifest: &McpManifest,
    tool_name: &str,
    arguments: &Value,
) -> Value {
    let repo_full = state.repo_path.join(format!("{}.git", repo_name));
    let Some((_canonical, def)) = resolve_tool(manifest, tool_name) else {
        return json_tool_error(format!("unknown tool: {tool_name}"));
    };
    let via = def.via.as_deref().unwrap_or("query");

    let result = (|| -> anyhow::Result<Value> {
        match via {
            "query" => {
                let collection = arguments
                    .get("collection")
                    .and_then(|v| v.as_str())
                    .or(def.collection.as_deref())
                    .unwrap_or("index");
                let q = arguments
                    .get("query")
                    .cloned()
                    .or_else(|| arguments.get("q").cloned());
                if let Some(Value::String(s)) = &q {
                    if s.trim().len() < 2 {
                        anyhow::bail!("query must be at least 2 characters");
                    }
                } else if q.is_none() {
                    anyhow::bail!("query is required");
                }
                let limit = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.clamp(1, 500) as usize);
                let outcome = execute_search_outcome(
                    &state.repo_path,
                    repo_name,
                    branch,
                    q.clone(),
                    collection,
                    limit,
                )?;
                Ok(outcome.to_json_envelope())
            }
            "git_read" => {
                let raw = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("path is required"))?;
                let path = safe_repo_rel_path(raw)?;
                let bytes = read_file_from_repo(&repo_full, branch, path)
                    .map_err(|_| anyhow::anyhow!("file not found: {path}"))?;
                let text = String::from_utf8_lossy(&bytes);
                let offset = arguments
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let limit = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(12_000)
                    .clamp(1, 40_000) as usize;
                let slice: String = text.chars().skip(offset).take(limit).collect();
                Ok(json!({
                    "path": path,
                    "offset": offset,
                    "content": slice,
                    "truncated": text.chars().count() > offset + slice.chars().count(),
                }))
            }
            "json_match" => {
                let source = def
                    .source
                    .as_deref()
                    .or_else(|| arguments.get("source").and_then(|v| v.as_str()))
                    .unwrap_or("site-claims.json");
                let source = safe_repo_rel_path(source)?;
                let entries = load_json_entries(&repo_full, branch, source)?;
                let q = arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let id = arguments.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let limit = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.clamp(1, 100) as usize)
                    .unwrap_or(10);
                if id.is_empty() && q.len() < 2 {
                    anyhow::bail!("Provide query (min 2 chars) or id");
                }
                let mut hits = Vec::new();
                for entry in entries {
                    if !id.is_empty() {
                        let eid = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if eid == id {
                            hits.push(entry);
                            break;
                        }
                        continue;
                    }
                    let hay = serde_json::to_string(&entry).unwrap_or_default().to_lowercase();
                    let terms: Vec<&str> = q.split_whitespace().collect();
                    if terms.iter().all(|t| hay.contains(t)) {
                        hits.push(entry);
                        if hits.len() >= limit {
                            break;
                        }
                    }
                }
                Ok(json!({
                    "source": source,
                    "query": if id.is_empty() { Value::String(q) } else { Value::Null },
                    "id": if id.is_empty() { Value::Null } else { Value::String(id.into()) },
                    "count": hits.len(),
                    "claims": hits,
                }))
            }
            "events_lookup" => {
                let source = def
                    .source
                    .as_deref()
                    .unwrap_or("data/events.json");
                let source = safe_repo_rel_path(source)?;
                let entries = load_json_entries(&repo_full, branch, source)?;
                let event_id = arguments
                    .get("eventId")
                    .or_else(|| arguments.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let title_q = arguments
                    .get("titleQuery")
                    .or_else(|| arguments.get("query"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let ref_year = arguments
                    .get("refYear")
                    .and_then(|v| v.as_i64())
                    .unwrap_or_else(current_utc_year);
                let blip_adjust = arguments
                    .get("blipAdjust")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let mut found: Option<Value> = None;
                if !event_id.is_empty() {
                    found = entries.into_iter().find(|e| {
                        e.get("id").and_then(|v| v.as_str()) == Some(event_id)
                    });
                } else if title_q.len() >= 2 {
                    let terms: Vec<&str> = title_q.split_whitespace().collect();
                    found = entries.into_iter().find(|e| {
                        let title = e
                            .get("title")
                            .or_else(|| e.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let hay = format!(
                            "{} {}",
                            title,
                            serde_json::to_string(e).unwrap_or_default().to_lowercase()
                        );
                        terms.iter().all(|t| hay.contains(t))
                    });
                } else {
                    anyhow::bail!("Provide eventId or titleQuery (min 2 chars)");
                }

                let Some(event) = found else {
                    anyhow::bail!("event not found");
                };
                let signed = project_signed_year(&event);
                let mut elapsed = Map::new();
                if let Some(y) = signed {
                    let cal = calendar_years_elapsed(y, ref_year);
                    elapsed.insert("calendarYears".into(), json!(cal));
                    if blip_adjust {
                        let overlap = blip_overlap_years(y, ref_year);
                        elapsed.insert("blipAdjustedYears".into(), json!(cal.saturating_sub(overlap)));
                        elapsed.insert(
                            "note".into(),
                            json!(format!(
                                "blip interval {BLIP_SIGNED_START}..{BLIP_SIGNED_END} omitted when overlapping"
                            )),
                        );
                    }
                    elapsed.insert("refYear".into(), json!(ref_year));
                    elapsed.insert("signedYear".into(), json!(y));
                }

                Ok(json!({
                    "event": event,
                    "projectSignedYear": signed,
                    "elapsed": elapsed,
                    "coverage": if signed.is_some() { "ok" } else { "no_signed_year" },
                }))
            }
            "research_status" => {
                let question = arguments
                    .get("question")
                    .or_else(|| arguments.get("query"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if question.len() < 2 {
                    anyhow::bail!("question must be at least 2 characters");
                }
                let collection = def.collection.as_deref().unwrap_or("index");
                let limit = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.clamp(1, 50) as usize)
                    .unwrap_or(8);
                let hits = execute_search_limited(
                    &state.repo_path,
                    repo_name,
                    branch,
                    Some(Value::String(question.clone())),
                    collection,
                    Some(limit),
                )?;
                let coverage = if hits.is_empty() {
                    "not_in_corpus"
                } else {
                    "partial"
                };
                Ok(json!({
                    "coverage": coverage,
                    "confidence": if hits.is_empty() { "low" } else { "medium" },
                    "searched": { "queries": [question], "toolsCalled": ["research_status", "search"] },
                    "corpusHits": hits,
                    "gaps": if hits.is_empty() {
                        json!(["No sqlite/QUERY hits for this question in the configured collection"])
                    } else {
                        json!([])
                    },
                    "suggestedNextSteps": if hits.is_empty() {
                        json!([{ "action": "broaden_query", "detail": "Try fewer or alternate keywords" }])
                    } else {
                        json!([{ "action": "read_article", "detail": "Call fetch/read_article on top hit paths" }])
                    },
                }))
            }
            other => anyhow::bail!("unsupported tool via: {other}"),
        }
    })();

    match result {
        Ok(data) => json_tool_result(data),
        Err(e) => json_tool_error(e.to_string()),
    }
}

fn handle_jsonrpc(
    state: &AppState,
    repo_name: &str,
    branch: &str,
    manifest: &McpManifest,
    body: &Value,
) -> Value {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Notifications: no JSON-RPC response body required.
    if body.get("id").is_none()
        && (method.starts_with("notifications/") || method == "initialized")
    {
        return json!({});
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": manifest.name.clone().unwrap_or_else(|| format!("relay-{repo_name}")),
                "version": manifest.version.clone().unwrap_or_else(|| "0.1.0".into())
            }
        })),
        "tools/list" | "list_tools" => Ok(tools_list_payload(manifest)),
        "tools/call" | "call_tool" => {
            let params = body.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.is_empty() {
                Err(anyhow::anyhow!("params.name is required"))
            } else {
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                // tools/call errors are returned as tool results with isError (MCP convention),
                // not as JSON-RPC errors, when the method itself is valid.
                Ok(call_tool(state, repo_name, branch, manifest, name, &args))
            }
        }
        "ping" => Ok(json!({})),
        "" => Err(anyhow::anyhow!("missing method")),
        other => Err(anyhow::anyhow!("Method not found: {other}")),
    };

    match result {
        Ok(r) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": r
        }),
        Err(e) => {
            let code = if e.to_string().starts_with("Method not found") {
                -32601
            } else if e.to_string().contains("required") {
                -32602
            } else {
                -32000
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": e.to_string() }
            })
        }
    }
}

fn mcp_enabled(state: &AppState, repo_name: &str, branch: &str) -> bool {
    let Some(repo) = open_repo(&state.repo_path, repo_name) else {
        return false;
    };
    let Some((oid, _, _)) = get_branch_commit_info(&repo, branch) else {
        return false;
    };
    let repo_full = state.repo_path.join(format!("{}.git", repo_name));
    let cfg = read_relay_config(&repo, &oid).unwrap_or_default();
    index_policy::clever_mcp_hook_path(&cfg).is_some()
        || read_file_from_repo(&repo_full, branch, "mcp.yaml").is_ok()
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(body),
    )
        .into_response()
}

/// GET /api/mcp/:repo — health when `?health=1`.
pub async fn get_mcp(
    State(state): State<AppState>,
    AxPath(repo): AxPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if q.get("health").map(|s| s.as_str()) != Some("1") {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST, GET, DELETE")],
            "Use POST for MCP JSON-RPC, GET ?health=1, or DELETE for session teardown",
        )
            .into_response();
    }
    let repo_full = state.repo_path.join(format!("{}.git", repo));
    if !repo_full.exists() {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "error": "repo not found" }),
        );
    }
    let branch = DEFAULT_BRANCH;
    let enabled = mcp_enabled(&state, &repo, branch);
    let (name, version, tool_count) = if enabled {
        let manifest = load_manifest(&repo_full, branch);
        (
            manifest
                .name
                .unwrap_or_else(|| format!("relay-{repo}")),
            manifest.version.unwrap_or_else(|| "0.1.0".into()),
            manifest.tools.len(),
        )
    } else {
        (format!("relay-{repo}"), "0.1.0".into(), 0)
    };
    json_response(
        StatusCode::OK,
        json!({
            "name": name,
            "version": version,
            "status": if enabled { "ok" } else { "disabled" },
            "transport": "streamable-http",
            "stateless": true,
            "mcpEnabled": enabled,
            "toolCount": tool_count,
        }),
    )
}

/// DELETE /api/mcp/:repo — stateless session teardown (noop).
pub async fn delete_mcp(
    State(state): State<AppState>,
    AxPath(repo): AxPath<String>,
) -> Response {
    let repo_full = state.repo_path.join(format!("{}.git", repo));
    if !repo_full.exists() {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "error": "repo not found" }),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /api/mcp/:repo — MCP JSON-RPC (stateless).
pub async fn post_mcp(
    State(state): State<AppState>,
    AxPath(repo): AxPath<String>,
    headers: axum::http::HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let repo_full = state.repo_path.join(format!("{}.git", repo));
    if !repo_full.exists() {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "error": "repo not found" }),
        );
    }

    let branch = headers
        .get(crate::HEADER_BRANCH)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(DEFAULT_BRANCH);

    if !mcp_enabled(&state, &repo, branch) {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": "MCP not enabled for this repo. Add mcp.yaml and/or server.hooks.mcp in .relay.yaml"
            }),
        );
    }

    let Some(Json(body)) = body else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "JSON body required" }),
        );
    };

    let manifest = load_manifest(&repo_full, branch);

    if let Value::Array(batch) = &body {
        let out: Vec<Value> = batch
            .iter()
            .map(|b| handle_jsonrpc(&state, &repo, branch, &manifest, b))
            .filter(|r| r.as_object().map(|o| !o.is_empty()).unwrap_or(true))
            .collect();
        return json_response(StatusCode::OK, Value::Array(out));
    }

    let resp = handle_jsonrpc(&state, &repo, branch, &manifest, &body);
    if resp.as_object().map(|o| o.is_empty()).unwrap_or(false) {
        return StatusCode::ACCEPTED.into_response();
    }
    json_response(StatusCode::OK, resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_alias() {
        let mut tools = HashMap::new();
        tools.insert(
            "search_site".into(),
            McpToolDef {
                aliases: vec!["search".into()],
                via: Some("query".into()),
                ..Default::default()
            },
        );
        let m = McpManifest {
            tools,
            ..Default::default()
        };
        assert!(resolve_tool(&m, "search").is_some());
        assert!(resolve_tool(&m, "search_site").is_some());
        assert!(resolve_tool(&m, "nope").is_none());
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(safe_repo_rel_path("../etc/passwd").is_err());
        assert!(safe_repo_rel_path("ok/file.md").is_ok());
    }

    #[test]
    fn blip_elapsed_math() {
        // From -670 to 1053 is the full phantom span.
        let full = calendar_years_elapsed(BLIP_SIGNED_START, BLIP_SIGNED_END);
        assert!(full > 1700);
        let overlap = blip_overlap_years(-1000, 2000);
        assert_eq!(overlap, full);
        let none = blip_overlap_years(1200, 1800);
        assert_eq!(none, 0);
    }
}

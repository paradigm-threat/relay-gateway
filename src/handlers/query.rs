use crate::git::sqlite_index::{self, QueryOrder, QUERY_LIMIT_MAX};
use crate::handlers::accept::{negotiate_query_format, QueryResponseFormat};
use crate::handlers::query_response::serialize_query_results;
use crate::{helpers, AppState};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use std::collections::HashMap;
use tracing::error;

/// Max length for `q` / FTS string from the URL query string.
const QS_Q_MAX_LEN: usize = 200;

/// Parsed QUERY options from allowlisted URL QS + JSON body (body wins).
#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub collection: String,
    pub query: Option<Value>,
    pub limit: Option<usize>,
    pub order: QueryOrder,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            collection: "index".to_string(),
            query: None,
            limit: None,
            order: QueryOrder::default(),
        }
    }
}

/// Merge allowlisted QS keys, then JSON body overrides. Invalid `order` → Err.
pub fn parse_query_options(
    qs: Option<&HashMap<String, String>>,
    body: Option<&Value>,
    path_query: Option<Value>,
) -> Result<QueryOptions, String> {
    let mut opts = QueryOptions {
        query: path_query,
        ..QueryOptions::default()
    };

    if let Some(qmap) = qs {
        if let Some(q) = qmap.get("q") {
            let trimmed = q.trim();
            if !trimmed.is_empty() {
                let sliced: String = trimmed.chars().take(QS_Q_MAX_LEN).collect();
                opts.query = Some(Value::String(sliced));
            }
        }
        if let Some(c) = qmap.get("collection") {
            let c = c.trim();
            if !c.is_empty() {
                opts.collection = c.to_string();
            }
        }
        if let Some(l) = qmap.get("limit") {
            if let Ok(n) = l.parse::<u64>() {
                opts.limit = Some((n as usize).clamp(1, QUERY_LIMIT_MAX));
            }
        }
        if let Some(o) = qmap.get("order") {
            opts.order = QueryOrder::parse(o)?;
        }
    }

    if let Some(b) = body {
        if let Some(q) = b.get("query") {
            opts.query = Some(q.clone());
        }
        if let Some(c) = b.get("collection").and_then(|v| v.as_str()) {
            if !c.is_empty() {
                opts.collection = c.to_string();
            }
        }
        if let Some(n) = b.get("limit").and_then(|v| v.as_u64()) {
            opts.limit = Some((n as usize).clamp(1, QUERY_LIMIT_MAX));
        }
        if let Some(o) = b.get("order").and_then(|v| v.as_str()) {
            opts.order = QueryOrder::parse(o)?;
        }
    }

    // Final clamp (random hard-cap / default).
    opts.limit = Some(sqlite_index::clamp_query_limit(opts.order, opts.limit));
    Ok(opts)
}

/// Resolve repo for QUERY:
/// - `{repo}.{node}` Host → that repo; full path is optional query text
/// - parked catalog domain Host → that repo (same as GET/SSR)
/// - apex `{node}` Host + first path segment matching a bare repo → that repo (like `/api/mcp/:repo`)
/// - else apex default repo (`RELAY_DEFAULT_REPO` / `relay-master`)
///
/// Unknown Hosts (when a parked map is configured) resolve to `None` — same as GET.
fn resolve_query_repo_and_path(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    path: &str,
) -> Option<(String, String)> {
    if let Some(name) = helpers::repo_from_host(state, state.node_fqdn.as_deref(), headers) {
        return Some((name, path.to_string()));
    }
    if let Some(name) = helpers::repo_from_parked_domain(state, headers) {
        return Some((name, path.to_string()));
    }
    let names = crate::git::repo_names_for_state(state);
    let mut parts = path.splitn(2, '/');
    let first = parts.next().unwrap_or("");
    if !first.is_empty() && names.iter().any(|n| n == first) {
        let rest = parts.next().unwrap_or("").to_string();
        return Some((first.to_string(), rest));
    }
    let has_parked = state
        .parked_domains
        .as_ref()
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if has_parked && !helpers::host_is_node_apex(state.node_fqdn.as_deref(), headers) {
        return None;
    }
    helpers::default_http_repo_from_apex(state).map(|name| (name, path.to_string()))
}

pub async fn handle_query(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
    query: Option<Query<HashMap<String, String>>>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let (repo_name, path_rest) = match resolve_query_repo_and_path(&state, &headers, &path) {
        Some(pair) => pair,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Host must be {repo}.{RELAY_PUBLIC_HOSTNAME}, or path /{repo} on the node apex",
            )
                .into_response();
        }
    };
    let branch = helpers::branch_for_request(&headers, &state, Some(repo_name.as_str()));

    let path_query = if !path_rest.is_empty() && path_rest != "query" {
        Some(serde_json::Value::String(path_rest))
    } else {
        None
    };

    let opts = match parse_query_options(
        query.as_ref().map(|q| &q.0),
        body.as_ref().map(|Json(b)| b),
        path_query,
    ) {
        Ok(o) => o,
        Err(msg) => {
            return (StatusCode::BAD_REQUEST, msg).into_response();
        }
    };

    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());
    let response_format = negotiate_query_format(accept);

    match crate::git::query::execute_query_outcome(
        &state.repo_path,
        &repo_name,
        &branch,
        opts.query,
        &opts.collection,
        opts.limit,
        Some(opts.order),
    ) {
        Ok(outcome) => match response_format {
            QueryResponseFormat::Json => {
                (StatusCode::OK, Json(outcome.to_json_envelope())).into_response()
            }
            fmt => {
                let (body, content_type) = serialize_query_results(fmt, &outcome.results);
                match Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(body))
                {
                    Ok(r) => r.into_response(),
                    Err(e) => {
                        error!(?e, "QUERY response build failed");
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Query response serialization failed",
                        )
                            .into_response()
                    }
                }
            }
        },
        Err(e) => {
            error!(?e, "Query failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Query execution failed").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn qs_only_random_defaults_limit_to_one() {
        let mut qs = HashMap::new();
        qs.insert("order".into(), "random".into());
        let opts = parse_query_options(Some(&qs), None, None).unwrap();
        assert_eq!(opts.order, QueryOrder::Random);
        assert_eq!(opts.limit, Some(1));
    }

    #[test]
    fn body_overrides_qs() {
        let mut qs = HashMap::new();
        qs.insert("order".into(), "random".into());
        qs.insert("limit".into(), "5".into());
        qs.insert("collection".into(), "events".into());
        let body = json!({
            "order": "-path",
            "limit": 20,
            "collection": "index",
            "query": "venus"
        });
        let opts = parse_query_options(Some(&qs), Some(&body), None).unwrap();
        assert_eq!(opts.order, QueryOrder::PathDesc);
        assert_eq!(opts.limit, Some(20));
        assert_eq!(opts.collection, "index");
        assert_eq!(opts.query, Some(json!("venus")));
    }

    #[test]
    fn invalid_order_errors() {
        let mut qs = HashMap::new();
        qs.insert("order".into(), "drop-table".into());
        assert!(parse_query_options(Some(&qs), None, None).is_err());
    }

    #[test]
    fn random_limit_capped_at_100() {
        let body = json!({ "order": "random", "limit": 9999 });
        let opts = parse_query_options(None, Some(&body), None).unwrap();
        assert_eq!(opts.limit, Some(100));
    }
}

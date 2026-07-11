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
use std::collections::HashMap;
use tracing::error;

/// Resolve repo for QUERY:
/// - `{repo}.{node}` Host → that repo; full path is optional query text
/// - apex `{node}` Host + first path segment matching a bare repo → that repo (like `/api/mcp/:repo`)
/// - else apex default repo (`RELAY_DEFAULT_REPO` / `relay-master`)
fn resolve_query_repo_and_path(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    path: &str,
) -> Option<(String, String)> {
    if let Some(name) = helpers::repo_from_host(state, state.node_fqdn.as_deref(), headers) {
        return Some((name, path.to_string()));
    }
    let names = crate::git::repo_names_for_state(state);
    let mut parts = path.splitn(2, '/');
    let first = parts.next().unwrap_or("");
    if !first.is_empty() && names.iter().any(|n| n == first) {
        let rest = parts.next().unwrap_or("").to_string();
        return Some((first.to_string(), rest));
    }
    helpers::default_http_repo_from_apex(state).map(|name| (name, path.to_string()))
}

pub async fn handle_query(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
    _query: Option<Query<HashMap<String, String>>>,
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

    let mut query_val = if !path_rest.is_empty() && path_rest != "query" {
        Some(serde_json::Value::String(path_rest))
    } else {
        None
    };

    let mut collection_storage = "index".to_string();
    let mut limit: Option<usize> = None;

    if let Some(Json(b)) = body {
        if let Some(q) = b.get("query") {
            query_val = Some(q.clone());
        }
        if let Some(c) = b.get("collection").and_then(|v| v.as_str()) {
            collection_storage = c.to_string();
        }
        if let Some(n) = b.get("limit").and_then(|v| v.as_u64()) {
            limit = Some(n.clamp(1, 10_000) as usize);
        }
    }

    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());
    let response_format = negotiate_query_format(accept);

    match crate::git::query::execute_query_limited(
        &state.repo_path,
        &repo_name,
        &branch,
        query_val,
        &collection_storage,
        limit,
    ) {
        Ok(rows) => match response_format {
            QueryResponseFormat::Json => {
                (StatusCode::OK, Json(serde_json::json!({ "results": rows }))).into_response()
            }
            fmt => {
                let (body, content_type) = serialize_query_results(fmt, &rows);
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

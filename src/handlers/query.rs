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

pub async fn handle_query(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
    _query: Option<Query<HashMap<String, String>>>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let repo_name_opt =
        helpers::repo_from_host_or_default(&state.repo_path, state.node_fqdn.as_deref(), &headers);
    let repo_name = match repo_name_opt {
        Some(name) => name,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Host must be {repo}.{RELAY_PUBLIC_HOSTNAME}",
            )
                .into_response();
        }
    };
    let branch = helpers::branch_for_request(&headers, &state.repo_path, Some(repo_name.as_str()));

    let mut query_val = if !path.is_empty() && path != "query" {
        Some(serde_json::Value::String(path))
    } else {
        None
    };

    let mut collection_storage = "index".to_string();

    if let Some(Json(b)) = body {
        if let Some(q) = b.get("query") {
            query_val = Some(q.clone());
        }
        if let Some(c) = b.get("collection").and_then(|v| v.as_str()) {
            collection_storage = c.to_string();
        }
    }

    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());
    let response_format = negotiate_query_format(accept);

    match crate::git::query::execute_query(
        &state.repo_path,
        &repo_name,
        &branch,
        query_val,
        &collection_storage,
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

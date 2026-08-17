//! Node-FQDN operator `/api/*` vs Host-selected repo files.

use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{Path as AxPath, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};

use crate::types::AppState;

use super::{delete_file, handle_get_file, head_file, helpers, put_file};

/// If `Host` is not the node ops surface, serve the site path (GET/HEAD/PUT/DELETE) or 404 (POST/other).
pub async fn if_not_node_ops(
    state: AppState,
    method: Method,
    headers: HeaderMap,
    site_path: String,
    query: Option<Query<HashMap<String, String>>>,
    body: Bytes,
) -> Option<Response> {
    if helpers::host_is_node_ops_api(state.node_fqdn.as_deref(), &headers) {
        return None;
    }
    Some(dispatch_site(state, method, headers, site_path, query, body).await)
}

pub async fn dispatch_site(
    state: AppState,
    method: Method,
    headers: HeaderMap,
    site_path: String,
    query: Option<Query<HashMap<String, String>>>,
    body: Bytes,
) -> Response {
    match method {
        Method::GET => handle_get_file(State(state), headers, AxPath(site_path), query)
            .await
            .into_response(),
        Method::HEAD => head_file(State(state), headers, AxPath(site_path), query)
            .await
            .into_response(),
        Method::PUT => put_file(State(state), headers, AxPath(site_path), query, body)
            .await
            .into_response(),
        Method::DELETE => delete_file(State(state), headers, AxPath(site_path), query)
            .await
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub fn empty_head(resp: Response) -> Response {
    let (parts, _) = resp.into_parts();
    Response::from_parts(parts, axum::body::Body::empty())
}

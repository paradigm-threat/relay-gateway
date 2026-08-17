//! Cache-backed HTTP gateway: `GET /ipfs/{cid}`.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::collections::HashMap;

use crate::git::ipfs::{
    asset_error_response, asset_status_response, fetch_cid_bytes, parse_wait_seconds,
    wants_asset_status_only,
};
use crate::handlers::helpers;
use crate::types::AppState;

pub async fn handle_get_ipfs(
    State(_state): State<AppState>,
    headers: HeaderMap,
    Path(cid): Path<String>,
    query: Option<Query<HashMap<String, String>>>,
) -> impl IntoResponse {
    let cid = cid.trim().trim_start_matches('/').to_string();
    let qmap = query.map(|q| q.0).unwrap_or_default();
    if wants_asset_status_only(&qmap, &headers) {
        return asset_status_response(&cid, "", "", &format!("/ipfs/{cid}")).into_response();
    }
    let wait_secs = parse_wait_seconds(&qmap, &headers);

    match fetch_cid_bytes(&cid, None, wait_secs) {
        Ok((bytes, ct)) => {
            let content_type = ct.unwrap_or_else(|| "application/octet-stream".to_string());
            (
                StatusCode::OK,
                [
                    ("Content-Type", content_type),
                    ("Cache-Control", "public, max-age=604800".to_string()),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) => asset_error_response(&e, "", "", &format!("/ipfs/{}", cid)),
    }
}

pub async fn handle_get_ipfs_prefixed(
    state: State<AppState>,
    headers: HeaderMap,
    path: axum::extract::Path<String>,
    query: Option<Query<HashMap<String, String>>>,
) -> impl IntoResponse {
    let cid = helpers::url_decode(&path.0)
        .decode_utf8_lossy()
        .trim_start_matches("ipfs/")
        .to_string();
    handle_get_ipfs(state, headers, Path(cid), query).await
}

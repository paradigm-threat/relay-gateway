//! Cache-backed HTTP gateway: `GET /ipfs/{cid}`.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use std::collections::HashMap;

use crate::git::ipfs::{
    asset_error_response, asset_status_response, parse_wait_seconds,
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
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok());

    match crate::git::ipfs::fetch_cid_http(&cid, None, wait_secs, range) {
        Ok(http) => {
            let content_type = http
                .content_type
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let mut res = (
                http.status,
                [
                    ("Content-Type", content_type),
                    ("Cache-Control", "public, max-age=604800".to_string()),
                ],
                http.bytes,
            )
                .into_response();
            let h = res.headers_mut();
            if let Some(cr) = http.content_range {
                if let Ok(v) = cr.parse() {
                    h.insert(axum::http::header::CONTENT_RANGE, v);
                }
            }
            if let Some(ar) = http.accept_ranges {
                if let Ok(v) = ar.parse() {
                    h.insert(axum::http::header::ACCEPT_RANGES, v);
                }
            }
            res
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

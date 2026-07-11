//! In-process HTTP dispatch for embedded desktop clients.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use tower::ServiceExt;

/// Dispatch one HTTP request through the relay router (async).
pub async fn dispatch_request(router: Router, req: Request<Body>) -> Response {
    match router.oneshot(req).await {
        Ok(resp) => resp,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Dispatch one HTTP request on a shared Tokio runtime (blocking caller).
pub fn dispatch_request_blocking(
    runtime: &tokio::runtime::Runtime,
    router: Router,
    req: Request<Body>,
) -> Response {
    runtime.block_on(dispatch_request(router, req))
}

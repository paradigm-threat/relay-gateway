//! Axum router shared by the CLI gateway and embedded desktop server.

use axum::{
    body::Body,
    extract::{Path as AxPath, State},
    http::{header, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::{handlers, transpiler, AppState};

/// Build the relay HTTP router (GET/PUT/QUERY/hooks/api).
pub fn build_router(state: AppState) -> Router {
    let acme_dir = std::env::var("RELAY_ACME_DIR").unwrap_or_else(|_| "/var/www/certbot".to_string());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    Router::new()
        .route("/openapi.yaml", get(handlers::get_openapi_yaml))
        .route("/swagger-ui", get(handlers::get_swagger_ui))
        .route("/api/config", post(handlers::post_api_config))
        .route("/api/status", post(handlers::post_api_status))
        .route(
            "/api/mcp/:repo",
            get(handlers::mcp::get_mcp)
                .post(handlers::mcp::post_mcp)
                .delete(handlers::mcp::delete_mcp),
        )
        .route(
            "/api/git/ensure-bare/:repo",
            post(handlers::post_ensure_bare),
        )
        .route("/api/index/warm/:repo", post(handlers::post_index_warm))
        .route("/git-pull", post(handlers::post_git_pull))
        .route("/hooks/peer", post(handlers::post_peer_hook))
        .route("/hooks/mesh-event", post(handlers::post_mesh_event))
        .route("/hooks/github/:repo", post(handlers::post_github_hook))
        .route("/transpile", post(transpiler::post_transpile))
        .route(
            "/.well-known/acme-challenge/*path",
            get({
                let dir = acme_dir.clone();
                move |AxPath(path): AxPath<String>| async move {
                    handlers::serve_acme_challenge(&dir, &path).await
                }
            }),
        )
        .route(
            "/",
            get(handlers::get_root)
                .head(handlers::head_root)
                .options(handlers::options_capabilities)
                // QUERY on `/` (repo subdomain hosts) — without this, Axum returns 405.
                .fallback(relay_path_fallback),
        )
        .route("/ipfs/*cid", get(handlers::handle_get_ipfs_prefixed))
        .route(
            "/*path",
            get(handlers::handle_get_file)
                .head(handlers::head_file)
                .put(handlers::put_file)
                .delete(handlers::delete_file)
                .options(handlers::options_capabilities)
                .fallback(relay_path_fallback),
        )
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn relay_path_fallback(
    State(state): State<AppState>,
    req: Request<Body>,
) -> axum::response::Response {
    if req.method().as_str() != "QUERY" {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD, PUT, DELETE, OPTIONS, QUERY")],
            "Method Not Allowed",
        )
            .into_response();
    }
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().trim_start_matches('/').to_string();
    let query = if parts.uri.query().is_some() {
        axum::extract::Query::<std::collections::HashMap<String, String>>::try_from_uri(&parts.uri)
            .ok()
    } else {
        None
    };
    let headers = parts.headers;
    let json = match axum::body::to_bytes(body, 2 * 1024 * 1024).await {
        Ok(b) if b.is_empty() => None,
        Ok(b) => serde_json::from_slice::<serde_json::Value>(&b)
            .ok()
            .map(Json),
        Err(_) => None,
    };
    handlers::handle_query(State(state), headers, AxPath(path), query, json)
        .await
        .into_response()
}

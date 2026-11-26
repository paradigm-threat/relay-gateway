//! Axum router shared by the CLI gateway and embedded desktop server.

use std::time::Duration;

use axum::{
    body::Body,
    extract::{Path as AxPath, State},
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::{handlers, transpiler, AppState};

/// Build the relay HTTP router (GET/PUT/QUERY/hooks/api).
pub fn build_router(state: AppState) -> Router {
    let acme_dir =
        std::env::var("RELAY_ACME_DIR").unwrap_or_else(|_| "/var/www/certbot".to_string());

    // Open CORS for mesh browsers (Vercel SPA, parked sites, WASM QUERY).
    // Do not enable credentials: Allow-Origin: * is invalid with cookies.
    let query_method = Method::from_bytes(b"QUERY").expect("QUERY is a valid method token");
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            query_method,
        ])
        .allow_headers(Any)
        .expose_headers(Any)
        .max_age(Duration::from_secs(86400));

    Router::new()
        .route("/openapi.yaml", get(handlers::get_openapi_yaml))
        .route("/swagger-ui", get(handlers::get_swagger_ui))
        .route(
            "/api/config",
            get(handlers::api_config)
                .head(handlers::api_config)
                .post(handlers::api_config)
                .put(handlers::api_config)
                .delete(handlers::api_config),
        )
        .route(
            "/api/status",
            get(handlers::api_status)
                .head(handlers::api_status)
                .post(handlers::api_status)
                .put(handlers::api_status)
                .delete(handlers::api_status),
        )
        .route(
            "/api/rum",
            get(handlers::api_rum)
                .head(handlers::api_rum)
                .post(handlers::api_rum),
        )
        .route(
            "/api/hits",
            get(handlers::api_hits)
                .head(handlers::api_hits)
                .post(handlers::api_hits),
        )
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
        .route(
            "/api/sync/mesh/:repo",
            get(handlers::api_mesh_sync)
                .post(handlers::api_mesh_sync),
        )
        .route("/git-pull", post(handlers::post_git_pull))
        .route("/hooks/peer", post(handlers::post_peer_hook))
        .route("/hooks/park-tls", post(crate::park_tls::post_park_tls_hook))
        .route("/hooks/mesh-event", post(handlers::post_mesh_event))
        .route("/hooks/github/:repo", post(handlers::post_github_hook))
        .route("/transpile", post(transpiler::post_transpile))
        .route(
            "/.well-known/acme-challenge/*path",
            get({
                let dir = acme_dir.clone();
                move |AxPath(path): AxPath<String>| async move {
                    crate::park_tls::serve_challenge(&dir, &path).await
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
        // Compress QUERY catalogs + page.md for browsers. Never gzip/br movies or audio —
        // that breaks byte ranges and stalls the first frame.
        .layer(CompressionLayer::new().compress_when(
            DefaultPredicate::new()
                .and(NotForContentType::const_new("video/"))
                .and(NotForContentType::const_new("audio/"))
                .and(NotForContentType::const_new("application/octet-stream")),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(crate::http_stats::http_stats_middleware))
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

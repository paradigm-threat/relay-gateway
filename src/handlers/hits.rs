//! Public coarse hit counters (`GET`/`POST` `/api/hits`).
//!
//! Affirmation-gated only (bots that skip the ownership checkbox do not count):
//! - **POST `site_view`:** welcome checkbox on the front page (localStorage-gated).
//! - **POST `movie_view`:** title affirmation on a movie page (localStorage-gated).
//! - Not playback, not VLC/open clicks, not raw HTML GETs for public totals.
//!
//! **GET:** fleet-summed unique site + optional per-path movie counters; mesh via
//! `?local=1` (no recursion). Optional `html_*` from middleware remains for ops.
//!
//! No IPs, cookies, or unique-visitor fingerprints in this API.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::http_stats;
use crate::AppState;

const SCHEMA: &str = "relay.hits.v1";
const CACHE_TTL: Duration = Duration::from_secs(60);
const PEER_TIMEOUT: Duration = Duration::from_millis(800);
const MOVIE_LIST_CAP: usize = 24;
const PATH_MAX: usize = 160;
/// Internal path key for welcome / front-page checkbox unique views.
const SITE_PATH: &str = "__site__";

static MOVIE_DB: OnceLock<Mutex<Connection>> = OnceLock::new();
static FLEET_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Value)>>> = OnceLock::new();

/// Site pages that must never count as movie unique views.
const DEFAULT_EXCLUDE: &[&str] = &[
    "/",
    "/setup",
    "/submit",
    "/board",
    "/legal",
    "/terms",
    "/request",
    "/motivation",
    "/home",
    "/index",
];

#[derive(Debug, Deserialize)]
pub struct HitsQuery {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub local: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HitsPostBody {
    kind: String,
    #[serde(default)]
    path: Option<String>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_minute() -> i64 {
    now_unix() / 60
}

fn wants_local(q: &HitsQuery) -> bool {
    q.local
        .as_deref()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn master_peer_hosts() -> Vec<String> {
    std::env::var("RELAY_MASTER_PEER_LIST")
        .unwrap_or_default()
        .split(';')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn hits_dir() -> PathBuf {
    std::env::var("RELAY_HTTP_STATS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/relay/state/http_stats"))
}

fn open_movie_db() -> Result<Connection, String> {
    let dir = hits_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = Connection::open(dir.join("movie_views.sqlite")).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS movie_views (
            minute INTEGER NOT NULL,
            host TEXT NOT NULL,
            path TEXT NOT NULL,
            count INTEGER NOT NULL,
            PRIMARY KEY (minute, host, path)
         );
         CREATE INDEX IF NOT EXISTS movie_views_host_path_minute
           ON movie_views(host, path, minute);",
    )
    .map_err(|e| e.to_string())?;
    let _ = conn.execute(
        "DELETE FROM movie_views WHERE minute < ?1",
        params![now_minute() - (60 * 24 * 40)],
    );
    Ok(conn)
}

fn movie_db() -> Result<&'static Mutex<Connection>, String> {
    if let Some(m) = MOVIE_DB.get() {
        return Ok(m);
    }
    let conn = open_movie_db()?;
    let _ = MOVIE_DB.set(Mutex::new(conn));
    MOVIE_DB
        .get()
        .ok_or_else(|| "movie views db init race".to_string())
}

fn host_from_headers(headers: &HeaderMap, q: &HitsQuery) -> String {
    if let Some(h) = q.host.as_deref() {
        let s = http_stats::sanitize_host(h);
        if !s.is_empty() {
            return s;
        }
    }
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(http_stats::sanitize_host)
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Single-segment movie slug path: `/the-mummy-1999-debugged`.
pub fn sanitize_movie_path(raw: &str) -> Option<String> {
    let p = raw.split(['?', '#']).next().unwrap_or(raw).trim();
    let p = if p.is_empty() { "/" } else { p };
    let mut out = String::new();
    for c in p.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.') {
            out.push(c);
        }
    }
    if out.is_empty() || out.len() > PATH_MAX {
        return None;
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    if out.ends_with('/') && out.len() > 1 {
        out.pop();
    }
    if out == "/" {
        return None;
    }
    // One segment only (no nested paths).
    let rest = out.trim_start_matches('/');
    if rest.is_empty() || rest.contains('/') || rest.contains('.') {
        return None;
    }
    for ex in DEFAULT_EXCLUDE {
        if out.eq_ignore_ascii_case(ex) {
            return None;
        }
    }
    Some(out.to_ascii_lowercase())
}

fn increment_movie_view(host: &str, path: &str) -> Result<(), String> {
    let db = movie_db()?;
    let g = db.lock().map_err(|e| e.to_string())?;
    let minute = now_minute();
    g.execute(
        "INSERT INTO movie_views(minute, host, path, count) VALUES(?1,?2,?3,1)
         ON CONFLICT(minute, host, path) DO UPDATE SET count = count + 1",
        params![minute, host, path],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn movie_views_window(host: &str, path: Option<&str>, minutes: u64) -> u64 {
    let Ok(db) = movie_db() else {
        return 0;
    };
    let Ok(g) = db.lock() else {
        return 0;
    };
    let cutoff = now_minute().saturating_sub(minutes.max(1).saturating_sub(1) as i64);
    if let Some(p) = path {
        g.query_row(
            "SELECT COALESCE(SUM(count),0) FROM movie_views
             WHERE host = ?1 AND path = ?2 AND minute >= ?3",
            params![host, p, cutoff],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as u64)
        .unwrap_or(0)
    } else {
        g.query_row(
            "SELECT COALESCE(SUM(count),0) FROM movie_views
             WHERE host = ?1 AND minute >= ?2",
            params![host, cutoff],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as u64)
        .unwrap_or(0)
    }
}

fn top_movies(host: &str, minutes: u64, cap: usize) -> Vec<Value> {
    let Ok(db) = movie_db() else {
        return Vec::new();
    };
    let Ok(g) = db.lock() else {
        return Vec::new();
    };
    let cutoff_24 = now_minute().saturating_sub(minutes.max(1).saturating_sub(1) as i64);
    let cutoff_1h = now_minute().saturating_sub(59);
    let Ok(mut stmt) = g.prepare(
        "SELECT path,
                SUM(CASE WHEN minute >= ?3 THEN count ELSE 0 END) AS c1,
                SUM(count) AS c24
         FROM movie_views
         WHERE host = ?1 AND minute >= ?2 AND path != ?4
         GROUP BY path ORDER BY c24 DESC LIMIT ?5",
    ) else {
        return Vec::new();
    };
    let rows = stmt
        .query_map(
            params![host, cutoff_24, cutoff_1h, SITE_PATH, cap as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                ))
            },
        )
        .ok();
    let Some(rows) = rows else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (path, c1, c24) = row;
        out.push(json!({
            "path": path,
            "unique_1h": c1,
            "unique_24h": c24,
        }));
    }
    out
}

fn local_payload(host: &str, path: Option<&str>) -> Value {
    let enabled = http_stats::enabled();
    let html_1h = if enabled {
        http_stats::html_pageviews(Some(host), 60)
    } else {
        0
    };
    let html_24h = if enabled {
        http_stats::html_pageviews(Some(host), 60 * 24)
    } else {
        0
    };
    let site_1h = movie_views_window(host, Some(SITE_PATH), 60);
    let site_24h = movie_views_window(host, Some(SITE_PATH), 60 * 24);
    let mut body = json!({
        "schema": SCHEMA,
        "enabled": enabled,
        "host": host,
        // Affirmation-gated site uniques (welcome checkbox) — public footer metric.
        "site_unique_1h": site_1h,
        "site_unique_24h": site_24h,
        // Raw HTML GETs — ops only; not used for public “views”.
        "html_1h": html_1h,
        "html_24h": html_24h,
        "nodes": 1,
        "asOf": now_unix(),
        "cached": false,
        "local": true,
    });
    if let Some(p) = path {
        let u1 = movie_views_window(host, Some(p), 60);
        let u24 = movie_views_window(host, Some(p), 60 * 24);
        body["path"] = json!(p);
        body["unique_1h"] = json!(u1);
        body["unique_24h"] = json!(u24);
    } else {
        body["movies"] = json!(top_movies(host, 60 * 24, MOVIE_LIST_CAP));
        let movie_only = movie_views_window(host, None, 60 * 24).saturating_sub(site_24h);
        body["movie_unique_24h"] = json!(movie_only);
    }
    body
}

fn cache_key(host: &str, path: Option<&str>, local: bool) -> String {
    format!(
        "{}|{}|{}",
        host,
        path.unwrap_or(""),
        if local { "L" } else { "F" }
    )
}

fn cache_get(key: &str) -> Option<Value> {
    let map = FLEET_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(g) = map.lock() else {
        return None;
    };
    let (at, v) = g.get(key)?;
    if at.elapsed() > CACHE_TTL {
        return None;
    }
    let mut out = v.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("cached".into(), json!(true));
    }
    Some(out)
}

fn cache_put(key: String, v: Value) {
    let map = FLEET_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut g) = map.lock() {
        g.insert(key, (Instant::now(), v));
        if g.len() > 64 {
            g.retain(|_, (t, _)| t.elapsed() <= CACHE_TTL.saturating_mul(2));
        }
    }
}

async fn fetch_peer_local(
    client: &reqwest::Client,
    peer: &str,
    host: &str,
    path: Option<&str>,
) -> Option<Value> {
    let mut url = format!(
        "https://{}/api/hits?local=1&host={}",
        peer.trim(),
        urlencoding_host(host)
    );
    if let Some(p) = path {
        url.push_str("&path=");
        url.push_str(&urlencoding_path(p));
    }
    let resp = client
        .get(&url)
        .timeout(PEER_TIMEOUT)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

fn urlencoding_host(h: &str) -> String {
    h.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

fn urlencoding_path(p: &str) -> String {
    p.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_string()
            } else if c == '/' {
                "%2F".into()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

fn merge_fleet(local: Value, peers: Vec<Value>) -> Value {
    let mut html_1h = local.get("html_1h").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut html_24h = local.get("html_24h").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut site_1h = local
        .get("site_unique_1h")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut site_24h = local
        .get("site_unique_24h")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut unique_1h = local.get("unique_1h").and_then(|v| v.as_u64());
    let mut unique_24h = local.get("unique_24h").and_then(|v| v.as_u64());
    let mut movie_unique_24h = local
        .get("movie_unique_24h")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut nodes = 1u64;
    let mut movie_map: HashMap<String, (u64, u64)> = HashMap::new();
    if let Some(arr) = local.get("movies").and_then(|v| v.as_array()) {
        for m in arr {
            if let (Some(p), Some(u1), Some(u24)) = (
                m.get("path").and_then(|v| v.as_str()),
                m.get("unique_1h").and_then(|v| v.as_u64()),
                m.get("unique_24h").and_then(|v| v.as_u64()),
            ) {
                movie_map.insert(p.to_string(), (u1, u24));
            }
        }
    }
    for peer in peers {
        if peer.get("enabled") == Some(&json!(false)) {
            continue;
        }
        nodes = nodes.saturating_add(1);
        html_1h = html_1h.saturating_add(peer.get("html_1h").and_then(|v| v.as_u64()).unwrap_or(0));
        html_24h =
            html_24h.saturating_add(peer.get("html_24h").and_then(|v| v.as_u64()).unwrap_or(0));
        site_1h = site_1h.saturating_add(
            peer.get("site_unique_1h")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );
        site_24h = site_24h.saturating_add(
            peer.get("site_unique_24h")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );
        if let Some(u) = peer.get("unique_1h").and_then(|v| v.as_u64()) {
            unique_1h = Some(unique_1h.unwrap_or(0).saturating_add(u));
        }
        if let Some(u) = peer.get("unique_24h").and_then(|v| v.as_u64()) {
            unique_24h = Some(unique_24h.unwrap_or(0).saturating_add(u));
        }
        movie_unique_24h = movie_unique_24h.saturating_add(
            peer.get("movie_unique_24h")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );
        if let Some(arr) = peer.get("movies").and_then(|v| v.as_array()) {
            for m in arr {
                if let (Some(p), Some(u1), Some(u24)) = (
                    m.get("path").and_then(|v| v.as_str()),
                    m.get("unique_1h").and_then(|v| v.as_u64()),
                    m.get("unique_24h").and_then(|v| v.as_u64()),
                ) {
                    let e = movie_map.entry(p.to_string()).or_insert((0, 0));
                    e.0 = e.0.saturating_add(u1);
                    e.1 = e.1.saturating_add(u24);
                }
            }
        }
    }
    let mut movies: Vec<(String, u64, u64)> = movie_map
        .into_iter()
        .map(|(p, (u1, u24))| (p, u1, u24))
        .collect();
    movies.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    movies.truncate(MOVIE_LIST_CAP);
    let movies_json: Vec<Value> = movies
        .into_iter()
        .map(|(path, unique_1h, unique_24h)| {
            json!({
                "path": path,
                "unique_1h": unique_1h,
                "unique_24h": unique_24h,
            })
        })
        .collect();

    let mut out = json!({
        "schema": SCHEMA,
        "enabled": local.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        "host": local.get("host").cloned().unwrap_or(json!("")),
        "site_unique_1h": site_1h,
        "site_unique_24h": site_24h,
        "html_1h": html_1h,
        "html_24h": html_24h,
        "nodes": nodes,
        "asOf": now_unix(),
        "cached": false,
        "local": false,
    });
    if let Some(p) = local.get("path") {
        out["path"] = p.clone();
        if let Some(u) = unique_1h {
            out["unique_1h"] = json!(u);
        }
        if let Some(u) = unique_24h {
            out["unique_24h"] = json!(u);
        }
    } else {
        out["movies"] = json!(movies_json);
        out["movie_unique_24h"] = json!(movie_unique_24h);
    }
    out
}

/// GET/HEAD/POST `/api/hits`.
pub async fn api_hits(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Query(q): Query<HitsQuery>,
    body: Bytes,
) -> Response {
    if method == Method::POST {
        return post_movie_view(&headers, &q, &body);
    }
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD, POST")],
            "Use GET, HEAD, or POST for /api/hits",
        )
            .into_response();
    }

    let host = host_from_headers(&headers, &q);
    if host.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing host" })),
        )
            .into_response();
    }
    let path = q
        .path
        .as_deref()
        .and_then(sanitize_movie_path);
    let local_only = wants_local(&q);
    let key = cache_key(&host, path.as_deref(), local_only);
    if !local_only {
        if let Some(cached) = cache_get(&key) {
            if method == Method::HEAD {
                return StatusCode::OK.into_response();
            }
            return Json(cached).into_response();
        }
    }

    let local = local_payload(&host, path.as_deref());
    let out = if local_only {
        local
    } else {
        let peers = master_peer_hosts();
        let self_fqdn = state
            .node_fqdn
            .clone()
            .or_else(|| crate::mesh_role::node_fqdn_normalized());
        let client = reqwest::Client::builder()
            .timeout(PEER_TIMEOUT)
            .build()
            .ok();
        let mut peer_vals = Vec::new();
        if let Some(client) = client {
            let mut futs = Vec::new();
            for peer in &peers {
                if self_fqdn
                    .as_deref()
                    .map(|s| s.eq_ignore_ascii_case(peer))
                    .unwrap_or(false)
                {
                    continue;
                }
                futs.push(fetch_peer_local(&client, peer, &host, path.as_deref()));
            }
            let results = futures_util::future::join_all(futs).await;
            for r in results.into_iter().flatten() {
                peer_vals.push(r);
            }
        }
        let merged = merge_fleet(local, peer_vals);
        cache_put(key, merged.clone());
        merged
    };

    if method == Method::HEAD {
        return StatusCode::OK.into_response();
    }
    Json(out).into_response()
}

fn post_movie_view(headers: &HeaderMap, q: &HitsQuery, body: &Bytes) -> Response {
    if !http_stats::enabled() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "hits disabled" })),
        )
            .into_response();
    }
    let parsed: HitsPostBody = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let host = host_from_headers(headers, q);
    if host.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing host" })),
        )
            .into_response();
    }
    let path = match parsed.kind.as_str() {
        "site_view" => SITE_PATH.to_string(),
        "movie_view" => {
            let Some(raw) = parsed.path.as_deref() else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "ok": false, "error": "path required for movie_view" })),
                )
                    .into_response();
            };
            let Some(p) = sanitize_movie_path(raw) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "ok": false, "error": "invalid path" })),
                )
                    .into_response();
            };
            p
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "kind must be site_view or movie_view" })),
            )
                .into_response();
        }
    };
    match increment_movie_view(&host, &path) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!(target: "relay_hits", error = %e, "hit increment failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": e })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_movie_path_accepts_slug() {
        assert_eq!(
            sanitize_movie_path("/The-Mummy-1999-Debugged"),
            Some("/the-mummy-1999-debugged".into())
        );
    }

    #[test]
    fn sanitize_movie_path_rejects_site_and_nested() {
        assert!(sanitize_movie_path("/setup").is_none());
        assert!(sanitize_movie_path("/a/b").is_none());
        assert!(sanitize_movie_path("/foo.bar").is_none());
        assert!(sanitize_movie_path("/").is_none());
    }
}

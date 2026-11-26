//! Optional Real User Monitoring ingest (`POST /api/rum`).
//!
//! Off by default. Enable with **`RELAY_RUM_ENABLED=1`**. Store under
//! **`RELAY_RUM_DIR`** (default `/opt/relay/rum`) as SQLite `rum.sqlite`.
//! No cookies / PII — path, embed OID, vitals, and typed theater events only.
//!
//! Registered on the Axum router (not tip files), so parked Hosts can same-origin beacon.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

static RUM_DB: OnceLock<Mutex<Connection>> = OnceLock::new();

fn rum_enabled() -> bool {
    matches!(
        std::env::var("RELAY_RUM_ENABLED")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn rum_dir() -> PathBuf {
    std::env::var("RELAY_RUM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/relay/rum"))
}

fn rum_sample_rate() -> f64 {
    let rate: f64 = std::env::var("RELAY_RUM_SAMPLE_RATE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    rate.clamp(0.0, 1.0)
}

fn rum_retention_days() -> i64 {
    std::env::var("RELAY_RUM_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .max(1)
}

fn open_db() -> Result<Connection, String> {
    let dir = rum_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("rum dir: {e}"))?;
    let path = dir.join("rum.sqlite");
    let conn = Connection::open(&path).map_err(|e| format!("rum open: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rum_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            path TEXT,
            embed_oid TEXT,
            hydrate_ok INTEGER,
            ttfc_ms REAL,
            lcp_ms REAL,
            inp_ms REAL,
            cls REAL
         );
         CREATE INDEX IF NOT EXISTS rum_events_ts ON rum_events(ts);
         CREATE TABLE IF NOT EXISTS rum_theater_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            severity TEXT NOT NULL,
            code TEXT NOT NULL,
            path TEXT,
            rendition TEXT,
            engine TEXT,
            message TEXT
         );
         CREATE INDEX IF NOT EXISTS rum_theater_events_ts ON rum_theater_events(ts);
         CREATE INDEX IF NOT EXISTS rum_theater_events_sev_code ON rum_theater_events(severity, code);",
    )
    .map_err(|e| format!("rum schema: {e}"))?;
    Ok(conn)
}

fn with_db<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&Connection) -> Result<T, String>,
{
    let mutex = RUM_DB.get_or_init(|| {
        Mutex::new(
            open_db().unwrap_or_else(|e| {
                tracing::error!(target: "relay_rum", error = %e, "rum db open failed");
                // Open in-memory fallback so lock always has a connection; writes may be lost.
                Connection::open_in_memory().expect("in-memory sqlite")
            }),
        )
    });
    let guard = mutex.lock().map_err(|_| "rum lock poisoned".to_string())?;
    f(&guard)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn prune(conn: &Connection) -> Result<(), String> {
    let cutoff = now_unix() - rum_retention_days() * 86_400;
    conn.execute("DELETE FROM rum_events WHERE ts < ?1", params![cutoff])
        .map_err(|e| format!("rum prune: {e}"))?;
    conn.execute(
        "DELETE FROM rum_theater_events WHERE ts < ?1",
        params![cutoff],
    )
    .map_err(|e| format!("rum theater prune: {e}"))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RumBatch {
    #[serde(default)]
    events: Vec<RumEvent>,
}

#[derive(Debug, Default, Deserialize)]
struct RumEvent {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    rendition: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    embed_oid: Option<String>,
    #[serde(default)]
    hydrate_ok: Option<bool>,
    #[serde(default)]
    ttfc_ms: Option<f64>,
    #[serde(default)]
    lcp_ms: Option<f64>,
    #[serde(default)]
    inp_ms: Option<f64>,
    #[serde(default)]
    cls: Option<f64>,
}

fn is_theater_event(ev: &RumEvent) -> bool {
    if ev
        .kind
        .as_deref()
        .map(|k| k.eq_ignore_ascii_case("theater"))
        .unwrap_or(false)
    {
        return true;
    }
    ev.code.as_ref().is_some_and(|c| !c.trim().is_empty())
        && ev
            .severity
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
}

fn normalize_severity(s: &str) -> &'static str {
    match s.trim().to_ascii_lowercase().as_str() {
        "major" => "major",
        _ => "minor",
    }
}

fn insert_theater(conn: &Connection, ts: i64, ev: &RumEvent) -> Result<(), String> {
    let severity = normalize_severity(ev.severity.as_deref().unwrap_or("minor"));
    let code = truncate_opt(ev.code.as_deref(), 64).unwrap_or_else(|| "unknown".into());
    conn.execute(
        "INSERT INTO rum_theater_events (ts, severity, code, path, rendition, engine, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            ts,
            severity,
            code,
            truncate_opt(ev.path.as_deref(), 512),
            truncate_opt(ev.rendition.as_deref(), 64),
            truncate_opt(ev.engine.as_deref(), 32),
            truncate_opt(ev.message.as_deref(), 512),
        ],
    )
    .map_err(|e| format!("rum theater insert: {e}"))?;
    Ok(())
}

fn insert_perf(conn: &Connection, ts: i64, ev: &RumEvent) -> Result<(), String> {
    conn.execute(
        "INSERT INTO rum_events (ts, path, embed_oid, hydrate_ok, ttfc_ms, lcp_ms, inp_ms, cls)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            ts,
            truncate_opt(ev.path.as_deref(), 512),
            truncate_opt(ev.embed_oid.as_deref(), 64),
            ev.hydrate_ok.map(|b| if b { 1 } else { 0 }),
            ev.ttfc_ms,
            ev.lcp_ms,
            ev.inp_ms,
            ev.cls,
        ],
    )
    .map_err(|e| format!("rum insert: {e}"))?;
    Ok(())
}

/// GET/HEAD/POST `/api/rum`.
pub async fn api_rum(
    State(_state): State<AppState>,
    method: axum::http::Method,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !rum_enabled() {
        return (StatusCode::NOT_FOUND, "rum disabled").into_response();
    }
    if method == axum::http::Method::GET || method == axum::http::Method::HEAD {
        return rum_summary(method);
    }
    if method != axum::http::Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD, POST")],
            "Use GET or POST for /api/rum",
        )
            .into_response();
    }

    let events = match serde_json::from_slice::<RumBatch>(&body) {
        Ok(b) if !b.events.is_empty() => b.events,
        _ => match serde_json::from_slice::<RumEvent>(&body) {
            Ok(e) => vec![e],
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "ok": false, "error": e.to_string() })),
                )
                    .into_response();
            }
        },
    };

    let ts = now_unix();
    match with_db(|conn| {
        prune(conn)?;
        for ev in &events {
            if is_theater_event(ev) {
                insert_theater(conn, ts, ev)?;
            } else {
                insert_perf(conn, ts, ev)?;
            }
        }
        Ok(())
    }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!(target: "relay_rum", error = %e, "rum ingest failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": e })),
            )
                .into_response()
        }
    }
}

fn truncate_opt(s: Option<&str>, max: usize) -> Option<String> {
    s.map(|s| {
        let t = s.trim();
        if t.len() <= max {
            t.to_string()
        } else {
            t.chars().take(max).collect()
        }
    })
    .filter(|s| !s.is_empty())
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(idx).copied()
}

fn theater_summary_json(conn: &Connection, since: i64) -> Result<serde_json::Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT severity, code, COUNT(*) FROM rum_theater_events WHERE ts >= ?1 GROUP BY severity, code",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut major = 0u64;
    let mut minor = 0u64;
    let mut by_code: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_sev_code: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();

    for row in rows.flatten() {
        let (sev_raw, code, n) = row;
        let sev = normalize_severity(&sev_raw);
        match sev {
            "major" => major = major.saturating_add(n),
            _ => minor = minor.saturating_add(n),
        }
        *by_code.entry(code.clone()).or_insert(0) += n;
        *by_sev_code
            .entry(sev.to_string())
            .or_default()
            .entry(code)
            .or_insert(0) += n;
    }

    Ok(json!({
        "major": major,
        "minor": minor,
        "byCode": by_code,
        "bySeverityCode": by_sev_code,
    }))
}

fn rum_summary(method: axum::http::Method) -> Response {
    let since = now_unix() - 86_400;
    let summary = with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT ttfc_ms, lcp_ms, inp_ms, cls, hydrate_ok FROM rum_events WHERE ts >= ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![since], |r| {
                Ok((
                    r.get::<_, Option<f64>>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut ttfc = Vec::new();
        let mut lcp = Vec::new();
        let mut inp = Vec::new();
        let mut cls = Vec::new();
        let mut n = 0usize;
        let mut hydrate_ok = 0usize;
        for row in rows.flatten() {
            n += 1;
            if row.4 == Some(1) {
                hydrate_ok += 1;
            }
            if let Some(v) = row.0 {
                ttfc.push(v);
            }
            if let Some(v) = row.1 {
                lcp.push(v);
            }
            if let Some(v) = row.2 {
                inp.push(v);
            }
            if let Some(v) = row.3 {
                cls.push(v);
            }
        }
        for v in [&mut ttfc, &mut lcp, &mut inp, &mut cls] {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        }
        let theater24h = theater_summary_json(conn, since)?;
        Ok(json!({
            "ok": true,
            "enabled": true,
            "sample_rate": rum_sample_rate(),
            "window_hours": 24,
            "events": n,
            "hydrate_ok": hydrate_ok,
            "p75": {
                "ttfc_ms": percentile(&ttfc, 0.75),
                "lcp_ms": percentile(&lcp, 0.75),
                "inp_ms": percentile(&inp, 0.75),
                "cls": percentile(&cls, 0.75),
            },
            "theater24h": theater24h,
        }))
    });
    match summary {
        Ok(body) => {
            let resp = (StatusCode::OK, Json(body)).into_response();
            if method == axum::http::Method::HEAD {
                let (parts, _) = resp.into_parts();
                return Response::from_parts(parts, axum::body::Body::empty());
            }
            resp
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_p75() {
        let v = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(percentile(&v, 0.75), Some(30.0));
    }

    #[test]
    fn rum_disabled_env() {
        std::env::remove_var("RELAY_RUM_ENABLED");
        assert!(!rum_enabled());
    }

    #[test]
    fn theater_event_detection() {
        assert!(is_theater_event(&RumEvent {
            kind: Some("theater".into()),
            severity: Some("major".into()),
            code: Some("wasm_hard_fail".into()),
            ..Default::default()
        }));
        assert!(is_theater_event(&RumEvent {
            kind: None,
            severity: Some("minor".into()),
            code: Some("js_error".into()),
            ..Default::default()
        }));
        assert!(!is_theater_event(&RumEvent {
            path: Some("/".into()),
            ttfc_ms: Some(10.0),
            ..Default::default()
        }));
    }
}

//! In-process HTTP traffic aggregates for `/api/status`.
//!
//! Middleware records status class + latency; minute buckets feed 1m / 1h / 24h
//! rollups, including per-Host mix and top HTML paths (no client IPs). Optional
//! SQLite under `RELAY_HTTP_STATS_DIR` (default `/opt/relay/state/http_stats`)
//! survives restarts. Disable with `RELAY_HTTP_STATS=0`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;
use rusqlite::{params, Connection};
use serde::Serialize;

const LATENCY_RING: usize = 2048;
const HOST_TOP_N: usize = 8;
const PATH_TOP_N: usize = 8;
const PATHS_PER_MINUTE: usize = 48;
const PATH_MAX_LEN: usize = 160;
const MINUTE_HISTORY: usize = 60 * 24; // 24h

static ENABLED: AtomicBool = AtomicBool::new(true);
static STORE: OnceLock<Mutex<HttpStatsStore>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
struct HostMinute {
    count: u64,
    html: u64,
    api: u64,
    asset: u64,
    other: u64,
    s5xx: u64,
}

#[derive(Debug, Clone, Default)]
struct MinuteBucket {
    minute: u64,
    count: u64,
    s2xx: u64,
    s3xx: u64,
    s4xx: u64,
    s5xx: u64,
    api: u64,
    asset: u64,
    html: u64,
    other: u64,
    hosts: HashMap<String, HostMinute>,
    html_paths: HashMap<(String, String), u64>,
}

struct HttpStatsStore {
    current: MinuteBucket,
    history: Vec<MinuteBucket>,
    latency_ms: Vec<u32>,
    latency_i: usize,
    db: Option<Connection>,
    last_flush: Instant,
}

impl HttpStatsStore {
    fn new() -> Self {
        let db = open_db().ok();
        let mut history = Vec::new();
        if let Some(conn) = db.as_ref() {
            history = load_history(conn);
        }
        Self {
            current: MinuteBucket {
                minute: now_minute(),
                ..Default::default()
            },
            history,
            latency_ms: vec![0; LATENCY_RING],
            latency_i: 0,
            db,
            last_flush: Instant::now(),
        }
    }

    fn persist_current(&self) {
        let Some(conn) = self.db.as_ref() else {
            return;
        };
        let _ = persist_minute(conn, &self.current);
        let _ = persist_hosts(conn, &self.current);
        let _ = persist_paths(conn, &self.current);
        let _ = prune_db(conn);
    }

    fn roll_if_needed(&mut self) {
        let m = now_minute();
        if m == self.current.minute {
            return;
        }
        if self.current.count > 0 || self.current.minute > 0 {
            self.persist_current();
            self.history.push(self.current.clone());
            while self.history.len() > MINUTE_HISTORY {
                self.history.remove(0);
            }
        }
        self.current = MinuteBucket {
            minute: m,
            ..Default::default()
        };
    }

    fn record(&mut self, status: u16, ms: u64, path: &str, host: Option<&str>) {
        self.roll_if_needed();
        let class = classify_path(path);
        self.current.count = self.current.count.saturating_add(1);
        match status {
            200..=299 => self.current.s2xx += 1,
            300..=399 => self.current.s3xx += 1,
            400..=499 => self.current.s4xx += 1,
            500..=599 => self.current.s5xx += 1,
            _ => {}
        }
        match class {
            PathClass::Api => self.current.api += 1,
            PathClass::Asset => self.current.asset += 1,
            PathClass::Html => self.current.html += 1,
            PathClass::Other => self.current.other += 1,
        }
        let ms_u = ms.min(u32::MAX as u64) as u32;
        let i = self.latency_i % LATENCY_RING;
        self.latency_ms[i] = ms_u;
        self.latency_i = self.latency_i.saturating_add(1);

        if let Some(h) = host {
            let key = sanitize_host(h);
            if !key.is_empty() {
                let hm = self.current.hosts.entry(key.clone()).or_default();
                hm.count = hm.count.saturating_add(1);
                match class {
                    PathClass::Api => hm.api += 1,
                    PathClass::Asset => hm.asset += 1,
                    PathClass::Html => hm.html += 1,
                    PathClass::Other => hm.other += 1,
                }
                if (500..=599).contains(&status) {
                    hm.s5xx += 1;
                }
                if class == PathClass::Html {
                    if let Some(p) = sanitize_html_path(path) {
                        let pk = (key, p);
                        let known = self.current.html_paths.contains_key(&pk);
                        if known || self.current.html_paths.len() < PATHS_PER_MINUTE {
                            *self.current.html_paths.entry(pk).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        if self.last_flush.elapsed() > Duration::from_secs(30) {
            self.persist_current();
            self.last_flush = Instant::now();
        }
    }

    fn snapshot(&mut self) -> HttpTrafficStats {
        self.roll_if_needed();
        let sum_window = |mins: u64| -> WindowStats {
            let cutoff = now_minute().saturating_sub(mins.saturating_sub(1));
            let mut w = WindowStats::default();
            let mut add = |b: &MinuteBucket| {
                if b.minute < cutoff {
                    return;
                }
                w.requests = w.requests.saturating_add(b.count);
                w.status_2xx = w.status_2xx.saturating_add(b.s2xx);
                w.status_3xx = w.status_3xx.saturating_add(b.s3xx);
                w.status_4xx = w.status_4xx.saturating_add(b.s4xx);
                w.status_5xx = w.status_5xx.saturating_add(b.s5xx);
                w.api = w.api.saturating_add(b.api);
                w.asset = w.asset.saturating_add(b.asset);
                w.html = w.html.saturating_add(b.html);
                w.other = w.other.saturating_add(b.other);
            };
            for b in &self.history {
                add(b);
            }
            add(&self.current);
            w
        };

        let filled = self.latency_i.min(LATENCY_RING);
        let (p50, p95) = if filled == 0 {
            (None, None)
        } else {
            let mut samples: Vec<u32> = self.latency_ms[..filled].to_vec();
            samples.sort_unstable();
            let p = |pct: f64| -> Option<u32> {
                if samples.is_empty() {
                    return None;
                }
                let idx = ((samples.len() as f64 - 1.0) * pct).round() as usize;
                samples.get(idx).copied()
            };
            (p(0.50), p(0.95))
        };

        let cutoff_1h = now_minute().saturating_sub(59);
        let mut host_agg: HashMap<String, HostMinute> = HashMap::new();
        let mut path_agg: HashMap<(String, String), u64> = HashMap::new();
        let mut add_host_window = |b: &MinuteBucket| {
            if b.minute < cutoff_1h {
                return;
            }
            for (host, hm) in &b.hosts {
                let e = host_agg.entry(host.clone()).or_default();
                e.count = e.count.saturating_add(hm.count);
                e.html = e.html.saturating_add(hm.html);
                e.api = e.api.saturating_add(hm.api);
                e.asset = e.asset.saturating_add(hm.asset);
                e.other = e.other.saturating_add(hm.other);
                e.s5xx = e.s5xx.saturating_add(hm.s5xx);
            }
            for ((host, path), n) in &b.html_paths {
                *path_agg.entry((host.clone(), path.clone())).or_insert(0) += *n;
            }
        };
        for b in &self.history {
            add_host_window(b);
        }
        add_host_window(&self.current);

        let mut host_rows: Vec<(String, HostMinute)> = host_agg.into_iter().collect();
        host_rows.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then_with(|| a.0.cmp(&b.0))
        });
        host_rows.truncate(HOST_TOP_N);

        let mut path_rows: Vec<((String, String), u64)> = path_agg.into_iter().collect();
        path_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let hosts: Vec<HostWindow> = host_rows
            .iter()
            .map(|(host, hm)| {
                let top_paths: Vec<PathCount> = path_rows
                    .iter()
                    .filter(|((h, _), _)| h == host)
                    .take(3)
                    .map(|((_, path), n)| PathCount {
                        host: host.clone(),
                        path: path.clone(),
                        requests: *n,
                    })
                    .collect();
                HostWindow {
                    host: host.clone(),
                    req_1h: hm.count,
                    html: hm.html,
                    api: hm.api,
                    asset: hm.asset,
                    s5xx: hm.s5xx,
                    top_paths,
                }
            })
            .collect();

        let top_hosts: Vec<HostCount> = hosts
            .iter()
            .map(|h| HostCount {
                host: h.host.clone(),
                requests: h.req_1h,
            })
            .collect();

        let top_paths: Vec<PathCount> = path_rows
            .into_iter()
            .take(PATH_TOP_N)
            .map(|((host, path), requests)| PathCount {
                host,
                path,
                requests,
            })
            .collect();

        HttpTrafficStats {
            enabled: true,
            req_1m: sum_window(1).requests,
            req_1h: sum_window(60).requests,
            req_24h: sum_window(MINUTE_HISTORY as u64).requests,
            window_1h: sum_window(60),
            p50_ms: p50,
            p95_ms: p95,
            top_hosts,
            hosts,
            top_paths,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathClass {
    Api,
    Asset,
    Html,
    Other,
}

fn classify_path(path: &str) -> PathClass {
    let p = path.trim_start_matches('/');
    if p.starts_with("api/") || p == "api" {
        return PathClass::Api;
    }
    if p.starts_with("assets/") || p.starts_with("wasm/") || p.starts_with("ipfs/") {
        return PathClass::Asset;
    }
    // Heuristic: extension-less or .md often SSR HTML
    if p.is_empty() || p.ends_with(".md") || p.ends_with(".html") || !p.contains('.') {
        return PathClass::Html;
    }
    PathClass::Other
}

fn sanitize_host(raw: &str) -> String {
    let h = raw.split(',').next().unwrap_or(raw).trim();
    let h = h.split(':').next().unwrap_or(h).trim().to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 {
        return String::new();
    }
    if !h
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return String::new();
    }
    h
}

fn sanitize_html_path(path: &str) -> Option<String> {
    let p = path.split(['?', '#']).next().unwrap_or(path).trim();
    let mut out = String::new();
    for c in p.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-') {
            out.push(c);
        }
    }
    if out.is_empty() {
        return None;
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    if out.len() > PATH_MAX_LEN {
        out.truncate(PATH_MAX_LEN);
    }
    Some(out)
}

fn now_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 60)
        .unwrap_or(0)
}

fn stats_dir() -> PathBuf {
    std::env::var("RELAY_HTTP_STATS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/relay/state/http_stats"))
}

fn open_db() -> Result<Connection, String> {
    let dir = stats_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = Connection::open(dir.join("http_stats.sqlite")).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS minutes (
            minute INTEGER PRIMARY KEY,
            count INTEGER NOT NULL,
            s2xx INTEGER NOT NULL,
            s3xx INTEGER NOT NULL,
            s4xx INTEGER NOT NULL,
            s5xx INTEGER NOT NULL,
            api INTEGER NOT NULL,
            asset INTEGER NOT NULL,
            html INTEGER NOT NULL,
            other INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS host_minutes (
            minute INTEGER NOT NULL,
            host TEXT NOT NULL,
            count INTEGER NOT NULL,
            html INTEGER NOT NULL,
            api INTEGER NOT NULL,
            asset INTEGER NOT NULL,
            other INTEGER NOT NULL,
            s5xx INTEGER NOT NULL,
            PRIMARY KEY (minute, host)
         );
         CREATE TABLE IF NOT EXISTS path_minutes (
            minute INTEGER NOT NULL,
            host TEXT NOT NULL,
            path TEXT NOT NULL,
            count INTEGER NOT NULL,
            PRIMARY KEY (minute, host, path)
         );",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn persist_minute(conn: &Connection, b: &MinuteBucket) -> Result<(), String> {
    conn.execute(
        "INSERT INTO minutes(minute,count,s2xx,s3xx,s4xx,s5xx,api,asset,html,other)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(minute) DO UPDATE SET
           count=excluded.count, s2xx=excluded.s2xx, s3xx=excluded.s3xx,
           s4xx=excluded.s4xx, s5xx=excluded.s5xx, api=excluded.api,
           asset=excluded.asset, html=excluded.html, other=excluded.other",
        params![
            b.minute as i64,
            b.count as i64,
            b.s2xx as i64,
            b.s3xx as i64,
            b.s4xx as i64,
            b.s5xx as i64,
            b.api as i64,
            b.asset as i64,
            b.html as i64,
            b.other as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn persist_hosts(conn: &Connection, b: &MinuteBucket) -> Result<(), String> {
    conn.execute(
        "DELETE FROM host_minutes WHERE minute = ?1",
        params![b.minute as i64],
    )
    .map_err(|e| e.to_string())?;
    for (host, hm) in &b.hosts {
        conn.execute(
            "INSERT INTO host_minutes(minute,host,count,html,api,asset,other,s5xx)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                b.minute as i64,
                host,
                hm.count as i64,
                hm.html as i64,
                hm.api as i64,
                hm.asset as i64,
                hm.other as i64,
                hm.s5xx as i64
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn persist_paths(conn: &Connection, b: &MinuteBucket) -> Result<(), String> {
    conn.execute(
        "DELETE FROM path_minutes WHERE minute = ?1",
        params![b.minute as i64],
    )
    .map_err(|e| e.to_string())?;
    for ((host, path), n) in &b.html_paths {
        conn.execute(
            "INSERT INTO path_minutes(minute,host,path,count) VALUES(?1,?2,?3,?4)",
            params![b.minute as i64, host, path, *n as i64],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn prune_db(conn: &Connection) -> Result<(), String> {
    let cutoff = now_minute().saturating_sub(MINUTE_HISTORY as u64) as i64;
    conn.execute("DELETE FROM minutes WHERE minute < ?1", params![cutoff])
        .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM host_minutes WHERE minute < ?1",
        params![cutoff],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM path_minutes WHERE minute < ?1",
        params![cutoff],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_history(conn: &Connection) -> Vec<MinuteBucket> {
    let cutoff = now_minute().saturating_sub(MINUTE_HISTORY as u64) as i64;
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT minute,count,s2xx,s3xx,s4xx,s5xx,api,asset,html,other
         FROM minutes WHERE minute >= ?1 ORDER BY minute ASC",
    ) else {
        return out;
    };
    let Ok(rows) = stmt.query_map(params![cutoff], |r| {
        Ok(MinuteBucket {
            minute: r.get::<_, i64>(0)? as u64,
            count: r.get::<_, i64>(1)? as u64,
            s2xx: r.get::<_, i64>(2)? as u64,
            s3xx: r.get::<_, i64>(3)? as u64,
            s4xx: r.get::<_, i64>(4)? as u64,
            s5xx: r.get::<_, i64>(5)? as u64,
            api: r.get::<_, i64>(6)? as u64,
            asset: r.get::<_, i64>(7)? as u64,
            html: r.get::<_, i64>(8)? as u64,
            other: r.get::<_, i64>(9)? as u64,
            hosts: HashMap::new(),
            html_paths: HashMap::new(),
        })
    }) else {
        return out;
    };
    for row in rows.flatten() {
        out.push(row);
    }
    drop(stmt);

    let mut host_map: HashMap<u64, HashMap<String, HostMinute>> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT minute,host,count,html,api,asset,other,s5xx FROM host_minutes WHERE minute >= ?1",
    ) {
        if let Ok(rows) = stmt.query_map(params![cutoff], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, String>(1)?,
                HostMinute {
                    count: r.get::<_, i64>(2)? as u64,
                    html: r.get::<_, i64>(3)? as u64,
                    api: r.get::<_, i64>(4)? as u64,
                    asset: r.get::<_, i64>(5)? as u64,
                    other: r.get::<_, i64>(6)? as u64,
                    s5xx: r.get::<_, i64>(7)? as u64,
                },
            ))
        }) {
            for row in rows.flatten() {
                host_map.entry(row.0).or_default().insert(row.1, row.2);
            }
        }
    }

    let mut path_map: HashMap<u64, HashMap<(String, String), u64>> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT minute,host,path,count FROM path_minutes WHERE minute >= ?1",
    ) {
        if let Ok(rows) = stmt.query_map(params![cutoff], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as u64,
            ))
        }) {
            for row in rows.flatten() {
                path_map
                    .entry(row.0)
                    .or_default()
                    .insert((row.1, row.2), row.3);
            }
        }
    }

    for b in &mut out {
        if let Some(h) = host_map.remove(&b.minute) {
            b.hosts = h;
        }
        if let Some(p) = path_map.remove(&b.minute) {
            b.html_paths = p;
        }
    }
    out
}

fn store() -> &'static Mutex<HttpStatsStore> {
    STORE.get_or_init(|| Mutex::new(HttpStatsStore::new()))
}

/// Call once at process start (optional; defaults to enabled unless env says otherwise).
pub fn init_from_env() {
    let on = !matches!(
        std::env::var("RELAY_HTTP_STATS")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "no" | "off"
    );
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        let _ = store();
    }
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Public aggregate blob nested under `/api/status` as `http`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct HttpTrafficStats {
    pub enabled: bool,
    pub req_1m: u64,
    pub req_1h: u64,
    pub req_24h: u64,
    pub window_1h: WindowStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_hosts: Vec<HostCount>,
    /// 1h per-Host mix (top N by requests).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostWindow>,
    /// 1h top HTML paths (query stripped, no client IPs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_paths: Vec<PathCount>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct WindowStats {
    pub requests: u64,
    pub status_2xx: u64,
    pub status_3xx: u64,
    pub status_4xx: u64,
    pub status_5xx: u64,
    pub api: u64,
    pub asset: u64,
    pub html: u64,
    pub other: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostCount {
    pub host: String,
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HostWindow {
    pub host: String,
    pub req_1h: u64,
    pub html: u64,
    pub api: u64,
    pub asset: u64,
    pub s5xx: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_paths: Vec<PathCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathCount {
    pub host: String,
    pub path: String,
    pub requests: u64,
}

pub fn snapshot() -> Option<HttpTrafficStats> {
    if !enabled() {
        return Some(HttpTrafficStats {
            enabled: false,
            ..Default::default()
        });
    }
    let mut g = store().lock().ok()?;
    Some(g.snapshot())
}

pub fn record_sample(status: u16, elapsed: Duration, path: &str, host: Option<&str>) {
    if !enabled() {
        return;
    }
    let Ok(mut g) = store().lock() else {
        return;
    };
    g.record(status, elapsed.as_millis() as u64, path, host);
}

/// Axum middleware: record status + latency for every completed request.
pub async fn http_stats_middleware(req: Request<Body>, next: Next) -> Response {
    if !enabled() {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let start = Instant::now();
    let res = next.run(req).await;
    record_sample(
        res.status().as_u16(),
        start.elapsed(),
        &path,
        host.as_deref(),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_store() -> HttpStatsStore {
        HttpStatsStore {
            current: MinuteBucket {
                minute: now_minute(),
                ..Default::default()
            },
            history: Vec::new(),
            latency_ms: vec![0; LATENCY_RING],
            latency_i: 0,
            db: None,
            last_flush: Instant::now(),
        }
    }

    #[test]
    fn classify_paths() {
        assert!(matches!(classify_path("/api/status"), PathClass::Api));
        assert!(matches!(
            classify_path("/assets/c/abc/embed.js"),
            PathClass::Asset
        ));
        assert!(matches!(classify_path("/"), PathClass::Html));
        assert!(matches!(classify_path("/foo/bar.md"), PathClass::Html));
    }

    #[test]
    fn sanitize_host_strips_port() {
        assert_eq!(sanitize_host("Relay.Example.Net:443"), "relay.example.net");
    }

    #[test]
    fn sanitize_html_path_strips_query() {
        assert_eq!(
            sanitize_html_path("/science/computing/crichtons-prophecy.md?x=1#h"),
            Some("/science/computing/crichtons-prophecy.md".into())
        );
    }

    #[test]
    fn windowed_hosts_isolate_park_html() {
        let mut store = empty_store();
        store.record(
            200,
            10,
            "/science/computing/crichtons-prophecy.md",
            Some("paradigmthreat.net"),
        );
        store.record(200, 3, "/api/status", Some("iad1.relaygateway.net"));
        store.record(500, 40, "/api/query", Some("iad1.relaygateway.net"));
        let snap = store.snapshot();
        let park = snap
            .hosts
            .iter()
            .find(|h| h.host == "paradigmthreat.net")
            .expect("park host");
        assert_eq!(park.html, 1);
        assert_eq!(park.api, 0);
        let node = snap
            .hosts
            .iter()
            .find(|h| h.host == "iad1.relaygateway.net")
            .expect("node host");
        assert_eq!(node.api, 2);
        assert_eq!(node.html, 0);
        assert_eq!(node.s5xx, 1);
        assert_eq!(
            snap.top_paths[0].path,
            "/science/computing/crichtons-prophecy.md"
        );
        assert_eq!(snap.top_hosts[0].host, "iad1.relaygateway.net");
    }

    #[test]
    fn top_hosts_are_one_hour_not_process_life() {
        let mut store = empty_store();
        let old = now_minute().saturating_sub(120);
        let mut stale = MinuteBucket {
            minute: old,
            count: 999,
            ..Default::default()
        };
        stale.hosts.insert(
            "stale.example".into(),
            HostMinute {
                count: 999,
                html: 999,
                ..Default::default()
            },
        );
        store.history.push(stale);
        store.record(200, 8, "/", Some("paradigmthreat.net"));
        let snap = store.snapshot();
        assert!(
            snap.hosts.iter().all(|h| h.host != "stale.example"),
            "2h-old host must not appear in 1h window"
        );
        assert!(snap.hosts.iter().any(|h| h.host == "paradigmthreat.net"));
    }

    #[test]
    fn snapshot_counts_current_minute() {
        init_from_env();
        ENABLED.store(true, Ordering::Relaxed);
        {
            let mut g = store().lock().unwrap();
            g.record(200, 12, "/", Some("park.example"));
            g.record(404, 5, "/gone.md", Some("park.example"));
            g.record(500, 40, "/api/x", Some("node.example"));
            let snap = g.snapshot();
            assert!(snap.req_1m >= 3);
            assert!(snap.window_1h.status_2xx >= 1);
            assert!(snap.window_1h.status_4xx >= 1);
            assert!(snap.window_1h.status_5xx >= 1);
            assert!(!snap.top_hosts.is_empty());
            assert!(!snap.hosts.is_empty());
        }
    }
}

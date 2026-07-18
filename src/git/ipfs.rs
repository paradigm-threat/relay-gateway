//! IPFS pointer stubs, global CID disk cache, and HTTP gateway fetch.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use reqwest::blocking::Client;
use tracing::warn;

use crate::types::{HEADER_BRANCH, HEADER_REPO};

pub const POINTER_MAGIC: &str = "relay-ipfs-v1";
pub const ASSET_ERROR_HEADER: &str = "X-Relay-Asset-Error";
pub const MANIFEST_PATH: &str = "assets.manifest.json";
pub const DEFAULT_ASSETS_DIR: &str = ".relay/assets";
pub const DEFAULT_IPFS_SUFFIX: &str = ".ipfs";

/// Extensions treated as IPFS-backed assets (ingest, pre-receive, tree-miss GET).
pub const ASSET_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip", ".mp3", ".mp4", ".webm",
    ".mkv", ".avi", ".mov", ".woff", ".woff2", ".ttf", ".eot", ".doc", ".docx", ".epub",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpfsPointer {
    pub cid: String,
    pub content_type: Option<String>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetErrorCode {
    RepoNotFound,
    InvalidCid,
    IpfsNotFound,
    IpfsGatewayTimeout,
    IpfsGatewayError,
    IpfsPending,
    IpfsBusy,
    Internal,
}

impl AssetErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepoNotFound => "repo-not-found",
            Self::InvalidCid => "invalid-cid",
            Self::IpfsNotFound => "ipfs-not-found",
            Self::IpfsGatewayTimeout => "ipfs-gateway-timeout",
            Self::IpfsGatewayError => "ipfs-gateway-error",
            Self::IpfsPending => "ipfs-pending",
            Self::IpfsBusy => "ipfs-busy",
            Self::Internal => "internal",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Self::RepoNotFound => StatusCode::NOT_FOUND,
            Self::InvalidCid => StatusCode::BAD_REQUEST,
            Self::IpfsNotFound => StatusCode::GONE,
            Self::IpfsGatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::IpfsGatewayError => StatusCode::BAD_GATEWAY,
            Self::IpfsPending => StatusCode::TOO_EARLY,
            Self::IpfsBusy => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::IpfsGatewayTimeout | Self::IpfsGatewayError | Self::IpfsPending | Self::IpfsBusy
        )
    }
}

/// Live seeding / availability snapshot for UI (opt-in clients poll this).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedStatus {
    /// `cached` | `fetching` | `missing` | `unavailable` | `unknown`
    pub state: String,
    /// 0–100 when known (in-flight byte progress or fully cached).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
    /// Provider count from Kubo `routing/findprovs` when `RELAY_IPFS_API` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<u32>,
    /// Whether the block is present on the configured Kubo API node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl SeedStatus {
    pub fn cached() -> Self {
        Self {
            state: "cached".into(),
            progress_pct: Some(100),
            bytes_received: None,
            bytes_total: None,
            providers: None,
            local_block: Some(true),
            hint: None,
        }
    }

    pub fn unknown(hint: impl Into<String>) -> Self {
        Self {
            state: "unknown".into(),
            progress_pct: None,
            bytes_received: None,
            bytes_total: None,
            providers: None,
            local_block: None,
            hint: Some(hint.into()),
        }
    }
}

#[derive(Debug)]
pub struct AssetResolveError {
    pub code: AssetErrorCode,
    pub message: String,
    pub cid: Option<String>,
    pub retry_after_secs: Option<u64>,
    pub seed: Option<SeedStatus>,
}

impl AssetResolveError {
    pub fn new(
        code: AssetErrorCode,
        message: impl Into<String>,
        cid: Option<String>,
    ) -> Self {
        let retry_after_secs = if code.retryable() { Some(5) } else { None };
        Self {
            code,
            message: message.into(),
            cid,
            retry_after_secs,
            seed: None,
        }
    }

    pub fn with_seed(mut self, seed: SeedStatus) -> Self {
        self.seed = Some(seed);
        self
    }

    pub fn with_retry_after(mut self, secs: Option<u64>) -> Self {
        self.retry_after_secs = secs;
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetsManifest {
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub assets: Vec<ManifestAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestAsset {
    pub path: String,
    pub ipfs: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

pub fn cache_root_from_env() -> PathBuf {
    std::env::var("RELAY_IPFS_CACHE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(crate::types::DEFAULT_IPFS_CACHE_ROOT))
}

pub fn max_cache_bytes_from_env() -> u64 {
    std::env::var("RELAY_IPFS_CACHE_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2 * 1024 * 1024 * 1024)
}

pub fn gateway_base_from_env() -> String {
    std::env::var("RELAY_IPFS_GATEWAY")
        .unwrap_or_else(|_| "https://ipfs.io".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn validated_gateway_base() -> Result<String, AssetResolveError> {
    let base = gateway_base_from_env();
    validate_gateway_base(&base).map_err(|message| AssetResolveError {
        code: AssetErrorCode::Internal,
        message,
        cid: None,
        retry_after_secs: None,
            seed: None,
        })?;
    Ok(base)
}

pub fn fetch_timeout_from_env() -> Duration {
    let secs = std::env::var("RELAY_IPFS_FETCH_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    Duration::from_secs(secs.max(1).min(300))
}

pub fn wait_max_from_env() -> u64 {
    std::env::var("RELAY_IPFS_WAIT_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
        .min(600)
}

pub fn parse_wait_seconds(query: &HashMap<String, String>, headers: &HeaderMap) -> u64 {
    let from_q = query
        .get("wait")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let from_pref = headers.get("prefer").and_then(|h| h.to_str().ok()).and_then(|s| {
        s.split(',').find_map(|part| {
            let part = part.trim();
            part.strip_prefix("wait=")
                .and_then(|v| v.trim().parse::<u64>().ok())
        })
    });
    from_q.max(from_pref.unwrap_or(0)).min(wait_max_from_env())
}

/// True when the client wants a seed/status JSON probe instead of (or before) binary bytes.
///
/// Triggers: `?relayAssetStatus=1` / `true`, or
/// `Accept: application/vnd.relay.asset-status+json`.
pub fn wants_asset_status_only(query: &HashMap<String, String>, headers: &HeaderMap) -> bool {
    if query
        .get("relayAssetStatus")
        .map(|s| {
            let t = s.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
    {
        return true;
    }
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/vnd.relay.asset-status+json"))
}

fn api_base_from_env() -> Option<String> {
    std::env::var("RELAY_IPFS_API")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone)]
struct InFlightFetch {
    bytes_received: u64,
    bytes_total: Option<u64>,
    started: Instant,
}

static IN_FLIGHT: OnceLock<Mutex<HashMap<String, InFlightFetch>>> = OnceLock::new();

fn in_flight_map() -> &'static Mutex<HashMap<String, InFlightFetch>> {
    IN_FLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

fn begin_in_flight(cid: &str, bytes_total: Option<u64>) {
    if let Ok(mut m) = in_flight_map().lock() {
        m.insert(
            cid.to_string(),
            InFlightFetch {
                bytes_received: 0,
                bytes_total,
                started: Instant::now(),
            },
        );
    }
}

fn update_in_flight(cid: &str, bytes_received: u64, bytes_total: Option<u64>) {
    if let Ok(mut m) = in_flight_map().lock() {
        if let Some(e) = m.get_mut(cid) {
            e.bytes_received = bytes_received;
            if bytes_total.is_some() {
                e.bytes_total = bytes_total;
            }
        }
    }
}

fn end_in_flight(cid: &str) {
    if let Ok(mut m) = in_flight_map().lock() {
        m.remove(cid);
    }
}

fn in_flight_seed(cid: &str) -> Option<SeedStatus> {
    let m = in_flight_map().lock().ok()?;
    let e = m.get(cid)?;
    let pct = match e.bytes_total {
        Some(total) if total > 0 => Some(((e.bytes_received * 100) / total).min(99) as u8),
        _ => None,
    };
    Some(SeedStatus {
        state: "fetching".into(),
        progress_pct: pct,
        bytes_received: Some(e.bytes_received),
        bytes_total: e.bytes_total,
        providers: None,
        local_block: None,
        hint: Some(format!(
            "gateway fetch in progress ({}s)",
            e.started.elapsed().as_secs()
        )),
    })
}

fn probe_kubo_local_block(cid: &str) -> Option<bool> {
    let api = api_base_from_env()?;
    let client = Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let url = format!("{api}/api/v0/block/stat?arg={cid}");
    let resp = client.post(&url).send().ok()?;
    Some(resp.status().is_success())
}

fn probe_kubo_providers(cid: &str) -> Option<u32> {
    let api = api_base_from_env()?;
    let client = Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    let url = format!("{api}/api/v0/routing/findprovs?arg={cid}&num-providers=20");
    let resp = client.post(&url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    // NDJSON lines from findprovs — count Objects / ID fields roughly.
    let text = resp.text().ok()?;
    let mut n = 0u32;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains("\"ID\"") || t.contains("\"Addrs\"") || t.contains("\"ID\":") {
            n = n.saturating_add(1);
        }
    }
    Some(n)
}

/// Build the best-effort seed snapshot for a CID (cache / in-flight / optional Kubo API).
pub fn probe_seed_status(cid: &str) -> SeedStatus {
    if !is_plausible_cid(cid) {
        return SeedStatus::unknown("invalid CID");
    }
    if read_cached_bytes(cid).ok().flatten().is_some() {
        return SeedStatus::cached();
    }
    if let Some(s) = in_flight_seed(cid) {
        return s;
    }
    let local_block = probe_kubo_local_block(cid);
    let providers = probe_kubo_providers(cid);
    if local_block == Some(true) {
        return SeedStatus {
            state: "cached".into(),
            progress_pct: Some(100),
            bytes_received: None,
            bytes_total: None,
            providers,
            local_block: Some(true),
            hint: Some("present on RELAY_IPFS_API node; disk cache miss".into()),
        };
    }
    if providers == Some(0) {
        return SeedStatus {
            state: "unavailable".into(),
            progress_pct: Some(0),
            bytes_received: None,
            bytes_total: None,
            providers,
            local_block,
            hint: Some("no DHT providers found for this CID".into()),
        };
    }
    if providers.is_some() || local_block.is_some() {
        return SeedStatus {
            state: "missing".into(),
            progress_pct: Some(0),
            bytes_received: None,
            bytes_total: None,
            providers,
            local_block,
            hint: Some("not on this gateway yet; retry while peers seed".into()),
        };
    }
    SeedStatus {
        state: "missing".into(),
        progress_pct: None,
        bytes_received: None,
        bytes_total: None,
        providers: None,
        local_block: None,
        hint: Some(
            "not in relay disk cache; set RELAY_IPFS_API for provider/local probes".into(),
        ),
    }
}

/// JSON body shared by HTTP asset errors and status probes (`relay.asset-status.v1`).
pub fn asset_status_body(
    err: Option<&AssetResolveError>,
    cid: Option<&str>,
    rel_path: &str,
    seed: &SeedStatus,
) -> serde_json::Value {
    let (error, message, retryable, retry_after) = match err {
        Some(e) => (
            e.code.as_str(),
            e.message.as_str(),
            e.code.retryable(),
            e.retry_after_secs,
        ),
        None => (
            "ok",
            "asset available",
            false,
            None,
        ),
    };
    let mut body = serde_json::json!({
        "schema": "relay.asset-status.v1",
        "error": error,
        "message": message,
        "cid": cid,
        "path": rel_path,
        "retryable": retryable,
        "seed": seed,
    });
    if let Some(ra) = retry_after {
        body["retryAfterSecs"] = serde_json::json!(ra);
    }
    body
}

pub fn is_plausible_cid(cid: &str) -> bool {
    let cid = cid.trim();
    if cid.len() < 46 || cid.len() > 120 {
        return false;
    }
    if cid.contains('/') || cid.contains('\\') || cid.contains("..") {
        return false;
    }
    if !cid
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return false;
    }
    cid.starts_with("bafy") || cid.starts_with("bafk") || cid.starts_with("Qm")
}

fn allow_private_gateway_hosts() -> bool {
    std::env::var("RELAY_IPFS_GATEWAY_ALLOW_PRIVATE")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn gateway_host_allowlist() -> Option<Vec<String>> {
    let raw = std::env::var("RELAY_IPFS_GATEWAY_ALLOWLIST").ok()?;
    let hosts: Vec<String> = raw
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if hosts.is_empty() {
        None
    } else {
        Some(hosts)
    }
}

fn ip_is_restricted(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.octets()[0] == 169 && v4.octets()[1] == 254
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn host_is_private_or_local(host: &str) -> bool {
    let host = host.trim().trim_matches(|c| c == '[' || c == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host.ends_with(".local") || host.ends_with(".internal") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip_is_restricted(ip);
    }
    false
}

fn host_allowed_by_allowlist(host: &str, allowlist: &[String]) -> bool {
    let host = host.trim().to_ascii_lowercase();
    allowlist
        .iter()
        .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
}

/// Validate `RELAY_IPFS_GATEWAY` before outbound fetch (SSRF mitigation).
pub fn validate_gateway_base(base: &str) -> Result<(), String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("RELAY_IPFS_GATEWAY is empty".into());
    }
    let url = reqwest::Url::parse(base).map_err(|e| format!("invalid gateway URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("gateway scheme must be http or https, not {other}")),
    }
    let host = url
        .host_str()
        .ok_or_else(|| "gateway URL must include a host".to_string())?;
    if url.username() != "" || url.password().is_some() {
        return Err("gateway URL must not embed credentials".into());
    }
    if let Some(allowlist) = gateway_host_allowlist() {
        if !host_allowed_by_allowlist(host, &allowlist) {
            return Err(format!("gateway host {host:?} is not in RELAY_IPFS_GATEWAY_ALLOWLIST"));
        }
    }
    if !allow_private_gateway_hosts() && host_is_private_or_local(host) {
        return Err(format!(
            "gateway host {host:?} is private/loopback; set RELAY_IPFS_GATEWAY_ALLOW_PRIVATE=1 for local Kubo"
        ));
    }
    Ok(())
}

pub fn max_fetch_bytes_from_env() -> usize {
    std::env::var("RELAY_IPFS_MAX_FETCH_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100 * 1024 * 1024)
        .clamp(1024, 512 * 1024 * 1024)
}

pub fn parse_pointer_bytes(bytes: &[u8]) -> Option<IpfsPointer> {
    let text = std::str::from_utf8(bytes).ok()?;
    parse_pointer_str(text)
}

pub fn parse_pointer_str(text: &str) -> Option<IpfsPointer> {
    let mut lines = text.lines();
    if lines.next()?.trim() != POINTER_MAGIC {
        return None;
    }
    let cid = lines.next()?.trim().to_string();
    if !is_plausible_cid(&cid) {
        return None;
    }
    let mut content_type = None;
    let mut sha256 = None;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(h) = line.strip_prefix("sha256=") {
            sha256 = Some(h.trim().to_string());
        } else if !line.contains('=') {
            content_type = Some(line.to_string());
        }
    }
    Some(IpfsPointer {
        cid,
        content_type,
        sha256,
    })
}

pub fn format_pointer_stub(pointer: &IpfsPointer) -> String {
    let mut out = format!("{}\n{}", POINTER_MAGIC, pointer.cid);
    if let Some(ct) = &pointer.content_type {
        out.push('\n');
        out.push_str(ct);
    }
    if let Some(h) = &pointer.sha256 {
        out.push('\n');
        out.push_str("sha256=");
        out.push_str(h);
    }
    out.push('\n');
    out
}

pub fn is_likely_binary_content(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if parse_pointer_bytes(bytes).is_some() {
        return false;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return false;
    }
    true
}

pub fn parse_manifest_bytes(bytes: &[u8]) -> Result<AssetsManifest> {
    serde_json::from_slice(bytes).context("parse assets.manifest.json")
}

pub fn manifest_cid_for_path<'a>(
    manifest: &'a AssetsManifest,
    rel_path: &str,
) -> Option<&'a ManifestAsset> {
    let rel = rel_path.trim_start_matches('/');
    manifest
        .assets
        .iter()
        .find(|a| a.path.trim_start_matches('/') == rel)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpfsResolveSource {
    CentralDir,
    Inline,
    Manifest,
}

#[derive(Debug, Clone)]
pub struct IpfsResolveOptions {
    pub sources: Vec<IpfsResolveSource>,
    pub assets_dir: String,
    pub ipfs_suffix: String,
    pub manifest_path: String,
}

impl Default for IpfsResolveOptions {
    fn default() -> Self {
        Self {
            sources: vec![IpfsResolveSource::CentralDir],
            assets_dir: DEFAULT_ASSETS_DIR.to_string(),
            ipfs_suffix: DEFAULT_IPFS_SUFFIX.to_string(),
            manifest_path: MANIFEST_PATH.to_string(),
        }
    }
}

pub fn is_asset_like_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    ASSET_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
}

pub fn central_dir_stub_path(opts: &IpfsResolveOptions, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    let dir = opts.assets_dir.trim().trim_end_matches('/');
    format!("{dir}/{rel}{}", opts.ipfs_suffix)
}

fn pointer_from_manifest(manifest: &AssetsManifest, rel: &str) -> Option<IpfsPointer> {
    let asset = manifest_cid_for_path(manifest, rel)?;
    Some(IpfsPointer {
        cid: asset.ipfs.clone(),
        content_type: asset.content_type.clone(),
        sha256: asset.sha256.clone(),
    })
}

/// Resolve a logical asset path to an IPFS pointer using ordered sources.
pub fn resolve_ipfs_pointer(
    opts: &IpfsResolveOptions,
    read_repo: &dyn Fn(&str) -> Option<Vec<u8>>,
    rel: &str,
    inline_bytes: Option<&[u8]>,
) -> Option<IpfsPointer> {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }

    for source in &opts.sources {
        match source {
            IpfsResolveSource::CentralDir => {
                let stub_path = central_dir_stub_path(opts, rel);
                if let Some(bytes) = read_repo(&stub_path) {
                    if let Some(p) = parse_pointer_bytes(&bytes) {
                        return Some(p);
                    }
                }
            }
            IpfsResolveSource::Inline => {
                let bytes = inline_bytes
                    .map(|b| b.to_vec())
                    .or_else(|| read_repo(rel))?;
                if let Some(p) = parse_pointer_bytes(&bytes) {
                    return Some(p);
                }
            }
            IpfsResolveSource::Manifest => {
                let manifest_bytes = read_repo(&opts.manifest_path)?;
                let manifest = parse_manifest_bytes(&manifest_bytes).ok()?;
                if let Some(p) = pointer_from_manifest(&manifest, rel) {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn cid_dir(root: &Path, cid: &str) -> PathBuf {
    root.join(cid)
}

fn cid_data_path(root: &Path, cid: &str) -> PathBuf {
    cid_dir(root, cid).join("data")
}

fn cid_meta_path(root: &Path, cid: &str) -> PathBuf {
    cid_dir(root, cid).join("meta.json")
}

#[derive(Debug, Serialize, Deserialize)]
struct CidMeta {
    content_type: Option<String>,
    size: u64,
    fetched_at: u64,
}

pub fn read_cached_bytes(cid: &str) -> Result<Option<(Vec<u8>, Option<String>)>> {
    let root = cache_root_from_env();
    let data_path = cid_data_path(&root, cid);
    if !data_path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&data_path)?;
    let ct = cid_meta_path(&root, cid)
        .exists()
        .then(|| {
            fs::read_to_string(cid_meta_path(&root, cid))
                .ok()
                .and_then(|s| serde_json::from_str::<CidMeta>(&s).ok())
                .and_then(|m| m.content_type)
        })
        .flatten();
    touch_access(&root, cid);
    Ok(Some((bytes, ct)))
}

fn touch_access(root: &Path, cid: &str) {
    let p = cid_dir(root, cid);
    let _ = fs::OpenOptions::new().write(true).open(p.join(".access"));
}

fn write_cache(cid: &str, bytes: &[u8], content_type: Option<&str>) -> Result<()> {
    let root = cache_root_from_env();
    let dir = cid_dir(&root, cid);
    fs::create_dir_all(&dir)?;
    let data_path = dir.join("data");
    let tmp = dir.join(format!("data.{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &data_path)?;
    let meta = CidMeta {
        content_type: content_type.map(|s| s.to_string()),
        size: bytes.len() as u64,
        fetched_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    fs::write(cid_meta_path(&root, cid), serde_json::to_string(&meta)?)?;
    evict_if_needed(&root)?;
    Ok(())
}

fn evict_if_needed(root: &Path) -> Result<()> {
    let max = max_cache_bytes_from_env();
    let mut total = 0u64;
    let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    if !root.exists() {
        return Ok(());
    }
    for e in fs::read_dir(root)? {
        let e = e?;
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let data = p.join("data");
        if data.is_file() {
            let meta = fs::metadata(&data)?;
            let sz = meta.len();
            total += sz;
            let tm = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((data, sz, tm));
        }
    }
    if total <= max {
        return Ok(());
    }
    entries.sort_by_key(|(_, _, t)| *t);
    for (path, sz, _) in entries {
        if total <= max {
            break;
        }
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
        total = total.saturating_sub(sz);
    }
    Ok(())
}

static CID_FETCH_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn fetch_lock_for(cid: &str) -> Arc<Mutex<()>> {
    let mut map = CID_FETCH_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("cid fetch locks");
    map.entry(cid.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub fn fetch_cid_bytes(
    cid: &str,
    content_type_hint: Option<&str>,
    wait_secs: u64,
) -> Result<(Vec<u8>, Option<String>), AssetResolveError> {
    if !is_plausible_cid(cid) {
        return Err(AssetResolveError {
            code: AssetErrorCode::InvalidCid,
            message: "invalid CID".into(),
            cid: Some(cid.to_string()),
            retry_after_secs: None,
            seed: None,
        });
    }

    if let Ok(Some((bytes, ct))) = read_cached_bytes(cid) {
        return Ok((bytes, ct.or_else(|| content_type_hint.map(|s| s.to_string()))));
    }

    let lock = fetch_lock_for(cid);
    let _guard = lock
        .lock()
        .map_err(|_| AssetResolveError {
            code: AssetErrorCode::Internal,
            message: "fetch lock poisoned".into(),
            cid: Some(cid.to_string()),
            retry_after_secs: None,
            seed: None,
        })?;

    if let Ok(Some((bytes, ct))) = read_cached_bytes(cid) {
        return Ok((bytes, ct.or_else(|| content_type_hint.map(|s| s.to_string()))));
    }

    if wait_secs == 0 {
        return do_fetch(cid, content_type_hint, fetch_timeout_from_env());
    }

    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let seed = probe_seed_status(cid);
            return Err(AssetResolveError {
                code: AssetErrorCode::IpfsPending,
                message: "fetch still in progress".into(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
                seed: Some(seed),
            });
        }
        match do_fetch(cid, content_type_hint, remaining.min(fetch_timeout_from_env())) {
            Ok(v) => return Ok(v),
            Err(e) if e.code.retryable() => {
                std::thread::sleep(Duration::from_millis(400));
                if let Ok(Some((bytes, ct))) = read_cached_bytes(cid) {
                    return Ok((bytes, ct.or_else(|| content_type_hint.map(|s| s.to_string()))));
                }
            }
            Err(e) => return Err(e),
        }
    }
}

fn do_fetch(
    cid: &str,
    content_type_hint: Option<&str>,
    timeout: Duration,
) -> Result<(Vec<u8>, Option<String>), AssetResolveError> {
    if let Ok(Some((bytes, ct))) = read_cached_bytes(cid) {
        return Ok((bytes, ct.or_else(|| content_type_hint.map(|s| s.to_string()))));
    }

    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| AssetResolveError {
            code: AssetErrorCode::Internal,
            message: e.to_string(),
            cid: Some(cid.to_string()),
            retry_after_secs: None,
            seed: None,
        })?;

    let base = validated_gateway_base()?;
    let url = format!("{}/ipfs/{}", base, cid);
    let resp = client.get(&url).send().map_err(|e| {
        let seed = probe_seed_status(cid);
        if e.is_timeout() {
            AssetResolveError {
                code: AssetErrorCode::IpfsGatewayTimeout,
                message: e.to_string(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
                seed: Some(seed),
            }
        } else {
            AssetResolveError {
                code: AssetErrorCode::IpfsGatewayError,
                message: e.to_string(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
                seed: Some(seed),
            }
        }
    })?;

    let status = resp.status();
    // Known CIDs from git pointers that are not yet on this gateway are *pending*, not Gone.
    // 410 is reserved for truly unavailable CIDs (e.g. zero providers when API probe says so).
    if status.as_u16() == 404 {
        let seed = probe_seed_status(cid);
        let (code, message, retry) = if seed.state == "unavailable" {
            (
                AssetErrorCode::IpfsNotFound,
                "CID has no known providers".to_string(),
                None,
            )
        } else {
            (
                AssetErrorCode::IpfsPending,
                "CID not on IPFS gateway yet (still seeding)".to_string(),
                Some(5),
            )
        };
        return Err(AssetResolveError {
            code,
            message,
            cid: Some(cid.to_string()),
            retry_after_secs: retry,
            seed: Some(seed),
        });
    }
    if !status.is_success() {
        let seed = probe_seed_status(cid);
        return Err(AssetResolveError {
            code: AssetErrorCode::IpfsGatewayError,
            message: format!("gateway returned {}", status),
            cid: Some(cid.to_string()),
            retry_after_secs: Some(5),
            seed: Some(seed),
        });
    }

    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| content_type_hint.map(|s| s.to_string()));

    let content_length = resp.content_length();
    if let Some(len) = content_length {
        let max = max_fetch_bytes_from_env() as u64;
        if len > max {
            return Err(AssetResolveError {
                code: AssetErrorCode::IpfsGatewayError,
                message: format!("gateway Content-Length {len} exceeds RELAY_IPFS_MAX_FETCH_BYTES ({max})"),
                cid: Some(cid.to_string()),
                retry_after_secs: None,
                seed: None,
            });
        }
    }

    begin_in_flight(cid, content_length);
    let max = max_fetch_bytes_from_env();
    let mut buf: Vec<u8> = match content_length {
        Some(n) if n <= max as u64 => Vec::with_capacity(n as usize),
        _ => Vec::new(),
    };
    let mut reader = resp;
    let mut chunk = [0u8; 64 * 1024];
    let read_result = (|| -> Result<Vec<u8>, AssetResolveError> {
        loop {
            let n = reader.read(&mut chunk).map_err(|e| AssetResolveError {
                code: AssetErrorCode::IpfsGatewayError,
                message: e.to_string(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
                seed: Some(probe_seed_status(cid)),
            })?;
            if n == 0 {
                break;
            }
            if buf.len() + n > max {
                return Err(AssetResolveError {
                    code: AssetErrorCode::IpfsGatewayError,
                    message: format!(
                        "gateway body exceeds RELAY_IPFS_MAX_FETCH_BYTES ({max})"
                    ),
                    cid: Some(cid.to_string()),
                    retry_after_secs: None,
                    seed: None,
                });
            }
            buf.extend_from_slice(&chunk[..n]);
            update_in_flight(cid, buf.len() as u64, content_length);
        }
        Ok(buf)
    })();
    end_in_flight(cid);
    let bytes = read_result?;

    if let Err(e) = write_cache(cid, &bytes, ct.as_deref()) {
        warn!(?e, %cid, "ipfs cache write failed");
    }

    Ok((bytes, ct))
}

pub fn asset_error_response(
    err: &AssetResolveError,
    branch: &str,
    repo: &str,
    rel_path: &str,
) -> Response {
    let seed = err
        .seed
        .clone()
        .unwrap_or_else(|| match err.cid.as_deref() {
            Some(cid) => probe_seed_status(cid),
            None => SeedStatus::unknown("no CID"),
        });
    let body = asset_status_body(Some(err), err.cid.as_deref(), rel_path, &seed);
    let mut res = (
        err.code.status(),
        [(
            "Content-Type",
            "application/vnd.relay.asset-status+json; charset=utf-8",
        )],
        body.to_string(),
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(
        ASSET_ERROR_HEADER,
        err.code.as_str().parse().expect("asset error header"),
    );
    h.insert(HEADER_BRANCH, branch.parse().expect("branch header"));
    h.insert(HEADER_REPO, repo.parse().expect("repo header"));
    if let Some(ra) = err.retry_after_secs {
        h.insert("Retry-After", ra.to_string().parse().expect("retry-after"));
    }
    if let Some(pct) = seed.progress_pct {
        h.insert(
            "X-Relay-Asset-Progress",
            pct.to_string().parse().expect("progress header"),
        );
    }
    h.insert(
        "X-Relay-Asset-Seed-State",
        seed.state.parse().expect("seed state header"),
    );
    res
}

/// Status-only probe (no long wait): cached → 200; else pending/unavailable JSON.
pub fn asset_status_response(
    cid: &str,
    branch: &str,
    repo: &str,
    rel_path: &str,
) -> Response {
    if read_cached_bytes(cid).ok().flatten().is_some() {
        let seed = SeedStatus::cached();
        let body = asset_status_body(None, Some(cid), rel_path, &seed);
        let mut res = (
            StatusCode::OK,
            [(
                "Content-Type",
                "application/vnd.relay.asset-status+json; charset=utf-8",
            )],
            body.to_string(),
        )
            .into_response();
        let h = res.headers_mut();
        h.insert(HEADER_BRANCH, branch.parse().expect("branch header"));
        h.insert(HEADER_REPO, repo.parse().expect("repo header"));
        h.insert(
            "X-Relay-Asset-Seed-State",
            "cached".parse().expect("seed state"),
        );
        h.insert(
            "X-Relay-Asset-Progress",
            "100".parse().expect("progress"),
        );
        return res;
    }
    let seed = probe_seed_status(cid);
    let err = if seed.state == "unavailable" {
        AssetResolveError::new(
            AssetErrorCode::IpfsNotFound,
            "CID has no known providers",
            Some(cid.to_string()),
        )
        .with_seed(seed.clone())
        .with_retry_after(None)
    } else if seed.state == "fetching" {
        AssetResolveError::new(
            AssetErrorCode::IpfsBusy,
            "fetch already in progress on this node",
            Some(cid.to_string()),
        )
        .with_seed(seed.clone())
        .with_retry_after(Some(2))
    } else {
        AssetResolveError::new(
            AssetErrorCode::IpfsPending,
            "CID not ready on this gateway yet",
            Some(cid.to_string()),
        )
        .with_seed(seed.clone())
        .with_retry_after(Some(5))
    };
    asset_error_response(&err, branch, repo, rel_path)
}

pub fn resolve_pointer_blob(
    pointer: &IpfsPointer,
    wait_secs: u64,
) -> Result<(Vec<u8>, String), AssetResolveError> {
    let (bytes, ct) = fetch_cid_bytes(
        &pointer.cid,
        pointer.content_type.as_deref(),
        wait_secs,
    )?;
    let content_type = ct
        .or_else(|| pointer.content_type.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok((bytes, content_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pointer_roundtrip() {
        let p = IpfsPointer {
            cid: "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi".into(),
            content_type: Some("image/png".into()),
            sha256: None,
        };
        let stub = format_pointer_stub(&p);
        let parsed = parse_pointer_str(&stub).unwrap();
        assert_eq!(parsed.cid, p.cid);
        assert_eq!(parsed.content_type, p.content_type);
    }

    #[test]
    fn binary_detection() {
        assert!(!is_likely_binary_content(b"relay-ipfs-v1\nbafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi\n"));
        assert!(is_likely_binary_content(&[0xFF, 0xD8, 0xFF]));
    }

    #[test]
    fn central_dir_stub_path_joins() {
        let opts = IpfsResolveOptions::default();
        assert_eq!(
            central_dir_stub_path(&opts, "media/foo.png"),
            ".relay/assets/media/foo.png.ipfs"
        );
    }

    #[test]
    fn resolve_sources_central_before_inline() {
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let central_stub = format!("{POINTER_MAGIC}\n{cid}\nimage/png\n");
        let inline_stub = format!("{POINTER_MAGIC}\nbafkreidifferentcidxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\nimage/png\n");
        let mut opts = IpfsResolveOptions::default();
        opts.sources = vec![IpfsResolveSource::CentralDir, IpfsResolveSource::Inline];
        let central_path = central_dir_stub_path(&opts, "a.png");
        let read = |path: &str| -> Option<Vec<u8>> {
            if path == central_path {
                Some(central_stub.as_bytes().to_vec())
            } else if path == "a.png" {
                Some(inline_stub.as_bytes().to_vec())
            } else {
                None
            }
        };
        let p = resolve_ipfs_pointer(&opts, &read, "a.png", None).unwrap();
        assert_eq!(p.cid, cid);
    }

    #[test]
    fn resolve_manifest_custom_path() {
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let manifest = serde_json::json!({
            "schema": "relay.assets-manifest.v1",
            "assets": [{ "path": "x.jpg", "ipfs": cid, "contentType": "image/jpeg" }]
        });
        let mut opts = IpfsResolveOptions::default();
        opts.sources = vec![IpfsResolveSource::Manifest];
        opts.manifest_path = "custom-manifest.json".into();
        let manifest_bytes = manifest.to_string();
        let read = |path: &str| -> Option<Vec<u8>> {
            if path == "custom-manifest.json" {
                Some(manifest_bytes.as_bytes().to_vec())
            } else {
                None
            }
        };
        let p = resolve_ipfs_pointer(&opts, &read, "x.jpg", None).unwrap();
        assert_eq!(p.cid, cid);
    }

    #[test]
    fn cid_rejects_path_traversal() {
        assert!(!is_plausible_cid("bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi/../etc"));
        assert!(!is_plausible_cid("../Qmfoo"));
    }

    #[test]
    fn gateway_validation_blocks_private_by_default() {
        assert!(validate_gateway_base("http://127.0.0.1:8080").is_err());
        assert!(validate_gateway_base("https://ipfs.io").is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn gateway_validation_allowlist() {
        std::env::set_var("RELAY_IPFS_GATEWAY_ALLOWLIST", "ipfs.io,custom.gateway");
        assert!(validate_gateway_base("https://evil.example").is_err());
        assert!(validate_gateway_base("https://ipfs.io").is_ok());
        std::env::remove_var("RELAY_IPFS_GATEWAY_ALLOWLIST");
    }

    #[test]
    fn asset_status_body_includes_seed() {
        let seed = SeedStatus {
            state: "missing".into(),
            progress_pct: Some(0),
            bytes_received: None,
            bytes_total: None,
            providers: Some(2),
            local_block: Some(false),
            hint: Some("retry".into()),
        };
        let err = AssetResolveError::new(
            AssetErrorCode::IpfsPending,
            "seeding",
            Some("bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi".into()),
        )
        .with_seed(seed.clone());
        let body = asset_status_body(Some(&err), err.cid.as_deref(), "a.png", &seed);
        assert_eq!(body["schema"], "relay.asset-status.v1");
        assert_eq!(body["error"], "ipfs-pending");
        assert_eq!(body["retryable"], true);
        assert_eq!(body["seed"]["state"], "missing");
        assert_eq!(body["seed"]["providers"], 2);
    }

    #[test]
    fn pending_status_is_too_early() {
        assert_eq!(AssetErrorCode::IpfsPending.status(), StatusCode::TOO_EARLY);
        assert!(AssetErrorCode::IpfsPending.retryable());
    }
}

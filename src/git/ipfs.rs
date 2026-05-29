//! IPFS pointer stubs, global CID disk cache, and HTTP gateway fetch.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
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

#[derive(Debug)]
pub struct AssetResolveError {
    pub code: AssetErrorCode,
    pub message: String,
    pub cid: Option<String>,
    pub retry_after_secs: Option<u64>,
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
            return Err(AssetResolveError {
                code: AssetErrorCode::IpfsPending,
                message: "fetch still in progress".into(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
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
        })?;

    let base = validated_gateway_base()?;
    let url = format!("{}/ipfs/{}", base, cid);
    let resp = client.get(&url).send().map_err(|e| {
        if e.is_timeout() {
            AssetResolveError {
                code: AssetErrorCode::IpfsGatewayTimeout,
                message: e.to_string(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
            }
        } else {
            AssetResolveError {
                code: AssetErrorCode::IpfsGatewayError,
                message: e.to_string(),
                cid: Some(cid.to_string()),
                retry_after_secs: Some(5),
            }
        }
    })?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(AssetResolveError {
            code: AssetErrorCode::IpfsNotFound,
            message: "CID not found on IPFS gateway".into(),
            cid: Some(cid.to_string()),
            retry_after_secs: None,
        });
    }
    if !status.is_success() {
        return Err(AssetResolveError {
            code: AssetErrorCode::IpfsGatewayError,
            message: format!("gateway returned {}", status),
            cid: Some(cid.to_string()),
            retry_after_secs: Some(5),
        });
    }

    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| content_type_hint.map(|s| s.to_string()));

    if let Some(len) = resp.content_length() {
        let max = max_fetch_bytes_from_env() as u64;
        if len > max {
            return Err(AssetResolveError {
                code: AssetErrorCode::IpfsGatewayError,
                message: format!("gateway Content-Length {len} exceeds RELAY_IPFS_MAX_FETCH_BYTES ({max})"),
                cid: Some(cid.to_string()),
                retry_after_secs: None,
            });
        }
    }

    let bytes = resp.bytes().map_err(|e| AssetResolveError {
        code: AssetErrorCode::IpfsGatewayError,
        message: e.to_string(),
        cid: Some(cid.to_string()),
        retry_after_secs: Some(5),
    })?;

    let max = max_fetch_bytes_from_env();
    if bytes.len() > max {
        return Err(AssetResolveError {
            code: AssetErrorCode::IpfsGatewayError,
            message: format!(
                "gateway body {} bytes exceeds RELAY_IPFS_MAX_FETCH_BYTES ({max})",
                bytes.len()
            ),
            cid: Some(cid.to_string()),
            retry_after_secs: None,
        });
    }

    if let Err(e) = write_cache(cid, &bytes, ct.as_deref()) {
        warn!(?e, %cid, "ipfs cache write failed");
    }

    Ok((bytes.to_vec(), ct))
}

pub fn asset_error_response(
    err: &AssetResolveError,
    branch: &str,
    repo: &str,
    rel_path: &str,
) -> Response {
    let body = serde_json::json!({
        "error": err.code.as_str(),
        "message": err.message,
        "cid": err.cid,
        "path": rel_path,
        "retryable": err.code.retryable(),
    });
    let mut res = (
        err.code.status(),
        [("Content-Type", "application/json; charset=utf-8")],
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
    res
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
}

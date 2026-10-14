//! Path-keyed local asset store.
//!
//! IPFS (Kubo) only syncs catalog files onto disk. HTTP serves the finished
//! file at `{cache}/{repo}/files/{rel}` — never a partial Kubo DAG.

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use tracing::{info, warn};

use crate::git::ipfs::{
    asset_error_response, cache_root_from_env, enqueue_kubo_pin, is_plausible_cid,
    parse_byte_range_header, probe_kubo_local_file_size, probe_seed_status, AssetErrorCode,
    AssetResolveError, IpfsPointer, SeedStatus, ASSET_ERROR_HEADER,
};
use crate::types::{HEADER_BRANCH, HEADER_REPO};

const META_SUFFIX: &str = ".relay-meta.json";
const FILES_DIR: &str = "files";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServeMeta {
    pub cid: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub fetched_at: u64,
}

#[derive(Debug, Clone)]
pub struct CatalogServeKey {
    pub repo: String,
    pub rel: String,
}

#[derive(Debug, Clone)]
enum SyncTrack {
    InFlight {
        started: Instant,
        cid: String,
        expected: Option<u64>,
    },
}

static SYNC_TRACK: OnceLock<Mutex<HashMap<String, SyncTrack>>> = OnceLock::new();

fn sync_map() -> &'static Mutex<HashMap<String, SyncTrack>> {
    SYNC_TRACK.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sync_key(repo: &str, rel: &str) -> String {
    format!("{}\0{}", repo, rel.trim_start_matches('/'))
}

fn sanitize_repo(repo: &str) -> Option<String> {
    let r = repo.trim();
    if r.is_empty() || r.contains('/') || r.contains('\\') || r.contains("..") {
        return None;
    }
    if !r
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return None;
    }
    Some(r.to_string())
}

fn sanitize_rel(rel: &str) -> Option<String> {
    let rel = rel.trim().trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    let path = Path::new(rel);
    for c in path.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                if s.is_empty() || s == META_SUFFIX || s.ends_with(META_SUFFIX) {
                    // allow normal names; meta is only our sidecar suffix on the full path
                }
                if s.contains('\0') {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if rel.contains('\0') {
        return None;
    }
    Some(rel.to_string())
}

pub fn serve_file_path(repo: &str, rel: &str) -> Option<PathBuf> {
    let repo = sanitize_repo(repo)?;
    let rel = sanitize_rel(rel)?;
    Some(
        cache_root_from_env()
            .join(&repo)
            .join(FILES_DIR)
            .join(Path::new(&rel)),
    )
}

fn meta_path_for(file_path: &Path) -> PathBuf {
    let mut s = file_path.as_os_str().to_os_string();
    s.push(META_SUFFIX);
    PathBuf::from(s)
}

pub fn read_serve_meta(repo: &str, rel: &str) -> Option<ServeMeta> {
    let path = serve_file_path(repo, rel)?;
    let meta_path = meta_path_for(&path);
    let raw = fs::read_to_string(meta_path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// True when the on-disk file matches the catalog CID and expected size.
pub fn local_asset_ready(
    repo: &str,
    rel: &str,
    cid: &str,
    expected_bytes: Option<u64>,
) -> Option<(PathBuf, ServeMeta)> {
    if !is_plausible_cid(cid) {
        return None;
    }
    let path = serve_file_path(repo, rel)?;
    if !path.is_file() {
        return None;
    }
    let meta = read_serve_meta(repo, rel)?;
    if meta.cid != cid {
        return None;
    }
    let len = fs::metadata(&path).ok()?.len();
    if len != meta.size {
        return None;
    }
    if let Some(exp) = expected_bytes {
        if len != exp {
            return None;
        }
    }
    Some((path, meta))
}

pub fn sync_in_flight(repo: &str, rel: &str) -> Option<SeedStatus> {
    let key = sync_key(repo, rel);
    let m = sync_map().lock().ok()?;
    match m.get(&key)? {
        SyncTrack::InFlight {
            started,
            cid,
            expected,
        } => Some(SeedStatus {
            state: "seeding".into(),
            progress_pct: None,
            bytes_received: None,
            bytes_total: *expected,
            providers: None,
            local_block: probe_kubo_local_file_size(cid).map(|sz| sz > 0),
            hint: Some(format!(
                "still seeding onto this node ({}s)",
                started.elapsed().as_secs()
            )),
        }),
    }
}

fn legacy_cid_data_path(cid: &str) -> PathBuf {
    cache_root_from_env().join(cid).join("data")
}

fn write_meta(path: &Path, meta: &ServeMeta) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let meta_path = meta_path_for(path);
    let tmp = meta_path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, serde_json::to_vec(meta).unwrap_or_default())?;
    fs::rename(tmp, meta_path)?;
    Ok(())
}

fn atomic_replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // Drop previous version of this path (latest-only).
    let _ = fs::remove_file(dest);
    fs::rename(tmp, dest)?;
    Ok(())
}

fn export_cid_to_path(cid: &str, dest: &Path, expected: Option<u64>) -> Result<u64, String> {
    // Prefer promoting a complete legacy CID cache entry (no second Kubo read).
    let legacy = legacy_cid_data_path(cid);
    if legacy.is_file() {
        let len = fs::metadata(&legacy).map_err(|e| e.to_string())?.len();
        if expected.map(|e| e == len).unwrap_or(true) && len > 0 {
            let tmp = dest.with_extension(format!("promoting.{}", std::process::id()));
            if let Some(parent) = tmp.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(&legacy, &tmp).map_err(|e| e.to_string())?;
            atomic_replace_file(&tmp, dest).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&legacy);
            let _ = fs::remove_dir(cache_root_from_env().join(cid));
            return Ok(len);
        }
    }

    let api = std::env::var("RELAY_IPFS_API")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "RELAY_IPFS_API not set".to_string())?;

    let local_sz = probe_kubo_local_file_size(cid);
    if let (Some(exp), Some(got)) = (expected, local_sz) {
        if got != exp {
            return Err(format!(
                "kubo size {got} != manifest bytes {exp}; not ready to export"
            ));
        }
    } else if local_sz.is_none() {
        return Err("CID not fully present on local Kubo".into());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6 * 60 * 60))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{api}/api/v0/cat?arg={cid}");
    let mut resp = client.post(&url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("kubo cat returned {}", resp.status()));
    }

    let tmp = dest.with_extension(format!("part.{}", std::process::id()));
    if let Some(parent) = tmp.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut out = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut written = 0u64;
    let mut buf = [0u8; 1024 * 256];
    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        written += n as u64;
        if let Some(exp) = expected {
            if written > exp {
                let _ = fs::remove_file(&tmp);
                return Err(format!("export exceeded expected size {exp}"));
            }
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);

    if let Some(exp) = expected {
        if written != exp {
            let _ = fs::remove_file(&tmp);
            return Err(format!("exported {written} bytes, expected {exp}"));
        }
    }
    if written == 0 {
        let _ = fs::remove_file(&tmp);
        return Err("exported empty body".into());
    }

    atomic_replace_file(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(written)
}

/// Background: pin in Kubo if needed, then export the full object onto the serve path.
pub fn enqueue_asset_sync(
    repo: &str,
    rel: &str,
    cid: &str,
    expected_bytes: Option<u64>,
    content_type: Option<String>,
    sha256: Option<String>,
) {
    if !is_plausible_cid(cid) {
        return;
    }
    let Some(repo_s) = sanitize_repo(repo) else {
        return;
    };
    if crate::git::ipfs::catalog_skips_fleet_ipfs_for_slug(&repo_s) {
        tracing::info!(
            repo = %repo_s,
            rel,
            cid,
            "enqueue_asset_sync: skipped (catalog fleet_pin_ipfs=false or max_ipfs_bytes=0)"
        );
        return;
    }
    let Some(rel_s) = sanitize_rel(rel) else {
        return;
    };
    if local_asset_ready(&repo_s, &rel_s, cid, expected_bytes).is_some() {
        return;
    }
    let key = sync_key(&repo_s, &rel_s);
    {
        let Ok(mut m) = sync_map().lock() else {
            return;
        };
        if matches!(m.get(&key), Some(SyncTrack::InFlight { .. })) {
            return;
        }
        m.insert(
            key.clone(),
            SyncTrack::InFlight {
                started: Instant::now(),
                cid: cid.to_string(),
                expected: expected_bytes,
            },
        );
    }

    let cid = cid.to_string();
    let ct = content_type;
    let sha = sha256;
    std::thread::spawn(move || {
        // Kick pin first when the DAG is incomplete.
        if probe_kubo_local_file_size(&cid).is_none() {
            enqueue_kubo_pin(&cid);
            // Wait for pin worker / local size (up to ~6h, poll).
            for _ in 0..(6 * 60 * 12) {
                if let Some(sz) = probe_kubo_local_file_size(&cid) {
                    if expected_bytes.map(|e| e == sz).unwrap_or(true) {
                        break;
                    }
                }
                if sync_map()
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&key).cloned())
                    .is_none()
                {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }

        let Some(dest) = serve_file_path(&repo_s, &rel_s) else {
            let _ = sync_map().lock().map(|mut m| m.remove(&key));
            return;
        };

        match export_cid_to_path(&cid, &dest, expected_bytes) {
            Ok(size) => {
                let meta = ServeMeta {
                    cid: cid.clone(),
                    size,
                    content_type: ct,
                    sha256: sha,
                    fetched_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                };
                if let Err(e) = write_meta(&dest, &meta) {
                    warn!(error = %e, path = %dest.display(), "asset meta write failed");
                } else {
                    info!(
                        repo = %repo_s,
                        rel = %rel_s,
                        cid = %cid,
                        size,
                        "catalog asset ready on disk"
                    );
                }
            }
            Err(e) => {
                warn!(
                    repo = %repo_s,
                    rel = %rel_s,
                    cid = %cid,
                    error = %e,
                    "catalog asset sync/export failed"
                );
            }
        }
        let _ = sync_map().lock().map(|mut m| m.remove(&key));
    });
}

fn content_type_for(meta: &ServeMeta, pointer: &IpfsPointer, rel: &str) -> String {
    meta.content_type
        .clone()
        .or_else(|| pointer.content_type.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            mime_guess::from_path(rel)
                .first_or_octet_stream()
                .essence_str()
                .to_string()
        })
}

fn is_head_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-relay-http-method")
        .and_then(|v| v.to_str().ok())
        .map(|m| m.eq_ignore_ascii_case("HEAD"))
        .unwrap_or(false)
}

fn media_response_headers(
    h: &mut HeaderMap,
    content_type: &str,
    len: u64,
    status: StatusCode,
    start: u64,
    end: u64,
    total: u64,
    branch: &str,
    repo: &str,
) {
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).expect("len"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).expect("cr"),
        );
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=604800"),
    );
    // Keep movies identity-encoded even if a proxy offers gzip/br.
    h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("identity"));
    h.insert(
        HEADER_BRANCH,
        HeaderValue::from_str(branch).unwrap_or_else(|_| HeaderValue::from_static("main")),
    );
    h.insert(
        HEADER_REPO,
        HeaderValue::from_str(repo).unwrap_or_else(|_| HeaderValue::from_static("repo")),
    );
    h.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    let _ = ASSET_ERROR_HEADER;
}

/// Stream a finished local file with optional HTTP Range (never buffers the whole movie).
///
/// For HEAD, returns the same headers and an empty body without opening a read stream.
pub fn stream_local_file_response(
    path: &Path,
    meta: &ServeMeta,
    content_type: &str,
    range_header: Option<&str>,
    branch: &str,
    repo: &str,
    head_only: bool,
) -> Response {
    let total = meta.size;
    let parsed = range_header.and_then(|r| parse_byte_range_header(r, Some(total)));
    let (status, start, end) = match parsed {
        Some(br) if br.start <= br.end && br.end < total => {
            (StatusCode::PARTIAL_CONTENT, br.start, br.end)
        }
        Some(_) => {
            let mut res = (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{total}")).expect("cr"),
                )],
            )
                .into_response();
            res.headers_mut().insert(
                HEADER_BRANCH,
                HeaderValue::from_str(branch).unwrap_or_else(|_| HeaderValue::from_static("main")),
            );
            res.headers_mut().insert(
                HEADER_REPO,
                HeaderValue::from_str(repo).unwrap_or_else(|_| HeaderValue::from_static("repo")),
            );
            res.headers_mut().insert(
                header::CONTENT_ENCODING,
                HeaderValue::from_static("identity"),
            );
            return res;
        }
        None => {
            if total == 0 {
                return StatusCode::OK.into_response();
            }
            (StatusCode::OK, 0, total - 1)
        }
    };
    let len = end - start + 1;

    if head_only {
        let mut res = Response::new(Body::empty());
        *res.status_mut() = status;
        media_response_headers(
            res.headers_mut(),
            content_type,
            len,
            status,
            start,
            end,
            total,
            branch,
            repo,
        );
        return res;
    }

    let mut std_file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "open local asset failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(e) = std_file.seek(SeekFrom::Start(start)) {
        warn!(error = %e, "seek local asset failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let async_file = tokio::fs::File::from_std(std_file);
    let reader = async_file.take(len);
    let stream = ReaderStream::new(reader);
    let body = Body::from_stream(stream);

    let mut res = Response::new(body);
    *res.status_mut() = status;
    media_response_headers(
        res.headers_mut(),
        content_type,
        len,
        status,
        start,
        end,
        total,
        branch,
        repo,
    );
    res
}

fn seeding_error(cid: &str, seed: SeedStatus) -> AssetResolveError {
    AssetResolveError::new(
        AssetErrorCode::IpfsSeeding,
        "still seeding",
        Some(cid.to_string()),
    )
    .with_seed(seed)
    .with_retry_after(Some(5))
}

fn missing_error(cid: &str, seed: SeedStatus) -> AssetResolveError {
    AssetResolveError::new(
        AssetErrorCode::IpfsPending,
        "asset not on this node yet",
        Some(cid.to_string()),
    )
    .with_seed(seed)
    .with_retry_after(Some(5))
}

fn catalog_only_error(cid: &str) -> AssetResolveError {
    let seed = crate::git::ipfs::probe_seed_status_with(cid, false);
    AssetResolveError::new(
        AssetErrorCode::IpfsPending,
        "catalog-only; use swarm / ipfs://",
        Some(cid.to_string()),
    )
    .with_seed(seed)
    .with_retry_after(None)
}

/// Serve a catalog asset from the local path store, or return an immediate status error.
///
/// Never waits on the IPFS network and never returns a partial DAG.
pub fn serve_catalog_asset(
    repo: &str,
    branch: &str,
    rel: &str,
    pointer: &IpfsPointer,
    expected_bytes: Option<u64>,
    headers: &HeaderMap,
) -> Response {
    let rel = rel.trim_start_matches('/');
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    let head_only = is_head_request(headers);

    if let Some((path, meta)) = local_asset_ready(repo, rel, &pointer.cid, expected_bytes) {
        let ct = content_type_for(&meta, pointer, rel);
        return stream_local_file_response(&path, &meta, &ct, range, branch, repo, head_only);
    }

    if crate::git::ipfs::catalog_skips_fleet_ipfs_for_slug(repo) {
        let err = catalog_only_error(&pointer.cid);
        return asset_error_response(&err, branch, repo, rel);
    }

    if let Some(seed) = sync_in_flight(repo, rel) {
        let err = seeding_error(&pointer.cid, seed);
        return asset_error_response(&err, branch, repo, rel);
    }

    // Stale/wrong CID on disk: replace via sync.
    enqueue_asset_sync(
        repo,
        rel,
        &pointer.cid,
        expected_bytes,
        pointer.content_type.clone(),
        pointer.sha256.clone(),
    );

    if let Some(seed) = sync_in_flight(repo, rel) {
        let err = seeding_error(&pointer.cid, seed);
        return asset_error_response(&err, branch, repo, rel);
    }

    let mut seed = probe_seed_status(&pointer.cid);
    if seed.state == "cached" {
        // Kubo has bytes but serve path is not ready yet — treat as seeding.
        seed.state = "seeding".into();
        seed.hint = Some("exporting finished Kubo object onto the serve path".into());
        let err = seeding_error(&pointer.cid, seed);
        return asset_error_response(&err, branch, repo, rel);
    }
    if seed.state == "unavailable" {
        let err = AssetResolveError::new(
            AssetErrorCode::IpfsNotFound,
            "CID has no known providers",
            Some(pointer.cid.clone()),
        )
        .with_seed(seed)
        .with_retry_after(None);
        return asset_error_response(&err, branch, repo, rel);
    }

    seed.state = "missing".into();
    seed.hint = Some("sync queued; file is not served until fully on disk".into());
    let err = missing_error(&pointer.cid, seed);
    asset_error_response(&err, branch, repo, rel)
}

/// Read a ready local asset into memory (small web assets only).
pub fn read_local_asset_bytes(
    repo: &str,
    rel: &str,
    cid: &str,
    expected_bytes: Option<u64>,
    max_bytes: usize,
) -> Result<Option<(Vec<u8>, Option<String>)>, AssetResolveError> {
    let Some((path, meta)) = local_asset_ready(repo, rel, cid, expected_bytes) else {
        return Ok(None);
    };
    if meta.size as usize > max_bytes {
        return Err(AssetResolveError::new(
            AssetErrorCode::IpfsBusy,
            format!(
                "asset is {} bytes; too large to buffer (max {max_bytes})",
                meta.size
            ),
            Some(cid.to_string()),
        ));
    }
    let bytes = fs::read(&path).map_err(|e| {
        AssetResolveError::new(
            AssetErrorCode::Internal,
            e.to_string(),
            Some(cid.to_string()),
        )
    })?;
    Ok(Some((bytes, meta.content_type)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_rel("../etc/passwd").is_none());
        assert!(sanitize_rel("FanMixes/The Mummy (1999) - Debugged/x.mkv").is_some());
        assert!(sanitize_repo("fan-edits").is_some());
        assert!(sanitize_repo("../x").is_none());
    }

    #[test]
    fn ready_requires_matching_cid_and_size() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RELAY_IPFS_CACHE_ROOT", dir.path());
        let repo = "fan-edits";
        let rel = "movies/a.mkv";
        let path = serve_file_path(repo, rel).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let payload = b"hello-world-bytes!!";
        fs::write(&path, payload).unwrap();
        let meta = ServeMeta {
            cid: "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi".into(),
            size: payload.len() as u64,
            content_type: Some("video/x-matroska".into()),
            sha256: None,
            fetched_at: 1,
        };
        write_meta(&path, &meta).unwrap();
        assert!(local_asset_ready(repo, rel, &meta.cid, Some(meta.size)).is_some());
        assert!(local_asset_ready(repo, rel, &meta.cid, Some(meta.size + 1)).is_none());
        assert!(local_asset_ready(repo, rel, &meta.cid, Some(meta.size)).is_some());
        std::env::remove_var("RELAY_IPFS_CACHE_ROOT");
    }
}

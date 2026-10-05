//! Server CleverScript host: virtual `@relay/*` modules only (no global `Relay`).

use crate::git::chat_index;
use crate::git::hooks::HookContext;
use crate::git::image_cache::{cache_key_digest, ImageDiskCache};
use crate::git::ipfs::{
    self, resolve_ipfs_pointer, resolve_pointer_http, AssetResolveError, IpfsPointer,
    IpfsResolveOptions, IpfsResolveSource, ASSET_ERROR_HEADER,
};
use crate::git::repo::read_file_from_repo;
use crate::git::search_plugins::{self, PLUGIN_CHAT, PLUGIN_REDB, PLUGIN_SQLITE};
use axum::http::HeaderMap;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use relay_runtime::host::{Host, HostHandle, MockHost};
use relay_runtime::value::{NativeFnClosure, Value as CsValue};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub const RELAY_GIT_MODULE: &str = "@relay/git";
pub const RELAY_IMAGE_MODULE: &str = "@relay/image";
pub const RELAY_IPFS_MODULE: &str = "@relay/ipfs";
pub const RELAY_MD_MODULE: &str = "@relay/md";
pub const RELAY_SSR_MODULE: &str = "@relay/ssr";
pub const RELAY_WEB_ASSETS_MODULE: &str = crate::git::web_assets::RELAY_WEB_ASSETS_MODULE;
pub const RELAY_SEARCH_SQLITE_MODULE: &str = "@relay/search/sqlite";
pub const RELAY_SEARCH_REDB_MODULE: &str = "@relay/search/redb";
pub const RELAY_SEARCH_CHAT_MODULE: &str = "@relay/search/chat";

fn git_exports(repo_root: PathBuf, branch: String) -> CsValue {
    let read = {
        let repo_root = repo_root.clone();
        let branch = branch.clone();
        Rc::new(move |args: &[CsValue]| {
            let path = args
                .first()
                .and_then(|v| match v {
                    CsValue::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .ok_or_else(|| "@relay/git readFile requires a string path".to_string())?;
            let bytes =
                read_file_from_repo(&repo_root, &branch, path).map_err(|e| e.to_string())?;
            Ok(CsValue::String(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        })
    };
    let read_b64 = {
        let repo_root = repo_root.clone();
        let branch = branch.clone();
        Rc::new(move |args: &[CsValue]| {
            let path = args
                .first()
                .and_then(|v| match v {
                    CsValue::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .ok_or_else(|| "@relay/git readFileBase64 requires a string path".to_string())?;
            let bytes =
                read_file_from_repo(&repo_root, &branch, path).map_err(|e| e.to_string())?;
            Ok(CsValue::String(B64.encode(bytes)))
        })
    };
    let mut git = HashMap::new();
    git.insert(
        "readFile".to_string(),
        CsValue::NativeFn(NativeFnClosure(read)),
    );
    git.insert(
        "readFileBase64".to_string(),
        CsValue::NativeFn(NativeFnClosure(read_b64)),
    );
    CsValue::Object(git)
}

/// `@relay/search/sqlite` / `@relay/search/redb` / `@relay/search/chat`:
/// callable `search(opts)` runs the Rust plugin.
///
/// `opts` is the QUERY `ctx` (or a subset): `{ collection?, query?, limit?, order?, config? }`.
/// `config` defaults to repo-root `search.yaml` when present (used at index time; QUERY uses tip).
/// Returns `{ rows: [...] }` for the CleverScript QUERY result path.
///
/// Chat also exports `index(ctx)` for thin index hooks (`old_commit` / `new_commit` / `branch`).
fn search_plugin_exports(repo_root: PathBuf, branch: String, plugin: &'static str) -> CsValue {
    let search = {
        let repo_root = repo_root.clone();
        let branch = branch.clone();
        Rc::new(move |args: &[CsValue]| -> Result<CsValue, String> {
            let opts = match args.first() {
                Some(CsValue::Object(o)) => o,
                _ => {
                    return Err(format!(
                        "{plugin} search requires an options object (QUERY ctx)"
                    ))
                }
            };
            let collection = cs_obj_str(opts, "collection").unwrap_or(if plugin == PLUGIN_CHAT {
                "chat"
            } else {
                "index"
            });
            let query = opts.get("query").map(|q| q.to_json());
            let limit = opts.get("limit").and_then(|l| match l {
                CsValue::Number(n) if *n > 0.0 => Some(*n as usize),
                _ => None,
            });
            let order = match opts.get("order") {
                Some(CsValue::String(raw)) => {
                    crate::git::sqlite_index::QueryOrder::parse(raw).map_err(|e| e.to_string())?
                }
                Some(_) => return Err("search order must be a string".into()),
                None => crate::git::sqlite_index::QueryOrder::default(),
            };
            // Optional override path — validated to exist in the tip when provided.
            if let Some(cfg_path) = cs_obj_str(opts, "config") {
                if crate::git::repo::read_file_from_repo(&repo_root, &branch, cfg_path).is_err() {
                    return Err(format!("search config not found in git tip: {cfg_path}"));
                }
            }
            let rows = search_plugins::run_plugin_ordered(
                plugin,
                &repo_root,
                &branch,
                collection,
                query.as_ref(),
                None,
                limit,
                order,
            )
            .map_err(|e| e.to_string())?;
            let mut out = HashMap::new();
            out.insert(
                "rows".to_string(),
                CsValue::Array(rows.into_iter().map(|v| CsValue::from_json(&v)).collect()),
            );
            Ok(CsValue::Object(out))
        })
    };
    let search_fn = CsValue::NativeFn(NativeFnClosure(search));
    let mut m = HashMap::new();
    m.insert("search".to_string(), search_fn.clone());
    m.insert("default".to_string(), search_fn);
    m.insert("plugin".to_string(), CsValue::String(plugin.to_string()));
    if plugin == PLUGIN_CHAT {
        let index = {
            let repo_root = repo_root.clone();
            let branch = branch.clone();
            Rc::new(move |args: &[CsValue]| -> Result<CsValue, String> {
                let opts = match args.first() {
                    Some(CsValue::Object(o)) => o,
                    _ => {
                        return Err(
                            "@relay/search/chat index requires an options object (index ctx)"
                                .into(),
                        )
                    }
                };
                let new_commit = cs_obj_str(opts, "new_commit")
                    .or_else(|| cs_obj_str(opts, "newCommit"))
                    .unwrap_or("")
                    .to_string();
                if new_commit.is_empty() {
                    return Err("@relay/search/chat index requires new_commit".into());
                }
                let old_commit = cs_obj_str(opts, "old_commit")
                    .or_else(|| cs_obj_str(opts, "oldCommit"))
                    .unwrap_or("")
                    .to_string();
                let br = cs_obj_str(opts, "branch")
                    .unwrap_or(branch.as_str())
                    .to_string();
                let root = cs_obj_str(opts, "root")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        chat_index::chat_root_from_search_yaml(&repo_root, &new_commit)
                    });
                let ctx = HookContext {
                    repo_path: repo_root.clone(),
                    old_commit,
                    new_commit,
                    refname: format!("refs/heads/{}", br),
                    branch: br,
                    is_verified: true,
                    files: Default::default(),
                };
                chat_index::ensure_chat_indexed(&ctx, &root).map_err(|e| e.to_string())?;
                Ok(CsValue::Null)
            })
        };
        m.insert(
            "index".to_string(),
            CsValue::NativeFn(NativeFnClosure(index)),
        );
    }
    CsValue::Object(m)
}

fn cs_obj_str<'a>(m: &'a HashMap<String, CsValue>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(|v| match v {
        CsValue::String(s) => Some(s.as_str()),
        _ => None,
    })
}

const DEFAULT_WIDTH_QUERY_KEYS: &[&str] = &["w", "width", "maxWidth"];

fn parse_width_number(n: f64) -> Option<u32> {
    let w = n as u64;
    if (1..=8192).contains(&w) {
        Some(w as u32)
    } else {
        None
    }
}

fn parse_width_from_value(v: &CsValue) -> Option<u32> {
    match v {
        CsValue::Number(n) => parse_width_number(*n),
        CsValue::String(s) => {
            let w: u32 = s.trim().parse().ok()?;
            if (1..=8192).contains(&w) {
                Some(w)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_width_from_opts(props: &HashMap<String, CsValue>) -> Option<u32> {
    props.get("width").and_then(parse_width_from_value)
}

fn width_keys_from_opts(args: &[CsValue]) -> Vec<String> {
    let Some(CsValue::Object(props)) = args.get(1) else {
        return DEFAULT_WIDTH_QUERY_KEYS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    };
    let Some(CsValue::Array(arr)) = props.get("keys") else {
        return DEFAULT_WIDTH_QUERY_KEYS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    };
    let keys: Vec<String> = arr
        .iter()
        .filter_map(|v| match v {
            CsValue::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        })
        .collect();
    if keys.is_empty() {
        DEFAULT_WIDTH_QUERY_KEYS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        keys
    }
}

fn query_width_from_ctx(m: &HashMap<String, CsValue>, keys: &[String]) -> Option<u32> {
    let q = m.get("query").and_then(|v| match v {
        CsValue::Object(inner) => Some(inner),
        _ => None,
    })?;
    for k in keys {
        if let Some(v) = q.get(k.as_str()) {
            if let Some(w) = parse_width_from_value(v) {
                return Some(w);
            }
        }
    }
    None
}

fn is_raster_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("image/jpeg")
        || ct.starts_with("image/jpg")
        || ct.starts_with("image/png")
        || ct.starts_with("image/gif")
        || ct.starts_with("image/webp")
}

fn inferred_content_type(ctx: &HashMap<String, CsValue>) -> String {
    let from_ctx = cs_obj_str(ctx, "contentType").unwrap_or("").trim();
    if !from_ctx.is_empty() {
        return from_ctx.to_string();
    }
    if let Some(rel) = cs_obj_str(ctx, "relPath") {
        return content_type_for_rel_path(rel);
    }
    String::new()
}

fn content_type_for_rel_path(rel: &str) -> String {
    mime_guess::from_path(rel)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

fn pointer_content_type(pointer: &IpfsPointer, rel: &str) -> String {
    pointer
        .content_type
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| content_type_for_rel_path(rel))
}

/// Body from hook context when present; does not error if the path is absent from Git.
fn optional_body_bytes_from_ctx(
    m: &HashMap<String, CsValue>,
    repo_root: &PathBuf,
    branch: &str,
) -> Result<Option<Vec<u8>>, String> {
    if let Some(b64) = cs_obj_str(m, "bodyBase64").filter(|s| !s.is_empty()) {
        return B64
            .decode(b64.as_bytes())
            .map(Some)
            .map_err(|e| format!("invalid bodyBase64: {e}"));
    }
    if let Some(path) = cs_obj_str(m, "relPath").filter(|s| !s.is_empty()) {
        return Ok(read_file_from_repo(repo_root, branch, path).ok());
    }
    Ok(None)
}

fn parse_ipfs_source_name(s: &str) -> Option<IpfsResolveSource> {
    match s.trim().to_ascii_lowercase().as_str() {
        "centraldir" | "central_dir" | "central-dir" => Some(IpfsResolveSource::CentralDir),
        "inline" => Some(IpfsResolveSource::Inline),
        "manifest" => Some(IpfsResolveSource::Manifest),
        _ => None,
    }
}

fn parse_ipfs_resolve_options(args: &[CsValue]) -> IpfsResolveOptions {
    let Some(CsValue::Object(props)) = args.get(1) else {
        return IpfsResolveOptions::default();
    };
    let mut opts = IpfsResolveOptions::default();
    if let Some(CsValue::Array(arr)) = props.get("sources") {
        let mut sources = Vec::new();
        for v in arr {
            if let CsValue::String(s) = v {
                if let Some(src) = parse_ipfs_source_name(s) {
                    sources.push(src);
                }
            }
        }
        if !sources.is_empty() {
            opts.sources = sources;
        }
    }
    if let Some(CsValue::String(s)) = props.get("assetsDir") {
        if !s.trim().is_empty() {
            opts.assets_dir = s.trim().to_string();
        }
    }
    if let Some(CsValue::String(s)) = props.get("ipfsSuffix") {
        if !s.is_empty() {
            opts.ipfs_suffix = s.to_string();
        }
    }
    if let Some(CsValue::String(s)) = props.get("manifestPath") {
        if !s.trim().is_empty() {
            opts.manifest_path = s.trim().to_string();
        }
    }
    opts
}

fn resolve_pointer_with_options(
    opts: &IpfsResolveOptions,
    repo_root: &PathBuf,
    branch: &str,
    rel: &str,
    inline_bytes: Option<&[u8]>,
) -> Option<IpfsPointer> {
    let repo_root = repo_root.clone();
    let branch = branch.to_string();
    resolve_ipfs_pointer(
        opts,
        &|path| read_file_from_repo(&repo_root, &branch, path).ok(),
        rel,
        inline_bytes,
    )
}

struct FetchedIpfsBytes {
    bytes: Vec<u8>,
    content_type: String,
    cache_key: String,
    status: Option<u16>,
    extra_headers: HashMap<String, String>,
}

fn ipfs_bytes_to_cs_object(fetched: &FetchedIpfsBytes) -> CsValue {
    let mut o = HashMap::new();
    o.insert(
        "bodyBase64".to_string(),
        CsValue::String(B64.encode(&fetched.bytes)),
    );
    o.insert(
        "contentType".to_string(),
        CsValue::String(fetched.content_type.clone()),
    );
    o.insert(
        "cacheKey".to_string(),
        CsValue::String(fetched.cache_key.clone()),
    );
    CsValue::Object(o)
}

fn fetched_bytes_from_cs_object(o: &HashMap<String, CsValue>) -> Result<FetchedIpfsBytes, String> {
    let b64 = cs_obj_str(o, "bodyBase64")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "bytes object missing bodyBase64".to_string())?;
    let bytes = B64
        .decode(b64.as_bytes())
        .map_err(|e| format!("invalid bodyBase64: {e}"))?;
    let content_type = cs_obj_str(o, "contentType")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let cache_key = cs_obj_str(o, "cacheKey")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| hex::encode(Sha256::digest(bytes.as_slice())));
    Ok(FetchedIpfsBytes {
        bytes,
        content_type,
        cache_key,
        status: None,
        extra_headers: HashMap::new(),
    })
}

enum FetchIpfsOutcome {
    Miss,
    Ok(FetchedIpfsBytes),
    AssetErr(AssetResolveError),
}

fn range_header_from_ctx(m: &HashMap<String, CsValue>) -> Option<String> {
    for key in ["requestHeaders", "headers"] {
        let Some(CsValue::Object(headers)) = m.get(key) else {
            continue;
        };
        for (k, v) in headers {
            if !k.eq_ignore_ascii_case("range") {
                continue;
            }
            if let CsValue::String(s) = v {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

fn fetch_ipfs_bytes_inner(
    m: &HashMap<String, CsValue>,
    repo_root: &PathBuf,
    branch: &str,
    opts: &IpfsResolveOptions,
) -> Result<FetchIpfsOutcome, String> {
    let rel = cs_obj_str(m, "relPath")
        .unwrap_or("")
        .trim_start_matches('/');
    if rel.is_empty() {
        return Ok(FetchIpfsOutcome::Miss);
    }
    let inline = optional_body_bytes_from_ctx(m, repo_root, branch)?;
    let wait_secs = parse_wait_from_ctx(m);
    let Some(pointer) =
        resolve_pointer_with_options(opts, repo_root, branch, rel, inline.as_deref())
    else {
        return Ok(FetchIpfsOutcome::Miss);
    };
    let range = range_header_from_ctx(m);
    match resolve_pointer_http(&pointer, wait_secs, range.as_deref()) {
        Ok(http) => {
            let content_type = http
                .content_type
                .filter(|ct| !ct.trim().is_empty())
                .unwrap_or_else(|| pointer_content_type(&pointer, rel));
            let mut extra_headers = HashMap::new();
            if let Some(cr) = http.content_range {
                extra_headers.insert("Content-Range".to_string(), cr);
            }
            if let Some(ar) = http.accept_ranges {
                extra_headers.insert("Accept-Ranges".to_string(), ar);
            }
            Ok(FetchIpfsOutcome::Ok(FetchedIpfsBytes {
                bytes: http.bytes,
                content_type,
                cache_key: format!("ipfs:{}", pointer.cid),
                status: Some(http.status.as_u16()),
                extra_headers,
            }))
        }
        Err(e) => Ok(FetchIpfsOutcome::AssetErr(e)),
    }
}

fn parse_optimize_raster_args(
    args: &[CsValue],
) -> (Option<&HashMap<String, CsValue>>, Option<u32>) {
    let mut bytes_obj = None;
    let mut width = None;
    for arg in args.iter().skip(1) {
        let CsValue::Object(o) = arg else { continue };
        if o.contains_key("bodyBase64") {
            bytes_obj = Some(o);
        }
        if o.contains_key("width") {
            width = parse_width_from_opts(o).or(width);
        }
    }
    (bytes_obj, width)
}

fn hook_file_object_webp(webp_bytes: &[u8]) -> CsValue {
    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String("public, max-age=604800".into()),
    );
    let mut file = HashMap::new();
    file.insert("kind".to_string(), CsValue::String("file".into()));
    file.insert(
        "contentType".to_string(),
        CsValue::String("image/webp".into()),
    );
    file.insert(
        "bodyBase64".to_string(),
        CsValue::String(B64.encode(webp_bytes)),
    );
    file.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(file)
}

fn hook_file_object_bytes_with_status(
    bytes: &[u8],
    content_type: &str,
    cache_control: &str,
    status: Option<u16>,
    extra_headers: &HashMap<String, String>,
) -> CsValue {
    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String(cache_control.into()),
    );
    for (k, v) in extra_headers {
        headers.insert(k.clone(), CsValue::String(v.clone()));
    }
    let mut file = HashMap::new();
    file.insert("kind".to_string(), CsValue::String("file".into()));
    if let Some(st) = status {
        file.insert("status".to_string(), CsValue::Number(st as f64));
    }
    file.insert(
        "contentType".to_string(),
        CsValue::String(content_type.to_string()),
    );
    let is_text = content_type.to_ascii_lowercase().starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("javascript");
    if is_text && std::str::from_utf8(bytes).is_ok() {
        file.insert(
            "bodyUtf8".to_string(),
            CsValue::String(String::from_utf8_lossy(bytes).into_owned()),
        );
    } else {
        file.insert("bodyBase64".to_string(), CsValue::String(B64.encode(bytes)));
    }
    file.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(file)
}

fn hook_file_object_bytes(bytes: &[u8], content_type: &str, cache_control: &str) -> CsValue {
    hook_file_object_bytes_with_status(bytes, content_type, cache_control, None, &HashMap::new())
}

fn hook_file_object_asset_error(err: &AssetResolveError) -> CsValue {
    let seed = err
        .seed
        .clone()
        .unwrap_or_else(|| match err.cid.as_deref() {
            Some(cid) => crate::git::ipfs::probe_seed_status(cid),
            None => crate::git::ipfs::SeedStatus::unknown("no CID"),
        });
    let body =
        crate::git::ipfs::asset_status_body(Some(err), err.cid.as_deref(), "", &seed).to_string();
    let mut headers = HashMap::new();
    headers.insert(
        ASSET_ERROR_HEADER.to_string(),
        CsValue::String(err.code.as_str().into()),
    );
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String("public, max-age=30".into()),
    );
    headers.insert(
        "Content-Type".to_string(),
        CsValue::String("application/vnd.relay.asset-status+json; charset=utf-8".into()),
    );
    if let Some(ra) = err.retry_after_secs {
        headers.insert("Retry-After".to_string(), CsValue::String(ra.to_string()));
    }
    if let Some(pct) = seed.progress_pct {
        headers.insert(
            "X-Relay-Asset-Progress".to_string(),
            CsValue::String(pct.to_string()),
        );
    }
    headers.insert(
        "X-Relay-Asset-Seed-State".to_string(),
        CsValue::String(seed.state.clone()),
    );
    let mut file = HashMap::new();
    file.insert("kind".to_string(), CsValue::String("file".into()));
    file.insert(
        "status".to_string(),
        CsValue::Number(err.code.status().as_u16() as f64),
    );
    file.insert(
        "contentType".to_string(),
        CsValue::String("application/vnd.relay.asset-status+json; charset=utf-8".into()),
    );
    file.insert("bodyUtf8".to_string(), CsValue::String(body));
    file.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(file)
}

fn hook_file_object_html(html: &str) -> CsValue {
    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String("public, max-age=3600".into()),
    );
    headers.insert("Vary".to_string(), CsValue::String("Accept".into()));
    let mut file = HashMap::new();
    file.insert("kind".to_string(), CsValue::String("file".into()));
    file.insert(
        "contentType".to_string(),
        CsValue::String("text/html; charset=utf-8".into()),
    );
    file.insert("bodyUtf8".to_string(), CsValue::String(html.to_string()));
    file.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(file)
}

fn hook_file_object_ssr_html(html: String) -> CsValue {
    hook_file_object_ssr_html_with_status(html, None, false)
}

fn hook_file_object_ssr_html_with_status(
    html: String,
    status: Option<u16>,
    noindex: bool,
) -> CsValue {
    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String(crate::git::ssr_html_cache::SSR_HTML_CACHE_CONTROL.into()),
    );
    headers.insert("Vary".to_string(), CsValue::String("Accept".into()));
    if noindex {
        headers.insert(
            "X-Robots-Tag".to_string(),
            CsValue::String("noindex, follow".into()),
        );
    }
    let mut file = HashMap::new();
    file.insert("kind".to_string(), CsValue::String("file".into()));
    file.insert(
        "contentType".to_string(),
        CsValue::String("text/html; charset=utf-8".into()),
    );
    file.insert("bodyUtf8".to_string(), CsValue::String(html));
    file.insert("headers".to_string(), CsValue::Object(headers));
    if let Some(code) = status {
        file.insert("status".to_string(), CsValue::Number(f64::from(code)));
    }
    CsValue::Object(file)
}

fn ctx_flag_true(m: &HashMap<String, CsValue>, key: &str) -> bool {
    match m.get(key) {
        Some(CsValue::Bool(b)) => *b,
        Some(CsValue::String(s)) => {
            let s = s.to_ascii_lowercase();
            s == "1" || s == "true" || s == "yes" || s == "on"
        }
        _ => false,
    }
}

fn markdown_body_from_ctx(
    m: &HashMap<String, CsValue>,
    repo_root: &PathBuf,
    branch: &str,
) -> Result<Vec<u8>, String> {
    if let Some(b64) = cs_obj_str(m, "bodyBase64").filter(|s| !s.is_empty()) {
        return B64
            .decode(b64.as_bytes())
            .map_err(|e| format!("markdownHtmlFromGetContext: invalid bodyBase64: {e}"));
    }
    if let Some(rel) = cs_obj_str(m, "relPath").filter(|s| !s.is_empty()) {
        return read_file_from_repo(repo_root, branch, rel).map_err(|e| e.to_string());
    }
    Err("markdownHtmlFromGetContext: missing bodyBase64 and relPath".into())
}

/// Markdown + `markdownHtmlRequest` ⇒ static HTML via md2ast. Returns **`null`** if not eligible.
fn markdown_html_from_get_context_native(
    repo_root: PathBuf,
    branch: String,
    args: &[CsValue],
) -> Result<CsValue, String> {
    let ctx_obj = args
        .first()
        .ok_or_else(|| "markdownHtmlFromGetContext requires hook context object".to_string())?;
    let CsValue::Object(m) = ctx_obj else {
        return Err("@relay/md markdownHtmlFromGetContext: first arg must be an object".into());
    };
    if !ctx_flag_true(m, "markdownHtmlRequest") {
        return Ok(CsValue::Null);
    }
    let ct = inferred_content_type(m);
    let rel = cs_obj_str(m, "relPath").unwrap_or("");
    if !crate::git::markdown_html::is_markdown_content_type(&ct)
        && !crate::git::markdown_html::is_markdown_path(rel)
    {
        return Ok(CsValue::Null);
    }
    let bytes = markdown_body_from_ctx(m, &repo_root, &branch)?;
    let md = String::from_utf8_lossy(&bytes);
    let portraits = crate::git::portrait::portrait_sibling_set(&repo_root, &branch);
    let html =
        crate::git::markdown_html::markdown_to_html_fragment_with_portraits(&md, Some(&portraits))?;
    Ok(hook_file_object_html(&html))
}

fn md_exports(repo_root: PathBuf, branch: String) -> CsValue {
    let from_ctx = {
        let repo_root = repo_root.clone();
        let branch = branch.clone();
        Rc::new(move |args: &[CsValue]| {
            markdown_html_from_get_context_native(repo_root.clone(), branch.clone(), args)
        })
    };
    let mut md = HashMap::new();
    md.insert(
        "markdownHtmlFromGetContext".to_string(),
        CsValue::NativeFn(NativeFnClosure(from_ctx)),
    );
    CsValue::Object(md)
}

fn nested_obj_str<'a>(
    obj: &'a HashMap<String, CsValue>,
    object_key: &str,
    key: &str,
) -> Option<&'a str> {
    let CsValue::Object(nested) = obj.get(object_key)? else {
        return None;
    };
    cs_obj_str(nested, key)
}

fn ssr_public_origin(ctx: &HashMap<String, CsValue>) -> String {
    let host = nested_obj_str(ctx, "requestHeaders", "host")
        .unwrap_or("")
        .trim();
    if host.is_empty() {
        return String::new();
    }
    let host_l = host.to_ascii_lowercase();
    let default_proto = if host_l.contains("localhost")
        || host_l.starts_with("127.0.0.1")
        || host_l.starts_with("[::1]")
    {
        // park-local / loopback are plain HTTP; https://…localhost:9080 fails SSL.
        "http"
    } else {
        "https"
    };
    let proto = nested_obj_str(ctx, "requestHeaders", "x-forwarded-proto")
        .and_then(|p| p.split(',').next())
        .map(str::trim)
        .filter(|p| *p == "http" || *p == "https")
        .unwrap_or(default_proto);
    format!("{proto}://{host}")
}

/// Client-hydrate HTML without running the client hook (bots / timeout / full slots).
/// `notice`: visible `#root` placeholder (timeout/busy). `None` for silent skip (HEAD/bots).
fn cheap_client_shell(
    repo: &str,
    request_branch: &str,
    request_path: &str,
    client_hook_path: &str,
    repo_root: &std::path::Path,
    ctx: &HashMap<String, CsValue>,
    notice: Option<&str>,
) -> CsValue {
    let client_hook_http = format!("/{client_hook_path}");
    let assets_prefix =
        if crate::git::web_assets::content_bare_opts_into_web_assets(repo_root, request_branch) {
            crate::git::web_assets::pinned_assets_url_prefix_from_content_bare(repo_root)
        } else {
            None
        };
    let site_cfg =
        crate::git::relay_config_cache::read_relay_config_at_path_cached(repo_root, request_branch);
    let site_name = site_cfg
        .as_ref()
        .and_then(|cfg| cfg.name.clone())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    let site_description = site_cfg
        .as_ref()
        .and_then(|cfg| cfg.description.clone())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    let missing = ctx_flag_true(ctx, "siteShellMissingPath");
    let is_home = !missing
        && (request_path == "/"
            || request_path.is_empty()
            || request_path.trim_matches('/') == "page.md");
    let html = crate::git::embed_html::embed_html_document_with_assets_and_seo_extras(
        repo,
        request_branch,
        request_path,
        &client_hook_http,
        notice,
        None,
        assets_prefix.as_deref(),
        site_name.as_deref(),
        site_description.as_deref(),
        None,
        crate::git::embed_html::EmbedSeoExtras {
            og_image: None,
            is_article: false,
            noindex: missing,
            site_name: site_name.as_deref(),
            is_website_home: is_home,
            csr_placeholder: notice.is_some(),
        },
    );
    let portraits_json = crate::git::portrait::portrait_sibling_set_json(repo_root, request_branch);
    let html = crate::git::embed_html::inject_portrait_sibling_bootstrap(&html, &portraits_json);
    if missing {
        hook_file_object_ssr_html_with_status(html, Some(404), true)
    } else {
        hook_file_object_ssr_html(html)
    }
}

/// Build `@relay/ssr`. Calling the helper is the explicit opt-in boundary: routing and
/// eligibility remain in repository-owned CleverScript.
fn ssr_exports(repo_root: PathBuf, branch: String) -> CsValue {
    let render = Rc::new(move |args: &[CsValue]| -> Result<CsValue, String> {
        let Some(CsValue::Object(ctx)) = args.first() else {
            return Err("ssrClientHookFromGetContext requires hook context object".into());
        };
        let opts = args.get(1).and_then(|v| match v {
            CsValue::Object(o) => Some(o),
            _ => None,
        });
        let client_hook_path = opts
            .and_then(|o| cs_obj_str(o, "clientHookPath"))
            .unwrap_or("hooks/client/get.jsx")
            .trim_start_matches('/');
        let request_path = format!(
            "/{}",
            cs_obj_str(ctx, "relPath")
                .unwrap_or("")
                .trim_start_matches('/')
        );
        // Render path is the URL path only. Do not substitute `/` or another cached page.
        let render_path = crate::git::ssr_policy::normalize_request_path(&request_path);
        let initial_path = render_path.clone();
        if crate::git::ssr_policy::hook_initial_path(&request_path) != render_path {
            tracing::error!(
                target: "relay_ssr",
                path = %render_path,
                "SSR hook path diverged from the request; refusing homepage rewrite"
            );
            return Ok(cheap_client_shell(
                cs_obj_str(ctx, "repo").unwrap_or("relay"),
                cs_obj_str(ctx, "branch").unwrap_or(&branch),
                &render_path,
                client_hook_path,
                &repo_root,
                ctx,
                Some(crate::git::embed_html::ssr_timeout_notice_html()),
            ));
        }
        let skip = crate::git::ssr_policy::should_skip_ssr_from_ctx(ctx);
        let slots_full = crate::git::ssr_limits::ssr_slots_full();
        let repo = cs_obj_str(ctx, "repo").unwrap_or("relay");
        let request_branch = cs_obj_str(ctx, "branch").unwrap_or(&branch);
        let origin = opts
            .and_then(|o| cs_obj_str(o, "assetBaseUrl"))
            .map(str::to_string)
            .unwrap_or_else(|| ssr_public_origin(ctx));

        let cached_exact = crate::git::ssr_html_cache::get(
            &repo_root,
            request_branch,
            &render_path,
            client_hook_path,
        )
        .filter(|html| crate::git::embed_html::ssr_html_matches_request(html, &render_path));
        if let Some(cached) = cached_exact {
            tracing::debug!(
                target: "relay_ssr",
                path = %request_path,
                "SSR HTML cache hit"
            );
            let html = if cached.contains("relay-portrait-siblings") {
                cached
            } else {
                let portraits_json =
                    crate::git::portrait::portrait_sibling_set_json(&repo_root, request_branch);
                crate::git::embed_html::inject_portrait_sibling_bootstrap(&cached, &portraits_json)
            };
            let missing = ctx_flag_true(ctx, "siteShellMissingPath");
            return Ok(if missing {
                hook_file_object_ssr_html_with_status(html, Some(404), true)
            } else {
                hook_file_object_ssr_html(html)
            });
        }

        if skip || slots_full {
            // Do not substitute another path's HTML. Hydration would keep that page.
            tracing::info!(
                target: "relay_ssr",
                path = %request_path,
                skip,
                slots_full,
                "SSR skipped; no cache (cheap client shell)"
            );
            return Ok(cheap_client_shell(
                repo,
                request_branch,
                &request_path,
                client_hook_path,
                &repo_root,
                ctx,
                if skip {
                    None
                } else {
                    Some(crate::git::embed_html::ssr_busy_notice_html())
                },
            ));
        }

        let Some(rendered) = crate::git::ssr_client_hook::try_ssr_client_hook(
            &crate::git::ssr_client_hook::ClientHookSsrRequest {
                repo_path: &repo_root,
                branch: request_branch,
                client_hook_rel: client_hook_path,
                asset_base_url: &origin,
                initial_path: &initial_path,
            },
        ) else {
            // A timeout must not serve some other URL's SSR document.
            tracing::warn!(
                target: "relay_ssr",
                path = %request_path,
                "SSR miss/timeout; no cache (cheap client shell)"
            );
            return Ok(cheap_client_shell(
                repo,
                request_branch,
                &request_path,
                client_hook_path,
                &repo_root,
                ctx,
                Some(crate::git::embed_html::ssr_timeout_notice_html()),
            ));
        };
        let client_hook_http = format!("/{}", client_hook_path);
        let assets_prefix = if crate::git::web_assets::content_bare_opts_into_web_assets(
            &repo_root,
            request_branch,
        ) {
            crate::git::web_assets::pinned_assets_url_prefix_from_content_bare(&repo_root)
        } else {
            None
        };
        let site_cfg = crate::git::relay_config_cache::read_relay_config_at_path_cached(
            &repo_root,
            request_branch,
        );
        let site_name = site_cfg
            .as_ref()
            .and_then(|cfg| cfg.name.clone())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        let site_description_cfg = site_cfg
            .as_ref()
            .and_then(|cfg| cfg.description.clone())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        let public_canonical_origin = site_cfg
            .as_ref()
            .and_then(|cfg| cfg.canonical.clone())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());

        // Per-article SEO from tip markdown (no SQLite / QUERY). Hook may have
        // rendered `/` (shell-only); wrap this request path.
        let rel_for_read = request_path.trim_start_matches('/');
        let md_rel_guess = if rel_for_read.is_empty() {
            Some("page.md".to_string())
        } else if crate::git::markdown_html::is_markdown_path(rel_for_read) {
            Some(rel_for_read.to_string())
        } else if !rel_for_read.rsplit('/').next().unwrap_or("").contains('.') {
            // Pretty path without extension → try `<path>.md`
            Some(format!("{rel_for_read}.md"))
        } else {
            None
        };
        let article_seo =
            md_rel_guess.and_then(|md_rel| {
                match crate::git::repo::read_file_from_repo(&repo_root, request_branch, &md_rel) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        Some(crate::git::seo_from_markdown::seo_from_markdown(
                            crate::git::seo_from_markdown::SeoFromMarkdownInput {
                                markdown: &text,
                                rel_path: &md_rel,
                                public_origin: public_canonical_origin.as_deref(),
                                request_origin: Some(origin.as_str()),
                                site_name: site_name.as_deref(),
                            },
                        ))
                    }
                    Err(_) => None,
                }
            });

        let doc_title = article_seo
            .as_ref()
            .map(|s| s.title.clone())
            .or_else(|| site_name.clone());
        let doc_description = article_seo
            .as_ref()
            .and_then(|s| s.description.clone())
            .or_else(|| site_description_cfg.clone());
        let doc_canonical = article_seo
            .as_ref()
            .and_then(|s| s.canonical_url.clone())
            .or_else(|| {
                // Site-level canonical alone is only useful for `/`.
                public_canonical_origin.clone()
            });
        let og_image = article_seo.as_ref().and_then(|s| s.image_url.clone());
        let is_article = article_seo.as_ref().map(|s| s.is_article).unwrap_or(false);
        let missing = ctx_flag_true(ctx, "siteShellMissingPath");
        let is_home = !missing
            && (request_path == "/"
                || request_path.is_empty()
                || request_path.trim_matches('/') == "page.md");

        let html = crate::git::embed_html::embed_html_document_with_assets_and_seo_extras(
            repo,
            request_branch,
            &render_path,
            &client_hook_http,
            Some(&rendered.body_html),
            if rendered.css.trim().is_empty() {
                None
            } else {
                Some(rendered.css.as_str())
            },
            assets_prefix.as_deref(),
            doc_title.as_deref(),
            doc_description.as_deref(),
            doc_canonical.as_deref(),
            crate::git::embed_html::EmbedSeoExtras {
                og_image: og_image.as_deref(),
                is_article: is_article && !missing,
                noindex: missing,
                site_name: site_name.as_deref(),
                is_website_home: is_home,
                csr_placeholder: false,
            },
        );
        let portraits_json =
            crate::git::portrait::portrait_sibling_set_json(&repo_root, request_branch);
        let html =
            crate::git::embed_html::inject_portrait_sibling_bootstrap(&html, &portraits_json);
        if !crate::git::embed_html::ssr_html_matches_request(&html, &render_path) {
            tracing::error!(
                target: "relay_ssr",
                path = %render_path,
                "SSR document path marker mismatch; not caching"
            );
            return Ok(cheap_client_shell(
                repo,
                request_branch,
                &render_path,
                client_hook_path,
                &repo_root,
                ctx,
                Some(crate::git::embed_html::ssr_timeout_notice_html()),
            ));
        }
        crate::git::ssr_html_cache::put(
            &repo_root,
            request_branch,
            &render_path,
            client_hook_path,
            html.clone(),
        );
        Ok(if missing {
            hook_file_object_ssr_html_with_status(html, Some(404), true)
        } else {
            hook_file_object_ssr_html(html)
        })
    });
    let mut module = HashMap::new();
    module.insert(
        "ssrClientHookFromGetContext".to_string(),
        CsValue::NativeFn(NativeFnClosure(render)),
    );
    CsValue::Object(module)
}

/// `@relay/web-assets`: declare catalog CDN routes + read commit-pinned URL prefixes.
fn web_assets_exports(repo_root: PathBuf) -> CsValue {
    let pinned = {
        let repo_root = repo_root.clone();
        Rc::new(move |_args: &[CsValue]| -> Result<CsValue, String> {
            Ok(
                match crate::git::web_assets::pinned_assets_url_prefix_from_content_bare(&repo_root)
                {
                    Some(p) => CsValue::String(p),
                    None => CsValue::Null,
                },
            )
        })
    };
    let wasm = {
        let repo_root = repo_root.clone();
        Rc::new(move |_args: &[CsValue]| -> Result<CsValue, String> {
            Ok(
                match crate::git::web_assets::pinned_wasm_url_prefix_from_content_bare(&repo_root) {
                    Some(p) => CsValue::String(p),
                    None => CsValue::Null,
                },
            )
        })
    };
    let mut module = HashMap::new();
    module.insert(
        "pinnedPrefix".to_string(),
        CsValue::NativeFn(NativeFnClosure(pinned)),
    );
    module.insert(
        "wasmPrefix".to_string(),
        CsValue::NativeFn(NativeFnClosure(wasm)),
    );
    CsValue::Object(module)
}

/// Raster resize/WebP (+ disk cache). Requires **`opts.width`**; optional **`bytes`** from `fetchIpfsBytes`.
fn optimize_raster_from_get_context_native(
    repo_root: PathBuf,
    branch: String,
    repo_slug: String,
    cache: Arc<ImageDiskCache>,
    args: &[CsValue],
) -> Result<CsValue, String> {
    let ctx_obj = args
        .first()
        .ok_or_else(|| "optimizeRasterFromGetContext requires hook context object".to_string())?;
    let CsValue::Object(m) = ctx_obj else {
        return Err(
            "@relay/image optimizeRasterFromGetContext: first arg must be an object".into(),
        );
    };
    let (bytes_arg, max_w) = parse_optimize_raster_args(args);
    let Some(max_w) = max_w else {
        return Ok(CsValue::Null);
    };

    let (raw, cache_seg) = if let Some(o) = bytes_arg {
        let fetched = fetched_bytes_from_cs_object(o)?;
        if !is_raster_content_type(&fetched.content_type) {
            return Ok(CsValue::Null);
        }
        (fetched.bytes, fetched.cache_key)
    } else {
        let ct = inferred_content_type(m);
        if !is_raster_content_type(&ct) {
            return Ok(CsValue::Null);
        }
        let git_bytes = optional_body_bytes_from_ctx(m, &repo_root, &branch)?.unwrap_or_default();
        if git_bytes.is_empty() {
            return Ok(CsValue::Null);
        }
        let blob_explicit = cs_obj_str(m, "blobOid").unwrap_or("");
        let blob_seg = if blob_explicit.is_empty() {
            hex::encode(Sha256::digest(git_bytes.as_slice()))
        } else {
            blob_explicit.to_string()
        };
        (git_bytes, blob_seg)
    };

    let commit = cs_obj_str(m, "commitSha").unwrap_or("");
    let key = cache_key_digest(&repo_slug, &branch, commit, &cache_seg, max_w);

    if let Ok(Some(cached)) = cache.read_cached(&key) {
        return Ok(hook_file_object_webp(&cached));
    }

    let webp =
        crate::git::image_transform::resize_to_webp(&raw, max_w).map_err(|e| e.to_string())?;

    let _ = cache.write_and_evict(&key, &webp);

    Ok(hook_file_object_webp(&webp))
}

fn query_width_from_get_context_native(args: &[CsValue]) -> Result<CsValue, String> {
    let ctx_obj = args
        .first()
        .ok_or_else(|| "queryWidth requires hook context object".to_string())?;
    let CsValue::Object(m) = ctx_obj else {
        return Err("@relay/image queryWidth: first arg must be an object".into());
    };
    let keys = width_keys_from_opts(args);
    Ok(query_width_from_ctx(m, &keys)
        .map(|w| CsValue::Number(w as f64))
        .unwrap_or(CsValue::Null))
}

fn parse_wait_from_ctx(m: &HashMap<String, CsValue>) -> u64 {
    if let Some(CsValue::Number(n)) = m.get("ipfsWait") {
        return (*n as u64).min(ipfs::wait_max_from_env());
    }
    let q = m.get("query").and_then(|v| match v {
        CsValue::Object(inner) => Some(inner),
        _ => None,
    });
    let from_q = q
        .and_then(|q| q.get("wait"))
        .and_then(|v| match v {
            CsValue::String(s) => s.parse::<u64>().ok(),
            CsValue::Number(n) => Some(*n as u64),
            _ => None,
        })
        .unwrap_or(0);
    from_q.min(ipfs::wait_max_from_env())
}

fn fetch_ipfs_bytes_from_get_context_native(
    repo_root: PathBuf,
    branch: String,
    args: &[CsValue],
) -> Result<CsValue, String> {
    let ctx_obj = args
        .first()
        .ok_or_else(|| "fetchIpfsBytesFromGetContext requires hook context object".to_string())?;
    let CsValue::Object(m) = ctx_obj else {
        return Err("@relay/ipfs fetchIpfsBytesFromGetContext: first arg must be an object".into());
    };
    let opts = parse_ipfs_resolve_options(args);
    match fetch_ipfs_bytes_inner(m, &repo_root, &branch, &opts)? {
        FetchIpfsOutcome::Ok(fetched) => Ok(ipfs_bytes_to_cs_object(&fetched)),
        FetchIpfsOutcome::Miss => Ok(CsValue::Null),
        FetchIpfsOutcome::AssetErr(e) => Ok(hook_file_object_asset_error(&e)),
    }
}

fn ctx_wants_asset_status(m: &HashMap<String, CsValue>) -> bool {
    if let Some(CsValue::Object(q)) = m.get("query") {
        if let Some(CsValue::String(s)) = q.get("relayAssetStatus") {
            let t = s.trim();
            if t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes") {
                return true;
            }
        }
    }
    if let Some(CsValue::Object(headers)) = m.get("headers") {
        for (k, v) in headers {
            if k.eq_ignore_ascii_case("accept") {
                if let CsValue::String(a) = v {
                    if a.contains("application/vnd.relay.asset-status+json") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn hook_file_object_asset_status_ok(cid: &str, rel: &str) -> CsValue {
    let seed = ipfs::SeedStatus::cached();
    let body = ipfs::asset_status_body(None, Some(cid), rel, &seed).to_string();
    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String("public, max-age=60".into()),
    );
    headers.insert(
        "X-Relay-Asset-Seed-State".to_string(),
        CsValue::String("cached".into()),
    );
    headers.insert(
        "X-Relay-Asset-Progress".to_string(),
        CsValue::String("100".into()),
    );
    let mut file = HashMap::new();
    file.insert("kind".to_string(), CsValue::String("file".into()));
    file.insert("status".to_string(), CsValue::Number(200.0));
    file.insert(
        "contentType".to_string(),
        CsValue::String("application/vnd.relay.asset-status+json; charset=utf-8".into()),
    );
    file.insert("bodyUtf8".to_string(), CsValue::String(body));
    file.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(file)
}

/// IPFS pointer or manifest-backed path ⇒ fetch CID (disk cache, then gateway). Returns **`null`** if not eligible.
fn resolve_ipfs_from_get_context_native(
    repo_root: PathBuf,
    branch: String,
    args: &[CsValue],
) -> Result<CsValue, String> {
    let ctx_obj = args
        .first()
        .ok_or_else(|| "resolveIpfsFromGetContext requires hook context object".to_string())?;
    let CsValue::Object(m) = ctx_obj else {
        return Err("@relay/ipfs resolveIpfsFromGetContext: first arg must be an object".into());
    };
    let opts = parse_ipfs_resolve_options(args);
    let status_only = ctx_wants_asset_status(m);
    if status_only {
        let rel = cs_obj_str(m, "relPath")
            .unwrap_or("")
            .trim_start_matches('/');
        if rel.is_empty() {
            return Ok(CsValue::Null);
        }
        let inline = optional_body_bytes_from_ctx(m, &repo_root, &branch)?;
        let Some(pointer) =
            resolve_pointer_with_options(&opts, &repo_root, &branch, rel, inline.as_deref())
        else {
            return Ok(CsValue::Null);
        };
        let seed = ipfs::probe_seed_status(&pointer.cid);
        if seed.state == "cached" {
            return Ok(hook_file_object_asset_status_ok(&pointer.cid, rel));
        }
        let err = if seed.state == "unavailable" {
            AssetResolveError::new(
                ipfs::AssetErrorCode::IpfsNotFound,
                "CID has no known providers",
                Some(pointer.cid.clone()),
            )
            .with_seed(seed)
            .with_retry_after(None)
        } else if seed.state == "fetching" {
            AssetResolveError::new(
                ipfs::AssetErrorCode::IpfsBusy,
                "fetch already in progress on this node",
                Some(pointer.cid.clone()),
            )
            .with_seed(seed)
            .with_retry_after(Some(2))
        } else {
            AssetResolveError::new(
                ipfs::AssetErrorCode::IpfsPending,
                "CID not ready on this gateway yet",
                Some(pointer.cid.clone()),
            )
            .with_seed(seed)
            .with_retry_after(Some(5))
        };
        return Ok(hook_file_object_asset_error(&err));
    }
    match fetch_ipfs_bytes_inner(m, &repo_root, &branch, &opts)? {
        FetchIpfsOutcome::Ok(fetched) => Ok(hook_file_object_bytes_with_status(
            &fetched.bytes,
            &fetched.content_type,
            "public, max-age=604800",
            fetched.status,
            &fetched.extra_headers,
        )),
        FetchIpfsOutcome::Miss => Ok(CsValue::Null),
        FetchIpfsOutcome::AssetErr(e) => Ok(hook_file_object_asset_error(&e)),
    }
}

fn ipfs_exports(repo_root: PathBuf, branch: String) -> CsValue {
    let repo_fetch = repo_root.clone();
    let branch_fetch = branch.clone();
    let fetch_bytes = Rc::new(move |args: &[CsValue]| {
        fetch_ipfs_bytes_from_get_context_native(repo_fetch.clone(), branch_fetch.clone(), args)
    });
    let repo_resolve = repo_root.clone();
    let branch_resolve = branch.clone();
    let resolve = Rc::new(move |args: &[CsValue]| {
        resolve_ipfs_from_get_context_native(repo_resolve.clone(), branch_resolve.clone(), args)
    });
    let mut ipfs_mod = HashMap::new();
    ipfs_mod.insert(
        "fetchIpfsBytesFromGetContext".to_string(),
        CsValue::NativeFn(NativeFnClosure(fetch_bytes)),
    );
    ipfs_mod.insert(
        "resolveIpfsFromGetContext".to_string(),
        CsValue::NativeFn(NativeFnClosure(resolve)),
    );
    CsValue::Object(ipfs_mod)
}

fn image_exports(
    repo_root: PathBuf,
    branch: String,
    repo_slug: String,
    disk_cache: Arc<ImageDiskCache>,
) -> CsValue {
    let cache = disk_cache.clone();
    let to_webp = Rc::new(move |args: &[CsValue]| {
        let b64 = args
            .first()
            .and_then(|v| match v {
                CsValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .ok_or_else(|| "@relay/image toWebp requires base64 string".to_string())?;
        let max_w = args
            .get(1)
            .and_then(|v| match v {
                CsValue::Number(n) => Some(*n as u32),
                _ => None,
            })
            .unwrap_or(800);
        let raw = B64
            .decode(b64.as_bytes())
            .map_err(|e| format!("base64: {}", e))?;
        let out =
            crate::git::image_transform::resize_to_webp(&raw, max_w).map_err(|e| e.to_string())?;
        Ok(CsValue::String(B64.encode(out)))
    });
    let from_ctx = {
        let repo_root = repo_root.clone();
        let branch = branch.clone();
        let repo_slug = repo_slug.clone();
        let cache = cache.clone();
        Rc::new(move |args: &[CsValue]| {
            optimize_raster_from_get_context_native(
                repo_root.clone(),
                branch.clone(),
                repo_slug.clone(),
                cache.clone(),
                args,
            )
        })
    };
    let portrait_if = {
        let repo_root = repo_root.clone();
        let branch = branch.clone();
        Rc::new(move |args: &[CsValue]| -> Result<CsValue, String> {
            let src = args
                .first()
                .and_then(|v| match v {
                    CsValue::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            match crate::git::portrait::portrait_sibling_if_present(&repo_root, &branch, src) {
                Some(p) => Ok(CsValue::String(p)),
                None => Ok(CsValue::String(String::new())),
            }
        })
    };
    let mut image = HashMap::new();
    image.insert(
        "toWebp".to_string(),
        CsValue::NativeFn(NativeFnClosure(to_webp)),
    );
    let query_w = Rc::new(move |args: &[CsValue]| query_width_from_get_context_native(args));
    image.insert(
        "queryWidth".to_string(),
        CsValue::NativeFn(NativeFnClosure(query_w)),
    );
    image.insert(
        "optimizeRasterFromGetContext".to_string(),
        CsValue::NativeFn(NativeFnClosure(from_ctx)),
    );
    image.insert(
        "portraitSiblingIfPresent".to_string(),
        CsValue::NativeFn(NativeFnClosure(portrait_if)),
    );
    CsValue::Object(image)
}

/// Host for server hooks: delegates browser stubs to [`MockHost`]; resolves `@relay/git`,
/// `@relay/search/*`, and (when enabled) `@relay/image` / `@relay/ipfs` / `@relay/md`.
pub struct CleverScriptServerHost {
    inner: Rc<RefCell<MockHost>>,
    git_exports: CsValue,
    search_sqlite_exports: CsValue,
    search_redb_exports: CsValue,
    search_chat_exports: CsValue,
    image_exports: Option<CsValue>,
    ipfs_exports: Option<CsValue>,
    md_exports: Option<CsValue>,
    ssr_exports: Option<CsValue>,
    web_assets_exports: Option<CsValue>,
    #[allow(dead_code)]
    image_disk_cache: Option<Arc<ImageDiskCache>>,
}

impl CleverScriptServerHost {
    pub fn new(repo_root: PathBuf, branch: String, include_image: bool) -> Self {
        let repo_slug = repo_root
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("repo")
            .to_string();
        let git_exports = git_exports(repo_root.clone(), branch.clone());
        let search_sqlite_exports =
            search_plugin_exports(repo_root.clone(), branch.clone(), PLUGIN_SQLITE);
        let search_redb_exports =
            search_plugin_exports(repo_root.clone(), branch.clone(), PLUGIN_REDB);
        let search_chat_exports =
            search_plugin_exports(repo_root.clone(), branch.clone(), PLUGIN_CHAT);
        let (image_exports, image_disk_cache) = if include_image {
            let dc = Arc::new(ImageDiskCache::from_env());
            (
                Some(image_exports(
                    repo_root.clone(),
                    branch.clone(),
                    repo_slug,
                    dc.clone(),
                )),
                Some(dc),
            )
        } else {
            (None, None)
        };
        let md_exports = if include_image {
            Some(md_exports(repo_root.clone(), branch.clone()))
        } else {
            None
        };
        let ssr_exports = if include_image {
            Some(ssr_exports(repo_root.clone(), branch.clone()))
        } else {
            None
        };
        let web_assets_exports = if include_image {
            Some(web_assets_exports(repo_root.clone()))
        } else {
            None
        };
        let ipfs_exports = if include_image {
            Some(ipfs_exports(repo_root.clone(), branch.clone()))
        } else {
            None
        };
        Self {
            inner: MockHost::new(),
            git_exports,
            search_sqlite_exports,
            search_redb_exports,
            search_chat_exports,
            image_exports,
            ipfs_exports,
            md_exports,
            ssr_exports,
            web_assets_exports,
            image_disk_cache,
        }
    }

    pub fn host_handle(repo_root: PathBuf, branch: String, include_image: bool) -> HostHandle {
        Rc::new(RefCell::new(Self::new(repo_root, branch, include_image)))
    }
}

impl Host for CleverScriptServerHost {
    fn fetch(&self, url: &str, options: Option<&CsValue>) -> Result<CsValue, String> {
        self.inner.borrow().fetch(url, options)
    }

    fn navigate(&self, path: &str) -> Result<(), String> {
        self.inner.borrow().navigate(path)
    }

    fn create_element(
        &self,
        tag: &str,
        props: &CsValue,
        children: &[CsValue],
    ) -> Result<CsValue, String> {
        self.inner.borrow().create_element(tag, props, children)
    }

    fn load_module(&self, path: &str) -> Result<CsValue, String> {
        if let Some(m) = relay_runtime::relay_modules::try_load_relay_module(path) {
            return Ok(m);
        }
        match path {
            RELAY_GIT_MODULE => Ok(self.git_exports.clone()),
            RELAY_SEARCH_SQLITE_MODULE => Ok(self.search_sqlite_exports.clone()),
            RELAY_SEARCH_REDB_MODULE => Ok(self.search_redb_exports.clone()),
            RELAY_SEARCH_CHAT_MODULE => Ok(self.search_chat_exports.clone()),
            RELAY_IMAGE_MODULE => self
                .image_exports
                .as_ref()
                .cloned()
                .ok_or_else(|| format!("unknown module {:?}", path)),
            RELAY_IPFS_MODULE => self
                .ipfs_exports
                .as_ref()
                .cloned()
                .ok_or_else(|| format!("unknown module {:?}", path)),
            RELAY_MD_MODULE => self
                .md_exports
                .as_ref()
                .cloned()
                .ok_or_else(|| format!("unknown module {:?}", path)),
            RELAY_SSR_MODULE => self
                .ssr_exports
                .as_ref()
                .cloned()
                .ok_or_else(|| format!("unknown module {:?}", path)),
            RELAY_WEB_ASSETS_MODULE => self
                .web_assets_exports
                .as_ref()
                .cloned()
                .ok_or_else(|| format!("unknown module {:?}", path)),
            _ => Err(format!("unknown module {:?}", path)),
        }
    }

    fn log(&self, level: &str, message: &str) {
        self.inner.borrow().log(level, message)
    }
}

/// **`next`** for GET hooks (`export default function run(ctx, next)`): reads `ctx.relPath` from git and
/// returns a hook-shaped file object, mirroring the default template GET fallback (UTF-8 vs base64
/// bodies; JSON body with HTTP 404 on read failure).
pub fn hook_default_next_native_fn(
    ctx: serde_json::Value,
    repo_root: PathBuf,
    branch: String,
) -> CsValue {
    CsValue::NativeFn(NativeFnClosure(Rc::new(move |_args| {
        default_plain_file_hook_from_context(&ctx, &repo_root, &branch)
    })))
}

fn default_plain_file_hook_from_context(
    ctx: &serde_json::Value,
    repo_root: &PathBuf,
    branch: &str,
) -> Result<CsValue, String> {
    let rel = ctx.get("relPath").and_then(|v| v.as_str()).unwrap_or("");
    let ct_field = ctx
        .get("contentType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let inferred = (!rel.is_empty()).then(|| {
        mime_guess::from_path(rel)
            .first_or_octet_stream()
            .essence_str()
            .to_string()
    });
    let ct = if !ct_field.is_empty() {
        ct_field.to_string()
    } else {
        inferred.unwrap_or_default()
    };

    match read_file_from_repo(repo_root, branch, rel) {
        Ok(bytes) => Ok(default_plain_ok_response(&ct, &bytes)),
        Err(_) => Ok(default_plain_not_found(rel)),
    }
}

fn default_plain_ok_response(declared_ct: &str, bytes: &[u8]) -> CsValue {
    let ct_trim = declared_ct.trim();
    let ct_lower = ct_trim.to_ascii_lowercase();
    let is_text_like = ct_lower.starts_with("text/") || ct_lower.starts_with("application/json");

    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String("public, max-age=3600".into()),
    );

    let mut obj = HashMap::new();
    obj.insert("kind".to_string(), CsValue::String("file".into()));

    if is_text_like {
        let content_type = if ct_trim.is_empty() {
            "text/plain; charset=utf-8".to_string()
        } else {
            ct_trim.to_string()
        };
        obj.insert("contentType".to_string(), CsValue::String(content_type));
        obj.insert(
            "bodyUtf8".to_string(),
            CsValue::String(String::from_utf8_lossy(bytes).into_owned()),
        );
    } else {
        let content_type = if ct_trim.is_empty() {
            "application/octet-stream".to_string()
        } else {
            ct_trim.to_string()
        };
        obj.insert("contentType".to_string(), CsValue::String(content_type));
        obj.insert("bodyBase64".to_string(), CsValue::String(B64.encode(bytes)));
    }

    obj.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(obj)
}

fn default_plain_not_found(rel_path: &str) -> CsValue {
    let body = serde_json::json!({
        "ok": false,
        "status": "not_found",
        "path": rel_path,
        "messageForReaders": "There is nothing at this path in the repository branch you requested. If you followed a link or typed the URL, check the path spelling and branch name.",
        "hintForDebugging": format!(
            "read_file_from_repo failed for relPath `{}` (branch HEAD on this gateway); file missing, wrong branch, or not yet pushed.",
            rel_path
        ),
    }).to_string();
    let mut headers = HashMap::new();
    headers.insert(
        "Cache-Control".to_string(),
        CsValue::String("public, max-age=60".into()),
    );
    let mut obj = HashMap::new();
    obj.insert("kind".to_string(), CsValue::String("file".into()));
    obj.insert("status".to_string(), CsValue::Number(404.0));
    obj.insert(
        "contentType".to_string(),
        CsValue::String("application/json; charset=utf-8".into()),
    );
    obj.insert("bodyUtf8".to_string(), CsValue::String(body));
    obj.insert("headers".to_string(), CsValue::Object(headers));
    CsValue::Object(obj)
}

/// Insert `requestHeaders` (sanitized) into hook JSON context.
pub fn augment_get_context_headers(context: &mut Value, headers: &HeaderMap) {
    let Value::Object(map) = context else {
        return;
    };
    map.insert(
        "requestHeaders".to_string(),
        crate::git::request_headers::request_headers_object(headers),
    );
}

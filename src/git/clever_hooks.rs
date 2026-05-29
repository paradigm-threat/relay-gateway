//! CleverScript execution for server hooks (GET + git hooks) with virtual `@relay/*` modules.

use relay_runtime::run_query_script;
use std::path::PathBuf;

/// Run CleverScript default export; returns JSON of the hook return value.
///
/// `include_image`: when true, `@relay/image`, `@relay/ipfs`, and `@relay/md` resolve on GET hooks.
/// When false, those modules are unknown (git hooks / QUERY).
///
/// Runtime failures are `anyhow::Error` values that wrap `relay_runtime::RuntimeError` so the gateway
/// can return HTTP 500 JSON with `technicalReport` plus structured `cleverScript` when available.
pub fn run_clever_hook(
    source: &str,
    context: &serde_json::Value,
    repo_root: PathBuf,
    branch: String,
    include_image: bool,
) -> anyhow::Result<serde_json::Value> {
    let next_fn = crate::git::clever_script_host::hook_default_next_native_fn(
        context.clone(),
        repo_root.clone(),
        branch.clone(),
    );
    let host = crate::git::clever_script_host::CleverScriptServerHost::host_handle(
        repo_root,
        branch,
        include_image,
    );
    let raw =
        run_query_script(source, host, context, None, Some(next_fn)).map_err(anyhow::Error::new)?;
    Ok(raw.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use git2::{Repository, Signature};
    use image::{Rgb, RgbImage, ImageBuffer, ImageFormat, Rgba};
    use serial_test::serial;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[test]
    fn run_clever_hook_returns_json() {
        let j = run_clever_hook(
            "export default function run(c) { return { n: 7 }; }",
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap();
        assert_eq!(j["n"].as_f64(), Some(7.0));
    }

    #[test]
    fn run_clever_import_git_readfile_reads_blob() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("z.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let oid = repo.blob(b"payload-bytes").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("note.txt", oid, 0o100644).unwrap();
        let tid = tb.write().unwrap();
        let tree = repo.find_tree(tid).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "c", &tree, &[])
            .unwrap();
        let src = r#"import { readFile } from '@relay/git';
export default function run(c) {
  return { body: readFile("note.txt") };
}"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({}),
            repo_path,
            "main".into(),
            false,
        )
        .unwrap();
        assert_eq!(j["body"], "payload-bytes");
    }

    #[test]
    fn get_hook_next_serves_plain_file_when_optimize_skips() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("z.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let oid = repo.blob(b"payload-bytes").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("note.txt", oid, 0o100644).unwrap();
        let tid = tb.write().unwrap();
        let tree = repo.find_tree(tid).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "c", &tree, &[])
            .unwrap();
        let src = r#"import { optimizeRasterFromGetContext } from '@relay/image';
export default function run(ctx, next) {
  var o = optimizeRasterFromGetContext(ctx);
  if (o !== null) return o;
  return next();
}"#;
        let ctx = serde_json::json!({
            "relPath": "note.txt",
            "contentType": "text/plain",
        });
        let j = run_clever_hook(src, &ctx, repo_path, "main".into(), true).unwrap();
        assert_eq!(j["kind"], "file");
        assert_eq!(j["bodyUtf8"], "payload-bytes");
    }

    #[test]
    fn next_returns_structured_404_when_repo_path_missing() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("empty.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let tid = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tid).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "empty", &tree, &[])
            .unwrap();

        let src = "export default function run(ctx, next) { return next(); }";
        let ctx = serde_json::json!({
            "relPath": "no-such-blob.bin",
            "contentType": "application/octet-stream",
        });
        let j = run_clever_hook(src, &ctx, repo_path, "main".into(), true).unwrap();
        assert_eq!(j["kind"], "file");
        let st = &j["status"];
        assert!(
            st.as_u64() == Some(404)
                || st.as_i64() == Some(404)
                || st.as_f64() == Some(404.0),
            "unexpected status field: {:?}",
            st
        );

        let body: serde_json::Value =
            serde_json::from_str(j["bodyUtf8"].as_str().unwrap()).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["status"], "not_found");
        assert_eq!(body["path"], "no-such-blob.bin");
        assert!(body["messageForReaders"].as_str().unwrap().len() > 30);
        let hint = body["hintForDebugging"].as_str().unwrap();
        assert!(
            hint.contains("no-such-blob.bin") && hint.contains("read_file_from_repo"),
            "hint={hint:?}"
        );
    }

    #[test]
    fn relay_global_is_null_without_import_usage() {
        let j = run_clever_hook(
            "export default function run(c) { return { relay: Relay }; }",
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap();
        assert!(j["relay"].is_null());
    }

    #[test]
    fn relay_git_without_import_errors() {
        let err = run_clever_hook(
            r#"export default function run(c) { return { body: Relay.git.readFile("x") }; }"#,
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Member access on non-object")
                || msg.contains("undefined")
                || msg.contains("readFile"),
            "unexpected error: {}",
            msg
        );
    }

    #[test]
    fn unknown_module_import_errors() {
        let err = run_clever_hook(
            r#"import { x } from '@relay/nope';
export default function run(c) { return x; }"#,
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown module"),
            "{}",
            err
        );
    }

    #[test]
    fn image_module_unknown_when_host_git_only() {
        let err = run_clever_hook(
            "import { toWebp } from '@relay/image';\nexport default function run(c) { return null; }\n",
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown module"), "{}", err);
    }

    #[test]
    fn ipfs_module_unknown_when_host_git_only() {
        let err = run_clever_hook(
            "import { resolveIpfsFromGetContext } from '@relay/ipfs';\nexport default function run(c) { return null; }\n",
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown module"), "{}", err);
    }

    fn count_webp_under(dir: &std::path::Path) -> usize {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += count_webp_under(&p);
                } else if p.extension().map(|x| x == "webp").unwrap_or(false) {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    #[serial]
    fn optimize_raster_from_get_context_writes_cache_entry() {
        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IMAGE_CACHE_ROOT", cache_dir.path());
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("z.git");
        let _ = Repository::init_bare(&repo_path).unwrap();

        let wide: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(80, 2, Rgba([7u8, 8u8, 9u8, 255u8]));
        let dyn_img = image::DynamicImage::ImageRgba8(wide);
        let mut curo = Cursor::new(Vec::new());
        dyn_img.write_to(&mut curo, ImageFormat::Png).unwrap();
        let png_b64 = B64.encode(curo.into_inner());

        let src = r#"import { queryWidth, optimizeRasterFromGetContext } from '@relay/image';
export default function run(ctx) {
  var maxW = queryWidth(ctx);
  if (maxW !== null) {
    return optimizeRasterFromGetContext(ctx, null, { width: maxW });
  }
  return null;
}"#;
        let ctx = serde_json::json!({
            "repo": "z",
            "contentType": "image/png",
            "bodyBase64": png_b64,
            "commitSha": "ccc",
            "blobOid": "ddd",
            "query": { "w": "40" },
        });

        let first = run_clever_hook(src, &ctx, repo_path.clone(), "main".into(), true).unwrap();
        assert_eq!(first["contentType"], "image/webp");

        assert!(
            count_webp_under(cache_dir.path()) >= 1,
            "cache should contain at least one .webp"
        );

        let second = run_clever_hook(src, &ctx, repo_path, "main".into(), true).unwrap();
        assert_eq!(second["bodyBase64"], first["bodyBase64"]);
    }

    #[test]
    #[serial]
    fn jpeg_base64_optimizes_via_production_style_get_hook() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("z.git");
        let _ = Repository::init_bare(&repo_path).unwrap();

        let jpeg_rgb: RgbImage = ImageBuffer::from_pixel(613, 434, Rgb([90u8, 120u8, 140u8]));
        let dyn_img = image::DynamicImage::ImageRgb8(jpeg_rgb);
        let mut cur = Cursor::new(Vec::new());
        dyn_img.write_to(&mut cur, ImageFormat::Jpeg).unwrap();
        let jpg_b64 = B64.encode(cur.into_inner());

        let src = r#"import { queryWidth, optimizeRasterFromGetContext } from '@relay/image';

export default function run(ctx, next) {
  var maxW = queryWidth(ctx);
  if (maxW !== null) {
    var optimized = optimizeRasterFromGetContext(ctx, null, { width: maxW });
    if (optimized !== null) { return optimized; }
  }
  return next();
}"#;

        let ctx = serde_json::json!({
            "relPath": "cosmos/cosmology/calendars.jpg",
            "repo": "z",
            "branch": "main",
            "contentType": "image/jpeg",
            "bodyBase64": jpg_b64,
            "commitSha": "eee",
            "blobOid": "fff",
            "query": { "w": "400" },
        });

        let j = run_clever_hook(src, &ctx, repo_path, "main".into(), true).unwrap();
        assert_eq!(j["kind"], "file");
        assert!(
            j["contentType"].as_str().unwrap().contains("webp"),
            "expected webp, got {:?}",
            j["contentType"]
        );
    }

    #[test]
    fn query_width_ignores_ctx_w_and_rejects_out_of_range() {
        let src = r#"import { queryWidth } from '@relay/image';
export default function run(ctx) { return queryWidth(ctx); }"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({ "w": 400, "query": {} }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert!(j.is_null(), "width must come from ctx.query only, not ctx.w");

        let j = run_clever_hook(
            src,
            &serde_json::json!({ "query": { "w": "0" } }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert!(j.is_null());

        let j = run_clever_hook(
            src,
            &serde_json::json!({ "query": { "w": "99999" } }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert!(j.is_null());
    }

    #[test]
    fn optimize_raster_requires_explicit_width_not_legacy_ipfs_opts() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("z.git");
        let _ = Repository::init_bare(&repo_path).unwrap();
        let wide: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(40, 2, Rgba([1u8, 2u8, 3u8, 255u8]));
        let mut curo = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(wide)
            .write_to(&mut curo, ImageFormat::Png)
            .unwrap();
        let png_b64 = B64.encode(curo.into_inner());

        let src = r#"import { optimizeRasterFromGetContext } from '@relay/image';
export default function run(ctx) {
  return optimizeRasterFromGetContext(ctx, { sources: ["inline"] });
}"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({
                "contentType": "image/png",
                "bodyBase64": png_b64,
                "query": { "w": "32" },
            }),
            repo_path,
            "main".into(),
            true,
        )
        .unwrap();
        assert!(j.is_null(), "ipfs opts must not imply width or trigger resize");
    }

    #[test]
    #[serial]
    fn fetch_ipfs_bytes_returns_object_with_cache_key() {
        use crate::git::ipfs::{format_pointer_stub, IpfsPointer};

        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IPFS_CACHE_ROOT", cache_dir.path());
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let payload = b"raw-bytes";
        std::fs::create_dir_all(cache_dir.path().join(cid)).unwrap();
        std::fs::write(cache_dir.path().join(cid).join("data"), payload).unwrap();
        let stub = format_pointer_stub(&IpfsPointer {
            cid: cid.to_string(),
            content_type: Some("image/png".into()),
            sha256: None,
        });

        let src = r#"import { fetchIpfsBytesFromGetContext } from '@relay/ipfs';
export default function run(ctx) {
  return fetchIpfsBytesFromGetContext(ctx, { sources: ["inline"] });
}"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({
                "relPath": "x.png",
                "bodyBase64": B64.encode(&stub),
            }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j["contentType"], "image/png");
        assert!(j["cacheKey"].as_str().unwrap().starts_with("ipfs:"));
        let body = B64.decode(j["bodyBase64"].as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(body, payload);
    }

    #[test]
    fn fetch_ipfs_bytes_null_without_pointer() {
        let src = r#"import { fetchIpfsBytesFromGetContext } from '@relay/ipfs';
export default function run(ctx) { return fetchIpfsBytesFromGetContext(ctx); }"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({ "relPath": "missing.png" }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert!(j.is_null());
    }

    #[test]
    #[serial]
    fn resolve_ipfs_with_w_still_returns_full_file_not_deferred() {
        use crate::git::ipfs::{format_pointer_stub, IpfsPointer};

        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IPFS_CACHE_ROOT", cache_dir.path());
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let payload = b"full-body";
        std::fs::create_dir_all(cache_dir.path().join(cid)).unwrap();
        std::fs::write(cache_dir.path().join(cid).join("data"), payload).unwrap();
        let stub = format_pointer_stub(&IpfsPointer {
            cid: cid.to_string(),
            content_type: Some("image/jpeg".into()),
            sha256: None,
        });

        let src = r#"import { resolveIpfsFromGetContext } from '@relay/ipfs';
export default function run(ctx) {
  return resolveIpfsFromGetContext(ctx, { sources: ["inline"] });
}"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({
                "relPath": "media/hero.jpg",
                "bodyBase64": B64.encode(&stub),
                "query": { "w": "64" },
            }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert!(
            j["contentType"].as_str().unwrap().contains("jpeg"),
            "resolveIpfs must not defer when ?w= is present"
        );
        let body = B64.decode(j["bodyBase64"].as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(body, payload);
    }

    #[test]
    fn query_width_reads_ctx_query_keys_in_order() {
        let src = r#"import { queryWidth } from '@relay/image';
export default function run(ctx) { return queryWidth(ctx); }"#;
        let j = run_clever_hook(
            src,
            &serde_json::json!({ "query": { "width": "100", "w": "50" } }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j.as_f64(), Some(50.0));

        let j = run_clever_hook(
            src,
            &serde_json::json!({ "query": { "maxWidth": "320" } }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j.as_f64(), Some(320.0));

        let custom = r#"import { queryWidth } from '@relay/image';
export default function run(ctx) {
  return queryWidth(ctx, { keys: ["maxWidth", "w"] });
}"#;
        let j = run_clever_hook(
            custom,
            &serde_json::json!({ "query": { "w": "10", "maxWidth": "200" } }),
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j.as_f64(), Some(200.0));
    }

    #[test]
    #[serial]
    fn resolve_ipfs_from_get_context_reads_disk_cache() {
        use crate::git::ipfs::{format_pointer_stub, IpfsPointer};

        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IPFS_CACHE_ROOT", cache_dir.path());
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let payload = b"cached-png-bytes";
        let cid_dir = cache_dir.path().join(cid);
        std::fs::create_dir_all(&cid_dir).unwrap();
        std::fs::write(cid_dir.join("data"), payload).unwrap();
        let pointer = IpfsPointer {
            cid: cid.to_string(),
            content_type: Some("image/png".into()),
            sha256: None,
        };
        let stub = format_pointer_stub(&pointer);

        let src = r#"import { resolveIpfsFromGetContext } from '@relay/ipfs';
export default function run(ctx) {
  return resolveIpfsFromGetContext(ctx, { sources: ["inline"] });
}"#;
        let ctx = serde_json::json!({
            "relPath": "media/hero.png",
            "branch": "main",
            "bodyBase64": B64.encode(&stub),
        });
        let j = run_clever_hook(
            src,
            &ctx,
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j["kind"], "file");
        assert_eq!(j["contentType"], "image/png");
        let body = B64.decode(j["bodyBase64"].as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(body, payload);
    }

    #[test]
    #[serial]
    fn ipfs_raster_with_w_defers_to_optimize_which_fetches_and_webps() {
        use crate::git::ipfs::{format_pointer_stub, IpfsPointer};

        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IPFS_CACHE_ROOT", cache_dir.path());
        std::env::set_var("RELAY_IMAGE_CACHE_ROOT", cache_dir.path().join("img"));
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let jpeg_rgb: RgbImage = ImageBuffer::from_pixel(120, 80, Rgb([10u8, 20u8, 30u8]));
        let dyn_img = image::DynamicImage::ImageRgb8(jpeg_rgb);
        let mut cur = Cursor::new(Vec::new());
        dyn_img.write_to(&mut cur, ImageFormat::Jpeg).unwrap();
        let jpeg_bytes = cur.into_inner();
        let cid_dir = cache_dir.path().join(cid);
        std::fs::create_dir_all(&cid_dir).unwrap();
        std::fs::write(cid_dir.join("data"), &jpeg_bytes).unwrap();

        let pointer = IpfsPointer {
            cid: cid.to_string(),
            content_type: Some("image/jpeg".into()),
            sha256: None,
        };
        let stub = format_pointer_stub(&pointer);

        let ipfs_opts = r#"{ sources: ["inline"] }"#;
        let src = format!(
            r#"import {{ fetchIpfsBytesFromGetContext }} from '@relay/ipfs';
import {{ queryWidth, optimizeRasterFromGetContext }} from '@relay/image';
export default function run(ctx) {{
  var opts = {ipfs_opts};
  var maxW = queryWidth(ctx);
  if (maxW !== null) {{
    var bytes = fetchIpfsBytesFromGetContext(ctx, opts);
    var optimized = optimizeRasterFromGetContext(ctx, bytes, {{ width: maxW }});
    if (optimized !== null) {{ return optimized; }}
    return null;
  }}
  return null;
}}"#
        );
        let ctx = serde_json::json!({
            "relPath": "media/hero.jpg",
            "repo": "z",
            "branch": "main",
            "contentType": "image/jpeg",
            "bodyBase64": B64.encode(&stub),
            "query": { "w": "64" },
            "commitSha": "abc",
            "blobOid": "def",
        });
        let j = run_clever_hook(
            &src,
            &ctx,
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j["kind"], "file");
        assert!(
            j["contentType"].as_str().unwrap().contains("webp"),
            "expected webp from IPFS+raster pipeline, got {:?}",
            j["contentType"]
        );
        let webp = B64.decode(j["bodyBase64"].as_str().unwrap().as_bytes()).unwrap();
        assert!(webp.starts_with(b"RIFF"));
    }

    #[test]
    #[serial]
    fn ipfs_resolve_alone_serves_cached_raster_without_w() {
        use crate::git::ipfs::{format_pointer_stub, IpfsPointer};

        let cache_dir = tempdir().unwrap();
        std::env::set_var("RELAY_IPFS_CACHE_ROOT", cache_dir.path());
        let cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        let payload = b"full-jpeg-bytes";
        let cid_dir = cache_dir.path().join(cid);
        std::fs::create_dir_all(&cid_dir).unwrap();
        std::fs::write(cid_dir.join("data"), payload).unwrap();
        let stub = format_pointer_stub(&IpfsPointer {
            cid: cid.to_string(),
            content_type: Some("image/jpeg".into()),
            sha256: None,
        });

        let src = r#"import { resolveIpfsFromGetContext } from '@relay/ipfs';
export default function run(ctx) {
  return resolveIpfsFromGetContext(ctx, { sources: ["inline"] });
}"#;
        let ctx = serde_json::json!({
            "relPath": "media/hero.jpg",
            "bodyBase64": B64.encode(&stub),
        });
        let j = run_clever_hook(
            src,
            &ctx,
            PathBuf::from("/nonexistent/repo.git"),
            "main".into(),
            true,
        )
        .unwrap();
        assert_eq!(j["contentType"], "image/jpeg");
        let body = B64.decode(j["bodyBase64"].as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(body, payload);
    }
}

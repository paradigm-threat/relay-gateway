//! HTTP mapping for CleverScript `server.hooks.get` return JSON (`kind: file` / `dir`).
//!
//! **`get_hook_execution_error_response`** builds HTTP 500 JSON when the GET hook script fails — full
//! error chain (`{:#}`), optional Rust backtrace (`RUST_BACKTRACE=1`), and CleverScript
//! diagnostics when available.

use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use relay_runtime::RuntimeError;
use serde_json::Value;

use crate::types::{HEADER_BRANCH, HEADER_REPO};

fn hook_status(val: &Value) -> StatusCode {
    let Some(raw) = val.get("status") else {
        return StatusCode::OK;
    };
    // CleverScript `CsValue::Number` is f64; tip hooks often emit `404.0`.
    // `as_u64()` is None for floats, which previously forced HTTP 200 JSON for
    // missing blobs and blanked mobile `<picture>` sources.
    let n = if let Some(u) = raw.as_u64() {
        u
    } else if let Some(f) = raw.as_f64() {
        if !f.is_finite() || f.fract() != 0.0 || f < 0.0 {
            return StatusCode::OK;
        }
        f as u64
    } else {
        return StatusCode::OK;
    };
    if !(200..=599).contains(&n) {
        return StatusCode::OK;
    }
    StatusCode::from_u16(n as u16).unwrap_or(StatusCode::OK)
}

/// Merge optional `headers` object from hook JSON. Skips invalid names/values. Does not set
/// `X-Relay-Branch` / `X-Relay-Repo` (gateway owns those).
fn merge_hook_headers(out: &mut HeaderMap, val: &Value) {
    let Some(obj) = val.get("headers").and_then(|v| v.as_object()) else {
        return;
    };
    for (k, v) in obj {
        if k.eq_ignore_ascii_case(HEADER_BRANCH) || k.eq_ignore_ascii_case(HEADER_REPO) {
            continue;
        }
        let Ok(name) = HeaderName::try_from(k.as_str()) else {
            continue;
        };
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        if let Ok(hv) = HeaderValue::try_from(s) {
            out.insert(name, hv);
        }
    }
}

/// HTTP 500 when the GET `.cs` hook throws or fails during execution — **not** for intentional 404s
/// (`next()` handles missing blobs with `status` 404 in hook JSON).
///
/// `technicalReport` is the full `{:#}` `anyhow` chain (nothing stripped for prod). Attach
/// `RUST_BACKTRACE=1` to capture **`rustBacktrace`** when Rust has backtraces enabled.
pub fn get_hook_execution_error_response(
    branch: &str,
    repo_name: &str,
    err: &anyhow::Error,
) -> Response {
    let clever = err
        .downcast_ref::<RuntimeError>()
        .map(RuntimeError::to_error_json);

    let mut body = serde_json::json!({
        "ok": false,
        "error": "get_hook_execution_failed",
        "summaryForReaders": "The site could not run its GET hook script. That is usually a mistake in the repo hook or relay configuration — not something you fixed by refreshing the page.",
        "technicalReport": format!("{:#}", err),
        "branch": branch,
        "repo": repo_name,
    });
    if let Some(clever_detail) = clever {
        body["cleverScript"] = clever_detail;
    }
    let bt = err.backtrace();
    if bt.status() == std::backtrace::BacktraceStatus::Captured {
        body["rustBacktrace"] = Value::String(format!("{}", bt));
    }

    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| {
        let tech_esc =
            serde_json::to_string(&format!("{:#}", err)).unwrap_or_else(|_| "\"\"".to_string());
        format!(
            "{{\"ok\":false,\"error\":\"serialization_failed\",\"technicalReport\":{}}}",
            tech_esc
        )
        .into_bytes()
    });

    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    if let Ok(b) = HeaderValue::try_from(branch) {
        headers.insert(HEADER_BRANCH, b);
    }
    if let Ok(r) = HeaderValue::try_from(repo_name) {
        headers.insert(HEADER_REPO, r);
    }

    let mut res = Response::new(Body::from(body_bytes));
    *res.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    *res.headers_mut() = headers;
    res
}

/// Turn GET-hook JSON into an HTTP response, or `None` if the payload is unusable.
///
/// - **`kind: "file"`** — `contentType`, and either **`bodyUtf8`** (UTF-8 text) or **`bodyBase64`** (binary).
///   Optional **`status`** (200–599),
///   optional **`headers`** (JSON object of string header names to string or number values).
/// - **`kind: "dir"`** — response body is JSON (`val`); optional **`status`** / **`headers`**.
pub fn get_hook_json_to_response(val: &Value, branch: &str, repo_name: &str) -> Option<Response> {
    let kind = val.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    match kind {
        "file" => {
            let ct = val
                .get("contentType")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream");
            let bytes: Vec<u8> = match val.get("bodyUtf8") {
                Some(Value::String(s)) => s.as_bytes().to_vec(),
                _ => {
                    let b64 = val.get("bodyBase64").and_then(|v| v.as_str())?;
                    B64.decode(b64.as_bytes()).ok()?
                }
            };
            let status = hook_status(val);
            let mut headers = HeaderMap::new();
            merge_hook_headers(&mut headers, val);
            headers.insert(CONTENT_TYPE, HeaderValue::try_from(ct).ok()?);
            headers.insert(HEADER_BRANCH, HeaderValue::try_from(branch).ok()?);
            headers.insert(HEADER_REPO, HeaderValue::try_from(repo_name).ok()?);
            let mut res = Response::new(Body::from(bytes));
            *res.status_mut() = status;
            *res.headers_mut() = headers;
            Some(res)
        }
        "dir" => {
            let status = hook_status(val);
            let body = serde_json::to_vec(val).ok()?;
            let mut headers = HeaderMap::new();
            merge_hook_headers(&mut headers, val);
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(HEADER_BRANCH, HeaderValue::try_from(branch).ok()?);
            headers.insert(HEADER_REPO, HeaderValue::try_from(repo_name).ok()?);
            let mut res = Response::new(Body::from(body));
            *res.status_mut() = status;
            *res.headers_mut() = headers;
            Some(res)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_runtime::RuntimeError;
    use serde_json::json;

    #[test]
    fn file_merges_custom_headers() {
        let val = json!({
            "kind": "file",
            "contentType": "text/plain",
            "bodyBase64": "aGk=",
            "headers": {
                "Cache-Control": "public, max-age=60",
                "X-Test": "1"
            }
        });
        let r = get_hook_json_to_response(&val, "main", "repo").unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let h = r.headers();
        assert_eq!(h.get("cache-control").unwrap(), "public, max-age=60");
        assert_eq!(h.get("x-test").unwrap(), "1");
        assert_eq!(h.get(HEADER_BRANCH).unwrap(), "main");
        assert_eq!(h.get(HEADER_REPO).unwrap(), "repo");
    }

    #[test]
    fn file_custom_status() {
        let val = json!({
            "kind": "file",
            "contentType": "text/plain",
            "bodyBase64": "QQ==",
            "status": 201
        });
        let r = get_hook_json_to_response(&val, "b", "r").unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn file_body_utf8() {
        let val = json!({
            "kind": "file",
            "contentType": "application/json",
            "bodyUtf8": r#"{"hello":"world"}"#
        });
        let r = get_hook_json_to_response(&val, "m", "r").unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), br#"{"hello":"world"}"#);
    }

    #[test]
    fn hook_cannot_override_relay_branch_header() {
        let val = json!({
            "kind": "file",
            "contentType": "text/plain",
            "bodyBase64": "QQ==",
            "headers": {
                "X-Relay-Branch": "evil",
                "X-Relay-Repo": "evil"
            }
        });
        let r = get_hook_json_to_response(&val, "real-branch", "real-repo").unwrap();
        let h = r.headers();
        assert_eq!(h.get(HEADER_BRANCH).unwrap(), "real-branch");
        assert_eq!(h.get(HEADER_REPO).unwrap(), "real-repo");
    }

    #[test]
    fn execution_error_returns_500_json_with_technical_and_cleverscript_detail() {
        let inner = RuntimeError::Runtime("bad line in hook".into());
        let err = anyhow::Error::new(inner);
        let r = get_hook_execution_error_response("my-branch", "my-repo", &err);
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let h = r.headers();
        assert_eq!(
            h.get(CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
        assert_eq!(h.get(HEADER_BRANCH).unwrap(), "my-branch");
        assert_eq!(h.get(HEADER_REPO).unwrap(), "my-repo");
    }

    #[tokio::test]
    async fn execution_error_body_parses_expected_keys() {
        let inner = RuntimeError::Runtime("bad line in hook".into());
        let err = anyhow::Error::new(inner);
        let r = get_hook_execution_error_response("main", "r", &err);
        let body = serde_json::from_slice::<serde_json::Value>(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"], "get_hook_execution_failed");
        assert!(body["summaryForReaders"].as_str().unwrap().len() > 20);
        assert!(body["technicalReport"]
            .as_str()
            .unwrap()
            .contains("bad line in hook"));
        assert_eq!(body["branch"], "main");
        assert_eq!(body["repo"], "r");
        let cs = body.get("cleverScript").expect("cleverScript");
        assert_eq!(cs["type"], "runtime");
        assert_eq!(cs["message"], "bad line in hook");
    }

    #[test]
    fn file_kind_hook_status_404_maps_to_http_not_found() {
        let val = json!({
            "kind": "file",
            "status": 404,
            "contentType": "application/json; charset=utf-8",
            "bodyUtf8": r#"{"ok":false,"path":"gone.png"}"#,
            "headers": {
                "Cache-Control": "public, max-age=60"
            }
        });
        let r = get_hook_json_to_response(&val, "main", "rp").unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            r.headers().get("cache-control").unwrap(),
            "public, max-age=60"
        );
    }

    #[test]
    fn file_kind_hook_status_float_404_maps_to_http_not_found() {
        let val = json!({
            "kind": "file",
            "status": 404.0,
            "contentType": "application/json; charset=utf-8",
            "bodyUtf8": r#"{"ok":false,"path":"gone.png"}"#
        });
        let r = get_hook_json_to_response(&val, "main", "rp").unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn file_kind_hook_status_fractional_stays_ok() {
        let val = json!({
            "kind": "file",
            "status": 404.5,
            "contentType": "text/plain",
            "bodyUtf8": "x"
        });
        let r = get_hook_json_to_response(&val, "main", "rp").unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[test]
    fn file_kind_hook_status_out_of_range_stays_ok() {
        let val = json!({
            "kind": "file",
            "status": 99.0,
            "contentType": "text/plain",
            "bodyUtf8": "x"
        });
        let r = get_hook_json_to_response(&val, "main", "rp").unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn execution_error_lists_unknown_module_in_technical_report() {
        let err = crate::git::clever_hooks::run_clever_hook(
            r#"import { z } from '@relay/nope';
export default function run(ctx, next) { return null; }"#,
            &serde_json::json!({}),
            std::path::PathBuf::from("/nonexistent/path/repo.git"),
            "main".into(),
            false,
        )
        .unwrap_err();

        let r = get_hook_execution_error_response("b1", "r1", &err);
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = serde_json::from_slice::<serde_json::Value>(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();

        assert_eq!(body["error"], "get_hook_execution_failed");
        let tech = body["technicalReport"].as_str().unwrap();
        assert!(
            tech.contains("unknown module") || tech.contains("@relay/nope"),
            "technicalReport={tech:?}"
        );
    }
}

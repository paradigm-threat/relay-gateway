//! Sanitized HTTP request headers for CleverScript GET hooks (`ctx.requestHeaders`).

use axum::http::{header::HeaderMap, HeaderName};
use serde_json::{Map, Value};

const SKIP_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
];

/// Build `{ "Accept": "...", ... }` excluding sensitive cookie/auth headers (deny-by-name).
pub fn request_headers_object(headers: &HeaderMap) -> Value {
    let mut m = Map::new();
    for (name, val) in headers.iter() {
        let name_lc = name.as_str().to_ascii_lowercase();
        if SKIP_HEADERS
            .iter()
            .any(|s| name_lc.eq_ignore_ascii_case(s))
        {
            continue;
        }
        if HeaderName::try_from(name.as_str()).is_err() {
            continue;
        }
        match val.to_str() {
            Ok(s) => {
                m.insert(name.as_str().to_string(), Value::String(s.to_string()));
            }
            Err(_) => {
                m.insert(
                    name.as_str().to_string(),
                    Value::String(format!("<binary {}>", val.len())),
                );
            }
        }
    }
    Value::Object(m)
}

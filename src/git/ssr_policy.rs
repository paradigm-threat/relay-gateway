//! When to skip or cheapen parked SSR (bots, HEAD, prefetch).
//!
//! The client hook always receives the request path. Rendering `/` for every URL
//! and marking it `data-relay-ssr` made hydration keep the homepage on article routes.
//! `RELAY_SSR_SHELL_ONLY` is ignored for the hook path (kept so old env files are harmless).
//!
//! Skip is header-driven (no env): `HEAD`, `Sec-Purpose: prefetch`, known crawler UAs.

use serde_json::Value;

/// Path passed into the client GET hook. Always the request path so article URLs
/// do not hydrate the homepage shell.
pub fn hook_initial_path(request_path: &str) -> String {
    if request_path.is_empty() || request_path == "/" {
        "/".into()
    } else if request_path.starts_with('/') {
        request_path.to_string()
    } else {
        format!("/{request_path}")
    }
}

pub fn should_skip_ssr(http_method: Option<&str>, request_headers: Option<&Value>) -> bool {
    if method_is_head(http_method) {
        return true;
    }
    let Some(Value::Object(h)) = request_headers else {
        return false;
    };
    if header_lookup(h, "x-relay-http-method")
        .map(|s| s.eq_ignore_ascii_case("HEAD"))
        .unwrap_or(false)
    {
        return true;
    }
    if header_is_prefetch(h) {
        return true;
    }
    if header_is_bot_ua(h) {
        return true;
    }
    false
}

pub fn should_skip_ssr_from_ctx(ctx: &std::collections::HashMap<String, relay_runtime::value::Value>) -> bool {
    let method = cs_str(ctx, "httpMethod").or_else(|| cs_str(ctx, "method"));
    let headers = ctx.get("requestHeaders");
    let json_headers = headers.and_then(cs_headers_to_json);
    should_skip_ssr(method.as_deref(), json_headers.as_ref())
}

fn method_is_head(m: Option<&str>) -> bool {
    m.map(|s| s.eq_ignore_ascii_case("HEAD")).unwrap_or(false)
}

fn header_lookup<'a>(
    h: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Option<&'a str> {
    h.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.as_str())
}

fn header_is_prefetch(h: &serde_json::Map<String, Value>) -> bool {
    for key in ["sec-purpose", "sec-fetch-purpose", "purpose"] {
        if let Some(v) = header_lookup(h, key) {
            if v.to_ascii_lowercase().contains("prefetch") {
                return true;
            }
        }
    }
    false
}

fn header_is_bot_ua(h: &serde_json::Map<String, Value>) -> bool {
    let Some(ua) = header_lookup(h, "user-agent") else {
        return false;
    };
    ua_looks_like_bot(ua)
}

/// UptimeRobot / search / SEO crawlers that should not occupy SSR slots.
pub fn ua_looks_like_bot(ua: &str) -> bool {
    let u = ua.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "uptimerobot",
        "googlebot",
        "bingbot",
        "applebot",
        "yandexbot",
        "duckduckbot",
        "baiduspider",
        "facebookexternalhit",
        "twitterbot",
        "linkedinbot",
        "slackbot",
        "discordbot",
        "whatsapp",
        "telegrambot",
        "semrush",
        "ahrefs",
        "mj12bot",
        "dotbot",
        "petalbot",
        "bytespider",
        "gptbot",
        "claudebot",
        "amazonbot",
        "barkrowler",
        "screaming frog",
        "siteaudit",
        "pingdom",
        "statuscake",
        "better uptime",
        "nodeping",
    ];
    NEEDLES.iter().any(|n| u.contains(n))
}

fn cs_str(ctx: &std::collections::HashMap<String, relay_runtime::value::Value>, key: &str) -> Option<String> {
    match ctx.get(key) {
        Some(relay_runtime::value::Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn cs_headers_to_json(v: &relay_runtime::value::Value) -> Option<Value> {
    match v {
        relay_runtime::value::Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                if let relay_runtime::value::Value::String(s) = val {
                    out.insert(k.clone(), Value::String(s.clone()));
                }
            }
            Some(Value::Object(out))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn head_and_prefetch_and_bots_skip() {
        assert!(should_skip_ssr(Some("HEAD"), None));
        assert!(should_skip_ssr(
            Some("GET"),
            Some(&json!({"Sec-Purpose": "prefetch"}))
        ));
        assert!(should_skip_ssr(
            Some("GET"),
            Some(&json!({"User-Agent": "Mozilla/5.0+(compatible; UptimeRobot/2.0)"}))
        ));
        assert!(should_skip_ssr(
            Some("GET"),
            Some(&json!({"user-agent": "Barkrowler/0.9"}))
        ));
        assert!(!should_skip_ssr(
            Some("GET"),
            Some(&json!({"User-Agent": "Mozilla/5.0 Chrome/120"}))
        ));
    }

    #[test]
    fn hook_path_follows_request_even_if_shell_only_set() {
        std::env::set_var("RELAY_SSR_SHELL_ONLY", "1");
        assert_eq!(
            hook_initial_path("/influence/predictive_programming/the-laugh-before-the-planet.md"),
            "/influence/predictive_programming/the-laugh-before-the-planet.md"
        );
        assert_eq!(hook_initial_path("/"), "/");
        assert_eq!(hook_initial_path(""), "/");
        std::env::remove_var("RELAY_SSR_SHELL_ONLY");
    }
}

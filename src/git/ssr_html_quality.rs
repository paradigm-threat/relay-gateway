//! Post-render HTML quality gate for SSR / bake (before LRU or fragment cache).
//!
//! Hard checks apply to every page. Optional site profiles (`.relay/ssr-qa.json` or
//! shared marker JSON) add required/forbidden substrings per route class.

use serde::Deserialize;
use std::path::Path;

use crate::git::embed_html::ssr_html_matches_request;

/// Hard-fail substrings (error UI / known CS panics).
pub const FORBIDDEN_HARD: &[&str] = &[
    "relay-error-boundary",
    "data-error-boundary",
    "Runtime error",
    "Property 'origin' not found",
    "CleverScript hook panicked",
    "ssrClientHookFromGetContext disabled",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityFailure {
    pub reason: String,
}

impl QualityFailure {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for QualityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

#[derive(Debug, Clone, Default)]
pub struct QualityOptions<'a> {
    /// Request path for `ssr_html_matches_request` (empty skips path check).
    pub request_path: Option<&'a str>,
    /// When true, require a full HTML document scaffold.
    pub expect_full_document: bool,
    /// Optional site profile (required/forbidden by route class).
    pub profile: Option<&'a SsrQaProfile>,
    /// Route class for profile matching: `home` | `article` | `chat` | `default`.
    pub route_class: &'a str,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SsrQaProfile {
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub routes: Vec<SsrQaRouteRule>,
    /// Always-forbidden (merged with hard list for reporting; hard still always applies).
    #[serde(default)]
    pub forbid_always: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SsrQaRouteRule {
    /// `home` | `article` | `chat` | `default` | `*`
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub require: Vec<String>,
    #[serde(default)]
    pub forbid: Vec<String>,
}

/// Infer a coarse route class from the request path.
pub fn infer_route_class(request_path: &str) -> &'static str {
    let p = request_path.trim().trim_matches('/');
    if p.is_empty() || p == "index" || p == "index.md" || p == "index.html" {
        return "home";
    }
    let lower = p.to_ascii_lowercase();
    if lower.contains("chat") || lower.contains("livechat") {
        return "chat";
    }
    "article"
}

/// Load `.relay/ssr-qa.json` from a content tree root (worktree or extracted tip).
pub fn load_profile_from_path(path: &Path) -> Option<SsrQaProfile> {
    let p = path.join(".relay/ssr-qa.json");
    let raw = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Parse profile JSON (shared marker file / bake helper).
pub fn parse_profile_json(raw: &str) -> Result<SsrQaProfile, String> {
    serde_json::from_str(raw).map_err(|e| format!("ssr-qa.json: {e}"))
}

/// Hard + optional profile checks. Returns `Ok(())` when HTML is publishable/cacheable.
pub fn check_ssr_html(html: &str, opts: QualityOptions<'_>) -> Result<(), QualityFailure> {
    if html.trim().is_empty() {
        return Err(QualityFailure::new("SSR HTML is empty"));
    }
    for needle in FORBIDDEN_HARD {
        if html.contains(needle) {
            return Err(QualityFailure::new(format!(
                "SSR HTML contains forbidden marker: {needle}"
            )));
        }
    }
    if let Some(path) = opts.request_path {
        if !path.is_empty() && !ssr_html_matches_request(html, path) {
            return Err(QualityFailure::new(format!(
                "SSR HTML path marker mismatch for request path {path}"
            )));
        }
    }
    if opts.expect_full_document {
        let lower = html.to_ascii_lowercase();
        if !lower.contains("<!doctype") && !lower.contains("<html") {
            return Err(QualityFailure::new(
                "SSR HTML missing document scaffold (<!DOCTYPE>/<html>)",
            ));
        }
        // Embed shell uses `#root`; park shells may use `#relay-root` / `<main>`.
        let has_root = lower.contains("<main")
            || html.contains("id=\"relay-root\"")
            || html.contains("id='relay-root'")
            || html.contains("id=\"root\"")
            || html.contains("id='root'");
        if !has_root {
            return Err(QualityFailure::new(
                "SSR HTML missing <main>, #relay-root, or #root",
            ));
        }
    }
    if let Some(profile) = opts.profile {
        for needle in &profile.forbid_always {
            if !needle.is_empty() && html.contains(needle.as_str()) {
                return Err(QualityFailure::new(format!(
                    "SSR HTML contains profile-forbidden marker: {needle}"
                )));
            }
        }
        let class = opts.route_class;
        for rule in &profile.routes {
            let matches = rule.class.is_empty()
                || rule.class == "*"
                || rule.class.eq_ignore_ascii_case(class)
                || rule.class.eq_ignore_ascii_case("default");
            if !matches {
                continue;
            }
            // Only apply `default` when class matches or rule is literally default for all;
            // skip default rules when a more specific rule exists for this class.
            if rule.class.eq_ignore_ascii_case("default")
                && class != "default"
                && profile
                    .routes
                    .iter()
                    .any(|r| r.class.eq_ignore_ascii_case(class))
            {
                continue;
            }
            for needle in &rule.forbid {
                if !needle.is_empty() && html.contains(needle.as_str()) {
                    return Err(QualityFailure::new(format!(
                        "SSR HTML contains forbidden ({class}): {needle}"
                    )));
                }
            }
            for needle in &rule.require {
                if !needle.is_empty() && !html.contains(needle.as_str()) {
                    return Err(QualityFailure::new(format!(
                        "SSR HTML missing required ({class}): {needle}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Convenience: hard checks for a body fragment (not a full document).
/// Path identity is checked on the composed document, not the body alone.
pub fn check_ssr_body_fragment(html: &str, request_path: &str) -> Result<(), QualityFailure> {
    check_ssr_html(
        html,
        QualityOptions {
            request_path: None,
            expect_full_document: false,
            profile: None,
            route_class: infer_route_class(request_path),
        },
    )
}

/// Full document (embed shell) before LRU put.
pub fn check_ssr_document(html: &str, request_path: &str) -> Result<(), QualityFailure> {
    check_ssr_html(
        html,
        QualityOptions {
            request_path: Some(request_path),
            expect_full_document: true,
            profile: None,
            route_class: infer_route_class(request_path),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_doc(path: &str) -> String {
        format!(
            r#"<!DOCTYPE html><html><body data-relay-ssr="1" data-relay-ssr-path="{path}">
<script type="application/json" id="relay-embed">{{"initialPath":"{path}"}}</script>
<main id="relay-root"><article data-pt-article>Hello</article></main>
</body></html>"#
        )
    }

    #[test]
    fn rejects_empty() {
        assert!(check_ssr_document("", "/").is_err());
    }

    #[test]
    fn rejects_error_boundary() {
        let html = good_doc("/").replace(
            "Hello",
            r#"<div class="relay-error-boundary">Runtime error</div>"#,
        );
        let err = check_ssr_document(&html, "/").unwrap_err();
        assert!(err.reason.contains("forbidden") || err.reason.contains("Runtime"));
    }

    #[test]
    fn rejects_path_mismatch() {
        let html = good_doc("/");
        assert!(check_ssr_document(&html, "/events/foo.md").is_err());
    }

    #[test]
    fn accepts_good_document() {
        let html = good_doc("/");
        assert!(check_ssr_document(&html, "/").is_ok());
    }

    #[test]
    fn profile_require_article() {
        let profile = SsrQaProfile {
            schema: None,
            routes: vec![SsrQaRouteRule {
                class: "article".into(),
                require: vec!["data-pt-article".into()],
                forbid: vec!["pt-live-chat-slot".into()],
            }],
            forbid_always: vec![],
        };
        let path = "/events/x.md";
        let mut html = good_doc(path);
        assert!(check_ssr_html(
            &html,
            QualityOptions {
                request_path: Some(path),
                expect_full_document: true,
                profile: Some(&profile),
                route_class: "article",
            }
        )
        .is_ok());
        html = html.replace("data-pt-article", "data-other");
        assert!(check_ssr_html(
            &html,
            QualityOptions {
                request_path: Some(path),
                expect_full_document: true,
                profile: Some(&profile),
                route_class: "article",
            }
        )
        .is_err());
    }

    #[test]
    fn infer_home_and_chat() {
        assert_eq!(infer_route_class("/"), "home");
        assert_eq!(infer_route_class("/ChatRoom.md"), "chat");
        assert_eq!(infer_route_class("/events/a.md"), "article");
    }
}

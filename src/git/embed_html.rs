//! Default **Relay embed** HTML shell when `server.hooks.get` returns raw markdown but the client
//! requested an embedded universal client (`Accept: text/html` or `?embed=1`).

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Replace `<` so arbitrary repo/path strings cannot break out of `<script type="application/json">`.
fn escape_json_for_html_script(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for ch in json.chars() {
        if ch == '<' {
            out.push_str("\\u003c");
        } else {
            out.push(ch);
        }
    }
    out
}

/// Minimal HTML entity escape for text nodes / `<title>` (untrusted repo slug).
fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape for double-quoted HTML attribute values.
fn escape_html_attr(s: &str) -> String {
    escape_html_text(s).replace('\'', "&#39;")
}

/// Strip `<script…>` / `</script>` from SSR body so a hostile hook cannot break the embed document.
fn sanitize_ssr_body_html(body: &str) -> String {
    let mut remaining = body;
    let mut out = String::with_capacity(body.len());
    loop {
        let lower = remaining.to_ascii_lowercase();
        let open = lower.find("<script");
        let orphan_close = lower.find("</script");
        let next = match (open, orphan_close) {
            (Some(o), Some(c)) => Some((o.min(c), o <= c)),
            (Some(o), None) => Some((o, true)),
            (None, Some(c)) => Some((c, false)),
            (None, None) => None,
        };
        let Some((idx, is_open)) = next else {
            out.push_str(remaining);
            break;
        };
        out.push_str(&remaining[..idx]);
        remaining = &remaining[idx..];
        let lower = remaining.to_ascii_lowercase();
        if is_open {
            if let Some(end_rel) = lower.find("</script>") {
                remaining = &remaining[end_rel + "</script>".len()..];
                continue;
            }
            // Unclosed script — drop the rest
            break;
        }
        // Orphan </script…>
        if let Some(gt) = remaining.find('>') {
            remaining = &remaining[gt + 1..];
            continue;
        }
        break;
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEmbedConfig<'a> {
    /// Bare repo name (same as `X-Relay-Repo`).
    pub repo: &'a str,
    /// Branch tip for this GET (`X-Relay-Branch`).
    pub branch: &'a str,
    /// Repo-relative path for the current document (leading slash, e.g. `/blog/x.md`).
    pub initial_path: String,
    /// URL path for the client GET hook (default `/hooks/client/get.jsx`).
    pub client_hook_path: &'a str,
}

/// Build the Relay embed document. `ssr_body_html=None` produces the existing empty CSR shell;
/// `Some` pre-fills `#root` for relay-runtime hydration.
///
/// `ssr_css` is optional generated theme CSS for no-JS browsers (media queries, themes).
pub fn embed_html_document(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
) -> String {
    embed_html_document_with_css(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        None,
    )
}

/// Like [`embed_html_document`], with optional SSR stylesheet.
///
/// `assets_url_prefix` is normally `"/assets"` (disk) or `"/assets/c/{40hex}"` (commit-pinned
/// web-assets repo). See `docs/RELAY_WEB_ASSETS.md`.
pub fn embed_html_document_with_css(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
    ssr_css: Option<&str>,
) -> String {
    embed_html_document_with_assets(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        ssr_css,
        None,
    )
}

/// Like [`embed_html_document_with_css`], with an explicit assets URL prefix.
fn document_title(repo: &str, site_title: Option<&str>) -> String {
    match site_title.map(str::trim).filter(|s| !s.is_empty()) {
        Some(title) => escape_html_text(title),
        None => format!("{} — Relay", escape_html_text(repo)),
    }
}

pub fn embed_html_document_with_assets(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
    ssr_css: Option<&str>,
    assets_url_prefix: Option<&str>,
) -> String {
    embed_html_document_with_assets_and_title(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        ssr_css,
        assets_url_prefix,
        None,
    )
}

/// Like [`embed_html_document_with_assets`], with optional catalog `name` for `<title>`.
pub fn embed_html_document_with_assets_and_title(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
    ssr_css: Option<&str>,
    assets_url_prefix: Option<&str>,
    site_title: Option<&str>,
) -> String {
    embed_html_document_with_assets_and_seo(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        ssr_css,
        assets_url_prefix,
        site_title,
        None,
        None,
    )
}

/// Optional Open Graph / Twitter image + article vs website typing.
#[derive(Debug, Clone, Default)]
pub struct EmbedSeoExtras<'a> {
    pub og_image: Option<&'a str>,
    /// When true, emit `og:type=article` instead of `website`.
    pub is_article: bool,
}

/// Like [`embed_html_document_with_assets_and_title`], with optional description + canonical.
pub fn embed_html_document_with_assets_and_seo(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
    ssr_css: Option<&str>,
    assets_url_prefix: Option<&str>,
    site_title: Option<&str>,
    site_description: Option<&str>,
    canonical_url: Option<&str>,
) -> String {
    embed_html_document_with_assets_and_seo_extras(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        ssr_css,
        assets_url_prefix,
        site_title,
        site_description,
        canonical_url,
        EmbedSeoExtras::default(),
    )
}

/// Like [`embed_html_document_with_assets_and_seo`], with og:image / twitter / article type.
pub fn embed_html_document_with_assets_and_seo_extras(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
    ssr_css: Option<&str>,
    assets_url_prefix: Option<&str>,
    site_title: Option<&str>,
    site_description: Option<&str>,
    canonical_url: Option<&str>,
    extras: EmbedSeoExtras<'_>,
) -> String {
    let initial_path = if rel_path.starts_with('/') {
        rel_path.to_string()
    } else {
        format!("/{}", rel_path.trim_start_matches('/'))
    };
    let cfg = RelayEmbedConfig {
        repo,
        branch,
        initial_path,
        client_hook_path,
    };
    let json = serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".to_string());
    let json = escape_json_for_html_script(&json);
    let title_repo = document_title(repo, site_title);
    let (root_attrs, root_inner) = match ssr_body_html {
        Some(body) if !body.trim().is_empty() => {
            (" data-relay-ssr=\"1\"", sanitize_ssr_body_html(body))
        }
        _ => ("", String::new()),
    };
    let theme_style = match ssr_css {
        Some(css) if !css.trim().is_empty() => {
            // Strip </style> so hostile theme CSS cannot break out of the style element.
            let safe = css
                .replace("</style", "<\\/style")
                .replace("</STYLE", "<\\/STYLE");
            format!("\n  <style id=\"relay-ssr-theme\">{}</style>", safe)
        }
        _ => String::new(),
    };
    let assets = assets_url_prefix
        .map(|s| s.trim_end_matches('/'))
        .filter(|s| !s.is_empty())
        .unwrap_or("/assets");
    let mut seo_tags = String::new();
    if let Some(desc) = site_description.map(str::trim).filter(|s| !s.is_empty()) {
        seo_tags.push_str(&format!(
            "\n  <meta name=\"description\" content=\"{}\"/>",
            escape_html_attr(desc)
        ));
        seo_tags.push_str(&format!(
            "\n  <meta property=\"og:description\" content=\"{}\"/>",
            escape_html_attr(desc)
        ));
    }
    if let Some(title) = site_title.map(str::trim).filter(|s| !s.is_empty()) {
        seo_tags.push_str(&format!(
            "\n  <meta property=\"og:title\" content=\"{}\"/>",
            escape_html_attr(title)
        ));
        seo_tags.push_str(&format!(
            "\n  <meta name=\"twitter:title\" content=\"{}\"/>",
            escape_html_attr(title)
        ));
    }
    if let Some(desc) = site_description.map(str::trim).filter(|s| !s.is_empty()) {
        seo_tags.push_str(&format!(
            "\n  <meta name=\"twitter:description\" content=\"{}\"/>",
            escape_html_attr(desc)
        ));
    }
    if let Some(canon) = canonical_url.map(str::trim).filter(|s| !s.is_empty()) {
        let escaped = escape_html_attr(canon);
        seo_tags.push_str(&format!("\n  <link rel=\"canonical\" href=\"{escaped}\"/>"));
        seo_tags.push_str(&format!(
            "\n  <meta property=\"og:url\" content=\"{escaped}\"/>"
        ));
    }
    let og_type = if extras.is_article {
        "article"
    } else {
        "website"
    };
    seo_tags.push_str(&format!(
        "\n  <meta property=\"og:type\" content=\"{og_type}\"/>"
    ));
    if let Some(img) = extras.og_image.map(str::trim).filter(|s| !s.is_empty()) {
        let escaped = escape_html_attr(img);
        seo_tags.push_str(&format!(
            "\n  <meta property=\"og:image\" content=\"{escaped}\"/>"
        ));
        seo_tags.push_str(&format!(
            "\n  <meta name=\"twitter:image\" content=\"{escaped}\"/>"
        ));
        seo_tags.push_str("\n  <meta name=\"twitter:card\" content=\"summary_large_image\"/>");
    } else {
        seo_tags.push_str("\n  <meta name=\"twitter:card\" content=\"summary\"/>");
    }
    // data-pt-hydrated="0" until embed JS promotes live (relay-runtime-web syncHtmlHydratedAttr).
    // Park tips style .pt-client-only chrome as inactive until "1".
    format!(
        r#"<!DOCTYPE html>
<html lang="en" data-pt-hydrated="0">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <meta name="color-scheme" content="light dark"/>
  <title>{title_repo}</title>{seo_tags}
  <link rel="stylesheet" href="{assets}/embed.css"/>
  <style>html,body,#root{{margin:0;height:100%;}}</style>{theme_style}
</head>
<body>
  <div id="root"{root_attrs}>{root_inner}</div>
  <script type="application/json" id="relay-embed-config">{json}</script>
  <script type="module" src="{assets}/embed.js"></script>
</body>
</html>"#,
    )
}

/// Inject tip portrait-sibling map for PopImage (before embed.js).
///
/// Sets `globalThis.__RELAY_PORTRAIT_SIBLINGS__` to a `{ "path/to/foo-portrait.png": true }` map.
pub fn inject_portrait_sibling_bootstrap(html: &str, portraits_json: &str) -> String {
    let safe = escape_json_for_html_script(portraits_json);
    let boot = format!(
        r#"  <script type="application/json" id="relay-portrait-siblings">{safe}</script>
  <script>try{{var __el=document.getElementById('relay-portrait-siblings');if(__el){{globalThis.__RELAY_PORTRAIT_SIBLINGS__=JSON.parse(__el.textContent||'{{}}');}}}}catch(e){{globalThis.__RELAY_PORTRAIT_SIBLINGS__={{}};}}</script>
"#
    );
    if let Some(idx) = html.find(r#"<script type="module" src=""#) {
        let mut out = String::with_capacity(html.len() + boot.len());
        out.push_str(&html[..idx]);
        out.push_str(&boot);
        out.push_str(&html[idx..]);
        out
    } else if let Some(idx) = html.find("</body>") {
        let mut out = String::with_capacity(html.len() + boot.len());
        out.push_str(&html[..idx]);
        out.push_str(&boot);
        out.push_str(&html[idx..]);
        out
    } else {
        format!("{html}{boot}")
    }
}

/// Build a minimal HTML document that boots **`/assets/embed.js`** and passes config via JSON
/// in `#relay-embed-config`.
///
/// When `ssr_body_html` is `Some`, `#root` is pre-filled with the client-hook markup and marked
/// `data-relay-ssr="1"` so the embed client hydrates instead of mounting an empty CSR shell.
pub fn fallback_embed_html_response(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
) -> Response {
    fallback_embed_html_response_with_ssr(repo, branch, rel_path, client_hook_path, None)
}

/// Like [`fallback_embed_html_response`], with optional SSR body HTML for `#root`.
pub fn fallback_embed_html_response_with_ssr(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
) -> Response {
    fallback_embed_html_response_with_ssr_and_assets(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        None,
    )
}

/// Like [`fallback_embed_html_response_with_ssr`], with commit-pinned assets prefix.
pub fn fallback_embed_html_response_with_ssr_and_assets(
    repo: &str,
    branch: &str,
    rel_path: &str,
    client_hook_path: &str,
    ssr_body_html: Option<&str>,
    assets_url_prefix: Option<&str>,
) -> Response {
    use crate::types::{HEADER_BRANCH, HEADER_REPO};
    let html = embed_html_document_with_assets(
        repo,
        branch,
        rel_path,
        client_hook_path,
        ssr_body_html,
        None,
        assets_url_prefix,
    );

    let mut resp = html.into_response();
    *resp.status_mut() = StatusCode::OK;
    {
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        h.insert(header::VARY, HeaderValue::from_static("Accept"));
        if let Ok(v) = HeaderValue::from_str(branch) {
            h.insert(HEADER_BRANCH, v);
        }
        if let Ok(v) = HeaderValue::from_str(repo) {
            h.insert(HEADER_REPO, v);
        }
    }
    resp
}

/// Merge **`Vary: Accept`** onto an existing GET-hook response when serving negotiated content.
pub fn merge_vary_accept(mut resp: Response) -> Response {
    use axum::http::header::VARY;
    if !resp.headers().contains_key(VARY) {
        resp.headers_mut()
            .insert(VARY, HeaderValue::from_static("Accept"));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fallback_embed_json_escapes_markup_for_script_tag() {
        let r = fallback_embed_html_response(
            "x</script><script>alert(1)</script>",
            "main",
            "a.md",
            "/hooks/client/get.jsx",
        );
        let body = r.into_body();
        let b = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let s = String::from_utf8(b.to_vec()).unwrap();
        assert!(
            s.contains(
                "<title>x&lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt; — Relay</title>"
            ),
            "title must HTML-escape repo slug"
        );
        let needle = r#"id="relay-embed-config">"#;
        let open = s.find(needle).expect("embed config script") + needle.len();
        let rest = &s[open..];
        let close_rel = rest
            .find("</script>")
            .expect("closing script after embed JSON");
        let json_body = &rest[..close_rel];
        assert!(
            !json_body.contains("</script>") && !json_body.contains("<script"),
            "JSON inside script must not contain raw markup breakouts: {json_body:?}"
        );
    }

    #[test]
    fn fallback_embed_contains_assets() {
        let r = fallback_embed_html_response("r", "main", "a/b.md", "/hooks/client/get.jsx");
        assert_eq!(r.status(), StatusCode::OK);
        let ct = r
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"));
    }

    #[tokio::test]
    async fn embed_with_ssr_body_marks_root() {
        let r = fallback_embed_html_response_with_ssr(
            "r",
            "main",
            "/doc.md",
            "/hooks/client/get.jsx",
            Some(r#"<div class="ssr">Hi</div>"#),
        );
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let s = String::from_utf8(body.to_vec()).unwrap();
        assert!(s.contains(r#"id="root" data-relay-ssr="1""#), "{s}");
        assert!(s.contains(r#"class="ssr">Hi</div>"#), "{s}");
    }

    #[tokio::test]
    async fn embed_ssr_strips_script_tags() {
        let r = fallback_embed_html_response_with_ssr(
            "r",
            "main",
            "/doc.md",
            "/hooks/client/get.jsx",
            Some(r#"<div>ok</div><script>alert(1)</script><p>tail</p>"#),
        );
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let s = String::from_utf8(body.to_vec()).unwrap();
        assert!(s.contains("<div>ok</div>"), "{s}");
        assert!(s.contains("<p>tail</p>"), "{s}");
        assert!(!s.contains("alert(1)"), "script body must be stripped: {s}");
    }

    #[test]
    fn sanitize_ssr_removes_script() {
        let s = sanitize_ssr_body_html(r#"a<script src=x></script>b</script>c"#);
        assert_eq!(s, "abc");
    }

    #[test]
    fn inject_portrait_sibling_bootstrap_before_embed_js() {
        let base = embed_html_document("demo", "main", "/", "/hooks/client/get.jsx", None);
        let html =
            inject_portrait_sibling_bootstrap(&base, r#"{"governance/war/x-portrait.png":true}"#);
        assert!(html.contains("id=\"relay-portrait-siblings\""), "{html}");
        assert!(
            html.contains(r#"{"governance/war/x-portrait.png":true}"#)
                || html.contains("governance/war/x-portrait.png"),
            "{html}"
        );
        assert!(html.contains("__RELAY_PORTRAIT_SIBLINGS__"), "{html}");
        let portrait_idx = html.find("relay-portrait-siblings").unwrap();
        let embed_idx = html.find(r#"src="/assets/embed.js""#).unwrap();
        assert!(
            portrait_idx < embed_idx,
            "portrait bootstrap must precede embed.js"
        );
    }

    #[test]
    fn inject_portrait_escapes_script_breakout() {
        let base = embed_html_document("demo", "main", "/", "/hooks/client/get.jsx", None);
        let html = inject_portrait_sibling_bootstrap(&base, r#"{"":"</script><script>alert(1)"}"#);
        assert!(!html.contains("</script><script>alert"), "{html}");
    }

    #[test]
    fn embed_with_ssr_css_injects_theme_style() {
        let html = embed_html_document_with_css(
            "demo",
            "main",
            "/",
            "/hooks/client/get.jsx",
            Some("<main id=\"x\">hi</main>"),
            Some(".ct-page{background-color:#020817;}"),
        );
        assert!(html.contains("id=\"relay-ssr-theme\""));
        assert!(html.contains(".ct-page{background-color:#020817;}"));
        assert!(html.contains("data-relay-ssr=\"1\""));
        assert!(
            html.contains(r#"<html lang="en" data-pt-hydrated="0">"#),
            "SSR shell must mark html inactive until client live: {html}"
        );
    }

    #[test]
    fn embed_seo_tags_escape_and_include_canonical() {
        let html = embed_html_document_with_assets_and_seo(
            "r",
            "main",
            "/",
            "/hooks/client/get.jsx",
            None,
            None,
            None,
            Some("CleverTree"),
            Some("Portfolio \"site\" & more"),
            Some("https://clevertree.net/"),
        );
        assert!(html.contains("<title>CleverTree</title>"), "{html}");
        assert!(
            html.contains(
                r#"<meta name="description" content="Portfolio &quot;site&quot; &amp; more"/>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<link rel="canonical" href="https://clevertree.net/"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<meta property="og:url" content="https://clevertree.net/"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<meta name="twitter:card" content="summary"/>"#),
            "{html}"
        );
    }

    #[test]
    fn embed_seo_extras_include_og_image_and_article_type() {
        let html = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/a.md",
            "/hooks/client/get.jsx",
            None,
            None,
            None,
            Some("Title | Site"),
            Some("Desc"),
            Some("https://paradigmthreat.net/a"),
            EmbedSeoExtras {
                og_image: Some("https://paradigmthreat.net/a.png?w=800"),
                is_article: true,
            },
        );
        assert!(
            html.contains(r#"property="og:type" content="article""#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"property="og:image" content="https://paradigmthreat.net/a.png?w=800""#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"name="twitter:card" content="summary_large_image""#),
            "{html}"
        );
    }
}

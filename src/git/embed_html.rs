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
    /// Soft-missing SPA shell: tell crawlers not to index this URL.
    pub noindex: bool,
    /// Site name for JSON-LD `isPartOf` / `WebSite` (falls back to document title).
    pub site_name: Option<&'a str>,
    /// When true (typically `/`), emit `WebSite` + `SearchAction` JSON-LD instead of `Article`.
    pub is_website_home: bool,
    /// `#root` holds a pre-hydrate notice only — do **not** set `data-relay-ssr` so embed.js
    /// CSR-mounts the client hook and replaces the notice.
    pub csr_placeholder: bool,
    /// Tip `.relay/client.json` `hydrate: false` + real SSR: omit CleverScript `embed.js`.
    /// Ignored when [`Self::csr_placeholder`] is true (timeout/busy shells still need CSR).
    pub omit_embed_js: bool,
}

/// True when this document is the SSR/CSR shell for `request_path`.
///
/// A cached homepage (or any other URL) must not be returned for an article: the client
/// keeps `data-relay-ssr` markup and will not load that path's markdown.
/// Real SSR documents must carry `data-relay-ssr-path` equal to the embed `initialPath`.
pub fn ssr_html_matches_request(html: &str, request_path: &str) -> bool {
    let want = crate::git::ssr_policy::normalize_request_path(request_path);
    let Some(got) = embed_initial_path(html) else {
        return false;
    };
    if got != want {
        return false;
    }
    if html.contains("data-relay-ssr=\"1\"") {
        let marker = format!("data-relay-ssr-path=\"{}\"", escape_html_attr(&want));
        if !html.contains(&marker) {
            return false;
        }
    }
    true
}

fn embed_initial_path(html: &str) -> Option<String> {
    let key = "\"initialPath\":\"";
    let start = html.find(key)? + key.len();
    let rest = html.get(start..)?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Visible `#root` body when SSR did not finish. Embed.js replaces this on CSR mount.
pub fn ssr_timeout_notice_html() -> &'static str {
    r#"<div id="relay-ssr-fallback" data-relay-ssr-fallback="timeout" role="status"><p><strong>This page is still loading.</strong> Server-side render timed out; the live page should appear in a moment.</p></div>"#
}

/// Same shape when SSR slots are full (not a wall-clock timeout).
pub fn ssr_busy_notice_html() -> &'static str {
    r#"<div id="relay-ssr-fallback" data-relay-ssr-fallback="busy" role="status"><p><strong>This page is still loading.</strong> The server is busy pre-rendering; the live page should appear in a moment.</p></div>"#
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
    let ssr_path_attr = escape_html_attr(&initial_path);
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
            if extras.csr_placeholder {
                (String::new(), sanitize_ssr_body_html(body))
            } else {
                (
                    format!(" data-relay-ssr=\"1\" data-relay-ssr-path=\"{ssr_path_attr}\""),
                    sanitize_ssr_body_html(body),
                )
            }
        }
        _ => (String::new(), String::new()),
    };
    let placeholder_style = if extras.csr_placeholder {
        r#"#relay-ssr-fallback{box-sizing:border-box;font:16px/1.45 system-ui,sans-serif;max-width:40rem;margin:2rem auto;padding:1rem 1.25rem;border:1px solid #8884;border-radius:8px;background:#fff8e6;color:#1a1a1a}#relay-ssr-fallback p{margin:0}@media (prefers-color-scheme:dark){#relay-ssr-fallback{background:#3d3518;color:#f4efe4;border-color:#c4a035}}"#
    } else {
        ""
    };
    let theme_style = match ssr_css {
        Some(css) if !css.trim().is_empty() => {
            // Strip </style> so hostile theme CSS cannot break out of the style element.
            let safe = css
                .replace("</style", "<\\/style")
                .replace("</STYLE", "<\\/STYLE");
            // Flashback: themed-styler drops nested @media from theme.yaml — tip CSS
            // supplies mobile/tablet/desktop/wide theater-bg covers.
            let theater_bg = if css.contains("theater-bg-desktop.jpg") {
                "\n  <link rel=\"stylesheet\" id=\"fb-theater-bg-css\" href=\"/hooks/client/brand/theater-bg.css\"/>"
            } else {
                ""
            };
            format!("\n  <style id=\"relay-ssr-theme\">{}</style>{}", safe, theater_bg)
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
    if extras.noindex {
        seo_tags.push_str("\n  <meta name=\"robots\" content=\"noindex,follow\"/>");
    }
    if let Some(ld) = build_json_ld(
        site_title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(repo),
        site_description,
        canonical_url,
        &extras,
    ) {
        seo_tags.push_str("\n  <script type=\"application/ld+json\">");
        seo_tags.push_str(&ld);
        seo_tags.push_str("</script>");
    }
    // data-pt-hydrated="0" until embed JS promotes live (relay-runtime-web syncHtmlHydratedAttr).
    // Park tips style .pt-client-only chrome as inactive until "1".
    // SSR-static (omit_embed_js): mark hydrated so park CSS does not wait on CleverScript.
    let omit_embed = extras.omit_embed_js && !extras.csr_placeholder;
    let hydrated_attr = if omit_embed { "1" } else { "0" };
    let embed_scripts = if omit_embed {
        format!(
            r#"  <script type="application/json" id="relay-embed-config">{json}</script>
"#
        )
    } else {
        format!(
            r#"  <script type="application/json" id="relay-embed-config">{json}</script>
  <script type="module" src="{assets}/embed.js"></script>
"#
        )
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en" data-pt-hydrated="{hydrated_attr}">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <meta name="color-scheme" content="light dark"/>
  <title>{title_repo}</title>{seo_tags}
  <link rel="stylesheet" href="{assets}/embed.css"/>
  <style>html,body,#root{{margin:0;height:100%;}}{placeholder_style}</style>{theme_style}
</head>
<body>
  <div id="root"{root_attrs}>{root_inner}</div>
{embed_scripts}</body>
</html>"#,
    )
}

/// Tip `.relay/client.json`: `{"hydrate": false}` disables CleverScript embed on successful SSR.
/// Missing / invalid file → hydrate enabled (legacy default).
pub fn tip_wants_client_hydrate(repo_root: &std::path::Path, branch: &str) -> bool {
    let repo_path = repo_root.to_path_buf();
    let Ok(bytes) =
        crate::git::repo::read_file_from_repo(&repo_path, branch, ".relay/client.json")
    else {
        return true;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return true;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return true;
    };
    match v.get("hydrate") {
        Some(serde_json::Value::Bool(false)) => false,
        Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("false") => false,
        _ => true,
    }
}

/// Remove CleverScript `embed.js` module tags from an HTML document (cached SSR + tip static mode).
pub fn strip_embed_js_scripts(html: &str) -> String {
    let mut out = html.to_string();
    // <script type="module" src="…/embed.js"></script> (optional attributes / query)
    let mut start = 0;
    while let Some(rel) = out[start..].find("embed.js") {
        let abs = start + rel;
        let Some(tag_start) = out[..abs].rfind("<script") else {
            start = abs + 8;
            continue;
        };
        let Some(close_rel) = out[abs..].find("</script>") else {
            break;
        };
        let tag_end = abs + close_rel + "</script>".len();
        let tag = &out[tag_start..tag_end];
        if tag.contains("type=\"module\"") || tag.contains("type='module'") {
            out.replace_range(tag_start..tag_end, "");
            start = tag_start;
        } else {
            start = tag_end;
        }
    }
    out = out.replacen(
        r#"data-pt-hydrated="0""#,
        r#"data-pt-hydrated="1""#,
        1,
    );
    out
}

/// Apply tip SSR-static policy: strip embed.js when hydrate is off and `#root` is real SSR.
pub fn apply_ssr_static_policy(html: &str, repo_root: &std::path::Path, branch: &str) -> String {
    if tip_wants_client_hydrate(repo_root, branch) {
        return html.to_string();
    }
    if html.contains("data-relay-ssr-fallback") {
        return html.to_string();
    }
    if !html.contains("data-relay-ssr=\"1\"") {
        return html.to_string();
    }
    strip_embed_js_scripts(html)
}

/// Compact JSON-LD for crawlers (`Article` or home `WebSite` + `SearchAction`).
fn build_json_ld(
    title: &str,
    description: Option<&str>,
    canonical_url: Option<&str>,
    extras: &EmbedSeoExtras<'_>,
) -> Option<String> {
    if extras.noindex {
        return None;
    }
    let canon = canonical_url.map(str::trim).filter(|s| !s.is_empty())?;
    let site = extras
        .site_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(title);
    let origin = origin_from_canonical(canon).unwrap_or_else(|| canon.to_string());
    let mut obj = serde_json::Map::new();
    obj.insert(
        "@context".into(),
        serde_json::Value::String("https://schema.org".into()),
    );
    let home_like = extras.is_website_home
        || (!extras.is_article && canon.trim_end_matches('/') == origin.trim_end_matches('/'));
    if home_like {
        obj.insert("@type".into(), serde_json::Value::String("WebSite".into()));
        obj.insert("name".into(), serde_json::Value::String(site.to_string()));
        obj.insert("url".into(), serde_json::Value::String(origin.clone()));
        if let Some(d) = description.map(str::trim).filter(|s| !s.is_empty()) {
            obj.insert(
                "description".into(),
                serde_json::Value::String(d.to_string()),
            );
        }
        let search_target = format!(
            "{}/search?q={{search_term_string}}",
            origin.trim_end_matches('/')
        );
        obj.insert(
            "potentialAction".into(),
            serde_json::json!({
                "@type": "SearchAction",
                "target": search_target,
                "query-input": "required name=search_term_string"
            }),
        );
    } else if extras.is_article {
        obj.insert("@type".into(), serde_json::Value::String("Article".into()));
        obj.insert(
            "headline".into(),
            serde_json::Value::String(title.to_string()),
        );
        if let Some(d) = description.map(str::trim).filter(|s| !s.is_empty()) {
            obj.insert(
                "description".into(),
                serde_json::Value::String(d.to_string()),
            );
        }
        obj.insert("url".into(), serde_json::Value::String(canon.to_string()));
        if let Some(img) = extras.og_image.map(str::trim).filter(|s| !s.is_empty()) {
            obj.insert("image".into(), serde_json::Value::String(img.to_string()));
        }
        obj.insert(
            "isPartOf".into(),
            serde_json::json!({
                "@type": "WebSite",
                "name": site,
                "url": origin
            }),
        );
    } else {
        return None;
    }
    let raw = serde_json::to_string(&serde_json::Value::Object(obj)).ok()?;
    // Prevent `</script>` breakout in hostile titles/descriptions.
    Some(
        raw.replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026"),
    )
}

fn origin_from_canonical(canon: &str) -> Option<String> {
    let s = canon.trim();
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))?;
    let scheme = if s.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    let host_port = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host_port}"))
}

/// Strip `class` / `style` from `<relay-image-gallery>` opening tags.
///
/// Park themes sometimes put fullscreen overlay classes (e.g. `.pt-gallery`) on the CE
/// host. Before the custom element upgrades, that paints a blank black sheet over the page.
fn strip_relay_image_gallery_host_attrs(html: &str) -> String {
    const OPEN: &str = "<relay-image-gallery";
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let tail = &html[i..];
        let tail_lower = tail.to_ascii_lowercase();
        let Some(rel) = tail_lower.find(OPEN) else {
            out.push_str(tail);
            break;
        };
        out.push_str(&tail[..rel]);
        let after = rel + OPEN.len();
        let after_tail = &tail[after..];
        let Some(gt) = after_tail.find('>') else {
            out.push_str(&tail[rel..]);
            break;
        };
        let attrs = &after_tail[..gt];
        let self_close = attrs.trim_end().ends_with('/');
        out.push_str("<relay-image-gallery");
        if self_close {
            out.push_str(" />");
        } else {
            out.push('>');
        }
        i += after + gt + 1;
    }
    out
}

/// Guard against fullscreen theme classes on unresolved `<relay-image-gallery>` hosts.
///
/// Strips host `class`/`style` and injects a tiny CSS shield so empty/undefined hosts
/// cannot cover the page before `@relay/gallery` upgrades the CE.
pub fn guard_relay_image_gallery_hosts(html: &str) -> String {
    if !html.to_ascii_lowercase().contains("relay-image-gallery") {
        return html.to_string();
    }
    let mut out = strip_relay_image_gallery_host_attrs(html);
    if out.contains("id=\"relay-gallery-host-guard\"")
        || out.contains("id='relay-gallery-host-guard'")
    {
        return out;
    }
    const STYLE: &str = concat!(
        r#"<style id="relay-gallery-host-guard">"#,
        "relay-image-gallery:not(:defined),relay-image-gallery:empty{display:none!important}",
        "relay-image-gallery.pt-gallery{",
        "position:static!important;inset:auto!important;background:transparent!important;",
        "z-index:auto!important;display:contents!important}",
        "relay-image-gallery:defined{display:contents!important}",
        "</style>",
    );
    if let Some(idx) = out.find("</head>") {
        out.insert_str(idx, STYLE);
    } else if let Some(idx) = out.to_ascii_lowercase().find("<body") {
        out.insert_str(idx, STYLE);
    } else {
        out.insert_str(0, STYLE);
    }
    out
}

/// Inject park-native host scripts before the embed module (inside `<body>`).
///
/// CleverScript GET hooks may fail to place these correctly; gateway normalizes any HTML
/// response that already contains `data-relay-park-native` placeholders **or** a CSR
/// `#relay-ssr-fallback` shell (timeout/busy) so theater/board hosts still load before
/// client hydrate paints placeholders.
pub fn inject_park_native_host_scripts(html: &str) -> String {
    inject_park_native_host_scripts_with_assets(html, None)
}

/// Pull `/assets/c/{40hex}` from an already-stamped HTML document (embed script src).
pub fn infer_assets_prefix_from_html(html: &str) -> Option<String> {
    let marker = "/assets/c/";
    let mut start = 0;
    while let Some(rel) = html[start..].find(marker) {
        let i = start + rel + marker.len();
        if let Some(oid) = html.get(i..i + 40) {
            if oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Some(format!("{marker}{oid}"));
            }
        }
        start = i;
        if start >= html.len() {
            break;
        }
    }
    None
}

/// Like [`inject_park_native_host_scripts`], but chat hosts prefer commit-pinned
/// `relay-web-assets` URLs when `assets_prefix` is `Some("/assets/c/{oid}")`.
pub fn inject_park_native_host_scripts_with_assets(
    html: &str,
    assets_prefix: Option<&str>,
) -> String {
    const MARKER: &str = "data-fb-park-native=\"1\"";
    const PLACEHOLDER: &str = "data-relay-park-native";
    const CSR_FALLBACK: &str = "data-relay-ssr-fallback";
    // Bump when always-injected host list changes (cache-bust + skip re-inject).
    const CURRENT: &str = "LobbyAudio.host.js?v=44";
    if !html.contains(PLACEHOLDER)
        && !html.contains(CSR_FALLBACK)
        && !html.contains("data-fb-pop-image")
    {
        return guard_relay_image_gallery_hosts(html);
    }
    let body_close = html.rfind("</body>");
    if let Some(first) = html.find(MARKER) {
        if body_close.is_none_or(|bc| first < bc) && html.contains(CURRENT) {
            return guard_relay_image_gallery_hosts(html);
        }
    }
    let mut cleaned = html.to_string();
    if let Some(doc_end) = cleaned.rfind("</html>") {
        let tail = &cleaned[doc_end + 7..];
        if tail.contains(MARKER) {
            cleaned.truncate(doc_end + 7);
        }
    }
    let host_open = "<script data-fb-park-native=\"1\"";
    loop {
        let Some(hi) = cleaned.find(host_open) else {
            break;
        };
        let Some(he) = cleaned[hi..].find("</script>") else {
            break;
        };
        cleaned.replace_range(hi..hi + he + 9, "");
    }
    // TheaterPlayer host omitted — ExternalPlay / VLC only (avoids tab crashes from HTML5/WASM).
    // Inject only hosts for placeholders present; CSR timeout shells get a recovery set (no Board).
    let csr_fallback = cleaned.contains(CSR_FALLBACK);
    let want_affirmation =
        csr_fallback || cleaned.contains("data-relay-park-native=\"fb-affirmation\"");
    let want_external =
        csr_fallback || cleaned.contains("data-relay-park-native=\"fb-external-play\"");
    let want_now_showing =
        csr_fallback || cleaned.contains("data-relay-park-native=\"fb-now-showing\"");
    let want_board = cleaned.contains("data-relay-park-native=\"fb-board\"");
    // CSR timeout shells mount LiveChat/ChatApp from get.jsx after hydrate — inject hosts.
    let want_live_chat =
        csr_fallback || cleaned.contains("data-relay-park-native=\"fb-live-chat\"");
    let want_chat_app = cleaned.contains("data-relay-park-native=\"fb-chat-app\"")
        || (csr_fallback
            && (cleaned.contains("\"initialPath\":\"/chat\"")
                || cleaned.contains("\"initialPath\":\"/chat/")));
    let want_pop_image = cleaned.contains("data-fb-pop-image");
    // v=35: park-host-common pointer-events + Affirmation inert/session-dismiss fix.
    let _assets_prefix = assets_prefix;
    let chat_common = "/hooks/client/native/ChatCommon.host.js?v=32".to_string();
    let chat_live = "/hooks/client/native/LiveChat.host.js?v=32".to_string();
    let chat_app = "/hooks/client/native/ChatApp.host.js?v=32".to_string();
    let mut snip = String::from(
        "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/park-host-common.js?v=45\"></script>\
<script data-fb-park-native=\"1\" src=\"/hooks/client/native/telemetry.js?v=32\"></script>\
<script data-fb-park-native=\"1\" src=\"/hooks/client/native/Hits.host.js?v=32\"></script>\
<script data-fb-park-native=\"1\" src=\"/hooks/client/lobby/LobbyPlaylist.js?v=44\"></script>\
<script data-fb-park-native=\"1\" src=\"/hooks/client/native/LobbyAudio.host.js?v=44\"></script>",
    );
    if want_pop_image {
        snip.push_str(
            "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/PopImage.host.js?v=32\"></script>",
        );
    }
    if want_affirmation {
        snip.push_str(
            "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/Affirmation.host.js?v=38\"></script>",
        );
    }
    if want_external {
        snip.push_str(
            "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/ExternalPlay.host.js?v=32\"></script>",
        );
    }
    if want_now_showing {
        snip.push_str(
            "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/NowShowing.host.js?v=39\"></script>",
        );
    }
    if want_board {
        snip.push_str(
            "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/Board.host.js?v=32\"></script>",
        );
    }
    if want_live_chat || want_chat_app {
        snip.push_str(&format!(
            "<script data-fb-park-native=\"1\" src=\"{chat_common}\"></script>"
        ));
    }
    if want_live_chat {
        snip.push_str(&format!(
            "<script data-fb-park-native=\"1\" src=\"{chat_live}\"></script>"
        ));
    }
    if want_chat_app {
        snip.push_str(&format!(
            "<script data-fb-park-native=\"1\" src=\"{chat_app}\"></script>"
        ));
    }
    let injected = if let Some(idx) = cleaned.rfind(r#"<script type="module" src=""#) {
        let mut out = String::with_capacity(cleaned.len() + snip.len());
        out.push_str(&cleaned[..idx]);
        out.push_str(&snip);
        out.push_str(&cleaned[idx..]);
        out
    } else if let Some(idx) = cleaned.rfind("</body>") {
        let mut out = String::with_capacity(cleaned.len() + snip.len());
        out.push_str(&cleaned[..idx]);
        out.push_str(&snip);
        out.push_str(&cleaned[idx..]);
        out
    } else {
        format!("{cleaned}{snip}")
    };
    guard_relay_image_gallery_hosts(&injected)
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
    fn inject_park_native_host_scripts_before_embed() {
        let sample = concat!(
            "<html><body><div data-relay-park-native=\"fb-affirmation\"></div>",
            "<script type=\"application/json\" id=\"relay-embed-config\">{}</script>",
            "<script type=\"module\" src=\"/assets/c/abc/embed.js\"></script></body></html>",
            "<script data-fb-park-native=\"1\" src=\"/hooks/client/native/Affirmation.host.js\"></script>"
        );
        let out = inject_park_native_host_scripts(sample);
        let body_close = out.find("</body>").unwrap();
        let first_host = out.find("data-fb-park-native=\"1\"").unwrap();
        assert!(first_host < body_close, "hosts must be inside body: {out}");
        assert!(out.contains("park-host-common.js"));
        assert!(!out.contains("</html><script data-fb-park-native"));
    }

    #[test]
    fn inject_park_native_host_scripts_on_csr_timeout_shell() {
        let sample = concat!(
            r#"<html><body><div id="root">"#,
            r#"<div id="relay-ssr-fallback" data-relay-ssr-fallback="timeout"></div></div>"#,
            r#"<script type="module" src="/assets/c/abc/embed.js"></script></body></html>"#
        );
        let out = inject_park_native_host_scripts(sample);
        assert!(
            !out.contains("TheaterPlayer.host.js"),
            "theater host must not be injected: {out}"
        );
        assert!(out.contains("ExternalPlay.host.js?v=32"), "{out}");
        assert!(out.contains("Affirmation.host.js?v=38"), "{out}");
        assert!(out.contains("NowShowing.host.js?v=39"), "{out}");
        assert!(out.contains("LobbyAudio.host.js?v=44"), "{out}");
        assert!(out.contains("LobbyPlaylist.js?v=44"), "{out}");
        assert!(
            !out.contains("Board.host.js"),
            "Board host only when fb-board placeholder present: {out}"
        );
        let body_close = out.rfind("</body>").unwrap();
        let first_host = out.find("data-fb-park-native=\"1\"").unwrap();
        assert!(first_host < body_close);
    }

    #[test]
    fn inject_park_native_hosts_only_for_present_tags() {
        let sample = concat!(
            r#"<html><body><div data-relay-park-native="fb-now-showing"></div>"#,
            r#"<script type="module" src="/assets/c/abc/embed.js"></script></body></html>"#
        );
        let out = inject_park_native_host_scripts(sample);
        assert!(out.contains("NowShowing.host.js?v=39"), "{out}");
        assert!(out.contains("LobbyAudio.host.js?v=44"), "{out}");
        assert!(!out.contains("Board.host.js"), "{out}");
        assert!(!out.contains("Affirmation.host.js"), "{out}");
        assert!(!out.contains("ExternalPlay.host.js"), "{out}");
        assert!(!out.contains("PopImage.host.js"), "{out}");
    }

    #[test]
    fn inject_pop_image_host_when_marker_present() {
        let sample = concat!(
            r#"<html><body><img class="fb-movie-poster" data-fb-pop-image="1" src="/p.jpg" />"#,
            r#"<div data-relay-park-native="fb-external-play"></div>"#,
            r#"<script type="module" src="/assets/c/abc/embed.js"></script></body></html>"#
        );
        let out = inject_park_native_host_scripts(sample);
        assert!(out.contains("PopImage.host.js?v=32"), "{out}");
        assert!(out.contains("ExternalPlay.host.js?v=32"), "{out}");
        assert!(out.contains("LobbyAudio.host.js?v=44"), "{out}");
    }

    #[test]
    fn guard_strips_gallery_host_class_and_injects_css() {
        let sample = concat!(
            r#"<html><head><title>t</title></head><body>"#,
            r#"<relay-image-gallery class="pt-gallery" style="display:flex"></relay-image-gallery>"#,
            r#"</body></html>"#
        );
        let out = guard_relay_image_gallery_hosts(sample);
        assert!(
            out.contains("<relay-image-gallery></relay-image-gallery>"),
            "host attrs stripped: {out}"
        );
        assert!(!out.contains("class=\"pt-gallery\""), "{out}");
        assert!(out.contains("id=\"relay-gallery-host-guard\""), "{out}");
        assert!(out.contains("relay-image-gallery:not(:defined)"), "{out}");
    }

    #[test]
    fn strip_embed_js_and_ssr_static_policy() {
        let with = concat!(
            r#"<html lang="en" data-pt-hydrated="0"><body>"#,
            r#"<div id="root" data-relay-ssr="1" data-relay-ssr-path="/"></div>"#,
            r#"<script type="module" src="/assets/c/abc/embed.js"></script></body></html>"#
        );
        let stripped = strip_embed_js_scripts(with);
        assert!(!stripped.contains("embed.js"), "{stripped}");
        assert!(stripped.contains(r#"data-pt-hydrated="1""#), "{stripped}");
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
                ..Default::default()
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
        assert!(
            html.contains(r#"application/ld+json"#) && html.contains(r#""@type":"Article""#),
            "{html}"
        );
    }

    #[test]
    fn embed_seo_noindex_skips_json_ld() {
        let html = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/missing",
            "/hooks/client/get.jsx",
            None,
            None,
            None,
            Some("Missing"),
            None,
            Some("https://example.test/missing"),
            EmbedSeoExtras {
                noindex: true,
                ..Default::default()
            },
        );
        assert!(
            html.contains(r#"name="robots" content="noindex,follow""#),
            "{html}"
        );
        assert!(!html.contains("application/ld+json"), "{html}");
    }

    #[test]
    fn embed_seo_home_emits_website_json_ld() {
        let html = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/",
            "/hooks/client/get.jsx",
            None,
            None,
            None,
            Some("Paradigm Threat"),
            Some("Corpus"),
            Some("https://relay.paradigmthreat.net/"),
            EmbedSeoExtras {
                is_website_home: true,
                site_name: Some("Paradigm Threat"),
                ..Default::default()
            },
        );
        assert!(html.contains(r#""@type":"WebSite""#), "{html}");
        assert!(html.contains("SearchAction"), "{html}");
    }

    #[test]
    fn omit_embed_js_on_real_ssr_keeps_csr_placeholder_embed() {
        let ssr = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/van-helsing-deanna-d",
            "/hooks/client/get.jsx",
            Some("<main><h1>VH</h1></main>"),
            None,
            None,
            Some("VH"),
            None,
            None,
            EmbedSeoExtras {
                omit_embed_js: true,
                ..Default::default()
            },
        );
        assert!(!ssr.contains("embed.js"), "{ssr}");
        assert!(ssr.contains(r#"data-pt-hydrated="1""#), "{ssr}");
        assert!(ssr.contains("data-relay-ssr=\"1\""), "{ssr}");

        let csr = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/van-helsing-deanna-d",
            "/hooks/client/get.jsx",
            Some(ssr_timeout_notice_html()),
            None,
            None,
            Some("VH"),
            None,
            None,
            EmbedSeoExtras {
                csr_placeholder: true,
                omit_embed_js: true, // ignored for CSR shells
                ..Default::default()
            },
        );
        assert!(csr.contains("embed.js"), "{csr}");
        assert!(!csr.contains("data-relay-ssr="), "{csr}");
    }

    #[test]
    fn csr_placeholder_notice_does_not_mark_ssr() {
        let html = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/",
            "/hooks/client/get.jsx",
            Some(ssr_timeout_notice_html()),
            None,
            None,
            Some("Site"),
            None,
            None,
            EmbedSeoExtras {
                csr_placeholder: true,
                ..Default::default()
            },
        );
        assert!(html.contains("Server-side render timed out"), "{html}");
        assert!(html.contains("id=\"relay-ssr-fallback\""), "{html}");
        assert!(html.contains("id=\"relay-embed-config\""), "{html}");
        assert!(html.contains("embed.js"), "{html}");
        assert!(
            !html.contains("data-relay-ssr="),
            "placeholder must CSR-mount, not hydrate: {html}"
        );
    }

    #[test]
    fn ssr_document_path_marker_rejects_other_url_and_unmarked_hydrate() {
        let article = "/influence/predictive_programming/the-laugh-before-the-planet.md";
        let html = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            article,
            "/hooks/client/get.jsx",
            Some("<main><h1>The laugh</h1></main>"),
            None,
            None,
            Some("The laugh"),
            None,
            None,
            EmbedSeoExtras::default(),
        );
        assert!(
            html.contains(&format!("data-relay-ssr-path=\"{article}\"")),
            "{html}"
        );
        assert!(ssr_html_matches_request(&html, article), "{html}");
        assert!(!ssr_html_matches_request(&html, "/"));
        let home = embed_html_document_with_assets_and_seo_extras(
            "r",
            "main",
            "/",
            "/hooks/client/get.jsx",
            Some("<main><h1>Paradigm Threat Project</h1></main>"),
            None,
            None,
            Some("Home"),
            None,
            None,
            EmbedSeoExtras::default(),
        );
        assert!(!ssr_html_matches_request(&home, article));
        let unmarked = html.replace(" data-relay-ssr-path=", " data-x=");
        assert!(
            !ssr_html_matches_request(&unmarked, article),
            "hydrate markup without a path marker must not be reused"
        );
    }
}

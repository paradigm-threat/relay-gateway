//! Fast per-article SEO extraction from tip markdown (no SQLite / QUERY).
//!
//! Used when wrapping site-shell SSR HTML so crawlers get title, description,
//! og:image (`?w=`), and a public canonical URL.

/// Max characters for meta / og description.
pub const OG_DESCRIPTION_MAX: usize = 320;

/// Default raster width for Open Graph / Twitter preview images.
pub const OG_SHARE_IMAGE_WIDTH: u32 = 800;

/// Cap how much markdown we scan (keeps huge blobs cheap).
const SCAN_BYTES: usize = 128 * 1024;

const RASTER_EXT: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".avif", ".bmp", ".ico", ".apng",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleSeo {
    /// Document / og title (may include site name suffix).
    pub title: String,
    pub description: Option<String>,
    /// Absolute image URL, preferably with `?w=800` for rasters.
    pub image_url: Option<String>,
    /// Absolute public canonical URL for this article path.
    pub canonical_url: Option<String>,
    /// True when this path looks like a markdown article leaf.
    pub is_article: bool,
}

pub struct SeoFromMarkdownInput<'a> {
    pub markdown: &'a str,
    /// Repo-relative path (`governance/war/foo.md` or `/governance/war/foo.md`).
    pub rel_path: &'a str,
    /// Public site origin from `.relay.yaml` `canonical` (e.g. `https://paradigmthreat.net/`).
    pub public_origin: Option<&'a str>,
    /// Fallback origin when canonical is unset (request host).
    pub request_origin: Option<&'a str>,
    /// Site name from `.relay.yaml` (e.g. `Paradigm Threat`).
    pub site_name: Option<&'a str>,
}

/// Derive SEO fields from markdown text already loaded for P0 SSR.
pub fn seo_from_markdown(input: SeoFromMarkdownInput<'_>) -> ArticleSeo {
    let scan = if input.markdown.len() > SCAN_BYTES {
        &input.markdown[..SCAN_BYTES]
    } else {
        input.markdown
    };
    let (fm, body) = split_frontmatter(scan);
    let fm_title = fm_string(&fm, "title");
    let fm_desc = fm_string(&fm, "description");
    let fm_image = fm_string(&fm, "image").or_else(|| fm_string(&fm, "og_image"));

    let heading = extract_first_h1(body);
    let basename_title = title_from_path(input.rel_path);
    let article_title = fm_title
        .or(heading)
        .or(basename_title)
        .unwrap_or_else(|| "Article".to_string());

    let site = input
        .site_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Paradigm Threat");
    let title = if article_title.eq_ignore_ascii_case(site) || article_title.contains(site) {
        article_title.clone()
    } else {
        format!("{article_title} | {site}")
    };

    let description = fm_desc
        .or_else(|| extract_description(body))
        .map(|d| truncate_description(&d));

    let origin = normalize_origin(input.public_origin.or(input.request_origin));
    let image_url = fm_image
        .or_else(|| extract_first_markdown_image(body))
        .and_then(|raw| resolve_image_url(&raw, input.rel_path, origin.as_deref()));

    let pretty = pretty_public_path(input.rel_path);
    let is_article = path_is_markdown(input.rel_path);
    let canonical_url = origin.as_ref().map(|o| join_origin_path(o, &pretty));

    ArticleSeo {
        title,
        description,
        image_url,
        canonical_url,
        is_article,
    }
}

fn normalize_origin(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.trim_end_matches('/').to_string())
}

fn join_origin_path(origin: &str, pretty_path: &str) -> String {
    let path = if pretty_path.is_empty() || pretty_path == "/" {
        String::new()
    } else {
        format!("/{}", pretty_path.trim_start_matches('/'))
    };
    format!("{origin}{path}")
}

/// Extensionless public path: `foo/bar.md` → `/foo/bar`, `foo/page.md` → `/foo`, `/` stays `/`.
pub fn pretty_public_path(rel_path: &str) -> String {
    let mut p = rel_path.trim().trim_start_matches('/').to_string();
    if p.is_empty() {
        return "/".to_string();
    }
    let lower = p.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".mdx") {
        p.truncate(p.len() - if lower.ends_with(".mdx") { 4 } else { 3 });
    }
    if p.eq_ignore_ascii_case("page") || p.is_empty() {
        return "/".to_string();
    }
    if let Some(parent) = p.strip_suffix("/page") {
        if parent.is_empty() {
            return "/".to_string();
        }
        return format!("/{parent}");
    }
    format!("/{p}")
}

fn path_is_markdown(rel_path: &str) -> bool {
    let lower = rel_path.trim().to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".mdx") || !lower.contains('.')
}

fn title_from_path(rel_path: &str) -> Option<String> {
    let pretty = pretty_public_path(rel_path);
    let seg = pretty.rsplit('/').find(|s| !s.is_empty())?;
    let t = seg.replace(['-', '_'], " ");
    let t = t.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn split_frontmatter(md: &str) -> (Option<&str>, &str) {
    let md = if let Some(s) = md.strip_prefix('\u{feff}') {
        s
    } else {
        md
    };
    if !md.starts_with("---") {
        return (None, md);
    }
    let after_open = &md[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
    let Some(idx) = after_open.find("\n---") else {
        return (None, md);
    };
    let fm = &after_open[..idx];
    let rest = &after_open[idx + 4..];
    let body = rest.strip_prefix('\n').unwrap_or(rest);
    (Some(fm), body)
}

fn fm_string(fm: &Option<&str>, key: &str) -> Option<String> {
    let fm = (*fm)?;
    let prefix = format!("{key}:");
    for line in fm.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(&prefix) {
            let mut v = rest.trim().to_string();
            if (v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\''))
            {
                v = v[1..v.len() - 1].to_string();
            }
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn extract_first_h1(body: &str) -> Option<String> {
    for line in body.lines().take(40) {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("# ") {
            let t = rest.trim().trim_end_matches('#').trim();
            if !t.is_empty() {
                return Some(strip_md_inline(t));
            }
        }
    }
    None
}

fn strip_md_inline(s: &str) -> String {
    s.replace("**", "").replace('*', "").replace('`', "")
}

fn extract_description(body: &str) -> Option<String> {
    let mut in_blockquote = false;
    let mut buf = String::new();
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        if t.is_empty() {
            if !buf.is_empty() {
                break;
            }
            in_blockquote = false;
            continue;
        }
        if t.starts_with("![") {
            continue;
        }
        if let Some(rest) = t.strip_prefix('>') {
            in_blockquote = true;
            let piece = rest.trim();
            if piece.is_empty() {
                continue;
            }
            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(&strip_md_inline(piece));
            continue;
        }
        if in_blockquote && !buf.is_empty() {
            break;
        }
        // Prefer a TL;DR-ish blockquote; otherwise first paragraph.
        if !buf.is_empty() {
            break;
        }
        buf.push_str(&strip_md_inline(t));
        if buf.len() >= OG_DESCRIPTION_MAX {
            break;
        }
    }
    let cleaned = buf
        .replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn truncate_description(plain: &str) -> String {
    let t = plain.trim();
    if t.len() <= OG_DESCRIPTION_MAX {
        return t.to_string();
    }
    let cut = &t[..OG_DESCRIPTION_MAX.saturating_sub(1)];
    let last_space = cut.rfind(' ').filter(|&i| i > 40).unwrap_or(cut.len());
    format!("{}…", &cut[..last_space])
}

fn extract_first_markdown_image(body: &str) -> Option<String> {
    // ![alt](url) — stop at first ')'
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if bytes[i] == b'!' && bytes[i + 1] == b'[' {
            if let Some(alt_end) = body[i + 2..].find(']') {
                let after_alt = i + 2 + alt_end;
                if body.as_bytes().get(after_alt + 1) == Some(&b'(') {
                    let url_start = after_alt + 2;
                    if let Some(url_end) = body[url_start..].find(')') {
                        let raw = body[url_start..url_start + url_end].trim();
                        // Drop optional title: url "title"
                        let url = raw.split_whitespace().next().unwrap_or(raw);
                        if !url.is_empty() {
                            return Some(url.to_string());
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn is_raster_path(path_or_url: &str) -> bool {
    let path = path_or_url.split('?').next().unwrap_or(path_or_url);
    let lower = path.to_ascii_lowercase();
    RASTER_EXT.iter().any(|ext| lower.ends_with(ext))
}

fn resolve_image_url(raw: &str, article_rel: &str, origin: Option<&str>) -> Option<String> {
    let raw = raw.trim().split('?').next()?.trim();
    if raw.is_empty() {
        return None;
    }

    let absolute = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else if let Some(origin) = origin {
        let rel = if raw.starts_with('/') {
            raw.trim_start_matches('/').to_string()
        } else {
            let dir = article_dir(article_rel);
            if dir.is_empty() {
                raw.trim_start_matches("./").to_string()
            } else {
                format!("{dir}/{}", raw.trim_start_matches("./"))
            }
        };
        // Strip legacy /files/ prefix if present in absolute-ish site paths
        let rel = rel.strip_prefix("files/").unwrap_or(&rel).to_string();
        format!("{origin}/{}", rel.trim_start_matches('/'))
    } else {
        return None;
    };

    Some(maybe_append_og_width(&absolute))
}

/// Movie-page SEO from a catalog `index.json` when the request path is a project slug.
///
/// Flashback movie URLs (`/<slug>` and `/movie/<slug>`) have no markdown file, so the
/// markdown extractor never fills title, blurb, or poster. Crawlers then unfurl the
/// site-wide description with no image.
pub fn seo_from_catalog_index(
    index_json: &[u8],
    request_path: &str,
    public_origin: Option<&str>,
    request_origin: Option<&str>,
    site_name: Option<&str>,
) -> Option<ArticleSeo> {
    let slug = catalog_slug_from_path(request_path)?;
    let v: serde_json::Value = serde_json::from_slice(index_json).ok()?;
    let projects = v.get("projects")?.as_array()?;
    let project = projects.iter().find(|entry| {
        entry
            .get("slug")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s == slug)
    })?;

    let article_title = json_nonempty(project, "title").unwrap_or_else(|| slug.to_string());
    let site = site_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Flashback Repository");
    let title = if article_title.eq_ignore_ascii_case(site) || article_title.contains(site) {
        article_title
    } else {
        format!("{article_title} | {site}")
    };

    let description = json_nonempty(project, "blurb")
        .or_else(|| json_nonempty(project, "description"))
        .map(|d| truncate_description(&d));

    let origin = normalize_origin(public_origin.or(request_origin));
    let image_raw = json_nonempty(project, "image").or_else(|| {
        let dir = json_nonempty(project, "path")?;
        let poster = json_nonempty(project, "poster").unwrap_or_else(|| "poster.jpg".to_string());
        Some(format!("{dir}/{poster}"))
    });
    let image_url = image_raw.and_then(|raw| {
        let encoded = encode_repo_path(&raw);
        resolve_image_url(&encoded, "", origin.as_deref())
    });

    let canonical_url = origin.as_ref().map(|o| join_origin_path(o, &format!("/{slug}")));

    Some(ArticleSeo {
        title,
        description,
        image_url,
        canonical_url,
        is_article: false,
    })
}

fn catalog_slug_from_path(request_path: &str) -> Option<&str> {
    let mut slug = request_path.trim().trim_matches('/');
    if let Some(rest) = slug.strip_prefix("movie/") {
        slug = rest;
    }
    if slug.is_empty() || slug.contains('/') || slug.contains('.') {
        return None;
    }
    Some(slug)
}

fn json_nonempty(obj: &serde_json::Value, key: &str) -> Option<String> {
    let s = obj.get(key)?.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn encode_repo_path(raw: &str) -> String {
    const PATH_SEG: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}');
    raw.trim()
        .trim_start_matches('/')
        .split('/')
        .map(|seg| percent_encoding::utf8_percent_encode(seg, PATH_SEG).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn article_dir(rel_path: &str) -> String {
    let p = rel_path.trim().trim_start_matches('/');
    match p.rfind('/') {
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

fn maybe_append_og_width(absolute_url: &str) -> String {
    if !is_raster_path(absolute_url) {
        return absolute_url
            .split('?')
            .next()
            .unwrap_or(absolute_url)
            .to_string();
    }
    let base = absolute_url.split('?').next().unwrap_or(absolute_url);
    format!("{base}?w={OG_SHARE_IMAGE_WIDTH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_h1_description_image_and_canonical() {
        let md = r#"# Mission Accomplished (Again)

Intelligence agencies shaped the Middle East.

![Hero](mission-accomplished-again-iran-2026.png)

More body.
"#;
        let seo = seo_from_markdown(SeoFromMarkdownInput {
            markdown: md,
            rel_path: "governance/war/mission-accomplished-again-iran-2026.md",
            public_origin: Some("https://paradigmthreat.net/"),
            request_origin: None,
            site_name: Some("Paradigm Threat"),
        });
        assert_eq!(seo.title, "Mission Accomplished (Again) | Paradigm Threat");
        assert!(
            seo.description
                .as_deref()
                .unwrap_or("")
                .contains("Intelligence agencies"),
            "{:?}",
            seo.description
        );
        assert_eq!(
            seo.image_url.as_deref(),
            Some(
                "https://paradigmthreat.net/governance/war/mission-accomplished-again-iran-2026.png?w=800"
            )
        );
        assert_eq!(
            seo.canonical_url.as_deref(),
            Some("https://paradigmthreat.net/governance/war/mission-accomplished-again-iran-2026")
        );
        assert!(seo.is_article);
    }

    #[test]
    fn absolute_site_image_gets_w_param() {
        let md = "![x](https://paradigmthreat.net/governance/war/hero.png)\n\n# T\n";
        let seo = seo_from_markdown(SeoFromMarkdownInput {
            markdown: md,
            rel_path: "governance/war/x.md",
            public_origin: Some("https://paradigmthreat.net"),
            request_origin: None,
            site_name: Some("Paradigm Threat"),
        });
        assert_eq!(
            seo.image_url.as_deref(),
            Some("https://paradigmthreat.net/governance/war/hero.png?w=800")
        );
    }

    #[test]
    fn page_md_pretty_path_is_parent() {
        assert_eq!(
            pretty_public_path("governance/war/page.md"),
            "/governance/war"
        );
        assert_eq!(pretty_public_path("page.md"), "/");
        assert_eq!(pretty_public_path("/"), "/");
    }

    #[test]
    fn frontmatter_overrides() {
        let md = r#"---
title: Custom Title
description: Custom desc here
image: ./cover.jpg
---

# Ignored Heading

Body.
"#;
        let seo = seo_from_markdown(SeoFromMarkdownInput {
            markdown: md,
            rel_path: "blog/post.md",
            public_origin: Some("https://paradigmthreat.net"),
            request_origin: None,
            site_name: Some("Paradigm Threat"),
        });
        assert_eq!(seo.title, "Custom Title | Paradigm Threat");
        assert_eq!(seo.description.as_deref(), Some("Custom desc here"));
        assert_eq!(
            seo.image_url.as_deref(),
            Some("https://paradigmthreat.net/blog/cover.jpg?w=800")
        );
    }

    #[test]
    fn prefers_blockquote_tldr_when_present() {
        let md = r#"# Title

> **TL;DR:** Western covert history brackets latency.

Intro paragraph ignored when blockquote came first after heading/image skip path.
"#;
        let seo = seo_from_markdown(SeoFromMarkdownInput {
            markdown: md,
            rel_path: "a.md",
            public_origin: Some("https://paradigmthreat.net"),
            request_origin: None,
            site_name: Some("Paradigm Threat"),
        });
        let d = seo.description.unwrap();
        assert!(d.contains("Western covert history"), "{d}");
    }

    #[test]
    fn catalog_slug_uses_title_blurb_and_poster() {
        let catalog = seo_from_catalog_index(
            br#"{
              "projects": [{
                "slug": "matrix-reloaded-2003-unlinked",
                "title": "Matrix Reloaded (2003) - Unlinked",
                "blurb": "Bane sabotages the hovercrafts. Post-production sabotages the franchise.",
                "image": "FanMixes/Matrix Reloaded (2003) - Unlinked/poster.jpg"
              }]
            }"#,
            "/movie/matrix-reloaded-2003-unlinked",
            Some("https://flashbackrepository.org/"),
            None,
            Some("Flashback Repository"),
        )
        .expect("catalog seo");
        assert_eq!(
            catalog.title,
            "Matrix Reloaded (2003) - Unlinked | Flashback Repository"
        );
        assert_eq!(
            catalog.description.as_deref(),
            Some("Bane sabotages the hovercrafts. Post-production sabotages the franchise.")
        );
        assert_eq!(
            catalog.image_url.as_deref(),
            Some("https://flashbackrepository.org/FanMixes/Matrix%20Reloaded%20(2003)%20-%20Unlinked/poster.jpg?w=800")
        );
        assert_eq!(
            catalog.canonical_url.as_deref(),
            Some("https://flashbackrepository.org/matrix-reloaded-2003-unlinked")
        );
        assert!(!catalog.is_article);
        assert!(seo_from_catalog_index(b"{}", "/search", None, None, None).is_none());
    }
}

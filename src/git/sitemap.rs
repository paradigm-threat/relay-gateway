//! Tip-based `sitemap.xml` / `robots.txt` for parked hosts.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use git2::{ObjectType, Repository};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::git::relay_config_cache::read_relay_config_at_path_cached;
use crate::git::seo_from_markdown::pretty_public_path;
use crate::helpers;
use crate::AppState;

/// Soft cap so a huge tip cannot blow memory on one request.
pub const SITEMAP_URL_CAP: usize = 50_000;

const SKIP_PREFIXES: &[&str] = &[
    "hooks/",
    ".git/",
    ".github/",
    "node_modules/",
    "target/",
    "assets/",
    "wasm/",
    "scripts/",
];

/// True when this path should be served as dynamic SEO sitemap/robots (not tip blobs).
pub fn is_seo_meta_path(rel: &str) -> bool {
    let p = rel.trim_start_matches('/');
    p.eq_ignore_ascii_case("sitemap.xml") || p.eq_ignore_ascii_case("robots.txt")
}

/// Serve `sitemap.xml` or `robots.txt` for a parked / repo Host.
pub fn try_seo_meta_response(
    state: &AppState,
    headers: &HeaderMap,
    branch: &str,
    repo_name: &str,
    rel: &str,
) -> Option<Response> {
    let p = rel.trim_start_matches('/');
    if p.eq_ignore_ascii_case("robots.txt") {
        return Some(robots_txt_response(state, headers, branch, repo_name));
    }
    if p.eq_ignore_ascii_case("sitemap.xml") {
        return Some(sitemap_xml_response(state, headers, branch, repo_name));
    }
    None
}

fn bare_path(state: &AppState, repo_name: &str) -> Option<std::path::PathBuf> {
    match &state.serve_context {
        crate::ServeContext::BareFleet => {
            let p = state.repo_path.join(format!("{repo_name}.git"));
            if p.is_dir() {
                Some(p)
            } else {
                None
            }
        }
        crate::ServeContext::LocalWorktree { checkout, .. } => Some(checkout.clone()),
    }
}

fn public_origin(state: &AppState, headers: &HeaderMap, branch: &str, repo_name: &str) -> String {
    if let Some(bare) = bare_path(state, repo_name) {
        if let Some(cfg) = read_relay_config_at_path_cached(&bare, branch) {
            if let Some(c) = cfg.canonical.as_ref() {
                let t = c.trim().trim_end_matches('/');
                if !t.is_empty() {
                    return t.to_string();
                }
            }
        }
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if host.is_empty() {
        return String::new();
    }
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|p| p.split(',').next())
        .map(str::trim)
        .filter(|p| *p == "http" || *p == "https")
        .unwrap_or("https");
    format!("{proto}://{host}")
}

fn robots_txt_response(
    state: &AppState,
    headers: &HeaderMap,
    branch: &str,
    repo_name: &str,
) -> Response {
    let origin = public_origin(state, headers, branch, repo_name);
    let body = if origin.is_empty() {
        "User-agent: *\nAllow: /\n".to_string()
    } else {
        format!(
            "User-agent: *\nAllow: /\nSitemap: {origin}/sitemap.xml\n"
        )
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
        .into_response()
}

fn sitemap_xml_response(
    state: &AppState,
    headers: &HeaderMap,
    branch: &str,
    repo_name: &str,
) -> Response {
    let origin = public_origin(state, headers, branch, repo_name);
    let Some(bare) = bare_path(state, repo_name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let entries = collect_sitemap_entries(&bare, branch, &origin);
    let mut xml = String::with_capacity(256 + entries.len() * 128);
    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push('\n');
    xml.push_str(r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    xml.push('\n');
    for (loc, lastmod) in entries {
        xml.push_str("  <url>\n    <loc>");
        xml.push_str(&escape_xml(&loc));
        xml.push_str("</loc>\n");
        if let Some(d) = lastmod {
            xml.push_str("    <lastmod>");
            xml.push_str(&d);
            xml.push_str("</lastmod>\n");
        }
        xml.push_str("  </url>\n");
    }
    xml.push_str("</urlset>\n");
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        xml,
    )
        .into_response()
}

fn collect_sitemap_entries(
    repo_path: &Path,
    branch: &str,
    origin: &str,
) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    if origin.is_empty() {
        return out;
    }
    let Ok(repo) = Repository::open(repo_path).or_else(|_| Repository::open_bare(repo_path)) else {
        return out;
    };
    let Ok(reference) = repo.find_reference(&format!("refs/heads/{branch}")) else {
        return out;
    };
    let Ok(commit) = reference.peel_to_commit() else {
        return out;
    };
    let tip_lastmod = commit_lastmod(&commit);
    let Ok(tree) = commit.tree() else {
        return out;
    };

    // Home
    out.push((format!("{}/", origin.trim_end_matches('/')), tip_lastmod.clone()));
    out.push((
        format!("{}/search", origin.trim_end_matches('/')),
        tip_lastmod.clone(),
    ));

    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if out.len() >= SITEMAP_URL_CAP {
            return git2::TreeWalkResult::Abort;
        }
        if entry.kind() != Some(ObjectType::Blob) {
            return git2::TreeWalkResult::Ok;
        }
        let name = entry.name().unwrap_or("");
        if name.is_empty() || name.starts_with('.') {
            return git2::TreeWalkResult::Ok;
        }
        let rel = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}{name}")
        };
        if should_skip_rel(&rel) {
            return git2::TreeWalkResult::Ok;
        }
        if !rel.ends_with(".md") && !rel.ends_with(".markdown") {
            return git2::TreeWalkResult::Ok;
        }
        // Skip page.md home duplicate (pretty path `/`).
        if rel == "page.md" {
            return git2::TreeWalkResult::Ok;
        }
        let pretty = pretty_public_path(&rel);
        let loc = join_origin_path(origin, &pretty);
        out.push((loc, tip_lastmod.clone()));
        git2::TreeWalkResult::Ok
    });
    out
}

fn should_skip_rel(rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    SKIP_PREFIXES.iter().any(|p| rel.starts_with(p))
        || rel.contains("/.")
        || rel.ends_with(".cs")
        || rel.ends_with(".jsx")
        || rel.ends_with(".tsx")
        || base.eq_ignore_ascii_case("agents.md")
        || base.eq_ignore_ascii_case("readme.md")
        || base.eq_ignore_ascii_case("reorganization_map.md")
}

fn join_origin_path(origin: &str, path: &str) -> String {
    let o = origin.trim_end_matches('/');
    let p = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{o}{p}")
}

fn commit_lastmod(commit: &git2::Commit<'_>) -> Option<String> {
    let secs = commit.time().seconds();
    if secs <= 0 {
        return None;
    }
    let t = UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))?;
    format_date(t)
}

fn format_date(t: SystemTime) -> Option<String> {
    let dur = t.duration_since(UNIX_EPOCH).ok()?;
    let secs = dur.as_secs() as i64;
    // YYYY-MM-DD via approximate civil date (good enough for lastmod).
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Howard Hinnant civil-from-days (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Helper for tests / ops: resolve parked repo + branch like file GETs.
pub fn try_seo_meta_for_request(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
) -> Option<Response> {
    if !is_seo_meta_path(path) {
        return None;
    }
    let repo_name = helpers::repo_from_host_or_default(state, state.node_fqdn.as_deref(), headers)?;
    let branch = helpers::branch_for_request(headers, state, Some(repo_name.as_str()));
    try_seo_meta_response(state, headers, &branch, &repo_name, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seo_meta_path_detects() {
        assert!(is_seo_meta_path("sitemap.xml"));
        assert!(is_seo_meta_path("/robots.txt"));
        assert!(!is_seo_meta_path("page.md"));
    }

    #[test]
    fn escape_xml_basic() {
        assert_eq!(escape_xml("a&b"), "a&amp;b");
    }
}

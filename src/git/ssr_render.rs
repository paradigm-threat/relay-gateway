//! Render CleverScript client-hook SSR for a path at a branch tip or commit OID.
//! Used by pre-receive QA and publisher bake (not the HTML request path for `ssrCache` Hosts).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::git::embed_html;
use crate::git::relay_config_cache;
use crate::git::ssr_client_hook::{try_ssr_client_hook, ClientHookSsrRequest, ClientHookSsrResult};
use crate::git::ssr_html_quality::{
    check_ssr_html, infer_route_class, load_profile_from_path, QualityFailure, QualityOptions,
    SsrQaProfile,
};

#[derive(Debug, Clone)]
pub struct SsrRenderOutput {
    pub html: String,
    pub body_html: String,
    pub css: String,
    pub client_hook_rel: String,
}

#[derive(Debug, Clone)]
pub struct SsrRenderError {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for SsrRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

fn client_hook_rel_at_rev(repo_path: &Path, rev: &str) -> String {
    relay_config_cache::read_relay_config_at_path_cached(repo_path, rev)
        .and_then(|cfg| {
            cfg.client
                .as_ref()
                .and_then(|c| c.hooks.get.as_ref())
                .map(|h| h.path.trim().trim_start_matches('/').to_string())
                .filter(|p| !p.is_empty())
        })
        .unwrap_or_else(|| "hooks/client/get.jsx".to_string())
}

fn normalize_initial_path(path: &str) -> String {
    crate::git::ssr_policy::normalize_request_path(path)
}

/// Run client-hook SSR at `rev` (branch or commit OID) for `request_path`.
pub fn ssr_render_path_at_commit(
    repo_path: &Path,
    rev: &str,
    request_path: &str,
) -> Result<SsrRenderOutput, SsrRenderError> {
    let initial_path = normalize_initial_path(request_path);
    let client_hook_rel = client_hook_rel_at_rev(repo_path, rev);
    let repo_buf = PathBuf::from(repo_path);
    let rev_owned = rev.to_string();
    let hook_owned = client_hook_rel.clone();
    let path_owned = initial_path.clone();

    let rendered: Option<ClientHookSsrResult> = try_ssr_client_hook(&ClientHookSsrRequest {
        repo_path: &repo_buf,
        branch: &rev_owned,
        client_hook_rel: &hook_owned,
        asset_base_url: "",
        initial_path: &path_owned,
    });

    let Some(result) = rendered else {
        return Err(SsrRenderError {
            path: initial_path,
            reason: "client hook SSR returned no body (timeout, panic, or empty)".into(),
        });
    };
    if result.body_html.trim().is_empty() {
        return Err(SsrRenderError {
            path: initial_path,
            reason: "client hook SSR body empty".into(),
        });
    }

    let repo_slug = repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .trim_end_matches(".git");
    let client_hook_http = format!("/{}", client_hook_rel.trim_start_matches('/'));
    let html = embed_html::embed_html_document_with_assets_and_seo_extras(
        repo_slug,
        rev,
        initial_path.trim_start_matches('/'),
        &client_hook_http,
        Some(&result.body_html),
        Some(&result.css),
        None,
        None,
        None,
        None,
        embed_html::EmbedSeoExtras {
            omit_embed_js: false,
            ..Default::default()
        },
    );

    Ok(SsrRenderOutput {
        html,
        body_html: result.body_html,
        css: result.css,
        client_hook_rel,
    })
}

/// Render + quality gate. `profile` optional; otherwise tries `.relay/ssr-qa.json` via git show.
pub fn ssr_render_and_qa(
    repo_path: &Path,
    rev: &str,
    request_path: &str,
    profile: Option<&SsrQaProfile>,
) -> Result<SsrRenderOutput, SsrRenderError> {
    let out = ssr_render_path_at_commit(repo_path, rev, request_path)?;
    let route = infer_route_class(request_path);
    let initial = normalize_initial_path(request_path);
    let loaded = if profile.is_some() {
        None
    } else {
        repo_path
            .parent()
            .and_then(|p| {
                let name = repo_path
                    .file_name()
                    .and_then(|s| s.to_str())?
                    .trim_end_matches(".git");
                load_profile_from_path(&p.join(name))
            })
            .or_else(|| {
                let bytes =
                    crate::git::repo::read_file_via_git_show(repo_path, rev, ".relay/ssr-qa.json")
                        .ok()?;
                let s = String::from_utf8(bytes).ok()?;
                crate::git::ssr_html_quality::parse_profile_json(&s).ok()
            })
    };
    let profile_ref = profile.or(loaded.as_ref());

    if let Err(e) = check_ssr_html(
        &out.html,
        QualityOptions {
            request_path: Some(&initial),
            expect_full_document: true,
            profile: profile_ref,
            route_class: route,
        },
    ) {
        return Err(SsrRenderError {
            path: initial.clone(),
            reason: e.reason,
        });
    }
    // Body fragment hard checks (no path marker on body alone).
    if let Err(e) = check_ssr_html(
        &out.body_html,
        QualityOptions {
            request_path: None,
            expect_full_document: false,
            profile: None,
            route_class: route,
        },
    ) {
        return Err(SsrRenderError {
            path: initial,
            reason: format!("body: {}", e.reason),
        });
    }
    Ok(out)
}

/// Wall-clock budget for a whole pre-receive SSR pass.
pub fn pre_receive_wall_budget() -> Duration {
    let secs = std::env::var("RELAY_SSR_PRE_RECEIVE_WALL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(90);
    Duration::from_secs(secs.max(5))
}

pub fn deadline_exceeded(started: Instant, budget: Duration) -> bool {
    started.elapsed() >= budget
}

/// Map quality failure into pre-receive message.
pub fn quality_to_render_error(path: &str, e: QualityFailure) -> SsrRenderError {
    SsrRenderError {
        path: normalize_initial_path(path),
        reason: e.reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// CleverScript host uses a process-wide lock; serialize WASM SSR tests.
    static SSR_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn bare_with_files(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        let repo = Repository::init_bare(&bare).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        for (path, src) in files {
            let blob = repo.blob(src.as_bytes()).unwrap();
            let entry = git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: src.len() as u32,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            };
            index.add(&entry).unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, bare)
    }

    #[test]
    fn render_and_qa_accepts_and_rejects() {
        let _guard = SSR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let good = r#"
export default function App() {
  return <div className="ssr-root"><h1>Hello SSR</h1></div>
}
"#;
        let (_dir_ok, bare_ok) = bare_with_files(&[("hooks/client/get.jsx", good)]);
        let out = ssr_render_and_qa(&bare_ok, "main", "/README.md", None).expect("qa ok");
        assert!(out.body_html.contains("Hello SSR"), "{}", out.body_html);
        assert!(
            out.html.to_ascii_lowercase().contains("<!doctype") || out.html.contains("<html"),
            "{}",
            &out.html[..out.html.len().min(200)]
        );

        let bad = r#"
export default function App() {
  return <div className="relay-error-boundary">Runtime error</div>
}
"#;
        let (_dir_bad, bare_bad) = bare_with_files(&[("hooks/client/get.jsx", bad)]);
        let err = ssr_render_and_qa(&bare_bad, "main", "/", None).expect_err("must fail QA");
        assert!(
            err.reason.contains("forbidden")
                || err.reason.contains("Runtime")
                || err.reason.contains("boundary")
                || err.reason.contains("empty"),
            "{}",
            err.reason
        );
    }
}

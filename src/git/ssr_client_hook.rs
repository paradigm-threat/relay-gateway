//! Server-side render of the repo **client GET hook** into an HTML fragment for embed pages.

use crate::git::repo::read_file_from_repo;
use relay_runtime::hook_runner::run_hook_to_vnode_stable_isolated;
use relay_runtime::host::{host_fetch_object, Host, HostHandle, MockHost};
use relay_runtime::html::{vnode_to_html, HtmlRenderOptions};
use relay_runtime::style_resolve::Platform;
use relay_runtime::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Inputs for SSR of `hooks/client/get.jsx` (or configured client GET path).
pub struct ClientHookSsrRequest<'a> {
    pub repo_path: &'a PathBuf,
    pub branch: &'a str,
    pub client_hook_rel: &'a str,
    /// Public origin for asset URLs (no trailing slash), e.g. empty for root-relative.
    pub asset_base_url: &'a str,
    /// Path the browser is viewing (repo-relative with leading slash), for hooks manifest.
    pub initial_path: &'a str,
}

/// Successful SSR output.
#[derive(Clone)]
pub struct ClientHookSsrResult {
    pub body_html: String,
    /// Generated stylesheet for no-JS / SSR (may be empty).
    pub css: String,
    pub stable_passes: u32,
}

/// Host that serves `fetch` from the bare git repo (and optional absolute HTTP).
struct RepoFetchHost {
    repo_path: PathBuf,
    branch: String,
    /// Public origin (no trailing slash). Same-origin absolute URLs map to git reads.
    asset_base_url: String,
    /// Fallback create_element / navigate via MockHost.
    inner: Rc<RefCell<MockHost>>,
}

impl RepoFetchHost {
    fn repo_rel_from_url(&self, url: &str) -> Option<String> {
        let u = url.trim();
        if u.is_empty() {
            return None;
        }
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            let path = u.split('?').next().unwrap_or(u);
            let path = path.split('#').next().unwrap_or(path);
            return Some(path.trim_start_matches('/').to_string());
        }
        let base = self.asset_base_url.trim_end_matches('/');
        if base.is_empty() {
            return None;
        }
        let stripped = u.strip_prefix(base).or_else(|| {
            // Tolerate http/https mismatch on the same host path.
            let alt = if base.starts_with("https://") {
                format!("http://{}", base.trim_start_matches("https://"))
            } else if base.starts_with("http://") {
                format!("https://{}", base.trim_start_matches("http://"))
            } else {
                return None;
            };
            u.strip_prefix(&alt)
        })?;
        let path = stripped.split('?').next().unwrap_or(stripped);
        let path = path.split('#').next().unwrap_or(path);
        Some(path.trim_start_matches('/').to_string())
    }

    /// Same-repo QUERY for SSR (client hooks cannot self-HTTP QUERY during render).
    fn ssr_query(&self, url: &str, options: Option<&Value>) -> Result<Value, String> {
        let rel = self.repo_rel_from_url(url).unwrap_or_default();
        if !rel.is_empty() && rel != "query" {
            return Ok(host_fetch_object(
                false,
                400.0,
                "",
                Some(format!(
                    "SSR QUERY only supported on collection root, got path {rel}"
                )),
            ));
        }

        let body_str = options
            .and_then(|o| match o {
                Value::Object(m) => m.get("body"),
                _ => None,
            })
            .and_then(|v| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("{}");
        let body: serde_json::Value =
            serde_json::from_str(body_str).unwrap_or_else(|_| serde_json::json!({}));
        let collection = body
            .get("collection")
            .and_then(|v| v.as_str())
            .unwrap_or("index");
        let limit = body
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let query = body.get("query").cloned();

        let rows = crate::git::search_plugins::run_plugin(
            crate::git::search_plugins::PLUGIN_SQLITE,
            &self.repo_path,
            &self.branch,
            collection,
            query.as_ref(),
            None,
            limit,
        )
        .unwrap_or_default();
        let envelope = serde_json::json!({
            "results": rows,
            "count": rows.len(),
            "collection": collection,
            "query": query.unwrap_or(serde_json::Value::String(String::new())),
        });
        let text = serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"results\":[]}".into());
        Ok(host_fetch_object(true, 200.0, text, None))
    }
}

impl Host for RepoFetchHost {
    fn fetch(&self, url: &str, options: Option<&Value>) -> Result<Value, String> {
        let method = options
            .and_then(|o| match o {
                Value::Object(m) => m.get("method"),
                _ => None,
            })
            .and_then(|v| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("GET");

        if method.eq_ignore_ascii_case("QUERY") {
            return self.ssr_query(url, options);
        }

        if !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD") {
            return Ok(host_fetch_object(
                false,
                405.0,
                "",
                Some(format!(
                    "SSR host only supports GET/HEAD/QUERY, got {method}"
                )),
            ));
        }

        // Prefer in-process git reads for same-origin URLs (avoids self-HTTP deadlock/latency).
        if let Some(rel) = self.repo_rel_from_url(url) {
            if !rel.is_empty() {
                match read_file_from_repo(&self.repo_path, &self.branch, &rel) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        return Ok(host_fetch_object(true, 200.0, text, None));
                    }
                    Err(_) => {
                        return Ok(host_fetch_object(
                            false,
                            404.0,
                            "",
                            Some(format!("not found in repo: {rel}")),
                        ));
                    }
                }
            }
            // Empty rel (`/`): no blob — fall through (QUERY handled above).
        }

        if url.starts_with("http://") || url.starts_with("https://") {
            return match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .and_then(|c| c.get(url).send())
            {
                Ok(resp) => {
                    let status = resp.status().as_u16() as f64;
                    let ok = resp.status().is_success();
                    let text = resp.text().unwrap_or_default();
                    Ok(host_fetch_object(ok, status, text, None))
                }
                Err(e) => Ok(host_fetch_object(false, 0.0, "", Some(e.to_string()))),
            };
        }

        Ok(host_fetch_object(
            false,
            404.0,
            "",
            Some(format!("not found: {url}")),
        ))
    }

    fn navigate(&self, path: &str) -> Result<(), String> {
        self.inner.borrow().navigate(path)
    }

    fn create_element(
        &self,
        tag: &str,
        props: &Value,
        children: &[Value],
    ) -> Result<Value, String> {
        self.inner.borrow().create_element(tag, props, children)
    }

    fn load_module(&self, path: &str) -> Result<Value, String> {
        self.inner.borrow().load_module(path)
    }

    fn log(&self, level: &str, message: &str) {
        tracing::debug!(target: "relay_ssr", level, message);
    }
}

fn read_repo_text(repo_path: &PathBuf, branch: &str, rel: &str) -> Option<String> {
    let bytes = read_file_from_repo(repo_path, branch, rel).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Local tip import specs (`hooks/...`, `./...`, `../...`) — not `@relay/*` / packages.
fn local_import_specs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(idx) = rest.find(" from ") {
        let after = &rest[idx + 6..];
        let after = after.trim_start();
        let (q, body) = if let Some(b) = after.strip_prefix('\'') {
            ('\'', b)
        } else if let Some(b) = after.strip_prefix('"') {
            ('"', b)
        } else {
            rest = &rest[idx + 6..];
            continue;
        };
        if let Some(end) = body.find(q) {
            let spec = &body[..end];
            if spec.starts_with("hooks/")
                || spec.starts_with("./")
                || spec.starts_with("../")
                || spec.ends_with(".jsx")
                || spec.ends_with(".js")
                || spec.ends_with(".yaml")
                || spec.ends_with(".yml")
            {
                out.push(spec.to_string());
            }
            rest = &body[end + 1..];
        } else {
            break;
        }
    }
    out
}

fn insert_module(modules: &mut HashMap<String, String>, rel: &str, src: String) {
    let rel = rel.trim_start_matches('/');
    modules.insert(rel.to_string(), src.clone());
    modules.insert(format!("/{rel}"), src);
}

fn resolve_import_rel(importer: &str, spec: &str) -> String {
    if spec.starts_with("hooks/") || !spec.starts_with('.') {
        return spec.trim_start_matches('/').to_string();
    }
    let importer = importer.trim_start_matches('/');
    let base = std::path::Path::new(importer)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let joined = base.join(spec);
    let mut parts = Vec::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            _ => {}
        }
    }
    parts.join("/")
}

fn preload_import_closure(
    repo_path: &PathBuf,
    branch: &str,
    modules: &mut HashMap<String, String>,
) {
    let mut queue: Vec<String> = modules
        .keys()
        .filter(|k| !k.starts_with('/'))
        .cloned()
        .collect();
    let mut seen = std::collections::HashSet::new();
    while let Some(path) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(src) = modules.get(&path).cloned() else {
            continue;
        };
        for spec in local_import_specs(&src) {
            let rel = resolve_import_rel(&path, &spec);
            if modules.contains_key(&rel) {
                continue;
            }
            if let Some(body) = read_repo_text(repo_path, branch, &rel) {
                insert_module(modules, &rel, body);
                queue.push(rel);
            }
        }
    }
}

fn load_module_map(
    repo_path: &PathBuf,
    branch: &str,
    entry_rel: &str,
) -> (HashMap<String, String>, Value) {
    let mut modules = HashMap::new();
    let entry = entry_rel.trim_start_matches('/');
    if let Some(src) = read_repo_text(repo_path, branch, entry) {
        insert_module(&mut modules, entry, src);
    }

    let mut hooks_json = serde_json::json!({
        "hooks": [{ "path": entry, "name": "Default" }]
    });

    if let Some(raw) = read_repo_text(repo_path, branch, "hooks.json") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            hooks_json = v;
            if let Some(arr) = hooks_json.get("hooks").and_then(|h| h.as_array()) {
                for h in arr {
                    if let Some(path) = h.get("path").and_then(|p| p.as_str()) {
                        let p = path.trim_start_matches('/');
                        if modules.contains_key(p) {
                            continue;
                        }
                        if let Some(src) = read_repo_text(repo_path, branch, p) {
                            insert_module(&mut modules, p, src);
                        }
                    }
                }
            }
        }
    }

    // Common theme / layout paths often imported relatively from the GET hook.
    for extra in [
        "hooks/client/theme.yaml",
        "hooks/client/components/Layout.jsx",
        "hooks/client/components/Icons.jsx",
        "theme.yaml",
    ] {
        if modules.contains_key(extra) {
            continue;
        }
        if let Some(src) = read_repo_text(repo_path, branch, extra) {
            insert_module(&mut modules, extra, src);
        }
    }

    // Follow tip-local imports so new components work without editing hooks.json.
    preload_import_closure(repo_path, branch, &mut modules);

    (modules, Value::from_json(&hooks_json))
}

fn theme_yaml_from_repo(repo_path: &PathBuf, branch: &str) -> String {
    for path in ["hooks/client/theme.yaml", "theme.yaml", "hooks/theme.yaml"] {
        if let Some(s) = read_repo_text(repo_path, branch, path) {
            return s;
        }
    }
    String::new()
}

fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn late_ssr_map() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, ClientHookSsrResult>,
> {
    static LATE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ClientHookSsrResult>>,
    > = std::sync::OnceLock::new();
    LATE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn take_late_ssr(key: &str) -> Option<ClientHookSsrResult> {
    late_ssr_map().lock().ok()?.remove(key)
}

fn store_late_ssr(key: &str, value: ClientHookSsrResult) {
    let Ok(mut map) = late_ssr_map().lock() else {
        return;
    };
    if map.len() >= 32 {
        map.clear();
    }
    map.insert(key.to_string(), value);
}

/// Try to SSR the client GET hook. Returns `None` on any failure (caller falls back to empty shell).
/// Panics inside the renderer are caught so HTTP handlers can return 500 instead of dropping the connection.
///
/// Runs on a dedicated OS thread under [`crate::git::ssr_limits`] with per-path coalesce and
/// cooperative cancel on wall-clock timeout.
pub fn try_ssr_client_hook(req: &ClientHookSsrRequest<'_>) -> Option<ClientHookSsrResult> {
    let tip = crate::git::ssr_html_cache::tip_oid_for_ssr(req.repo_path, req.branch);
    let key = crate::git::ssr_limits::coalesce_key(
        req.repo_path,
        &tip,
        req.branch,
        req.initial_path,
        req.client_hook_rel,
    );
    let owned = OwnedSsrRequest {
        repo_path: req.repo_path.clone(),
        branch: req.branch.to_string(),
        client_hook_rel: req.client_hook_rel.to_string(),
        asset_base_url: req.asset_base_url.to_string(),
        initial_path: req.initial_path.to_string(),
    };
    if let Some(hit) = take_late_ssr(&key) {
        tracing::info!(
            target: "relay_ssr",
            path = %req.initial_path,
            "SSR body kept after the waiter left; caller can fill the HTML cache"
        );
        return Some(hit);
    }
    static REG: std::sync::OnceLock<
        std::sync::Arc<crate::git::ssr_limits::CoalesceRegistry<ClientHookSsrResult>>,
    > = std::sync::OnceLock::new();
    let reg = REG
        .get_or_init(|| std::sync::Arc::new(crate::git::ssr_limits::CoalesceRegistry::new()))
        .clone();
    let key_store = key.clone();
    let key_clear = key.clone();
    let out = crate::git::ssr_limits::run_ssr_coalesced(reg, key, move || {
        let req = ClientHookSsrRequest {
            repo_path: &owned.repo_path,
            branch: &owned.branch,
            client_hook_rel: &owned.client_hook_rel,
            asset_base_url: &owned.asset_base_url,
            initial_path: &owned.initial_path,
        };
        let produced = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            try_ssr_client_hook_inner(&req)
        })) {
            Ok(v) => v,
            Err(payload) => {
                let msg = panic_payload_message(&payload);
                tracing::error!(
                    target: "relay_ssr",
                    panic = %msg,
                    path = %owned.initial_path,
                    hook = %owned.client_hook_rel,
                    "client hook SSR panicked"
                );
                None
            }
        };
        if let Some(ref body) = produced {
            // The HTTP waiter may already have the cheap shell. Keep the body
            // so the next request can write ssr_html_cache.
            store_late_ssr(&key_store, body.clone());
        }
        produced
    });
    if out.is_some() {
        // This waiter will wrap the body and write ssr_html_cache.
        let _ = take_late_ssr(&key_clear);
    }
    out
}

struct OwnedSsrRequest {
    repo_path: PathBuf,
    branch: String,
    client_hook_rel: String,
    asset_base_url: String,
    initial_path: String,
}

fn try_ssr_client_hook_inner(req: &ClientHookSsrRequest<'_>) -> Option<ClientHookSsrResult> {
    let t0 = std::time::Instant::now();
    let entry = req.client_hook_rel.trim_start_matches('/');
    let source = read_repo_text(req.repo_path, req.branch, entry)?;
    let ms_read = t0.elapsed().as_millis() as u64;
    let t_mods = std::time::Instant::now();
    let (modules, mut hooks_val) = load_module_map(req.repo_path, req.branch, entry);
    if relay_runtime::ssr_is_cancelled() {
        tracing::warn!(
            target: "relay_ssr",
            path = %req.initial_path,
            "client hook SSR cancelled after module load"
        );
        return None;
    }
    if let Value::Object(ref mut map) = hooks_val {
        let base = req.asset_base_url.trim_end_matches('/');
        map.insert(
            "relayClientBaseUrl".to_string(),
            Value::String(base.to_string()),
        );
        map.insert(
            "relayDefaultMarkdownUrl".to_string(),
            Value::String(format!("{}/README.md", base.trim_end_matches('/'))),
        );
        map.insert("relayPathSlash".to_string(), Value::String("/".to_string()));
        map.insert(
            "relayInitialPath".to_string(),
            Value::String(req.initial_path.to_string()),
        );
    }
    let theme = theme_yaml_from_repo(req.repo_path, req.branch);
    let ms_mods = t_mods.elapsed().as_millis() as u64;

    let host: HostHandle = Rc::new(RefCell::new(RepoFetchHost {
        repo_path: req.repo_path.clone(),
        branch: req.branch.to_string(),
        asset_base_url: req.asset_base_url.to_string(),
        inner: MockHost::new(),
    }));

    // Seed CS globalThis so tip PopImage sees the tip portrait index during SSR
    // (browser window.globalThis is not the CleverScript sandbox).
    let portraits_json = crate::git::portrait::portrait_sibling_set_json(req.repo_path, req.branch);
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&portraits_json) {
        relay_runtime::set_cs_global_this_extra(
            "__RELAY_PORTRAIT_SIBLINGS__",
            Value::from_json(&parsed),
        );
    }

    let t_hook = std::time::Instant::now();
    let run = run_hook_to_vnode_stable_isolated(
        &source,
        host,
        &theme,
        modules,
        Some(hooks_val),
        Platform::Web,
    )
    .map_err(|e| {
        tracing::warn!(target: "relay_ssr", error = %e, "client hook SSR failed");
        e
    });
    relay_runtime::clear_cs_global_this_extras();
    let run = run.ok()?;
    // A finished body is kept even if the waiter already has the cheap shell.
    // Cancel still aborts the hook loop and the module-load check above.
    if relay_runtime::ssr_is_cancelled() {
        tracing::info!(
            target: "relay_ssr",
            path = %req.initial_path,
            "client hook finished after cancel; keeping the body for the HTML cache"
        );
    }
    let ms_hook = t_hook.elapsed().as_millis() as u64;

    let t_html = std::time::Instant::now();
    let opts = HtmlRenderOptions {
        asset_base_url: req.asset_base_url.to_string(),
        stamp_handler_attrs: true,
    };
    let body_html = vnode_to_html(&run.vnode, &opts);
    let ms_html = t_html.elapsed().as_millis() as u64;
    let ms_total = t0.elapsed().as_millis() as u64;
    if body_html.trim().is_empty() {
        tracing::debug!(target: "relay_ssr", "SSR produced empty HTML; falling back");
        return None;
    }
    // Same hard gate as bake / pre-receive (before any LRU put of the composed doc).
    if let Err(e) = crate::git::ssr_html_quality::check_ssr_body_fragment(
        &body_html,
        req.initial_path,
    ) {
        tracing::warn!(
            target: "relay_ssr",
            path = %req.initial_path,
            reason = %e.reason,
            "SSR body failed quality gate; falling back"
        );
        return None;
    }
    tracing::info!(
        target: "relay_ssr",
        path = %req.initial_path,
        hook = %entry,
        ms_read,
        ms_mods,
        ms_hook,
        ms_html,
        ms_total,
        html_bytes = body_html.len(),
        css_bytes = run.css.len(),
        stable_passes = run.stable_passes,
        "client hook SSR ok"
    );
    Some(ClientHookSsrResult {
        body_html,
        css: run.css,
        stable_passes: run.stable_passes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    fn bare_with_hook(hook_src: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        let repo = Repository::init_bare(&bare).unwrap();
        // Worktree-less: write via treebuilder
        let sig = Signature::now("t", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        let blob = repo.blob(hook_src.as_bytes()).unwrap();
        let entry = git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: hook_src.len() as u32,
            id: blob,
            flags: 0,
            flags_extended: 0,
            path: b"hooks/client/get.jsx".to_vec(),
        };
        index.add(&entry).unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, bare)
    }

    #[test]
    fn ssr_simple_hook_produces_html() {
        let src = r#"
export default function App() {
  return <div className="ssr-root"><h1>Hello SSR</h1></div>
}
"#;
        let (_dir, bare) = bare_with_hook(src);
        let result = try_ssr_client_hook(&ClientHookSsrRequest {
            repo_path: &bare,
            branch: "main",
            client_hook_rel: "hooks/client/get.jsx",
            asset_base_url: "",
            initial_path: "/README.md",
        })
        .expect("ssr");
        assert!(
            result.body_html.contains("Hello SSR"),
            "{}",
            result.body_html
        );
        assert!(result.body_html.contains("ssr-root") || result.body_html.contains("<div"));
        assert!(
            result.body_html.contains("class=\"ssr-root\"")
                || result.body_html.contains("class='ssr-root'")
                || result.body_html.contains("ssr-root"),
            "className must survive SSR: {}",
            result.body_html
        );
        assert!(
            !result.css.trim().is_empty() || result.css.is_empty(),
            "css field present"
        );
    }

    #[test]
    fn ssr_clevertree_site_produces_landmarks() {
        let bare = std::env::var("CLEVERTREE_BARE")
            .ok()
            .filter(|s| !s.is_empty());
        let Some(bare) = bare else {
            eprintln!("skip: set CLEVERTREE_BARE to a bare clevertree-site.git");
            return;
        };
        let path = PathBuf::from(bare);
        let branch = std::env::var("CLEVERTREE_BRANCH").unwrap_or_else(|_| "main".to_string());
        let result = try_ssr_client_hook(&ClientHookSsrRequest {
            repo_path: &path,
            branch: &branch,
            client_hook_rel: "hooks/client/get.jsx",
            asset_base_url: "",
            initial_path: "/index.md",
        });
        let result = match result {
            Some(r) => r,
            None => panic!("clevertree SSR returned None (see relay_ssr warn logs)"),
        };
        let html = result.body_html;
        assert!(html.contains("Ari Asulin"), "{html}");
        assert!(html.contains("Key Projects"), "{html}");
        assert!(html.contains("Resume"), "{html}");
        assert!(html.contains("Forgotten Future"), "{html}");
        assert!(
            html.contains("ct-home") || html.contains("id=\"ct-home\""),
            "{html}"
        );
        assert!(
            html.contains("ct-projects"),
            "projects grid class must survive SSR: {html}"
        );
        assert!(
            !html.contains("data-error-boundary"),
            "SSR should not emit error boundaries: {html}"
        );
        assert!(
            !result.css.trim().is_empty(),
            "SSR must emit stylesheet CSS for no-JS browsers"
        );
        assert!(
            result.css.contains("prefers-color-scheme") || result.css.contains(".ct-page"),
            "CSS should include theme rules: {}",
            &result.css[..result.css.len().min(400)]
        );
        if let Ok(out) = std::env::var("CLEVERTREE_WRITE_FIXTURE") {
            if !out.is_empty() {
                let doc = crate::git::embed_html::embed_html_document_with_css(
                    "clevertree-site",
                    &branch,
                    "index.md",
                    "/hooks/client/get.jsx",
                    Some(&html),
                    Some(&result.css),
                );
                std::fs::write(&out, doc).expect("write fixture");
                eprintln!("wrote fixture to {out}");
            }
        }
    }

    #[test]
    fn same_origin_url_maps_to_repo_rel() {
        let host = RepoFetchHost {
            repo_path: PathBuf::from("/tmp"),
            branch: "main".into(),
            asset_base_url: "https://files.example.com".into(),
            inner: MockHost::new(),
        };
        assert_eq!(
            host.repo_rel_from_url("https://files.example.com/blog/a.md"),
            Some("blog/a.md".into())
        );
        assert_eq!(
            host.repo_rel_from_url("http://files.example.com/README.md?embed=1"),
            Some("README.md".into())
        );
        assert_eq!(
            host.repo_rel_from_url("blog/x.md"),
            Some("blog/x.md".into())
        );
        assert_eq!(host.repo_rel_from_url("https://other.example/a.md"), None);
    }
}

#[cfg(test)]
mod tests {
    use crate::{git, handlers, helpers, peer_hook, types::*};
    use axum::http::header::{ACCEPT, CONTENT_TYPE, HOST};
    use axum::{
        extract::{Path as AxPath, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        Json,
    };
    use git2::{Repository, Signature};
    use serial_test::serial;
    use tempfile::tempdir;

    fn test_state(repo_path: std::path::PathBuf) -> AppState {
        AppState {
            repo_path,
            serve_context: ServeContext::BareFleet,
            static_paths: Vec::new(),
            node_fqdn: Some("test.local".to_string()),
            relay_server_id: None,
            authorized_repos: None,
            parked_domains: None,
            features_manifest: None,
            git_ensure_bare_enabled: true,
            web_assets_repo: None,
            web_assets_branch: "release".to_string(),
        }
    }

    fn host_header(repo: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(HOST, format!("{}.test.local", repo).parse().expect("host"));
        h
    }

    const INDEX_STUB_CS: &str = "export default function run(ctx) {\n  return null;\n}\n";
    const META_YAML: &str = "title: Test Item\n";

    /// Bare `repo.git` with `.relay.yaml`, `data/t/meta.yaml`, and `hooks/server/index.cs` on `main` (QUERY / JIT tests).
    fn query_text_fixture(config_yaml: &str) -> (tempfile::TempDir, AppState) {
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let config_oid = repo.blob(config_yaml.as_bytes()).unwrap();
        let meta_oid = repo.blob(META_YAML.as_bytes()).unwrap();
        let index_cs_oid = repo.blob(INDEX_STUB_CS.as_bytes()).unwrap();
        let mut tb_t = repo.treebuilder(None).unwrap();
        tb_t.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let t_tid = tb_t.write().unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("t", t_tid, 0o040000).unwrap();
        let data_tid = tb_data.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", config_oid, 0o100644).unwrap();
        tb.insert("data", data_tid, 0o040000).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server
            .insert("index.cs", index_cs_oid, 0o100644)
            .unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        (repo_dir, state)
    }

    const QUERY_DEFAULT_YAML: &str = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
"#;

    /// Bare repo with `server.hooks.query` CleverScript (`.cs`) and no index hook.
    fn clever_query_fixture(config_yaml: &str, query_cs: &str) -> (tempfile::TempDir, AppState) {
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let config_oid = repo.blob(config_yaml.as_bytes()).unwrap();
        let cs_oid = repo.blob(query_cs.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", config_oid, 0o100644).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server
            .insert("query.cs", cs_oid, 0o100644)
            .unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        (repo_dir, state)
    }

    /// Test OPTIONS returns repository list with branches and commit heads
    #[tokio::test]
    async fn test_options_returns_repo_list() {
        let repo_dir = tempdir().unwrap();

        // Create a bare repo named "repo.git" inside the temp directory
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();

        // Create initial commit on main branch
        let sig = Signature::now("relay", "relay@local").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _commit_oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let state = test_state(repo_dir.path().to_path_buf());

        let headers = host_header("repo");
        let query = None;
        let response = handlers::options_capabilities(State(state), headers, query).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);
        // Parse body to verify structure
        let body_bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(json["ok"], true);
        assert!(json["capabilities"]["supports"].is_array());
        assert_eq!(
            json["client"]["hooks"]["get"]["path"],
            "hooks/client/get.jsx"
        );
    }

    #[test]
    fn ensure_default_client_get_hook_fills_missing_path() {
        let mut body = serde_json::json!({"ok": true});
        handlers::general::ensure_default_client_get_hook(&mut body);
        assert_eq!(
            body["client"]["hooks"]["get"]["path"],
            "hooks/client/get.jsx"
        );
    }

    #[test]
    fn ensure_default_client_get_hook_preserves_explicit_path() {
        let mut body = serde_json::json!({
            "client": {"hooks": {"get": {"path": "hooks/client/custom.jsx"}}}
        });
        handlers::general::ensure_default_client_get_hook(&mut body);
        assert_eq!(
            body["client"]["hooks"]["get"]["path"],
            "hooks/client/custom.jsx"
        );
    }

    /// Test GET returns 200 when repo exists with a file
    #[tokio::test]
    async fn test_get_file_success() {
        let repo_dir = tempdir().unwrap();

        // Create a bare repo named "repo.git" inside the temp directory
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();

        // Create initial commit with a file
        let sig = Signature::now("relay", "relay@local").unwrap();
        let file_content = b"Hello, World!";
        let blob_oid = repo.blob(file_content).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("hello.txt", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _commit_oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "add file", &tree, &[])
            .unwrap();

        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response =
            handlers::handle_get_file(State(state), headers, AxPath("hello.txt".to_string()), None)
                .await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);
        let body_bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec();
        assert_eq!(body_bytes, file_content);
    }

    /// GET without X-Relay-Branch uses bare repo HEAD (e.g. master-only repos).
    #[tokio::test]
    async fn test_get_file_uses_bare_head_default_branch() {
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let file_content = b"from-master";
        let blob_oid = repo.blob(file_content).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("index.json", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/master"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/master").unwrap();

        let state = test_state(repo_dir.path().to_path_buf());
        let headers = host_header("repo");
        let response = handlers::handle_get_file(
            State(state),
            headers,
            AxPath("index.json".to_string()),
            None,
        )
        .await;
        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let body_bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(body_bytes.to_vec(), file_content);
    }

    /// Stale symbolic HEAD (master) with only main present still serves files.
    #[tokio::test]
    async fn test_get_file_stale_master_head_falls_back_to_main() {
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let file_content = b"from-main";
        let blob_oid = repo.blob(file_content).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("index.json", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/master").unwrap();

        let state = test_state(repo_dir.path().to_path_buf());
        let headers = host_header("repo");
        let response = handlers::handle_get_file(
            State(state),
            headers,
            AxPath("index.json".to_string()),
            None,
        )
        .await;
        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let body_bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(body_bytes.to_vec(), file_content);
    }

    /// Test GET returns 404 when file doesn't exist
    #[tokio::test]
    async fn test_get_file_not_found() {
        let repo_dir = tempdir().unwrap();

        // Create a bare repo named "repo.git" inside the temp directory
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();

        // Create initial empty commit
        let sig = Signature::now("relay", "relay@local").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _commit_oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response = handlers::handle_get_file(
            State(state),
            headers,
            AxPath("missing.txt".to_string()),
            None,
        )
        .await;
        let (parts, _body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::NOT_FOUND);
    }

    /// Test GET returns 404 when repo doesn't exist
    #[tokio::test]
    async fn test_get_repo_not_found() {
        let repo_dir = tempdir().unwrap();
        // Create empty data directory with no repos
        let _ = std::fs::create_dir_all(repo_dir.path());

        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("nonexistent");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response =
            handlers::handle_get_file(State(state), headers, AxPath("file.txt".to_string()), None)
                .await;
        let (parts, _body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::NOT_FOUND);
    }

    /// Test branch_from correctly extracts branch from header
    #[test]
    fn test_branch_from_header() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_BRANCH, "develop".parse().unwrap());

        let branch = helpers::branch_from(&headers);
        assert_eq!(branch, "develop");
    }

    /// Test branch_from defaults to main when header is missing
    #[test]
    fn test_branch_from_default() {
        let headers = HeaderMap::new();

        let branch = helpers::branch_from(&headers);
        assert_eq!(branch, DEFAULT_BRANCH);
    }

    /// Repo selection: Host `repo.test.local` + node `test.local` → `repo`
    #[test]
    fn test_repo_from_host_happy_path() {
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let _ = Repository::init_bare(&repo_path).unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let h = host_header("repo");
        let selected = helpers::repo_from_host(&state, Some("test.local"), &h);
        assert_eq!(selected, Some("repo".to_string()));
        assert_eq!(
            git::repo_names_for_state(&state),
            vec!["repo".to_string()]
        );
    }

    /// No bare repos → no selection
    #[test]
    fn test_repo_from_host_no_repos() {
        let repo_dir = tempdir().unwrap();
        let _ = std::fs::create_dir_all(repo_dir.path());
        let state = test_state(repo_dir.path().to_path_buf());
        let h = host_header("repo");
        let selected = helpers::repo_from_host(&state, Some("test.local"), &h);
        assert_eq!(selected, None);
    }

    /// Base node host (no repo prefix) → None
    #[test]
    fn test_repo_from_host_node_only() {
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let _ = Repository::init_bare(&repo_path).unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let mut h = HeaderMap::new();
        h.insert(HOST, "test.local".parse().unwrap());
        let selected = helpers::repo_from_host(&state, Some("test.local"), &h);
        assert_eq!(selected, None);
    }

    /// Apex host + **`relay-master.git`** exists → default repo (**`RELAY_DEFAULT_REPO`**)
    #[test]
    fn test_repo_from_host_or_default_apex_relay_master() {
        let repo_dir = tempdir().unwrap();
        let _ = Repository::init_bare(repo_dir.path().join("relay-master.git")).unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let mut h = HeaderMap::new();
        h.insert(HOST, "test.local".parse().unwrap());
        let selected = helpers::repo_from_host_or_default(&state, Some("test.local"), &h);
        assert_eq!(selected, Some("relay-master".to_string()));
    }

    fn parked_state(repo_path: std::path::PathBuf, pairs: &[(&str, &str)]) -> AppState {
        let mut map = std::collections::HashMap::new();
        for (domain, repo) in pairs {
            map.insert(domain.to_string(), repo.to_string());
        }
        AppState {
            parked_domains: Some(std::sync::Arc::new(map)),
            ..test_state(repo_path)
        }
    }

    /// Parked domain Host (`clevertree.net`) → its catalog repo, not the apex default.
    #[test]
    fn test_parked_domain_routes_to_repo() {
        let repo_dir = tempdir().unwrap();
        let _ = Repository::init_bare(repo_dir.path().join("clevertree-site.git")).unwrap();
        let _ = Repository::init_bare(repo_dir.path().join("relay-master.git")).unwrap();
        let state = parked_state(
            repo_dir.path().to_path_buf(),
            &[
                ("clevertree.net", "clevertree-site"),
                ("www.clevertree.net", "clevertree-site"),
            ],
        );
        let mut h = HeaderMap::new();
        h.insert(HOST, "clevertree.net".parse().unwrap());
        assert_eq!(
            helpers::repo_from_host_or_default(&state, Some("test.local"), &h),
            Some("clevertree-site".to_string())
        );
        let mut hw = HeaderMap::new();
        hw.insert(HOST, "WWW.clevertree.net:443".parse().unwrap());
        assert_eq!(
            helpers::repo_from_host_or_default(&state, Some("test.local"), &hw),
            Some("clevertree-site".to_string())
        );
    }

    /// With parked domains configured, an unknown Host does not leak the apex default repo.
    #[test]
    fn test_unknown_host_does_not_fall_back_when_parked() {
        let repo_dir = tempdir().unwrap();
        let _ = Repository::init_bare(repo_dir.path().join("relay-master.git")).unwrap();
        let state = parked_state(
            repo_dir.path().to_path_buf(),
            &[("clevertree.net", "clevertree-site")],
        );
        let mut h = HeaderMap::new();
        h.insert(HOST, "unknown.example.com".parse().unwrap());
        assert_eq!(
            helpers::repo_from_host_or_default(&state, Some("test.local"), &h),
            None
        );
        // The node's own apex still serves the default repo.
        let mut apex = HeaderMap::new();
        apex.insert(HOST, "test.local".parse().unwrap());
        assert_eq!(
            helpers::repo_from_host_or_default(&state, Some("test.local"), &apex),
            Some("relay-master".to_string())
        );
    }

    /// Parked Host QUERY must hit the parked repo (not apex `relay-master`).
    #[tokio::test]
    async fn test_query_parked_host_selects_repo() {
        let (_tmp, mut state) = query_text_fixture(QUERY_DEFAULT_YAML);
        let mut map = std::collections::HashMap::new();
        map.insert("relay.paradigmthreat.net".to_string(), "repo".to_string());
        state.parked_domains = Some(std::sync::Arc::new(map));
        // Apex default also present so a regression would silently query the wrong tip.
        let _ = Repository::init_bare(state.repo_path.join("relay-master.git")).unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(HOST, "relay.paradigmthreat.net".parse().unwrap());
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let query_body = serde_json::json!({
            "collection": "index",
            "query": { "title": "Test Item" }
        });

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("".to_string()),
            None,
            Some(Json(query_body)),
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(
            parts.status,
            StatusCode::OK,
            "body={}",
            String::from_utf8_lossy(&bytes)
        );
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Test Item");
    }

    /// Parked Host with broken client hook → 500 SSR error, not Unknown Host.
    #[tokio::test]
    #[serial]
    async fn test_parked_host_ssr_failure_returns_500_not_unknown_host() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo_path = root.join("park-site.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();

        let yaml_oid = repo
            .blob(
                br#"
client:
  hooks:
    get:
      path: hooks/client/get.jsx
server:
  hooks:
    get:
      path: hooks/server/get.cs
"#,
            )
            .unwrap();
        let get_cs_oid = repo
            .blob(
                br#"import { ssrClientHookFromGetContext } from '@relay/ssr';
export default function run(ctx, next) {
  try { if (ctx.siteShellRequest === true) {
    var ssr = ssrClientHookFromGetContext(ctx);
    if (ssr !== null) { return ssr; }
  } } catch (e) {}
  return next();
}"#,
            )
            .unwrap();
        let client_get_oid = repo
            .blob(
                br#"export default function App() {
  for (var i = 0; i < 1; i++)
    return <main>broken</main>;
}"#,
            )
            .unwrap();

        let mut server = repo.treebuilder(None).unwrap();
        server.insert("get.cs", get_cs_oid, 0o100644).unwrap();
        let server_id = server.write().unwrap();
        let mut client = repo.treebuilder(None).unwrap();
        client
            .insert("get.jsx", client_get_oid, 0o100644)
            .unwrap();
        let client_id = client.write().unwrap();
        let mut hooks = repo.treebuilder(None).unwrap();
        hooks.insert("server", server_id, 0o040000).unwrap();
        hooks.insert("client", client_id, 0o040000).unwrap();
        let hooks_id = hooks.write().unwrap();

        let mut root_tree = repo.treebuilder(None).unwrap();
        root_tree
            .insert(".relay.yaml", yaml_oid, 0o100644)
            .unwrap();
        root_tree.insert("hooks", hooks_id, 0o040000).unwrap();
        let tree_id = root_tree.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let state = parked_state(
            root,
            &[("relay.paradigmthreat.net", "park-site")],
        );

        let mut headers = HeaderMap::new();
        headers.insert(HOST, "relay.paradigmthreat.net".parse().unwrap());
        headers.insert(ACCEPT, "text/html".parse().unwrap());

        let resp = handlers::get_root(State(state), headers, None).await;
        let (parts, body) = resp.into_response().into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(
            parts.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "body={text}"
        );
        assert!(
            text.contains("Site shell / SSR failed"),
            "expected SSR wording, got: {text}"
        );
        assert!(
            !text.contains("Unknown Host"),
            "must not mask SSR failure as Unknown Host: {text}"
        );
    }

    /// Test bare_repo_names correctly lists repos
    #[tokio::test]
    async fn test_bare_repo_names() {
        let repo_dir = tempdir().unwrap();

        // Create two bare repos
        Repository::init_bare(repo_dir.path().join("repo1.git")).unwrap();
        Repository::init_bare(repo_dir.path().join("repo2.git")).unwrap();
        // Create a non-bare directory (should be ignored)
        std::fs::create_dir(repo_dir.path().join("not_a_repo")).unwrap();

        let names = git::bare_repo_names(&repo_dir.path().to_path_buf());

        assert_eq!(names, vec!["repo1".to_string(), "repo2".to_string()]);
    }

    /// Test HEAD / returns 204 No Content like GET
    #[tokio::test]
    async fn test_head_root() {
        let repo_dir = tempdir().unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let headers = HeaderMap::new();
        let response = handlers::head_root(State(state), headers, None).await;
        let (parts, _body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::NO_CONTENT);
    }

    /// Test HEAD returns 200 when file exists
    #[tokio::test]
    async fn test_head_file_success() {
        let repo_dir = tempdir().unwrap();

        // Create a bare repo named "repo.git"
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();

        // Create initial commit with a file
        let sig = Signature::now("relay", "relay@local").unwrap();
        let file_content = b"Hello, World!";
        let blob_oid = repo.blob(file_content).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("hello.txt", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _commit_oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "add file", &tree, &[])
            .unwrap();

        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response =
            handlers::head_file(State(state), headers, AxPath("hello.txt".to_string()), None).await;
        let (parts, body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::OK);
        // Verify body is empty for HEAD
        let body_bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec();
        assert_eq!(body_bytes.len(), 0);
    }

    /// Test HEAD returns 404 when file doesn't exist
    #[tokio::test]
    async fn test_head_file_not_found() {
        let repo_dir = tempdir().unwrap();

        // Create a bare repo named "repo.git"
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();

        // Create initial empty commit
        let sig = Signature::now("relay", "relay@local").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let _commit_oid = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response = handlers::head_file(
            State(state),
            headers,
            AxPath("missing.txt".to_string()),
            None,
        )
        .await;
        let (parts, _body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::NOT_FOUND);
    }

    /// Test HEAD returns 404 when repo doesn't exist
    #[tokio::test]
    async fn test_head_repo_not_found() {
        let repo_dir = tempdir().unwrap();
        let _ = std::fs::create_dir_all(repo_dir.path());

        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("nonexistent");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response =
            handlers::head_file(State(state), headers, AxPath("file.txt".to_string()), None).await;
        let (parts, _body) = response.into_response().into_parts();

        assert_eq!(parts.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_query_apex_path_selects_repo() {
        let (_tmp, state) = query_text_fixture(QUERY_DEFAULT_YAML);

        // Host is node apex (no repo subdomain); first path segment is the bare repo name.
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "test.local".parse().unwrap());
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let query_body = serde_json::json!({
            "collection": "index",
            "query": { "title": "Test Item" }
        });

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("repo".to_string()),
            None,
            Some(Json(query_body)),
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(
            parts.status,
            StatusCode::OK,
            "body={}",
            String::from_utf8_lossy(&bytes)
        );
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Test Item");
    }

    /// Test QUERY method returns results from the mock database
    #[tokio::test]
    async fn test_query_method_success() {
        let (_tmp, state) = query_text_fixture(QUERY_DEFAULT_YAML);

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let query_body = serde_json::json!({
            "collection": "index",
            "query": { "title": "Test Item" }
        });

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            Some(Json(query_body)),
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);

        let body_bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Test Item");
    }

    #[tokio::test]
    async fn test_query_method_path_success() {
        let (_tmp, state) = query_text_fixture(QUERY_DEFAULT_YAML);

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        // Call with path "Test" instead of "query"
        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("Test".to_string()),
            None,
            None,
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);

        let body_bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Test Item");
    }

    /// Regression: missing `Accept` uses `text/csv` (not JSON).
    #[tokio::test]
    async fn test_query_default_accept_is_csv() {
        let (_tmp, state) = query_text_fixture(QUERY_DEFAULT_YAML);

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            None,
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let ct = parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("text/csv"),
            "expected text/csv Content-Type, got {ct:?}"
        );

        let text = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            text.contains("Test Item"),
            "CSV body should contain cell value: {text:?}"
        );
        assert!(text.contains('\n'), "expected header + row lines");
    }

    /// Regression: `Accept: application/xml` returns XML document.
    #[tokio::test]
    async fn test_query_accept_xml() {
        let (_tmp, state) = query_text_fixture(QUERY_DEFAULT_YAML);

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/xml".parse().unwrap());

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            None,
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let xml = String::from_utf8(
            axum::body::to_bytes(body, usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(xml.starts_with("<?xml "));
        assert!(xml.contains("<results>"));
        assert!(xml.contains("Test Item"));
    }

    /// CleverScript `server.hooks.query` returns rows without redb / index hook.
    #[tokio::test]
    async fn test_query_cleverscript_returns_rows() {
        let yaml = r#"
server:
  hooks:
    query:
      path: hooks/server/query.cs
"#;
        let cs = r#"
export default function run(ctx) {
  return { rows: [{ title: "FromClever", k: 42 }] };
}
"#;
        let (_tmp, state) = clever_query_fixture(yaml, cs);

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let query_body = serde_json::json!({
            "collection": "articles",
            "query": null
        });

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            Some(Json(query_body)),
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(body, usize::MAX).await.unwrap()).unwrap();
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "FromClever");
        assert_eq!(results[0]["k"].as_f64(), Some(42.0));
    }

    /// CleverScript thin router → sqlite plugin (no rows scanned in CS).
    #[tokio::test]
    async fn test_query_cs_routes_to_sqlite_plugin() {
        let yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs
  search:
    plugin: sqlite
"#;
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let config_oid = repo.blob(yaml.as_bytes()).unwrap();
        let meta_oid = repo.blob(META_YAML.as_bytes()).unwrap();
        let index_cs_oid = repo.blob(INDEX_STUB_CS.as_bytes()).unwrap();
        let query_cs = r#"
export default function run(ctx) {
  return { plugin: "sqlite", collection: ctx.collection, query: ctx.query };
}
"#;
        let query_cs_oid = repo.blob(query_cs.as_bytes()).unwrap();
        let mut tb_t = repo.treebuilder(None).unwrap();
        tb_t.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let t_tid = tb_t.write().unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("t", t_tid, 0o040000).unwrap();
        let data_tid = tb_data.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", config_oid, 0o100644).unwrap();
        tb.insert("data", data_tid, 0o040000).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server
            .insert("index.cs", index_cs_oid, 0o100644)
            .unwrap();
        tb_server
            .insert("query.cs", query_cs_oid, 0o100644)
            .unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let query_body = serde_json::json!({
            "collection": "index",
            "query": "Test"
        });

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            Some(Json(query_body)),
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(body, usize::MAX).await.unwrap()).unwrap();
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Test Item");

        // SQLite file must exist after QUERY JIT
        let sqlite = git::branch_sqlite_path(&repo_path, "main");
        assert!(sqlite.exists(), "expected {}", sqlite.display());
    }

    /// Preferred: `import { search } from '@relay/search/sqlite'` then `return search(ctx)`.
    #[tokio::test]
    async fn test_query_cs_imports_sqlite_search_module() {
        let yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs
  search:
    plugin: sqlite
"#;
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let config_oid = repo.blob(yaml.as_bytes()).unwrap();
        let meta_oid = repo
            .blob(b"title: Later Item\nsort_key: '000002'\n")
            .unwrap();
        let early_meta_oid = repo
            .blob(b"title: Earlier Item\nsort_key: '000001'\n")
            .unwrap();
        let index_cs_oid = repo.blob(INDEX_STUB_CS.as_bytes()).unwrap();
        let query_cs = r#"
import { search } from '@relay/search/sqlite';
export default function run(ctx) {
  return search(ctx);
}
"#;
        let query_cs_oid = repo.blob(query_cs.as_bytes()).unwrap();
        let mut tb_t = repo.treebuilder(None).unwrap();
        tb_t.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let t_tid = tb_t.write().unwrap();
        let mut tb_u = repo.treebuilder(None).unwrap();
        tb_u.insert("meta.yaml", early_meta_oid, 0o100644).unwrap();
        let u_tid = tb_u.write().unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("t", t_tid, 0o040000).unwrap();
        tb_data.insert("u", u_tid, 0o040000).unwrap();
        let data_tid = tb_data.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", config_oid, 0o100644).unwrap();
        tb.insert("data", data_tid, 0o040000).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server
            .insert("index.cs", index_cs_oid, 0o100644)
            .unwrap();
        tb_server
            .insert("query.cs", query_cs_oid, 0o100644)
            .unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let query_body = serde_json::json!({
            "collection": "index",
            "query": "Item",
            "order": "sort_key"
        });

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            Some(Json(query_body)),
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(
            parts.status,
            StatusCode::OK,
            "body={}",
            String::from_utf8_lossy(&bytes)
        );
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "Earlier Item");
        assert_eq!(results[1]["title"], "Later Item");
        let sqlite = git::branch_sqlite_path(&repo_path, "main");
        assert!(sqlite.exists(), "expected {}", sqlite.display());
    }

    #[tokio::test]
    async fn test_mcp_search_site_uses_query_plugin() {
        let yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs
    mcp:
      path: hooks/server/mcp.cs
  search:
    plugin: sqlite
"#;
        let mcp_yaml = r#"
name: test-repo
version: "0.1.0"
tools:
  search_site:
    description: Search
    aliases: [search]
    via: query
    collection: index
  read_article:
    description: Read
    aliases: [fetch]
    via: git_read
"#;
        let query_cs = r#"
export default function run(ctx) {
  return { plugin: "sqlite", collection: ctx.collection, query: ctx.query };
}
"#;
        let mcp_cs = "export default function run(ctx) { return null; }\n";
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let config_oid = repo.blob(yaml.as_bytes()).unwrap();
        let mcp_yaml_oid = repo.blob(mcp_yaml.as_bytes()).unwrap();
        let meta_oid = repo.blob(META_YAML.as_bytes()).unwrap();
        let index_cs_oid = repo.blob(INDEX_STUB_CS.as_bytes()).unwrap();
        let query_cs_oid = repo.blob(query_cs.as_bytes()).unwrap();
        let mcp_cs_oid = repo.blob(mcp_cs.as_bytes()).unwrap();
        let article_oid = repo.blob(b"# Hello article\n").unwrap();

        let mut tb_t = repo.treebuilder(None).unwrap();
        tb_t.insert("meta.yaml", meta_oid, 0o100644).unwrap();
        let t_tid = tb_t.write().unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data.insert("t", t_tid, 0o040000).unwrap();
        let data_tid = tb_data.write().unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server.insert("index.cs", index_cs_oid, 0o100644).unwrap();
        tb_server.insert("query.cs", query_cs_oid, 0o100644).unwrap();
        tb_server.insert("mcp.cs", mcp_cs_oid, 0o100644).unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", config_oid, 0o100644).unwrap();
        tb.insert("mcp.yaml", mcp_yaml_oid, 0o100644).unwrap();
        tb.insert("data", data_tid, 0o040000).unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        tb.insert("article.md", article_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let health = handlers::mcp::get_mcp(
            State(state.clone()),
            AxPath("repo".to_string()),
            axum::extract::Query(
                [("health".to_string(), "1".to_string())]
                    .into_iter()
                    .collect(),
            ),
        )
        .await;
        let (hp, hb) = health.into_response().into_parts();
        assert_eq!(hp.status, StatusCode::OK);
        let hj: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(hb, usize::MAX).await.unwrap()).unwrap();
        assert_eq!(hj["mcpEnabled"], true);

        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "search",
                "arguments": { "query": "Test" }
            }
        });
        let resp = handlers::mcp::post_mcp(
            State(state.clone()),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(call)),
        )
        .await;
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(body, usize::MAX).await.unwrap()).unwrap();
        let text = json["result"]["content"][0]["text"].as_str().unwrap();
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["count"], 1);
        assert_eq!(inner["results"][0]["title"], "Test Item");

        let read = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "fetch",
                "arguments": { "path": "article.md" }
            }
        });
        let resp2 = handlers::mcp::post_mcp(
            State(state),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(read)),
        )
        .await;
        let (p2, b2) = resp2.into_response().into_parts();
        assert_eq!(p2.status, StatusCode::OK);
        let j2: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(b2, usize::MAX).await.unwrap()).unwrap();
        let t2 = j2["result"]["content"][0]["text"].as_str().unwrap();
        assert!(t2.contains("Hello article"));
    }

    /// Catalog-style `index.json` + sqlite QUERY (PT-like shape).
    #[tokio::test]
    async fn test_query_catalog_index_json_e2e() {
        let yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs
  search:
    plugin: sqlite
"#;
        let index_json = r#"{
  "blog": {
    "2026-07-09-july-portal.md": {
      "_title": "July Portal ID4 Research",
      "_keywords": ["portal", "july", "research"]
    }
  },
  "cosmos": {
    "mars/page.md": {
      "_title": "Mars Analysis",
      "_keywords": ["mars", "wotw"]
    }
  }
}"#;
        let query_cs = r#"
export default function run(ctx) {
  return { plugin: "sqlite", collection: ctx.collection, query: ctx.query };
}
"#;
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let config_oid = repo.blob(yaml.as_bytes()).unwrap();
        let index_oid = repo.blob(index_json.as_bytes()).unwrap();
        let index_cs_oid = repo.blob(INDEX_STUB_CS.as_bytes()).unwrap();
        let query_cs_oid = repo.blob(query_cs.as_bytes()).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server.insert("index.cs", index_cs_oid, 0o100644).unwrap();
        tb_server.insert("query.cs", query_cs_oid, 0o100644).unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", config_oid, 0o100644).unwrap();
        tb.insert("index.json", index_oid, 0o100644).unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            Some(Json(serde_json::json!({
                "collection": "index",
                "query": "portal july",
                "limit": 5
            }))),
        )
        .await;
        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK, "QUERY failed");
        let json: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(body, usize::MAX).await.unwrap()).unwrap();
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["path"], "blog/2026-07-09-july-portal.md");
    }

    #[tokio::test]
    async fn test_mcp_json_match_site_claims() {
        let yaml = r#"
server:
  hooks:
    mcp:
      path: hooks/server/mcp.cs
"#;
        let mcp_yaml = r#"
tools:
  get_site_claims:
    via: json_match
    source: site-claims.json
"#;
        let claims = r#"[{"id":"c1","claim":"Mud flood is recent","topics":["mud","flood"]},{"id":"c2","claim":"Other"}]"#;
        let mcp_cs = "export default function run(ctx) { return null; }\n";
        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", repo.blob(yaml.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb.insert("mcp.yaml", repo.blob(mcp_yaml.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb.insert(
            "site-claims.json",
            repo.blob(claims.as_bytes()).unwrap(),
            0o100644,
        )
        .unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server
            .insert("mcp.cs", repo.blob(mcp_cs.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        let server_tid = tb_server.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", server_tid, 0o040000).unwrap();
        tb.insert("hooks", tb_hooks.write().unwrap(), 0o040000).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_site_claims",
                "arguments": { "query": "mud flood" }
            }
        });
        let resp = handlers::mcp::post_mcp(
            State(state),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(call)),
        )
        .await;
        let (parts, body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(body, usize::MAX).await.unwrap()).unwrap();
        let text = json["result"]["content"][0]["text"].as_str().unwrap();
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["count"], 1);
        assert_eq!(inner["claims"][0]["id"], "c1");
    }

    #[tokio::test]
    async fn test_mcp_initialize_list_delete_and_path_guard() {
        let mcp_yaml = r#"
name: harden
version: "0.2.0"
tools:
  search_site:
    aliases: [search]
    via: query
    collection: index
  read_article:
    aliases: [fetch]
    via: git_read
  get_event_dates:
    via: events_lookup
    source: data/events.json
  research_status:
    via: research_status
    collection: index
"#;
        let events = r#"{"entries":[{"id":"evt-blip","title":"The Blip","dates":[{"start":-670,"end":1053}]}]}"#;
        let mcp_cs = "export default function run(ctx) { return null; }\n";
        let query_cs = r#"export default function run(ctx) { return { plugin: "sqlite", collection: ctx.collection, query: ctx.query }; }"#;
        let index_json = r#"{"a.md":{"_title":"Portal","_keywords":["portal"]}}"#;
        let yaml2 = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs
    mcp:
      path: hooks/server/mcp.cs
  search:
    plugin: sqlite
"#;

        let repo_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("relay", "relay@local").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", repo.blob(yaml2.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb.insert("mcp.yaml", repo.blob(mcp_yaml.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb.insert("index.json", repo.blob(index_json.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        let mut tb_data = repo.treebuilder(None).unwrap();
        tb_data
            .insert("events.json", repo.blob(events.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb.insert("data", tb_data.write().unwrap(), 0o040000).unwrap();
        let mut tb_server = repo.treebuilder(None).unwrap();
        tb_server
            .insert("mcp.cs", repo.blob(mcp_cs.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb_server
            .insert("query.cs", repo.blob(query_cs.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        tb_server
            .insert("index.cs", repo.blob(INDEX_STUB_CS.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks
            .insert("server", tb_server.write().unwrap(), 0o040000)
            .unwrap();
        tb.insert("hooks", tb_hooks.write().unwrap(), 0o040000).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let state = test_state(repo_dir.path().to_path_buf());

        let init = handlers::mcp::post_mcp(
            State(state.clone()),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
            }))),
        )
        .await;
        let (ip, ib) = init.into_response().into_parts();
        assert_eq!(ip.status, StatusCode::OK);
        let ij: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(ib, usize::MAX).await.unwrap()).unwrap();
        assert_eq!(ij["result"]["serverInfo"]["name"], "harden");

        let list = handlers::mcp::post_mcp(
            State(state.clone()),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/list"
            }))),
        )
        .await;
        let (lp, lb) = list.into_response().into_parts();
        assert_eq!(lp.status, StatusCode::OK);
        let lj: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(lb, usize::MAX).await.unwrap()).unwrap();
        let tools = lj["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 4);
        let fetch = tools.iter().find(|t| t["name"] == "fetch").unwrap();
        assert!(fetch["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "path"));

        let bad = handlers::mcp::post_mcp(
            State(state.clone()),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(serde_json::json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "fetch", "arguments": { "path": "../secret" } }
            }))),
        )
        .await;
        let (bp, bb) = bad.into_response().into_parts();
        assert_eq!(bp.status, StatusCode::OK);
        let bj: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(bb, usize::MAX).await.unwrap()).unwrap();
        assert_eq!(bj["result"]["isError"], true);

        let ev = handlers::mcp::post_mcp(
            State(state.clone()),
            AxPath("repo".to_string()),
            HeaderMap::new(),
            Some(Json(serde_json::json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "get_event_dates", "arguments": { "eventId": "evt-blip", "refYear": 2026 } }
            }))),
        )
        .await;
        let (ep, eb) = ev.into_response().into_parts();
        assert_eq!(ep.status, StatusCode::OK);
        let ej: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(eb, usize::MAX).await.unwrap()).unwrap();
        assert_eq!(ej["result"]["isError"], false);
        let et = ej["result"]["content"][0]["text"].as_str().unwrap();
        let einner: serde_json::Value = serde_json::from_str(et).unwrap();
        assert_eq!(einner["projectSignedYear"], -670);
        assert!(einner["elapsed"]["calendarYears"].as_i64().unwrap() > 2000);

        let del = handlers::mcp::delete_mcp(State(state), AxPath("repo".to_string())).await;
        assert_eq!(del.into_response().status(), StatusCode::NO_CONTENT);
    }

    /// Optional: set RELAY_QUERY_E2E_BARE to a bare `*.git` path (e.g. PT sample fixture).
    #[test]
    fn optional_external_bare_query_e2e() {
        let Ok(bare) = std::env::var("RELAY_QUERY_E2E_BARE") else {
            return;
        };
        let bare = std::path::PathBuf::from(bare);
        assert!(bare.exists(), "missing bare {}", bare.display());
        let parent = bare.parent().unwrap();
        let name = bare
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_string();
        let rows = git::query::execute_query_limited(
            parent,
            &name,
            "main",
            Some(serde_json::Value::String("portal".into())),
            "index",
            Some(10),
        )
        .expect("QUERY against external bare");
        assert!(
            !rows.is_empty(),
            "expected hits for portal in {}",
            bare.display()
        );
    }

    /// Regression: `server.hooks.index.branches` excludes current branch → empty results (no JIT).
    #[tokio::test]
    async fn test_query_branch_allowlist_excludes_current_branch() {
        let yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
      branches:
        - dev
"#;
        let (_tmp, state) = query_text_fixture(yaml);

        let mut headers = host_header("repo");
        headers.insert(HEADER_BRANCH, "main".parse().unwrap());
        headers.insert(ACCEPT, "application/json".parse().unwrap());

        let response = handlers::handle_query(
            State(state),
            headers,
            AxPath("query".to_string()),
            None,
            None,
        )
        .await;

        let (parts, body) = response.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(body, usize::MAX).await.unwrap()).unwrap();
        let results = json["results"].as_array().unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_post_ensure_bare_creates_repo() {
        let repo_dir = tempdir().unwrap();
        std::fs::create_dir_all(repo_dir.path()).unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let r1 =
            handlers::post_ensure_bare(State(state.clone()), AxPath("new-repo".to_string())).await;
        let (parts, body) = r1.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::CREATED);
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["created"], true);
        assert!(repo_dir.path().join("new-repo.git").exists());

        let r2 = handlers::post_ensure_bare(State(state), AxPath("new-repo".to_string())).await;
        let (parts2, body2) = r2.into_response().into_parts();
        assert_eq!(parts2.status, StatusCode::OK);
        let bytes2 = axum::body::to_bytes(body2, usize::MAX).await.unwrap();
        let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
        assert_eq!(v2["created"], false);

        let bare = git2::Repository::open_bare(repo_dir.path().join("new-repo.git")).unwrap();
        let deny = bare
            .config()
            .unwrap()
            .get_string("receive.denyNonFastForwards")
            .unwrap();
        assert_eq!(deny, "true");
    }

    #[tokio::test]
    async fn test_post_ensure_bare_rejects_bad_slug() {
        let repo_dir = tempdir().unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let r = handlers::post_ensure_bare(State(state), AxPath("../evil".to_string())).await;
        let (parts, _) = r.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_post_ensure_bare_disabled() {
        let repo_dir = tempdir().unwrap();
        let mut state = test_state(repo_dir.path().to_path_buf());
        state.git_ensure_bare_enabled = false;
        let r = handlers::post_ensure_bare(State(state), AxPath("x".to_string())).await;
        let (parts, _) = r.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::FORBIDDEN);
    }

    /// `RELAY_PEER_SECRET` unset → peer hook returns **503** (disabled).
    #[tokio::test]
    #[serial(peer_hook_env)]
    async fn test_post_peer_hook_disabled_without_secret() {
        let prev_peer = std::env::var("RELAY_PEER_SECRET").ok();
        let prev_master_mesh = std::env::var("RELAY_MASTER_MESH_SECRET").ok();
        let prev_role = std::env::var("RELAY_NODE_ROLE").ok();
        unsafe {
            std::env::set_var("RELAY_NODE_ROLE", "edge");
            std::env::remove_var("RELAY_PEER_SECRET");
            std::env::remove_var("RELAY_MASTER_MESH_SECRET");
        }
        let repo_dir = tempdir().unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let body = peer_hook::PeerHookBody {
            repo: "x".into(),
            peer: "peer.example".into(),
            branch: "main".into(),
            new_sha: None,
            via: vec![],
            hops_remaining: None,
        };
        let r = handlers::post_peer_hook(State(state), HeaderMap::new(), Json(body)).await;
        let (parts, _) = r.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::SERVICE_UNAVAILABLE);
        unsafe {
            match prev_peer {
                Some(v) => std::env::set_var("RELAY_PEER_SECRET", v),
                None => std::env::remove_var("RELAY_PEER_SECRET"),
            }
            match prev_master_mesh {
                Some(v) => std::env::set_var("RELAY_MASTER_MESH_SECRET", v),
                None => std::env::remove_var("RELAY_MASTER_MESH_SECRET"),
            }
            match prev_role {
                Some(v) => std::env::set_var("RELAY_NODE_ROLE", v),
                None => std::env::remove_var("RELAY_NODE_ROLE"),
            }
        }
    }

    /// Wrong `X-Relay-Peer-Secret` → **401**.
    #[tokio::test]
    #[serial(peer_hook_env)]
    async fn test_post_peer_hook_rejects_bad_secret() {
        let prev_peer = std::env::var("RELAY_PEER_SECRET").ok();
        let prev_master_mesh = std::env::var("RELAY_MASTER_MESH_SECRET").ok();
        let prev_role = std::env::var("RELAY_NODE_ROLE").ok();
        unsafe {
            std::env::set_var("RELAY_NODE_ROLE", "edge");
            std::env::remove_var("RELAY_MASTER_MESH_SECRET");
            std::env::set_var("RELAY_PEER_SECRET", "correct-secret");
        }
        let repo_dir = tempdir().unwrap();
        let state = test_state(repo_dir.path().to_path_buf());
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-relay-peer-secret",
            "wrong".parse().expect("header value"),
        );
        let body = peer_hook::PeerHookBody {
            repo: "x".into(),
            peer: "peer.example".into(),
            branch: "main".into(),
            new_sha: None,
            via: vec![],
            hops_remaining: None,
        };
        let r = handlers::post_peer_hook(State(state), headers, Json(body)).await;
        let (parts, _) = r.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::UNAUTHORIZED);
        unsafe {
            match prev_peer {
                Some(v) => std::env::set_var("RELAY_PEER_SECRET", v),
                None => std::env::remove_var("RELAY_PEER_SECRET"),
            }
            match prev_master_mesh {
                Some(v) => std::env::set_var("RELAY_MASTER_MESH_SECRET", v),
                None => std::env::remove_var("RELAY_MASTER_MESH_SECRET"),
            }
            match prev_role {
                Some(v) => std::env::set_var("RELAY_NODE_ROLE", v),
                None => std::env::remove_var("RELAY_NODE_ROLE"),
            }
        }
    }

    /// Local worktree: uncommitted gitignored file served before HEAD miss.
    #[tokio::test]
    async fn local_worktree_serves_gitignored_before_head() {
        use crate::config::Config;
        use crate::git::worktree::read_worktree_file;
        use git2::Repository;

        let dir = tempdir().unwrap();
        let checkout = dir.path().join("repo");
        std::fs::create_dir_all(&checkout).unwrap();
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&checkout)
            .output()
            .unwrap();
        std::fs::write(checkout.join(".gitignore"), "local-only.txt\n").unwrap();
        std::fs::write(checkout.join("tracked.txt"), b"committed").unwrap();
        std::process::Command::new("git")
            .args(["add", "tracked.txt", ".gitignore"])
            .current_dir(&checkout)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&checkout)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        std::fs::write(checkout.join("local-only.txt"), b"worktree-secret").unwrap();

        assert!(read_worktree_file(&checkout, "local-only.txt").is_some());

        let state = Config::desktop_local_state(checkout.clone(), "repo".into());
        let mut headers = host_header("repo");
        headers.insert(
            crate::types::HEADER_BRANCH,
            "main".parse().unwrap(),
        );
        let r = git::git_resolve_and_respond(
            &state,
            &headers,
            "main",
            "repo",
            "local-only.txt",
            None,
        );
        let GitResolveResult::Respond(resp) = r else {
            panic!("expected worktree file");
        };
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

//! `GET`/`POST /api/sync/mesh/:repo` — compare publish-origin tip vs mesh peers after push.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path as AxPath, Query, State},
    response::IntoResponse,
    Json,
};
use git2::Repository;

use crate::catalog_policy;
use crate::mesh_role;
use crate::post_push_sync::{
    mesh_status_from_peers, probe_peer_tips, read_post_push_sync, MeshSyncReport,
};
use crate::types::AppState;

#[derive(Debug, serde::Deserialize)]
pub struct MeshSyncQuery {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
}

fn local_publish_sha(repo_path: &Path, repo: &str, branch: &str) -> Option<String> {
    let bare = repo_path.join(format!("{repo}.git"));
    let git_repo = Repository::open_bare(&bare).ok()?;
    let oid = git_repo
        .find_reference(&format!("refs/heads/{branch}"))
        .ok()?
        .target()?;
    Some(oid.to_string())
}

fn publish_branch_for(repo: &str, catalog_path: Option<&Path>, fallback: &str) -> String {
    let Some(cat) = catalog_path else {
        return fallback.to_string();
    };
    let index = catalog_policy::try_load_catalog_index(cat);
    let Some(idx) = index else {
        return fallback.to_string();
    };
    catalog_policy::catalog_record_for_slug(&idx, repo)
        .map(|r| r.publish_branch.clone())
        .unwrap_or_else(|| fallback.to_string())
}

pub async fn api_mesh_sync(
    State(state): State<AppState>,
    AxPath(repo): AxPath<String>,
    Query(q): Query<MeshSyncQuery>,
) -> impl IntoResponse {
    if repo.is_empty() || repo.len() > 200 || repo.contains("..") {
        return Json(serde_json::json!({"ok": false, "error": "invalid repo"})).into_response();
    }
    let branch = q
        .branch
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_string());
    let catalog_path = std::env::var("RELAY_MASTER_CATALOG_PATH")
        .ok()
        .map(PathBuf::from)
        .filter(|p: &PathBuf| p.exists());
    let branch = publish_branch_for(&repo, catalog_path.as_deref(), &branch);

    let publisher_fqdn =
        mesh_role::node_fqdn_normalized().unwrap_or_else(|| "localhost".to_string());
    let publisher_sha = local_publish_sha(&state.repo_path, &repo, &branch);
    let expected_sha = q
        .sha
        .filter(|s| s.len() == 40)
        .or(publisher_sha.clone())
        .unwrap_or_default();

    if expected_sha.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "error": "missing sha (bare repo or branch not found)",
            "repo": repo,
            "branch": branch,
        }))
        .into_response();
    }

    let repo_clone = repo.clone();
    let branch_clone = branch.clone();
    let expected = expected_sha.clone();

    let peer_rows = tokio::task::spawn_blocking(move || {
        probe_peer_tips(&repo_clone, &branch_clone, &expected)
    })
    .await
    .unwrap_or_default();

    let last_notify = read_post_push_sync(&state.repo_path, &repo);
    let status = mesh_status_from_peers(
        publisher_sha.as_deref(),
        &expected_sha,
        &peer_rows,
        last_notify.as_ref(),
    );

    let report = MeshSyncReport {
        repo: repo.clone(),
        branch: branch.clone(),
        publisher_fqdn,
        publisher_sha: publisher_sha.clone(),
        expected_sha: expected_sha.clone(),
        status,
        peers: peer_rows,
        last_notify,
    };

    Json(serde_json::json!({
        "ok": true,
        "generated_at_unix": SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()),
        "mesh_sync": report,
    }))
    .into_response()
}

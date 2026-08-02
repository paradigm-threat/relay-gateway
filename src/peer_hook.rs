//! `POST /hooks/peer` — trusted peer notifies this node to **`git fetch`** from **`git://peer:port/repo.git`**.
//! See `docs/RELAY_MESH_NETWORKING_PLAN.md`.

use std::path::Path;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use git2::Repository;
use serde::Deserialize;

use crate::authorized_repos;
use crate::mesh_events;
use crate::mesh_role::{self, RelayNodeRole};
use crate::notify;
use crate::types::AppState;
use std::fmt;

/// Fetch / policy outcome for **`run_peer_fetch`** (distinct from transport errors).
#[derive(Debug, Clone)]
pub enum PeerFetchError {
    /// Trust anchor check failed after fetch (`validate_anchor`).
    PullPolicyValidation {
        repo: String,
        branch: String,
        message: String,
        after_tip: Option<String>,
        trigger_peer: Option<String>,
    },
    Other(String),
}

impl fmt::Display for PeerFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerFetchError::PullPolicyValidation { message, .. } => write!(f, "{}", message),
            PeerFetchError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl PeerFetchError {
    pub fn user_message(&self) -> String {
        match self {
            PeerFetchError::PullPolicyValidation { message, .. } => message.clone(),
            PeerFetchError::Other(s) => s.clone(),
        }
    }

    pub fn is_pull_policy_validation(&self) -> bool {
        matches!(self, PeerFetchError::PullPolicyValidation { .. })
    }
}

/// JSON body for `POST /hooks/peer`.
#[derive(Debug, Clone, Deserialize)]
pub struct PeerHookBody {
    pub repo: String,
    /// Git peer hostname (no scheme, no path). Fetched from `git://{peer}:{port}/{repo}.git`.
    pub peer: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// When set, if `refs/heads/{branch}` already points here, skip fetch (idempotent notify).
    #[serde(default)]
    pub new_sha: Option<String>,
    /// Hosts already notified (loop guard for ring hop-forward).
    #[serde(default)]
    pub via: Vec<String>,
    /// Remaining ring hops after this fetch (decremented per hop).
    #[serde(default)]
    pub hops_remaining: Option<u32>,
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Debug, serde::Serialize)]
pub struct PeerHookResponse {
    pub ok: bool,
    pub skipped: bool,
    pub repo: String,
    pub peer: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn validate_repo_slug(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    let base = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    if base.is_empty() || base.len() > 200 {
        return Err("invalid repo name length");
    }
    if base.starts_with('.') {
        return Err("repo name may not start with '.'");
    }
    if !base
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("repo name must be ASCII letters, digits, '-', '_', or '.'");
    }
    if base.contains("..") {
        return Err("repo name may not contain '..'");
    }
    Ok(base.to_string())
}

/// Hostname or FQDN only (no port, no URL). Conservative allowlist for `git://` construction.
pub fn validate_peer_host(raw: &str) -> Result<String, &'static str> {
    let s = raw.trim();
    if s.is_empty() || s.len() > 253 {
        return Err("invalid peer host length");
    }
    if s.contains("://") || s.contains('/') || s.contains('@') || s.contains(':') {
        return Err("peer must be a hostname only (no scheme, port, path, or user)");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err("peer host contains invalid characters");
    }
    if s.starts_with('.') || s.ends_with('.') || s.contains("..") {
        return Err("invalid peer host");
    }
    Ok(s.to_ascii_lowercase())
}

fn constant_time_eq_str(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    if ab.len() != bb.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in ab.iter().zip(bb.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

pub fn peer_secret_ok(provided_header: Option<&str>, expected: &str) -> bool {
    let Some(p) = provided_header.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    constant_time_eq_str(p, expected)
}

/// If `RELAY_MASTER_PEER_LIST` is non-empty, `peer` must match one entry (case-insensitive).
pub fn peer_in_configured_list(peer: &str) -> bool {
    let raw = std::env::var("RELAY_MASTER_PEER_LIST").unwrap_or_default();
    let items: Vec<String> = raw
        .split(';')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if items.is_empty() {
        return true;
    }
    let p = peer.trim().to_ascii_lowercase();
    items.iter().any(|x| x == &p)
}

fn normalize_new_sha(raw: &str) -> Result<String, &'static str> {
    let t = raw.trim();
    if t.len() != 40 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("new_sha must be 40 hex characters");
    }
    Ok(t.to_ascii_lowercase())
}

/// Build `git://` URL for relay git daemon (default port from env).
pub fn peer_git_url(peer: &str, repo_slug: &str, port: u16) -> String {
    format!("git://{}:{}/{}.git", peer, port, repo_slug)
}

/// Fetch `branch` from `remote_url` into bare repo at `bare_path`. Validates anchor when `cfg` is set.
pub fn fetch_from_peer_url(
    bare_path: &Path,
    remote_url: &str,
    repo_name: &str,
    branch: &str,
    cfg: Option<&authorized_repos::AuthorizedReposFile>,
) -> Result<PeerHookResponse, PeerFetchError> {
    let timeout_secs = std::env::var("RELAY_PEER_FETCH_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(60);

    let bare_path = bare_path.to_path_buf();
    let remote_url = remote_url.to_string();
    let repo_name = repo_name.to_string();
    let branch = branch.to_string();
    // Clone authorized config into owned data for the worker thread when present.
    let cfg_owned = cfg.cloned();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = fetch_from_peer_url_inner(
            &bare_path,
            &remote_url,
            &repo_name,
            &branch,
            cfg_owned.as_ref(),
        );
        let _ = tx.send(r);
    });

    match rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(PeerFetchError::Other(format!(
            "peer fetch timed out after {}s",
            timeout_secs
        ))),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(PeerFetchError::Other(
            "peer fetch worker disconnected".into(),
        )),
    }
}

fn fetch_from_peer_url_inner(
    bare_path: &Path,
    remote_url: &str,
    repo_name: &str,
    branch: &str,
    cfg: Option<&authorized_repos::AuthorizedReposFile>,
) -> Result<PeerHookResponse, PeerFetchError> {
    let branch = branch.trim();
    if branch.is_empty() || branch.contains("..") || branch.contains('/') {
        return Err(PeerFetchError::Other("invalid branch".into()));
    }

    let repo = Repository::open_bare(bare_path)
        .map_err(|e| PeerFetchError::Other(format!("open bare repo: {}", e)))?;

    let refname = format!("refs/heads/{}", branch);
    let before = repo.refname_to_id(&refname).ok().map(|o| o.to_string());

    let mut remote = repo
        .remote_anonymous(remote_url)
        .map_err(|e| PeerFetchError::Other(format!("remote_anonymous: {}", e)))?;

    // No `+` prefix: fast-forward fetch only (force would bypass append-only / signing guarantees).
    let refspec = format!("refs/heads/{}:refs/heads/{}", branch, branch);
    remote
        .fetch(&[refspec.as_str()], None, None)
        .map_err(|e| PeerFetchError::Other(format!("fetch: {}", e)))?;

    let after = repo.refname_to_id(&refname).ok().map(|o| o.to_string());

    if let Some(ref c) = cfg {
        if let Err(msg) = authorized_repos::validate_anchor(&repo, repo_name, c) {
            authorized_repos::rollback_branch(&repo, branch, before.as_deref());
            return Err(PeerFetchError::PullPolicyValidation {
                repo: repo_name.to_string(),
                branch: branch.to_string(),
                message: msg,
                after_tip: after,
                trigger_peer: None,
            });
        }
    }

    if let Some(ref a) = after {
        let old_sid = before
            .as_deref()
            .unwrap_or("0000000000000000000000000000000000000000");
        if let Err(msg) = crate::git::enforce_push_range(bare_path, old_sid, a, branch) {
            authorized_repos::rollback_branch(&repo, branch, before.as_deref());
            return Err(PeerFetchError::Other(format!(
                "peer fetch signing policy: {}",
                msg
            )));
        }
    }

    Ok(PeerHookResponse {
        ok: true,
        skipped: false,
        repo: repo_name.to_string(),
        peer: String::new(),
        branch: branch.to_string(),
        before,
        after,
        error: None,
    })
}

/// Full handler logic (blocking). `peer_display` is echoed in the response.
pub fn run_peer_fetch(
    repo_root: &Path,
    body: &PeerHookBody,
    cfg: Option<&authorized_repos::AuthorizedReposFile>,
    relay_server_id: Option<&str>,
    git_port: u16,
) -> Result<PeerHookResponse, PeerFetchError> {
    let slug = validate_repo_slug(&body.repo).map_err(|e| PeerFetchError::Other(e.to_string()))?;
    let peer = validate_peer_host(&body.peer).map_err(|e| PeerFetchError::Other(e.to_string()))?;

    if !peer_in_configured_list(&peer) {
        return Err(PeerFetchError::Other(format!(
            "peer '{}' is not listed in RELAY_MASTER_PEER_LIST",
            peer
        )));
    }

    if let Some(ref c) = cfg {
        authorized_repos::ensure_pull_allowed(c, relay_server_id, &slug)
            .map_err(PeerFetchError::Other)?;
    }

    let bare_path = repo_root.join(format!("{}.git", slug));
    if !bare_path.is_dir() {
        return Err(PeerFetchError::Other(format!(
            "bare repository {}.git not found",
            slug
        )));
    }

    let branch = body.branch.trim();
    let branch = if branch.is_empty() { "main" } else { branch };

    if let Some(ref c) = cfg {
        if let Some(anchor) = c.repos.get(&slug) {
            if anchor.branch != branch {
                return Err(PeerFetchError::Other(format!(
                    "branch mismatch: hook requested '{}', policy requires '{}' for repo '{}'",
                    branch, anchor.branch, slug
                )));
            }
        }
    }

    if let Some(ref sha_raw) = body.new_sha {
        let want = normalize_new_sha(sha_raw).map_err(|e| PeerFetchError::Other(e.to_string()))?;
        let repo =
            Repository::open_bare(&bare_path).map_err(|e| PeerFetchError::Other(e.to_string()))?;
        let refname = format!("refs/heads/{}", branch);
        if let Ok(oid) = repo.refname_to_id(&refname) {
            if oid.to_string().to_ascii_lowercase() == want {
                return Ok(PeerHookResponse {
                    ok: true,
                    skipped: true,
                    repo: slug,
                    peer: peer.clone(),
                    branch: branch.to_string(),
                    before: Some(want.clone()),
                    after: Some(want),
                    error: None,
                });
            }
        }
    }

    let url = peer_git_url(&peer, &slug, git_port);
    match fetch_from_peer_url(&bare_path, &url, &slug, branch, cfg) {
        Ok(mut res) => {
            res.peer = peer;
            Ok(res)
        }
        Err(PeerFetchError::PullPolicyValidation {
            repo,
            branch,
            message,
            after_tip,
            trigger_peer: _,
        }) => Err(PeerFetchError::PullPolicyValidation {
            repo,
            branch,
            message,
            after_tip,
            trigger_peer: Some(peer),
        }),
        Err(e) => Err(e),
    }
}

/// `POST /hooks/peer` — header **`X-Relay-Peer-Secret`**:
/// - **Master** with **`RELAY_MASTER_MESH_SECRET`**: must match that (inter-master inject).
/// - **Master** without master mesh secret: **`RELAY_PEER_SECRET`** (legacy).
/// - **Edge**: **`RELAY_PEER_SECRET`** only.
///
/// When **`RELAY_MASTER_PEER_LIST`** is non-empty, **`peer`** in the JSON body must appear in that list.
pub async fn post_peer_hook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PeerHookBody>,
) -> impl IntoResponse {
    let role = mesh_role::relay_node_role();
    let master_mesh = std::env::var("RELAY_MASTER_MESH_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let peer_secret = std::env::var("RELAY_PEER_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let hook_disabled = match role {
        RelayNodeRole::Master => master_mesh.is_none() && peer_secret.is_none(),
        RelayNodeRole::Edge => peer_secret.is_none(),
    };
    if hook_disabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(PeerHookResponse {
                ok: false,
                skipped: false,
                repo: String::new(),
                peer: String::new(),
                branch: String::new(),
                before: None,
                after: None,
                error: Some(
                    "peer hook disabled (edge: set RELAY_PEER_SECRET; master: set RELAY_MASTER_MESH_SECRET and/or RELAY_PEER_SECRET)"
                        .into(),
                ),
            }),
        )
            .into_response();
    }

    let provided = headers
        .get("x-relay-peer-secret")
        .and_then(|v| v.to_str().ok());
    let authorized = match role {
        RelayNodeRole::Master => {
            if let Some(ref ms) = master_mesh {
                peer_secret_ok(provided, ms)
            } else {
                peer_secret
                    .as_ref()
                    .is_some_and(|ps| peer_secret_ok(provided, ps))
            }
        }
        RelayNodeRole::Edge => peer_secret
            .as_ref()
            .is_some_and(|ps| peer_secret_ok(provided, ps)),
    };
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            Json(PeerHookResponse {
                ok: false,
                skipped: false,
                repo: String::new(),
                peer: String::new(),
                branch: String::new(),
                before: None,
                after: None,
                error: Some("invalid X-Relay-Peer-Secret".into()),
            }),
        )
            .into_response();
    }

    let port = std::env::var("RELAY_GIT_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(9418u16);

    let root = state.repo_path.clone();
    let authorized = state.authorized_repos.clone();
    let sid = state.relay_server_id.clone();
    let node_fqdn = state.node_fqdn.clone();
    let repo_err = body.repo.clone();
    let peer_err = body.peer.clone();
    let branch_err = body.branch.clone();
    let via = body.via.clone();
    let hops_remaining = body.hops_remaining;

    let res = tokio::task::spawn_blocking(move || {
        let cfg = authorized.as_deref();
        run_peer_fetch(&root, &body, cfg, sid.as_deref(), port)
    })
    .await;

    let Ok(inner) = res else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(PeerHookResponse {
                ok: false,
                skipped: false,
                repo: String::new(),
                peer: String::new(),
                branch: String::new(),
                before: None,
                after: None,
                error: Some("internal task join failed".into()),
            }),
        )
            .into_response();
    };

    match inner {
        Ok(r) => {
            if r.ok && !r.skipped && r.after.is_some() && r.before.as_ref() != r.after.as_ref() {
                if let Some(ref sha) = r.after {
                    notify::spawn_commit_update_notify(
                        "peer_fetch",
                        r.repo.clone(),
                        r.branch.clone(),
                        sha.clone(),
                        state.relay_server_id.clone(),
                        state.node_fqdn.clone(),
                    );
                    let bare = state.repo_path.join(format!("{}.git", r.repo));
                    if crate::git::catalog_apply::is_relay_master_slug(&r.repo) {
                        crate::git::catalog_apply::handle_relay_master_peer_fetch(
                            &bare,
                            r.before.as_deref(),
                            sha,
                            &r.branch,
                            &via,
                            hops_remaining,
                        );
                    } else {
                        crate::git::spawn_ensure_indexed(
                            bare.clone(),
                            r.branch.clone(),
                            sha.clone(),
                        );
                        if let Some(git) =
                            crate::peer_notify::git_config_from_bare_tip(&bare, &r.branch, sha)
                        {
                            let mut next_via = via.clone();
                            if let Some(self_host) = node_fqdn.as_deref() {
                                next_via.push(self_host.to_string());
                            }
                            let next_hops = hops_remaining.map(|h| h.saturating_sub(1));
                            if next_hops.unwrap_or(0) > 0 {
                                crate::peer_notify::spawn_hop_forward(
                                    r.repo.clone(),
                                    r.branch.clone(),
                                    sha.clone(),
                                    git,
                                    next_via,
                                    next_hops,
                                );
                            }
                        }
                    }
                }
            }
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(PeerFetchError::PullPolicyValidation {
            repo,
            branch,
            message,
            after_tip,
            trigger_peer,
        }) => {
            mesh_events::spawn_publish_pull_validation_failure(
                mesh_events::PullValidationPublishCtx {
                    source: "peer_hook".into(),
                    repo: repo.clone(),
                    branch: branch.clone(),
                    error: message.clone(),
                    relay_server_id: state.relay_server_id.clone(),
                    node_fqdn: state.node_fqdn.clone(),
                    after_tip,
                    trigger_peer,
                },
            );
            (
                StatusCode::FORBIDDEN,
                Json(PeerHookResponse {
                    ok: false,
                    skipped: false,
                    repo,
                    peer: peer_err,
                    branch,
                    before: None,
                    after: None,
                    error: Some(message),
                }),
            )
                .into_response()
        }
        Err(PeerFetchError::Other(msg)) if msg.contains("peer fetch signing policy") => {
            mesh_events::spawn_publish_peer_signing_policy_failure(
                mesh_events::PullValidationPublishCtx {
                    source: "peer_hook".into(),
                    repo: repo_err.clone(),
                    branch: branch_err.clone(),
                    error: msg.clone(),
                    relay_server_id: state.relay_server_id.clone(),
                    node_fqdn: state.node_fqdn.clone(),
                    after_tip: None,
                    trigger_peer: Some(peer_err.clone()),
                },
            );
            (
                StatusCode::FORBIDDEN,
                Json(PeerHookResponse {
                    ok: false,
                    skipped: false,
                    repo: repo_err,
                    peer: peer_err,
                    branch: branch_err,
                    before: None,
                    after: None,
                    error: Some(msg),
                }),
            )
                .into_response()
        }
        Err(PeerFetchError::Other(msg)) => {
            let status = if msg.contains("not found") || msg.contains("invalid") {
                StatusCode::BAD_REQUEST
            } else if msg.contains("not listed") || msg.contains("not in authorized") {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(PeerHookResponse {
                    ok: false,
                    skipped: false,
                    repo: repo_err,
                    peer: peer_err,
                    branch: branch_err,
                    before: None,
                    after: None,
                    error: Some(msg),
                }),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn validate_peer_host_rejects_url() {
        assert!(validate_peer_host("evil.com:22").is_err());
        assert!(validate_peer_host("//x").is_err());
        assert!(validate_peer_host("a@b").is_err());
        assert_eq!(
            validate_peer_host("Atlanta1.EXAMPLE.net").unwrap(),
            "atlanta1.example.net"
        );
    }

    #[test]
    fn peer_git_url_format() {
        assert_eq!(
            peer_git_url("n.example", "r", 9418),
            "git://n.example:9418/r.git"
        );
    }

    #[test]
    fn peer_secret_constant_time_shape() {
        assert!(!peer_secret_ok(None, "secret"));
        assert!(!peer_secret_ok(Some(""), "secret"));
        assert!(peer_secret_ok(Some("secret"), "secret"));
        assert!(!peer_secret_ok(Some("secres"), "secret"));
        assert!(!peer_secret_ok(Some("Secret"), "secret"));
    }

    #[test]
    fn fetch_from_file_url_updates_bare() {
        let td = tempdir().unwrap();
        let upstream = td.path().join("upstream.git");
        let downstream = td.path().join("downstream.git");

        let u = Repository::init_bare(&upstream).unwrap();
        let sig = Signature::now("a", "a@a").unwrap();
        let relay_yaml = br#"git:
  branchRules:
    branches:
      - name: "*"
        rule:
          allowUnsigned: true
          requireSigned: false
"#;
        let blob = u.blob(relay_yaml).unwrap();
        let mut tb = u.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", blob, 0o100644).unwrap();
        let t = tb.write().unwrap();
        let tree = u.find_tree(t).unwrap();
        u.commit(Some("refs/heads/main"), &sig, &sig, "u", &tree, &[])
            .unwrap();

        let _d = Repository::init_bare(&downstream).unwrap();

        // Anonymous fetch from file:// (unit test only; production uses git://).
        let url = format!("file://{}", upstream.display());
        let mut cfg = authorized_repos::AuthorizedReposFile {
            relay_server_id: None,
            repos: HashMap::new(),
        };
        let root = u
            .refname_to_id("refs/heads/main")
            .expect("main tip")
            .to_string();
        cfg.repos.insert(
            "downstream".into(),
            authorized_repos::RepoAnchor {
                anchor_commit: root,
                branch: "main".into(),
            },
        );

        let r = fetch_from_peer_url(&downstream, &url, "downstream", "main", Some(&cfg)).unwrap();
        assert!(r.ok);
        assert!(!r.skipped);
        assert!(r.after.is_some());
    }
}

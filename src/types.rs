use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Legacy response header (informational only; **not** used for repo selection).
pub const HEADER_REPO: &str = "X-Relay-Repo";
pub const HEADER_BRANCH: &str = "X-Relay-Branch";
pub const DEFAULT_BRANCH: &str = "main";
pub const DEFAULT_IPFS_CACHE_ROOT: &str = "/tmp/ipfs-cache";

/// How GET resolves repo content: fleet bare repos vs desktop live checkout.
#[derive(Clone, Default)]
pub enum ServeContext {
    /// Fleet gateway: bare repos under `repo_path` (`name.git`).
    #[default]
    BareFleet,
    /// Desktop `--repo`: serve live working tree, then branch HEAD from checkout git.
    LocalWorktree {
        checkout: PathBuf,
        repo_name: String,
        /// Branch checked out on disk (`git rev-parse --abbrev-ref HEAD` at session start).
        checked_out_branch: String,
    },
}

#[derive(Clone)]
pub struct AppState {
    /// Directory containing bare repos (`name.git`). For `LocalWorktree`, the checkout root.
    pub repo_path: PathBuf,
    pub serve_context: ServeContext,
    pub static_paths: Vec<PathBuf>,
    /// Node FQDN for HTTP routing: `{repo}.{node_fqdn}` (e.g. `atlanta1.relaygateway.net`).
    /// Set via **`RELAY_PUBLIC_HOSTNAME`** or **`RELAY_NODE_FQDN`**.
    pub node_fqdn: Option<String>,
    /// Unique server name; required when authorized pull list is configured.
    pub relay_server_id: Option<String>,
    /// When set, git-pull only for listed repos and anchor validation after fetch.
    pub authorized_repos: Option<Arc<crate::authorized_repos::AuthorizedReposFile>>,
    /// Parked-domain routing map (`Host` → repo slug), materialized from the catalog `domains[]`.
    /// A request `Host` matching a key is served by that repo instead of the apex default.
    pub parked_domains: Option<Arc<std::collections::HashMap<String, String>>>,
    /// Written by relay-install.sh (`state/features.json`); exposed in `POST /api/config`.
    pub features_manifest: Option<Arc<serde_json::Value>>,
    /// When true, `POST /api/git/ensure-bare/:repo` may create `RELAY_REPO_PATH/<repo>.git` so anonymous `git push` over git:// succeeds.
    pub git_ensure_bare_enabled: bool,
}

#[derive(Deserialize, Debug)]
pub struct RulesDoc {
    pub rules: Vec<String>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct RelayConfig {
    #[serde(default)]
    pub client: Option<ClientConfig>,
    #[serde(default)]
    pub server: Option<ServerConfig>,
    #[serde(default)]
    pub git: Option<GitConfig>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct ServerConfig {
    pub hooks: Option<std::collections::HashMap<String, HookPath>>,
    /// Search plugin selection for QUERY / MCP search tools (default: sqlite).
    #[serde(default)]
    pub search: Option<SearchConfig>,
    /// Opt-in TLS plugin for parked domains (e.g. `park` for shared HTTP-01 + cert fan-out).
    #[serde(default)]
    pub tls: Option<TlsConfig>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct SearchConfig {
    /// Plugin id: `sqlite` (default) or `redb` (legacy).
    #[serde(default)]
    pub plugin: Option<String>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct TlsConfig {
    /// Plugin id: `park` enables coordinated multi-node ACME for catalog `domains[]`.
    #[serde(default)]
    pub plugin: Option<String>,
}

impl RelayConfig {
    /// True when this repo opts into the shared parked-domain TLS plugin.
    pub fn has_park_tls_plugin(&self) -> bool {
        self.server
            .as_ref()
            .and_then(|s| s.tls.as_ref())
            .and_then(|t| t.plugin.as_deref())
            .map(|p| p.eq_ignore_ascii_case("park"))
            .unwrap_or(false)
    }
}

#[derive(Deserialize, Debug, Default, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum NotifyMode {
    #[default]
    Next,
    All,
}

#[derive(Deserialize, Debug, Serialize, Clone, PartialEq)]
pub struct GitOrigin {
    pub name: Option<String>,
    /// `relay` (mesh peer) is the supported product path.
    #[serde(default = "default_origin_kind")]
    pub kind: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub fetch: Option<bool>,
    #[serde(default)]
    pub notify: Option<bool>,
}

impl Default for GitOrigin {
    fn default() -> Self {
        Self {
            name: None,
            kind: default_origin_kind(),
            host: None,
            url: None,
            fetch: None,
            notify: None,
        }
    }
}

fn default_origin_kind() -> String {
    "relay".to_string()
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct GitConfig {
    /// Mesh tip-notify: `next` (ring) or `all` (fan-out). Default `next`.
    #[serde(rename = "notifyMode", default)]
    pub notify_mode: NotifyMode,
    #[serde(default)]
    pub origins: Option<Vec<GitOrigin>>,
    #[serde(rename = "autoPush")]
    pub auto_push: Option<AutoPushConfig>,
    #[serde(rename = "branchRules")]
    pub branch_rules: Option<BranchRulesConfig>,
    pub github: Option<GithubHooksConfig>,
    /// Declared allowlist of Relay server IDs and key fingerprints (see docs/RELAY_TRUST_AND_BOOTSTRAP.md).
    #[serde(rename = "relayTrust")]
    pub relay_trust: Option<RelayTrustConfig>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct BranchRulesConfig {
    pub default: Option<BranchRule>,
    pub branches: Option<Vec<BranchRuleNamed>>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct BranchRuleNamed {
    pub name: String,
    pub rule: BranchRule,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct BranchRule {
    #[serde(rename = "requireSigned")]
    pub require_signed: Option<bool>,
    #[serde(rename = "allowedKeys")]
    pub allowed_keys: Option<Vec<String>>,
    /// SSH pub lines that may sign privileged `.relay.yaml` changes (origins, branchRules, …).
    #[serde(rename = "adminKeys")]
    pub admin_keys: Option<Vec<String>>,
    #[serde(rename = "allowedKeyFingerprints")]
    pub allowed_key_fingerprints: Option<Vec<String>>,
    #[serde(rename = "allowUnsigned")]
    pub allow_unsigned: Option<bool>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct RelayTrustConfig {
    #[serde(rename = "authorizedServerIds")]
    pub authorized_server_ids: Option<Vec<String>>,
    #[serde(rename = "authorizedServerKeyFingerprints")]
    pub authorized_server_key_fingerprints: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct GithubHooksConfig {
    pub enabled: bool,
    pub path: String,
    pub events: Vec<String>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct AutoPushConfig {
    pub branches: Vec<String>,
    #[serde(rename = "originList")]
    pub origin_list: Vec<String>,
    #[serde(rename = "debounceSeconds")]
    pub debounce_seconds: Option<u64>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct ClientConfig {
    #[serde(default)]
    pub hooks: HooksConfig,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct HooksConfig {
    #[serde(default)]
    pub get: Option<HookPath>,
    #[serde(default)]
    pub query: Option<HookPath>,
}

/// Hook script path and optional metadata (used by `index` for branch allowlist and QUERY export fields).
#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct HookPath {
    pub path: String,
    /// When set on the `index` hook, only these branch names are indexed and queryable (omit = all branches).
    #[serde(default)]
    pub branches: Option<Vec<String>>,
    /// Per-collection field order for QUERY JSON/XML/CSV projection (see README).
    #[serde(default)]
    pub query: Option<IndexQueryExportConfig>,
}

/// Under `server.hooks.index.query` in `.relay.yaml`.
#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct IndexQueryExportConfig {
    #[serde(default)]
    pub collections: std::collections::HashMap<String, CollectionExportFields>,
}

#[derive(Deserialize, Debug, Default, Serialize, Clone)]
pub struct CollectionExportFields {
    pub fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct TranspileRequest {
    pub code: String,
    pub filename: Option<String>,
    #[serde(default)]
    pub to_common_js: bool,
}

#[derive(Debug, Serialize)]
pub struct TranspileResponse {
    pub code: Option<String>,
    pub map: Option<String>,
    pub diagnostics: Option<String>,
    pub ok: bool,
}

pub enum GitResolveResult {
    Respond(Response),
    NotFound(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("not found")]
    NotFound,
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Not Found: {0}")]
    NotFound(String),
    #[error("Internal Server Error: {0}")]
    Internal(String),
    #[error("Bad Request: {0}")]
    BadRequest(String),
    #[error("Git Error: {0}")]
    Git(#[from] git2::Error),
    #[error("Transpile Error: {0}")]
    Transpile(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::Git(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            ApiError::Transpile(msg) => (StatusCode::BAD_REQUEST, msg),
        };

        let body = Json(serde_json::json!({
            "error": message,
            "ok": false,
        }));

        (status, body).into_response()
    }
}

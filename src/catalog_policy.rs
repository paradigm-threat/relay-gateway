//! Unified relay-master catalog: per-repo records under `catalog/repos/*.json`.
//!
//! See `docs/RELAY_CATALOG.md`.

use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoTier {
    Primary,
    Hosted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SigningKeysSource {
    #[default]
    Repo,
    Catalog,
    Hybrid,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SigningKeyEntry {
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default = "default_active_status")]
    pub status: String,
    #[serde(default)]
    pub added_at: Option<String>,
    #[serde(default)]
    pub disabled_at: Option<String>,
    #[serde(default)]
    pub disabled_reason: Option<String>,
}

fn default_active_status() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BranchFeatureComments {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_comments_branch")]
    pub branch_name: String,
    #[serde(default = "default_sync_publish")]
    pub sync_from: String,
    #[serde(default)]
    pub sync_required: bool,
    #[serde(default = "default_true")]
    pub require_signed_commits: bool,
    #[serde(default)]
    pub allow_unsigned_commits: bool,
    #[serde(default = "default_markdown")]
    pub format: String,
    #[serde(default = "default_comments_path_root")]
    pub path_root: String,
    #[serde(default)]
    pub allow_anonymous_session_keys: bool,
    #[serde(default)]
    pub anonymous_session_max_age_seconds: Option<u64>,
    #[serde(default)]
    pub allow_edit_own_comment_while_page_open: bool,
}

fn default_comments_branch() -> String {
    "comments".to_string()
}
fn default_sync_publish() -> String {
    "publish_branch".to_string()
}
fn default_true() -> bool {
    true
}
fn default_markdown() -> String {
    "markdown".to_string()
}
fn default_comments_path_root() -> String {
    ".relay/comments/by-path/".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BranchFeatureDev {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_dev_branch")]
    pub branch_name: String,
    #[serde(default = "default_sync_publish")]
    pub sync_from: String,
    #[serde(default)]
    pub sync_required: bool,
    #[serde(default = "default_true")]
    pub allow_unsigned_commits: bool,
    #[serde(default = "default_merge_publish")]
    pub merge_target: String,
    #[serde(default = "default_true")]
    pub merge_into_target_requires_catalog_signer: bool,
}

fn default_dev_branch() -> String {
    "dev".to_string()
}
fn default_merge_publish() -> String {
    "publish_branch".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BranchFeatures {
    #[serde(default)]
    pub comments: Option<BranchFeatureComments>,
    #[serde(default)]
    pub dev: Option<BranchFeatureDev>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogRepoRecord {
    #[serde(default)]
    pub schema: Option<String>,
    pub repo: String,
    pub tier: RepoTier,
    #[serde(default)]
    pub forge_url: Option<String>,
    pub relay_root_commit: String,
    #[serde(default)]
    pub approved_head: Option<String>,
    #[serde(default = "default_publish_branch")]
    pub publish_branch: String,
    #[serde(default)]
    pub owner_key_id: Option<String>,
    #[serde(default)]
    pub signing_keys: Vec<SigningKeyEntry>,
    /// Legacy flat list; treated as active keys when `signing_keys` is empty.
    #[serde(default)]
    pub allowed_keys: Vec<String>,
    #[serde(default)]
    pub signing_keys_source: SigningKeysSource,
    /// Parked apex/host domains this repo answers on (e.g. `clevertree.net`, `www.clevertree.net`).
    /// Matched case-insensitively against the request `Host`. Globally unique across the catalog.
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub branch_features: Option<BranchFeatures>,
    #[serde(default)]
    pub limits: Option<serde_json::Value>,
    #[serde(default)]
    pub last_history_reset: Option<serde_json::Value>,
}

fn default_publish_branch() -> String {
    "main".to_string()
}

#[derive(Debug, Clone)]
pub struct CatalogIndex {
    pub repos: HashMap<String, CatalogRepoRecord>,
}

impl CatalogRepoRecord {
    pub fn normalized_signing_keys(&self) -> Vec<SigningKeyEntry> {
        if !self.signing_keys.is_empty() {
            return self.signing_keys.clone();
        }
        self.allowed_keys
            .iter()
            .map(|k| SigningKeyEntry {
                public_key: k.clone(),
                key_id: None,
                status: "active".to_string(),
                added_at: None,
                disabled_at: None,
                disabled_reason: None,
            })
            .collect()
    }

    pub fn verify_key_lines(&self) -> Vec<String> {
        self.normalized_signing_keys()
            .into_iter()
            .map(|e| e.public_key)
            .filter(|s| !s.trim().is_empty())
            .collect()
    }

    pub fn authorize_key_lines(&self) -> Vec<String> {
        self.normalized_signing_keys()
            .into_iter()
            .filter(|e| e.status == "active")
            .map(|e| e.public_key)
            .filter(|s| !s.trim().is_empty())
            .collect()
    }

    pub fn uses_catalog_signing(&self) -> bool {
        matches!(
            self.signing_keys_source,
            SigningKeysSource::Catalog | SigningKeysSource::Hybrid
        ) && !self.verify_key_lines().is_empty()
    }

    pub fn comments_branch_name(&self) -> Option<String> {
        self.branch_features
            .as_ref()
            .and_then(|b| b.comments.as_ref())
            .filter(|c| c.enabled)
            .map(|c| c.branch_name.clone())
    }

    pub fn dev_branch_name(&self) -> Option<String> {
        self.branch_features
            .as_ref()
            .and_then(|b| b.dev.as_ref())
            .filter(|d| d.enabled)
            .map(|d| d.branch_name.clone())
    }

    pub fn branch_feature_allows_unsigned(&self, branch: &str) -> Option<bool> {
        if let Some(ref dev) = self
            .branch_features
            .as_ref()
            .and_then(|b| b.dev.as_ref())
        {
            if dev.enabled && dev.branch_name == branch {
                return Some(dev.allow_unsigned_commits);
            }
        }
        if let Some(ref comments) = self
            .branch_features
            .as_ref()
            .and_then(|b| b.comments.as_ref())
        {
            if comments.enabled && comments.branch_name == branch {
                return Some(comments.allow_unsigned_commits);
            }
        }
        None
    }

    pub fn allow_binary_files(&self) -> bool {
        if let Some(limits) = &self.limits {
            if let Some(v) = limits.get("allow_binary_files") {
                if let Some(b) = v.as_bool() {
                    return b;
                }
            }
            if self.thin_ipfs_assets() {
                return false;
            }
        }
        matches!(self.tier, RepoTier::Primary)
    }

    pub fn thin_ipfs_assets(&self) -> bool {
        self.limits
            .as_ref()
            .and_then(|l| l.get("thin_ipfs_assets"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn merge_to_publish_requires_catalog_signer(&self, branch: &str) -> bool {
        if branch != self.publish_branch {
            return false;
        }
        self.branch_features
            .as_ref()
            .and_then(|b| b.dev.as_ref())
            .filter(|d| d.enabled)
            .map(|d| d.merge_into_target_requires_catalog_signer)
            .unwrap_or(false)
    }
}

/// Load `catalog/repos/*.json` from a relay-master checkout path.
pub fn load_catalog_index(catalog_path: &Path) -> anyhow::Result<CatalogIndex> {
    let repos_dir = catalog_path.join("catalog/repos");
    if !repos_dir.is_dir() {
        anyhow::bail!(
            "no catalog/repos/ under {} (unified catalog not present)",
            catalog_path.display()
        );
    }
    let mut repos = HashMap::new();
    for entry in std::fs::read_dir(&repos_dir).context("read catalog/repos")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let s = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let rec: CatalogRepoRecord =
            serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?;
        let slug = rec.repo.clone();
        repos.insert(slug, rec);
    }
    Ok(CatalogIndex { repos })
}

/// Try catalog dir, else return None (caller uses legacy relay-catalog.json only).
pub fn try_load_catalog_index(catalog_path: &Path) -> Option<CatalogIndex> {
    load_catalog_index(catalog_path).ok()
}

/// Resolve catalog record for a bare repo slug (`paradigm-threat-files` from `paradigm-threat-files.git`).
pub fn catalog_record_for_slug<'a>(
    index: &'a CatalogIndex,
    repo_slug: &str,
) -> Option<&'a CatalogRepoRecord> {
    index.repos.get(repo_slug)
}

/// Normalize a `Host` (or catalog domain) for parked-domain matching:
/// strip an optional `:port`, drop a trailing dot, and lowercase.
pub fn normalize_domain(host: &str) -> String {
    let h = host.trim();
    let h = h.split(':').next().unwrap_or(h);
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// Basic hostname validation (labels 1..=63 of `[a-z0-9-]`, not edge-hyphenated, >=2 labels).
fn is_valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.split('.').count() >= 2
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// Build a `domain -> repo slug` map from the catalog, enforcing global uniqueness.
///
/// Errors if two repos claim the same domain, or a domain is empty/malformed. Repos are
/// visited in sorted-slug order so collision errors are deterministic.
pub fn build_parked_domain_map(index: &CatalogIndex) -> anyhow::Result<HashMap<String, String>> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut slugs: Vec<&String> = index.repos.keys().collect();
    slugs.sort();
    for slug in slugs {
        let rec = &index.repos[slug];
        for raw in &rec.domains {
            let d = normalize_domain(raw);
            if d.is_empty() {
                anyhow::bail!("catalog repo '{}' has an empty domain entry", rec.repo);
            }
            if !is_valid_domain(&d) {
                anyhow::bail!("catalog repo '{}' has invalid domain '{}'", rec.repo, raw);
            }
            if let Some(existing) = map.get(&d) {
                if existing != &rec.repo {
                    anyhow::bail!(
                        "parked domain '{}' claimed by both '{}' and '{}'",
                        d,
                        existing,
                        rec.repo
                    );
                }
            }
            map.insert(d, rec.repo.clone());
        }
    }
    Ok(map)
}

/// Resolve a request `Host` to a parked repo slug via a `domain -> repo` map.
pub fn repo_for_parked_host(map: &HashMap<String, String>, host_header: &str) -> Option<String> {
    map.get(&normalize_domain(host_header)).cloned()
}

/// Path for the parked-domain allowlist consumed by `caddy-ask.py` (Caddy on-demand TLS).
/// `RELAY_PARKED_DOMAINS_PATH`, else `${RELAY_INSTALL_ROOT}/state/parked-domains.txt`.
pub fn parked_domains_allowlist_path() -> PathBuf {
    if let Ok(p) = std::env::var("RELAY_PARKED_DOMAINS_PATH") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let root = std::env::var("RELAY_INSTALL_ROOT").unwrap_or_else(|_| "/opt/relay".into());
    Path::new(&root).join("state/parked-domains.txt")
}

/// Write the parked-domain routing map and the TLS ask allowlist.
///
/// `parked-domains.json` always holds the full Host→repo map (HTTP routing).
/// `parked-domains.txt` (caddy-ask) lists only hosts whose repo enables
/// `server.tls.plugin: park`. When `repo_path` is unknown, falls back to writing
/// the full map (legacy / tests).
pub fn write_parked_domains_allowlist(map: &HashMap<String, String>) {
    write_parked_domains_allowlist_at(None, map);
}

/// Like [`write_parked_domains_allowlist`], but filter the TLS allowlist using
/// `.relay.yaml` from bare repos under `repo_path`.
pub fn write_parked_domains_allowlist_at(
    repo_path: Option<&Path>,
    map: &HashMap<String, String>,
) {
    if let Some(rp) = repo_path {
        crate::park_tls::write_tls_allowlist(rp, map);
        return;
    }
    let path = parked_domains_allowlist_path();
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            return;
        }
    }
    let mut domains: Vec<&String> = map.keys().collect();
    domains.sort();
    let mut body = String::from("# relay parked domains (Host -> repo); managed by relay-gateway\n");
    for d in domains {
        body.push_str(d);
        body.push('\n');
    }
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write parked-domains allowlist");
    }
    let map_path = parked_domains_map_path();
    match serde_json::to_string_pretty(map) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&map_path, format!("{json}\n")) {
                tracing::warn!(error = %e, path = %map_path.display(), "failed to write parked-domains map");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialize parked-domains map"),
    }
}

/// JSON domain→repo map path (`RELAY_PARKED_DOMAINS_MAP_PATH` or state/parked-domains.json).
pub fn parked_domains_map_path() -> PathBuf {
    if let Ok(p) = std::env::var("RELAY_PARKED_DOMAINS_MAP_PATH") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let root = std::env::var("RELAY_INSTALL_ROOT").unwrap_or_else(|_| "/opt/relay".into());
    Path::new(&root).join("state/parked-domains.json")
}

/// Load a previously materialized domain→repo map from disk.
pub fn load_parked_domains_map_file() -> Option<HashMap<String, String>> {
    let path = parked_domains_map_path();
    let s = std::fs::read_to_string(&path).ok()?;
    let raw: HashMap<String, String> = serde_json::from_str(&s).ok()?;
    let mut map = HashMap::new();
    for (k, v) in raw {
        map.insert(normalize_domain(&k), v);
    }
    (!map.is_empty()).then_some(map)
}

/// Load `catalog/repos/*.json` from the tip of a bare `relay-master.git` (fleet nodes without a worktree).
pub fn load_catalog_index_from_bare(bare_path: &Path) -> anyhow::Result<CatalogIndex> {
    let repo = git2::Repository::open_bare(bare_path)
        .with_context(|| format!("open bare {}", bare_path.display()))?;
    let tip = repo
        .head()
        .context("bare HEAD")?
        .peel_to_commit()
        .context("peel HEAD")?
        .tree()
        .context("HEAD tree")?;
    let catalog = tip
        .get_path(Path::new("catalog"))
        .ok()
        .and_then(|e| e.to_object(&repo).ok())
        .and_then(|o| o.peel_to_tree().ok())
        .context("catalog/ missing at bare tip")?;
    let repos_tree = catalog
        .get_path(Path::new("repos"))
        .ok()
        .and_then(|e| e.to_object(&repo).ok())
        .and_then(|o| o.peel_to_tree().ok())
        .context("catalog/repos/ missing at bare tip")?;
    let mut repos = HashMap::new();
    for entry in repos_tree.iter() {
        let name = entry.name().unwrap_or("");
        if !name.ends_with(".json") {
            continue;
        }
        let blob = entry
            .to_object(&repo)
            .ok()
            .and_then(|o| o.peel_to_blob().ok())
            .with_context(|| format!("read catalog/repos/{name}"))?;
        let rec: CatalogRepoRecord = serde_json::from_slice(blob.content())
            .with_context(|| format!("parse catalog/repos/{name}"))?;
        repos.insert(rec.repo.clone(), rec);
    }
    Ok(CatalogIndex { repos })
}

/// Resolve parked-domain map: worktree catalog → bare relay-master tip → state map file.
pub fn resolve_parked_domains(
    repo_path: &Path,
    catalog_worktree: Option<&Path>,
) -> anyhow::Result<Option<HashMap<String, String>>> {
    if let Some(c) = catalog_worktree {
        if let Ok(index) = load_catalog_index(c) {
            let map = build_parked_domain_map(&index)?;
            write_parked_domains_allowlist_at(Some(repo_path), &map);
            return Ok((!map.is_empty()).then_some(map));
        }
    }
    let bare = repo_path.join("relay-master.git");
    if bare.is_dir() {
        if let Ok(index) = load_catalog_index_from_bare(&bare) {
            let map = build_parked_domain_map(&index)?;
            if !map.is_empty() {
                write_parked_domains_allowlist_at(Some(repo_path), &map);
                return Ok(Some(map));
            }
        }
    }
    if let Some(map) = load_parked_domains_map_file() {
        write_parked_domains_allowlist_at(Some(repo_path), &map);
        return Ok(Some(map));
    }
    Ok(None)
}

/// Env `RELAY_MASTER_CATALOG_PATH` or parent of bare data when syncing.
pub fn catalog_path_from_env() -> Option<PathBuf> {
    std::env::var("RELAY_MASTER_CATALOG_PATH")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// URL-safe path segment for comment storage (RFC 3986 unreserved + slash encoded).
pub fn encode_comment_target_path(repo_relative_path: &str) -> String {
    let trimmed = repo_relative_path.trim_start_matches('/');
    percent_encoding::utf8_percent_encode(trimmed, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_comment_path() {
        assert!(encode_comment_target_path("content/foo.md").contains("content"));
        assert!(encode_comment_target_path("content/foo.md").contains("foo"));
    }

    #[test]
    fn authorize_excludes_disabled() {
        let rec = CatalogRepoRecord {
            schema: None,
            repo: "t".into(),
            tier: RepoTier::Hosted,
            forge_url: None,
            relay_root_commit: "a".repeat(40),
            approved_head: None,
            publish_branch: "release".into(),
            owner_key_id: None,
            signing_keys: vec![
                SigningKeyEntry {
                    public_key: "ssh-ed25519 AAAA".into(),
                    key_id: None,
                    status: "active".into(),
                    added_at: None,
                    disabled_at: None,
                    disabled_reason: None,
                },
                SigningKeyEntry {
                    public_key: "ssh-ed25519 BBBB".into(),
                    key_id: None,
                    status: "disabled".into(),
                    added_at: None,
                    disabled_at: Some("2026-01-01T00:00:00Z".into()),
                    disabled_reason: None,
                },
            ],
            allowed_keys: vec![],
            signing_keys_source: SigningKeysSource::Catalog,
            domains: vec![],
            branch_features: None,
            limits: None,
            last_history_reset: None,
        };
        assert_eq!(rec.verify_key_lines().len(), 2);
        assert_eq!(rec.authorize_key_lines().len(), 1);
    }

    fn rec_with_domains(repo: &str, domains: &[&str]) -> CatalogRepoRecord {
        CatalogRepoRecord {
            schema: None,
            repo: repo.into(),
            tier: RepoTier::Hosted,
            forge_url: None,
            relay_root_commit: "a".repeat(40),
            approved_head: None,
            publish_branch: "release".into(),
            owner_key_id: None,
            signing_keys: vec![],
            allowed_keys: vec![],
            signing_keys_source: SigningKeysSource::Repo,
            domains: domains.iter().map(|s| s.to_string()).collect(),
            branch_features: None,
            limits: None,
            last_history_reset: None,
        }
    }

    fn index_of(records: Vec<CatalogRepoRecord>) -> CatalogIndex {
        let mut repos = HashMap::new();
        for r in records {
            repos.insert(r.repo.clone(), r);
        }
        CatalogIndex { repos }
    }

    #[test]
    fn normalize_domain_strips_port_dot_and_case() {
        assert_eq!(normalize_domain("CleverTree.NET"), "clevertree.net");
        assert_eq!(normalize_domain("clevertree.net:443"), "clevertree.net");
        assert_eq!(normalize_domain("clevertree.net."), "clevertree.net");
        assert_eq!(normalize_domain(" www.clevertree.net "), "www.clevertree.net");
    }

    #[test]
    fn parked_domain_map_resolves_apex_and_www() {
        let idx = index_of(vec![rec_with_domains(
            "clevertree-site",
            &["clevertree.net", "www.clevertree.net"],
        )]);
        let map = build_parked_domain_map(&idx).unwrap();
        assert_eq!(
            repo_for_parked_host(&map, "clevertree.net").as_deref(),
            Some("clevertree-site")
        );
        assert_eq!(
            repo_for_parked_host(&map, "WWW.CleverTree.net:443").as_deref(),
            Some("clevertree-site")
        );
        assert_eq!(repo_for_parked_host(&map, "example.com"), None);
    }

    #[test]
    fn parked_domain_map_rejects_duplicate_across_repos() {
        let idx = index_of(vec![
            rec_with_domains("clevertree-site", &["clevertree.net"]),
            rec_with_domains("other-site", &["clevertree.net"]),
        ]);
        let err = build_parked_domain_map(&idx).unwrap_err().to_string();
        assert!(err.contains("clevertree.net"), "unexpected: {err}");
        assert!(err.contains("claimed by both"), "unexpected: {err}");
    }

    #[test]
    fn parked_domain_map_rejects_malformed_domain() {
        let idx = index_of(vec![rec_with_domains("bad-site", &["not_a_domain"])]);
        assert!(build_parked_domain_map(&idx).is_err());
        let idx2 = index_of(vec![rec_with_domains("bad-site", &["nodot"])]);
        assert!(build_parked_domain_map(&idx2).is_err());
    }

    #[test]
    fn parked_domain_map_allows_same_repo_repeated_domain() {
        let idx = index_of(vec![rec_with_domains(
            "clevertree-site",
            &["clevertree.net", "Clevertree.net."],
        )]);
        let map = build_parked_domain_map(&idx).unwrap();
        assert_eq!(map.len(), 1);
    }
}

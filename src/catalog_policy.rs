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
            branch_features: None,
            limits: None,
            last_history_reset: None,
        };
        assert_eq!(rec.verify_key_lines().len(), 2);
        assert_eq!(rec.authorize_key_lines().len(), 1);
    }
}

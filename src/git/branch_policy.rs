//! Branch rules from `.relay.yaml`: resolve `*` / exact / default, verify SSH signatures per commit,
//! and enforce governance when `.relay.yaml` changes (parent allowlist must verify the commit).
//!
//! When [`RELAY_MASTER_CATALOG_PATH`] is set and `catalog/repos/` exists, catalog records can
//! supply signing keys (`signing_keys_source: catalog`) and branch feature overrides. See
//! `docs/RELAY_CATALOG.md`.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use tempfile::NamedTempFile;

use crate::catalog_policy::{
    self, CatalogIndex, CatalogRepoRecord, RepoTier, SigningKeysSource,
};
use crate::git::read_git_config_from_revision;
use crate::types::{BranchRule, BranchRulesConfig};

static CATALOG_INDEX: OnceLock<Option<CatalogIndex>> = OnceLock::new();

fn catalog_index() -> Option<&'static CatalogIndex> {
    CATALOG_INDEX
        .get_or_init(|| {
            catalog_policy::catalog_path_from_env()
                .and_then(|p| catalog_policy::try_load_catalog_index(&p))
        })
        .as_ref()
}

fn repo_slug_from_path(repo_path: &Path) -> String {
    repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .trim_end_matches(".git")
        .to_string()
}

fn catalog_record_for_repo(repo_path: &Path) -> Option<&'static CatalogRepoRecord> {
    let slug = repo_slug_from_path(repo_path);
    catalog_index().and_then(|idx| catalog_policy::catalog_record_for_slug(idx, &slug))
}

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Resolve rule for `branch`: exact name in `branches`, then `name == "*"`, then `default`.
pub fn resolve_branch_rule(branch: &str, config: &BranchRulesConfig) -> Option<BranchRule> {
    if let Some(ref branches) = config.branches {
        for b in branches {
            if b.name == branch {
                return Some(b.rule.clone());
            }
        }
        for b in branches {
            if b.name == "*" {
                return Some(b.rule.clone());
            }
        }
    }
    config.default.clone()
}

/// True if commits on this branch must be cryptographically signed and key-allowlisted.
pub fn branch_requires_signature(rule: &BranchRule) -> bool {
    if rule.allow_unsigned == Some(true) {
        return false;
    }
    if rule.require_signed == Some(false) {
        return false;
    }
    true
}

/// `merge-base(refs/heads/main|master, tip)` when that ref exists; else `None` (use full `tip` history).
fn merge_base_with_default_branch(repo_path: &Path, tip: &str) -> Result<Option<String>, String> {
    for name in ["main", "master"] {
        let pref = format!("refs/heads/{}", name);
        let has = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .arg("show-ref")
            .arg("--verify")
            .arg(&pref)
            .output()
            .map_err(|e| format!("git show-ref: {}", e))?;
        if !has.status.success() {
            continue;
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .arg("merge-base")
            .arg(&pref)
            .arg(tip)
            .output()
            .map_err(|e| format!("git merge-base: {}", e))?;
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let b = s.trim().to_string();
            if b.len() >= 40 {
                return Ok(Some(b));
            }
        }
    }
    Ok(None)
}

/// List commits in `old_oid..new_oid` (exclusive old, inclusive new), oldest first.
///
/// When `old_oid` is all zeros (**new ref**), use `merge-base(default_branch, new_oid)..new_oid` so we
/// do not walk pre-policy history (Policy I for branches created from `main`).
pub fn rev_list_range(
    repo_path: &Path,
    old_oid: &str,
    new_oid: &str,
) -> Result<Vec<String>, String> {
    if new_oid == ZERO_OID || new_oid.len() < 40 {
        return Ok(vec![]);
    }
    let range_arg = if old_oid == ZERO_OID || old_oid.chars().all(|c| c == '0') {
        match merge_base_with_default_branch(repo_path, new_oid)? {
            Some(base) if base != new_oid => format!("{}..{}", base, new_oid),
            Some(_) => return Ok(vec![]),
            None => new_oid.to_string(),
        }
    } else {
        format!("{}..{}", old_oid, new_oid)
    };
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-list")
        .arg("--reverse")
        .arg(&range_arg)
        .output()
        .map_err(|e| format!("git rev-list: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Vec<String> = s
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(v)
}

fn first_parent(repo_path: &Path, commit: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-parse")
        .arg(format!("{}^", commit))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn relay_yaml_changed(repo_path: &Path, parent: &str, commit: &str) -> bool {
    let out = match Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("diff-tree")
        .arg("--no-commit-id")
        .arg("-q")
        .arg(parent)
        .arg(commit)
        .arg("--")
        .arg(".relay.yaml")
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    out.status.code() == Some(1)
}

fn allowed_signers_body_from_lines(key_lines: &[String]) -> Result<String, String> {
    if key_lines.is_empty() {
        return Err("branch rule requires signing but allowedKeys is empty".into());
    }
    let mut buf = String::new();
    for line in key_lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains("ssh-ed25519")
            || t.contains("ssh-rsa")
            || t.contains("ecdsa-sha2")
            || t.contains("sk-ssh")
        {
            if t.contains('@') || t.starts_with('*') {
                buf.push_str(t);
            } else {
                buf.push('*');
                buf.push(' ');
                buf.push_str(t);
            }
            buf.push('\n');
        } else {
            buf.push('*');
            buf.push(' ');
            buf.push_str(t);
            buf.push('\n');
        }
    }
    if buf.is_empty() {
        return Err("allowedKeys had no usable SSH public key lines".into());
    }
    Ok(buf)
}

fn allowed_signers_body(rule: &BranchRule) -> Result<String, String> {
    let keys = match &rule.allowed_keys {
        Some(k) if !k.is_empty() => k.clone(),
        _ => {
            if rule
                .allowed_key_fingerprints
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
            {
                return Err(
                    "allowedKeyFingerprints requires matching allowedKeys (SSH pub lines) for verification"
                        .into(),
                );
            }
            return Err("branch rule requires signing but allowedKeys is empty".into());
        }
    };
    allowed_signers_body_from_lines(&keys)
}

fn commit_committer_iso8601(repo_path: &Path, commit: &str) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("log")
        .arg("-1")
        .arg("--format=%cI")
        .arg(commit)
        .output()
        .map_err(|e| format!("git log committer date: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git log committer date for {} failed",
            commit
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn merge_parent_count(repo_path: &Path, commit: &str) -> Result<usize, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-list")
        .arg("--parents")
        .arg("-n")
        .arg("1")
        .arg(commit)
        .output()
        .map_err(|e| format!("git rev-list parents: {}", e))?;
    if !out.status.success() {
        return Err(format!("rev-list parents for {} failed", commit));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<_> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(0);
    }
    Ok(parts.len().saturating_sub(1))
}

fn apply_catalog_branch_flags(rule: &BranchRule, catalog: Option<&CatalogRepoRecord>, branch: &str) -> BranchRule {
    let mut r = rule.clone();
    if let Some(rec) = catalog {
        if let Some(allow_unsigned) = rec.branch_feature_allows_unsigned(branch) {
            if allow_unsigned {
                r.allow_unsigned = Some(true);
                r.require_signed = Some(false);
            } else {
                r.allow_unsigned = Some(false);
                r.require_signed = Some(true);
            }
        }
    }
    r
}

fn effective_allowed_keys_for_verify(
    rule: &BranchRule,
    catalog: Option<&CatalogRepoRecord>,
) -> Result<Vec<String>, String> {
    let repo_keys = rule.allowed_keys.clone().unwrap_or_default();
    let Some(rec) = catalog else {
        return Ok(repo_keys);
    };
    if !rec.uses_catalog_signing() {
        return Ok(repo_keys);
    }
    let catalog_verify = rec.verify_key_lines();
    match rec.signing_keys_source {
        SigningKeysSource::Catalog => {
            if catalog_verify.is_empty() && repo_keys.is_empty() {
                return Err("catalog signing_keys_source=catalog but no signing_keys in catalog record".into());
            }
            Ok(if catalog_verify.is_empty() {
                repo_keys
            } else {
                catalog_verify
            })
        }
        SigningKeysSource::Hybrid => {
            if catalog_verify.is_empty() {
                return Ok(repo_keys);
            }
            let cat_set: std::collections::HashSet<_> = catalog_verify.iter().map(|s| s.trim()).collect();
            let inter: Vec<String> = repo_keys
                .into_iter()
                .filter(|k| cat_set.contains(k.trim()))
                .collect();
            if inter.is_empty() {
                return Err("hybrid signing: no overlap between repo allowedKeys and catalog signing_keys".into());
            }
            Ok(inter)
        }
        SigningKeysSource::Repo => Ok(repo_keys),
    }
}

fn reject_foreign_repo_keys(
    rule: &BranchRule,
    catalog: Option<&CatalogRepoRecord>,
) -> Result<(), String> {
    let Some(rec) = catalog else {
        return Ok(());
    };
    if rec.tier != RepoTier::Hosted || !rec.uses_catalog_signing() {
        return Ok(());
    }
    let repo_keys = match &rule.allowed_keys {
        Some(k) if !k.is_empty() => k,
        _ => return Ok(()),
    };
    let verify: std::collections::HashSet<_> = rec
        .verify_key_lines()
        .iter()
        .map(|s| s.trim().to_string())
        .collect();
    for k in repo_keys {
        if !verify.contains(k.trim()) {
            return Err(format!(
                "hosted catalog mode: repo allowedKeys contains key not in catalog signing_keys"
            ));
        }
    }
    Ok(())
}

fn authorize_catalog_signer(
    repo_path: &Path,
    commit: &str,
    rec: &CatalogRepoRecord,
) -> Result<(), String> {
    let active = rec.authorize_key_lines();
    if active.is_empty() {
        return Err("catalog record has no active signing keys".into());
    }
    let active_body = allowed_signers_body_from_lines(&active)?;
    if git_verify_commit_with_allowed_signers(repo_path, commit, &active_body).is_ok() {
        return Ok(());
    }
    let committer = commit_committer_iso8601(repo_path, commit)?;
    for entry in rec.normalized_signing_keys() {
        if entry.status != "disabled" {
            continue;
        }
        let line = entry.public_key.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let body = allowed_signers_body_from_lines(&[line])?;
        if git_verify_commit_with_allowed_signers(repo_path, commit, &body).is_ok() {
            let disabled_at = entry.disabled_at.as_deref().unwrap_or("");
            if !disabled_at.is_empty() && committer.as_str() >= disabled_at {
                return Err(format!(
                    "commit {}: signer key disabled at {} (committer {})",
                    commit, disabled_at, committer
                ));
            }
            return Ok(());
        }
    }
    Err(format!(
        "commit {}: signature not from an active catalog signing key",
        commit
    ))
}

fn git_verify_commit_with_allowed_signers(
    repo_path: &Path,
    commit: &str,
    signers_body: &str,
) -> Result<(), String> {
    let mut tmp = NamedTempFile::new().map_err(|e| e.to_string())?;
    tmp.write_all(signers_body.as_bytes())
        .map_err(|e| e.to_string())?;
    tmp.flush().map_err(|e| e.to_string())?;
    let path = tmp.path().to_owned();
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("-c")
        .arg(format!("gpg.ssh.allowedSignersFile={}", path.display()))
        .arg("verify-commit")
        .arg(commit)
        .output()
        .map_err(|e| format!("git verify-commit: {}", e))?;
    drop(tmp);
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        Err(format!(
            "git verify-commit {} failed: {}{}",
            commit, stderr, stdout
        ))
    }
}

fn verify_commit_for_rule(
    repo_path: &Path,
    commit: &str,
    rule: &BranchRule,
    catalog: Option<&CatalogRepoRecord>,
) -> Result<(), String> {
    reject_foreign_repo_keys(rule, catalog)?;
    if !branch_requires_signature(rule) {
        return Ok(());
    }
    let keys = effective_allowed_keys_for_verify(rule, catalog)?;
    let body = allowed_signers_body_from_lines(&keys)?;
    git_verify_commit_with_allowed_signers(repo_path, commit, &body)?;
    if let Some(rec) = catalog {
        if rec.uses_catalog_signing() {
            authorize_catalog_signer(repo_path, commit, rec)?;
        }
    }
    Ok(())
}

pub fn enforce_push_range(
    repo_path: &Path,
    old_oid: &str,
    new_oid: &str,
    branch: &str,
) -> Result<(), String> {
    enforce_push_range_with_catalog(repo_path, old_oid, new_oid, branch, catalog_record_for_repo(repo_path))
}

pub fn enforce_push_range_with_catalog(
    repo_path: &Path,
    old_oid: &str,
    new_oid: &str,
    branch: &str,
    catalog: Option<&CatalogRepoRecord>,
) -> Result<(), String> {
    if new_oid == ZERO_OID {
        return Ok(());
    }
    let commits = rev_list_range(repo_path, old_oid, new_oid)?;
    for commit in commits {
        enforce_single_commit_with_catalog(repo_path, &commit, branch, catalog)?;
    }
    Ok(())
}

/// Validate **every** commit reachable from `refs/heads/<branch>` (oldest first), using the same
/// rules as pre-receive per-commit checks. Use after history repair so no ancestor can fail
/// [`enforce_push_range`] when fleet evaluates a large `old..new` catch-up window.
///
/// When `first_parent_only` is true, uses `git rev-list --first-parent` (linearized main); when
/// false, follows merges into the branch like a full `rev-list`.
pub fn enforce_all_commits_on_branch(
    repo_path: &Path,
    branch: &str,
    first_parent_only: bool,
) -> Result<(), String> {
    let refname = format!("refs/heads/{}", branch);
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_path)
        .arg("rev-list")
        .arg("--reverse");
    if first_parent_only {
        cmd.arg("--first-parent");
    }
    cmd.arg(&refname);
    let out = cmd
        .output()
        .map_err(|e| format!("git rev-list: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let commits: Vec<String> = s
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for commit in commits {
        enforce_single_commit_with_catalog(
            repo_path,
            &commit,
            branch,
            catalog_record_for_repo(repo_path),
        )?;
    }
    Ok(())
}

pub fn enforce_single_commit(repo_path: &Path, commit: &str, branch: &str) -> Result<(), String> {
    enforce_single_commit_with_catalog(repo_path, commit, branch, catalog_record_for_repo(repo_path))
}

fn enforce_single_commit_with_catalog(
    repo_path: &Path,
    commit: &str,
    branch: &str,
    catalog: Option<&CatalogRepoRecord>,
) -> Result<(), String> {
    let git_config = read_git_config_from_revision(repo_path, commit).ok_or_else(|| {
        format!(
            "commit {}: missing or invalid .relay.yaml (no commits allowed without policy)",
            commit
        )
    })?;

    let rules_config = git_config
        .branch_rules
        .ok_or_else(|| format!("commit {}: .relay.yaml has no git.branchRules", commit))?;

    let rule = resolve_branch_rule(branch, &rules_config).ok_or_else(|| {
        format!(
            "commit {}: branch {:?} is not covered by branchRules (no matching rule or *)",
            commit, branch
        )
    })?;
    let rule = apply_catalog_branch_flags(&rule, catalog, branch);

    verify_commit_for_rule(repo_path, commit, &rule, catalog)?;

    if let Some(rec) = catalog {
        if rec.merge_to_publish_requires_catalog_signer(branch) {
            if merge_parent_count(repo_path, commit)? >= 2 {
                if !rec.uses_catalog_signing() {
                    return Err(format!(
                        "commit {}: merge into publish branch requires catalog signing_keys",
                        commit
                    ));
                }
                authorize_catalog_signer(repo_path, commit, rec)?;
            }
        }
    }

    if let Some(rec) = catalog {
        crate::git::binary_policy::enforce_commit_no_disallowed_binaries(
            repo_path,
            commit,
            crate::git::binary_policy::allow_binary_files(Some(rec)),
        )?;
    }

    if let Some(parent) = first_parent(repo_path, commit) {
        if crate::git::privileged_policy::privileged_relay_yaml_changed(repo_path, &parent, commit) {
            let parent_git =
                read_git_config_from_revision(repo_path, &parent).ok_or_else(|| {
                    format!(
                        "commit {}: parent {} has no readable .relay.yaml for admin check",
                        commit, parent
                    )
                })?;
            let parent_rules = parent_git.branch_rules.ok_or_else(|| {
                format!(
                    "commit {}: parent has no branchRules for admin check",
                    commit
                )
            })?;
            let parent_rule = resolve_branch_rule(branch, &parent_rules).ok_or_else(|| {
                format!(
                    "commit {}: parent policy does not cover branch {:?}",
                    commit, branch
                )
            })?;
            let parent_rule = apply_catalog_branch_flags(&parent_rule, catalog, branch);
            let admin_rule = crate::git::privileged_policy::admin_rule_from_parent(&parent_rule)?;
            verify_commit_for_rule(repo_path, commit, &admin_rule, catalog)?;
        } else if relay_yaml_changed(repo_path, &parent, commit) {
            let parent_git =
                read_git_config_from_revision(repo_path, &parent).ok_or_else(|| {
                    format!(
                        "commit {}: parent {} has no readable .relay.yaml for governance check",
                        commit, parent
                    )
                })?;
            let parent_rules = parent_git.branch_rules.ok_or_else(|| {
                format!(
                    "commit {}: parent has no branchRules for governance",
                    commit
                )
            })?;
            let parent_rule = resolve_branch_rule(branch, &parent_rules).ok_or_else(|| {
                format!(
                    "commit {}: parent policy does not cover branch {:?}",
                    commit, branch
                )
            })?;
            let parent_rule = apply_catalog_branch_flags(&parent_rule, catalog, branch);
            if branch_requires_signature(&parent_rule) {
                verify_commit_for_rule(repo_path, commit, &parent_rule, catalog)?;
            }
        }
    }

    Ok(())
}

pub fn enforce_unsigned_http_allowed(
    repo_path: &Path,
    branch: &str,
    parent_oid: Option<&str>,
) -> Result<(), String> {
    let rev = parent_oid.ok_or_else(|| {
        "cannot evaluate branch policy on empty branch without a parent commit".to_string()
    })?;

    let git_config = read_git_config_from_revision(repo_path, rev).ok_or_else(|| {
        "missing or invalid .relay.yaml on branch tip (HTTP write rejected)".to_string()
    })?;

    let rules_config = git_config
        .branch_rules
        .ok_or_else(|| ".relay.yaml has no git.branchRules (HTTP write rejected)".to_string())?;

    let rule = resolve_branch_rule(branch, &rules_config).ok_or_else(|| {
        format!(
            "branch {:?} is not covered by branchRules (HTTP write rejected)",
            branch
        )
    })?;

    if branch_requires_signature(&rule) {
        return Err(
            "HTTP write would create an unsigned commit on a branch that requires signed commits"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BranchRuleNamed;

    fn rule(req: Option<bool>, uns: Option<bool>) -> BranchRule {
        BranchRule {
            require_signed: req,
            allowed_keys: None,
            admin_keys: None,
            allowed_key_fingerprints: None,
            allow_unsigned: uns,
        }
    }

    #[test]
    fn resolve_exact_over_wildcard() {
        let cfg = BranchRulesConfig {
            default: None,
            branches: Some(vec![
                BranchRuleNamed {
                    name: "*".into(),
                    rule: rule(Some(true), Some(false)),
                },
                BranchRuleNamed {
                    name: "develop".into(),
                    rule: rule(Some(false), Some(true)),
                },
            ]),
        };
        assert!(branch_requires_signature(
            &resolve_branch_rule("main", &cfg).unwrap()
        ));
        assert!(!branch_requires_signature(
            &resolve_branch_rule("develop", &cfg).unwrap()
        ));
    }

    #[test]
    fn uncovered_branch_is_none() {
        let cfg = BranchRulesConfig {
            default: None,
            branches: Some(vec![BranchRuleNamed {
                name: "main".into(),
                rule: rule(Some(true), None),
            }]),
        };
        assert!(resolve_branch_rule("feature", &cfg).is_none());
    }
}

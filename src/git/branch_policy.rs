//! Branch rules from `.relay.yaml`: resolve `*` / exact / default, verify SSH signatures per commit,
//! and enforce governance when `.relay.yaml` changes (parent allowlist must verify the commit).

use std::io::Write;
use std::path::Path;
use std::process::Command;

use tempfile::NamedTempFile;

use crate::git::read_git_config_from_revision;
use crate::types::{BranchRule, BranchRulesConfig};

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

fn allowed_signers_body(rule: &BranchRule) -> Result<String, String> {
    let keys = match &rule.allowed_keys {
        Some(k) if !k.is_empty() => k,
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
    let mut buf = String::new();
    for line in keys {
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

fn verify_commit_for_rule(repo_path: &Path, commit: &str, rule: &BranchRule) -> Result<(), String> {
    if !branch_requires_signature(rule) {
        return Ok(());
    }
    let body = allowed_signers_body(rule)?;
    git_verify_commit_with_allowed_signers(repo_path, commit, &body)
}

pub fn enforce_push_range(
    repo_path: &Path,
    old_oid: &str,
    new_oid: &str,
    branch: &str,
) -> Result<(), String> {
    if new_oid == ZERO_OID {
        return Ok(());
    }
    let commits = rev_list_range(repo_path, old_oid, new_oid)?;
    for commit in commits {
        enforce_single_commit(repo_path, &commit, branch)?;
    }
    Ok(())
}

fn enforce_single_commit(repo_path: &Path, commit: &str, branch: &str) -> Result<(), String> {
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

    verify_commit_for_rule(repo_path, commit, &rule)?;

    if let Some(parent) = first_parent(repo_path, commit) {
        if relay_yaml_changed(repo_path, &parent, commit) {
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
            if branch_requires_signature(&parent_rule) {
                verify_commit_for_rule(repo_path, commit, &parent_rule)?;
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

//! Relay-defined privileged floor for `.relay.yaml` changes (admin signature required).

use std::path::Path;

use crate::git::relay_config_cache;
use crate::types::{BranchRule, RelayConfig};

/// Built-in privileged sections — repos cannot shrink this set.
pub fn privileged_relay_yaml_changed(repo_path: &Path, parent: &str, commit: &str) -> bool {
    let parent_cfg = match relay_config_cache::read_relay_config_at_path_cached(repo_path, parent) {
        Some(c) => c,
        None => return relay_yaml_file_changed(repo_path, parent, commit),
    };
    let commit_cfg = match relay_config_cache::read_relay_config_at_path_cached(repo_path, commit) {
        Some(c) => c,
        None => return false,
    };
    privileged_config_differs(&parent_cfg, &commit_cfg)
}

fn relay_yaml_file_changed(repo_path: &Path, parent: &str, commit: &str) -> bool {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("diff-tree")
        .arg("--no-commit-id")
        .arg("-q")
        .arg(parent)
        .arg(commit)
        .arg("--")
        .arg(".relay.yaml")
        .output();
    matches!(out, Ok(o) if o.status.code() == Some(1))
}

fn privileged_config_differs(parent: &RelayConfig, commit: &RelayConfig) -> bool {
    snapshot(parent) != snapshot(commit)
}

fn snapshot(cfg: &RelayConfig) -> serde_json::Value {
    serde_json::json!({
        "git": {
            "notifyMode": cfg.git.as_ref().map(|g| format!("{:?}", g.notify_mode)),
            "origins": cfg.git.as_ref().and_then(|g| g.origins.as_ref()),
            "autoPush": cfg.git.as_ref().and_then(|g| g.auto_push.as_ref()),
            "branchRules": cfg.git.as_ref().and_then(|g| g.branch_rules.as_ref()),
        },
        "serverHooks": cfg.server.as_ref().and_then(|s| s.hooks.as_ref()),
    })
}

/// Admin rule from parent tip: signers = parent's `adminKeys`, or `allowedKeys` when bootstrapping first admin set.
pub fn admin_rule_from_parent(parent_rule: &BranchRule) -> Result<BranchRule, String> {
    let admin = parent_rule
        .admin_keys
        .as_ref()
        .filter(|k| !k.is_empty())
        .or_else(|| parent_rule.allowed_keys.as_ref().filter(|k| !k.is_empty()))
        .ok_or_else(|| {
            "privileged .relay.yaml change requires adminKeys or allowedKeys on parent branchRules"
                .to_string()
        })?;
    Ok(BranchRule {
        require_signed: Some(true),
        allow_unsigned: Some(false),
        allowed_keys: Some(admin.clone()),
        admin_keys: parent_rule.admin_keys.clone(),
        allowed_key_fingerprints: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AutoPushConfig, BranchRule, BranchRulesConfig, GitConfig, GitOrigin, NotifyMode,
        RelayConfig, ServerConfig,
    };

    fn git_cfg(origins: Option<Vec<GitOrigin>>, notify_mode: NotifyMode) -> RelayConfig {
        RelayConfig {
            git: Some(GitConfig {
                notify_mode,
                origins,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn privileged_detects_origins_change() {
        let parent = git_cfg(
            Some(vec![GitOrigin {
                host: Some("a.example".into()),
                ..Default::default()
            }]),
            NotifyMode::Next,
        );
        let commit = git_cfg(
            Some(vec![GitOrigin {
                host: Some("b.example".into()),
                ..Default::default()
            }]),
            NotifyMode::Next,
        );
        assert!(privileged_config_differs(&parent, &commit));
    }

    #[test]
    fn privileged_ignores_non_privileged_server_fields() {
        let parent = RelayConfig {
            server: Some(ServerConfig {
                search: Some(crate::types::SearchConfig {
                    plugin: Some("sqlite".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let commit = RelayConfig {
            server: Some(ServerConfig {
                search: Some(crate::types::SearchConfig {
                    plugin: Some("redb".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!privileged_config_differs(&parent, &commit));
    }

    #[test]
    fn privileged_detects_branch_rules_change() {
        let parent = git_cfg(None, NotifyMode::Next);
        let mut commit = parent.clone();
        commit.git.as_mut().unwrap().branch_rules = Some(BranchRulesConfig {
            default: Some(BranchRule {
                require_signed: Some(true),
                ..Default::default()
            }),
            branches: None,
        });
        assert!(privileged_config_differs(&parent, &commit));
    }

    #[test]
    fn privileged_detects_auto_push_change() {
        let parent = git_cfg(None, NotifyMode::Next);
        let mut commit = parent.clone();
        commit.git.as_mut().unwrap().auto_push = Some(AutoPushConfig {
            origin_list: vec!["dfw1.relaygateway.net".into()],
            ..Default::default()
        });
        assert!(privileged_config_differs(&parent, &commit));
    }

    #[test]
    fn admin_rule_requires_parent_admin_keys() {
        let rule = BranchRule {
            allowed_keys: None,
            admin_keys: None,
            ..Default::default()
        };
        assert!(admin_rule_from_parent(&rule).is_err());
    }

    #[test]
    fn admin_rule_bootstraps_from_allowed_keys() {
        let rule = BranchRule {
            allowed_keys: Some(vec!["ssh-ed25519 AAAA editor".into()]),
            admin_keys: None,
            ..Default::default()
        };
        let admin = admin_rule_from_parent(&rule).unwrap();
        assert_eq!(
            admin.allowed_keys.as_deref(),
            Some(&["ssh-ed25519 AAAA editor".to_string()][..])
        );
    }

    #[test]
    fn admin_rule_maps_admin_to_allowed() {
        let rule = BranchRule {
            admin_keys: Some(vec!["ssh-ed25519 AAAA admin".into()]),
            ..Default::default()
        };
        let admin = admin_rule_from_parent(&rule).unwrap();
        assert_eq!(admin.require_signed, Some(true));
        assert_eq!(admin.allow_unsigned, Some(false));
        assert_eq!(
            admin.allowed_keys.as_deref(),
            Some(&["ssh-ed25519 AAAA admin".to_string()][..])
        );
    }
}

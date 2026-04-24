//! Index hook branch allowlist and QUERY field projection from `.relay.yaml`.

use crate::types::RelayConfig;

/// When `server.hooks.<name>.branches` is set, only those branch names match.
/// When the hook is missing or `branches` is unset, all branches are allowed.
pub fn branch_allowed_for_named_hook(cfg: &RelayConfig, hook_name: &str, branch: &str) -> bool {
    let Some(hook) = cfg
        .server
        .as_ref()
        .and_then(|s| s.hooks.as_ref())
        .and_then(|m| m.get(hook_name))
    else {
        return true;
    };
    match &hook.branches {
        None => true,
        Some(list) => list.iter().any(|b| b == branch),
    }
}

/// When `server.hooks.index.branches` is set, only those branches are indexed and queryable.
/// When unset, all branches are allowed.
pub fn branch_allowed_for_index(cfg: &RelayConfig, branch: &str) -> bool {
    branch_allowed_for_named_hook(cfg, "index", branch)
}

/// QUERY branch policy: if `server.hooks.query.branches` is set, use it; otherwise fall back to
/// [`branch_allowed_for_index`].
pub fn branch_allowed_for_query_request(cfg: &RelayConfig, branch: &str) -> bool {
    if let Some(q) = cfg
        .server
        .as_ref()
        .and_then(|s| s.hooks.as_ref())
        .and_then(|m| m.get("query"))
    {
        if q.branches.is_some() {
            return branch_allowed_for_named_hook(cfg, "query", branch);
        }
    }
    branch_allowed_for_index(cfg, branch)
}

/// `true` when `server.hooks.index` is configured.
pub fn has_index_hook(cfg: &RelayConfig) -> bool {
    cfg.server
        .as_ref()
        .and_then(|s| s.hooks.as_ref())
        .and_then(|m| m.get("index"))
        .is_some()
}

/// Path to CleverScript QUERY hook (`.cs`), if configured under `server.hooks.query`.
pub fn clever_query_hook_path(cfg: &RelayConfig) -> Option<String> {
    let p = cfg
        .server
        .as_ref()?
        .hooks
        .as_ref()?
        .get("query")
        .map(|h| h.path.as_str())?;
    if p.ends_with(".cs") {
        Some(p.to_string())
    } else {
        None
    }
}

/// Fields for `server.hooks.index.query.collections.{name}.fields` (projection order).
pub fn export_fields_for_collection(cfg: &RelayConfig, collection: &str) -> Option<Vec<String>> {
    let hook = cfg.server.as_ref()?.hooks.as_ref()?.get("index")?;
    let q = hook.query.as_ref()?;
    q.collections.get(collection).map(|c| c.fields.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        CollectionExportFields, HookPath, IndexQueryExportConfig, RelayConfig, ServerConfig,
    };
    use std::collections::HashMap;

    fn cfg_with_index<F: FnOnce(&mut HookPath)>(f: F) -> RelayConfig {
        let mut hooks = HashMap::new();
        let mut hp = HookPath {
            path: "hooks/x.mjs".into(),
            branches: None,
            query: None,
        };
        f(&mut hp);
        hooks.insert("index".to_string(), hp);
        RelayConfig {
            client: None,
            server: Some(ServerConfig { hooks: Some(hooks) }),
            git: None,
            name: None,
            version: None,
            description: None,
        }
    }

    #[test]
    fn branch_allowed_when_no_branches_key() {
        let cfg = cfg_with_index(|h| {
            h.branches = None;
        });
        assert!(branch_allowed_for_index(&cfg, "main"));
        assert!(branch_allowed_for_index(&cfg, "feature"));
    }

    #[test]
    fn branch_allowed_only_listed() {
        let cfg = cfg_with_index(|h| {
            h.branches = Some(vec!["main".into(), "dev".into()]);
        });
        assert!(branch_allowed_for_index(&cfg, "main"));
        assert!(!branch_allowed_for_index(&cfg, "other"));
    }

    #[test]
    fn branch_allowlist_empty_denies_all() {
        let cfg = cfg_with_index(|h| {
            h.branches = Some(vec![]);
        });
        assert!(!branch_allowed_for_index(&cfg, "main"));
    }

    #[test]
    fn no_index_hook_allows_branch() {
        let cfg = RelayConfig {
            client: None,
            server: Some(ServerConfig { hooks: None }),
            git: None,
            name: None,
            version: None,
            description: None,
        };
        assert!(branch_allowed_for_index(&cfg, "any"));
    }

    #[test]
    fn export_fields_none_without_query_block() {
        let cfg = cfg_with_index(|h| {
            h.query = None;
        });
        assert!(export_fields_for_collection(&cfg, "index").is_none());
    }

    #[test]
    fn export_fields_per_collection() {
        let mut qc = IndexQueryExportConfig::default();
        qc.collections.insert(
            "movies".into(),
            CollectionExportFields {
                fields: vec!["a".into(), "b".into()],
            },
        );
        let cfg = cfg_with_index(|h| {
            h.query = Some(qc);
        });
        assert_eq!(
            export_fields_for_collection(&cfg, "movies").unwrap(),
            vec!["a", "b"]
        );
        assert!(export_fields_for_collection(&cfg, "index").is_none());
    }
}

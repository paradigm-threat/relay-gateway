#[cfg(test)]
mod tests {
    use crate::git::repo::{read_git_config, read_relay_config};
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    #[test]
    fn test_read_relay_config() {
        let repo_dir = tempdir().unwrap();
        let repo = Repository::init_bare(repo_dir.path()).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();

        let config_yaml = r#"
name: "Test Repo"
version: "1.0.0"
git:
  autoPush:
    branches: ["main"]
    originList: ["peer1"]
"#;

        // Create initial commit with .relay.yaml
        let blob_oid = repo.blob(config_yaml.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "add config",
            &tree,
            &[],
        )
        .unwrap();

        let config = read_relay_config(&repo, "main").expect("failed to read config");
        assert_eq!(config.name.unwrap(), "Test Repo");
        assert_eq!(config.version.unwrap(), "1.0.0");

        let git_config = read_git_config(&repo, "main").expect("failed to read git config");
        let auto_push = git_config.auto_push.unwrap();
        assert_eq!(auto_push.branches, vec!["main"]);
        assert_eq!(auto_push.origin_list, vec!["peer1"]);
    }

    #[test]
    fn test_read_config_missing_file() {
        let repo_dir = tempdir().unwrap();
        let repo = Repository::init_bare(repo_dir.path()).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();

        // Create initial commit WITHOUT .relay.yaml
        let tb = repo.treebuilder(None).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let config = read_relay_config(&repo, "main");
        assert!(config.is_none());
    }

    #[test]
    fn test_read_config_invalid_yaml() {
        let repo_dir = tempdir().unwrap();
        let repo = Repository::init_bare(repo_dir.path()).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();

        let config_yaml = "invalid: yaml: : content";

        let blob_oid = repo.blob(config_yaml.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "add bad config",
            &tree,
            &[],
        )
        .unwrap();

        let config = read_relay_config(&repo, "main");
        assert!(config.is_none());
    }

    #[test]
    fn test_default_branch_from_bare_head_master() {
        use crate::git::repo::default_branch_from_bare_head;
        use std::path::PathBuf;
        let td = tempdir().unwrap();
        let root: PathBuf = td.path().to_path_buf();
        let repo_path = root.join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let tb = repo.treebuilder(None).unwrap();
        let tid = tb.write().unwrap();
        let tree = repo.find_tree(tid).unwrap();
        repo.commit(Some("refs/heads/master"), &sig, &sig, "x", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/master").unwrap();
        assert_eq!(
            default_branch_from_bare_head(&root, "r").as_deref(),
            Some("master")
        );
    }

    /// Regression: extended `HookPath` under `server.hooks.index` deserializes (branches + query.fields).
    #[test]
    fn test_read_relay_config_index_hook_extended() {
        let repo_dir = tempdir().unwrap();
        let repo = Repository::init_bare(repo_dir.path()).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();

        let config_yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.mjs
      branches: ["main", "dev"]
      query:
        collections:
          index:
            fields: ["title", "_meta_dir"]
"#;

        let blob_oid = repo.blob(config_yaml.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", blob_oid, 0o100644).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "config", &tree, &[])
            .unwrap();

        let config = read_relay_config(&repo, "main").expect("config");
        let hook = config
            .server
            .as_ref()
            .unwrap()
            .hooks
            .as_ref()
            .unwrap()
            .get("index")
            .unwrap();
        assert_eq!(hook.path, "hooks/server/index.mjs");
        assert_eq!(hook.branches.as_ref().unwrap(), &["main", "dev"]);
        let fields = hook
            .query
            .as_ref()
            .unwrap()
            .collections
            .get("index")
            .unwrap()
            .fields
            .clone();
        assert_eq!(fields, vec!["title", "_meta_dir"]);
    }
}

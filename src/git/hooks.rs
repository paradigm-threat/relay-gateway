use crate::git::clever_script_host::CleverScriptServerHost;
use crate::git::meta_index;
use crate::git::repo::read_file_from_repo;
use relay_runtime::value::Value as CsValue;
use std::path::PathBuf;
use tracing::debug;

pub struct HookContext {
    pub repo_path: PathBuf,
    pub old_commit: String,
    pub new_commit: String,
    pub refname: String,
    pub branch: String,
    pub is_verified: bool,
    /// Pre-extracted file contents from the repository (maps path to base64 content)
    pub files: std::collections::HashMap<String, String>,
}

fn strip_shebang_bytes(content: &[u8]) -> Vec<u8> {
    if content.starts_with(b"#!") {
        if let Some(pos) = content.iter().position(|&b| b == b'\n') {
            content[pos + 1..].to_vec()
        } else {
            Vec::new()
        }
    } else {
        content.to_vec()
    }
}

fn hook_return_ok(v: &CsValue) -> bool {
    match v {
        CsValue::Bool(b) => *b,
        CsValue::Number(n) => *n != 0.0,
        CsValue::Null => true,
        CsValue::Object(o) => {
            if let Some(CsValue::Bool(b)) = o.get("ok") {
                *b
            } else {
                true
            }
        }
        _ => true,
    }
}

/// Run a server hook from `.relay.yaml` as **CleverScript only** (`.cs`). Node/RelayHost is not supported.
pub fn execute_repo_hook(ctx: &HookContext, hook_name: &str) -> anyhow::Result<bool> {
    let config_output = std::process::Command::new("git")
        .arg("-C")
        .arg(&ctx.repo_path)
        .arg("show")
        .arg(format!("{}:.relay.yaml", ctx.new_commit))
        .output()?;

    let config: crate::types::RelayConfig = if config_output.status.success() {
        let content = String::from_utf8_lossy(&config_output.stdout);
        serde_yaml::from_str(&content)?
    } else {
        debug!(
            "No .relay.yaml found in commit {}, skipping hooks",
            ctx.new_commit
        );
        return Ok(true);
    };

    let hook_entry = match hook_name {
        "pre-commit" | "pre-receive" | "post-receive" | "index" => config
            .server
            .as_ref()
            .and_then(|s| s.hooks.as_ref())
            .and_then(|h| h.get(hook_name)),
        _ => None,
    };

    if hook_name == "index" {
        if let Some(hp) = hook_entry {
            if let Some(ref br) = hp.branches {
                if !br.iter().any(|b| b == &ctx.branch) {
                    debug!(
                        "index hook skipped: branch {:?} not in configured list {:?}",
                        ctx.branch, br
                    );
                    return Ok(true);
                }
            }
        }
    }

    let hook_path = match hook_entry {
        Some(p) => p.path.as_str(),
        None => {
            debug!("Hook '{}' not configured in .relay.yaml", hook_name);
            return Ok(true);
        }
    };

    if !hook_path.ends_with(".cs") {
        anyhow::bail!(
            "server hook {:?} must be a CleverScript file (.cs); Node hooks ({}) are no longer supported",
            hook_name,
            hook_path
        );
    }

    if hook_name == "index" {
        meta_index::sync_meta_yaml_index(ctx)?;
    }

    let bytes = read_file_from_repo(&ctx.repo_path, &ctx.branch, hook_path)?;
    let stripped = strip_shebang_bytes(&bytes);
    let source = String::from_utf8_lossy(&stripped).into_owned();

    let context_json = serde_json::json!({
        "old_commit": ctx.old_commit,
        "new_commit": ctx.new_commit,
        "refname": ctx.refname,
        "branch": ctx.branch,
        "files": ctx.files,
        "repo_path": ctx.repo_path,
        "is_verified": ctx.is_verified
    });

    let host = CleverScriptServerHost::host_handle(ctx.repo_path.clone(), ctx.branch.clone(), false);
    let raw = relay_runtime::run_query_script(&source, host, &context_json, None, None)
        .map_err(|e| anyhow::anyhow!("CleverScript hook {}: {}", hook_name, e))?;

    Ok(hook_return_ok(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn commit_hook_repo(hook_cs: &str, relay_yaml: &str) -> (tempfile::TempDir, PathBuf, HookContext) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let cfg_oid = repo.blob(relay_yaml.as_bytes()).unwrap();
        let hook_oid = repo.blob(hook_cs.as_bytes()).unwrap();
        let mut tb_srv = repo.treebuilder(None).unwrap();
        tb_srv.insert("pre-commit.cs", hook_oid, 0o100644).unwrap();
        let srv_tid = tb_srv.write().unwrap();
        let mut tb_hooks = repo.treebuilder(None).unwrap();
        tb_hooks.insert("server", srv_tid, 0o040000).unwrap();
        let hooks_tid = tb_hooks.write().unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert(".relay.yaml", cfg_oid, 0o100644).unwrap();
        tb.insert("hooks", hooks_tid, 0o040000).unwrap();
        let tree_id = tb.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
            .to_string();
        let ctx = HookContext {
            repo_path: repo_path.clone(),
            old_commit: "0000000000000000000000000000000000000000".into(),
            new_commit: commit,
            refname: "refs/heads/main".into(),
            branch: "main".into(),
            is_verified: true,
            files: HashMap::new(),
        };
        (dir, repo_path, ctx)
    }

    #[test]
    fn strip_shebang_removes_first_line() {
        let raw = b"#!/usr/bin/env false\nexport default function run(c) { return null; }\n";
        let out = strip_shebang_bytes(raw);
        assert!(!out.starts_with(b"#!"));
        assert!(String::from_utf8_lossy(&out).contains("export default"));
    }

    #[test]
    fn execute_pre_commit_cs_succeeds_on_null_return() {
        let yaml = r#"
server:
  hooks:
    pre-commit:
      path: hooks/server/pre-commit.cs
"#;
        let cs = "export default function run(c) { return null; }\n";
        let (_dir, _path, ctx) = commit_hook_repo(cs, yaml);
        assert!(execute_repo_hook(&ctx, "pre-commit").unwrap());
    }

    #[test]
    fn execute_pre_commit_cs_shebang_ok() {
        let yaml = r#"
server:
  hooks:
    pre-commit:
      path: hooks/server/pre-commit.cs
"#;
        let cs = "#!/bin/false\nexport default function run(c) { return null; }\n";
        let (_dir, _path, ctx) = commit_hook_repo(cs, yaml);
        assert!(execute_repo_hook(&ctx, "pre-commit").unwrap());
    }

    #[test]
    fn execute_pre_commit_rejects_mjs_path() {
        let yaml = r#"
server:
  hooks:
    pre-commit:
      path: hooks/server/pre-commit.mjs
"#;
        let cs = "export default function run(c) { return null; }\n";
        let (_dir, _path, ctx) = commit_hook_repo(cs, yaml);
        let err = execute_repo_hook(&ctx, "pre-commit").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no longer supported") || msg.contains("CleverScript"),
            "{}",
            msg
        );
    }

    #[test]
    fn execute_pre_commit_false_return_fails() {
        let yaml = r#"
server:
  hooks:
    pre-commit:
      path: hooks/server/pre-commit.cs
"#;
        let cs = "export default function run(c) { return false; }\n";
        let (_dir, _path, ctx) = commit_hook_repo(cs, yaml);
        assert!(!execute_repo_hook(&ctx, "pre-commit").unwrap());
    }

    #[test]
    fn execute_unknown_hook_name_is_noop() {
        let yaml = "server:\n  hooks: {}\n";
        let cs = "export default function run(c) { return false; }\n";
        let (_dir, _path, ctx) = commit_hook_repo(cs, yaml);
        assert!(execute_repo_hook(&ctx, "not-a-real-hook").unwrap());
    }

    #[test]
    fn execute_index_skipped_when_branch_not_allowlisted() {
        let yaml = r#"
server:
  hooks:
    index:
      path: hooks/server/index.cs
      branches: ["other"]
"#;
        let cs = "export default function run(c) { return false; }\n";
        let (_dir, _path, ctx) = commit_hook_repo(cs, yaml);
        assert!(
            execute_repo_hook(&ctx, "index").unwrap(),
            "branch allowlist should skip hook without running CS"
        );
    }
}

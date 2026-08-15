//! Chat search plugin (`@relay/search/chat`): `chat_comments` in the node SearchStore,
//! incremental ingest from `.relay/chat/**`, QUERY with typed filters.
//!
//! CleverScript hooks must only route (`import { search, index } from '@relay/search/chat'`).

use crate::git::hooks::HookContext;
use crate::git::search_store::{
    store_for_bare_repo, store_key_for_bare, ChatCommentRow, SearchStore,
};
use serde_json::Value;
use std::path::Path;
use tracing::{debug, info};

pub const DEFAULT_CHAT_ROOT: &str = ".relay/chat/channels";
const CHAT_BODY_MAX_CHARS: usize = 8_192;
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

fn is_full_rebuild(old: &str) -> bool {
    old.is_empty() || old.chars().all(|c| c == '0') || old == ZERO_OID
}

fn git_ls_tree(repo: &Path, commit: &str) -> anyhow::Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("ls-tree")
        .arg("-r")
        .arg("--name-only")
        .arg(commit)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

fn git_show(repo: &Path, commit: &str, path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{commit}:{path}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_diff_name_status(
    repo: &Path,
    old: &str,
    new: &str,
) -> anyhow::Result<Vec<(char, String)>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("diff-tree")
        .arg("--no-commit-id")
        .arg("--name-status")
        .arg("-r")
        .arg(old)
        .arg(new)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git diff-tree failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        let status = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        let code = status.chars().next().unwrap_or('M');
        rows.push((code, path));
    }
    Ok(rows)
}

fn under_root(path: &str, root: &str) -> bool {
    let root = root.trim_matches('/');
    path == root || path.starts_with(&format!("{}/", root))
}

fn strip_frontmatter(content: &str) -> (&str, Option<&str>) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (content, None);
    }
    let rest = &trimmed[3..];
    if let Some(end) = rest.find("\n---") {
        let fm = rest[..end].trim();
        let body = &rest[end + 4..];
        return (body, Some(fm));
    }
    (content, None)
}

fn yaml_to_string(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Null => None,
        other => other.as_str().map(|s| s.to_string()),
    }
}

#[derive(Debug, Default)]
struct ChatRow {
    path: String,
    id: String,
    channel: String,
    author: Option<String>,
    anonymous: bool,
    created: String,
    source: String,
    signed: bool,
    body: String,
}

fn parse_chat_markdown(path: &str, text: &str) -> Option<ChatRow> {
    if !path.ends_with(".md") {
        return None;
    }
    let (body, fm) = strip_frontmatter(text);
    let body = body.trim().chars().take(CHAT_BODY_MAX_CHARS).collect::<String>();
    let fm = fm?;
    let map: serde_yaml::Mapping = serde_yaml::from_str(fm).ok()?;
    let mut row = ChatRow {
        path: path.to_string(),
        body,
        anonymous: true,
        source: "web".into(),
        ..Default::default()
    };
    for (k, v) in map {
        let Some(key) = k.as_str() else { continue };
        match key {
            "schema" => {
                let s = yaml_to_string(&v)?;
                if s != "paradigm.chat.v1" {
                    return None;
                }
            }
            "id" => row.id = yaml_to_string(&v)?,
            "channel" => row.channel = yaml_to_string(&v)?,
            "author" => row.author = yaml_to_string(&v),
            "created" => row.created = yaml_to_string(&v)?,
            "source" => row.source = yaml_to_string(&v).unwrap_or_else(|| "web".into()),
            "anonymous" => {
                row.anonymous = v.as_bool().unwrap_or_else(|| {
                    v.as_str().map(|s| s != "false" && s != "0").unwrap_or(true)
                });
            }
            "signed" => {
                row.signed = v.as_bool().unwrap_or_else(|| {
                    v.as_str().map(|s| s == "true" || s == "1").unwrap_or(false)
                });
            }
            _ => {}
        }
    }
    if row.id.is_empty() || row.channel.is_empty() || row.created.is_empty() {
        return None;
    }
    if row.anonymous {
        row.author = None;
    }
    Some(row)
}

fn to_store_row(row: ChatRow, commit: &str) -> ChatCommentRow {
    ChatCommentRow {
        path: row.path,
        id: row.id,
        channel: row.channel,
        author: row.author,
        anonymous: row.anonymous,
        created: row.created,
        source: row.source,
        signed: row.signed,
        body: row.body,
        commit_oid: Some(commit.to_string()),
    }
}

fn ingest_path_row(repo: &Path, commit: &str, path: &str) -> Option<ChatCommentRow> {
    let text = git_show(repo, commit, path)?;
    let row = parse_chat_markdown(path, &text)?;
    Some(to_store_row(row, commit))
}

/// Full rebuild of chat_comments from tip tree under `root`.
pub fn rebuild_chat(
    repo: &Path,
    branch: &str,
    commit: &str,
    root: &str,
) -> anyhow::Result<usize> {
    let store = store_for_bare_repo(repo)?;
    let key = store_key_for_bare(repo, branch);
    let root = root.trim_matches('/');
    let mut rows = Vec::new();
    for path in git_ls_tree(repo, commit)? {
        if !under_root(&path, root) || !path.ends_with(".md") {
            continue;
        }
        if let Some(row) = ingest_path_row(repo, commit, &path) {
            rows.push(row);
        }
    }
    let n = rows.len();
    store.replace_chat_comments(&key, &rows, commit, root)?;
    info!(docs = n, head = commit, root, "chat rebuild complete");
    Ok(n)
}

/// Incremental update from `old_commit`..`new_commit` under `root`.
pub fn apply_chat_diff(
    repo: &Path,
    branch: &str,
    old_commit: &str,
    new_commit: &str,
    root: &str,
) -> anyhow::Result<usize> {
    let store = store_for_bare_repo(repo)?;
    let key = store_key_for_bare(repo, branch);
    let root = root.trim_matches('/');
    let mut n = 0usize;
    for (status, path) in git_diff_name_status(repo, old_commit, new_commit)? {
        if !under_root(&path, root) || !path.ends_with(".md") {
            continue;
        }
        match status {
            'D' => {
                store.delete_chat_path(&key, &path)?;
                n += 1;
            }
            _ => {
                if let Some(row) = ingest_path_row(repo, new_commit, &path) {
                    store.upsert_chat_comment(&key, &row)?;
                } else {
                    store.delete_chat_path(&key, &path)?;
                }
                n += 1;
            }
        }
    }
    store.set_chat_indexed_head(&key, new_commit, root)?;
    debug!(changed = n, head = new_commit, "chat incremental ingest");
    Ok(n)
}

/// Ensure chat index matches tip. Full rebuild if empty; else incremental.
pub fn ensure_chat_indexed(ctx: &HookContext, root: &str) -> anyhow::Result<()> {
    let store = store_for_bare_repo(&ctx.repo_path)?;
    let key = store_key_for_bare(&ctx.repo_path, &ctx.branch);
    let indexed = store.chat_indexed_head(&key)?.unwrap_or_default();
    let count = store.chat_count(&key)?;

    if indexed == ctx.new_commit && count > 0 {
        debug!(branch = %ctx.branch, "chat index fresh");
        return Ok(());
    }

    let root = if root.is_empty() {
        DEFAULT_CHAT_ROOT
    } else {
        root
    };

    if count == 0 || is_full_rebuild(&indexed) {
        info!(
            branch = %ctx.branch,
            "chat index empty/missing; full rebuild"
        );
        rebuild_chat(&ctx.repo_path, &ctx.branch, &ctx.new_commit, root)?;
    } else {
        info!(
            branch = %ctx.branch,
            old = %indexed,
            new = %ctx.new_commit,
            "chat index stale; incremental ingest"
        );
        apply_chat_diff(
            &ctx.repo_path,
            &ctx.branch,
            &indexed,
            &ctx.new_commit,
            root,
        )?;
    }
    Ok(())
}

/// QUERY against `chat_comments` for a bare repo + branch.
pub fn query_chat_for_bare(
    bare: &Path,
    branch: &str,
    query: Option<&Value>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    let store = store_for_bare_repo(bare)?;
    let key = store_key_for_bare(bare, branch);
    store.query_chat(&key, query, limit)
}

/// Resolve chat root from search.yaml (`root:`) or default.
pub fn chat_root_from_search_yaml(repo: &Path, commit: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{commit}:search.yaml"))
        .output();
    let Ok(out) = out else {
        return DEFAULT_CHAT_ROOT.to_string();
    };
    if !out.status.success() {
        return DEFAULT_CHAT_ROOT.to_string();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("root:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    DEFAULT_CHAT_ROOT.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use serde_json::json;
    use tempfile::tempdir;

    fn commit_chat(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("r.git");
        let repo = Repository::init_bare(&repo_path).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        for (path, body) in files {
            let oid = repo.blob(body.as_bytes()).unwrap();
            index
                .add(&git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: 0o100644,
                    uid: 0,
                    gid: 0,
                    file_size: body.len() as u32,
                    id: oid,
                    flags: 0,
                    flags_extended: 0,
                    path: path.as_bytes().to_vec(),
                })
                .unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(Some("refs/heads/chat/public"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, repo_path, oid.to_string())
    }

    #[test]
    fn rebuild_and_query_by_channel() {
        let md = "---\nschema: paradigm.chat.v1\nid: A1\nchannel: general\nauthor: ari\nanonymous: false\ncreated: 2026-07-16T12:00:00.000Z\nsource: web\nsigned: false\n---\nHello body\n";
        let (_dir, repo, commit) = commit_chat(&[(
            ".relay/chat/channels/general/2026-07-16T12-00-00-000Z_00000000_A1.md",
            md,
        )]);
        let n = rebuild_chat(&repo, "chat/public", &commit, DEFAULT_CHAT_ROOT).unwrap();
        assert_eq!(n, 1);
        let rows = query_chat_for_bare(
            &repo,
            "chat/public",
            Some(&json!({"channel": "general"})),
            Some(10),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["body"], "Hello body");
        assert_eq!(rows[0]["author"], "ari");
        assert_eq!(rows[0]["id"], "A1");
    }
}

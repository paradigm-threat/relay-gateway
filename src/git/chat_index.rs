//! Chat search plugin (`@relay/search/chat`): dedicated `chat_comments` table in the
//! branch SQLite DB, incremental ingest from `.relay/chat/**`, QUERY with typed filters.
//!
//! CleverScript hooks must only route (`import { search, index } from '@relay/search/chat'`).

use crate::git::hooks::HookContext;
use crate::git::indexing::branch_sqlite_path;
use crate::git::sqlite_index;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::path::Path;
use tracing::{debug, info};

pub const DEFAULT_CHAT_ROOT: &str = ".relay/chat/channels";
const CHAT_BODY_MAX_CHARS: usize = 8_192;

fn open_chat_db(db_path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS chat_comments (
            path TEXT PRIMARY KEY NOT NULL,
            id TEXT NOT NULL,
            channel TEXT NOT NULL,
            author TEXT,
            anonymous INTEGER NOT NULL DEFAULT 1,
            created TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'web',
            signed INTEGER NOT NULL DEFAULT 0,
            body TEXT NOT NULL,
            commit_oid TEXT
        );
        CREATE INDEX IF NOT EXISTS chat_comments_channel_created
            ON chat_comments(channel, created DESC);
        CREATE VIRTUAL TABLE IF NOT EXISTS chat_comments_fts USING fts5(
            body, author, channel, id, path UNINDEXED
        );
        ",
    )?;
    Ok(conn)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn read_meta(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v)
}

fn chat_row_count(conn: &Connection) -> anyhow::Result<i64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM chat_comments", [], |r| r.get(0))?;
    Ok(n)
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

fn upsert_row(conn: &Connection, row: &ChatRow, commit_oid: &str) -> anyhow::Result<()> {
    let _ = conn.execute(
        "DELETE FROM chat_comments_fts WHERE path = ?1",
        params![row.path],
    );
    conn.execute(
        "INSERT INTO chat_comments(path, id, channel, author, anonymous, created, source, signed, body, commit_oid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(path) DO UPDATE SET
           id=excluded.id,
           channel=excluded.channel,
           author=excluded.author,
           anonymous=excluded.anonymous,
           created=excluded.created,
           source=excluded.source,
           signed=excluded.signed,
           body=excluded.body,
           commit_oid=excluded.commit_oid",
        params![
            row.path,
            row.id,
            row.channel,
            row.author,
            if row.anonymous { 1 } else { 0 },
            row.created,
            row.source,
            if row.signed { 1 } else { 0 },
            row.body,
            commit_oid,
        ],
    )?;
    conn.execute(
        "INSERT INTO chat_comments_fts(body, author, channel, id, path) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row.body,
            row.author.clone().unwrap_or_default(),
            row.channel,
            row.id,
            row.path,
        ],
    )?;
    Ok(())
}

fn delete_path(conn: &Connection, path: &str) -> anyhow::Result<()> {
    let _ = conn.execute(
        "DELETE FROM chat_comments_fts WHERE path = ?1",
        params![path],
    );
    conn.execute("DELETE FROM chat_comments WHERE path = ?1", params![path])?;
    Ok(())
}

fn ingest_path(
    conn: &Connection,
    repo: &Path,
    commit: &str,
    path: &str,
) -> anyhow::Result<()> {
    let Some(text) = git_show(repo, commit, path) else {
        delete_path(conn, path)?;
        return Ok(());
    };
    let Some(row) = parse_chat_markdown(path, &text) else {
        delete_path(conn, path)?;
        return Ok(());
    };
    upsert_row(conn, &row, commit)?;
    Ok(())
}

/// Full rebuild of chat_comments from tip tree under `root`.
pub fn rebuild_chat(
    repo: &Path,
    commit: &str,
    db_path: &Path,
    root: &str,
) -> anyhow::Result<usize> {
    let conn = open_chat_db(db_path)?;
    conn.execute("DELETE FROM chat_comments", [])?;
    conn.execute("DELETE FROM chat_comments_fts", [])?;
    let root = root.trim_matches('/');
    let mut n = 0usize;
    for path in git_ls_tree(repo, commit)? {
        if !under_root(&path, root) || !path.ends_with(".md") {
            continue;
        }
        ingest_path(&conn, repo, commit, &path)?;
        n += 1;
    }
    set_meta(&conn, "indexed_head", commit)?;
    set_meta(&conn, "chat_root", root)?;
    info!(docs = n, head = commit, root, "chat rebuild complete");
    Ok(n)
}

/// Incremental update from `old_commit`..`new_commit` under `root`.
pub fn apply_chat_diff(
    repo: &Path,
    old_commit: &str,
    new_commit: &str,
    db_path: &Path,
    root: &str,
) -> anyhow::Result<usize> {
    let conn = open_chat_db(db_path)?;
    let root = root.trim_matches('/');
    let mut n = 0usize;
    for (status, path) in git_diff_name_status(repo, old_commit, new_commit)? {
        if !under_root(&path, root) || !path.ends_with(".md") {
            continue;
        }
        match status {
            'D' => {
                delete_path(&conn, &path)?;
                n += 1;
            }
            _ => {
                ingest_path(&conn, repo, new_commit, &path)?;
                n += 1;
            }
        }
    }
    set_meta(&conn, "indexed_head", new_commit)?;
    set_meta(&conn, "chat_root", root)?;
    debug!(changed = n, head = new_commit, "chat incremental ingest");
    Ok(n)
}

/// Ensure chat index matches tip. Full rebuild if empty; else incremental.
pub fn ensure_chat_indexed(
    ctx: &HookContext,
    root: &str,
) -> anyhow::Result<()> {
    let db_path = branch_sqlite_path(&ctx.repo_path, &ctx.branch);
    let conn = open_chat_db(&db_path)?;
    let indexed = read_meta(&conn, "indexed_head")?.unwrap_or_default();
    let count = chat_row_count(&conn)?;
    drop(conn);

    if indexed == ctx.new_commit && count > 0 {
        debug!(branch = %ctx.branch, "chat index fresh");
        return Ok(());
    }

    let root = if root.is_empty() {
        DEFAULT_CHAT_ROOT
    } else {
        root
    };

    if count == 0 || indexed.is_empty() {
        info!(
            branch = %ctx.branch,
            "chat index empty/missing; full rebuild"
        );
        rebuild_chat(&ctx.repo_path, &ctx.new_commit, &db_path, root)?;
    } else {
        info!(
            branch = %ctx.branch,
            old = %indexed,
            new = %ctx.new_commit,
            "chat index stale; incremental ingest"
        );
        apply_chat_diff(
            &ctx.repo_path,
            &indexed,
            &ctx.new_commit,
            &db_path,
            root,
        )?;
    }
    // Keep generic indexed_head in sync for ensure_indexed early-return.
    let _ = sqlite_index::read_indexed_head(&db_path);
    {
        let conn = open_chat_db(&db_path)?;
        set_meta(&conn, "indexed_head", &ctx.new_commit)?;
    }
    Ok(())
}

fn fts_match_query(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let cleaned: String = t
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if cleaned.is_empty() {
                return String::new();
            }
            format!("\"{}\"", cleaned.replace('"', ""))
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn row_to_json(
    path: String,
    id: String,
    channel: String,
    author: Option<String>,
    anonymous: i64,
    created: String,
    source: String,
    signed: i64,
    body: String,
) -> Value {
    let mut m = Map::new();
    m.insert("path".into(), Value::String(path));
    m.insert("id".into(), Value::String(id));
    m.insert("channel".into(), Value::String(channel));
    m.insert("schema".into(), Value::String("paradigm.chat.v1".into()));
    if let Some(a) = author {
        if !a.is_empty() {
            m.insert("author".into(), Value::String(a));
        }
    }
    m.insert("anonymous".into(), Value::Bool(anonymous != 0));
    m.insert("created".into(), Value::String(created));
    m.insert("source".into(), Value::String(source));
    m.insert("signed".into(), Value::Bool(signed != 0));
    m.insert("body".into(), Value::String(body));
    Value::Object(m)
}

/// QUERY against `chat_comments`.
pub fn query_chat(
    db_path: &Path,
    query: Option<&Value>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let conn = open_chat_db(db_path)?;
    let lim = limit.unwrap_or(50).max(1) as i64;
    let mut rows = Vec::new();

    match query {
        Some(Value::Object(obj)) => {
            if let Some(ch) = obj.get("channel").and_then(|v| v.as_str()) {
                let mut stmt = conn.prepare(
                    "SELECT path, id, channel, author, anonymous, created, source, signed, body
                     FROM chat_comments
                     WHERE channel = ?1
                     ORDER BY created DESC
                     LIMIT ?2",
                )?;
                let iter = stmt.query_map(params![ch, lim], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?;
                for item in iter {
                    let (path, id, channel, author, anonymous, created, source, signed, body) =
                        item?;
                    rows.push(row_to_json(
                        path, id, channel, author, anonymous, created, source, signed, body,
                    ));
                }
            } else if let Some(author) = obj.get("author").and_then(|v| v.as_str()) {
                let mut stmt = conn.prepare(
                    "SELECT path, id, channel, author, anonymous, created, source, signed, body
                     FROM chat_comments
                     WHERE anonymous = 0 AND lower(author) = lower(?1)
                     ORDER BY created DESC
                     LIMIT ?2",
                )?;
                let iter = stmt.query_map(params![author, lim], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?;
                for item in iter {
                    let (path, id, channel, author, anonymous, created, source, signed, body) =
                        item?;
                    rows.push(row_to_json(
                        path, id, channel, author, anonymous, created, source, signed, body,
                    ));
                }
            } else {
                // Unknown object filter → empty recent
                let mut stmt = conn.prepare(
                    "SELECT path, id, channel, author, anonymous, created, source, signed, body
                     FROM chat_comments ORDER BY created DESC LIMIT ?1",
                )?;
                let iter = stmt.query_map(params![lim], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?;
                for item in iter {
                    let (path, id, channel, author, anonymous, created, source, signed, body) =
                        item?;
                    rows.push(row_to_json(
                        path, id, channel, author, anonymous, created, source, signed, body,
                    ));
                }
            }
        }
        Some(Value::String(q)) if !q.trim().is_empty() => {
            let match_q = fts_match_query(q);
            if match_q.is_empty() {
                return Ok(vec![]);
            }
            let mut stmt = conn.prepare(
                "SELECT c.path, c.id, c.channel, c.author, c.anonymous, c.created, c.source, c.signed, c.body
                 FROM chat_comments c
                 INNER JOIN chat_comments_fts f ON f.path = c.path
                 WHERE chat_comments_fts MATCH ?1
                 ORDER BY c.created DESC
                 LIMIT ?2",
            )?;
            let iter = stmt.query_map(params![match_q, lim], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })?;
            for item in iter {
                let (path, id, channel, author, anonymous, created, source, signed, body) = item?;
                rows.push(row_to_json(
                    path, id, channel, author, anonymous, created, source, signed, body,
                ));
            }
        }
        _ => {
            let mut stmt = conn.prepare(
                "SELECT path, id, channel, author, anonymous, created, source, signed, body
                 FROM chat_comments ORDER BY created DESC LIMIT ?1",
            )?;
            let iter = stmt.query_map(params![lim], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })?;
            for item in iter {
                let (path, id, channel, author, anonymous, created, source, signed, body) = item?;
                rows.push(row_to_json(
                    path, id, channel, author, anonymous, created, source, signed, body,
                ));
            }
        }
    }
    Ok(rows)
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
    // Minimal parse: look for `root: ...` at top level
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
        let db = repo.join("chat.db");
        let n = rebuild_chat(&repo, &commit, &db, DEFAULT_CHAT_ROOT).unwrap();
        assert_eq!(n, 1);
        let rows = query_chat(&db, Some(&json!({"channel": "general"})), Some(10)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["body"], "Hello body");
        assert_eq!(rows[0]["author"], "ari");
        assert_eq!(rows[0]["id"], "A1");
    }
}

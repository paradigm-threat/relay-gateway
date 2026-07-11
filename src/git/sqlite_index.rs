//! Default SQLite search/index plugin (`index.db` under `.relay_data/branches/<hash>/`).
//!
//! Heavy work stays in Rust: schema, FTS/filter QUERY, and rebuild from JSON collections /
//! `meta.yaml` sync output. CleverScript must only route (plugin id + args).

use crate::git::indexing::branch_data_dir;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// `.relay_data/branches/<hash>/index.db`
pub fn sqlite_db_path(repo_path: &Path, branch: &str) -> PathBuf {
    branch_data_dir(repo_path, branch).join("index.db")
}

fn open_db(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS docs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            collection TEXT NOT NULL,
            doc_id TEXT NOT NULL,
            json TEXT NOT NULL,
            search_text TEXT NOT NULL DEFAULT '',
            UNIQUE(collection, doc_id)
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            search_text,
            content='docs',
            content_rowid='id'
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

/// `metadata.indexed_head` from SQLite, if present.
pub fn read_indexed_head(db_path: &Path) -> anyhow::Result<Option<String>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(db_path)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'indexed_head'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v)
}

fn doc_id_for(doc: &Value, fallback_index: usize) -> String {
    if let Some(s) = doc.get("path").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = doc.get("_meta_dir").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = doc.get("id").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    format!("row-{fallback_index}")
}

fn search_text_for(doc: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for key in ["title", "_title", "path", "name", "description", "summary"] {
        if let Some(s) = doc.get(key).and_then(|v| v.as_str()) {
            parts.push(s.to_string());
        }
    }
    if let Some(arr) = doc
        .get("_keywords")
        .or_else(|| doc.get("keywords"))
        .and_then(|v| v.as_array())
    {
        for k in arr {
            if let Some(s) = k.as_str() {
                parts.push(s.to_string());
            }
        }
    }
    if let Some(s) = doc.get("searchText").and_then(|v| v.as_str()) {
        parts.push(s.to_string());
    }
    // Flatten remaining string fields for simple contains-style FTS.
    if let Some(obj) = doc.as_object() {
        for (k, v) in obj {
            if k.starts_with('_') && k != "_title" && k != "_keywords" && k != "_meta_dir" {
                continue;
            }
            if let Some(s) = v.as_str() {
                if !parts.iter().any(|p| p == s) {
                    parts.push(s.to_string());
                }
            }
        }
    }
    parts.join(" ")
}

fn replace_collection(conn: &Connection, collection: &str, docs: &[Value]) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM docs_fts WHERE rowid IN (SELECT id FROM docs WHERE collection = ?1)",
        params![collection],
    )?;
    conn.execute(
        "DELETE FROM docs WHERE collection = ?1",
        params![collection],
    )?;
    for (i, doc) in docs.iter().enumerate() {
        let doc_id = doc_id_for(doc, i);
        let json = serde_json::to_string(doc)?;
        let search_text = search_text_for(doc);
        conn.execute(
            "INSERT INTO docs(collection, doc_id, json, search_text) VALUES (?1, ?2, ?3, ?4)",
            params![collection, doc_id, json, search_text],
        )?;
        let id: i64 = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO docs_fts(rowid, search_text) VALUES (?1, ?2)",
            params![id, search_text],
        )?;
    }
    Ok(())
}

/// Rebuild SQLite collections from `index.db.json` and stamp `indexed_head`.
pub fn sync_json_to_sqlite(json_path: &Path, db_path: &Path) -> anyhow::Result<()> {
    if !json_path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(json_path)?;
    let v: Value = serde_json::from_str(&text)?;
    let conn = open_db(db_path)?;
    let tx = conn.unchecked_transaction()?;

    if let Some(map) = v.get("collections").and_then(|c| c.as_object()) {
        for (name, val) in map {
            let docs = match val {
                Value::Array(a) => a.clone(),
                _ => continue,
            };
            replace_collection(&tx, name, &docs)?;
        }
    }

    if let Some(head) = v
        .get("metadata")
        .and_then(|x| x.get("indexed_head"))
        .and_then(|x| x.as_str())
    {
        set_meta(&tx, "indexed_head", head)?;
    }

    tx.commit()?;
    Ok(())
}

/// Upsert a single collection from an in-memory document list (e.g. flattened `index.json`).
pub fn replace_collection_docs(
    db_path: &Path,
    collection: &str,
    docs: &[Value],
    indexed_head: Option<&str>,
) -> anyhow::Result<()> {
    let conn = open_db(db_path)?;
    let tx = conn.unchecked_transaction()?;
    replace_collection(&tx, collection, docs)?;
    if let Some(head) = indexed_head {
        set_meta(&tx, "indexed_head", head)?;
    }
    tx.commit()?;
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

fn project_row(row: Value, fields: Option<&[String]>) -> Value {
    let Some(field_list) = fields.filter(|f| !f.is_empty()) else {
        return row;
    };
    let mut m = Map::new();
    if let Some(obj) = row.as_object() {
        for k in field_list {
            if let Some(v) = obj.get(k) {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

fn row_matches_object_filter(item: &Value, q_obj: &Map<String, Value>) -> bool {
    let Some(item_obj) = item.as_object() else {
        return false;
    };
    for (k, v) in q_obj {
        if item_obj.get(k) != Some(v) {
            return false;
        }
    }
    true
}

/// QUERY against SQLite: string → FTS AND; object → exact field match; null → all rows.
pub fn query_collection(
    db_path: &Path,
    collection: &str,
    query: Option<&Value>,
    fields: Option<&[String]>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let conn = Connection::open(db_path)?;
    let lim = limit.unwrap_or(10_000).max(1);

    let mut rows: Vec<Value> = Vec::new();

    match query {
        Some(Value::String(q_str)) if !q_str.trim().is_empty() => {
            let match_q = fts_match_query(q_str);
            if match_q.is_empty() {
                return Ok(vec![]);
            }
            let mut stmt = conn.prepare(
                "SELECT d.json FROM docs d
                 WHERE d.collection = ?1
                   AND d.id IN (
                     SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?2
                   )
                 LIMIT ?3",
            )?;
            let iter = stmt.query_map(params![collection, match_q, lim as i64], |r| {
                let json: String = r.get(0)?;
                Ok(json)
            })?;
            for item in iter {
                let json = item?;
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    rows.push(project_row(v, fields));
                }
            }
        }
        Some(Value::Object(q_obj)) => {
            let mut stmt = conn.prepare("SELECT json FROM docs WHERE collection = ?1")?;
            let iter = stmt.query_map(params![collection], |r| {
                let json: String = r.get(0)?;
                Ok(json)
            })?;
            for item in iter {
                let json = item?;
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    if row_matches_object_filter(&v, q_obj) {
                        rows.push(project_row(v, fields));
                        if rows.len() >= lim {
                            break;
                        }
                    }
                }
            }
        }
        _ => {
            let mut stmt = conn.prepare(
                "SELECT json FROM docs WHERE collection = ?1 LIMIT ?2",
            )?;
            let iter = stmt.query_map(params![collection, lim as i64], |r| {
                let json: String = r.get(0)?;
                Ok(json)
            })?;
            for item in iter {
                let json = item?;
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    rows.push(project_row(v, fields));
                }
            }
        }
    }

    Ok(rows)
}

/// Flatten nested paradigm-threat / catalog `index.json` into searchable docs.
pub fn flatten_path_index(root: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    fn walk(node: &Value, prefix: &str, out: &mut Vec<Value>) {
        let Some(obj) = node.as_object() else {
            return;
        };
        // Leaf entry: has _keywords / _title / _updated without nested path children as maps of maps
        let looks_like_entry = obj.contains_key("_keywords")
            || obj.contains_key("_title")
            || obj.contains_key("_updated");
        let has_nested = obj.values().any(|v| {
            v.as_object()
                .map(|o| {
                    o.keys()
                        .any(|k| !k.starts_with('_'))
                })
                .unwrap_or(false)
        });

        if looks_like_entry && !prefix.is_empty() && (!has_nested || obj.contains_key("_title")) {
            let mut doc = obj.clone();
            doc.insert("path".to_string(), Value::String(prefix.to_string()));
            if let Some(t) = obj.get("_title").cloned() {
                doc.insert("title".to_string(), t);
            }
            out.push(Value::Object(doc));
        }

        for (k, v) in obj {
            if k.starts_with('_') {
                continue;
            }
            if let Some(child) = v.as_object() {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}/{k}")
                };
                // If child is a leaf file entry
                let child_leaf = child.contains_key("_keywords")
                    || child.contains_key("_title")
                    || child.contains_key("_updated");
                let child_nested = child.keys().any(|ck| !ck.starts_with('_'));
                if child_leaf && !child_nested {
                    let mut doc = child.clone();
                    doc.insert("path".to_string(), Value::String(path));
                    if let Some(t) = child.get("_title").cloned() {
                        doc.insert("title".to_string(), t);
                    } else if !doc.contains_key("title") {
                        doc.insert("title".to_string(), Value::String(k.clone()));
                    }
                    out.push(Value::Object(doc));
                } else {
                    walk(v, &path, out);
                }
            }
        }
    }
    walk(root, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn sync_and_fts_query() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "abc" },
                "collections": {
                    "index": [
                        { "title": "Mud Flood", "_meta_dir": "data/mud", "keywords": ["geology"] },
                        { "title": "Other", "_meta_dir": "data/other" }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();

        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        assert_eq!(read_indexed_head(&db_path).unwrap().as_deref(), Some("abc"));

        let hits = query_collection(
            &db_path,
            "index",
            Some(&Value::String("mud".into())),
            None,
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], "Mud Flood");

        let eq = query_collection(
            &db_path,
            "index",
            Some(&json!({ "title": "Other" })),
            Some(&["title".into()]),
            None,
        )
        .unwrap();
        assert_eq!(eq.len(), 1);
        assert!(eq[0].get("_meta_dir").is_none());
        assert_eq!(eq[0]["title"], "Other");
    }

    #[test]
    fn fts_and_requires_all_terms() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "h" },
                "collections": {
                    "index": [
                        { "title": "Mud Flood Geology", "_meta_dir": "a" },
                        { "title": "Mud Only", "_meta_dir": "b" },
                        { "title": "Flood Only", "_meta_dir": "c" }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        let hits = query_collection(
            &db_path,
            "index",
            Some(&Value::String("mud flood".into())),
            None,
            Some(10),
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], "Mud Flood Geology");
    }

    #[test]
    fn query_limit_caps_results() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        let docs: Vec<_> = (0..20)
            .map(|i| json!({ "title": format!("Item {i}"), "_meta_dir": format!("d{i}") }))
            .collect();
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "h" },
                "collections": { "index": docs }
            })
            .to_string(),
        )
        .unwrap();
        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        let hits = query_collection(&db_path, "index", None, None, Some(3)).unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn empty_db_returns_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("missing.db");
        let hits = query_collection(
            &db_path,
            "index",
            Some(&Value::String("x".into())),
            None,
            None,
        )
        .unwrap();
        assert!(hits.is_empty());
    }
}

//! Default SQLite search/index plugin (`index.db` under `.relay_data/branches/<hash>/`).
//!
//! Heavy work stays in Rust: schema, FTS/filter QUERY, and rebuild from JSON collections /
//! `meta.yaml` sync output. CleverScript must only route (plugin id + args).

use crate::git::indexing::branch_data_dir;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use tracing::info;

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
        CREATE INDEX IF NOT EXISTS docs_collection_sort_key
            ON docs(collection, json_extract(json, '$.sort_key'));
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

/// Number of docs in a collection (0 if DB missing).
pub fn count_collection_docs(db_path: &Path, collection: &str) -> anyhow::Result<u64> {
    if !db_path.exists() {
        return Ok(0);
    }
    let conn = Connection::open(db_path)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM docs WHERE collection = ?1",
        params![collection],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}

fn doc_id_for(doc: &Value, fallback_index: usize, id_fields: Option<&[String]>) -> String {
    if let Some(fields) = id_fields {
        for k in fields {
            if let Some(s) = doc.get(k).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
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

fn search_text_for(doc: &Value, fts_fields: Option<&[String]>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(fields) = fts_fields {
        for key in fields {
            if let Some(s) = doc.get(key).and_then(|v| v.as_str()) {
                parts.push(s.to_string());
            } else if let Some(arr) = doc.get(key).and_then(|v| v.as_array()) {
                for k in arr {
                    if let Some(s) = k.as_str() {
                        parts.push(s.to_string());
                    }
                }
            }
        }
        return parts.join(" ");
    }
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

fn insert_doc(
    conn: &Connection,
    collection: &str,
    doc: &Value,
    index: usize,
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    let doc_id = doc_id_for(doc, index, id_fields);
    let json = serde_json::to_string(doc)?;
    let search_text = search_text_for(doc, fts_fields);
    conn.execute(
        "INSERT INTO docs(collection, doc_id, json, search_text) VALUES (?1, ?2, ?3, ?4)",
        params![collection, doc_id, json, search_text],
    )?;
    let id: i64 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO docs_fts(rowid, search_text) VALUES (?1, ?2)",
        params![id, search_text],
    )?;
    Ok(())
}

fn upsert_doc(
    conn: &Connection,
    collection: &str,
    doc: &Value,
    index: usize,
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    let doc_id = doc_id_for(doc, index, id_fields);
    let json = serde_json::to_string(doc)?;
    let search_text = search_text_for(doc, fts_fields);
    if let Ok(old_id) = conn.query_row(
        "SELECT id FROM docs WHERE collection = ?1 AND doc_id = ?2",
        params![collection, doc_id],
        |r| r.get::<_, i64>(0),
    ) {
        let _ = conn.execute("DELETE FROM docs_fts WHERE rowid = ?1", params![old_id]);
        conn.execute(
            "UPDATE docs SET json = ?1, search_text = ?2 WHERE id = ?3",
            params![json, search_text, old_id],
        )?;
        conn.execute(
            "INSERT INTO docs_fts(rowid, search_text) VALUES (?1, ?2)",
            params![old_id, search_text],
        )?;
    } else {
        insert_doc(conn, collection, doc, index, id_fields, fts_fields)?;
    }
    Ok(())
}

fn replace_collection(
    conn: &Connection,
    collection: &str,
    docs: &[Value],
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM docs_fts WHERE rowid IN (SELECT id FROM docs WHERE collection = ?1)",
        params![collection],
    )?;
    conn.execute(
        "DELETE FROM docs WHERE collection = ?1",
        params![collection],
    )?;
    for (i, doc) in docs.iter().enumerate() {
        insert_doc(conn, collection, doc, i, id_fields, fts_fields)?;
    }
    Ok(())
}

fn merge_collection(
    conn: &Connection,
    collection: &str,
    docs: &[Value],
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    for (i, doc) in docs.iter().enumerate() {
        upsert_doc(conn, collection, doc, i, id_fields, fts_fields)?;
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
            replace_collection(&tx, name, &docs, None, None)?;
            info!(collection = %name, docs = docs.len(), "sqlite sync_json replace");
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

/// Replace a single collection from an in-memory document list.
pub fn replace_collection_docs(
    db_path: &Path,
    collection: &str,
    docs: &[Value],
    indexed_head: Option<&str>,
) -> anyhow::Result<()> {
    replace_collection_docs_with_fields(db_path, collection, docs, indexed_head, None, None)
}

pub fn replace_collection_docs_with_fields(
    db_path: &Path,
    collection: &str,
    docs: &[Value],
    indexed_head: Option<&str>,
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    let conn = open_db(db_path)?;
    let tx = conn.unchecked_transaction()?;
    replace_collection(&tx, collection, docs, id_fields, fts_fields)?;
    if let Some(head) = indexed_head {
        set_meta(&tx, "indexed_head", head)?;
    }
    tx.commit()?;
    info!(
        collection = %collection,
        docs = docs.len(),
        head = indexed_head.unwrap_or(""),
        "sqlite replace_collection"
    );
    Ok(())
}

/// Merge/upsert docs into a collection without wiping other rows.
pub fn merge_collection_docs_with_fields(
    db_path: &Path,
    collection: &str,
    docs: &[Value],
    indexed_head: Option<&str>,
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    let conn = open_db(db_path)?;
    let tx = conn.unchecked_transaction()?;
    merge_collection(&tx, collection, docs, id_fields, fts_fields)?;
    if let Some(head) = indexed_head {
        set_meta(&tx, "indexed_head", head)?;
    }
    tx.commit()?;
    info!(
        collection = %collection,
        docs = docs.len(),
        head = indexed_head.unwrap_or(""),
        "sqlite merge_collection"
    );
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

/// Max values in an object-filter `IN` / `$ne` array (DoS guard).
pub const QUERY_FILTER_IN_MAX: usize = 32;

/// Predicate for one allowlisted JSON field in an object QUERY.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldPred {
    /// Exact string equality.
    Eq(String),
    /// Not equal to this string (missing field also matches).
    Ne(String),
    /// Field equals any of these strings.
    In(Vec<String>),
    /// Field equals none of these strings (missing field matches).
    Nin(Vec<String>),
}

/// True when `name` is safe to embed in a fixed `json_extract(..., '$.name')` template.
fn is_safe_json_field(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn string_list_from_array(arr: &[Value]) -> Option<Vec<String>> {
    if arr.is_empty() || arr.len() > QUERY_FILTER_IN_MAX {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let Value::String(s) = v else {
            return None;
        };
        if s.is_empty() || s.len() > 256 {
            return None;
        }
        out.push(s.clone());
    }
    Some(out)
}

/// Parse object QUERY predicates. Unsupported shapes → empty (no matches).
fn parse_object_predicates(q_obj: &Map<String, Value>) -> Option<Vec<(String, FieldPred)>> {
    if q_obj.is_empty() {
        return Some(Vec::new());
    }
    let mut preds = Vec::with_capacity(q_obj.len());
    for (key, val) in q_obj {
        if !is_safe_json_field(key) {
            return None;
        }
        let pred = match val {
            Value::String(s) if !s.is_empty() && s.len() <= 256 => FieldPred::Eq(s.clone()),
            Value::Array(arr) => FieldPred::In(string_list_from_array(arr)?),
            Value::Object(op) => {
                if op.len() != 1 {
                    return None;
                }
                if let Some(v) = op.get("$ne") {
                    match v {
                        Value::String(s) if !s.is_empty() && s.len() <= 256 => {
                            FieldPred::Ne(s.clone())
                        }
                        Value::Array(arr) => FieldPred::Nin(string_list_from_array(arr)?),
                        _ => return None,
                    }
                } else if let Some(v) = op.get("$in") {
                    let Value::Array(arr) = v else {
                        return None;
                    };
                    FieldPred::In(string_list_from_array(arr)?)
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        preds.push((key.clone(), pred));
    }
    Some(preds)
}

fn field_value_str<'a>(item_obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    item_obj.get(key).and_then(|v| v.as_str())
}

fn row_matches_predicates(item: &Value, preds: &[(String, FieldPred)]) -> bool {
    let Some(item_obj) = item.as_object() else {
        return false;
    };
    for (key, pred) in preds {
        let got = field_value_str(item_obj, key);
        let ok = match pred {
            FieldPred::Eq(want) => got == Some(want.as_str()),
            FieldPred::Ne(want) => got != Some(want.as_str()),
            FieldPred::In(want) => got.is_some_and(|g| want.iter().any(|w| w == g)),
            FieldPred::Nin(want) => !got.is_some_and(|g| want.iter().any(|w| w == g)),
        };
        if !ok {
            return false;
        }
    }
    true
}

fn row_matches_object_filter(item: &Value, q_obj: &Map<String, Value>) -> bool {
    match parse_object_predicates(q_obj) {
        Some(preds) => row_matches_predicates(item, &preds),
        None => false,
    }
}

/// Build `AND json_extract(json, '$.field') …` clauses with bound params.
/// Field names are allowlisted identifiers only (never user-controlled SQL).
fn object_filter_sql(
    preds: &[(String, FieldPred)],
) -> Option<(String, Vec<String>)> {
    let mut sql = String::new();
    let mut binds = Vec::new();
    for (key, pred) in preds {
        // `key` already passed `is_safe_json_field`.
        let extract = format!("json_extract(json, '$.{key}')");
        match pred {
            FieldPred::Eq(v) => {
                sql.push_str(&format!(" AND {extract} = ?"));
                binds.push(v.clone());
            }
            FieldPred::Ne(v) => {
                // NULL field ≠ value (SQL NULL = x is unknown).
                sql.push_str(&format!(" AND ({extract} IS NULL OR {extract} != ?)"));
                binds.push(v.clone());
            }
            FieldPred::In(vals) => {
                if vals.is_empty() {
                    return None;
                }
                let placeholders = vec!["?"; vals.len()].join(", ");
                sql.push_str(&format!(" AND {extract} IN ({placeholders})"));
                binds.extend(vals.iter().cloned());
            }
            FieldPred::Nin(vals) => {
                if vals.is_empty() {
                    return None;
                }
                let placeholders = vec!["?"; vals.len()].join(", ");
                sql.push_str(&format!(
                    " AND ({extract} IS NULL OR {extract} NOT IN ({placeholders}))"
                ));
                binds.extend(vals.iter().cloned());
            }
        }
    }
    Some((sql, binds))
}

fn order_by_sql_fragment(order: QueryOrder) -> &'static str {
    match order {
        QueryOrder::Random => "ORDER BY RANDOM()",
        QueryOrder::PathAsc => "ORDER BY json_extract(json, '$.path') ASC",
        QueryOrder::PathDesc => "ORDER BY json_extract(json, '$.path') DESC",
        QueryOrder::SortKeyAsc => {
            "ORDER BY coalesce(
               json_extract(json, '$.sort_key'),
               json_extract(json, '$.md_path'),
               json_extract(json, '$.path')
             ) ASC, doc_id ASC"
        }
        QueryOrder::SortKeyDesc => {
            "ORDER BY coalesce(
               json_extract(json, '$.sort_key'),
               json_extract(json, '$.md_path'),
               json_extract(json, '$.path')
             ) DESC, doc_id DESC"
        }
    }
}

/// Sort mode for QUERY. Selected via fixed SQL templates only (never interpolated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryOrder {
    /// `ORDER BY path ASC`
    #[default]
    PathAsc,
    /// `ORDER BY path DESC`.
    PathDesc,
    /// `ORDER BY sort_key ASC`, then stable document id.
    SortKeyAsc,
    /// `ORDER BY sort_key DESC`, then stable document id.
    SortKeyDesc,
    /// `ORDER BY RANDOM()` — full scan; callers should cap `limit` (see [`clamp_query_limit`]).
    Random,
}

impl QueryOrder {
    /// Parse allowlisted order tokens: `path`, `-path`, `sort_key`, `-sort_key`, `random`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "path" | "" => Ok(Self::PathAsc),
            "-path" => Ok(Self::PathDesc),
            "sort_key" => Ok(Self::SortKeyAsc),
            "-sort_key" => Ok(Self::SortKeyDesc),
            "random" => Ok(Self::Random),
            other => Err(format!(
                "invalid order {other:?}; expected path, -path, sort_key, -sort_key, or random"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathAsc => "path",
            Self::PathDesc => "-path",
            Self::SortKeyAsc => "sort_key",
            Self::SortKeyDesc => "-sort_key",
            Self::Random => "random",
        }
    }
}

/// Max rows for a normal QUERY.
pub const QUERY_LIMIT_MAX: usize = 10_000;
/// Hard cap when `order=random` (full collection shuffle is expensive).
pub const QUERY_RANDOM_LIMIT_MAX: usize = 100;

/// Clamp limit for the given order. Random defaults to 1 when omitted.
pub fn clamp_query_limit(order: QueryOrder, limit: Option<usize>) -> usize {
    match order {
        QueryOrder::Random => limit.unwrap_or(1).clamp(1, QUERY_RANDOM_LIMIT_MAX),
        _ => limit.unwrap_or(QUERY_LIMIT_MAX).clamp(1, QUERY_LIMIT_MAX),
    }
}

/// QUERY against SQLite: string → FTS AND; object → field predicates
/// (`eq` / `IN` array / `$ne` / `$in`); null/empty → all rows.
pub fn query_collection(
    db_path: &Path,
    collection: &str,
    query: Option<&Value>,
    fields: Option<&[String]>,
    limit: Option<usize>,
) -> anyhow::Result<Vec<Value>> {
    query_collection_ordered(
        db_path,
        collection,
        query,
        fields,
        limit,
        QueryOrder::default(),
    )
}

/// Like [`query_collection`] with an explicit [`QueryOrder`].
pub fn query_collection_ordered(
    db_path: &Path,
    collection: &str,
    query: Option<&Value>,
    fields: Option<&[String]>,
    limit: Option<usize>,
    order: QueryOrder,
) -> anyhow::Result<Vec<Value>> {
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let conn = Connection::open(db_path)?;
    let lim = clamp_query_limit(order, limit);

    let mut rows: Vec<Value> = Vec::new();

    match query {
        Some(Value::String(q_str)) if !q_str.trim().is_empty() => {
            let match_q = fts_match_query(q_str);
            if match_q.is_empty() {
                return Ok(vec![]);
            }
            let sql = match order {
                QueryOrder::Random => {
                    "SELECT d.json FROM docs d
                     WHERE d.collection = ?1
                       AND d.id IN (
                         SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?2
                       )
                     ORDER BY RANDOM()
                     LIMIT ?3"
                }
                QueryOrder::PathAsc => {
                    "SELECT d.json FROM docs d
                     WHERE d.collection = ?1
                       AND d.id IN (
                         SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?2
                       )
                     ORDER BY json_extract(d.json, '$.path') ASC
                     LIMIT ?3"
                }
                QueryOrder::PathDesc => {
                    "SELECT d.json FROM docs d
                     WHERE d.collection = ?1
                       AND d.id IN (
                         SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?2
                       )
                     ORDER BY json_extract(d.json, '$.path') DESC
                     LIMIT ?3"
                }
                QueryOrder::SortKeyAsc => {
                    "SELECT d.json FROM docs d
                     WHERE d.collection = ?1
                       AND d.id IN (
                         SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?2
                       )
                     ORDER BY coalesce(
                       json_extract(d.json, '$.sort_key'),
                       json_extract(d.json, '$.md_path'),
                       json_extract(d.json, '$.path')
                     ) ASC, d.doc_id ASC
                     LIMIT ?3"
                }
                QueryOrder::SortKeyDesc => {
                    "SELECT d.json FROM docs d
                     WHERE d.collection = ?1
                       AND d.id IN (
                         SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?2
                       )
                     ORDER BY coalesce(
                       json_extract(d.json, '$.sort_key'),
                       json_extract(d.json, '$.md_path'),
                       json_extract(d.json, '$.path')
                     ) DESC, d.doc_id DESC
                     LIMIT ?3"
                }
            };
            let mut stmt = conn.prepare(sql)?;
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
            let Some(preds) = parse_object_predicates(q_obj) else {
                return Ok(vec![]);
            };
            let Some((filter_sql, filter_binds)) = object_filter_sql(&preds) else {
                return Ok(vec![]);
            };
            let order_sql = order_by_sql_fragment(order);
            let lim_idx = 2 + filter_binds.len();
            let sql = format!(
                "SELECT json FROM docs WHERE collection = ?1{filter_sql} {order_sql} LIMIT ?{lim_idx}"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut sql_params: Vec<rusqlite::types::Value> =
                Vec::with_capacity(2 + filter_binds.len());
            sql_params.push(rusqlite::types::Value::Text(collection.to_string()));
            for b in filter_binds {
                sql_params.push(rusqlite::types::Value::Text(b));
            }
            sql_params.push(rusqlite::types::Value::Integer(lim as i64));
            let iter = stmt.query_map(rusqlite::params_from_iter(sql_params), |r| {
                let json: String = r.get(0)?;
                Ok(json)
            })?;
            for item in iter {
                let json = item?;
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    // Defense in depth: re-check predicates in Rust.
                    if row_matches_predicates(&v, &preds) {
                        rows.push(project_row(v, fields));
                    }
                }
            }
        }
        _ => {
            let sql = match order {
                QueryOrder::Random => {
                    "SELECT json FROM docs WHERE collection = ?1
                     ORDER BY RANDOM()
                     LIMIT ?2"
                }
                QueryOrder::PathAsc => {
                    "SELECT json FROM docs WHERE collection = ?1
                     ORDER BY json_extract(json, '$.path') ASC
                     LIMIT ?2"
                }
                QueryOrder::PathDesc => {
                    "SELECT json FROM docs WHERE collection = ?1
                     ORDER BY json_extract(json, '$.path') DESC
                     LIMIT ?2"
                }
                QueryOrder::SortKeyAsc => {
                    "SELECT json FROM docs WHERE collection = ?1
                     ORDER BY coalesce(
                       json_extract(json, '$.sort_key'),
                       json_extract(json, '$.md_path'),
                       json_extract(json, '$.path')
                     ) ASC, doc_id ASC
                     LIMIT ?2"
                }
                QueryOrder::SortKeyDesc => {
                    "SELECT json FROM docs WHERE collection = ?1
                     ORDER BY coalesce(
                       json_extract(json, '$.sort_key'),
                       json_extract(json, '$.md_path'),
                       json_extract(json, '$.path')
                     ) DESC, doc_id DESC
                     LIMIT ?2"
                }
            };
            let mut stmt = conn.prepare(sql)?;
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
        let looks_like_entry = obj.contains_key("_keywords")
            || obj.contains_key("_title")
            || obj.contains_key("_updated");
        let has_nested = obj.values().any(|v| {
            v.as_object()
                .map(|o| o.keys().any(|k| !k.starts_with('_')))
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
        assert_eq!(count_collection_docs(&db_path, "index").unwrap(), 2);

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
    }

    #[test]
    fn merge_preserves_other_docs() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        replace_collection_docs(
            &db_path,
            "index",
            &[json!({ "title": "Meta", "_meta_dir": "data/a", "path": "data/a" })],
            Some("h1"),
        )
        .unwrap();
        merge_collection_docs_with_fields(
            &db_path,
            "index",
            &[json!({ "_title": "FromIndex", "path": "blog/x.md" })],
            Some("h2"),
            Some(&["path".into()]),
            Some(&["_title".into(), "title".into()]),
        )
        .unwrap();
        assert_eq!(count_collection_docs(&db_path, "index").unwrap(), 2);
        let hits = query_collection(
            &db_path,
            "index",
            Some(&Value::String("FromIndex".into())),
            None,
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
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

    #[test]
    fn query_order_parse_allowlist() {
        assert_eq!(QueryOrder::parse("path").unwrap(), QueryOrder::PathAsc);
        assert_eq!(QueryOrder::parse("-path").unwrap(), QueryOrder::PathDesc);
        assert_eq!(
            QueryOrder::parse("sort_key").unwrap(),
            QueryOrder::SortKeyAsc
        );
        assert_eq!(
            QueryOrder::parse("-sort_key").unwrap(),
            QueryOrder::SortKeyDesc
        );
        assert_eq!(QueryOrder::parse("random").unwrap(), QueryOrder::Random);
        assert!(QueryOrder::parse("drop table").is_err());
    }

    #[test]
    fn random_limit_is_hard_capped() {
        assert_eq!(clamp_query_limit(QueryOrder::Random, None), 1);
        assert_eq!(clamp_query_limit(QueryOrder::Random, Some(40)), 40);
        assert_eq!(
            clamp_query_limit(QueryOrder::Random, Some(10_000)),
            QUERY_RANDOM_LIMIT_MAX
        );
        assert_eq!(
            clamp_query_limit(QueryOrder::PathDesc, Some(10_000)),
            QUERY_LIMIT_MAX
        );
    }

    #[test]
    fn query_order_random_respects_limit() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        let docs: Vec<_> = (0..50)
            .map(|i| {
                json!({
                    "title": format!("Item {i}"),
                    "path": format!("p/{i:02}"),
                    "_meta_dir": format!("d{i}")
                })
            })
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
        let hits =
            query_collection_ordered(&db_path, "index", None, None, Some(5), QueryOrder::Random)
                .unwrap();
        assert_eq!(hits.len(), 5);
    }

    #[test]
    fn query_order_path_asc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "h" },
                "collections": {
                    "index": [
                        { "title": "C", "path": "c", "_meta_dir": "c" },
                        { "title": "A", "path": "a", "_meta_dir": "a" },
                        { "title": "B", "path": "b", "_meta_dir": "b" }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        let hits =
            query_collection_ordered(&db_path, "index", None, None, Some(10), QueryOrder::PathAsc)
                .unwrap();
        assert_eq!(hits[0]["path"], "a");
        assert_eq!(hits[2]["path"], "c");
    }

    #[test]
    fn query_order_sort_key_asc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "h" },
                "collections": {
                    "events": [
                        { "id": "late", "sort_key": "000002.000000.000000" },
                        { "id": "early", "sort_key": "000001.000000.000000" },
                        { "id": "middle", "sort_key": "000001.000001.000000" }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        let hits =
            query_collection_ordered(&db_path, "events", None, None, None, QueryOrder::SortKeyAsc)
                .unwrap();
        let ids: Vec<_> = hits.iter().filter_map(|row| row["id"].as_str()).collect();
        assert_eq!(ids, vec!["early", "middle", "late"]);
    }

    #[test]
    fn query_order_sort_key_falls_back_to_md_path() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "h" },
                "collections": {
                    "events": [
                        {
                            "id": "evt-1053-ce-11th-century-ce-common-era-begins",
                            "md_path": "content/05.ce-11th-common-era-begins/05.00.00-11th-century.md"
                        },
                        {
                            "id": "evt-3147-bce-the-dark-ages",
                            "md_path": "content/03.the-dark-ages/03.00.00-the-dark-ages.md"
                        },
                        {
                            "id": "evt-4077-bce-the-golden-age",
                            "md_path": "content/02.the-golden-age/02.00.00-the-golden-age.md"
                        }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        let hits =
            query_collection_ordered(&db_path, "events", None, None, None, QueryOrder::SortKeyAsc)
                .unwrap();
        let ids: Vec<_> = hits.iter().filter_map(|row| row["id"].as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "evt-4077-bce-the-golden-age",
                "evt-3147-bce-the-dark-ages",
                "evt-1053-ce-11th-century-ce-common-era-begins"
            ]
        );
    }

    fn seed_content_kind_docs(dir: &tempfile::TempDir) -> PathBuf {
        let json_path = dir.path().join("index.db.json");
        let db_path = dir.path().join("index.db");
        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "h" },
                "collections": {
                    "index": [
                        { "path": "a.md", "content_kind": "article", "title": "A" },
                        { "path": "b.md", "content_kind": "investigation", "title": "B" },
                        { "path": "c.md", "content_kind": "blog", "title": "C" },
                        { "path": "d.png", "content_kind": "exclude", "title": "D" },
                        { "path": "e.jpg", "content_kind": "exclude", "title": "E" },
                        { "path": "f.md", "content_kind": "exclude", "title": "F" },
                        { "path": "readme.md", "content_kind": "exclude", "title": "README" },
                        { "path": "g.md", "content_kind": "article", "title": "G" }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        sync_json_to_sqlite(&json_path, &db_path).unwrap();
        db_path
    }

    #[test]
    fn object_filter_eq_excludes_other_kinds() {
        let dir = tempdir().unwrap();
        let db = seed_content_kind_docs(&dir);
        let hits = query_collection(
            &db,
            "index",
            Some(&json!({ "content_kind": "article" })),
            None,
            None,
        )
        .unwrap();
        let paths: Vec<_> = hits
            .iter()
            .filter_map(|r| r["path"].as_str())
            .collect();
        assert_eq!(paths, vec!["a.md", "g.md"]);
    }

    #[test]
    fn object_filter_in_array_allowlists_content_kinds() {
        let dir = tempdir().unwrap();
        let db = seed_content_kind_docs(&dir);
        let hits = query_collection(
            &db,
            "index",
            Some(&json!({
                "content_kind": ["article", "investigation", "blog"]
            })),
            None,
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 4);
        assert!(hits
            .iter()
            .all(|r| r["content_kind"] != "exclude"));
    }

    #[test]
    fn object_filter_ne_drops_exclude() {
        let dir = tempdir().unwrap();
        let db = seed_content_kind_docs(&dir);
        let hits = query_collection(
            &db,
            "index",
            Some(&json!({ "content_kind": { "$ne": "exclude" } })),
            None,
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 4);
        assert!(hits
            .iter()
            .all(|r| r["content_kind"].as_str() != Some("exclude")));
    }

    #[test]
    fn object_filter_random_limit_applies_after_sql_filter() {
        let dir = tempdir().unwrap();
        let db = seed_content_kind_docs(&dir);
        let q = json!({ "content_kind": ["article", "investigation", "blog"] });
        for _ in 0..8 {
            let hits = query_collection_ordered(
                &db,
                "index",
                Some(&q),
                None,
                Some(3),
                QueryOrder::Random,
            )
            .unwrap();
            assert_eq!(hits.len(), 3);
            assert!(hits.iter().all(|r| {
                matches!(
                    r["content_kind"].as_str(),
                    Some("article" | "investigation" | "blog")
                )
            }));
        }
    }

    #[test]
    fn object_filter_rejects_unsafe_field_names() {
        let dir = tempdir().unwrap();
        let db = seed_content_kind_docs(&dir);
        let hits = query_collection(
            &db,
            "index",
            Some(&json!({ "content_kind); DROP TABLE docs;--": "article" })),
            None,
            Some(10),
        )
        .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn object_filter_rejects_oversized_in_list() {
        let dir = tempdir().unwrap();
        let db = seed_content_kind_docs(&dir);
        let big: Vec<_> = (0..QUERY_FILTER_IN_MAX + 1)
            .map(|i| format!("k{i}"))
            .collect();
        let hits = query_collection(
            &db,
            "index",
            Some(&json!({ "content_kind": big })),
            None,
            Some(10),
        )
        .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn row_matches_object_filter_unit() {
        let row = json!({ "content_kind": "article", "path": "a.md" });
        assert!(row_matches_object_filter(
            &row,
            json!({ "content_kind": "article" }).as_object().unwrap()
        ));
        assert!(row_matches_object_filter(
            &row,
            json!({ "content_kind": ["article", "blog"] })
                .as_object()
                .unwrap()
        ));
        assert!(!row_matches_object_filter(
            &row,
            json!({ "content_kind": { "$ne": "article" } })
                .as_object()
                .unwrap()
        ));
        assert!(row_matches_object_filter(
            &row,
            json!({ "content_kind": { "$ne": "exclude" } })
                .as_object()
                .unwrap()
        ));
        // Missing field: $ne still matches.
        let bare = json!({ "path": "x.png" });
        assert!(row_matches_object_filter(
            &bare,
            json!({ "content_kind": { "$ne": "exclude" } })
                .as_object()
                .unwrap()
        ));
    }

    #[test]
    fn is_safe_json_field_allowlist() {
        assert!(is_safe_json_field("content_kind"));
        assert!(is_safe_json_field("type"));
        assert!(!is_safe_json_field(""));
        assert!(!is_safe_json_field("a.b"));
        assert!(!is_safe_json_field("a b"));
        assert!(!is_safe_json_field("x'); DROP--"));
    }
}

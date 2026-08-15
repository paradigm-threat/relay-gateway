//! Portable search catalog: QUERY and index I/O go through [`SearchStore`].
//!
//! v1 backend is SQLite (one file per node, `repo`+`branch` columns). A later
//! Postgres/NoSQL impl must preserve the same QUERY contract.

use crate::git::sqlite_index::{
    clamp_query_limit, fts_match_query, QueryOrder, QUERY_FILTER_IN_MAX,
};
use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tracing::{info, warn};

/// Website-default filterable keys when `search.yaml` omits `filterFields`.
pub const DEFAULT_FILTER_FIELDS: &[&str] = &[
    "content_kind",
    "type",
    "path",
    "id",
    "channel",
    "author",
    "title",
];
/// Website-default sortable keys when `search.yaml` omits `sortFields`.
pub const DEFAULT_SORT_FIELDS: &[&str] = &["path", "sort_key", "md_path", "created"];
/// Default FTS field names when `search.yaml` omits `ftsFields`.
pub const DEFAULT_FTS_FIELDS: &[&str] = &[
    "_title",
    "title",
    "_keywords",
    "keywords",
    "description",
    "body",
    "name",
    "summary",
];

const META_DOCS_HEAD: &str = "docs_indexed_head";
const META_CHAT_HEAD: &str = "chat_indexed_head";
/// Isolated unit-test repo slug when a helper only has a DB path.
pub const UNIT_TEST_REPO: &str = "_";
pub const UNIT_TEST_BRANCH: &str = "main";

/// Identity for a catalog partition. Never includes hostname or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoreKey {
    pub repo: String,
    pub branch: String,
}

impl StoreKey {
    pub fn new(repo: impl Into<String>, branch: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            branch: branch.into(),
        }
    }
}

/// Collection schema declared in `search.yaml` (portable across backends).
#[derive(Debug, Clone, Default)]
pub struct CollectionSchema {
    pub fts_fields: Vec<String>,
    pub filter_fields: Vec<String>,
    pub sort_fields: Vec<String>,
}

/// QUERY + health in one round-trip.
#[derive(Debug, Clone)]
pub struct StoreQueryOutcome {
    pub rows: Vec<Value>,
    pub doc_count: u64,
    pub indexed_head: Option<String>,
}

/// Backend-agnostic search catalog.
pub trait SearchStore: Send + Sync {
    fn replace_collection(
        &self,
        key: &StoreKey,
        collection: &str,
        docs: &[Value],
        id_fields: Option<&[String]>,
        fts_fields: Option<&[String]>,
        indexed_head: Option<&str>,
    ) -> anyhow::Result<()>;

    fn merge_collection(
        &self,
        key: &StoreKey,
        collection: &str,
        docs: &[Value],
        id_fields: Option<&[String]>,
        fts_fields: Option<&[String]>,
        indexed_head: Option<&str>,
    ) -> anyhow::Result<()>;

    fn delete_docs_by_paths(
        &self,
        key: &StoreKey,
        collection: &str,
        paths: &[String],
    ) -> anyhow::Result<()>;

    fn query(
        &self,
        key: &StoreKey,
        collection: &str,
        query: Option<&Value>,
        fields: Option<&[String]>,
        limit: Option<usize>,
        order: QueryOrder,
        filter_fields: &[String],
    ) -> anyhow::Result<Vec<Value>>;

    fn query_with_meta(
        &self,
        key: &StoreKey,
        collection: &str,
        query: Option<&Value>,
        fields: Option<&[String]>,
        limit: Option<usize>,
        order: QueryOrder,
        filter_fields: &[String],
    ) -> anyhow::Result<StoreQueryOutcome>;

    fn docs_indexed_head(&self, key: &StoreKey) -> anyhow::Result<Option<String>>;
    fn chat_indexed_head(&self, key: &StoreKey) -> anyhow::Result<Option<String>>;
    fn set_docs_indexed_head(&self, key: &StoreKey, head: &str) -> anyhow::Result<()>;
    fn count_docs(&self, key: &StoreKey, collection: &str) -> anyhow::Result<u64>;

    fn query_chat(
        &self,
        key: &StoreKey,
        query: Option<&Value>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<Value>>;

    fn replace_chat_comments(
        &self,
        key: &StoreKey,
        rows: &[ChatCommentRow],
        indexed_head: &str,
        chat_root: &str,
    ) -> anyhow::Result<()>;

    fn upsert_chat_comment(&self, key: &StoreKey, row: &ChatCommentRow) -> anyhow::Result<()>;
    fn delete_chat_path(&self, key: &StoreKey, path: &str) -> anyhow::Result<()>;
    fn chat_count(&self, key: &StoreKey) -> anyhow::Result<i64>;
    fn set_chat_indexed_head(
        &self,
        key: &StoreKey,
        head: &str,
        chat_root: &str,
    ) -> anyhow::Result<()>;

    /// Serialize index writes for `(repo, branch)`.
    fn lock_branch(&self, key: &StoreKey) -> Arc<Mutex<()>>;

    /// Declare filter/sort/FTS fields for this collection. SQLite is a no-op;
    /// Postgres/NoSQL backends must create indexes / filterable attributes here.
    fn ensure_schema(
        &self,
        key: &StoreKey,
        collection: &str,
        schema: &CollectionSchema,
    ) -> anyhow::Result<()>;
}

/// One chat comment row (plugin `chat`).
#[derive(Debug, Clone)]
pub struct ChatCommentRow {
    pub path: String,
    pub id: String,
    pub channel: String,
    pub author: Option<String>,
    pub anonymous: bool,
    pub created: String,
    pub source: String,
    pub signed: bool,
    pub body: String,
    pub commit_oid: Option<String>,
}

/// SQLite v1 backend: one WAL file, long-lived connection.
pub struct SqliteSearchStore {
    conn: Mutex<Connection>,
    branch_locks: Mutex<HashMap<(String, String), Arc<Mutex<()>>>>,
}

impl SqliteSearchStore {
    pub fn open(path: &Path) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open search db {}", path.display()))?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            CREATE TABLE IF NOT EXISTS meta (
                repo TEXT NOT NULL,
                branch TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (repo, branch, key)
            );
            CREATE TABLE IF NOT EXISTS docs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo TEXT NOT NULL,
                branch TEXT NOT NULL,
                collection TEXT NOT NULL,
                doc_id TEXT NOT NULL,
                sort_key TEXT,
                path TEXT,
                md_path TEXT,
                search_text TEXT NOT NULL DEFAULT '',
                json TEXT NOT NULL,
                UNIQUE(repo, branch, collection, doc_id)
            );
            CREATE INDEX IF NOT EXISTS docs_sort
                ON docs(repo, branch, collection, sort_key);
            CREATE INDEX IF NOT EXISTS docs_path
                ON docs(repo, branch, collection, path);
            CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
                search_text,
                content='docs',
                content_rowid='id'
            );
            CREATE TABLE IF NOT EXISTS chat_comments (
                repo TEXT NOT NULL,
                branch TEXT NOT NULL,
                path TEXT NOT NULL,
                id TEXT NOT NULL,
                channel TEXT NOT NULL,
                author TEXT,
                anonymous INTEGER NOT NULL DEFAULT 1,
                created TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'web',
                signed INTEGER NOT NULL DEFAULT 0,
                body TEXT NOT NULL,
                commit_oid TEXT,
                PRIMARY KEY (repo, branch, path)
            );
            CREATE INDEX IF NOT EXISTS chat_channel_created
                ON chat_comments(repo, branch, channel, created DESC);
            CREATE VIRTUAL TABLE IF NOT EXISTS chat_comments_fts USING fts5(
                body, author, channel, id,
                path UNINDEXED, repo UNINDEXED, branch UNINDEXED
            );
            ",
        )?;
        migrate_chat_fts(&conn)?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            branch_locks: Mutex::new(HashMap::new()),
        }))
    }
}

static STORES: OnceLock<Mutex<HashMap<PathBuf, Arc<SqliteSearchStore>>>> = OnceLock::new();

fn store_cache() -> &'static Mutex<HashMap<PathBuf, Arc<SqliteSearchStore>>> {
    STORES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Backend selected by `RELAY_SEARCH_STORE` (default `sqlite`).
/// Postgres and Meilisearch are reserved names for a later impl of the same trait.
pub fn configured_search_backend() -> String {
    std::env::var("RELAY_SEARCH_STORE")
        .unwrap_or_else(|_| "sqlite".into())
        .trim()
        .to_ascii_lowercase()
}

/// Parse `RELAY_SEARCH_STORE`. v1 only accepts sqlite; other names are reserved.
pub fn parse_search_backend(raw: &str) -> anyhow::Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "sqlite" => Ok("sqlite"),
        "postgres" | "postgresql" => anyhow::bail!(
            "RELAY_SEARCH_STORE=postgres is reserved for a later SearchStore backend; v1 is sqlite"
        ),
        "meilisearch" | "meili" => anyhow::bail!(
            "RELAY_SEARCH_STORE=meilisearch is reserved for a later SearchStore backend; v1 is sqlite"
        ),
        other => anyhow::bail!("unknown RELAY_SEARCH_STORE={other:?}; expected sqlite"),
    }
}

fn assert_sqlite_backend() -> anyhow::Result<()> {
    parse_search_backend(&configured_search_backend()).map(|_| ())
}

/// Path of the node-local search DB (`RELAY_SEARCH_DB` or `{repo_root}/.relay_search.db`).
pub fn search_db_path(repo_root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("RELAY_SEARCH_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    repo_root.join(".relay_search.db")
}

/// Bare `name.git` → catalog slug `name`.
pub fn repo_slug_from_bare(bare: &Path) -> String {
    bare.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.strip_suffix(".git").unwrap_or(s).to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

pub fn repo_root_from_bare(bare: &Path) -> PathBuf {
    bare.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn store_key_for_bare(bare: &Path, branch: &str) -> StoreKey {
    StoreKey::new(repo_slug_from_bare(bare), branch)
}

/// Process-wide store for this node's repo root (one SQLite file).
pub fn store_for_repo_root(repo_root: &Path) -> anyhow::Result<Arc<SqliteSearchStore>> {
    assert_sqlite_backend()?;
    let path = match std::fs::canonicalize(repo_root) {
        Ok(c) => search_db_path(&c),
        Err(_) => search_db_path(repo_root),
    };
    store_at_path(&path)
}

pub fn store_for_bare_repo(bare: &Path) -> anyhow::Result<Arc<SqliteSearchStore>> {
    store_for_repo_root(&repo_root_from_bare(bare))
}

pub fn store_at_path(path: &Path) -> anyhow::Result<Arc<SqliteSearchStore>> {
    let key = path.to_path_buf();
    let mut map = store_cache().lock().unwrap();
    if let Some(s) = map.get(&key) {
        return Ok(Arc::clone(s));
    }
    let store = SqliteSearchStore::open(path)?;
    map.insert(key, Arc::clone(&store));
    Ok(store)
}

pub fn effective_filter_fields(declared: Option<&[String]>) -> Vec<String> {
    match declared {
        Some(f) if !f.is_empty() => f.to_vec(),
        _ => DEFAULT_FILTER_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    }
}

pub fn effective_sort_fields(declared: Option<&[String]>) -> Vec<String> {
    match declared {
        Some(f) if !f.is_empty() => f.to_vec(),
        _ => DEFAULT_SORT_FIELDS.iter().map(|s| (*s).to_string()).collect(),
    }
}

pub fn effective_fts_fields(declared: Option<&[String]>) -> Vec<String> {
    match declared {
        Some(f) if !f.is_empty() => f.to_vec(),
        _ => DEFAULT_FTS_FIELDS.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// True when every object-QUERY key is in the declared `filterFields` list.
pub fn filter_keys_allowed(keys: impl IntoIterator<Item = impl AsRef<str>>, filter_fields: &[String]) -> bool {
    keys.into_iter()
        .all(|k| filter_fields.iter().any(|f| f == k.as_ref()))
}

/// Candidate pool for portable `order=random` (shuffle in Rust, not SQL `RANDOM()`).
pub const QUERY_RANDOM_CANDIDATE_MAX: usize = 2_000;

/// Fisher–Yates shuffle then truncate. Same contract for SQLite, Postgres, and NoSQL.
pub fn take_random(mut rows: Vec<Value>, limit: usize) -> Vec<Value> {
    shuffle_in_place(&mut rows);
    rows.truncate(limit.max(1));
    rows
}

fn shuffle_in_place<T>(items: &mut [T]) {
    if items.len() < 2 {
        return;
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        ^ (items.len() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for i in (1..items.len()).rev() {
        seed = seed
            .wrapping_mul(0x5851_F42D_4C95_7F2D)
            .wrapping_add(0x1405_7B7E_F767_814F);
        let j = (seed as usize) % (i + 1);
        items.swap(i, j);
    }
}

fn migrate_chat_fts(conn: &Connection) -> anyhow::Result<()> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='chat_comments_fts'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(());
    };
    if sql.contains("repo UNINDEXED") {
        return Ok(());
    }
    conn.execute_batch(
        "
        DROP TABLE IF EXISTS chat_comments_fts;
        CREATE VIRTUAL TABLE chat_comments_fts USING fts5(
            body, author, channel, id,
            path UNINDEXED, repo UNINDEXED, branch UNINDEXED
        );
        ",
    )?;
    Ok(())
}

fn set_meta(conn: &Connection, key: &StoreKey, meta_key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO meta(repo, branch, key, value) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(repo, branch, key) DO UPDATE SET value=excluded.value",
        params![key.repo, key.branch, meta_key, value],
    )?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &StoreKey, meta_key: &str) -> anyhow::Result<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE repo = ?1 AND branch = ?2 AND key = ?3",
            params![key.repo, key.branch, meta_key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v)
}

fn promoted_fields(doc: &Value) -> (Option<String>, Option<String>, Option<String>) {
    let str_field = |k: &str| doc.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
    (str_field("sort_key"), str_field("path"), str_field("md_path"))
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
    for k in ["path", "_meta_dir", "id"] {
        if let Some(s) = doc.get(k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
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
    for key in ["title", "_title", "path", "name", "description", "summary", "body"] {
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
    key: &StoreKey,
    collection: &str,
    doc: &Value,
    index: usize,
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    let doc_id = doc_id_for(doc, index, id_fields);
    let json = serde_json::to_string(doc)?;
    let search_text = search_text_for(doc, fts_fields);
    let (sort_key, path, md_path) = promoted_fields(doc);
    conn.execute(
        "INSERT INTO docs(repo, branch, collection, doc_id, sort_key, path, md_path, json, search_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            key.repo,
            key.branch,
            collection,
            doc_id,
            sort_key,
            path,
            md_path,
            json,
            search_text
        ],
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
    key: &StoreKey,
    collection: &str,
    doc: &Value,
    index: usize,
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    let doc_id = doc_id_for(doc, index, id_fields);
    let json = serde_json::to_string(doc)?;
    let search_text = search_text_for(doc, fts_fields);
    let (sort_key, path, md_path) = promoted_fields(doc);
    if let Ok(old_id) = conn.query_row(
        "SELECT id FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3 AND doc_id = ?4",
        params![key.repo, key.branch, collection, doc_id],
        |r| r.get::<_, i64>(0),
    ) {
        let _ = conn.execute("DELETE FROM docs_fts WHERE rowid = ?1", params![old_id]);
        conn.execute(
            "UPDATE docs SET json = ?1, search_text = ?2, sort_key = ?3, path = ?4, md_path = ?5
             WHERE id = ?6",
            params![json, search_text, sort_key, path, md_path, old_id],
        )?;
        conn.execute(
            "INSERT INTO docs_fts(rowid, search_text) VALUES (?1, ?2)",
            params![old_id, search_text],
        )?;
    } else {
        insert_doc(conn, key, collection, doc, index, id_fields, fts_fields)?;
    }
    Ok(())
}

fn replace_collection(
    conn: &Connection,
    key: &StoreKey,
    collection: &str,
    docs: &[Value],
    id_fields: Option<&[String]>,
    fts_fields: Option<&[String]>,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM docs_fts WHERE rowid IN (
            SELECT id FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3
         )",
        params![key.repo, key.branch, collection],
    )?;
    conn.execute(
        "DELETE FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3",
        params![key.repo, key.branch, collection],
    )?;
    for (i, doc) in docs.iter().enumerate() {
        insert_doc(conn, key, collection, doc, i, id_fields, fts_fields)?;
    }
    Ok(())
}

fn is_safe_json_field(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldPred {
    Eq(String),
    Ne(String),
    In(Vec<String>),
    Nin(Vec<String>),
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

fn object_filter_sql(preds: &[(String, FieldPred)]) -> Option<(String, Vec<String>)> {
    let mut sql = String::new();
    let mut binds = Vec::new();
    for (key, pred) in preds {
        let extract = format!("json_extract(json, '$.{key}')");
        match pred {
            FieldPred::Eq(v) => {
                sql.push_str(&format!(" AND {extract} = ?"));
                binds.push(v.clone());
            }
            FieldPred::Ne(v) => {
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
        // Random shuffles in Rust after a capped candidate fetch (portable across backends).
        QueryOrder::Random => "ORDER BY path ASC, doc_id ASC",
        QueryOrder::PathAsc => "ORDER BY path ASC",
        QueryOrder::PathDesc => "ORDER BY path DESC",
        QueryOrder::SortKeyAsc => {
            "ORDER BY coalesce(sort_key, md_path, path) ASC, doc_id ASC"
        }
        QueryOrder::SortKeyDesc => {
            "ORDER BY coalesce(sort_key, md_path, path) DESC, doc_id DESC"
        }
    }
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

fn query_docs(
    conn: &Connection,
    key: &StoreKey,
    collection: &str,
    query: Option<&Value>,
    fields: Option<&[String]>,
    limit: Option<usize>,
    order: QueryOrder,
    filter_fields: &[String],
) -> anyhow::Result<Vec<Value>> {
    let out_lim = clamp_query_limit(order, limit);
    let fetch_lim = if order == QueryOrder::Random {
        QUERY_RANDOM_CANDIDATE_MAX
    } else {
        out_lim
    };
    let mut rows: Vec<Value> = Vec::new();

    match query {
        Some(Value::String(q_str)) if !q_str.trim().is_empty() => {
            let match_q = fts_match_query(q_str);
            if match_q.is_empty() {
                return Ok(vec![]);
            }
            let order_sql = order_by_sql_fragment(order);
            let sql = format!(
                "SELECT d.json FROM docs d
                 WHERE d.repo = ?1 AND d.branch = ?2 AND d.collection = ?3
                   AND d.id IN (SELECT rowid FROM docs_fts WHERE docs_fts MATCH ?4)
                 {order_sql} LIMIT ?5"
            );
            let mut stmt = conn.prepare(&sql)?;
            let iter = match stmt.query_map(
                params![key.repo, key.branch, collection, match_q, fetch_lim as i64],
                |r| r.get::<_, String>(0),
            ) {
                Ok(i) => i,
                Err(e) => {
                    warn!(error = %e, match_q = %match_q, "fts MATCH failed; returning empty");
                    return Ok(vec![]);
                }
            };
            for item in iter {
                let json = match item {
                    Ok(j) => j,
                    Err(e) => {
                        warn!(error = %e, match_q = %match_q, "fts MATCH row failed; returning empty");
                        return Ok(vec![]);
                    }
                };
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    rows.push(project_row(v, fields));
                }
            }
        }
        Some(Value::Object(q_obj)) => {
            let Some(preds) = parse_object_predicates(q_obj) else {
                return Ok(vec![]);
            };
            if !filter_keys_allowed(preds.iter().map(|(k, _)| k.as_str()), filter_fields) {
                return Ok(vec![]);
            }
            let Some((filter_sql, filter_binds)) = object_filter_sql(&preds) else {
                return Ok(vec![]);
            };
            let order_sql = order_by_sql_fragment(order);
            let lim_idx = 4 + filter_binds.len();
            let sql = format!(
                "SELECT json FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3{filter_sql} {order_sql} LIMIT ?{lim_idx}"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut sql_params: Vec<rusqlite::types::Value> =
                Vec::with_capacity(4 + filter_binds.len());
            sql_params.push(rusqlite::types::Value::Text(key.repo.clone()));
            sql_params.push(rusqlite::types::Value::Text(key.branch.clone()));
            sql_params.push(rusqlite::types::Value::Text(collection.to_string()));
            for b in filter_binds {
                sql_params.push(rusqlite::types::Value::Text(b));
            }
            sql_params.push(rusqlite::types::Value::Integer(fetch_lim as i64));
            let iter = stmt.query_map(rusqlite::params_from_iter(sql_params), |r| {
                r.get::<_, String>(0)
            })?;
            for item in iter {
                let json = item?;
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    if row_matches_predicates(&v, &preds) {
                        rows.push(project_row(v, fields));
                    }
                }
            }
        }
        _ => {
            let order_sql = order_by_sql_fragment(order);
            let sql = format!(
                "SELECT json FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3
                 {order_sql} LIMIT ?4"
            );
            let mut stmt = conn.prepare(&sql)?;
            let iter = stmt.query_map(
                params![key.repo, key.branch, collection, fetch_lim as i64],
                |r| r.get::<_, String>(0),
            )?;
            for item in iter {
                let json = item?;
                if let Ok(v) = serde_json::from_str::<Value>(&json) {
                    rows.push(project_row(v, fields));
                }
            }
        }
    }
    if order == QueryOrder::Random {
        rows = take_random(rows, out_lim);
    }
    Ok(rows)
}

fn chat_json_from_tuple(
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
    chat_row_to_json(&map_chat_row(
        path, id, channel, author, anonymous, created, source, signed, body,
    ))
}

fn chat_rows_from_mapped(
    iter: impl Iterator<
        Item = rusqlite::Result<(
            String,
            String,
            String,
            Option<String>,
            i64,
            String,
            String,
            i64,
            String,
        )>,
    >,
) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for item in iter {
        let (path, id, channel, author, anonymous, created, source, signed, body) = item?;
        out.push(chat_json_from_tuple(
            path, id, channel, author, anonymous, created, source, signed, body,
        ));
    }
    Ok(out)
}

fn chat_row_to_json(row: &ChatCommentRow) -> Value {
    let mut m = Map::new();
    m.insert("path".into(), Value::String(row.path.clone()));
    m.insert("id".into(), Value::String(row.id.clone()));
    m.insert("channel".into(), Value::String(row.channel.clone()));
    m.insert("schema".into(), Value::String("paradigm.chat.v1".into()));
    if let Some(a) = &row.author {
        if !a.is_empty() {
            m.insert("author".into(), Value::String(a.clone()));
        }
    }
    m.insert("anonymous".into(), Value::Bool(row.anonymous));
    m.insert("created".into(), Value::String(row.created.clone()));
    m.insert("source".into(), Value::String(row.source.clone()));
    m.insert("signed".into(), Value::Bool(row.signed));
    m.insert("body".into(), Value::String(row.body.clone()));
    Value::Object(m)
}

fn map_chat_row(
    path: String,
    id: String,
    channel: String,
    author: Option<String>,
    anonymous: i64,
    created: String,
    source: String,
    signed: i64,
    body: String,
) -> ChatCommentRow {
    ChatCommentRow {
        path,
        id,
        channel,
        author,
        anonymous: anonymous != 0,
        created,
        source,
        signed: signed != 0,
        body,
        commit_oid: None,
    }
}

fn upsert_chat_row(conn: &Connection, key: &StoreKey, row: &ChatCommentRow) -> anyhow::Result<()> {
    let _ = conn.execute(
        "DELETE FROM chat_comments_fts WHERE path = ?1 AND repo = ?2 AND branch = ?3",
        params![row.path, key.repo, key.branch],
    );
    conn.execute(
        "INSERT INTO chat_comments(repo, branch, path, id, channel, author, anonymous, created, source, signed, body, commit_oid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(repo, branch, path) DO UPDATE SET
            id=excluded.id, channel=excluded.channel, author=excluded.author,
            anonymous=excluded.anonymous, created=excluded.created, source=excluded.source,
            signed=excluded.signed, body=excluded.body, commit_oid=excluded.commit_oid",
        params![
            key.repo,
            key.branch,
            row.path,
            row.id,
            row.channel,
            row.author,
            if row.anonymous { 1 } else { 0 },
            row.created,
            row.source,
            if row.signed { 1 } else { 0 },
            row.body,
            row.commit_oid,
        ],
    )?;
    conn.execute(
        "INSERT INTO chat_comments_fts(body, author, channel, id, path, repo, branch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.body,
            row.author.as_deref().unwrap_or(""),
            row.channel,
            row.id,
            row.path,
            key.repo,
            key.branch,
        ],
    )?;
    Ok(())
}

impl SearchStore for SqliteSearchStore {
    fn replace_collection(
        &self,
        key: &StoreKey,
        collection: &str,
        docs: &[Value],
        id_fields: Option<&[String]>,
        fts_fields: Option<&[String]>,
        indexed_head: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        replace_collection(&tx, key, collection, docs, id_fields, fts_fields)?;
        if let Some(head) = indexed_head {
            set_meta(&tx, key, META_DOCS_HEAD, head)?;
        }
        tx.commit()?;
        info!(
            repo = %key.repo,
            branch = %key.branch,
            collection = %collection,
            docs = docs.len(),
            "search store replace_collection"
        );
        Ok(())
    }

    fn merge_collection(
        &self,
        key: &StoreKey,
        collection: &str,
        docs: &[Value],
        id_fields: Option<&[String]>,
        fts_fields: Option<&[String]>,
        indexed_head: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        for (i, doc) in docs.iter().enumerate() {
            upsert_doc(&tx, key, collection, doc, i, id_fields, fts_fields)?;
        }
        if let Some(head) = indexed_head {
            set_meta(&tx, key, META_DOCS_HEAD, head)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn delete_docs_by_paths(
        &self,
        key: &StoreKey,
        collection: &str,
        paths: &[String],
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        for path in paths {
            conn.execute(
                "DELETE FROM docs_fts WHERE rowid IN (
                    SELECT id FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3 AND path = ?4
                 )",
                params![key.repo, key.branch, collection, path],
            )?;
            conn.execute(
                "DELETE FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3 AND path = ?4",
                params![key.repo, key.branch, collection, path],
            )?;
        }
        Ok(())
    }

    fn query(
        &self,
        key: &StoreKey,
        collection: &str,
        query: Option<&Value>,
        fields: Option<&[String]>,
        limit: Option<usize>,
        order: QueryOrder,
        filter_fields: &[String],
    ) -> anyhow::Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        query_docs(
            &conn,
            key,
            collection,
            query,
            fields,
            limit,
            order,
            filter_fields,
        )
    }

    fn query_with_meta(
        &self,
        key: &StoreKey,
        collection: &str,
        query: Option<&Value>,
        fields: Option<&[String]>,
        limit: Option<usize>,
        order: QueryOrder,
        filter_fields: &[String],
    ) -> anyhow::Result<StoreQueryOutcome> {
        let conn = self.conn.lock().unwrap();
        let rows = query_docs(
            &conn,
            key,
            collection,
            query,
            fields,
            limit,
            order,
            filter_fields,
        )?;
        let doc_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3",
            params![key.repo, key.branch, collection],
            |r| r.get(0),
        )?;
        let indexed_head = get_meta(&conn, key, META_DOCS_HEAD)?;
        Ok(StoreQueryOutcome {
            rows,
            doc_count: doc_count as u64,
            indexed_head,
        })
    }

    fn docs_indexed_head(&self, key: &StoreKey) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        get_meta(&conn, key, META_DOCS_HEAD)
    }

    fn chat_indexed_head(&self, key: &StoreKey) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        get_meta(&conn, key, META_CHAT_HEAD)
    }

    fn set_docs_indexed_head(&self, key: &StoreKey, head: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        set_meta(&conn, key, META_DOCS_HEAD, head)
    }

    fn count_docs(&self, key: &StoreKey, collection: &str) -> anyhow::Result<u64> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM docs WHERE repo = ?1 AND branch = ?2 AND collection = ?3",
            params![key.repo, key.branch, collection],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    fn query_chat(
        &self,
        key: &StoreKey,
        query: Option<&Value>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let lim = limit.unwrap_or(50).max(1) as i64;
        let mut rows;
        match query {
            Some(Value::Object(obj)) => {
                if let Some(ch) = obj.get("channel").and_then(|v| v.as_str()) {
                    let mut stmt = conn.prepare(
                        "SELECT path, id, channel, author, anonymous, created, source, signed, body
                         FROM chat_comments
                         WHERE repo = ?1 AND branch = ?2 AND channel = ?3
                         ORDER BY created DESC LIMIT ?4",
                    )?;
                    let iter = stmt.query_map(params![key.repo, key.branch, ch, lim], |r| {
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
                    rows = chat_rows_from_mapped(iter)?;
                } else if let Some(author) = obj.get("author").and_then(|v| v.as_str()) {
                    let mut stmt = conn.prepare(
                        "SELECT path, id, channel, author, anonymous, created, source, signed, body
                         FROM chat_comments
                         WHERE repo = ?1 AND branch = ?2 AND anonymous = 0 AND lower(author) = lower(?3)
                         ORDER BY created DESC LIMIT ?4",
                    )?;
                    let iter = stmt.query_map(params![key.repo, key.branch, author, lim], |r| {
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
                    rows = chat_rows_from_mapped(iter)?;
                } else {
                    let mut stmt = conn.prepare(
                        "SELECT path, id, channel, author, anonymous, created, source, signed, body
                         FROM chat_comments WHERE repo = ?1 AND branch = ?2
                         ORDER BY created DESC LIMIT ?3",
                    )?;
                    let iter = stmt.query_map(params![key.repo, key.branch, lim], |r| {
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
                    rows = chat_rows_from_mapped(iter)?;
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
                     INNER JOIN chat_comments_fts f ON f.path = c.path AND f.repo = c.repo AND f.branch = c.branch
                     WHERE c.repo = ?1 AND c.branch = ?2 AND chat_comments_fts MATCH ?3
                     ORDER BY c.created DESC LIMIT ?4",
                )?;
                let iter = match stmt.query_map(params![key.repo, key.branch, match_q, lim], |r| {
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
                }) {
                    Ok(i) => i,
                    Err(e) => {
                        warn!(error = %e, match_q = %match_q, "chat fts MATCH failed; returning empty");
                        return Ok(vec![]);
                    }
                };
                rows = chat_rows_from_mapped(iter)?;
            }
            _ => {
                let mut stmt = conn.prepare(
                    "SELECT path, id, channel, author, anonymous, created, source, signed, body
                     FROM chat_comments WHERE repo = ?1 AND branch = ?2
                     ORDER BY created DESC LIMIT ?3",
                )?;
                let iter = stmt.query_map(params![key.repo, key.branch, lim], |r| {
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
                rows = chat_rows_from_mapped(iter)?;
            }
        }
        Ok(rows)
    }

    fn replace_chat_comments(
        &self,
        key: &StoreKey,
        rows: &[ChatCommentRow],
        indexed_head: &str,
        chat_root: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM chat_comments WHERE repo = ?1 AND branch = ?2",
            params![key.repo, key.branch],
        )?;
        // FTS is standalone; rebuild by re-insert.
        for row in rows {
            upsert_chat_row(&tx, key, row)?;
        }
        set_meta(&tx, key, META_CHAT_HEAD, indexed_head)?;
        set_meta(&tx, key, "chat_root", chat_root)?;
        tx.commit()?;
        Ok(())
    }

    fn upsert_chat_comment(&self, key: &StoreKey, row: &ChatCommentRow) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        upsert_chat_row(&conn, key, row)
    }

    fn delete_chat_path(&self, key: &StoreKey, path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM chat_comments_fts WHERE path = ?1 AND repo = ?2 AND branch = ?3",
            params![path, key.repo, key.branch],
        );
        conn.execute(
            "DELETE FROM chat_comments WHERE repo = ?1 AND branch = ?2 AND path = ?3",
            params![key.repo, key.branch, path],
        )?;
        Ok(())
    }

    fn chat_count(&self, key: &StoreKey) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chat_comments WHERE repo = ?1 AND branch = ?2",
            params![key.repo, key.branch],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    fn set_chat_indexed_head(
        &self,
        key: &StoreKey,
        head: &str,
        chat_root: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        set_meta(&conn, key, META_CHAT_HEAD, head)?;
        set_meta(&conn, key, "chat_root", chat_root)?;
        Ok(())
    }

    fn lock_branch(&self, key: &StoreKey) -> Arc<Mutex<()>> {
        let mut map = self.branch_locks.lock().unwrap();
        map.entry((key.repo.clone(), key.branch.clone()))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn ensure_schema(
        &self,
        _key: &StoreKey,
        _collection: &str,
        _schema: &CollectionSchema,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn two_repos_isolated_in_one_file() {
        let dir = tempdir().unwrap();
        let store = SqliteSearchStore::open(&dir.path().join("s.db")).unwrap();
        let a = StoreKey::new("alpha", "main");
        let b = StoreKey::new("beta", "main");
        store
            .replace_collection(
                &a,
                "index",
                &[json!({"title": "Alpha", "path": "a.md"})],
                None,
                None,
                Some("aaa"),
            )
            .unwrap();
        store
            .replace_collection(
                &b,
                "index",
                &[json!({"title": "Beta", "path": "b.md"})],
                None,
                None,
                Some("bbb"),
            )
            .unwrap();
        let filters = effective_filter_fields(None);
        let hits = store
            .query(
                &a,
                "index",
                Some(&Value::String("Alpha".into())),
                None,
                None,
                QueryOrder::default(),
                &filters,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], "Alpha");
        assert_eq!(store.count_docs(&b, "index").unwrap(), 1);
        assert_eq!(store.docs_indexed_head(&a).unwrap().as_deref(), Some("aaa"));
    }

    #[test]
    fn undeclared_filter_key_is_empty() {
        let dir = tempdir().unwrap();
        let store = SqliteSearchStore::open(&dir.path().join("s.db")).unwrap();
        let key = StoreKey::new("r", "main");
        store
            .replace_collection(
                &key,
                "index",
                &[json!({"title": "X", "secret": "nope", "path": "x.md"})],
                None,
                None,
                Some("h"),
            )
            .unwrap();
        let hits = store
            .query(
                &key,
                "index",
                Some(&json!({"secret": "nope"})),
                None,
                None,
                QueryOrder::default(),
                &effective_filter_fields(None),
            )
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn take_random_respects_limit_and_preserves_items() {
        let rows: Vec<Value> = (0..20).map(|i| json!({"n": i})).collect();
        let out = take_random(rows.clone(), 5);
        assert_eq!(out.len(), 5);
        for row in &out {
            assert!(rows.contains(row));
        }
    }

    #[test]
    fn parse_search_backend_reserves_postgres_and_meili() {
        assert_eq!(parse_search_backend("sqlite").unwrap(), "sqlite");
        assert_eq!(parse_search_backend("").unwrap(), "sqlite");
        let pg = parse_search_backend("postgres").unwrap_err().to_string();
        assert!(pg.contains("reserved"), "{pg}");
        let meili = parse_search_backend("meilisearch").unwrap_err().to_string();
        assert!(meili.contains("reserved"), "{meili}");
        assert!(parse_search_backend("redis").is_err());
    }

    #[test]
    fn filter_keys_allowed_matches_declared_list() {
        let fields = effective_filter_fields(None);
        assert!(filter_keys_allowed(["content_kind", "title"], &fields));
        assert!(!filter_keys_allowed(["secret"], &fields));
    }

    #[test]
    fn random_query_returns_capped_shuffle() {
        let dir = tempdir().unwrap();
        let store = SqliteSearchStore::open(&dir.path().join("s.db")).unwrap();
        let key = StoreKey::new("r", "main");
        let docs: Vec<_> = (0..30)
            .map(|i| json!({"title": format!("T{i}"), "path": format!("p/{i:02}")}))
            .collect();
        store
            .replace_collection(&key, "index", &docs, None, None, Some("h"))
            .unwrap();
        let hits = store
            .query(
                &key,
                "index",
                None,
                None,
                Some(5),
                QueryOrder::Random,
                &effective_filter_fields(None),
            )
            .unwrap();
        assert_eq!(hits.len(), 5);
    }
}

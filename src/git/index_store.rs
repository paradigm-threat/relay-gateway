//! Sync `index.db.json` (written by Relay hooks) into `index.redb` for atomic reads.

use redb::{Database, TableDefinition};
use serde_json::Value;
use std::path::Path;

const META: TableDefinition<&str, &str> = TableDefinition::new("relay_meta");
const COLLECTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("relay_collections");

/// Persist JSON index file into a redb database at `redb_path`.
pub fn sync_json_to_redb(json_path: &Path, redb_path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(json_path)?;
    let v: Value = serde_json::from_str(&text)?;
    let db = Database::create(redb_path)?;
    let w = db.begin_write()?;
    {
        let mut meta = w.open_table(META)?;
        let mut cols = w.open_table(COLLECTIONS)?;
        meta.retain(|_, _| false)?;
        cols.retain(|_, _| false)?;
        if let Some(head) = v
            .get("metadata")
            .and_then(|x| x.get("indexed_head"))
            .and_then(|x| x.as_str())
        {
            meta.insert("indexed_head", head)?;
        }
        if let Some(map) = v.get("collections").and_then(|c| c.as_object()) {
            for (name, val) in map {
                let bytes = serde_json::to_vec(val)?;
                cols.insert(name.as_str(), bytes.as_slice())?;
            }
        }
    }
    w.commit()?;
    Ok(())
}

/// Read one collection array from redb (`collections.{name}` in JSON).
pub fn read_collection(redb_path: &Path, collection: &str) -> anyhow::Result<Option<Value>> {
    if !redb_path.exists() {
        return Ok(None);
    }
    let db = Database::open(redb_path)?;
    let r = db.begin_read()?;
    let t = r.open_table(COLLECTIONS)?;
    let Some(guard) = t.get(collection)? else {
        return Ok(None);
    };
    let slice = guard.value();
    Ok(Some(serde_json::from_slice(slice)?))
}

/// `metadata.indexed_head` from redb, if present.
pub fn read_indexed_head(redb_path: &Path) -> anyhow::Result<Option<String>> {
    if !redb_path.exists() {
        return Ok(None);
    }
    let db = Database::open(redb_path)?;
    let r = db.begin_read()?;
    let t = r.open_table(META)?;
    Ok(t.get("indexed_head")?.map(|g| g.value().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn sync_json_roundtrip_collections_and_meta() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("index.db.json");
        let redb_path = dir.path().join("index.redb");

        std::fs::write(
            &json_path,
            json!({
                "metadata": { "indexed_head": "abc123" },
                "collections": {
                    "index": [ { "title": "A", "n": 1 } ],
                    "other": [ "x" ]
                }
            })
            .to_string(),
        )
        .unwrap();

        sync_json_to_redb(&json_path, &redb_path).unwrap();

        assert_eq!(
            read_indexed_head(&redb_path).unwrap().as_deref(),
            Some("abc123")
        );

        let idx = read_collection(&redb_path, "index").unwrap().unwrap();
        let arr = idx.as_array().unwrap();
        assert_eq!(arr[0]["title"], "A");

        let other = read_collection(&redb_path, "other").unwrap().unwrap();
        let oarr = other.as_array().unwrap();
        assert_eq!(oarr[0], json!("x"));
    }

    #[test]
    fn read_missing_redb_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("nope.redb");
        assert!(read_collection(&p, "index").unwrap().is_none());
    }
}

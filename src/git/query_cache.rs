//! In-process LRU for QUERY results (repo tip + collection + query fingerprint).
//! Invalidates naturally when tip HEAD changes (tip is part of the key).

use crate::git::query::QueryOutcome;
use crate::git::sqlite_index::QueryOrder;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 64;
const TTL: Duration = Duration::from_secs(45);

struct Entry {
    at: Instant,
    outcome: QueryOutcome,
}

static CACHE: Mutex<CacheInner> = Mutex::new(CacheInner {
    map: None,
    order: None,
});

struct CacheInner {
    map: Option<HashMap<String, Entry>>,
    order: Option<VecDeque<String>>,
}

fn with_cache<R>(f: impl FnOnce(&mut HashMap<String, Entry>, &mut VecDeque<String>) -> R) -> R {
    let mut guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.map.is_none() {
        guard.map = Some(HashMap::new());
        guard.order = Some(VecDeque::new());
    }
    // Destructure so map/order are independent field borrows (avoids E0499 on Option::as_mut).
    let CacheInner { map, order } = &mut *guard;
    f(map.as_mut().unwrap(), order.as_mut().unwrap())
}

pub fn cache_key(
    repo: &str,
    tip_head: &str,
    collection: &str,
    query: Option<&Value>,
    limit: Option<usize>,
    order: QueryOrder,
) -> String {
    let q = query
        .map(|v| v.to_string())
        .unwrap_or_else(|| String::new());
    format!(
        "{}|{}|{}|{}|{}|{}",
        repo,
        tip_head,
        collection,
        q,
        limit.unwrap_or(0),
        order.as_str()
    )
}

pub fn get(key: &str) -> Option<QueryOutcome> {
    with_cache(|map, order| {
        let Some(entry) = map.get(key) else {
            return None;
        };
        if entry.at.elapsed() > TTL {
            map.remove(key);
            order.retain(|k| k != key);
            return None;
        }
        // Refresh LRU order
        order.retain(|k| k != key);
        order.push_back(key.to_string());
        Some(entry.outcome.clone())
    })
}

pub fn put(key: String, outcome: QueryOutcome) {
    with_cache(|map, order| {
        if map.contains_key(&key) {
            order.retain(|k| k != &key);
        }
        while map.len() >= MAX_ENTRIES {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            } else {
                break;
            }
        }
        order.push_back(key.clone());
        map.insert(
            key,
            Entry {
                at: Instant::now(),
                outcome,
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_outcome(tip: &str) -> QueryOutcome {
        QueryOutcome {
            results: vec![json!({"path": "a.md"})],
            collection: "index".into(),
            query: Some(json!("venus")),
            tip_head: tip.into(),
            indexed_head: Some(tip.into()),
            index_fresh: true,
            doc_count: 1,
            took_ms: 1,
            status: "ok",
        }
    }

    #[test]
    fn cache_hit_and_tip_key_miss() {
        let k1 = cache_key("repo", "aaa", "index", Some(&json!("q")), Some(10), QueryOrder::default());
        put(k1.clone(), sample_outcome("aaa"));
        assert!(get(&k1).is_some());
        let k2 = cache_key("repo", "bbb", "index", Some(&json!("q")), Some(10), QueryOrder::default());
        assert!(get(&k2).is_none());
    }
}

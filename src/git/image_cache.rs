//! On-disk cache for WebP derivatives (global budget per relay-gateway process / node).

use anyhow::{Context as _, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

const CACHE_SUBDIR_DEPTH: usize = 2;

/// Default total cache size budget (bytes) — 1 GiB, shared across all repos on this node.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 1024_u64 * 1024_u64 * 1024_u64;

/// Environment: cache directory (must be writable). Default `/var/cache/relay-image`.
pub fn cache_root_from_env() -> PathBuf {
    std::env::var("RELAY_IMAGE_CACHE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/cache/relay-image"))
}

pub fn max_cache_bytes_from_env() -> u64 {
    std::env::var("RELAY_IMAGE_CACHE_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_CACHE_BYTES)
}

/// Deterministic SHA-256 hex (64 chars) from cache key inputs.
pub fn cache_key_digest(
    repo: &str,
    branch: &str,
    commit: &str,
    blob_oid: &str,
    max_w: u32,
) -> String {
    let mut h = Sha256::new();
    h.update(repo.as_bytes());
    h.update(b"|");
    h.update(branch.as_bytes());
    h.update(b"|");
    h.update(commit.as_bytes());
    h.update(b"|");
    h.update(blob_oid.as_bytes());
    h.update(b"|");
    h.update(max_w.to_string().as_bytes());
    h.update(b"|webp");
    hex::encode(h.finalize())
}

fn path_under_root(root: &Path, key_hex: &str) -> PathBuf {
    debug_assert_eq!(key_hex.len(), 64);
    root.join(&key_hex[..CACHE_SUBDIR_DEPTH])
        .join(format!("{}.webp", key_hex))
}

fn sibling_temp(path: &Path) -> PathBuf {
    format!("{}.{}", path.display(), std::process::id())
        .into()
}

/// Total size of `.webp` files under `root`.
pub fn cached_bytes_total(root: &Path) -> Result<u64> {
    let mut sum: u64 = 0;
    if !root.exists() {
        return Ok(0);
    }
    for e in fs::read_dir(root)? {
        let e = e?;
        let p = e.path();
        let meta = fs::metadata(&p)?;
        if meta.is_dir() {
            for f in fs::read_dir(&p)? {
                let f = f?;
                if f.file_type()?.is_file()
                    && f.path().extension().map(|x| x == "webp").unwrap_or(false)
                {
                    sum += fs::metadata(f.path())?.len();
                }
            }
        } else if meta.is_file()
            && p.extension().map(|x| x == "webp").unwrap_or(false)
        {
            sum += meta.len();
        }
    }
    Ok(sum)
}

fn list_webp_entries(root: &Path) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for e in fs::read_dir(root)? {
        let e = e?;
        let p = e.path();
        let meta = fs::metadata(&p)?;
        if meta.is_dir() {
            for f in fs::read_dir(&p)? {
                let f = f?;
                let fp = f.path();
                if f.file_type()?.is_file()
                    && fp.extension().map(|x| x == "webp").unwrap_or(false)
                {
                    let m = fs::metadata(&fp)?;
                    let tm = m.modified().unwrap_or_else(|_| SystemTime::UNIX_EPOCH);
                    out.push((fp, m.len(), tm));
                }
            }
        } else if meta.is_file()
            && p.extension().map(|x| x == "webp").unwrap_or(false)
        {
            let tm = meta.modified().unwrap_or_else(|_| SystemTime::UNIX_EPOCH);
            out.push((p, meta.len(), tm));
        }
    }
    Ok(out)
}

/// Evict oldest files (by `modified`) until total size ≤ `target_max`.
pub(crate) fn evict_to_budget(root: &Path, target_max: u64) -> Result<()> {
    loop {
        let entries = list_webp_entries(root)?;
        let total: u64 = entries.iter().map(|(_, sz, _)| sz).sum();
        if total <= target_max {
            break;
        }
        if entries.is_empty() {
            break;
        }
        let mut sorted = entries;
        sorted.sort_by_key(|(_, _, t)| *t);
        if let Some((path, _, _)) = sorted.into_iter().next() {
            let _ = fs::remove_file(&path);
        } else {
            break;
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct ImageDiskCache {
    root: PathBuf,
    max_bytes: u64,
    mutex: Mutex<()>,
}

impl ImageDiskCache {
    pub fn from_env() -> Self {
        Self::new(cache_root_from_env(), max_cache_bytes_from_env())
    }

    pub fn new(root: PathBuf, max_bytes: u64) -> Self {
        Self {
            root,
            max_bytes,
            mutex: Mutex::new(()),
        }
    }

    fn ensure_parent(path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        Ok(())
    }

    pub fn read_cached(&self, key_hex: &str) -> Result<Option<Vec<u8>>> {
        let path = path_under_root(&self.root, key_hex);
        let _guard = self
            .mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("cache lock poisoned"))?;
        if !path.is_file() {
            return Ok(None);
        }
        let mut f = fs::File::open(&path).context("open cache hit")?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }

    /// Write `webp_bytes` to cache and shrink until roughly under budget.
    pub fn write_and_evict(&self, key_hex: &str, webp_bytes: &[u8]) -> Result<()> {
        let path = path_under_root(&self.root, key_hex);
        let _guard = self
            .mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("cache lock poisoned"))?;
        Self::ensure_parent(&path)?;
        let tmp = sibling_temp(&path);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(webp_bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path).context("commit cache entry")?;

        loop {
            let total = cached_bytes_total(&self.root)?;
            if total <= self.max_bytes {
                break;
            }
            let target = self.max_bytes.saturating_mul(92) / 100;
            evict_to_budget(&self.root, target)?;
            let total_after = cached_bytes_total(&self.root)?;
            if total_after >= total {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn eviction_drops_oldest() {
        let dir = tempdir().unwrap();
        let c = ImageDiskCache::new(dir.path().join("cache"), 2000);

        fn put(c: &ImageDiskCache, label: &[u8], webp_fake: &[u8]) -> String {
            let hex = hex::encode(Sha256::digest(label));
            c.write_and_evict(&hex, webp_fake).unwrap();
            hex
        }

        std::thread::sleep(std::time::Duration::from_millis(15));
        let k1 = put(&c, b"aaa", &[1; 900]);
        std::thread::sleep(std::time::Duration::from_millis(15));
        put(&c, b"bbb", &[2; 900]);
        std::thread::sleep(std::time::Duration::from_millis(15));
        put(&c, b"ccc", &[3; 900]);

        let total = cached_bytes_total(&c.root).unwrap();
        assert!(total <= c.max_bytes, "total {}", total);

        let p1 = path_under_root(&c.root, &k1);
        assert!(
            !p1.is_file(),
            "oldest oversized entry should be evicted ({p1:?})"
        );
    }
}

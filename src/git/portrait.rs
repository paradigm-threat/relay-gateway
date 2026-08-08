//! Portrait sibling art-direction helpers (`foo.ext` → `foo-portrait.ext`).
//!
//! Used by markdown HTML embed rendering and tip SSR bootstrap so clients do not
//! invent `<picture>` sources for missing blobs (Relay missing paths historically
//! returned HTTP 200 JSON, which blanks mobile `<source>` selection).

use git2::{ObjectType, Repository};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::git::repo::read_file_from_repo;
use crate::types::ReadError;

/// Narrow-viewport media query (PopImage / navbar parity).
pub const PORTRAIT_MEDIA_QUERY: &str = "(max-width: 768px)";

const PORTRAIT_SUFFIX: &str = "-portrait";

/// Landscape `foo.ext` → sibling `foo-portrait.ext`, preserving origin + query (`?w=`).
/// Empty / None when SVG, already `-portrait`, or unparseable.
pub fn portrait_sibling_src(src: &str) -> Option<String> {
    if src.is_empty() {
        return None;
    }
    let (path_and_host, query) = match src.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (src, None),
    };
    let lower = path_and_host.to_ascii_lowercase();
    if lower.ends_with(".svg") {
        return None;
    }
    let (prefix, file) = match path_and_host.rfind('/') {
        Some(i) => (&path_and_host[..=i], &path_and_host[i + 1..]),
        None => ("", path_and_host),
    };
    let dot = file.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let stem = &file[..dot];
    let ext = &file[dot..];
    if stem.to_ascii_lowercase().ends_with(PORTRAIT_SUFFIX) {
        return None;
    }
    let mut out = String::with_capacity(src.len() + PORTRAIT_SUFFIX.len());
    out.push_str(prefix);
    out.push_str(stem);
    out.push_str(PORTRAIT_SUFFIX);
    out.push_str(ext);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

/// Strip scheme/host and query → repo-relative path (`governance/war/foo.png`).
pub fn repo_rel_from_media_src(src: &str) -> Option<String> {
    let no_query = src.split('?').next().unwrap_or(src);
    let path = if no_query.starts_with("http://") || no_query.starts_with("https://") {
        let rest = no_query
            .split_once("://")
            .map(|(_, r)| r)
            .unwrap_or(no_query);
        let path = rest.find('/').map(|i| &rest[i..]).unwrap_or("/");
        path
    } else {
        no_query
    };
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    Some(rel.to_string())
}

fn is_portrait_rel(rel: &str) -> bool {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let Some(dot) = file.rfind('.') else {
        return false;
    };
    let stem = &file[..dot];
    stem.to_ascii_lowercase().ends_with(PORTRAIT_SUFFIX)
}

fn is_raster_ext(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.ends_with(".gif")
}

/// True when the tip blob exists (any content).
pub fn tip_blob_exists(repo_path: &Path, branch: &str, rel: &str) -> bool {
    match read_file_from_repo(&repo_path.to_path_buf(), branch, rel) {
        Ok(_) => true,
        Err(ReadError::NotFound) => false,
        Err(_) => false,
    }
}

/// Invent portrait URL and return it only when the tip sibling blob exists.
pub fn portrait_sibling_if_present(repo_path: &Path, branch: &str, src: &str) -> Option<String> {
    let cand = portrait_sibling_src(src)?;
    let rel = repo_rel_from_media_src(&cand)?;
    if tip_blob_exists(repo_path, branch, &rel) {
        Some(cand)
    } else {
        None
    }
}

fn tip_commit_oid(repo_path: &Path, branch: &str) -> Option<String> {
    let repo = Repository::open(repo_path)
        .or_else(|_| Repository::open_bare(repo_path))
        .ok()?;
    let reference = repo
        .find_reference(&format!("refs/heads/{}", branch))
        .ok()?;
    let commit = reference.peel_to_commit().ok()?;
    Some(commit.id().to_string())
}

struct PortraitIndexCache {
    /// key: `repo_path\0branch\0tip_oid` → set of repo-relative portrait paths
    entries: HashMap<String, HashSet<String>>,
}

fn portrait_index_state() -> &'static Mutex<PortraitIndexCache> {
    static STATE: OnceLock<Mutex<PortraitIndexCache>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(PortraitIndexCache {
            entries: HashMap::new(),
        })
    })
}

fn walk_portrait_rels(repo_path: &Path, branch: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(repo) = Repository::open(repo_path).or_else(|_| Repository::open_bare(repo_path)) else {
        return out;
    };
    let Ok(reference) = repo.find_reference(&format!("refs/heads/{}", branch)) else {
        return out;
    };
    let Ok(commit) = reference.peel_to_commit() else {
        return out;
    };
    let Ok(tree) = commit.tree() else {
        return out;
    };
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() != Some(ObjectType::Blob) {
            return git2::TreeWalkResult::Ok;
        }
        let name = entry.name().unwrap_or("");
        if name.is_empty() {
            return git2::TreeWalkResult::Ok;
        }
        let rel = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}{name}")
        };
        if is_raster_ext(&rel) && is_portrait_rel(&rel) {
            out.insert(rel);
        }
        git2::TreeWalkResult::Ok
    });
    out
}

/// Cached set of tip paths that end in `-portrait.{png,jpg,…}`.
pub fn portrait_sibling_set(repo_path: &Path, branch: &str) -> HashSet<String> {
    let oid = tip_commit_oid(repo_path, branch).unwrap_or_else(|| "unknown".into());
    let key = format!("{}\0{}\0{}", repo_path.display(), branch, oid);
    if let Ok(guard) = portrait_index_state().lock() {
        if let Some(hit) = guard.entries.get(&key) {
            return hit.clone();
        }
    }
    let built = walk_portrait_rels(repo_path, branch);
    if let Ok(mut guard) = portrait_index_state().lock() {
        if guard.entries.len() > 64 {
            guard.entries.clear();
        }
        guard.entries.insert(key, built.clone());
    }
    built
}

/// JSON object map `{ "path/to/foo-portrait.png": true, … }` for SSR bootstrap.
pub fn portrait_sibling_set_json(repo_path: &Path, branch: &str) -> String {
    let set = portrait_sibling_set(repo_path, branch);
    let mut map = serde_json::Map::new();
    for rel in set {
        map.insert(rel, serde_json::Value::Bool(true));
    }
    serde_json::Value::Object(map).to_string()
}

/// True when `src`'s invented portrait sibling is in the tip portrait set (no git read).
pub fn portrait_sibling_in_set(src: &str, set: &HashSet<String>) -> Option<String> {
    let cand = portrait_sibling_src(src)?;
    let rel = repo_rel_from_media_src(&cand)?;
    if set.contains(&rel) {
        Some(cand)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    #[test]
    fn invents_portrait_sibling_preserving_query() {
        let s = portrait_sibling_src("/a/b.png?w=800").unwrap();
        assert_eq!(s, "/a/b-portrait.png?w=800");
    }

    #[test]
    fn skips_svg_and_already_portrait() {
        assert!(portrait_sibling_src("/x.svg").is_none());
        assert!(portrait_sibling_src("/x-portrait.png").is_none());
    }

    #[test]
    fn repo_rel_strips_origin_and_query() {
        assert_eq!(
            repo_rel_from_media_src(
                "https://paradigm-threat-files.dfw1.relaygateway.net/governance/war/x.png?w=800"
            )
            .as_deref(),
            Some("governance/war/x.png")
        );
        assert_eq!(
            repo_rel_from_media_src("/governance/war/x.png").as_deref(),
            Some("governance/war/x.png")
        );
    }

    #[test]
    fn set_membership_gates_invent() {
        let mut set = HashSet::new();
        set.insert("governance/war/chart-portrait.png".into());
        assert_eq!(
            portrait_sibling_in_set("/governance/war/chart.png", &set).as_deref(),
            Some("/governance/war/chart-portrait.png")
        );
        assert!(portrait_sibling_in_set("/governance/war/missing.png", &set).is_none());
    }

    fn bare_with_blobs(paths: &[(&str, &[u8])]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        let repo = Repository::init_bare(&bare).unwrap();
        let sig = Signature::now("t", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        for (path, bytes) in paths {
            let blob = repo.blob(bytes).unwrap();
            let entry = git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: bytes.len() as u32,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            };
            index.add(&entry).unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, bare)
    }

    #[test]
    fn tip_set_lists_only_portrait_rasters() {
        let (_dir, bare) = bare_with_blobs(&[
            ("governance/war/hero.png", b"PNG"),
            ("governance/war/hero-portrait.png", b"PNGP"),
            ("governance/war/notes.md", b"# x"),
            ("a.svg", b"<svg/>"),
            ("nested/x-portrait.jpg", b"JPEG"),
        ]);
        let set = portrait_sibling_set(&bare, "main");
        assert!(set.contains("governance/war/hero-portrait.png"), "{set:?}");
        assert!(set.contains("nested/x-portrait.jpg"), "{set:?}");
        assert!(!set.contains("governance/war/hero.png"), "{set:?}");
        assert!(!set.contains("governance/war/notes.md"), "{set:?}");
        assert!(!set.contains("a.svg"), "{set:?}");
    }

    #[test]
    fn portrait_sibling_if_present_reads_tip() {
        let (_dir, bare) = bare_with_blobs(&[
            ("governance/war/hero.png", b"PNG"),
            ("governance/war/hero-portrait.png", b"PNGP"),
            ("governance/war/lonely.png", b"PNG"),
        ]);
        assert_eq!(
            portrait_sibling_if_present(&bare, "main", "/governance/war/hero.png?w=800").as_deref(),
            Some("/governance/war/hero-portrait.png?w=800")
        );
        assert!(
            portrait_sibling_if_present(&bare, "main", "/governance/war/lonely.png").is_none()
        );
    }

    #[test]
    fn portrait_set_json_is_object_map() {
        let (_dir, bare) =
            bare_with_blobs(&[("foo-portrait.png", b"x"), ("foo.png", b"y")]);
        let json = portrait_sibling_set_json(&bare, "main");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["foo-portrait.png"], true);
        assert!(v.get("foo.png").is_none());
    }
}

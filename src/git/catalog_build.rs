//! Build SQLite catalog docs from the tip tree (no committed index.json / events.json).
//! Parity targets: paradigm-threat `generate-index.py` / `build-events-from-content.py`.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const EXCLUDE_DIRS: &[&str] = &[".git", ".github", "scripts", "node_modules", ".idea", "docs"];
const EXCLUDE_FILES: &[&str] = &[
    "index.json",
    "package-lock.json",
    "package.json",
    "tsconfig.json",
    ".gitignore",
    "REORGANIZATION_MAP.md",
    "migrate_paths.py",
    "fix_nested_categories.py",
    "deduplicate_paths.py",
];
const INCLUDE_EXTS: &[&str] = &[
    ".md", ".txt", ".pdf", ".json", ".srt", ".jpg", ".jpeg", ".png", ".webp",
];

const MIN_YEAR: i64 = -10_000;
const MAX_YEAR: i64 = 3_000;

fn re_keywords() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?im)^Keywords:\s*(.+)$").unwrap())
}
fn re_blog() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})-.+\.md$").unwrap())
}
fn re_content() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{2})\.(\d{2})\.(\d{2})-(.+?)\.md$").unwrap())
}
fn re_md_img() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"!\[[^\]]*\]\(([^)\s"]+)(?:\s+"[^"]*")?\)(\s*\{[^}]+\})?"#).unwrap()
    })
}
fn re_html_img() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)<img[^>]+src=["']([^"']+)["']"#).unwrap())
}
fn re_hash_kw() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"#([a-zA-Z0-9\-]+)").unwrap())
}

fn git_ls_tree(repo: &Path, commit: &str) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("ls-tree")
        .arg("-r")
        .arg("--name-only")
        .arg(commit)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
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

fn path_excluded(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    for (i, p) in parts.iter().enumerate() {
        if EXCLUDE_DIRS.contains(p) {
            return true;
        }
        // skip hidden segments except allowing nothing under .git already covered
        if p.starts_with('.') && i == 0 {
            return true;
        }
    }
    let basename = parts.last().copied().unwrap_or("");
    EXCLUDE_FILES.contains(&basename) || basename.starts_with('.')
}

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default()
}

fn extract_md_title_keywords(content: &str) -> (Option<String>, Vec<String>) {
    let mut title = None;
    for line in content.lines().take(40) {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("# ") {
            title = Some(rest.trim().to_string());
            break;
        }
    }
    let mut kw = Vec::new();
    if let Some(m) = re_keywords().captures(content) {
        for cap in re_hash_kw().captures_iter(&m[1]) {
            kw.push(cap[1].to_string());
        }
    }
    (title, kw)
}

fn blog_updated_iso(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || parts[0] != "blog" {
        return None;
    }
    let basename = *parts.last()?;
    let m = re_blog().captures(basename)?;
    Some(format!("{}T12:00:00Z", &m[1]))
}

/// Flat path-catalog docs (leaf shape compatible with flatten_path_index).
pub fn build_path_tree_docs(repo: &Path, commit: &str) -> Result<Vec<Value>, String> {
    let paths = git_ls_tree(repo, commit)?;
    let mut docs = Vec::new();
    for path in paths {
        if path_excluded(&path) {
            continue;
        }
        let ext = ext_of(&path);
        let is_hooks_cs = ext == ".cs" && path.starts_with("hooks/");
        if !INCLUDE_EXTS.contains(&ext.as_str()) && !is_hooks_cs {
            continue;
        }

        let mut doc = Map::new();
        doc.insert("path".into(), Value::String(path.clone()));
        let basename = Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&path)
            .to_string();

        if ext == ".md" {
            if let Some(text) = git_show(repo, commit, &path) {
                let (title, kw) = extract_md_title_keywords(&text);
                if let Some(t) = title {
                    doc.insert("_title".into(), Value::String(t.clone()));
                    doc.insert("title".into(), Value::String(t));
                } else {
                    doc.insert("title".into(), Value::String(basename));
                }
                if !kw.is_empty() {
                    doc.insert(
                        "_keywords".into(),
                        Value::Array(kw.into_iter().map(Value::String).collect()),
                    );
                }
            }
        } else if is_hooks_cs {
            if let Some(text) = git_show(repo, commit, &path) {
                for line in text.lines().take(40) {
                    let s = line.trim();
                    if let Some(rest) = s.strip_prefix("//") {
                        let t = rest.trim();
                        let decorative = !t.is_empty()
                            && t.chars().all(|c| "=-#*_".contains(c))
                            && t.chars().collect::<std::collections::HashSet<_>>().len() <= 2;
                        if !t.is_empty() && !decorative {
                            let title: String = t.chars().take(240).collect();
                            doc.insert("_title".into(), Value::String(title.clone()));
                            doc.insert("title".into(), Value::String(title));
                            break;
                        }
                    } else if !s.is_empty() && !s.starts_with("/*") {
                        break;
                    }
                }
            }
            if !doc.contains_key("title") {
                doc.insert("title".into(), Value::String(basename));
            }
        } else {
            doc.insert("title".into(), Value::String(basename));
        }

        if let Some(iso) = blog_updated_iso(&path) {
            doc.insert("_updated".into(), Value::String(iso));
        }

        docs.push(Value::Object(doc));
    }
    Ok(docs)
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

fn first_header(content: &str) -> Option<String> {
    let (body, _) = strip_frontmatter(content);
    for line in body.lines() {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn parse_fm(fm: &str) -> Map<String, Value> {
    match serde_yaml::from_str::<Value>(fm) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn dates_from_slug(slug: &str) -> Option<Vec<Value>> {
    if let Some(m) = Regex::new(r"^.*?(\d{4})-(\d{4})").unwrap().captures(slug) {
        let start: i64 = m[1].parse().ok()?;
        let end: i64 = m[2].parse().ok()?;
        if !(MIN_YEAR..=MAX_YEAR).contains(&start)
            || !(MIN_YEAR..=MAX_YEAR).contains(&end)
            || start > end
        {
            return None;
        }
        return Some(vec![json!({"start": start, "end": end, "calendar": "CE"})]);
    }
    if let Some(m) = Regex::new(r"(?i)(\d{4})-bce").unwrap().captures(slug) {
        let y: i64 = m[1].parse().ok()?;
        let s = -y;
        return Some(vec![json!({"start": s, "end": s, "calendar": "BCE"})]);
    }
    if let Some(m) = Regex::new(r"(?i)(\d{4})-ce").unwrap().captures(slug) {
        let y: i64 = m[1].parse().ok()?;
        return Some(vec![json!({"start": y, "end": y, "calendar": "CE"})]);
    }
    None
}

fn normalize_media_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with("data:") || raw.starts_with('#') {
        return None;
    }
    Some(if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("/{raw}")
    })
}

fn media_from_md(content: &str) -> Vec<Value> {
    let mut order = Vec::new();
    let mut flags: HashMap<String, bool> = HashMap::new();
    for m in re_md_img().captures_iter(content) {
        let Some(p) = normalize_media_path(&m[1]) else {
            continue;
        };
        let book = m
            .get(2)
            .map(|b| b.as_str().contains(".book-illustration"))
            .unwrap_or(false);
        flags
            .entry(p.clone())
            .and_modify(|b| *b = *b || book)
            .or_insert_with(|| {
                order.push(p.clone());
                book
            });
    }
    for m in re_html_img().captures_iter(content) {
        let Some(p) = normalize_media_path(&m[1]) else {
            continue;
        };
        flags.entry(p.clone()).or_insert_with(|| {
            order.push(p.clone());
            false
        });
    }
    order
        .into_iter()
        .map(|p| {
            if flags.get(&p) == Some(&true) {
                json!({"path": p, "bookIllustration": true})
            } else {
                json!({"path": p})
            }
        })
        .collect()
}

fn keywords(content: &str) -> Vec<String> {
    let mut kw = Vec::new();
    if let Some(m) = re_keywords().captures(content) {
        for cap in re_hash_kw().captures_iter(&m[1]) {
            kw.push(cap[1].to_string());
        }
    }
    kw
}

type ContentFile = (String /*path*/, i32, i32, i32, String /*slug*/);

fn discover_content(paths: &[String]) -> Result<Vec<ContentFile>, String> {
    let mut result = Vec::new();
    let mut orphans = Vec::new();
    for path in paths {
        if !path.starts_with("content/") || !path.ends_with(".md") {
            continue;
        }
        let basename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if let Some(m) = re_content().captures(basename) {
            result.push((
                path.clone(),
                m[1].parse().unwrap_or(0),
                m[2].parse().unwrap_or(0),
                m[3].parse().unwrap_or(0),
                m[4].to_string(),
            ));
        } else {
            orphans.push(path.clone());
        }
    }
    if !orphans.is_empty() {
        return Err(format!(
            "orphan content .md (must match XX.YY.ZZ-slug.md): {}",
            orphans.join(", ")
        ));
    }
    result.sort_by_key(|t| (t.1, t.2, t.3));
    Ok(result)
}

fn build_entry(repo: &Path, commit: &str, md_path: &str, slug: &str) -> Result<Value, String> {
    let text =
        git_show(repo, commit, md_path).ok_or_else(|| format!("{md_path}: missing blob"))?;
    let title = first_header(&text)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| format!("{md_path}: missing first # header"))?;
    let (_, fm_raw) = strip_frontmatter(&text);
    let fm = fm_raw.map(parse_fm).unwrap_or_default();

    let evt_id = match fm.get("id") {
        Some(Value::String(id)) => {
            let id = id.trim();
            if !id.starts_with("evt-") {
                return Err(format!("{md_path}: frontmatter id must start with evt-"));
            }
            id.to_string()
        }
        _ => format!("evt-{slug}"),
    };

    let mut dates = dates_from_slug(slug);
    if let Some(Value::Array(arr)) = fm.get("dates") {
        if let Some(Value::Object(d0)) = arr.first() {
            let start = d0
                .get("start")
                .or_else(|| d0.get("value"))
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
            let end = d0
                .get("end")
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .or(start);
            if let Some(start) = start {
                let cal = d0
                    .get("calendar")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if end.is_some_and(|e| start < 0 && e > 0) {
                            "mixed".into()
                        } else if start < 0 {
                            "BCE".into()
                        } else {
                            "CE".into()
                        }
                    });
                let mut d = json!({"start": start, "end": end.unwrap_or(start), "calendar": cal});
                if let Some(obj) = d.as_object_mut() {
                    if let Some(v) = d0.get("display_year") {
                        obj.insert("display_year".into(), v.clone());
                    }
                    if let Some(v) = d0.get("note") {
                        obj.insert("note".into(), v.clone());
                    }
                }
                dates = Some(vec![d]);
            } else if let Some(end) = end {
                let cal = d0
                    .get("calendar")
                    .and_then(|v| v.as_str())
                    .unwrap_or(if end < 0 { "BCE" } else { "CE" });
                let start_label = d0
                    .get("start_label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("∞");
                dates = Some(vec![json!({
                    "start": Value::Null,
                    "end": end,
                    "calendar": cal,
                    "start_label": start_label,
                })]);
            }
        }
    }

    let mut entry = json!({
        "id": evt_id,
        "title": title,
        "type": if dates.is_some() { "event" } else { "article" },
        "md_path": md_path,
        "children": [],
    });
    let obj = entry.as_object_mut().unwrap();
    if let Some(d) = dates {
        obj.insert("dates".into(), Value::Array(d));
    }
    for key in [
        "related_events",
        "duplicate_of",
        "tags",
        "timeline_sources",
        "context",
        "categories",
    ] {
        if let Some(v) = fm.get(key) {
            if !v.is_null() {
                obj.insert(key.into(), v.clone());
            }
        }
    }
    if let Some(Value::Array(media)) = fm.get("media") {
        let mut norm = Vec::new();
        for p in media {
            match p {
                Value::String(s) => {
                    if let Some(path) = normalize_media_path(s) {
                        norm.push(json!({"path": path}));
                    }
                }
                Value::Object(m) => {
                    if let Some(path) = m
                        .get("path")
                        .or_else(|| m.get("src"))
                        .and_then(|v| v.as_str())
                        .and_then(normalize_media_path)
                    {
                        let book = m
                            .get("bookIllustration")
                            .or_else(|| m.get("book_illustration"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        norm.push(if book {
                            json!({"path": path, "bookIllustration": true})
                        } else {
                            json!({"path": path})
                        });
                    }
                }
                _ => {}
            }
        }
        if !norm.is_empty() {
            obj.insert("media".into(), Value::Array(norm));
        }
    } else {
        let extracted = media_from_md(&text);
        if !extracted.is_empty() {
            obj.insert("media".into(), Value::Array(extracted));
        }
    }
    let kw = keywords(&text);
    if !kw.is_empty() {
        obj.insert(
            "keywords".into(),
            Value::Array(kw.into_iter().map(Value::String).collect()),
        );
    }
    Ok(entry)
}

fn flatten_tree(entries: &[Value], out: &mut Vec<Value>) {
    for e in entries {
        out.push(e.clone());
        if let Some(ch) = e.get("children").and_then(|c| c.as_array()) {
            flatten_tree(ch, out);
        }
    }
}

fn collect_intro(node: &Value, out: &mut Vec<Value>) {
    out.push(json!({
        "id": node.get("id").cloned().unwrap_or(Value::Null),
        "title": node.get("title").cloned().unwrap_or(Value::Null),
        "md_path": node.get("md_path").cloned().unwrap_or(Value::Null),
    }));
    if let Some(ch) = node.get("children").and_then(|c| c.as_array()) {
        for c in ch {
            collect_intro(c, out);
        }
    }
}

/// Build event docs with nested children on parents (same flatten shape as events_json ingest).
pub fn build_content_events_docs(repo: &Path, commit: &str) -> Result<Vec<Value>, String> {
    let paths = git_ls_tree(repo, commit)?;
    let files = discover_content(&paths)?;
    if files.is_empty() {
        return Err("no content files found".into());
    }

    let mut by_key: HashMap<(i32, i32, i32), Value> = HashMap::new();
    for (md_path, xx, yy, zz, slug) in &files {
        let key = (*xx, *yy, *zz);
        if by_key.contains_key(&key) {
            return Err(format!(
                "Duplicate XX.YY.ZZ {xx:02}.{yy:02}.{zz:02}: {} and {md_path}",
                by_key[&key]["md_path"].as_str().unwrap_or("?")
            ));
        }
        by_key.insert(key, build_entry(repo, commit, md_path, slug)?);
    }

    // Materialize hierarchy: roots (yy=0,zz=0), sections (zz=0), leaves.
    // Use Rc-like updates via rebuilding from a parent-id map.
    let mut roots: Vec<Value> = Vec::new();
    let mut section_ids: HashMap<(i32, i32), String> = HashMap::new(); // (xx,yy) -> id
    let mut nodes: HashMap<String, Value> = HashMap::new(); // id -> node
    let mut root_order: Vec<String> = Vec::new();

    let mut keys: Vec<(i32, i32, i32)> = by_key.keys().copied().collect();
    keys.sort();

    for (xx, yy, zz) in keys {
        let entry = by_key.remove(&(xx, yy, zz)).unwrap();
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if yy == 0 && zz == 0 {
            nodes.insert(id.clone(), entry);
            section_ids.insert((xx, 0), id.clone());
            root_order.push(id);
        } else if zz == 0 {
            let parent_id = section_ids.get(&(xx, 0)).cloned();
            nodes.insert(id.clone(), entry);
            section_ids.insert((xx, yy), id.clone());
            if let Some(pid) = parent_id {
                if let Some(parent) = nodes.get_mut(&pid) {
                    if let Some(ch) = parent.get_mut("children").and_then(|c| c.as_array_mut()) {
                        ch.push(json!({"id": id})); // placeholder; resolve later
                    }
                }
            }
        } else {
            let parent_id = section_ids
                .get(&(xx, yy))
                .cloned()
                .or_else(|| section_ids.get(&(xx, 0)).cloned());
            nodes.insert(id.clone(), entry);
            if let Some(pid) = parent_id {
                if let Some(parent) = nodes.get_mut(&pid) {
                    if let Some(ch) = parent.get_mut("children").and_then(|c| c.as_array_mut()) {
                        ch.push(json!({"id": id}));
                    }
                }
            }
        }
    }

    // Resolve placeholders to full nested objects (bottom-up by sorting keys reverse depth is hard);
    // instead rebuild children arrays recursively.
    fn materialize(id: &str, nodes: &HashMap<String, Value>) -> Value {
        let mut node = nodes.get(id).cloned().unwrap_or(json!({}));
        let child_ids: Vec<String> = node
            .get("children")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        // placeholder {"id": "..."} or already full
                        c.get("id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .filter(|_| c.get("md_path").is_none())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !child_ids.is_empty() {
            let kids: Vec<Value> = child_ids.iter().map(|cid| materialize(cid, nodes)).collect();
            if let Some(obj) = node.as_object_mut() {
                obj.insert("children".into(), Value::Array(kids));
            }
        }
        node
    }

    for id in &root_order {
        roots.push(materialize(id, &nodes));
    }

    let mut intro = Vec::new();
    // xx==0 root is first with overview path
    for r in &roots {
        let md = r.get("md_path").and_then(|p| p.as_str()).unwrap_or("");
        if md.contains("00.overview") || md.contains("content/00.") {
            collect_intro(r, &mut intro);
            break;
        }
    }

    let mut docs = Vec::new();
    flatten_tree(&roots, &mut docs);
    if !intro.is_empty() {
        docs.push(json!({
            "id": "__introArticles__",
            "title": "introArticles",
            "type": "manifest",
            "introArticles": intro,
            "children": [],
        }));
    }
    docs.push(json!({
        "id": "__meta__",
        "title": "meta",
        "type": "manifest",
        "meta": {"source": "catalog_build::content_events"},
        "children": [],
    }));
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    fn commit_files(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf, String) {
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
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, repo_path, oid.to_string())
    }

    #[test]
    fn path_tree_indexes_markdown() {
        let (_dir, repo, commit) = commit_files(&[(
            "cosmos/venus/page.md",
            "# Venus\n\nHello\n\nKeywords: #venus #planet\n",
        )]);
        let docs = build_path_tree_docs(&repo, &commit).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0]["path"], "cosmos/venus/page.md");
        assert_eq!(docs[0]["_title"], "Venus");
    }

    #[test]
    fn content_events_builds_hierarchy() {
        let (_dir, repo, commit) = commit_files(&[
            (
                "content/00.overview/00.00.00-introduction.md",
                "# Introduction\n\nKeywords: #intro\n",
            ),
            (
                "content/00.overview/00.01.00-the-third-story.md",
                "# The Third Story\n\nKeywords: #third\n",
            ),
            (
                "content/01.era/01.00.00-era-root.md",
                "# Era Root\n\nKeywords: #era\n",
            ),
        ]);
        let docs = build_content_events_docs(&repo, &commit).unwrap();
        let events: Vec<_> = docs
            .iter()
            .filter(|d| d.get("type").and_then(|t| t.as_str()) != Some("manifest"))
            .collect();
        assert!(events.len() >= 3, "got {}", events.len());
        let intro = docs.iter().find(|d| d["id"] == "__introArticles__");
        assert!(intro.is_some());
        let root = events
            .iter()
            .find(|e| e["id"] == "evt-introduction")
            .unwrap();
        let kids = root["children"].as_array().unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0]["id"], "evt-the-third-story");
    }
}

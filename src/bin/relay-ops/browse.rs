//! On-demand repo browse for the local fleet UI (observer; no SSH).
//!
//! Prefers HTTPS `X-Relay-Tree` / blob GET on `{repo}.{node}`; falls back to
//! `git://` partial clone (`blob:none`) on older nodes.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

fn repo_mutex(key: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchRef {
    pub name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeEntry {
    pub name: String,
    pub kind: String,
    pub mode: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileBody {
    pub path: String,
    pub binary: bool,
    pub truncated: bool,
    pub text: String,
}

pub fn git_url(host: &str, port: u16, repo: &str) -> Result<String> {
    let host = ident(host, "fqdn")?;
    let repo = ident(repo, "repo")?;
    Ok(format!("git://{host}:{port}/{repo}.git"))
}

fn ident(s: &str, what: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() || t.len() > 200 {
        return Err(anyhow!("invalid {what}"));
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(anyhow!("invalid {what}"));
    }
    if t.contains("..") {
        return Err(anyhow!("invalid {what}"));
    }
    Ok(t.to_string())
}

pub fn sanitize_ref(git_ref: &str) -> Result<String> {
    let t = git_ref.trim();
    if t.is_empty() || t.len() > 200 || t.contains("..") || t.starts_with('-') {
        return Err(anyhow!("invalid ref"));
    }
    if t.chars()
        .all(|c| c.is_ascii_hexdigit() && t.len() >= 7 && t.len() <= 40)
    {
        return Ok(t.to_string());
    }
    if t.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
    {
        return Ok(t.to_string());
    }
    Err(anyhow!("invalid ref"))
}

pub fn sanitize_path(path: &str) -> Result<String> {
    let t = path.trim().trim_start_matches('/');
    if t.len() > 512 {
        return Err(anyhow!("path too long"));
    }
    if t.is_empty() {
        return Ok(String::new());
    }
    for part in t.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(anyhow!("invalid path"));
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '=' | '@'))
        {
            return Err(anyhow!("invalid path segment"));
        }
    }
    Ok(t.to_string())
}

pub fn parse_ls_remote_heads(text: &str) -> Vec<BranchRef> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut bits = line.split_whitespace();
        let Some(sha) = bits.next() else { continue };
        let Some(r) = bits.next() else { continue };
        let Some(name) = r.strip_prefix("refs/heads/") else {
            continue;
        };
        if sha.len() >= 7 && !name.is_empty() {
            out.push(BranchRef {
                name: name.to_string(),
                sha: sha.to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn parse_ls_tree(text: &str) -> Vec<TreeEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        // <mode> SP <type> SP <object> TAB <file>
        let Some((meta, name)) = line.split_once('\t') else {
            continue;
        };
        let mut bits = meta.split_whitespace();
        let Some(mode) = bits.next() else { continue };
        let Some(kind) = bits.next() else { continue };
        let Some(sha) = bits.next() else { continue };
        let base = name.rsplit('/').next().unwrap_or(name);
        if base.is_empty() {
            continue;
        }
        out.push(TreeEntry {
            name: base.to_string(),
            kind: kind.to_string(),
            mode: mode.to_string(),
            sha: sha.to_string(),
        });
    }
    out.sort_by(|a, b| match (a.kind.as_str(), b.kind.as_str()) {
        ("tree", "blob") => std::cmp::Ordering::Less,
        ("blob", "tree") => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    out
}

const HEADER_TREE: &str = "X-Relay-Tree";
const HEADER_BRANCH: &str = "X-Relay-Branch";
const TREE_MEDIA: &str = "application/vnd.relay.tree+json";

fn https_client() -> Result<&'static reqwest::Client> {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    Ok(C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .expect("reqwest client")
    }))
}

fn https_repo_url(host: &str, repo: &str, path: &str) -> Result<String> {
    let host = ident(host, "fqdn")?;
    let repo = ident(repo, "repo")?;
    let path = sanitize_path(path)?;
    if path.is_empty() {
        Ok(format!("https://{repo}.{host}/"))
    } else {
        Ok(format!("https://{repo}.{host}/{path}"))
    }
}

fn tree_kind_from_relay(t: &str) -> &'static str {
    match t {
        "dir" => "tree",
        "file" => "blob",
        "commit" => "commit",
        _ => "blob",
    }
}

/// Map gateway `relay.tree.v1` JSON to the UI `ls-tree` shape (`tree`/`blob`).
pub fn parse_relay_tree_v1(v: &serde_json::Value) -> Result<Vec<TreeEntry>> {
    if v.get("schema").and_then(|s| s.as_str()) != Some("relay.tree.v1") {
        return Err(anyhow!("not relay.tree.v1"));
    }
    let Some(arr) = v.get("entries").and_then(|e| e.as_array()) else {
        return Err(anyhow!("missing entries"));
    };
    let mut out = Vec::new();
    for e in arr {
        let name = e.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let typ = e.get("type").and_then(|x| x.as_str()).unwrap_or("file");
        out.push(TreeEntry {
            name: name.to_string(),
            kind: tree_kind_from_relay(typ).to_string(),
            mode: e
                .get("mode")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            sha: e
                .get("oid")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    out.sort_by(|a, b| match (a.kind.as_str(), b.kind.as_str()) {
        ("tree", "blob") => std::cmp::Ordering::Less,
        ("blob", "tree") => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    Ok(out)
}

fn content_type_is_tree(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.to_ascii_lowercase().contains(TREE_MEDIA))
        .unwrap_or(false)
}

async fn https_list_tree(
    host: &str,
    repo: &str,
    git_ref: &str,
    prefix: &str,
) -> Result<Vec<TreeEntry>> {
    let url = https_repo_url(host, repo, prefix)?;
    let resp = https_client()?
        .get(&url)
        .header(HEADER_TREE, "1")
        .header(HEADER_BRANCH, git_ref)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("https tree HTTP {}", resp.status()));
    }
    if !content_type_is_tree(&resp) {
        return Err(anyhow!("https tree: unexpected content-type"));
    }
    let v: serde_json::Value = resp.json().await.context("tree json")?;
    parse_relay_tree_v1(&v)
}

async fn https_show_file(host: &str, repo: &str, git_ref: &str, path: &str) -> Result<FileBody> {
    let url = https_repo_url(host, repo, path)?;
    let resp = https_client()?
        .get(&url)
        .header(HEADER_BRANCH, git_ref)
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("https file HTTP {}", resp.status()));
    }
    if content_type_is_tree(&resp) {
        return Err(anyhow!("path is a directory"));
    }
    if let Some(len) = resp.content_length() {
        if len > 2_000_000 {
            return Ok(FileBody {
                path: path.to_string(),
                binary: true,
                truncated: true,
                text: format!("({len} byte blob; skipped large HTTPS body)"),
            });
        }
    }
    let raw = resp.bytes().await.context("file body")?.to_vec();
    file_body_from_bytes(path.to_string(), raw)
}

fn file_body_from_bytes(path: String, raw: Vec<u8>) -> Result<FileBody> {
    let binary = raw.contains(&0);
    const MAX: usize = 200_000;
    let truncated = raw.len() > MAX;
    let slice = if truncated { &raw[..MAX] } else { &raw[..] };
    let text = if binary {
        format!("({} byte binary blob)", raw.len())
    } else {
        String::from_utf8_lossy(slice).into_owned()
    };
    Ok(FileBody {
        path,
        binary,
        truncated,
        text,
    })
}

pub async fn list_heads(host: &str, port: u16, repo: &str) -> Result<Vec<BranchRef>> {
    let url = git_url(host, port, repo)?;
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--heads", &url]);
    let out = timeout(Duration::from_secs(12), cmd.output())
        .await
        .context("ls-remote timeout")?
        .context("ls-remote")?;
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(parse_ls_remote_heads(&String::from_utf8_lossy(&out.stdout)))
}

fn cache_dir(host: &str, repo: &str) -> Result<PathBuf> {
    let host = ident(host, "fqdn")?;
    let repo = ident(repo, "repo")?;
    let base = std::env::temp_dir().join("relay-ops-git");
    Ok(base.join(host).join(format!("{repo}.git")))
}

fn git_dir_cmd(dir: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.arg("--git-dir").arg(dir);
    c
}

fn init_bare(dir: &Path) -> Result<()> {
    if dir.join("HEAD").is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let st = std::process::Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(dir)
        .status()
        .context("git init")?;
    if !st.success() {
        return Err(anyhow!("git init failed"));
    }
    Ok(())
}

/// Trees-only partial clone so listing a multi-gig bare does not download blobs.
fn configure_partial_clone(dir: &Path, url: &str) -> Result<()> {
    for (k, v) in [
        ("core.repositoryFormatVersion", "1"),
        ("extensions.partialClone", "origin"),
        ("remote.origin.url", url),
        ("remote.origin.promisor", "true"),
        ("remote.origin.partialclonefilter", "blob:none"),
    ] {
        let st = git_dir_cmd(dir)
            .args(["config", k, v])
            .status()
            .with_context(|| format!("git config {k}"))?;
        if !st.success() {
            return Err(anyhow!("git config {k} failed"));
        }
    }
    Ok(())
}

fn local_ref_sha(dir: &Path, git_ref: &str) -> Option<String> {
    let spec = if git_ref.chars().all(|c| c.is_ascii_hexdigit()) && git_ref.len() >= 7 {
        git_ref.to_string()
    } else {
        format!("refs/heads/{git_ref}")
    };
    let out = git_dir_cmd(dir)
        .args(["rev-parse", "--verify", "--quiet", &spec])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.len() >= 7 {
        Some(s)
    } else {
        None
    }
}

async fn remote_ref_sha(url: &str, git_ref: &str) -> Result<Option<String>> {
    let spec = if git_ref.chars().all(|c| c.is_ascii_hexdigit()) && git_ref.len() >= 7 {
        git_ref.to_string()
    } else {
        format!("refs/heads/{git_ref}")
    };
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", url, &spec]);
    let out = timeout(Duration::from_secs(12), cmd.output())
        .await
        .context("ls-remote timeout")?
        .context("ls-remote")?;
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let sha = line
        .split_whitespace()
        .next()
        .filter(|s| s.len() >= 7)
        .map(|s| s.to_string());
    Ok(sha)
}

async fn fetch_trees(dir: &Path, url: &str, git_ref: &str) -> Result<()> {
    let spec = if git_ref.chars().all(|c| c.is_ascii_hexdigit()) && git_ref.len() >= 7 {
        format!("{git_ref}:refs/heads/{git_ref}")
    } else {
        format!("+refs/heads/{git_ref}:refs/heads/{git_ref}")
    };
    let mut fetch = Command::new("git");
    fetch.args([
        "--git-dir",
        dir.to_str().unwrap_or("."),
        "fetch",
        "--depth=1",
        "--filter=blob:none",
        "--no-tags",
        "--prune",
        url,
        &spec,
    ]);
    let out = timeout(Duration::from_secs(20), fetch.output())
        .await
        .context("fetch timeout")?
        .context("fetch")?;
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

async fn ensure_fetched(host: &str, port: u16, repo: &str, git_ref: &str) -> Result<PathBuf> {
    let url = git_url(host, port, repo)?;
    let git_ref = sanitize_ref(git_ref)?;
    let dir = cache_dir(host, repo)?;
    let lock = repo_mutex(&format!("{host}/{repo}"));
    let _g = lock.lock().await;
    init_bare(&dir)?;
    configure_partial_clone(&dir, &url)?;
    let local = local_ref_sha(&dir, &git_ref);
    let remote = remote_ref_sha(&url, &git_ref).await?;
    match (local.as_deref(), remote.as_deref()) {
        (Some(l), Some(r)) if l == r || l.starts_with(r) || r.starts_with(l) => {
            return Ok(dir);
        }
        _ => {}
    }
    fetch_trees(&dir, &url, &git_ref).await?;
    Ok(dir)
}

pub async fn list_tree(
    host: &str,
    port: u16,
    repo: &str,
    git_ref: &str,
    prefix: &str,
) -> Result<Vec<TreeEntry>> {
    let prefix = sanitize_path(prefix)?;
    let git_ref = sanitize_ref(git_ref)?;
    match https_list_tree(host, repo, &git_ref, &prefix).await {
        Ok(v) => return Ok(v),
        Err(e) => tracing::debug!(%e, host, repo, "https X-Relay-Tree failed; git:// fallback"),
    }
    let dir = ensure_fetched(host, port, repo, &git_ref).await?;
    let treeish = if prefix.is_empty() {
        git_ref.clone()
    } else {
        format!("{git_ref}:{prefix}")
    };
    git_ls_tree(&dir, &treeish).await
}

async fn git_ls_tree(dir: &Path, treeish: &str) -> Result<Vec<TreeEntry>> {
    let mut cmd = Command::new("git");
    cmd.args(["--git-dir", dir.to_str().unwrap_or("."), "ls-tree", treeish]);
    let out = timeout(Duration::from_secs(12), cmd.output())
        .await
        .context("ls-tree timeout")?
        .context("ls-tree")?;
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(parse_ls_tree(&String::from_utf8_lossy(&out.stdout)))
}

pub async fn show_file(
    host: &str,
    port: u16,
    repo: &str,
    git_ref: &str,
    path: &str,
) -> Result<FileBody> {
    let path = sanitize_path(path)?;
    if path.is_empty() {
        return Err(anyhow!("path required"));
    }
    let git_ref = sanitize_ref(git_ref)?;
    match https_show_file(host, repo, &git_ref, &path).await {
        Ok(v) => return Ok(v),
        Err(e) => tracing::debug!(%e, host, repo, path, "https blob GET failed; git:// fallback"),
    }
    let dir = ensure_fetched(host, port, repo, &git_ref).await?;
    let spec = format!("{git_ref}:{path}");
    let mut cmd = Command::new("git");
    cmd.args(["--git-dir", dir.to_str().unwrap_or("."), "show", &spec]);
    let out = timeout(Duration::from_secs(20), cmd.output())
        .await
        .context("show timeout")?
        .context("git show")?;
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    file_body_from_bytes(path, out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_heads() {
        let t = "abc1234\trefs/heads/main\ndef5678\trefs/heads/release\n";
        let v = parse_ls_remote_heads(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "main");
        assert_eq!(v[1].name, "release");
    }

    #[test]
    fn parse_tree_sorts_dirs_first() {
        let t = "100644 blob deadbeef\tREADME.md\n040000 tree cafebabe\tcontent\n";
        let v = parse_ls_tree(t);
        assert_eq!(v[0].kind, "tree");
        assert_eq!(v[0].name, "content");
        assert_eq!(v[1].name, "README.md");
    }

    #[test]
    fn parse_relay_tree_maps_dir_file_and_sorts() {
        let v = serde_json::json!({
            "schema": "relay.tree.v1",
            "entries": [
                {"name": "README.md", "type": "file", "mode": "100644", "oid": "aa"},
                {"name": "blog", "type": "dir", "mode": "040000", "oid": "bb"}
            ]
        });
        let rows = parse_relay_tree_v1(&v).unwrap();
        assert_eq!(rows[0].kind, "tree");
        assert_eq!(rows[0].name, "blog");
        assert_eq!(rows[1].kind, "blob");
        assert_eq!(rows[1].sha, "aa");
    }

    #[test]
    fn parse_relay_tree_rejects_plain_json() {
        let v = serde_json::json!({"README.md": {"type": "file"}});
        assert!(parse_relay_tree_v1(&v).is_err());
    }

    #[test]
    fn rejects_path_escape() {
        assert!(sanitize_path("../etc/passwd").is_err());
        assert!(sanitize_path("ok/file.md").is_ok());
        assert!(ident("evil;host", "fqdn").is_err());
    }

    #[test]
    fn local_ref_sha_reads_bare_head() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("w");
        std::fs::create_dir(&work).unwrap();
        let env = [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ];
        let run = |args: &[&str]| {
            let mut c = std::process::Command::new("git");
            c.args(args);
            for (k, v) in &env {
                c.env(k, v);
            }
            assert!(c.status().unwrap().success(), "{args:?}");
        };
        run(&["init", "-q", "-b", "main", work.to_str().unwrap()]);
        std::fs::write(work.join("f"), b"hi").unwrap();
        run(&["-C", work.to_str().unwrap(), "add", "f"]);
        run(&["-C", work.to_str().unwrap(), "commit", "-qm", "n"]);
        let git = dir.path().join("t.git");
        run(&[
            "clone",
            "-q",
            "--bare",
            work.to_str().unwrap(),
            git.to_str().unwrap(),
        ]);
        let sha = local_ref_sha(&git, "main").expect("sha");
        assert!(sha.len() >= 7);
    }
}

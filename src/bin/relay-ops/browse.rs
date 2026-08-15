//! On-demand git:// browse for the local fleet UI (observer; no SSH).

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

fn cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

pub async fn list_heads(host: &str, port: u16, repo: &str) -> Result<Vec<BranchRef>> {
    let url = git_url(host, port, repo)?;
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--heads", &url]);
    let out = timeout(Duration::from_secs(12), cmd.output())
        .await
        .context("ls-remote timeout")?
        .context("ls-remote")?;
    if !out.status.success() {
        return Err(anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_ls_remote_heads(&String::from_utf8_lossy(&out.stdout)))
}

fn cache_dir(host: &str, repo: &str) -> Result<PathBuf> {
    let host = ident(host, "fqdn")?;
    let repo = ident(repo, "repo")?;
    let base = std::env::temp_dir().join("relay-ops-git");
    Ok(base.join(host).join(format!("{repo}.git")))
}

async fn ensure_fetched(host: &str, port: u16, repo: &str, git_ref: &str) -> Result<PathBuf> {
    let url = git_url(host, port, repo)?;
    let git_ref = sanitize_ref(git_ref)?;
    let dir = cache_dir(host, repo)?;
    let _g = cache_lock().lock().await;
    if !dir.join("HEAD").is_file() {
        std::fs::create_dir_all(&dir)?;
        let st = std::process::Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&dir)
            .status()
            .context("git init")?;
        if !st.success() {
            return Err(anyhow!("git init failed"));
        }
    }
    let spec = format!("+refs/heads/{git_ref}:refs/heads/{git_ref}");
    let mut fetch = Command::new("git");
    fetch
        .args([
            "--git-dir",
            dir.to_str().unwrap_or("."),
            "fetch",
            "--depth=1",
            "--no-tags",
            "--prune",
            &url,
            &spec,
        ]);
    let out = timeout(Duration::from_secs(25), fetch.output())
        .await
        .context("fetch timeout")?
        .context("fetch")?;
    if !out.status.success() {
        return Err(anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
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
    cmd.args([
        "--git-dir",
        dir.to_str().unwrap_or("."),
        "ls-tree",
        treeish,
    ]);
    let out = timeout(Duration::from_secs(12), cmd.output())
        .await
        .context("ls-tree timeout")?
        .context("ls-tree")?;
    if !out.status.success() {
        return Err(anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
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
    let dir = ensure_fetched(host, port, repo, &git_ref).await?;
    let spec = format!("{git_ref}:{path}");
    let mut cmd = Command::new("git");
    cmd.args(["--git-dir", dir.to_str().unwrap_or("."), "show", &spec]);
    let out = timeout(Duration::from_secs(12), cmd.output())
        .await
        .context("show timeout")?
        .context("git show")?;
    if !out.status.success() {
        return Err(anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let raw = out.stdout;
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
    fn rejects_path_escape() {
        assert!(sanitize_path("../etc/passwd").is_err());
        assert!(sanitize_path("ok/file.md").is_ok());
        assert!(ident("evil;host", "fqdn").is_err());
    }
}

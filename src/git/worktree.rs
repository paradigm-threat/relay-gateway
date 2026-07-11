//! Live checkout reads for desktop local mode (uncommitted + gitignored files).

use axum::response::IntoResponse;
use std::path::Path;

/// Current branch name for an open checkout (`HEAD` symbolic ref).
pub fn checked_out_branch(checkout: &Path) -> Option<String> {
    let repo = git2::Repository::open(checkout).ok()?;
    let head = repo.head().ok()?;
    if head.is_branch() {
        return head.shorthand().map(str::to_string);
    }
    head.symbolic_target()
        .and_then(|t| t.strip_prefix("refs/heads/"))
        .map(str::to_string)
}

/// Read a repo-relative file from the live working tree (includes gitignored paths).
pub fn read_worktree_file(checkout: &Path, rel: &str) -> Option<Vec<u8>> {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() || path_traversal_attempt(rel) {
        return None;
    }
    let path = checkout.join(rel);
    if !path.is_file() {
        return None;
    }
    let root_canon = checkout.canonicalize().ok()?;
    let file_canon = path.canonicalize().ok()?;
    if !file_canon.starts_with(&root_canon) {
        return None;
    }
    std::fs::read(&path).ok()
}

fn path_traversal_attempt(rel: &str) -> bool {
    if rel.contains("..") || rel.contains('\\') {
        return true;
    }
    let lower = rel.to_ascii_lowercase();
    lower.contains("%2e%2e") || lower.contains("%2f%2e%2e")
}

pub fn worktree_file_response(
    branch: &str,
    repo_name: &str,
    rel: &str,
    bytes: Vec<u8>,
) -> axum::response::Response {
    let ct = mime_guess::from_path(rel)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    (
        axum::http::StatusCode::OK,
        [
            ("Content-Type", ct),
            (crate::types::HEADER_BRANCH, branch.to_string()),
            (crate::types::HEADER_REPO, repo_name.to_string()),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn read_worktree_includes_gitignored() {
        let dir = tempdir().unwrap();
        let checkout = dir.path();
        let gitignore = checkout.join(".gitignore");
        std::fs::write(&gitignore, "ignored.txt\n").unwrap();
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(checkout)
            .output()
            .expect("git init");
        let ignored = checkout.join("ignored.txt");
        std::fs::write(&ignored, b"secret").unwrap();
        let bytes = read_worktree_file(checkout, "ignored.txt").expect("ignored file");
        assert_eq!(bytes, b"secret");
    }

    #[test]
    fn read_worktree_blocks_traversal() {
        let dir = tempdir().unwrap();
        assert!(read_worktree_file(dir.path(), "../etc/passwd").is_none());
    }

    #[test]
    fn checked_out_branch_reads_head() {
        let dir = tempdir().unwrap();
        let checkout = dir.path();
        std::process::Command::new("git")
            .args(["init", "-b", "develop"])
            .current_dir(checkout)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(checkout)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("commit");
        assert_eq!(checked_out_branch(checkout).as_deref(), Some("develop"));
    }
}

//! Durable post-push mesh notify outcomes + fleet tip comparison for operators/clients.
//!
//! State: `{RELAY_REPO_PATH}/../state/post_push_sync/{repo}.json`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::mesh_role;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerNotifyOutcome {
    pub target: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PostPushSyncRecord {
    pub repo: String,
    pub branch: String,
    pub pushed_sha: String,
    pub publisher_fqdn: String,
    pub finished_at_unix: u64,
    pub notify_ok_count: u32,
    pub notify_fail_count: u32,
    pub peers: Vec<PeerNotifyOutcome>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeshSyncStatus {
    Success,
    Partial,
    Fail,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerTipRow {
    pub host: String,
    pub sha: Option<String>,
    pub match_publisher: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeshSyncReport {
    pub repo: String,
    pub branch: String,
    pub publisher_fqdn: String,
    pub publisher_sha: Option<String>,
    pub expected_sha: String,
    pub status: MeshSyncStatus,
    pub peers: Vec<PeerTipRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_notify: Option<PostPushSyncRecord>,
}

pub fn state_dir(repo_path: &Path) -> PathBuf {
    repo_path
        .parent()
        .map(|p| p.join("state").join("post_push_sync"))
        .unwrap_or_else(|| PathBuf::from("state/post_push_sync"))
}

fn record_path(repo_path: &Path, repo: &str) -> PathBuf {
    state_dir(repo_path).join(format!("{repo}.json"))
}

pub fn write_post_push_sync(repo_path: &Path, record: &PostPushSyncRecord) {
    let path = record_path(repo_path, &record.repo);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(record) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub fn read_post_push_sync(repo_path: &Path, repo: &str) -> Option<PostPushSyncRecord> {
    let s = std::fs::read_to_string(record_path(repo_path, repo)).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn mesh_status_from_peers(
    publisher_sha: Option<&str>,
    expected_sha: &str,
    rows: &[PeerTipRow],
    notify: Option<&PostPushSyncRecord>,
) -> MeshSyncStatus {
    let pub_matches = publisher_sha.is_some_and(|s| s == expected_sha);
    if !pub_matches {
        return MeshSyncStatus::Fail;
    }
    if rows.is_empty() {
        if let Some(n) = notify {
            if n.notify_fail_count > 0 && n.notify_ok_count == 0 {
                return MeshSyncStatus::Fail;
            }
            if n.notify_fail_count > 0 {
                return MeshSyncStatus::Partial;
            }
        }
        return MeshSyncStatus::Success;
    }
    let all_match = rows.iter().all(|r| r.match_publisher);
    if all_match {
        return MeshSyncStatus::Success;
    }
    if pub_matches {
        return MeshSyncStatus::Partial;
    }
    let any_match = rows.iter().any(|r| r.match_publisher);
    if any_match {
        return MeshSyncStatus::Partial;
    }
    if let Some(n) = notify {
        if n.notify_fail_count > 0 {
            return MeshSyncStatus::Partial;
        }
    }
    MeshSyncStatus::Pending
}

pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Outcome of blocking peer tip wait (update hook / operators).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshSyncWaitResult {
    pub status: MeshSyncStatus,
    pub peers: Vec<PeerTipRow>,
    pub timed_out: bool,
}

pub fn master_peer_hosts() -> Vec<String> {
    let self_host = mesh_role::node_fqdn_normalized();
    std::env::var("RELAY_MASTER_PEER_LIST")
        .unwrap_or_default()
        .split(';')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .filter(|p| {
            self_host
                .as_ref()
                .map(|s| !p.eq_ignore_ascii_case(s))
                .unwrap_or(true)
        })
        .collect()
}

pub fn git_port() -> u16 {
    std::env::var("RELAY_GIT_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(9418)
}

pub fn mesh_sync_wait_secs() -> u64 {
    std::env::var("RELAY_MESH_SYNC_WAIT_SECS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(45)
}

pub fn mesh_sync_poll_ms() -> u64 {
    std::env::var("RELAY_MESH_SYNC_POLL_MS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1000)
}

pub fn ls_remote_branch(host: &str, port: u16, repo: &str, branch: &str) -> Result<String, String> {
    let url = format!("git://{host}:{port}/{repo}.git");
    let out = Command::new("git")
        .args(["ls-remote", &url, &format!("refs/heads/{branch}")])
        .output()
        .map_err(|e| format!("git ls-remote: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let sha = line
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("")
        .to_string();
    if sha.len() != 40 {
        return Err("no ref on peer".into());
    }
    Ok(sha)
}

pub fn probe_peer_tips(repo: &str, branch: &str, expected_sha: &str) -> Vec<PeerTipRow> {
    let port = git_port();
    master_peer_hosts()
        .into_iter()
        .map(|host| match ls_remote_branch(&host, port, repo, branch) {
            Ok(sha) => PeerTipRow {
                host,
                sha: Some(sha.clone()),
                match_publisher: sha == expected_sha,
                error: None,
            },
            Err(e) => PeerTipRow {
                host,
                sha: None,
                match_publisher: false,
                error: Some(e),
            },
        })
        .collect()
}

/// Poll fleet peers until all match `expected_sha` or the wait budget expires.
pub fn wait_peers_match_tip(repo: &str, branch: &str, expected_sha: &str) -> MeshSyncWaitResult {
    wait_peers_match_tip_with_probe(repo, branch, expected_sha, probe_peer_tips)
}

pub fn wait_peers_match_tip_with_probe(
    repo: &str,
    branch: &str,
    expected_sha: &str,
    mut probe: impl FnMut(&str, &str, &str) -> Vec<PeerTipRow>,
) -> MeshSyncWaitResult {
    let deadline = Instant::now() + Duration::from_secs(mesh_sync_wait_secs());
    let poll = Duration::from_millis(mesh_sync_poll_ms());
    let mut timed_out = false;
    let mut last_rows = Vec::new();
    let mut last_status = MeshSyncStatus::Pending;

    loop {
        last_rows = probe(repo, branch, expected_sha);
        last_status = mesh_status_from_peers(Some(expected_sha), expected_sha, &last_rows, None);
        if last_status == MeshSyncStatus::Success {
            return MeshSyncWaitResult {
                status: last_status,
                peers: last_rows,
                timed_out: false,
            };
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        thread::sleep(poll);
    }

    MeshSyncWaitResult {
        status: if timed_out && last_status == MeshSyncStatus::Pending {
            MeshSyncStatus::Partial
        } else {
            last_status
        },
        peers: last_rows,
        timed_out,
    }
}

pub fn git_update_ref(
    repo_path: &Path,
    refname: &str,
    new_sha: &str,
    old_sha: &str,
) -> Result<(), String> {
    let repo = repo_path.to_str().ok_or("invalid repo path")?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).arg("update-ref").arg(refname).arg(new_sha);
    if old_sha != ZERO_OID {
        cmd.arg(old_sha);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("git update-ref: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn git_revert_ref(
    repo_path: &Path,
    refname: &str,
    old_sha: &str,
    new_sha: &str,
) -> Result<(), String> {
    let repo = repo_path.to_str().ok_or("invalid repo path")?;
    if old_sha == ZERO_OID {
        let out = Command::new("git")
            .args(["-C", repo, "update-ref", "-d", refname])
            .output()
            .map_err(|e| format!("git update-ref -d: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    } else {
        git_update_ref(repo_path, refname, old_sha, new_sha)
    }
}

pub fn format_mesh_sync_failure(result: &MeshSyncWaitResult) -> String {
    let reason = if result.timed_out {
        "timed out waiting for mesh peers"
    } else {
        "mesh peers did not match publish tip"
    };
    let mut parts = vec![reason.to_string()];
    for p in &result.peers {
        if p.match_publisher {
            continue;
        }
        let detail = match (&p.sha, &p.error) {
            (_, Some(e)) if !e.is_empty() => e.clone(),
            (Some(s), _) => format!("tip {s}"),
            _ => "unreachable".into(),
        };
        parts.push(format!("{}: {detail}", p.host));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_peers_match_is_success() {
        let rows = vec![
            PeerTipRow {
                host: "hkg1.example".into(),
                sha: Some("abc".into()),
                match_publisher: true,
                error: None,
            },
            PeerTipRow {
                host: "ord1.example".into(),
                sha: Some("abc".into()),
                match_publisher: true,
                error: None,
            },
        ];
        assert_eq!(
            mesh_status_from_peers(Some("abc"), "abc", &rows, None),
            MeshSyncStatus::Success
        );
    }

    #[test]
    fn lagging_peer_is_partial() {
        let rows = vec![PeerTipRow {
            host: "hkg1.example".into(),
            sha: Some("old".into()),
            match_publisher: false,
            error: None,
        }];
        assert_eq!(
            mesh_status_from_peers(Some("new"), "new", &rows, None),
            MeshSyncStatus::Partial
        );
    }

    #[test]
    fn wait_succeeds_when_peers_match_immediately() {
        let result = wait_peers_match_tip_with_probe(
            "demo",
            "main",
            "abcabcabcabcabcabcabcabcabcabcabcabcabca",
            |_, _, expected| {
                vec![PeerTipRow {
                    host: "hkg1.example".into(),
                    sha: Some(expected.to_string()),
                    match_publisher: true,
                    error: None,
                }]
            },
        );
        assert_eq!(result.status, MeshSyncStatus::Success);
        assert!(!result.timed_out);
    }

    #[test]
    fn wait_times_out_when_peer_never_catches_up() {
        std::env::set_var("RELAY_MESH_SYNC_WAIT_SECS", "1");
        std::env::set_var("RELAY_MESH_SYNC_POLL_MS", "100");
        let result = wait_peers_match_tip_with_probe(
            "demo",
            "main",
            "abcabcabcabcabcabcabcabcabcabcabcabcabca",
            |_, _, _| {
                vec![PeerTipRow {
                    host: "hkg1.example".into(),
                    sha: Some("oldoldoldoldoldoldoldoldoldoldoldoldoldold".into()),
                    match_publisher: false,
                    error: None,
                }]
            },
        );
        assert!(result.timed_out);
        assert_ne!(result.status, MeshSyncStatus::Success);
    }

    #[test]
    fn format_failure_includes_lagging_peer() {
        let msg = format_mesh_sync_failure(&MeshSyncWaitResult {
            status: MeshSyncStatus::Partial,
            peers: vec![PeerTipRow {
                host: "hkg1.example".into(),
                sha: Some("old".into()),
                match_publisher: false,
                error: None,
            }],
            timed_out: true,
        });
        assert!(msg.contains("timed out"));
        assert!(msg.contains("hkg1.example"));
    }
}

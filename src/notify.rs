//! Optional Slack notifications (Incoming Webhook) for relay lifecycle and Git activity.
//!
//! **Commit-update dedup:** Incoming webhooks cannot read channel history. For cross-server
//! deduplication (same repo + SHA already announced in Slack), set **`RELAY_SLACK_BOT_TOKEN`**
//! (Bot User OAuth, `xoxb-…`) and **`RELAY_SLACK_DEDUP_CHANNEL_ID`** (`C…` for the same channel
//! the webhook posts to). The bot needs **`channels:history`** (and typically **`channels:read`**).
//! Without a bot token, only an **in-process** TTL cache is used (no cross-node dedup).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

/// Crate version (shown in `server_started` Slack lines).
pub const RELAY_VERSION: &str = env!("CARGO_PKG_VERSION");

const DEDUP_TTL: Duration = Duration::from_secs(900);

fn local_dedup_map() -> &'static Mutex<HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn scrub_local_dedup() {
    let now = Instant::now();
    if let Ok(mut m) = local_dedup_map().lock() {
        m.retain(|_, t| now.duration_since(*t) < DEDUP_TTL);
    }
}

/// Public marker line embedded in Slack text so **`conversations.history`** can find duplicates.
pub fn commit_dedup_marker(repo: &str, sha: &str) -> String {
    format!(
        "relay_dedup kind=commit_update repo={} sha={}",
        normalize_repo_slug(repo),
        sha.trim().to_ascii_lowercase()
    )
}

pub fn normalize_repo_slug(repo: &str) -> String {
    let r = repo.trim();
    r.strip_suffix(".git").unwrap_or(r).to_string()
}

fn dedup_cache_key(repo: &str, sha: &str) -> String {
    format!(
        "{}:{}",
        normalize_repo_slug(repo),
        sha.trim().to_ascii_lowercase()
    )
}

fn object_sha_ok(sha: &str) -> bool {
    let t = sha.trim();
    t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit()) && !t.chars().all(|c| c == '0')
}

/// Incoming Webhook URL (same as mesh Slack path).
pub fn slack_webhook_url() -> Option<String> {
    std::env::var("RELAY_NOTIFY_SLACK_WEBHOOK_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn slack_bot_token() -> Option<String> {
    std::env::var("RELAY_SLACK_BOT_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn slack_dedup_channel_id() -> Option<String> {
    std::env::var("RELAY_SLACK_DEDUP_CHANNEL_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn slack_history_has_commit_marker_blocking(channel_id: &str, repo: &str, sha: &str) -> bool {
    let Some(token) = slack_bot_token() else {
        return false;
    };
    let marker = commit_dedup_marker(repo, sha);
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(err=%e, "Slack dedup: client build failed");
            return false;
        }
    };
    let url = format!(
        "https://slack.com/api/conversations.history?channel={}&limit=75",
        channel_id
    );
    let resp = match client.get(&url).bearer_auth(&token).send() {
        Ok(r) => r,
        Err(e) => {
            warn!(err=%e, "Slack dedup: conversations.history request failed");
            return false;
        }
    };
    let v: serde_json::Value = match resp.json() {
        Ok(j) => j,
        Err(e) => {
            warn!(err=%e, "Slack dedup: invalid JSON");
            return false;
        }
    };
    if !v["ok"].as_bool().unwrap_or(false) {
        warn!(
            slack_error = %v["error"].as_str().unwrap_or("unknown"),
            "Slack dedup: conversations.history not ok"
        );
        return false;
    }
    let Some(msgs) = v["messages"].as_array() else {
        return false;
    };
    for msg in msgs {
        if let Some(t) = msg["text"].as_str() {
            if t.contains(&marker) {
                return true;
            }
        }
    }
    false
}

/// `true` = do **not** post (duplicate); also records local cache when Slack says duplicate.
pub fn commit_update_is_duplicate(repo: &str, sha: &str) -> bool {
    if !object_sha_ok(sha) {
        return true;
    }
    let repo_n = normalize_repo_slug(repo);
    let sha_l = sha.trim().to_ascii_lowercase();
    scrub_local_dedup();
    let key = dedup_cache_key(&repo_n, &sha_l);
    {
        let m = local_dedup_map().lock().unwrap();
        if let Some(t) = m.get(&key) {
            if Instant::now().duration_since(*t) < DEDUP_TTL {
                return true;
            }
        }
    }
    if let Some(ch) = slack_dedup_channel_id() {
        if slack_history_has_commit_marker_blocking(&ch, &repo_n, &sha_l) {
            local_dedup_map()
                .lock()
                .unwrap()
                .insert(key, Instant::now());
            debug!(repo=%repo_n, %sha_l, "Slack dedup: marker found in channel history");
            return true;
        }
    }
    false
}

pub fn mark_commit_update_seen(repo: &str, sha: &str) {
    if !object_sha_ok(sha) {
        return;
    }
    let key = dedup_cache_key(repo, sha);
    local_dedup_map()
        .lock()
        .unwrap()
        .insert(key, Instant::now());
}

fn format_commit_update_text(
    source: &str,
    repo: &str,
    branch: &str,
    sha: &str,
    relay_server_id: Option<&str>,
    node_fqdn: Option<&str>,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("[Relay] commit_update".into());
    lines.push(format!("source: {}", source));
    lines.push(format!(
        "repo: {}  branch: {}",
        normalize_repo_slug(repo),
        branch
    ));
    if let Some(id) = relay_server_id.filter(|s| !s.is_empty()) {
        lines.push(format!("relay_server_id: {}", id));
    }
    if let Some(n) = node_fqdn.filter(|s| !s.is_empty()) {
        lines.push(format!("node: {}", n));
    }
    lines.push(format!("sha: {}", sha.trim().to_ascii_lowercase()));
    lines.push(commit_dedup_marker(repo, sha));
    lines.join("\n")
}

pub async fn post_slack_webhook(
    client: &reqwest::Client,
    url: &str,
    text: &str,
) -> Result<(), String> {
    let body = serde_json::json!({ "text": text });
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        Err(format!("HTTP {status}: {err_body}"))
    }
}

pub fn post_slack_webhook_blocking(url: &str, text: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({ "text": text });
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let err_body = resp.text().unwrap_or_default();
        Err(format!("HTTP {status}: {err_body}"))
    }
}

/// Fire-and-forget Slack line (no dedup): startup, panic, generic errors, validation failures.
pub fn spawn_slack_text(text: String) {
    let Some(url) = slack_webhook_url() else {
        return;
    };
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(err=%e, "notify: reqwest client build failed");
                return;
            }
        };
        if let Err(e) = post_slack_webhook(&client, &url, &text).await {
            warn!(err=%e, "Slack notify failed");
        }
    });
}

pub fn spawn_slack_text_blocking(text: String) {
    let Some(url) = slack_webhook_url() else {
        return;
    };
    if let Err(e) = post_slack_webhook_blocking(&url, &text) {
        warn!(err=%e, "Slack notify (blocking) failed");
    }
}

/// `git_push` | `git_pull` | `peer_fetch`
pub fn spawn_commit_update_notify(
    source: &'static str,
    repo: String,
    branch: String,
    sha: String,
    relay_server_id: Option<String>,
    node_fqdn: Option<String>,
) {
    let Some(url) = slack_webhook_url() else {
        return;
    };
    if !object_sha_ok(&sha) {
        return;
    }
    tokio::spawn(async move {
        let repo_dedup = repo.clone();
        let sha_dedup = sha.clone();
        let dup = tokio::task::spawn_blocking(move || {
            commit_update_is_duplicate(&repo_dedup, &sha_dedup)
        })
        .await
        .unwrap_or(true);
        if dup {
            debug!(repo=%repo, sha=%sha, "Slack commit_update skipped (dedup)");
            return;
        }
        let text = format_commit_update_text(
            source,
            &repo,
            &branch,
            &sha,
            relay_server_id.as_deref(),
            node_fqdn.as_deref(),
        );
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(err=%e, "notify: reqwest client build failed");
                return;
            }
        };
        if post_slack_webhook(&client, &url, &text).await.is_ok() {
            mark_commit_update_seen(&repo, &sha);
        }
    });
}

/// For **`relay-hook-handler`** (no async runtime).
pub fn commit_update_notify_blocking(
    source: &'static str,
    repo: &str,
    branch: &str,
    sha: &str,
    relay_server_id: Option<&str>,
    node_fqdn: Option<&str>,
) {
    let Some(url) = slack_webhook_url() else {
        return;
    };
    if !object_sha_ok(sha) {
        return;
    }
    if commit_update_is_duplicate(repo, sha) {
        debug!(repo, sha, "Slack commit_update skipped (dedup)");
        return;
    }
    let text = format_commit_update_text(source, repo, branch, sha, relay_server_id, node_fqdn);
    if post_slack_webhook_blocking(&url, &text).is_ok() {
        mark_commit_update_seen(repo, sha);
    }
}

pub fn notify_server_started(node_fqdn: Option<&str>, relay_server_id: Option<&str>) {
    let mut lines = vec![
        "[Relay] server_started".to_string(),
        format!("version: {}", RELAY_VERSION),
    ];
    if let Some(n) = node_fqdn.filter(|s| !s.is_empty()) {
        lines.push(format!("node: {}", n));
    }
    if let Some(id) = relay_server_id.filter(|s| !s.is_empty()) {
        lines.push(format!("relay_server_id: {}", id));
    }
    spawn_slack_text(lines.join("\n"));
}

pub fn notify_hook_error(context: &str, message: &str) {
    let msg: String = message.chars().take(1500).collect();
    let text = format!("[Relay] hook_error\ncontext: {}\n{}", context, msg);
    spawn_slack_text_blocking(text);
}

/// Structured Slack line for `pre-receive` rejections (branch policy, missing `.relay.yaml`, etc.).
pub fn notify_pre_receive_rejection(
    repo_slug: &str,
    branch: &str,
    refname: &str,
    old_oid: &str,
    new_oid: &str,
    message: &str,
) {
    let msg: String = message.chars().take(1500).collect();
    let text = format!(
        "[Relay] pre_receive_rejected\nrepo: {}\nbranch: {}\nref: {}\nold: {}\nnew: {}\n{}",
        repo_slug, branch, refname, old_oid, new_oid, msg
    );
    spawn_slack_text_blocking(text);
}

pub fn notify_auto_push_error(origin: &str, detail: &str) {
    let d: String = detail.chars().take(1200).collect();
    let text = format!("[Relay] auto_push_failed\norigin: {}\n{}", origin, d);
    spawn_slack_text_blocking(text);
}

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("(no message)");
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let text = format!(
            "[Relay] process_panic\nlocation: {}\ndetail: {}",
            loc,
            payload.chars().take(1200).collect::<String>()
        );
        spawn_slack_text_blocking(text);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_marker_stable() {
        assert_eq!(
            commit_dedup_marker("relay-master.git", "AbCdEf0123456789AbCdEf0123456789AbCdEf01"),
            "relay_dedup kind=commit_update repo=relay-master sha=abcdef0123456789abcdef0123456789abcdef01"
        );
    }

    #[test]
    fn object_sha_ok_rejects_zeros() {
        assert!(!object_sha_ok("0000000000000000000000000000000000000000"));
        assert!(object_sha_ok("0000000000000000000000000000000000000001"));
    }
}

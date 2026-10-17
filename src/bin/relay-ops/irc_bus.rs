//! Optional IRC subscriber: join Ergo ops channels, append lines to the brain feed,
//! and forward safe `!status` / `!pause` / `!resume` commands to brain `/chat`.
//!
//! Enable with **`RELAY_OPS_IRC=1`** (or **`RELAY_IRC_BUS=1`**).
//! Connect: **`RELAY_OPS_IRC_HOST`** (default `flashbackrepository.org`) /
//! **`RELAY_OPS_IRC_PORT`** (default `6667`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{info, warn};

pub fn irc_bus_enabled() -> bool {
    env_truthy("RELAY_OPS_IRC") || env_truthy("RELAY_IRC_BUS")
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn irc_host() -> String {
    std::env::var("RELAY_OPS_IRC_HOST")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "flashbackrepository.org".into())
}

fn irc_port() -> u16 {
    std::env::var("RELAY_OPS_IRC_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(6667)
}

fn irc_nick() -> String {
    std::env::var("RELAY_OPS_IRC_NICK")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "RelayOps".into())
}

fn irc_channels() -> Vec<String> {
    let raw = std::env::var("RELAY_OPS_IRC_CHANNELS").unwrap_or_else(|_| "#relay,#logs".into());
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|c| {
            if c.starts_with('#') {
                c
            } else {
                format!("#{c}")
            }
        })
        .collect()
}

/// Spawn reconnecting IRC client. Sets `up` while registered and joined.
pub fn spawn_irc_bus(brain_token: String, brain_base: String, up: Arc<AtomicBool>) {
    if !irc_bus_enabled() {
        return;
    }
    tokio::spawn(async move {
        loop {
            up.store(false, Ordering::Relaxed);
            match run_once(&brain_token, &brain_base, up.clone()).await {
                Ok(()) => warn!("irc_bus: connection ended — reconnecting"),
                Err(e) => warn!("irc_bus: {e:#} — reconnecting"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

async fn run_once(
    brain_token: &str,
    brain_base: &str,
    up: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let host = irc_host();
    let port = irc_port();
    let nick = irc_nick();
    let channels = irc_channels();
    let addr = format!("{host}:{port}");
    info!(%addr, %nick, "irc_bus: connecting");
    let stream = TcpStream::connect(&addr).await?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    writer
        .write_all(format!("NICK {nick}\r\nUSER relayops 0 * :Relay ops brain bridge\r\n").as_bytes())
        .await?;

    let mut registered = false;
    while let Some(line) = lines.next_line().await? {
        if line.starts_with("PING ") {
            let token = line.trim_start_matches("PING ").trim();
            writer
                .write_all(format!("PONG {token}\r\n").as_bytes())
                .await?;
            continue;
        }
        // 001 welcome
        if !registered && line.contains(" 001 ") {
            registered = true;
            for ch in &channels {
                writer
                    .write_all(format!("JOIN {ch}\r\n").as_bytes())
                    .await?;
            }
            up.store(true, Ordering::Relaxed);
            info!(?channels, "irc_bus: joined");
            continue;
        }
        if let Some(ev) = parse_privmsg(&line) {
            // Ignore our own nick
            if ev.nick.eq_ignore_ascii_case(&nick) {
                continue;
            }
            let _ = post_brain_feed(
                brain_base,
                brain_token,
                &ev.channel,
                &ev.nick,
                &ev.text,
            )
            .await;
            if let Some(cmd) = parse_bot_command(&ev.text) {
                let reply = post_brain_chat(brain_base, brain_token, &cmd).await;
                if let Ok(r) = reply {
                    let target = if ev.channel.starts_with('#') {
                        ev.channel.clone()
                    } else {
                        ev.nick.clone()
                    };
                    let msg = sanitize_irc_text(&r);
                    let _ = writer
                        .write_all(format!("PRIVMSG {target} :{msg}\r\n").as_bytes())
                        .await;
                }
            }
        }
    }
    up.store(false, Ordering::Relaxed);
    Ok(())
}

struct Privmsg {
    nick: String,
    channel: String,
    text: String,
}

fn parse_privmsg(line: &str) -> Option<Privmsg> {
    // :nick!user@host PRIVMSG #chan :text
    let rest = line.strip_prefix(':')?;
    let (prefix, cmd) = rest.split_once(' ')?;
    let nick = prefix.split('!').next()?.to_string();
    if !cmd.starts_with("PRIVMSG ") {
        return None;
    }
    let after = cmd.strip_prefix("PRIVMSG ")?;
    let (target, text) = after.split_once(" :")?;
    Some(Privmsg {
        nick,
        channel: target.to_string(),
        text: text.to_string(),
    })
}

/// Map `!status`, `!status iad1`, `!pause`, `!resume` → brain `/chat` keywords.
fn parse_bot_command(text: &str) -> Option<String> {
    let t = text.trim();
    let t = t.strip_prefix('!')?;
    let mut parts = t.split_whitespace();
    let cmd = parts.next()?.to_ascii_lowercase();
    match cmd.as_str() {
        "status" => {
            if let Some(node) = parts.next() {
                Some(format!("status {node}"))
            } else {
                Some("status".into())
            }
        }
        "pause" => Some("pause".into()),
        "resume" => Some("resume".into()),
        "blocked" => Some("blocked".into()),
        _ => None,
    }
}

fn sanitize_irc_text(s: &str) -> String {
    s.replace(['\r', '\n'], " ").chars().take(350).collect()
}

async fn post_brain_feed(
    base: &str,
    token: &str,
    channel: &str,
    nick: &str,
    text: &str,
) -> anyhow::Result<()> {
    // Reuse /chat free-text logging so feed gets an entry via brain.
    let body = serde_json::json!({
        "text": format!("[irc {channel}] <{nick}> {text}")
    });
    let client = reqwest::Client::new();
    let url = format!("{}/chat", base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-relay-brain-token", token)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("brain chat {}", resp.status());
    }
    Ok(())
}

async fn post_brain_chat(base: &str, token: &str, text: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let url = format!("{}/chat", base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-relay-brain-token", token)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("brain chat {status}: {body}");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_privmsg() {
        let p = parse_privmsg(":iad1!u@h PRIVMSG #logs :iad1 | warn | caddy | boom").unwrap();
        assert_eq!(p.nick, "iad1");
        assert_eq!(p.channel, "#logs");
        assert!(p.text.contains("caddy"));
    }

    #[test]
    fn bot_commands() {
        assert_eq!(parse_bot_command("!status").as_deref(), Some("status"));
        assert_eq!(
            parse_bot_command("!status iad1").as_deref(),
            Some("status iad1")
        );
        assert_eq!(parse_bot_command("!pause").as_deref(), Some("pause"));
        assert_eq!(parse_bot_command("hello").as_deref(), None);
    }
}

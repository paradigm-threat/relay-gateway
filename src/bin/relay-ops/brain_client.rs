//! HTTP client for local/remote relay-brain (CLI + ops UI proxy).

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

pub fn brain_url() -> String {
    std::env::var("RELAY_BRAIN_URL").unwrap_or_else(|_| "http://127.0.0.1:7781".into())
}

pub fn brain_token() -> String {
    if let Ok(t) = std::env::var("RELAY_BRAIN_TOKEN") {
        if !t.is_empty() {
            return t;
        }
    }
    let p = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("relay-brain")
        .join("token");
    std::fs::read_to_string(p)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("http client")
}

pub fn get_json(path: &str) -> Result<Value> {
    let url = format!("{}{}", brain_url().trim_end_matches('/'), path);
    let resp = client()?.get(&url).send().context(url)?;
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        return Err(anyhow!("GET {path}: {status} {text}"));
    }
    serde_json::from_str(&text).context("json")
}

pub fn post_auth(path: &str, body: Option<Value>) -> Result<String> {
    let url = format!("{}{}", brain_url().trim_end_matches('/'), path);
    let token = brain_token();
    if token.is_empty() {
        return Err(anyhow!(
            "no RELAY_BRAIN_TOKEN and no ~/.local/state/relay-brain/token"
        ));
    }
    let mut req = client()?
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Relay-Brain-Token", &token);
    req = match body {
        Some(b) => req.json(&b),
        None => req,
    };
    let resp = req.send().context(url)?;
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        return Err(anyhow!("POST {path}: {status} {text}"));
    }
    Ok(text)
}

pub async fn get_json_async(path: &str) -> Result<Value> {
    let url = format!("{}{}", brain_url().trim_end_matches('/'), path);
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .context(url.clone())?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("GET {path}: {status} {text}"));
    }
    Ok(serde_json::from_str(&text)?)
}

pub async fn post_auth_async(path: &str, body: Option<Value>) -> Result<String> {
    let url = format!("{}{}", brain_url().trim_end_matches('/'), path);
    let token = brain_token();
    if token.is_empty() {
        return Err(anyhow!("no brain token"));
    }
    let mut req = reqwest::Client::new()
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Relay-Brain-Token", &token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.context(url)?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("POST {path}: {status} {text}"));
    }
    Ok(text)
}

/// Steps the brain currently authorizes for fqdn (from status nodes / empty).
pub async fn allowlisted_job_kinds(fqdn: &str) -> Option<Vec<String>> {
    let st = get_json_async("/status").await.ok()?;
    let nodes = st.get("nodes")?.as_array()?;
    for n in nodes {
        if n.get("fqdn")?.as_str()? == fqdn {
            let blocked = n.get("blocked").and_then(|v| v.as_bool()).unwrap_or(false);
            let confirm = n
                .get("requires_confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if blocked || confirm {
                // Only approved requests unlock — check pending list resolved elsewhere
                return Some(vec![]);
            }
            let steps = n
                .get("steps")?
                .as_array()?
                .iter()
                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                .collect();
            return Some(steps);
        }
    }
    None
}

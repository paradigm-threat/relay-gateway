use crate::config::{cache_dir, ensure_dir, OpsConfig};
use crate::topology::FleetFile;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct GitlabProject {
    pub api_base: String,
    pub web_base: String,
    pub path: String,
}

impl GitlabProject {
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
        let url = if let Some(rest) = url.strip_prefix("git@") {
            let rest = rest.replacen(':', "/", 1);
            format!("https://{rest}")
        } else if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            format!("https://{url}")
        };
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| anyhow!("GitLab URL missing scheme"))?;
        let (host, path) = rest
            .split_once('/')
            .ok_or_else(|| anyhow!("GitLab URL missing project path"))?;
        let path = path
            .split("/-/")
            .next()
            .unwrap_or(path)
            .trim_end_matches('/')
            .to_string();
        if path.is_empty() {
            return Err(anyhow!("GitLab URL missing project path"));
        }
        let web_base = format!("{scheme}://{host}");
        Ok(GitlabProject {
            api_base: format!("{web_base}/api/v4"),
            web_base,
            path,
        })
    }

    pub fn raw_url(&self, file: &str, git_ref: &str) -> String {
        format!("{}/{}/-/raw/{}/{}", self.web_base, self.path, git_ref, file)
    }
}

pub async fn load_topology(cfg: &OpsConfig, gitlab_url: &str) -> Result<(FleetFile, String)> {
    let git_ref = cfg.gitlab_ref.as_deref().unwrap_or("main");
    let project = GitlabProject::parse(gitlab_url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;

    match fetch_file(&client, cfg, &project, "ops/fleet.json", git_ref).await {
        Ok(raw) => {
            let fleet = FleetFile::parse_json(&raw)?;
            cache_write(gitlab_url, "ops/fleet.json", &raw)?;
            return Ok((fleet, "ops/fleet.json".into()));
        }
        Err(e) => {
            tracing::warn!("ops/fleet.json: {e:#}");
        }
    }

    // Unpublished or private topology: use the checkout before a public env.example.
    if let Some(local) = local_checkout_fleet_json(cfg) {
        return Ok(local);
    }

    match fetch_file(&client, cfg, &project, "relay.env.example", git_ref).await {
        Ok(raw) => {
            let fleet = FleetFile::parse_env_example(&raw)?;
            cache_write(gitlab_url, "relay.env.example", &raw)?;
            return Ok((fleet, "relay.env.example".into()));
        }
        Err(e) => {
            tracing::warn!("relay.env.example: {e:#}");
        }
    }

    if let Some(cached) = cache_read(gitlab_url) {
        return Ok(cached);
    }
    if let Some(local) = local_checkout_topology(cfg) {
        return Ok(local);
    }
    Err(anyhow!(
        "could not fetch ops/fleet.json or relay.env.example from {gitlab_url} (need a public file or GITLAB_TOKEN)"
    ))
}

fn local_checkout_fleet_json(cfg: &OpsConfig) -> Option<(FleetFile, String)> {
    let root = cfg.checkout_path()?;
    let fleet_path = root.join("ops/fleet.json");
    let raw = fs::read_to_string(&fleet_path).ok()?;
    let fleet = FleetFile::parse_json(&raw).ok()?;
    Some((fleet, fleet_path.display().to_string()))
}

fn local_checkout_topology(cfg: &OpsConfig) -> Option<(FleetFile, String)> {
    if let Some(local) = local_checkout_fleet_json(cfg) {
        return Some(local);
    }
    let root = cfg.checkout_path()?;
    let env_path = root.join("relay.env.example");
    if let Ok(raw) = fs::read_to_string(&env_path) {
        if let Ok(fleet) = FleetFile::parse_env_example(&raw) {
            return Some((fleet, env_path.display().to_string()));
        }
    }
    None
}

async fn fetch_file(
    client: &reqwest::Client,
    cfg: &OpsConfig,
    project: &GitlabProject,
    file: &str,
    git_ref: &str,
) -> Result<String> {
    let raw_url = project.raw_url(file, git_ref);
    let mut req = client.get(&raw_url);
    if let Some(tok) = cfg.gitlab_token.as_deref() {
        req = req.header("PRIVATE-TOKEN", tok);
    }
    let resp = req.send().await.with_context(|| raw_url.clone())?;
    if resp.status().is_success() {
        return Ok(resp.text().await?);
    }
    let status = resp.status();
    let encoded_proj = urlencoding::encode(&project.path);
    let encoded_file = urlencoding::encode(file);
    let api = format!(
        "{}/projects/{}/repository/files/{}/raw?ref={}",
        project.api_base, encoded_proj, encoded_file, git_ref
    );
    let mut req = client.get(&api);
    if let Some(tok) = cfg.gitlab_token.as_deref() {
        req = req.header("PRIVATE-TOKEN", tok);
    }
    let resp = req.send().await.with_context(|| api.clone())?;
    if resp.status().is_success() {
        return Ok(resp.text().await?);
    }
    Err(anyhow!("{file}: raw {status}, api {}", resp.status()))
}

fn cache_key(gitlab_url: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(gitlab_url.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

fn cache_write(gitlab_url: &str, source: &str, raw: &str) -> Result<()> {
    let dir = cache_dir().join(cache_key(gitlab_url));
    ensure_dir(&dir)?;
    fs::write(dir.join("source.txt"), source)?;
    fs::write(dir.join("body.txt"), raw)?;
    fs::write(dir.join("url.txt"), gitlab_url)?;
    Ok(())
}

fn cache_read(gitlab_url: &str) -> Option<(FleetFile, String)> {
    let dir = cache_dir().join(cache_key(gitlab_url));
    let raw = fs::read_to_string(dir.join("body.txt")).ok()?;
    let source = fs::read_to_string(dir.join("source.txt")).ok()?;
    let fleet = if source.contains("fleet.json") {
        FleetFile::parse_json(&raw).ok()?
    } else {
        FleetFile::parse_env_example(&raw).ok()?
    };
    Some((fleet, format!("{source} (cache)")))
}

/// Percent-encoding helper used by GitLab API paths.
mod urlencoding {
    pub fn encode(s: &str) -> String {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https_project() {
        let p = GitlabProject::parse("https://gitlab.com/clevertree/relay-gateway.git").unwrap();
        assert_eq!(p.path, "clevertree/relay-gateway");
        assert!(p
            .raw_url("ops/fleet.json", "main")
            .contains("/-/raw/main/ops/fleet.json"));
    }

    #[test]
    fn parse_ssh_project() {
        let p = GitlabProject::parse("git@gitlab.com:clevertree/relay-gateway.git").unwrap();
        assert_eq!(p.path, "clevertree/relay-gateway");
    }

    #[test]
    fn parse_self_hosted_and_trailing_slash() {
        let p = GitlabProject::parse("https://git.example.net/acme/relay.git/").unwrap();
        assert_eq!(p.path, "acme/relay");
        assert_eq!(p.web_base, "https://git.example.net");
    }
}

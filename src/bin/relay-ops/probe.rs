use crate::topology::{FleetFile, FleetNode};
use serde::Serialize;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize)]
pub struct NodeProbe {
    pub fqdn: String,
    pub dns: Vec<String>,
    pub tcp22: bool,
    pub tcp443: bool,
    pub tcp_git: bool,
    pub https_config: Option<ConfigProbe>,
    pub git_tips: Vec<GitTip>,
    pub healthy: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigProbe {
    pub ok: bool,
    pub relay_server_id: Option<String>,
    pub peers: Vec<String>,
    pub status: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitTip {
    pub repo: String,
    pub sha: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngressProbe {
    pub fqdn: String,
    pub dns: Vec<String>,
}

pub async fn probe_fleet(fleet: &FleetFile) -> (Vec<NodeProbe>, Option<IngressProbe>, Vec<String>) {
    let git_port = fleet.git_port();
    let repos = fleet.git_repos.clone();
    let mut set = tokio::task::JoinSet::new();
    for n in fleet.nodes.clone() {
        let repos = repos.clone();
        set.spawn(async move { probe_node(&n, git_port, &repos).await });
    }
    let mut nodes = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(n) = res {
            nodes.push(n);
        }
    }
    nodes.sort_by(|a, b| a.fqdn.cmp(&b.fqdn));
    let mut live_peers = Vec::new();
    for n in &nodes {
        if let Some(cfg) = &n.https_config {
            for p in &cfg.peers {
                if !live_peers.iter().any(|x: &String| x.eq_ignore_ascii_case(p)) {
                    live_peers.push(p.clone());
                }
            }
        }
    }
    let ingress = if let Some(fqdn) = fleet.fleet_ingress.clone() {
        Some(IngressProbe {
            dns: resolve_ips(&fqdn).await,
            fqdn,
        })
    } else {
        None
    };
    (nodes, ingress, live_peers)
}

async fn probe_node(node: &FleetNode, git_port: u16, repos: &[String]) -> NodeProbe {
    let fqdn = node.fqdn.clone();
    let (dns, tcp22, tcp443, tcp_git, https_config) = tokio::join!(
        resolve_ips(&fqdn),
        tcp_open(&fqdn, 22),
        tcp_open(&fqdn, 443),
        tcp_open(&fqdn, git_port),
        https_api_config(&fqdn),
    );
    let mut git_tips = Vec::new();
    for repo in repos {
        git_tips.push(git_ls_remote(&fqdn, git_port, repo).await);
    }
    let healthy = https_config.as_ref().map(|c| c.ok).unwrap_or(false) && tcp_git;
    NodeProbe {
        fqdn,
        dns,
        tcp22,
        tcp443,
        tcp_git,
        https_config,
        git_tips,
        healthy,
        error: None,
    }
}

pub async fn resolve_ips(host: &str) -> Vec<String> {
    let lookup = format!("{host}:0");
    match timeout(Duration::from_secs(4), tokio::net::lookup_host(lookup)).await {
        Ok(Ok(addrs)) => {
            let mut out: Vec<String> = addrs
                .map(|a: SocketAddr| a.ip().to_string())
                .collect();
            out.sort();
            out.dedup();
            out
        }
        _ => Vec::new(),
    }
}

async fn tcp_open(host: &str, port: u16) -> bool {
    let target = format!("{host}:{port}");
    timeout(Duration::from_secs(3), TcpStream::connect(&target))
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some()
}

async fn https_api_config(host: &str) -> Option<ConfigProbe> {
    let url = format!("https://{host}/api/config");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .danger_accept_invalid_certs(false)
        .build()
        .ok()?;
    let resp = client.post(&url).json(&serde_json::json!({})).send().await.ok()?;
    let status = resp.status().as_u16();
    let ok = resp.status().is_success();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    let relay_server_id = body
        .get("relay_server_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let peers = body
        .get("peers")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(ConfigProbe {
        ok,
        relay_server_id,
        peers,
        status,
    })
}

async fn git_ls_remote(host: &str, port: u16, repo: &str) -> GitTip {
    if which_git().is_none() {
        return GitTip {
            repo: repo.into(),
            sha: None,
            error: Some("git not installed".into()),
        };
    }
    let url = format!("git://{host}:{port}/{repo}.git");
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", &url, "refs/heads/main"]);
    match timeout(Duration::from_secs(8), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let sha = text
                .split_whitespace()
                .next()
                .filter(|s| s.len() >= 7)
                .map(|s| s.to_string());
            GitTip {
                repo: repo.into(),
                sha,
                error: None,
            }
        }
        Ok(Ok(out)) => GitTip {
            repo: repo.into(),
            sha: None,
            error: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        },
        Ok(Err(e)) => GitTip {
            repo: repo.into(),
            sha: None,
            error: Some(e.to_string()),
        },
        Err(_) => GitTip {
            repo: repo.into(),
            sha: None,
            error: Some("timeout".into()),
        },
    }
}

fn which_git() -> Option<()> {
    which::which_os("git")
}

mod which {
    pub fn which_os(name: &str) -> Option<()> {
        let exe = if cfg!(windows) && !name.ends_with(".exe") {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                if dir.join(&exe).is_file() {
                    return Some(());
                }
            }
        }
        None
    }
}

/// IPs of nodes that still look alive enough to keep on fleet ingress.
pub fn healthy_node_ips(nodes: &[NodeProbe]) -> Vec<String> {
    let mut ips = Vec::new();
    for n in nodes {
        if n.healthy || n.tcp443 {
            for ip in &n.dns {
                if !ips.contains(ip) {
                    ips.push(ip.clone());
                }
            }
        }
    }
    ips
}

pub fn dead_ingress_ips(ingress: &IngressProbe, keep: &[String]) -> Vec<String> {
    ingress
        .dns
        .iter()
        .filter(|ip| !keep.iter().any(|k| k == *ip))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_stale_ingress_ips() {
        let ingress = IngressProbe {
            fqdn: "fleet.example.net".into(),
            dns: vec!["1.1.1.1".into(), "2.2.2.2".into()],
        };
        let keep = vec!["1.1.1.1".to_string()];
        assert_eq!(dead_ingress_ips(&ingress, &keep), vec!["2.2.2.2".to_string()]);
    }
}

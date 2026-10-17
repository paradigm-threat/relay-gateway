use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetFile {
    #[serde(default)]
    pub schema: String,
    #[serde(rename = "fleetIngress", default)]
    pub fleet_ingress: Option<String>,
    #[serde(rename = "gitPort")]
    pub git_port: Option<u16>,
    #[serde(default)]
    pub nodes: Vec<FleetNode>,
    #[serde(rename = "gitRepos", default)]
    pub git_repos: Vec<String>,
    #[serde(default)]
    pub health: HealthSpec,
    #[serde(rename = "catalogPath", default)]
    pub catalog_path: Option<String>,
    #[serde(rename = "dataRoot", default)]
    pub data_root: Option<String>,
    /// Single-MX inbound mailbox (read via relay-ops Inbox). Not multi-host.
    #[serde(default)]
    pub mail: Option<MailSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailSpec {
    /// MX host FQDN (OpenSMTPD). Default iad1.relaygateway.net.
    #[serde(default)]
    pub host: Option<String>,
    /// Absolute Maildir path on that host. Default /home/arl/Maildir.
    #[serde(default)]
    pub maildir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthSpec {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(rename = "sslDomains", default)]
    pub ssl_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetNode {
    pub fqdn: String,
    #[serde(rename = "sshUser", default)]
    pub ssh_user: Option<String>,
    #[serde(rename = "deployTarget", default)]
    pub deploy_target: Option<String>,
    #[serde(rename = "spotCloudspace", default)]
    pub spot_cloudspace: Option<String>,
}

impl FleetFile {
    pub fn git_port(&self) -> u16 {
        self.git_port.unwrap_or(9418)
    }

    pub fn parse_json(raw: &str) -> Result<Self> {
        let f: FleetFile = serde_json::from_str(raw).context("parse ops/fleet.json")?;
        if f.schema != "relay.ops.fleet.v1" && !f.schema.is_empty() {
            // Unknown schema versions are still usable if nodes exist.
        }
        if f.nodes.is_empty() {
            return Err(anyhow!("ops/fleet.json has no nodes"));
        }
        Ok(f)
    }

    pub fn parse_env_example(raw: &str) -> Result<Self> {
        let mut peers = Vec::new();
        for key in ["RELAY_OPERATOR_FLEET", "RELAY_MASTER_PEER_LIST"] {
            if let Some(v) = env_assignment(raw, key) {
                peers = v
                    .split(';')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !peers.is_empty() {
                    break;
                }
            }
        }
        if peers.is_empty() {
            return Err(anyhow!("relay.env.example has no peer FQDNs"));
        }
        Ok(FleetFile {
            schema: "relay.ops.fleet.v1".into(),
            fleet_ingress: None,
            git_port: Some(9418),
            nodes: peers
                .into_iter()
                .map(|fqdn| FleetNode {
                    fqdn,
                    ssh_user: None,
                    deploy_target: None,
                    spot_cloudspace: None,
                })
                .collect(),
            git_repos: Vec::new(),
            health: HealthSpec::default(),
            catalog_path: None,
            data_root: None,
            mail: Some(MailSpec {
                host: Some("iad1.relaygateway.net".into()),
                maildir: Some("/home/arl/Maildir".into()),
            }),
        })
    }

    pub fn union_peers(&mut self, extra: &[String]) {
        for p in extra {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            if !self.nodes.iter().any(|n| n.fqdn.eq_ignore_ascii_case(p)) {
                self.nodes.push(FleetNode {
                    fqdn: p.to_string(),
                    ssh_user: None,
                    deploy_target: None,
                    spot_cloudspace: None,
                });
            }
        }
    }

    pub fn filtered(&self, only: Option<&str>) -> anyhow::Result<Self> {
        let Some(only) = only.filter(|s| !s.is_empty()) else {
            return Ok(self.clone());
        };
        let nodes: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| n.fqdn.eq_ignore_ascii_case(only))
            .cloned()
            .collect();
        if nodes.is_empty() {
            anyhow::bail!("--only={only} matched no fleet node");
        }
        let mut out = self.clone();
        out.nodes = nodes;
        Ok(out)
    }

    pub fn catalog_path(&self) -> &str {
        self.catalog_path
            .as_deref()
            .unwrap_or("/opt/relay/src/relay-master")
    }

    pub fn data_root(&self) -> &str {
        self.data_root.as_deref().unwrap_or("/opt/relay/data")
    }

    pub fn health_repos(&self) -> Vec<String> {
        if !self.health.repos.is_empty() {
            self.health.repos.clone()
        } else {
            self.git_repos.clone()
        }
    }

    pub fn health_query(&self) -> &str {
        self.health.query.as_deref().unwrap_or("venus")
    }

    pub fn ssh_user<'a>(&'a self, fqdn: &str, cfg_default: Option<&'a str>) -> &'a str {
        self.node(fqdn)
            .and_then(|n| n.ssh_user.as_deref())
            .or(cfg_default)
            .unwrap_or("ubuntu")
    }

    pub fn node(&self, fqdn: &str) -> Option<&FleetNode> {
        self.nodes
            .iter()
            .find(|n| n.fqdn.eq_ignore_ascii_case(fqdn))
    }
}

fn env_assignment(raw: &str, key: &str) -> Option<String> {
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fleet_json() {
        let raw = r#"{
          "schema": "relay.ops.fleet.v1",
          "fleetIngress": "fleet.example.net",
          "nodes": [{"fqdn": "a.example.net", "sshUser": "ubuntu"}]
        }"#;
        let f = FleetFile::parse_json(raw).unwrap();
        assert_eq!(f.nodes[0].fqdn, "a.example.net");
        assert_eq!(f.fleet_ingress.as_deref(), Some("fleet.example.net"));
    }

    #[test]
    fn parse_health_spec() {
        let raw = r#"{
          "schema": "relay.ops.fleet.v1",
          "nodes": [{"fqdn": "a.example.net"}],
          "gitRepos": ["fallback-repo"],
          "health": {"query": "probe", "repos": ["app"]}
        }"#;
        let f = FleetFile::parse_json(raw).unwrap();
        assert_eq!(f.health_query(), "probe");
        assert_eq!(f.health_repos(), vec!["app".to_string()]);
    }

    #[test]
    fn health_repos_falls_back_to_git_repos() {
        let f = FleetFile::parse_json(
            r#"{"schema":"relay.ops.fleet.v1","nodes":[{"fqdn":"a.example.net"}],"gitRepos":["x"]}"#,
        )
        .unwrap();
        assert_eq!(f.health_repos(), vec!["x".to_string()]);
        assert_eq!(f.health_query(), "venus");
    }

    #[test]
    fn parse_env_example_peers() {
        let raw = r#"
# comment
RELAY_MASTER_PEER_LIST="a.example.net;b.example.net"
"#;
        let f = FleetFile::parse_env_example(raw).unwrap();
        assert_eq!(f.nodes.len(), 2);
        assert_eq!(f.nodes[1].fqdn, "b.example.net");
    }

    #[test]
    fn committed_ops_fleet_json_parses() {
        let raw = include_str!("../../../ops/fleet.json");
        let f = FleetFile::parse_json(raw).unwrap();
        assert!(f.nodes.len() >= 3);
        assert_eq!(f.schema, "relay.ops.fleet.v1");
        assert!(f
            .fleet_ingress
            .as_deref()
            .unwrap()
            .contains("relaygateway.net"));
        assert!(f.nodes.iter().all(|n| n.fqdn.contains('.')));
        assert!(f.nodes.iter().all(|n| n.spot_cloudspace.is_some()));
        assert!(!f.health_repos().is_empty());
        assert!(!f.health_query().is_empty());
    }

    #[test]
    fn union_adds_missing_peers() {
        let mut f = FleetFile::parse_json(
            r#"{"schema":"relay.ops.fleet.v1","nodes":[{"fqdn":"a.example.net"}]}"#,
        )
        .unwrap();
        f.union_peers(&["a.example.net".into(), "c.example.net".into()]);
        assert_eq!(f.nodes.len(), 2);
    }

    #[test]
    fn filter_only_host() {
        let f = FleetFile::parse_json(
            r#"{"schema":"relay.ops.fleet.v1","nodes":[{"fqdn":"a.example.net"},{"fqdn":"b.example.net"}]}"#,
        )
        .unwrap();
        let one = f.filtered(Some("b.example.net")).unwrap();
        assert_eq!(one.nodes.len(), 1);
        assert_eq!(one.nodes[0].fqdn, "b.example.net");
        assert!(f.filtered(Some("nope")).is_err());
    }
}

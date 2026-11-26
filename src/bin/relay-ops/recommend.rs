//! Simple probe → operator message + visible actions (no scenario YAML / brain).

use crate::probe::{NodeProbe, RepoDrift, SyncAnomaly};

#[derive(Debug, Clone)]
pub struct RecommendInput<'a> {
    pub healthy: bool,
    pub sync_ok: bool,
    pub missing_repos: &'a [String],
    pub drift: &'a [RepoDrift],
    pub sync_anomalies: &'a [SyncAnomaly],
    pub dns_mismatch: bool,
    pub tcp22: bool,
    pub tcp443: bool,
    pub https_ok: bool,
    pub ssh_ok: Option<bool>,
    pub host_key_changed: bool,
    pub install_present: Option<bool>,
    pub index_ok: bool,
    pub index_stale_count: usize,
    pub index_pending_count: usize,
    pub index_errors: &'a [String],
    pub unreachable_peers: &'a [String],
    pub ownership_bad_repos: &'a [String],
    pub park_tls_broken: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Recommendation {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct VisibleActions {
    #[serde(rename = "point-dns")]
    pub point_dns: bool,
    pub sync: bool,
    #[serde(rename = "index-warm")]
    pub index_warm: bool,
    #[serde(rename = "fix-repo-ownership")]
    pub fix_repo_ownership: bool,
    pub repair: bool,
}

/// DNS mismatch vs expected node A (Linode IP). Pass `None` to skip.
pub fn dns_mismatch(dns: &[String], node_ip: Option<&str>) -> bool {
    match node_ip {
        Some(ip) if !dns.is_empty() => !dns.iter().any(|d| d == ip),
        _ => false,
    }
}

/// `fleet_dns_participant`: only publisher / `fleetDns: true` nodes should own park TLS.
/// Mesh IPs are not in park ALIAS multi-A, so SNI via mesh IP is expected to fail.
pub fn for_node(
    n: &NodeProbe,
    node_ip: Option<&str>,
    fleet_dns_participant: bool,
) -> (Option<Recommendation>, VisibleActions) {
    let input = RecommendInput {
        healthy: n.healthy,
        sync_ok: n.sync_ok,
        missing_repos: &n.missing_repos,
        drift: &n.drift,
        sync_anomalies: &n.sync_anomalies,
        dns_mismatch: dns_mismatch(&n.dns, node_ip),
        tcp22: n.tcp22,
        tcp443: n.tcp443,
        https_ok: n.https_config.as_ref().map(|c| c.ok).unwrap_or(false),
        ssh_ok: n.ssh_ok,
        host_key_changed: n.host_key_changed,
        install_present: n.install_present,
        index_ok: n.index_ok,
        index_stale_count: n.index_stale_repos.len(),
        index_pending_count: n.index_pending_repos.len(),
        index_errors: &n.index_errors,
        unreachable_peers: &n.unreachable_peers,
        ownership_bad_repos: &n.ownership_bad_repos,
        park_tls_broken: fleet_dns_participant && n.park_tls_ok == Some(false),
    };
    let vis = visible_actions(&input);
    (recommend(&input), vis)
}

pub fn visible_actions(input: &RecommendInput<'_>) -> VisibleActions {
    VisibleActions {
        point_dns: input.dns_mismatch,
        sync: !input.missing_repos.is_empty() || !input.drift.is_empty() || !input.sync_ok,
        index_warm: !input.index_ok || input.index_stale_count > 0,
        fix_repo_ownership: !input.ownership_bad_repos.is_empty(),
        repair: !input.healthy && input.tcp22,
    }
}

pub fn recommend(input: &RecommendInput<'_>) -> Option<Recommendation> {
    if input.host_key_changed {
        return Some(Recommendation {
            message: "SSH host key changed — reset known_hosts then reconnect.".into(),
            primary_action: Some("known-hosts-reset".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if !input.healthy && !input.tcp22 {
        return Some(Recommendation {
            message: "Node down (SSH closed). Check Linode power / console. If this is a mesh node at scale-down, shrink publisher RELAY_MASTER_PEER_LIST first.".into(),
            primary_action: Some("repair".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if !input.unreachable_peers.is_empty() {
        return Some(Recommendation {
            message: format!(
                "Configured peers unreachable: {} — shrink RELAY_MASTER_PEER_LIST before destroy or publish mesh-wait fails. See ops/CREDIT_BURN.md",
                input.unreachable_peers.join(", ")
            ),
            primary_action: None,
            scenario_id: None,
            blocked: false,
        });
    }
    if input.dns_mismatch {
        return Some(Recommendation {
            message: "FQDN A record does not match this node's IP — Point DNS.".into(),
            primary_action: Some("point-dns".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if !input.ownership_bad_repos.is_empty() {
        return Some(Recommendation {
            message: format!(
                "Bare repos not owned by relay: {} — Fix ownership.",
                input.ownership_bad_repos.join(", ")
            ),
            primary_action: Some("fix-repo-ownership".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if !input.missing_repos.is_empty() || !input.drift.is_empty() || !input.sync_ok {
        return Some(Recommendation {
            message: "Git tip / catalog out of sync with publish origin — Synchronize.".into(),
            primary_action: Some("sync".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if !input.index_errors.is_empty() {
        return Some(Recommendation {
            message: format!(
                "SearchStore warm failed: {} — Warm indexes / check journalctl.",
                input.index_errors.join("; ")
            ),
            primary_action: Some("index-warm".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if input.index_pending_count > 0 {
        return Some(Recommendation {
            message: format!(
                "SearchStore warming… ({} repos). Wait; re-check status.",
                input.index_pending_count
            ),
            primary_action: None,
            scenario_id: None,
            blocked: false,
        });
    }
    if !input.index_ok || input.index_stale_count > 0 {
        return Some(Recommendation {
            message: format!(
                "SearchStore lags tip ({} repos) — Warm indexes.",
                input.index_stale_count.max(1)
            ),
            primary_action: Some("index-warm".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    if input.park_tls_broken {
        return Some(Recommendation {
            message: "Park TLS check failed against this node IP.".into(),
            primary_action: None,
            scenario_id: None,
            blocked: false,
        });
    }
    if input.tcp443 && !input.https_ok {
        return Some(Recommendation {
            message: "HTTPS listener up but /api/config failed — check gateway/Caddy.".into(),
            primary_action: Some("repair".into()),
            scenario_id: None,
            blocked: false,
        });
    }
    None
}

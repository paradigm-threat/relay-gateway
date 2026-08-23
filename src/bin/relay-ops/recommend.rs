//! Map probe symptoms to one operator sentence and a primary UI action.
//!
//! Priority matches real capabilities (do not recommend Repair for host-key
//! change, empty Spot VMs, DNS-on-the-wrong-box, or catalog tip drift).

use crate::probe::{DriftKind, NodeProbe, RepoDrift, SyncAnomaly};

#[derive(Debug, Clone)]
pub struct RecommendInput<'a> {
    pub healthy: bool,
    pub sync_ok: bool,
    pub missing_repos: &'a [String],
    pub drift: &'a [RepoDrift],
    pub sync_anomalies: &'a [SyncAnomaly],
    pub dns_mismatch: bool,
    pub tcp22: bool,
    pub https_ok: bool,
    pub ssh_ok: Option<bool>,
    pub host_key_changed: bool,
    pub install_present: Option<bool>,
    /// Spot assigned an IPv4 for this node's cloudspace.
    pub has_spot_ip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Recommendation {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_action: Option<&'static str>,
}

/// Per-cell buttons that are valid for this probe. Invalid actions are omitted
/// (not greyed out). `known-hosts` is fleet-wide in the Admin header, never here.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct VisibleActions {
    #[serde(rename = "point-dns")]
    pub point_dns: bool,
    pub sync: bool,
    pub repair: bool,
    pub reclaim: bool,
}

pub fn dns_mismatch(dns: &[String], spot_ip: Option<&str>) -> bool {
    match spot_ip {
        Some(ip) if !dns.is_empty() => !dns.iter().any(|d| d == ip),
        _ => false,
    }
}

pub fn for_node(n: &NodeProbe, spot_ip: Option<&str>) -> (Option<Recommendation>, VisibleActions) {
    let input = RecommendInput {
        healthy: n.healthy,
        sync_ok: n.sync_ok,
        missing_repos: &n.missing_repos,
        drift: &n.drift,
        sync_anomalies: &n.sync_anomalies,
        dns_mismatch: dns_mismatch(&n.dns, spot_ip),
        tcp22: n.tcp22,
        https_ok: n.https_config.as_ref().map(|c| c.ok).unwrap_or(false),
        ssh_ok: n.ssh_ok,
        host_key_changed: n.host_key_changed,
        install_present: n.install_present,
        has_spot_ip: spot_ip.is_some(),
    };
    let vis = visible_actions(&input);
    (recommend(input), vis)
}

/// Which per-cell actions to render. Hide, do not disable, the rest.
pub fn visible_actions(input: &RecommendInput<'_>) -> VisibleActions {
    VisibleActions {
        point_dns: input.dns_mismatch,
        sync: input.healthy && needs_sync(input),
        repair: show_repair(input),
        reclaim: show_reclaim(input),
    }
}

fn needs_sync(input: &RecommendInput<'_>) -> bool {
    if input.sync_ok {
        return false;
    }
    if !input.missing_repos.is_empty() {
        return true;
    }
    input
        .drift
        .iter()
        .any(|d| d.kind != DriftKind::ReplicaAhead)
}

fn show_repair(input: &RecommendInput<'_>) -> bool {
    input.ssh_ok == Some(true)
        && input.install_present == Some(true)
        && !input.healthy
        && !input.dns_mismatch
        && !input.host_key_changed
}

fn show_reclaim(input: &RecommendInput<'_>) -> bool {
    input.has_spot_ip
        && input.ssh_ok == Some(true)
        && input.install_present == Some(false)
        && !input.dns_mismatch
        && !input.host_key_changed
}

pub fn recommend(input: RecommendInput<'_>) -> Option<Recommendation> {
    // Down + DNS A ≠ Spot IP: SSH to the FQDN may be the wrong box.
    if !input.healthy && input.dns_mismatch {
        return Some(Recommendation {
            message: "DNS A does not match the Spot IP — click Point DNS to Spot IP (do not Repair; SSH may hit the wrong box)."
                .into(),
            primary_action: Some("point-dns"),
        });
    }

    // Host key in ~/.ssh/known_hosts does not match. Repair uses SSH and will fail.
    if input.host_key_changed {
        return Some(Recommendation {
            message: "SSH host key changed — click Reset known_hosts in Admin (Repair will fail StrictHostKeyChecking)."
                .into(),
            primary_action: Some("known-hosts"),
        });
    }

    // SSH works on a blank Spot VM we own.
    if show_reclaim(&input) {
        return Some(Recommendation {
            message: "SSH works but /opt/relay is missing — click Reclaim to install on this empty VM (not Repair)."
                .into(),
            primary_action: Some("reclaim"),
        });
    }

    // Existing tree, units/HTTPS/git broken.
    if show_repair(&input) {
        return Some(Recommendation {
            message: "Install is present but the node is down — click Repair (install.sh repair). Do not Reclaim."
                .into(),
            primary_action: Some("repair"),
        });
    }

    // Publish origin is behind a non-publisher replica (mesh anomaly).
    if input.healthy && !input.sync_anomalies.is_empty() {
        return Some(Recommendation {
            message: publisher_anomaly_message(input.sync_anomalies),
            primary_action: None,
        });
    }

    // Reachable relay whose catalog/git tips disagree with the fleet.
    if input.healthy && needs_sync(&input) {
        return Some(Recommendation {
            message: sync_message(input.missing_repos, input.drift),
            primary_action: Some("sync"),
        });
    }

    if input.healthy && !input.sync_ok && !input.drift.is_empty() {
        return Some(Recommendation {
            message: sync_message(input.missing_repos, input.drift),
            primary_action: None,
        });
    }

    if input.healthy && input.dns_mismatch {
        return Some(Recommendation {
            message:
                "Spot IP differs from DNS A — click Point DNS to Spot IP if this node was replaced."
                    .into(),
            primary_action: Some("point-dns"),
        });
    }

    if input.healthy && input.sync_ok {
        return None;
    }

    Some(Recommendation {
        message: down_fallback(&input),
        primary_action: None,
    })
}

fn sync_message(missing: &[String], drift: &[RepoDrift]) -> String {
    let mut bits = Vec::new();
    if !missing.is_empty() {
        bits.push(format!("missing {}", missing.join(", ")));
    }
    if !drift.is_empty() {
        let behind: Vec<_> = drift
            .iter()
            .filter(|d| d.kind != DriftKind::ReplicaAhead)
            .map(|d| format!("{}@{}", d.repo, d.branch))
            .collect();
        if !behind.is_empty() {
            bits.push(format!("drift {}", behind.join(", ")));
        }
        let ahead: Vec<_> = drift
            .iter()
            .filter(|d| d.kind == DriftKind::ReplicaAhead)
            .map(|d| format!("{}@{}", d.repo, d.branch))
            .collect();
        if !ahead.is_empty() {
            bits.push(format!(
                "replica ahead of publish origin ({}) — fix on publisher, do not Synchronize here",
                ahead.join(", ")
            ));
        }
    }
    let detail = if bits.is_empty() {
        "catalog/git tips disagree".to_string()
    } else {
        bits.join("; ")
    };
    format!("Out of sync ({detail}) — click Synchronize to fetch catalog/peer tips (not Repair or Reclaim).")
}

fn publisher_anomaly_message(anomalies: &[SyncAnomaly]) -> String {
    let detail = anomalies
        .iter()
        .map(|a| {
            format!(
                "{}@{} publish origin {} behind {} ({})",
                a.repo, a.branch, a.publisher_fqdn, a.replica_fqdn, a.replica_sha
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Publish origin behind replica ({detail}) — push or republish on the publish-origin node; do not Synchronize replicas that are ahead."
    )
}

fn down_fallback(input: &RecommendInput<'_>) -> String {
    if !input.tcp22 {
        return "Node is down and port 22 is closed — check DNS/Spot, then Reclaim a new VM or Repair an existing install.".into();
    }
    if input.ssh_ok == Some(false) {
        return "SSH failed — Reset known_hosts in Admin if the host key changed; otherwise check the key and whether DNS points at the Spot IP.".into();
    }
    if input.https_ok {
        return "HTTPS is up but git/health failed — click Repair if units are broken; Synchronize only after the node is healthy.".into();
    }
    "Node is down. Reset known_hosts in Admin if the SSH host key changed; Reclaim a new empty VM; Repair an existing install.".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift(repo: &str, branch: &str) -> RepoDrift {
        RepoDrift {
            repo: repo.into(),
            branch: branch.into(),
            sha: Some("bbb".into()),
            expected_sha: "aaa".into(),
            kind: DriftKind::TipMismatch,
        }
    }

    fn base<'a>(missing: &'a [String], drift: &'a [RepoDrift]) -> RecommendInput<'a> {
        RecommendInput {
            healthy: true,
            sync_ok: true,
            missing_repos: missing,
            drift,
            sync_anomalies: &[],
            dns_mismatch: false,
            tcp22: true,
            https_ok: true,
            ssh_ok: None,
            host_key_changed: false,
            install_present: None,
            has_spot_ip: false,
        }
    }

    fn shown(input: &RecommendInput<'_>) -> VisibleActions {
        visible_actions(input)
    }

    #[test]
    fn healthy_in_sync_is_silent() {
        let input = base(&[], &[]);
        assert_eq!(recommend(input.clone()), None);
        assert_eq!(shown(&input), VisibleActions::default());
    }

    #[test]
    fn hkg1_style_drift_recommends_sync_not_repair() {
        let rows = [drift("clevertree-site", "release")];
        let mut input = base(&[], &rows);
        input.sync_ok = false;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("sync"));
        assert!(rec.message.contains("Synchronize"), "{}", rec.message);
        assert!(
            rec.message.contains("clevertree-site@release"),
            "{}",
            rec.message
        );
        assert!(rec.message.contains("not Repair"), "{}", rec.message);
        let vis = shown(&input);
        assert!(vis.sync);
        assert!(!vis.repair && !vis.reclaim && !vis.point_dns);
    }

    #[test]
    fn missing_catalog_repo_recommends_sync() {
        let missing = ["park".to_string()];
        let mut input = base(&missing, &[]);
        input.sync_ok = false;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("sync"));
        assert!(rec.message.contains("missing park"), "{}", rec.message);
        assert!(shown(&input).sync);
        assert!(!shown(&input).repair);
    }

    #[test]
    fn host_key_changed_recommends_known_hosts_not_repair() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.host_key_changed = true;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("known-hosts"));
        assert!(rec.message.contains("Reset known_hosts"), "{}", rec.message);
        assert!(rec.message.contains("in Admin"), "{}", rec.message);
        assert!(rec.message.contains("Repair will fail"), "{}", rec.message);
        let vis = shown(&input);
        assert!(!vis.repair && !vis.reclaim && !vis.sync && !vis.point_dns);
    }

    #[test]
    fn dns_mismatch_when_down_beats_host_key_and_repair() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.dns_mismatch = true;
        input.host_key_changed = true;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("point-dns"));
        assert!(rec.message.contains("Point DNS"), "{}", rec.message);
        assert!(rec.message.contains("do not Repair"), "{}", rec.message);
        let vis = shown(&input);
        assert!(vis.point_dns);
        assert!(!vis.repair && !vis.reclaim && !vis.sync);
    }

    #[test]
    fn empty_install_recommends_reclaim_not_repair() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.ssh_ok = Some(true);
        input.install_present = Some(false);
        input.has_spot_ip = true;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("reclaim"));
        assert!(rec.message.contains("Reclaim"), "{}", rec.message);
        assert!(rec.message.contains("not Repair"), "{}", rec.message);
        let vis = shown(&input);
        assert!(vis.reclaim);
        assert!(!vis.repair && !vis.sync && !vis.point_dns);
    }

    #[test]
    fn empty_install_without_spot_hides_reclaim() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.ssh_ok = Some(true);
        input.install_present = Some(false);
        input.has_spot_ip = false;
        let rec = recommend(input.clone()).unwrap();
        assert_ne!(rec.primary_action, Some("reclaim"));
        assert!(!shown(&input).reclaim);
    }

    #[test]
    fn broken_units_on_existing_install_recommends_repair_not_reclaim() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.ssh_ok = Some(true);
        input.install_present = Some(true);
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("repair"));
        assert!(rec.message.contains("Repair"), "{}", rec.message);
        assert!(rec.message.contains("Do not Reclaim"), "{}", rec.message);
        let vis = shown(&input);
        assert!(vis.repair);
        assert!(!vis.reclaim && !vis.sync && !vis.point_dns);
    }

    #[test]
    fn down_without_diagnosis_has_no_primary() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, None);
        assert!(rec.message.contains("Reset known_hosts"), "{}", rec.message);
        assert!(rec.message.contains("Reclaim"), "{}", rec.message);
        assert!(rec.message.contains("Repair"), "{}", rec.message);
        assert_eq!(shown(&input), VisibleActions::default());
    }

    #[test]
    fn healthy_dns_mismatch_without_drift_points_dns() {
        let mut input = base(&[], &[]);
        input.dns_mismatch = true;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("point-dns"));
        assert!(shown(&input).point_dns);
        assert!(!shown(&input).sync && !shown(&input).repair && !shown(&input).reclaim);
    }

    #[test]
    fn healthy_drift_beats_dns_mismatch() {
        let rows = [drift("park", "main")];
        let mut input = base(&[], &rows);
        input.sync_ok = false;
        input.dns_mismatch = true;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, Some("sync"));
        let vis = shown(&input);
        assert!(vis.sync && vis.point_dns);
        assert!(!vis.repair && !vis.reclaim);
    }

    #[test]
    fn lon1_host_key_only_hides_all_cell_actions() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.host_key_changed = true;
        input.has_spot_ip = false;
        assert_eq!(shown(&input), VisibleActions::default());
    }

    #[test]
    fn dns_mismatch_hides_repair_and_reclaim() {
        let mut input = base(&[], &[]);
        input.healthy = false;
        input.https_ok = false;
        input.dns_mismatch = true;
        input.has_spot_ip = true;
        input.ssh_ok = Some(true);
        input.install_present = Some(false);
        let vis = shown(&input);
        assert!(vis.point_dns);
        assert!(!vis.reclaim && !vis.repair && !vis.sync);
    }

    #[test]
    fn publisher_behind_replica_hides_sync_on_publisher() {
        let anomalies = [SyncAnomaly {
            kind: "publisher_behind_replica".into(),
            repo: "clevertree-site".into(),
            branch: "release".into(),
            publisher_fqdn: "iad1.example".into(),
            publisher_sha: "aaa".into(),
            replica_fqdn: "iad1.example".into(),
            replica_sha: "bbb".into(),
        }];
        let input = RecommendInput {
            healthy: true,
            sync_ok: true,
            missing_repos: &[],
            drift: &[],
            sync_anomalies: &anomalies,
            dns_mismatch: false,
            tcp22: true,
            https_ok: true,
            ssh_ok: None,
            host_key_changed: false,
            install_present: None,
            has_spot_ip: false,
        };
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, None);
        assert!(rec.message.contains("Publish origin behind replica"));
        assert!(!shown(&input).sync);
    }

    #[test]
    fn replica_ahead_hides_sync_button() {
        let rows = [RepoDrift {
            repo: "clevertree-site".into(),
            branch: "release".into(),
            sha: Some("bbb".into()),
            expected_sha: "aaa".into(),
            kind: DriftKind::ReplicaAhead,
        }];
        let mut input = base(&[], &rows);
        input.sync_ok = false;
        assert!(!shown(&input).sync);
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action, None);
        assert!(rec.message.contains("do not Synchronize here"));
    }
}

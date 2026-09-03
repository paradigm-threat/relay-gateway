//! Map probe symptoms to one operator sentence and a primary UI action.
//!
//! Classification comes from [`crate::scenarios`] (YAML). Message text may still
//! be specialized (repo names, anomaly detail) for the UI.

use crate::probe::{DriftKind, NodeProbe, RepoDrift, SyncAnomaly};
use crate::scenarios::{self, ScenarioHit, ScenarioStore};

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
    pub has_spot_ip: bool,
    pub index_ok: bool,
    pub index_stale_count: usize,
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

/// Per-cell buttons that are valid for this probe. Invalid actions are omitted
/// (not greyed out). `known-hosts` is fleet-wide in the Admin header, never here.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct VisibleActions {
    #[serde(rename = "point-dns")]
    pub point_dns: bool,
    pub sync: bool,
    #[serde(rename = "index-warm")]
    pub index_warm: bool,
    pub repair: bool,
    pub reclaim: bool,
}

pub fn dns_mismatch(dns: &[String], spot_ip: Option<&str>) -> bool {
    match spot_ip {
        Some(ip) if !dns.is_empty() => !dns.iter().any(|d| d == ip),
        _ => false,
    }
}

fn store() -> ScenarioStore {
    scenarios::load_default(
        crate::config::OpsConfig::load()
            .checkout_path()
            .as_deref()
            .or(Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))),
    )
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
        index_ok: n.index_ok,
        index_stale_count: n.index_stale_repos.len(),
    };
    let vis = visible_actions(&input);
    (recommend(input), vis)
}

/// Which per-cell actions to render. Hide, do not disable, the rest.
pub fn visible_actions(input: &RecommendInput<'_>) -> VisibleActions {
    VisibleActions {
        point_dns: input.dns_mismatch,
        sync: input.healthy && input_needs_sync(input),
        index_warm: input.healthy && !input.index_ok && input.index_stale_count > 0,
        repair: input_show_repair(input),
        reclaim: input_show_reclaim(input),
    }
}

pub fn input_needs_sync(input: &RecommendInput<'_>) -> bool {
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

pub fn input_show_repair(input: &RecommendInput<'_>) -> bool {
    input.ssh_ok == Some(true)
        && input.install_present == Some(true)
        && !input.healthy
        && !input.dns_mismatch
        && !input.host_key_changed
}

pub fn input_show_reclaim(input: &RecommendInput<'_>) -> bool {
    input.has_spot_ip
        && input.ssh_ok == Some(true)
        && input.install_present == Some(false)
        && !input.dns_mismatch
        && !input.host_key_changed
}

pub fn classify(input: &RecommendInput<'_>) -> Option<ScenarioHit> {
    store().match_input(input)
}

pub fn recommend(input: RecommendInput<'_>) -> Option<Recommendation> {
    let hit = classify(&input)?;
    Some(recommendation_from_hit(&input, &hit))
}

pub fn recommendation_from_hit(input: &RecommendInput<'_>, hit: &ScenarioHit) -> Recommendation {
    let message = specialize_message(input, hit);
    Recommendation {
        message,
        primary_action: hit.primary_action.clone(),
        scenario_id: Some(hit.id.clone()),
        blocked: hit.blocked,
    }
}

fn specialize_message(input: &RecommendInput<'_>, hit: &ScenarioHit) -> String {
    match hit.id.as_str() {
        "catalog_missing" => missing_catalog_message(input.missing_repos),
        "catalog_sync" => sync_message(input.missing_repos, input.drift),
        "publisher_behind_replica" => publisher_anomaly_message(input.sync_anomalies),
        "index_stale" => format!(
            "SearchStore lags git tip on {} repo(s) — click Warm indexes (QUERY is read-only until indexed).",
            input.index_stale_count
        ),
        "unknown_down" => down_fallback(input),
        "replica_ahead_of_publisher" => {
            let ahead: Vec<_> = input
                .drift
                .iter()
                .filter(|d| d.kind == DriftKind::ReplicaAhead)
                .map(|d| format!("{}@{}", d.repo, d.branch))
                .collect();
            if ahead.is_empty() {
                hit.message.clone()
            } else {
                format!(
                    "Out of sync (replica ahead of publish origin ({}) — fix on publisher, do not Synchronize here).",
                    ahead.join(", ")
                )
            }
        }
        _ => hit.message.clone(),
    }
}

fn missing_catalog_message(missing: &[String]) -> String {
    let list = if missing.is_empty() {
        "catalog repo(s)".to_string()
    } else {
        format!("missing {}", missing.join(", "))
    };
    format!(
        "Out of sync ({list}) — brain auto-fills from a healthy peer (no approval); Synchronize also works."
    )
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
    format!(
        "Out of sync ({detail}) — Approve sync or click Synchronize to fetch peer tips (not Repair or Reclaim)."
    )
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
            index_ok: true,
            index_stale_count: 0,
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
        assert_eq!(rec.primary_action.as_deref(), Some("sync"));
        assert!(rec.message.contains("Approve sync") || rec.message.contains("Synchronize"), "{}", rec.message);
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
        assert_eq!(rec.primary_action.as_deref(), Some("sync"));
        assert_eq!(rec.scenario_id.as_deref(), Some("catalog_missing"));
        assert!(rec.message.contains("missing park"), "{}", rec.message);
        assert!(rec.message.contains("auto-fills"), "{}", rec.message);
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
        assert_eq!(rec.primary_action.as_deref(), Some("known-hosts"));
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
        assert_eq!(rec.primary_action.as_deref(), Some("point-dns"));
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
        assert_eq!(rec.primary_action.as_deref(), Some("reclaim"));
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
        assert_ne!(rec.primary_action.as_deref(), Some("reclaim"));
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
        assert_eq!(rec.primary_action.as_deref(), Some("repair"));
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
        assert!(rec.blocked);
    }

    #[test]
    fn healthy_dns_mismatch_without_drift_points_dns() {
        let mut input = base(&[], &[]);
        input.dns_mismatch = true;
        let rec = recommend(input.clone()).unwrap();
        assert_eq!(rec.primary_action.as_deref(), Some("point-dns"));
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
        assert_eq!(rec.primary_action.as_deref(), Some("sync"));
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
            index_ok: true,
            index_stale_count: 0,
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
        assert_eq!(rec.scenario_id.as_deref(), Some("replica_ahead_of_publisher"));
        assert!(rec.blocked);
    }
}

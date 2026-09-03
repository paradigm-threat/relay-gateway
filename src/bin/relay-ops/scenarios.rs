//! Data-driven scenario store: fingerprint match → allowlisted steps / BLOCKED.
//! Shared by `recommend` (UI) and `relay-brain`.

use crate::probe::DriftKind;
use crate::recommend::RecommendInput;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    #[serde(default = "kind_ops")]
    pub kind: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub fingerprint: Fingerprint,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub message: String,
    /// When true, brain opens a pending request before running steps.
    #[serde(default)]
    pub requires_confirm: bool,
    /// Classification is BLOCKED (unknown / tip divergence needing human).
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub primary_action: Option<String>,
}

fn kind_ops() -> String {
    "ops".into()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Fingerprint {
    #[serde(default)]
    pub healthy: Option<bool>,
    #[serde(default)]
    pub sync_ok: Option<bool>,
    #[serde(default)]
    pub dns_mismatch: Option<bool>,
    #[serde(default)]
    pub host_key_changed: Option<bool>,
    #[serde(default)]
    pub needs_sync: Option<bool>,
    /// True when catalog allowlist includes bare(s) absent on disk.
    #[serde(default)]
    pub has_missing_repos: Option<bool>,
    #[serde(default)]
    pub replica_ahead: Option<bool>,
    #[serde(default)]
    pub has_sync_anomaly: Option<bool>,
    #[serde(default)]
    pub show_repair: Option<bool>,
    #[serde(default)]
    pub show_reclaim: Option<bool>,
    #[serde(default)]
    pub index_stale: Option<bool>,
    /// True when down with no more specific diagnosis (fallback).
    #[serde(default)]
    pub unknown_down: Option<bool>,
    #[serde(default)]
    pub healthy_in_sync: Option<bool>,
    #[serde(default)]
    pub tcp22: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct InputFlags {
    pub healthy: bool,
    pub sync_ok: bool,
    pub dns_mismatch: bool,
    pub host_key_changed: bool,
    pub needs_sync: bool,
    pub has_missing_repos: bool,
    pub replica_ahead: bool,
    pub has_sync_anomaly: bool,
    pub show_repair: bool,
    pub show_reclaim: bool,
    pub index_stale: bool,
    pub unknown_down: bool,
    pub healthy_in_sync: bool,
    pub tcp22: bool,
}

impl InputFlags {
    pub fn from_recommend(input: &RecommendInput<'_>) -> Self {
        let needs_sync = crate::recommend::input_needs_sync(input);
        let has_missing_repos = !input.missing_repos.is_empty();
        let replica_ahead = input
            .drift
            .iter()
            .any(|d| d.kind == DriftKind::ReplicaAhead);
        let has_sync_anomaly = !input.sync_anomalies.is_empty();
        let show_repair = crate::recommend::input_show_repair(input);
        let show_reclaim = crate::recommend::input_show_reclaim(input);
        let index_stale = input.healthy && !input.index_ok && input.index_stale_count > 0;
        let healthy_in_sync = input.healthy && input.sync_ok && !index_stale && !has_sync_anomaly && !input.dns_mismatch;
        // "unknown_down": unhealthy without dns/host-key/repair/reclaim diagnosis
        let unknown_down = !input.healthy
            && !input.dns_mismatch
            && !input.host_key_changed
            && !show_repair
            && !show_reclaim;
        Self {
            healthy: input.healthy,
            sync_ok: input.sync_ok,
            dns_mismatch: input.dns_mismatch,
            host_key_changed: input.host_key_changed,
            needs_sync,
            has_missing_repos,
            replica_ahead,
            has_sync_anomaly,
            show_repair,
            show_reclaim,
            index_stale,
            unknown_down,
            healthy_in_sync,
            tcp22: input.tcp22,
        }
    }
}

fn flag_match(want: Option<bool>, got: bool) -> bool {
    match want {
        None => true,
        Some(v) => v == got,
    }
}

impl Fingerprint {
    pub fn matches(&self, f: &InputFlags) -> bool {
        flag_match(self.healthy, f.healthy)
            && flag_match(self.sync_ok, f.sync_ok)
            && flag_match(self.dns_mismatch, f.dns_mismatch)
            && flag_match(self.host_key_changed, f.host_key_changed)
            && flag_match(self.needs_sync, f.needs_sync)
            && flag_match(self.has_missing_repos, f.has_missing_repos)
            && flag_match(self.replica_ahead, f.replica_ahead)
            && flag_match(self.has_sync_anomaly, f.has_sync_anomaly)
            && flag_match(self.show_repair, f.show_repair)
            && flag_match(self.show_reclaim, f.show_reclaim)
            && flag_match(self.index_stale, f.index_stale)
            && flag_match(self.unknown_down, f.unknown_down)
            && flag_match(self.healthy_in_sync, f.healthy_in_sync)
            && flag_match(self.tcp22, f.tcp22)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioHit {
    pub id: String,
    pub title: String,
    pub message: String,
    pub steps: Vec<String>,
    pub primary_action: Option<String>,
    pub requires_confirm: bool,
    pub blocked: bool,
    pub priority: i32,
}

#[derive(Debug, Clone)]
pub struct ScenarioStore {
    pub scenarios: Vec<Scenario>,
    pub source: String,
}

impl ScenarioStore {
    pub fn empty() -> Self {
        Self {
            scenarios: Vec::new(),
            source: "empty".into(),
        }
    }

    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut scenarios = Vec::new();
        if !dir.is_dir() {
            return Ok(Self {
                scenarios,
                source: format!("{} (missing)", dir.display()),
            });
        }
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .with_context(|| format!("read {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "yaml" || e == "yml")
            })
            .collect();
        paths.sort();
        for p in &paths {
            let raw = fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            let s: Scenario =
                serde_yaml::from_str(&raw).with_context(|| format!("parse {}", p.display()))?;
            scenarios.push(s);
        }
        scenarios.sort_by_key(|s| s.priority);
        Ok(Self {
            scenarios,
            source: dir.display().to_string(),
        })
    }

    pub fn match_flags(&self, flags: &InputFlags) -> Option<ScenarioHit> {
        if flags.healthy_in_sync {
            return None;
        }
        for s in &self.scenarios {
            if s.fingerprint.matches(flags) {
                let primary = s
                    .primary_action
                    .clone()
                    .or_else(|| s.steps.first().cloned());
                return Some(ScenarioHit {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    message: if s.message.is_empty() {
                        s.title.clone()
                    } else {
                        s.message.clone()
                    },
                    steps: s.steps.clone(),
                    primary_action: primary,
                    requires_confirm: s.requires_confirm || s.blocked,
                    blocked: s.blocked,
                    priority: s.priority,
                });
            }
        }
        // No YAML match and not healthy-in-sync → BLOCKED unknown
        Some(ScenarioHit {
            id: "unknown".into(),
            title: "Unknown fingerprint".into(),
            message: "No scenario matches this probe fingerprint — BLOCKED until ops/scenarios grows."
                .into(),
            steps: vec![],
            primary_action: None,
            requires_confirm: true,
            blocked: true,
            priority: 10_000,
        })
    }

    pub fn match_input(&self, input: &RecommendInput<'_>) -> Option<ScenarioHit> {
        self.match_flags(&InputFlags::from_recommend(input))
    }
}

static BUILTIN: OnceLock<ScenarioStore> = OnceLock::new();

/// Resolve scenario dir: RELAY_SCENARIOS_DIR, else checkout/ops/scenarios, else bundled empty+warn.
pub fn scenarios_dir(checkout: Option<&Path>) -> PathBuf {
    if let Ok(p) = std::env::var("RELAY_SCENARIOS_DIR") {
        return PathBuf::from(p);
    }
    if let Some(c) = checkout {
        let p = c.join("ops/scenarios");
        if p.is_dir() {
            return p;
        }
    }
    // Relative to CWD when developing
    let cwd = PathBuf::from("ops/scenarios");
    if cwd.is_dir() {
        return cwd;
    }
    PathBuf::from("ops/scenarios")
}

pub fn load_default(checkout: Option<&Path>) -> ScenarioStore {
    let dir = scenarios_dir(checkout);
    match ScenarioStore::load_dir(&dir) {
        Ok(s) if !s.scenarios.is_empty() => s,
        Ok(_) | Err(_) => BUILTIN
            .get_or_init(|| ScenarioStore {
                scenarios: builtin_scenarios(),
                source: "builtin".into(),
            })
            .clone(),
    }
}

fn builtin_scenarios() -> Vec<Scenario> {
    // Minimal builtins if YAML missing (tests / broken checkout).
    vec![
        Scenario {
            id: "dns_mismatch_down".into(),
            title: "DNS mismatch while down".into(),
            kind: "ops".into(),
            priority: 10,
            fingerprint: Fingerprint {
                healthy: Some(false),
                dns_mismatch: Some(true),
                ..Default::default()
            },
            steps: vec!["point-dns".into()],
            message: "DNS A does not match the Spot IP — click Point DNS to Spot IP (do not Repair; SSH may hit the wrong box).".into(),
            requires_confirm: true,
            blocked: false,
            primary_action: Some("point-dns".into()),
        },
        Scenario {
            id: "unknown".into(),
            title: "Unknown".into(),
            kind: "ops".into(),
            priority: 9999,
            fingerprint: Fingerprint {
                unknown_down: Some(true),
                ..Default::default()
            },
            steps: vec![],
            message: "Node is down with no matching scenario — BLOCKED.".into(),
            requires_confirm: true,
            blocked: true,
            primary_action: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{RepoDrift, SyncAnomaly};
    use crate::recommend::RecommendInput;

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

    #[test]
    fn load_repo_scenarios_if_present() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ops/scenarios");
        if !dir.is_dir() {
            return;
        }
        let store = ScenarioStore::load_dir(&dir).unwrap();
        assert!(
            store.scenarios.len() >= 3,
            "expected seeded scenarios in {}",
            dir.display()
        );
    }

    #[test]
    fn replica_ahead_matches_parent() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ops/scenarios");
        let store = ScenarioStore::load_dir(&dir).unwrap_or_else(|_| ScenarioStore::empty());
        if store.scenarios.is_empty() {
            return;
        }
        let rows = [RepoDrift {
            repo: "clevertree-site".into(),
            branch: "release".into(),
            sha: Some("bbb".into()),
            expected_sha: "aaa".into(),
            kind: DriftKind::ReplicaAhead,
        }];
        let mut input = base(&[], &rows);
        input.sync_ok = false;
        let hit = store.match_input(&input).expect("hit");
        assert!(
            hit.id == "replica_ahead_of_publisher" || hit.blocked,
            "got {}",
            hit.id
        );
    }

    #[test]
    fn healthy_in_sync_is_silent() {
        let store = load_default(Some(Path::new(env!("CARGO_MANIFEST_DIR"))));
        let input = base(&[], &[]);
        assert!(store.match_input(&input).is_none());
    }

    #[test]
    fn catalog_missing_vs_tip_drift() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ops/scenarios");
        let store = ScenarioStore::load_dir(&dir).unwrap_or_else(|_| ScenarioStore::empty());
        if store.scenarios.is_empty() {
            return;
        }
        let missing = ["park".to_string()];
        let mut miss_input = base(&missing, &[]);
        miss_input.sync_ok = false;
        let miss = store.match_input(&miss_input).expect("hit");
        assert_eq!(miss.id, "catalog_missing");
        assert!(!miss.requires_confirm);

        let rows = [RepoDrift {
            repo: "clevertree-site".into(),
            branch: "release".into(),
            sha: Some("bbb".into()),
            expected_sha: "aaa".into(),
            kind: DriftKind::TipMismatch,
        }];
        let mut drift_input = base(&[], &rows);
        drift_input.sync_ok = false;
        let drift = store.match_input(&drift_input).expect("hit");
        assert_eq!(drift.id, "catalog_sync");
        assert!(drift.requires_confirm);
    }

    #[test]
    fn publisher_anomaly_scenario() {
        let store = load_default(Some(Path::new(env!("CARGO_MANIFEST_DIR"))));
        let anomalies = [SyncAnomaly {
            kind: "publisher_behind_replica".into(),
            repo: "clevertree-site".into(),
            branch: "release".into(),
            publisher_fqdn: "iad1.example".into(),
            publisher_sha: "aaa".into(),
            replica_fqdn: "sjc1.example".into(),
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
        let hit = store.match_input(&input).expect("hit");
        assert_eq!(hit.id, "publisher_behind_replica");
        assert!(hit.primary_action.is_none());
    }
}

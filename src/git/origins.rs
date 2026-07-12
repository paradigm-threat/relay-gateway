//! Per-repo relay mesh origins from `.relay.yaml` `git.origins` + `notifyMode`.

use crate::mesh_role;
use crate::types::{GitConfig, GitOrigin, NotifyMode};

/// Ordered relay hosts eligible for tip-notify (`kind: relay`, `notify` not false).
pub fn relay_notify_hosts(git: &GitConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(ref origins) = git.origins {
        for o in origins {
            if !origin_is_relay_notify(o) {
                continue;
            }
            if let Some(h) = origin_host_allowed(o) {
                out.push(h);
            }
        }
    }
    if out.is_empty() {
        out = fallback_peer_list();
    }
    out
}

fn origin_is_relay_notify(o: &GitOrigin) -> bool {
    let kind = o.kind.trim().to_ascii_lowercase();
    if kind != "relay" {
        return false;
    }
    o.notify != Some(false)
}

fn origin_host_allowed(o: &GitOrigin) -> Option<String> {
    let host = o.host.as_deref()?.trim();
    if host.is_empty() {
        return None;
    }
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    let list = std::env::var("RELAY_MASTER_PEER_LIST").unwrap_or_default();
    if list.trim().is_empty() || mesh_role::host_in_master_peer_list(&normalized, &list) {
        Some(normalized)
    } else {
        None
    }
}

fn master_peer_list_hosts() -> Vec<String> {
    let list = std::env::var("RELAY_MASTER_PEER_LIST").unwrap_or_default();
    list.split(';')
        .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn fallback_peer_list() -> Vec<String> {
    let me = mesh_role::node_fqdn_normalized();
    master_peer_list_hosts()
        .into_iter()
        .filter(|h| me.as_deref() != Some(h.as_str()))
        .collect()
}

/// Full ordered ring (includes self) for next-hop / hop-forward traversal.
pub fn relay_ring_hosts(git: &GitConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(ref origins) = git.origins {
        for o in origins {
            if !origin_is_relay_notify(o) {
                continue;
            }
            if let Some(h) = origin_host_allowed(o) {
                out.push(h);
            }
        }
    }
    if out.is_empty() {
        out = master_peer_list_hosts();
    }
    out
}

/// Targets for tip-notify after a validated push on this node.
pub fn tip_notify_targets(git: &GitConfig, self_fqdn: Option<&str>) -> Vec<String> {
    let hosts = relay_notify_hosts(git);
    let me = self_fqdn
        .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
        .or_else(mesh_role::node_fqdn_normalized);

    match git.notify_mode {
        NotifyMode::All => hosts
            .into_iter()
            .filter(|h| me.as_deref() != Some(h.as_str()))
            .collect(),
        NotifyMode::Next => {
            let Some(self_host) = me else {
                return hosts;
            };
            next_hop_after(&relay_ring_hosts(git), &self_host)
                .into_iter()
                .collect()
        }
    }
}

/// Single next peer after `self_host` in the ring (wrap).
pub fn next_hop_after(hosts: &[String], self_host: &str) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }
    let me = self_host.trim().to_ascii_lowercase();
    if let Some(pos) = hosts.iter().position(|h| h == &me) {
        let next = (pos + 1) % hosts.len();
        return Some(hosts[next].clone());
    }
    // Self not in ring: notify first hop (bootstrap).
    Some(hosts[0].clone())
}

/// Hop-forward after peer-fetch: next after `self_host`, respecting `via` loop guard.
pub fn hop_forward_target(
    git: &GitConfig,
    self_fqdn: Option<&str>,
    via: &[String],
    hops_remaining: Option<u32>,
) -> Option<(String, u32)> {
    let hops = hops_remaining.unwrap_or_else(|| {
        relay_ring_hosts(git)
            .len()
            .saturating_sub(1) as u32
    });
    if hops == 0 {
        return None;
    }
    let me = self_fqdn
        .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
        .or_else(mesh_role::node_fqdn_normalized)?;
    let hosts = relay_ring_hosts(git);
    let next = next_hop_after(&hosts, &me)?;
    let next_l = next.to_ascii_lowercase();
    if via.iter().any(|v| v.trim().eq_ignore_ascii_case(&next_l)) {
        return None;
    }
    Some((next, hops.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GitConfig, GitOrigin, NotifyMode};

    fn relay_origin(host: &str) -> GitOrigin {
        GitOrigin {
            host: Some(host.into()),
            kind: "relay".into(),
            ..Default::default()
        }
    }

    fn with_empty_peer_list<F: FnOnce()>(f: F) {
        let prev = std::env::var("RELAY_MASTER_PEER_LIST").ok();
        unsafe {
            std::env::set_var("RELAY_MASTER_PEER_LIST", "");
        }
        f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("RELAY_MASTER_PEER_LIST", v),
                None => std::env::remove_var("RELAY_MASTER_PEER_LIST"),
            }
        }
    }

    #[test]
    fn next_hop_wraps() {
        let h = vec![
            "a.example".to_string(),
            "b.example".to_string(),
            "c.example".to_string(),
        ];
        assert_eq!(next_hop_after(&h, "b.example").as_deref(), Some("c.example"));
        assert_eq!(next_hop_after(&h, "c.example").as_deref(), Some("a.example"));
    }

    #[test]
    fn next_hop_bootstrap_when_self_missing() {
        let h = vec!["a.example".into(), "b.example".into()];
        assert_eq!(next_hop_after(&h, "z.example").as_deref(), Some("a.example"));
    }

    #[test]
    #[serial_test::serial]
    fn tip_notify_next_excludes_self() {
        with_empty_peer_list(|| {
            let git = GitConfig {
                notify_mode: NotifyMode::Next,
                origins: Some(vec![
                    relay_origin("a.example"),
                    relay_origin("b.example"),
                    relay_origin("c.example"),
                ]),
                ..Default::default()
            };
            let targets = tip_notify_targets(&git, Some("b.example"));
            assert_eq!(targets, vec!["c.example".to_string()]);
        });
    }

    #[test]
    #[serial_test::serial]
    fn tip_notify_all_excludes_self() {
        with_empty_peer_list(|| {
            let git = GitConfig {
                notify_mode: NotifyMode::All,
                origins: Some(vec![relay_origin("a.example"), relay_origin("b.example")]),
                ..Default::default()
            };
            let targets = tip_notify_targets(&git, Some("a.example"));
            assert_eq!(targets, vec!["b.example".to_string()]);
        });
    }

    #[test]
    #[serial_test::serial]
    fn relay_notify_skips_notify_false_and_non_relay() {
        with_empty_peer_list(|| {
            let git = GitConfig {
                origins: Some(vec![
                    GitOrigin {
                        host: Some("a.example".into()),
                        kind: "relay".into(),
                        notify: Some(false),
                        ..Default::default()
                    },
                    GitOrigin {
                        host: Some("b.example".into()),
                        kind: "gitlab".into(),
                        ..Default::default()
                    },
                    relay_origin("c.example"),
                ]),
                ..Default::default()
            };
            assert_eq!(relay_notify_hosts(&git), vec!["c.example".to_string()]);
        });
    }

    #[test]
    #[serial_test::serial]
    fn relay_notify_filters_peer_list() {
        let prev = std::env::var("RELAY_MASTER_PEER_LIST").ok();
        unsafe {
            std::env::set_var("RELAY_MASTER_PEER_LIST", "a.example;c.example");
        }
        let git = GitConfig {
            origins: Some(vec![
                relay_origin("a.example"),
                relay_origin("b.example"),
                relay_origin("c.example"),
            ]),
            ..Default::default()
        };
        assert_eq!(
            relay_notify_hosts(&git),
            vec!["a.example".to_string(), "c.example".to_string()]
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var("RELAY_MASTER_PEER_LIST", v),
                None => std::env::remove_var("RELAY_MASTER_PEER_LIST"),
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn hop_forward_respects_via_loop_guard() {
        with_empty_peer_list(|| {
            let git = GitConfig {
                notify_mode: NotifyMode::Next,
                origins: Some(vec![relay_origin("a.example"), relay_origin("b.example")]),
                ..Default::default()
            };
            let via = vec!["b.example".into()];
            assert!(hop_forward_target(&git, Some("a.example"), &via, Some(2)).is_none());
        });
    }

    #[test]
    #[serial_test::serial]
    fn hop_forward_returns_next_and_decremented_hops() {
        with_empty_peer_list(|| {
            let git = GitConfig {
                notify_mode: NotifyMode::Next,
                origins: Some(vec![relay_origin("a.example"), relay_origin("b.example")]),
                ..Default::default()
            };
            let (next, hops) =
                hop_forward_target(&git, Some("a.example"), &[], Some(3)).expect("next hop");
            assert_eq!(next, "b.example");
            assert_eq!(hops, 2);
        });
    }

    #[test]
    #[serial_test::serial]
    fn hop_forward_uses_full_ring_when_self_excluded_from_notify_list() {
        let prev = std::env::var("RELAY_MASTER_PEER_LIST").ok();
        unsafe {
            std::env::set_var(
                "RELAY_MASTER_PEER_LIST",
                "dfw1.example;lon1.example;hkg1.example",
            );
            std::env::set_var("RELAY_PUBLIC_HOSTNAME", "lon1.example");
        }
        let git = GitConfig::default();
        let (next, hops) =
            hop_forward_target(&git, Some("lon1.example"), &[], Some(2)).expect("next hop");
        assert_eq!(next, "hkg1.example");
        assert_eq!(hops, 1);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("RELAY_MASTER_PEER_LIST", v),
                None => std::env::remove_var("RELAY_MASTER_PEER_LIST"),
            }
            std::env::remove_var("RELAY_PUBLIC_HOSTNAME");
        }
    }

    #[test]
    #[serial_test::serial]
    fn hop_forward_zero_hops_returns_none() {
        with_empty_peer_list(|| {
            let git = GitConfig {
                origins: Some(vec![relay_origin("a.example")]),
                ..Default::default()
            };
            assert!(hop_forward_target(&git, Some("a.example"), &[], Some(0)).is_none());
        });
    }
}

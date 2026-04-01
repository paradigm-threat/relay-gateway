//! Master vs **edge** (non-master) role for mesh control-plane behavior.
//!
//! See **`docs/RELAY_MESH_NETWORKING_PLAN.md`**. **`RELAY_NODE_ROLE`** overrides inference from
//! **`RELAY_PUBLIC_HOSTNAME`** + **`RELAY_MASTER_PEER_LIST`**.

/// This node is a **designated master** (may fan out mesh HTTP events; stricter inbound secrets when configured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayNodeRole {
    Master,
    Edge,
}

fn normalize_fqdn(raw: &str) -> Option<String> {
    let s = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    (!s.is_empty()).then_some(s)
}

/// **`RELAY_PUBLIC_HOSTNAME`** or **`RELAY_PUBLIC_FQDN`** (trimmed, lowercased).
pub fn node_fqdn_normalized() -> Option<String> {
    std::env::var("RELAY_PUBLIC_HOSTNAME")
        .or_else(|_| std::env::var("RELAY_PUBLIC_FQDN"))
        .ok()
        .and_then(|s| normalize_fqdn(&s))
}

/// **`true`** if `host` (already normalized lowercase) appears in **`RELAY_MASTER_PEER_LIST`**.
pub fn host_in_master_peer_list(host: &str, list_csv: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    for p in list_csv.split(';') {
        let h = p.trim().trim_end_matches('.').to_ascii_lowercase();
        if !h.is_empty() && h == host {
            return true;
        }
    }
    false
}

/// Master vs edge: explicit **`RELAY_NODE_ROLE`**, else infer from peer list + public hostname.
///
/// - **`RELAY_MASTER_PEER_LIST`** empty → **Master** (backward compatible: lone / dev node).
/// - Non-empty list + this host in list → **Master**.
/// - Non-empty list + host not in list (or hostname unset) → **Edge**.
pub fn relay_node_role() -> RelayNodeRole {
    if let Ok(r) = std::env::var("RELAY_NODE_ROLE") {
        match r.trim().to_ascii_lowercase().as_str() {
            "master" => return RelayNodeRole::Master,
            "edge" => return RelayNodeRole::Edge,
            _ => {}
        }
    }

    let raw_list = std::env::var("RELAY_MASTER_PEER_LIST").unwrap_or_default();
    if raw_list.trim().is_empty() {
        return RelayNodeRole::Master;
    }

    let Some(me) = node_fqdn_normalized() else {
        return RelayNodeRole::Edge;
    };

    if host_in_master_peer_list(&me, &raw_list) {
        RelayNodeRole::Master
    } else {
        RelayNodeRole::Edge
    }
}

/// Only **masters** fan out **`POST /hooks/mesh-event`** to peers (one-way control plane).
pub fn should_publish_mesh_http_events() -> bool {
    relay_node_role() == RelayNodeRole::Master
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_in_list_case_insensitive() {
        assert!(host_in_master_peer_list(
            "jp-osa.relaygateway.net",
            "Atlanta3.Relaygateway.Net;jp-osa.relaygateway.net"
        ));
        assert!(!host_in_master_peer_list(
            "edge1.example.com",
            "a.example.com;b.example.com"
        ));
    }
}

//! Authenticated peer fan-out for challenges and certificates.

use crate::peer_hook::peer_secret_ok;
use crate::types::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParkTlsBody {
    Challenge {
        token: String,
        key_authorization: String,
    },
    Cert {
        domain: String,
        fullchain_pem: String,
        privkey_pem: String,
    },
}

#[derive(Serialize)]
struct ParkTlsResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn mesh_secret() -> Option<String> {
    std::env::var("RELAY_MASTER_MESH_SECRET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("RELAY_PEER_SECRET")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

pub fn authorize_park_tls_peer(headers: &HeaderMap) -> bool {
    let Some(secret) = mesh_secret() else {
        return false;
    };
    let provided = headers
        .get("x-relay-peer-secret")
        .and_then(|v| v.to_str().ok());
    peer_secret_ok(provided, &secret)
}

/// `POST /hooks/park-tls` — install challenge token or cert/key from a peer.
pub async fn post_park_tls_hook(
    State(_state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ParkTlsBody>,
) -> impl IntoResponse {
    if mesh_secret().is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ParkTlsResponse {
                ok: false,
                error: Some("park-tls peer hook disabled (set RELAY_MASTER_MESH_SECRET)".into()),
            }),
        )
            .into_response();
    }
    if !authorize_park_tls_peer(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ParkTlsResponse {
                ok: false,
                error: Some("invalid X-Relay-Peer-Secret".into()),
            }),
        )
            .into_response();
    }
    let result = match body {
        ParkTlsBody::Challenge {
            token,
            key_authorization,
        } => {
            if token.is_empty() || token.contains('/') || token.contains("..") {
                Err("invalid token".into())
            } else {
                super::store::persist_challenge(&token, &key_authorization)
                    .map_err(|e| e.to_string())
            }
        }
        ParkTlsBody::Cert {
            domain,
            fullchain_pem,
            privkey_pem,
        } => {
            let domain = crate::catalog_policy::normalize_domain(&domain);
            if domain.is_empty() || !domain.contains('.') {
                Err("invalid domain".into())
            } else {
                super::store::persist_cert_pair(&domain, &fullchain_pem, &privkey_pem)
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        // Always regenerate from *all* cert dirs — a one-domain map
                        // used to wipe every other parked site (incl. flashback chat).
                        let map = std::collections::HashMap::new();
                        super::caddy::write_parked_caddyfile(&map).map_err(|e| e.to_string())?;
                        super::caddy::reload_caddy();
                        Ok(())
                    })
            }
        }
    };
    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(ParkTlsResponse {
                ok: true,
                error: None,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ParkTlsResponse {
                ok: false,
                error: Some(e),
            }),
        )
            .into_response(),
    }
}

fn peer_hosts(state: &AppState) -> Vec<String> {
    let raw = std::env::var("RELAY_MASTER_PEER_LIST")
        .or_else(|_| std::env::var("RELAY_OPERATOR_FLEET"))
        .unwrap_or_default();
    let self_fqdn = state
        .node_fqdn
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    raw.split(|c| c == ';' || c == ',')
        .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty() && *s != self_fqdn)
        .collect()
}

fn post_to_peers(state: &AppState, body: &ParkTlsBody) {
    let Some(secret) = mesh_secret() else {
        tracing::debug!("park_tls fanout skipped: no mesh secret");
        return;
    };
    let peers = peer_hosts(state);
    if peers.is_empty() {
        return;
    }
    let body_json = match serde_json::to_vec(body) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "park_tls fanout serialize failed");
            return;
        }
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "park_tls fanout client failed");
            return;
        }
    };
    for host in peers {
        let url = format!("https://{host}/hooks/park-tls");
        match client
            .post(&url)
            .header("X-Relay-Peer-Secret", &secret)
            .header("Content-Type", "application/json")
            .body(body_json.clone())
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(%host, "park_tls fanout ok");
            }
            Ok(resp) => {
                tracing::warn!(%host, status = %resp.status(), "park_tls fanout rejected");
            }
            Err(e) => tracing::warn!(%host, error = %e, "park_tls fanout failed"),
        }
    }
}

pub fn spawn_fanout_challenge(state: &AppState, token: &str, key_authorization: &str) {
    let state = state.clone();
    let body = ParkTlsBody::Challenge {
        token: token.to_string(),
        key_authorization: key_authorization.to_string(),
    };
    std::thread::Builder::new()
        .name("park-tls-fanout-chal".into())
        .spawn(move || post_to_peers(&state, &body))
        .ok();
}

pub fn spawn_fanout_cert(state: &AppState, domain: &str, fullchain_pem: &str, privkey_pem: &str) {
    let state = state.clone();
    let body = ParkTlsBody::Cert {
        domain: domain.to_string(),
        fullchain_pem: fullchain_pem.to_string(),
        privkey_pem: privkey_pem.to_string(),
    };
    std::thread::Builder::new()
        .name("park-tls-fanout-cert".into())
        .spawn(move || {
            post_to_peers(&state, &body);
            super::caddy::reload_caddy();
        })
        .ok();
}

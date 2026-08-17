use crate::authorized_repos;
use crate::cli::{Cli, Commands};
use crate::types::AppState;
use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

pub struct Config {
    pub state: AppState,
    pub http_addr: SocketAddr,
    pub https_port: u16,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub acme_dir: String,
}

impl Config {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let (repo_path, mut static_paths, bind_cli): (PathBuf, Vec<PathBuf>, Option<String>) =
            match &cli.command {
                Some(Commands::Serve(sa)) => {
                    let rp = sa
                        .repo
                        .clone()
                        .or_else(|| std::env::var("RELAY_REPO_PATH").ok().map(PathBuf::from))
                        .unwrap_or_else(|| PathBuf::from("data"));
                    (rp, sa.static_paths.clone(), sa.bind.clone())
                }
                _ => {
                    let rp = std::env::var("RELAY_REPO_PATH")
                        .map(PathBuf::from)
                        .unwrap_or_else(|_| PathBuf::from("data"));
                    (rp, Vec::new(), None)
                }
            };

        if let Ok(extra) = std::env::var("RELAY_STATIC_DIR") {
            for p in extra.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                static_paths.push(PathBuf::from(p));
            }
        }

        let http_addr: SocketAddr =
            if let Some(bind) = bind_cli.or_else(|| std::env::var("RELAY_BIND").ok()) {
                SocketAddr::from_str(&bind)?
            } else {
                let port = std::env::var("RELAY_HTTP_PORT")
                    .ok()
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(80);
                SocketAddr::from_str(&format!("0.0.0.0:{}", port))?
            };

        let https_port = std::env::var("RELAY_HTTPS_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(443);
        let tls_cert = std::env::var("RELAY_TLS_CERT").ok();
        let tls_key = std::env::var("RELAY_TLS_KEY").ok();
        let acme_dir =
            std::env::var("RELAY_ACME_DIR").unwrap_or_else(|_| "/var/www/certbot".to_string());

        let node_fqdn = std::env::var("RELAY_PUBLIC_HOSTNAME")
            .or_else(|_| std::env::var("RELAY_NODE_FQDN"))
            .ok()
            .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty());

        let relay_server_id = std::env::var("RELAY_SERVER_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let catalog_path = std::env::var("RELAY_MASTER_CATALOG_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| PathBuf::from(s.trim()));

        let yaml_path = std::env::var("RELAY_AUTHORIZED_REPOS_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| PathBuf::from(s.trim()));

        let authorized_repos: Option<authorized_repos::AuthorizedReposFile> = match (
            &catalog_path,
            &yaml_path,
        ) {
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "set only one of RELAY_MASTER_CATALOG_PATH or RELAY_AUTHORIZED_REPOS_PATH, not both"
                );
            }
            (Some(c), None) => {
                let id = relay_server_id.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "RELAY_SERVER_ID is required when RELAY_MASTER_CATALOG_PATH is set"
                    )
                })?;
                if let Ok(t) = std::env::var("RELAY_MASTER_TRUST_ROOT") {
                    let t = t.trim();
                    if !t.is_empty() {
                        authorized_repos::verify_master_catalog_trust_root(c.as_path(), t)?;
                    }
                }
                Some(authorized_repos::load_from_master_catalog(c.as_path(), id)?)
            }
            (None, Some(p)) => Some(authorized_repos::load_from_path(p.as_path())?),
            (None, None) => None,
        };

        if authorized_repos.is_some() && relay_server_id.is_none() {
            anyhow::bail!(
                "RELAY_SERVER_ID is required when an authorized repo allowlist is configured (YAML or catalog)"
            );
        }
        if let Some(ref ar) = authorized_repos {
            let fid = ar
                .relay_server_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                    "authorized-repos: relay_server_id missing (YAML file or internal catalog load)"
                )
                })?;
            if relay_server_id.as_deref() != Some(fid) {
                anyhow::bail!(
                    "RELAY_SERVER_ID '{}' must match relay_server_id in policy ('{}')",
                    relay_server_id.as_deref().unwrap_or(""),
                    fid
                );
            }
        }

        let authorized_repos = authorized_repos.map(Arc::new);

        // Parked-domain routing: worktree catalog → bare relay-master tip → state map file.
        let parked_domains =
            crate::catalog_policy::resolve_parked_domains(&repo_path, catalog_path.as_deref())?
                .map(Arc::new);

        let features_path = std::env::var("RELAY_FEATURES_STATE_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let root =
                    std::env::var("RELAY_INSTALL_ROOT").unwrap_or_else(|_| "/opt/relay".into());
                let p = Path::new(&root).join("state/features.json");
                p.exists().then_some(p)
            });
        let features_manifest = features_path
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(Arc::new);

        let git_ensure_bare_enabled = env_truthy_or_default("RELAY_GIT_ENSURE_BARE_ENABLED", true);

        let web_assets_repo = std::env::var("RELAY_WEB_ASSETS_REPO")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let web_assets_branch = std::env::var("RELAY_WEB_ASSETS_BRANCH")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "release".to_string());

        Ok(Config {
            state: AppState {
                repo_path,
                serve_context: crate::types::ServeContext::BareFleet,
                static_paths,
                node_fqdn,
                relay_server_id,
                authorized_repos,
                parked_domains,
                features_manifest,
                git_ensure_bare_enabled,
                web_assets_repo,
                web_assets_branch,
            },
            http_addr,
            https_port,
            tls_cert,
            tls_key,
            acme_dir,
        })
    }

    /// App state for desktop `--repo` embedded server (live worktree GET).
    pub fn desktop_local_state(checkout: PathBuf, repo_name: String) -> AppState {
        let checked_out_branch = crate::git::worktree::checked_out_branch(&checkout)
            .unwrap_or_else(|| crate::types::DEFAULT_BRANCH.to_string());
        AppState {
            repo_path: checkout.clone(),
            serve_context: crate::types::ServeContext::LocalWorktree {
                checkout,
                repo_name,
                checked_out_branch,
            },
            static_paths: Vec::new(),
            node_fqdn: Some("localhost".to_string()),
            relay_server_id: None,
            authorized_repos: None,
            parked_domains: None,
            features_manifest: None,
            git_ensure_bare_enabled: false,
            web_assets_repo: None,
            web_assets_branch: "release".to_string(),
        }
    }

    pub fn initialize_repos(&self) {
        let repo_path = &self.state.repo_path;
        let _ = std::fs::create_dir_all(repo_path);
        // Repo cloning and updates are handled by the docker-entrypoint.sh
        // or external processes to keep the server lightweight.
    }
}

/// Env unset/empty → `default`. `0`, `false`, `no` (case-insensitive) → false; otherwise true.
fn env_truthy_or_default(var: &str, default: bool) -> bool {
    match std::env::var(var) {
        Ok(s) => {
            let t = s.trim();
            if t.is_empty() {
                return default;
            }
            let low = t.to_ascii_lowercase();
            !matches!(low.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => default,
    }
}

pub async fn load_rustls_config(cert_path: &str, key_path: &str) -> Result<RustlsConfig> {
    let cert_bytes = tokio::fs::read(cert_path).await?;
    let key_bytes = tokio::fs::read(key_path).await?;
    let config = RustlsConfig::from_pem(cert_bytes, key_bytes).await?;
    Ok(config)
}

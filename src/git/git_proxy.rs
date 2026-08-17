//! Public `git://` front: provision a missing bare on first `receive-pack`, then proxy to
//! loopback `git daemon`.
//!
//! Stock `git daemon` checks that `{repo}.git` exists **before** `--access-hook`, so a hook
//! cannot create the path. This process binds the public git port, creates the bare (same
//! rules as `POST /api/git/ensure-bare/:repo` plus the `publish_origin` fence), then forwards
//! the original pkt-line and the rest of the TCP stream.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::git::branch_policy::enforce_publish_origin_fence;
use crate::git::repo::ensure_bare_repo;
use crate::peer_hook::validate_repo_slug;

const PKT_LINE_MAX: usize = 65524;
const FIRST_PKT_TIMEOUT: Duration = Duration::from_secs(15);
const BACKEND_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitService {
    ReceivePack,
    UploadPack,
    UploadArchive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDaemonRequest {
    pub service: GitService,
    pub slug: String,
    pub host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionDecision {
    /// Repo already exists, or this is fetch/archive of a missing path (let git daemon answer).
    Proxy,
    /// Missing dest, `receive-pack`, policy allows create.
    CreateThenProxy,
    /// Send a git `ERR` pkt-line and close (do not create).
    Reject(String),
}

#[derive(Debug, Clone)]
pub struct GitProxyConfig {
    pub listen: SocketAddr,
    pub backend: SocketAddr,
    pub repo_path: PathBuf,
    pub ensure_bare_enabled: bool,
    pub public_hostname: Option<String>,
    pub max_connections: usize,
}

impl GitProxyConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let git_port: u16 = std::env::var("RELAY_GIT_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(9418);
        let listen: SocketAddr = std::env::var("RELAY_GIT_BIND")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], git_port)));
        let backend: SocketAddr = std::env::var("RELAY_GIT_BACKEND")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or_else(|| {
                let p: u16 = std::env::var("RELAY_GIT_DAEMON_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(9419);
                SocketAddr::from(([127, 0, 0, 1], p))
            });
        let repo_path = std::env::var("RELAY_REPO_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/opt/relay/data"));
        let public_hostname = std::env::var("RELAY_PUBLIC_HOSTNAME")
            .or_else(|_| std::env::var("RELAY_NODE_FQDN"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Self {
            listen,
            backend,
            repo_path,
            ensure_bare_enabled: env_truthy_or_default("RELAY_GIT_ENSURE_BARE_ENABLED", true),
            public_hostname,
            max_connections: 64,
        })
    }
}

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

/// Encode a git pkt-line (4 hex length bytes including the header).
pub fn encode_pkt_line(payload: &[u8]) -> Vec<u8> {
    let n = payload.len() + 4;
    let mut out = format!("{n:04x}").into_bytes();
    out.extend_from_slice(payload);
    out
}

pub fn encode_err_packet(message: &str) -> Vec<u8> {
    let mut payload = Vec::from("ERR ");
    payload.extend_from_slice(message.as_bytes());
    if !message.ends_with('\n') {
        payload.push(b'\n');
    }
    encode_pkt_line(&payload)
}

/// Parse the first git-daemon request pkt-line **payload** (no 4-byte length prefix).
pub fn parse_git_daemon_request(payload: &[u8]) -> Result<GitDaemonRequest, String> {
    let (head, rest) = match payload.iter().position(|&b| b == 0) {
        Some(i) => (&payload[..i], &payload[i + 1..]),
        None => (payload, &[][..]),
    };
    let head = std::str::from_utf8(head)
        .map_err(|_| "git request is not UTF-8".to_string())?
        .trim_end_matches(['\n', '\r']);
    let mut parts = head.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("").trim();
    let service = match cmd {
        "git-receive-pack" => GitService::ReceivePack,
        "git-upload-pack" => GitService::UploadPack,
        "git-upload-archive" => GitService::UploadArchive,
        _ => return Err(format!("unsupported git service: {cmd}")),
    };
    if path.is_empty() {
        return Err("missing repository path".to_string());
    }
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Err("missing repository path".to_string());
    }
    if path.contains('/') {
        return Err("repository path must be a single slug".to_string());
    }
    let slug = validate_repo_slug(path).map_err(|e| e.to_string())?;
    let host = parse_host_param(rest);
    Ok(GitDaemonRequest {
        service,
        slug,
        host,
    })
}

fn parse_host_param(rest: &[u8]) -> Option<String> {
    let (first, _) = match rest.iter().position(|&b| b == 0) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &[][..]),
    };
    let s = std::str::from_utf8(first).ok()?.trim();
    s.strip_prefix("host=").map(|h| h.trim().to_string())
}

/// Decide whether to create `dest` before proxying. Does not create.
pub fn decide_first_push_provision(
    service: GitService,
    dest: &Path,
    ensure_bare_enabled: bool,
    public_hostname: Option<&str>,
) -> ProvisionDecision {
    if dest.exists() {
        return ProvisionDecision::Proxy;
    }
    match service {
        GitService::ReceivePack => {
            if !ensure_bare_enabled {
                return ProvisionDecision::Reject(
                    "RELAY_GIT_ENSURE_BARE_ENABLED is disabled; create the bare repo on the node \
(or POST /api/git/ensure-bare)"
                        .to_string(),
                );
            }
            if let Err(e) = enforce_publish_origin_fence(dest, public_hostname) {
                return ProvisionDecision::Reject(e);
            }
            ProvisionDecision::CreateThenProxy
        }
        GitService::UploadPack | GitService::UploadArchive => ProvisionDecision::Proxy,
    }
}

pub async fn serve(cfg: GitProxyConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(cfg.listen).await?;
    info!(
        listen = %cfg.listen,
        backend = %cfg.backend,
        repo_path = %cfg.repo_path.display(),
        "relay-git-proxy listening"
    );
    let max = cfg.max_connections.max(1);
    let sem = Arc::new(Semaphore::new(max));
    let cfg = Arc::new(cfg);
    loop {
        let (socket, peer) = listener.accept().await?;
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tokio::spawn(async move {
                    let _ = send_err(socket, "too many git connections").await;
                });
                continue;
            }
        };
        let cfg = cfg.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection(socket, &cfg).await {
                warn!(%peer, error = %e, "git proxy connection failed");
            }
        });
    }
}

pub async fn handle_connection(mut inbound: TcpStream, cfg: &GitProxyConfig) -> anyhow::Result<()> {
    let _ = inbound.set_nodelay(true);
    let first = match tokio::time::timeout(FIRST_PKT_TIMEOUT, read_pkt_line(&mut inbound)).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            let _ = send_err_on(&mut inbound, &e.to_string()).await;
            return Err(e);
        }
        Err(_) => {
            let _ = send_err_on(&mut inbound, "timeout waiting for git request").await;
            anyhow::bail!("timeout waiting for git request");
        }
    };
    if first.len() < 4 {
        let _ = send_err_on(&mut inbound, "empty git request").await;
        anyhow::bail!("empty git request");
    }
    let payload = &first[4..];
    let req = match parse_git_daemon_request(payload) {
        Ok(r) => r,
        Err(e) => {
            let _ = send_err_on(&mut inbound, &e).await;
            anyhow::bail!(e);
        }
    };
    let dest = cfg.repo_path.join(format!("{}.git", req.slug));
    match decide_first_push_provision(
        req.service,
        &dest,
        cfg.ensure_bare_enabled,
        cfg.public_hostname.as_deref(),
    ) {
        ProvisionDecision::Reject(msg) => {
            let _ = send_err_on(&mut inbound, &msg).await;
            anyhow::bail!(msg);
        }
        ProvisionDecision::Proxy => {}
        ProvisionDecision::CreateThenProxy => {
            let slug = req.slug.clone();
            let root = cfg.repo_path.clone();
            match tokio::task::spawn_blocking(move || ensure_bare_repo(&root, &slug)).await {
                Ok(Ok((created, path))) => {
                    if created {
                        info!(repo = %req.slug, path = %path.display(), "first-push provisioned bare repo");
                    }
                }
                Ok(Err(e)) => {
                    let _ = send_err_on(&mut inbound, &e).await;
                    anyhow::bail!(e);
                }
                Err(_) => {
                    let _ = send_err_on(&mut inbound, "internal provision task failed").await;
                    anyhow::bail!("internal provision task failed");
                }
            }
        }
    }

    let mut backend = match tokio::time::timeout(
        BACKEND_CONNECT_TIMEOUT,
        TcpStream::connect(cfg.backend),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let msg = format!("git daemon backend unavailable: {e}");
            let _ = send_err_on(&mut inbound, &msg).await;
            anyhow::bail!(msg);
        }
        Err(_) => {
            let _ = send_err_on(&mut inbound, "git daemon backend connect timeout").await;
            anyhow::bail!("git daemon backend connect timeout");
        }
    };
    let _ = backend.set_nodelay(true);
    backend.write_all(&first).await?;
    tokio::io::copy_bidirectional(&mut inbound, &mut backend).await?;
    Ok(())
}

async fn read_pkt_line(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len_s =
        std::str::from_utf8(&len_buf).map_err(|_| anyhow::anyhow!("pkt-line length is not hex"))?;
    let len = usize::from_str_radix(len_s, 16)
        .map_err(|_| anyhow::anyhow!("pkt-line length is not hex"))?;
    if len == 0 {
        anyhow::bail!("unexpected flush packet");
    }
    if len < 4 || len > PKT_LINE_MAX {
        anyhow::bail!("invalid pkt-line length {len}");
    }
    let mut packet = Vec::with_capacity(len);
    packet.extend_from_slice(&len_buf);
    packet.resize(len, 0);
    stream.read_exact(&mut packet[4..]).await?;
    Ok(packet)
}

async fn send_err(mut stream: TcpStream, message: &str) -> std::io::Result<()> {
    send_err_on(&mut stream, message).await
}

async fn send_err_on(stream: &mut TcpStream, message: &str) -> std::io::Result<()> {
    stream.write_all(&encode_err_packet(message)).await?;
    let _ = stream.flush().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;
    use tokio::net::TcpListener;

    #[test]
    fn parse_receive_pack_slug() {
        let req = parse_git_daemon_request(
            b"git-receive-pack /new-repo.git\0host=dfw1.relaygateway.net\0",
        )
        .unwrap();
        assert_eq!(req.service, GitService::ReceivePack);
        assert_eq!(req.slug, "new-repo");
        assert_eq!(req.host.as_deref(), Some("dfw1.relaygateway.net"));
    }

    #[test]
    fn parse_upload_pack_and_v2_extra() {
        let req =
            parse_git_daemon_request(b"git-upload-pack /foo.git\0host=example.com\0\0version=2\0")
                .unwrap();
        assert_eq!(req.service, GitService::UploadPack);
        assert_eq!(req.slug, "foo");
    }

    #[test]
    fn parse_rejects_nested_path_and_bad_slug() {
        assert!(parse_git_daemon_request(b"git-receive-pack /repos/foo.git\0").is_err());
        assert!(parse_git_daemon_request(b"git-receive-pack /../evil.git\0").is_err());
        assert!(parse_git_daemon_request(b"git-receive-pack /\0").is_err());
    }

    #[test]
    fn encode_err_has_pkt_header() {
        let pkt = encode_err_packet("nope");
        let n = usize::from_str_radix(std::str::from_utf8(&pkt[..4]).unwrap(), 16).unwrap();
        assert_eq!(n, pkt.len());
        assert!(pkt[4..].starts_with(b"ERR nope"));
    }

    #[test]
    fn decide_proxy_when_dest_exists() {
        let d = tempdir().unwrap();
        let dest = d.path().join("x.git");
        std::fs::create_dir(&dest).unwrap();
        assert_eq!(
            decide_first_push_provision(GitService::ReceivePack, &dest, false, None),
            ProvisionDecision::Proxy
        );
    }

    #[test]
    fn decide_upload_pack_missing_does_not_create() {
        let d = tempdir().unwrap();
        let dest = d.path().join("missing.git");
        assert_eq!(
            decide_first_push_provision(
                GitService::UploadPack,
                &dest,
                true,
                Some("dfw1.relaygateway.net")
            ),
            ProvisionDecision::Proxy
        );
        assert!(!dest.exists());
    }

    #[test]
    fn decide_receive_pack_disabled_rejects() {
        let d = tempdir().unwrap();
        let dest = d.path().join("missing.git");
        match decide_first_push_provision(GitService::ReceivePack, &dest, false, None) {
            ProvisionDecision::Reject(msg) => {
                assert!(msg.contains("RELAY_GIT_ENSURE_BARE_ENABLED"), "{msg}");
            }
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn decide_receive_pack_missing_creates_without_catalog() {
        let d = tempdir().unwrap();
        let dest = d.path().join("brand-new.git");
        assert_eq!(
            decide_first_push_provision(
                GitService::ReceivePack,
                &dest,
                true,
                Some("dfw1.relaygateway.net"),
            ),
            ProvisionDecision::CreateThenProxy
        );
    }

    struct CatalogPathGuard {
        prev: Option<String>,
    }
    impl CatalogPathGuard {
        fn set(path: &Path) -> Self {
            let prev = std::env::var("RELAY_MASTER_CATALOG_PATH").ok();
            unsafe {
                std::env::set_var("RELAY_MASTER_CATALOG_PATH", path);
            }
            Self { prev }
        }
    }
    impl Drop for CatalogPathGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("RELAY_MASTER_CATALOG_PATH", v),
                    None => std::env::remove_var("RELAY_MASTER_CATALOG_PATH"),
                }
            }
        }
    }

    fn catalog_worktree(repo: &str, origin: Option<&str>) -> tempfile::TempDir {
        let d = tempdir().unwrap();
        let repos = d.path().join("catalog/repos");
        std::fs::create_dir_all(&repos).unwrap();
        let origin_json = match origin {
            Some(o) => format!(r#""publish_origin": "{o}","#),
            None => String::new(),
        };
        std::fs::write(
            repos.join(format!("{repo}.json")),
            format!(
                r#"{{
                  "repo": "{repo}",
                  "tier": "hosted",
                  "relay_root_commit": "{}",
                  "publish_branch": "main",
                  {origin_json}
                  "signing_keys_source": "repo"
                }}"#,
                "a".repeat(40)
            ),
        )
        .unwrap();
        d
    }

    #[test]
    #[serial(catalog_path_env)]
    fn decide_receive_pack_replica_does_not_create() {
        let catalog = catalog_worktree("files", Some("dfw1.relaygateway.net"));
        let _guard = CatalogPathGuard::set(catalog.path());
        let d = tempdir().unwrap();
        let dest = d.path().join("files.git");
        match decide_first_push_provision(
            GitService::ReceivePack,
            &dest,
            true,
            Some("lon1.relaygateway.net"),
        ) {
            ProvisionDecision::Reject(msg) => {
                assert!(msg.contains("publish_origin"), "{msg}");
                assert!(msg.contains("dfw1.relaygateway.net"), "{msg}");
            }
            other => panic!("expected reject, got {other:?}"),
        }
        assert!(!dest.exists());
    }

    #[test]
    #[serial(catalog_path_env)]
    fn decide_receive_pack_origin_creates() {
        let catalog = catalog_worktree("files", Some("dfw1.relaygateway.net"));
        let _guard = CatalogPathGuard::set(catalog.path());
        let d = tempdir().unwrap();
        let dest = d.path().join("files.git");
        assert_eq!(
            decide_first_push_provision(
                GitService::ReceivePack,
                &dest,
                true,
                Some("dfw1.relaygateway.net"),
            ),
            ProvisionDecision::CreateThenProxy
        );
    }

    #[tokio::test]
    async fn handle_connection_provisions_then_proxies() {
        let repos = tempdir().unwrap();
        let backend_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_l.local_addr().unwrap();
        let proxy_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_l.local_addr().unwrap();
        let cfg = GitProxyConfig {
            listen: proxy_addr,
            backend: backend_addr,
            repo_path: repos.path().to_path_buf(),
            ensure_bare_enabled: true,
            public_hostname: Some("dfw1.relaygateway.net".into()),
            max_connections: 4,
        };

        let backend = tokio::spawn(async move {
            let (mut s, _) = backend_l.accept().await.unwrap();
            let mut buf = vec![0u8; 256];
            let n = s.read(&mut buf).await.unwrap();
            s.write_all(b"ok").await.unwrap();
            buf[..n].to_vec()
        });

        let server = tokio::spawn(async move {
            let (inbound, _) = proxy_l.accept().await.unwrap();
            handle_connection(inbound, &cfg).await
        });

        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let payload = b"git-receive-pack /first-push-demo.git\0host=dfw1.relaygateway.net\0";
        client.write_all(&encode_pkt_line(payload)).await.unwrap();
        client.shutdown().await.unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, b"ok");

        let seen = backend.await.unwrap();
        assert!(
            seen.windows(payload.len()).any(|w| w == payload),
            "backend should see original request"
        );
        server.await.unwrap().unwrap();
        assert!(repos.path().join("first-push-demo.git").exists());
        let bare = git2::Repository::open_bare(repos.path().join("first-push-demo.git")).unwrap();
        assert_eq!(
            bare.config()
                .unwrap()
                .get_string("receive.denyNonFastForwards")
                .unwrap(),
            "true"
        );
    }

    #[tokio::test]
    async fn handle_connection_upload_pack_does_not_create() {
        let repos = tempdir().unwrap();
        let backend_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend_l.local_addr().unwrap();
        let proxy_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_l.local_addr().unwrap();
        let cfg = GitProxyConfig {
            listen: proxy_addr,
            backend: backend_addr,
            repo_path: repos.path().to_path_buf(),
            ensure_bare_enabled: true,
            public_hostname: Some("dfw1.relaygateway.net".into()),
            max_connections: 4,
        };

        let backend = tokio::spawn(async move {
            let (mut s, _) = backend_l.accept().await.unwrap();
            let mut buf = vec![0u8; 256];
            let _ = s.read(&mut buf).await.unwrap();
            s.write_all(b"ok").await.unwrap();
        });

        let server = tokio::spawn(async move {
            let (inbound, _) = proxy_l.accept().await.unwrap();
            handle_connection(inbound, &cfg).await
        });

        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let payload = b"git-upload-pack /ghost.git\0host=dfw1.relaygateway.net\0";
        client.write_all(&encode_pkt_line(payload)).await.unwrap();
        client.shutdown().await.unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, b"ok");
        backend.await.unwrap();
        server.await.unwrap().unwrap();
        assert!(!repos.path().join("ghost.git").exists());
    }
}

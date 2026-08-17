//! Public `git://` listener: first-push provision, then proxy to loopback `git daemon`.

use anyhow::Result;
use tracing::info;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

use relay_server::git::git_proxy::{serve, GitProxyConfig};

#[tokio::main]
async fn main() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_target(true).compact())
        .init();
    relay_server::notify::install_panic_hook();

    let cfg = GitProxyConfig::from_env()?;
    info!(
        listen = %cfg.listen,
        backend = %cfg.backend,
        ensure_bare = cfg.ensure_bare_enabled,
        "starting relay-git-proxy"
    );
    serve(cfg).await
}

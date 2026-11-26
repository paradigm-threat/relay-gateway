//! `relay-brain` — local fleet brain binary (shared modules with relay-ops).

#[path = "../relay-ops/brain.rs"]
mod brain;
#[path = "../relay-ops/browse.rs"]
mod browse;
#[path = "../relay-ops/config.rs"]
mod config;
#[path = "../relay-ops/fleet_cmd.rs"]
mod fleet_cmd;
#[path = "../relay-ops/gitlab.rs"]
mod gitlab;
#[path = "../relay-ops/inventory.rs"]
mod inventory;
#[path = "../relay-ops/irc_bus.rs"]
mod irc_bus;
#[path = "../relay-ops/notify.rs"]
mod notify;
#[path = "../relay-ops/probe.rs"]
mod probe;
#[path = "../relay-ops/recommend.rs"]
mod recommend;
#[path = "../relay-ops/scenarios.rs"]
mod scenarios;
#[path = "../relay-ops/spot.rs"]
mod spot;
#[path = "../relay-ops/ssh.rs"]
mod ssh;
#[path = "../relay-ops/topology.rs"]
mod topology;
#[path = "../relay-ops/vercel.rs"]
mod vercel;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "relay-brain",
    about = "Local Relay fleet brain (probe + scenarios + private feed)"
)]
struct Cli {
    /// Allow auto-run of non-confirm scenarios.
    #[arg(long)]
    execute: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
enum Cmd {
    Serve {
        #[arg(long)]
        execute: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let execute = match cli.cmd {
        Some(Cmd::Serve { execute }) => execute || cli.execute,
        None => cli.execute,
    };
    brain::run_serve(execute).await
}

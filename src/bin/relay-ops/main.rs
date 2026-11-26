mod browse;
mod brain;
mod brain_client;
mod config;
mod credit;
mod fleet_cmd;
mod gitlab;
mod inventory;
mod irc_bus;
mod jobs;
mod mail;
mod notify;
mod probe;
mod recommend;
mod scenarios;
mod serve;
mod ssh;
mod topology;
#[cfg(unix)]
mod tray;
mod vercel;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use config::OpsConfig;
use inventory::Inventory;
use probe::probe_fleet;
use serve::{App, Inner};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use vercel::Vercel;

#[derive(Parser)]
#[command(
    name = "relay-ops",
    about = "Generic Relay fleet CLI and local UI. Bootstrap from a GitLab project URL (ops/fleet.json)."
)]
struct Cli {
    /// GitLab project URL (https://… or git@…). Default: config, then git remote on the checkout.
    #[arg(long, global = true)]
    gitlab: Option<String>,
    /// Load env from PATH (same as RELAY_FLEET_ENV_FILE / ./relay.env).
    #[arg(long, global = true)]
    env_file: Option<PathBuf>,
    /// Limit to one fleet FQDN.
    #[arg(long, global = true)]
    only: Option<String>,
    /// With update/deploy-all: pack the deploy tarball first.
    #[arg(long, global = true)]
    build: bool,
    /// With update/deploy-all --build: allow packing a dirty/unpushed tree.
    #[arg(long, global = true)]
    allow_dirty: bool,
    /// Allow SSH password prompts (omit BatchMode).
    #[arg(long, global = true)]
    interactive: bool,
    /// Destructive reinstall: wipe bare repos (must also pass --confirm-wipe and --only).
    #[arg(long, global = true)]
    danger_wipe_relay_data: bool,
    /// Destructive reinstall: must match --only host.
    #[arg(long, global = true)]
    confirm_wipe: Option<String>,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print fleet hosts, SSH user, identity path.
    Hosts,
    /// Parallel SSH probe (fails if any host fails).
    #[command(name = "ssh-check")]
    SshCheck,
    /// Fetch topology and probe public DNS / HTTPS / git://.
    Status,
    /// QUERY (+ optional MCP) smoke + SSL report per host.
    Health {
        #[arg(long)]
        mcp: bool,
    },
    /// Parked-domain + node HTTPS report.
    Ssl,
    /// Compare catalog git HEAD across the fleet.
    Drift,
    /// Catalog + peer fetch on a node (no wipe, no force-push).
    Sync {
        /// Healthy peer FQDN to fetch from after startup catalog sync.
        #[arg(long)]
        peer: Option<String>,
    },
    /// Redeploy packed tarball per host (relay-linode-redeploy-seed).
    Update {
        #[arg(long)]
        build: bool,
    },
    /// Same as update.
    #[command(name = "deploy-all")]
    DeployAll {
        #[arg(long)]
        build: bool,
    },
    /// Remote: sudo install.sh repair (existing node).
    Repair,
    /// Remote install.sh reconfigure-features (non-interactive).
    #[command(name = "features-set")]
    FeaturesSet {
        /// Piper TTS (`/voices`, `/tts`). Default **1** (on for all fleet nodes). Pass `0` to disable.
        #[arg(long)]
        piper: Option<String>,
        #[arg(long)]
        npm_pkgs: Option<String>,
        #[arg(long)]
        translation: Option<String>,
        #[arg(long)]
        translation_pkgs: Option<String>,
        #[arg(long)]
        libretranslate: Option<String>,
        /// `1` / `0` — receive-only OpenSMTPD catch-all (see docs/MAIL_INBOUND.md).
        #[arg(long)]
        inbound_mail: Option<String>,
        #[arg(long)]
        mail_domains: Option<String>,
        #[arg(long)]
        mail_user: Option<String>,
        #[arg(long)]
        mail_hostname: Option<String>,
    },
    /// Remote bare reset + catalog sync.
    #[command(name = "reclone-repo")]
    RecloneRepo {
        repo: String,
        #[arg(long)]
        peer: Option<String>,
    },
    /// Point each catalog bare repo symbolic HEAD at its .gitmodules branch.
    #[command(name = "fix-bare-head")]
    FixBareHead,
    /// Chown `/opt/relay/data/*.git` back to relay:relay (fixes dubious-ownership warm failures).
    #[command(name = "fix-repo-ownership")]
    FixRepoOwnership,
    /// Install deploy pubkey via root+sshpass (SSHPASS or RELAY_ROOT_PASSWORD).
    #[command(name = "bootstrap-ssh")]
    BootstrapSsh,
    /// Reconcile fleet ingress A records from node FQDNs (Vercel DNS).
    #[command(name = "fleet-dns")]
    FleetDns {
        /// Keep only IPs of currently healthy nodes.
        #[arg(long)]
        drop_unhealthy: bool,
        #[arg(long)]
        dry_run: bool,
        /// Required with --drop-unhealthy when applying.
        #[arg(long)]
        yes: bool,
        /// Override ingress FQDN (default: fleetIngress in topology).
        #[arg(long)]
        fleet: Option<String>,
    },
    /// Safe default = update. Destructive wipe: --danger-wipe-relay-data --confirm-wipe=FQDN --only=FQDN.
    Reinstall,
    /// Print local admin inventory (no live SSH).
    Inventory,
    /// Local UI on 127.0.0.1 (default :7780).
    Serve {
        #[arg(long)]
        bind: Option<String>,
    },
    /// Point a node FQDN A record at an IPv4.
    #[command(name = "point-dns")]
    PointDns {
        #[arg(long)]
        fqdn: String,
        #[arg(long)]
        ip: String,
    },
    /// ssh-keygen -R on ~/.ssh/known_hosts for fqdn and optional IPs (no sudo).
    #[command(name = "known-hosts-reset")]
    KnownHostsReset {
        #[arg(long)]
        fqdn: String,
        /// Repeatable. DNS A / node IPv4 for the same host.
        #[arg(long)]
        ip: Vec<String>,
    },
    /// Install login daemon (systemd --user or Windows Startup).
    #[command(name = "install-service")]
    InstallService,
    /// Brain subcommands (disabled stub — prints disabled).
    Brain {
        #[command(subcommand)]
        cmd: BrainCmd,
    },
}

#[derive(Subcommand)]
enum BrainCmd {
    /// Run brain probe loop + loopback HTTP (default :7781).
    Serve {
        /// Allow auto-run of non-confirm scenarios (still opens requests for high-risk).
        #[arg(long)]
        execute: bool,
    },
    Status,
    Log {
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    Requests,
    Approve {
        id: String,
    },
    Decline {
        id: String,
    },
    Handoff {
        id: String,
    },
    Say {
        text: Vec<String>,
    },
    Resume,
    Pause,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();
    if let Some(p) = &cli.env_file {
        std::env::set_var("RELAY_FLEET_ENV_FILE", p);
    }
    let mut cfg = OpsConfig::load();
    if let Some(g) = &cli.gitlab {
        cfg.gitlab_url = Some(g.clone());
    }
    let only = cli.only.as_deref();
    let interactive = cli.interactive;
    match cli.cmd {
        Commands::Hosts => {
            let (fleet, source) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::hosts_text(&cfg, &fleet, &source));
            Ok(())
        }
        Commands::SshCheck => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::ssh_check(&cfg, &fleet, interactive)?);
            Ok(())
        }
        Commands::Status => cmd_status(&cfg, only).await,
        Commands::Health { mcp } => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::health(&cfg, &fleet, mcp).await?);
            Ok(())
        }
        Commands::Ssl => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::ssl_check(&cfg, &fleet).await?);
            Ok(())
        }
        Commands::Drift => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::drift(&cfg, &fleet, interactive)?);
            Ok(())
        }
        Commands::Sync { peer } => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!(
                "{}",
                fleet_cmd::sync_node(&cfg, &fleet, interactive, peer.as_deref())?
            );
            Ok(())
        }
        Commands::Update { build } | Commands::DeployAll { build } => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!(
                "{}",
                fleet_cmd::update(
                    &cfg,
                    &fleet,
                    interactive,
                    cli.build || build,
                    cli.allow_dirty,
                )?
            );
            Ok(())
        }
        Commands::Repair => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!(
                "{}",
                fleet_cmd::remote_install(&cfg, &fleet, interactive, "repair", None)?
            );
            Ok(())
        }
        Commands::FeaturesSet {
            piper,
            npm_pkgs,
            translation,
            translation_pkgs,
            libretranslate,
            inbound_mail,
            mail_domains,
            mail_user,
            mail_hostname,
        } => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            let env_body = fleet_cmd::features_env(
                piper.as_deref(),
                npm_pkgs.as_deref(),
                translation.as_deref(),
                translation_pkgs.as_deref(),
                libretranslate.as_deref(),
                inbound_mail.as_deref(),
                mail_domains.as_deref(),
                mail_user.as_deref(),
                mail_hostname.as_deref(),
            );
            print!(
                "{}",
                fleet_cmd::remote_install(
                    &cfg,
                    &fleet,
                    interactive,
                    "reconfigure-features",
                    Some(&env_body),
                )?
            );
            Ok(())
        }
        Commands::RecloneRepo { repo, peer } => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!(
                "{}",
                fleet_cmd::reclone_repo(&cfg, &fleet, interactive, &repo, peer.as_deref())?
            );
            Ok(())
        }
        Commands::FixBareHead => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::fix_bare_head(&cfg, &fleet, interactive)?);
            Ok(())
        }
        Commands::FixRepoOwnership => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!(
                "{}",
                fleet_cmd::fix_repo_ownership(&cfg, &fleet, interactive)?
            );
            Ok(())
        }
        Commands::BootstrapSsh => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!("{}", fleet_cmd::bootstrap_ssh(&cfg, &fleet)?);
            Ok(())
        }
        Commands::FleetDns {
            drop_unhealthy,
            dry_run,
            yes,
            fleet: ingress,
        } => {
            if drop_unhealthy && !dry_run && !yes {
                anyhow::bail!("--drop-unhealthy apply requires --yes (or pass --dry-run)");
            }
            let (fleet, _) = load_fleet(&cfg, only).await?;
            print!(
                "{}",
                fleet_cmd::fleet_dns(&cfg, &fleet, drop_unhealthy, dry_run, ingress.as_deref(),)
                    .await?
            );
            Ok(())
        }
        Commands::Reinstall => {
            let (fleet, _) = load_fleet(&cfg, only).await?;
            if cli.danger_wipe_relay_data {
                let confirm = cli
                    .confirm_wipe
                    .as_deref()
                    .ok_or_else(|| anyhow!("destructive reinstall needs --confirm-wipe=FQDN"))?;
                print!(
                    "{}",
                    fleet_cmd::reinstall_wipe(&cfg, &fleet, interactive, confirm)?
                );
            } else {
                print!(
                    "{}",
                    fleet_cmd::update(&cfg, &fleet, interactive, cli.build, cli.allow_dirty)?
                );
            }
            Ok(())
        }
        Commands::Inventory => {
            println!(
                "{}",
                serde_json::to_string_pretty(&Inventory::collect(&cfg))?
            );
            Ok(())
        }
        Commands::Serve { bind } => cmd_serve(&cfg, bind).await,
        Commands::PointDns { fqdn, ip } => {
            let tok = cfg
                .vercel_token
                .as_deref()
                .ok_or_else(|| anyhow!("needs Vercel token"))?;
            let v = Vercel::new(tok, cfg.vercel_team_id.as_deref())?;
            println!("{}", v.point_a(&fqdn, &ip).await?);
            Ok(())
        }
        Commands::KnownHostsReset { fqdn, ip } => {
            println!("{}", jobs::reset_known_hosts(&fqdn, &ip)?);
            Ok(())
        }
        Commands::InstallService => install_service(&cfg),
        Commands::Brain { cmd } => cmd_brain(cmd).await,
    }
}

async fn cmd_brain(cmd: BrainCmd) -> Result<()> {
    match cmd {
        BrainCmd::Serve { execute } => brain::run_serve(execute).await,
        BrainCmd::Status => {
            let v = brain_client::get_json("/status")?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        BrainCmd::Log { tail } => {
            let v = brain_client::get_json(&format!("/feed?tail={tail}"))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        BrainCmd::Requests => {
            let v = brain_client::get_json("/requests")?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        BrainCmd::Approve { id } => {
            println!("{}", brain_client::post_auth(&format!("/requests/{id}/approve"), None)?);
            Ok(())
        }
        BrainCmd::Decline { id } => {
            println!("{}", brain_client::post_auth(&format!("/requests/{id}/decline"), None)?);
            Ok(())
        }
        BrainCmd::Handoff { id } => {
            let v = brain_client::get_json("/requests")?;
            let pending = v
                .get("pending")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default();
            let req = pending
                .iter()
                .find(|r| r.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
                .ok_or_else(|| anyhow!("pending request {id} not found"))?;
            let br: brain::BrainRequest = serde_json::from_value(req.clone())?;
            println!("{}", brain::handoff_prompt(&br));
            Ok(())
        }
        BrainCmd::Say { text } => {
            let text = text.join(" ");
            println!(
                "{}",
                brain_client::post_auth("/chat", Some(serde_json::json!({ "text": text })))?
            );
            Ok(())
        }
        BrainCmd::Resume => {
            println!("{}", brain_client::post_auth("/resume", None)?);
            Ok(())
        }
        BrainCmd::Pause => {
            println!("{}", brain_client::post_auth("/pause", None)?);
            Ok(())
        }
    }
}

fn gitlab_url(cfg: &OpsConfig) -> Result<String> {
    cfg.gitlab_url_resolved().ok_or_else(|| {
        anyhow!(
            "pass --gitlab URL, set gitlab_url in ~/.config/relay-ops/config.toml, or run from a checkout with a git remote"
        )
    })
}

async fn load_fleet(cfg: &OpsConfig, only: Option<&str>) -> Result<(topology::FleetFile, String)> {
    let url = gitlab_url(cfg)?;
    let (fleet, source) = gitlab::load_topology(cfg, &url).await?;
    Ok((fleet.filtered(only)?, source))
}

async fn cmd_status(cfg: &OpsConfig, only: Option<&str>) -> Result<()> {
    let url = gitlab_url(cfg)?;
    let (fleet, source) = load_fleet(cfg, only).await?;
    let inv = Inventory::collect(cfg);
    let (nodes, ingress, live) = probe_fleet(&fleet, Some(&url)).await;
    let out = serde_json::json!({
        "gitlab_url": url,
        "topology_source": source,
        "inventory": inv,
        "nodes": nodes,
        "ingress": ingress,
        "live_peers": live,
        "fleet_nodes": fleet.nodes.iter().map(|n| &n.fqdn).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn cmd_serve(cfg: &OpsConfig, bind: Option<String>) -> Result<()> {
    let url = gitlab_url(cfg)?;
    let bind = bind.unwrap_or_else(|| cfg.bind_addr());
    let (fleet, source) = gitlab::load_topology(cfg, &url).await?;
    #[cfg(unix)]
    let tray_tx = Some(crate::tray::spawn_with_updates(format!("http://{bind}")));
    let app = App {
        cfg: cfg.clone(),
        gitlab_url: url,
        inner: Arc::new(Mutex::new(Inner {
            fleet,
            topology_source: source,
            nodes: Vec::new(),
            ingress: None,
            live_peers: Vec::new(),
            last_error: None,
        })),
        jobs: jobs::Jobs::default(),
        #[cfg(unix)]
        tray_tx,
    };
    serve::serve(app, &bind).await
}

fn install_service(cfg: &OpsConfig) -> Result<()> {
    let exe = std::env::current_exe()?;
    let gitlab = gitlab_url(cfg)?;
    #[cfg(windows)]
    {
        let startup = dirs::home_dir()
            .ok_or_else(|| anyhow!("no home dir"))?
            .join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup");
        fs::create_dir_all(&startup)?;
        let cmd_path = startup.join("relay-ops.cmd");
        let body = format!(
            "@echo off\r\nstart \"relay-ops\" /min \"{}\" serve --gitlab {}\r\n",
            exe.display(),
            gitlab
        );
        fs::write(&cmd_path, body)?;
        println!("wrote {}", cmd_path.display());
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
        let bin = home.join(".local/bin");
        fs::create_dir_all(&bin)?;
        let dest = bin.join("relay-ops");
        if exe != dest {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "stop", "relay-ops.service"])
                .status();
            fs::copy(&exe, &dest).with_context(|| format!("copy to {}", dest.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
            }
        }
        // Brain was removed in the Linode fleet redesign — stop any leftover unit/binary.
        let brain_dest = bin.join("relay-brain");
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "relay-brain.service"])
            .status();
        let _ = fs::remove_file(&brain_dest);
        let icon_dir = home.join(".local/share/icons/hicolor/128x128/apps");
        fs::create_dir_all(&icon_dir)?;
        let icon = icon_dir.join("relay-ops.png");
        #[cfg(unix)]
        crate::tray::write_png(&icon, false)?;
        for size in [24u32, 32, 48, 64] {
            let dir = home.join(format!(".local/share/icons/hicolor/{size}x{size}/apps"));
            fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                let p = dir.join("relay-ops.png");
                let _ = crate::tray::write_png_size(&p, size, false);
            }
        }

        let unit_dir = dirs::config_dir()
            .ok_or_else(|| anyhow!("no config dir"))?
            .join("systemd/user");
        fs::create_dir_all(&unit_dir)?;
        let unit = unit_dir.join("relay-ops.service");
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
        let xauth = std::env::var("XAUTHORITY").unwrap_or_default();
        let mut env_lines = format!("Environment=DISPLAY={display}\n");
        if !xauth.is_empty() {
            env_lines.push_str(&format!("Environment=XAUTHORITY={xauth}\n"));
        }
        let mut workdir = String::new();
        if let Some(root) = cfg.checkout_path() {
            env_lines.push_str(&format!(
                "Environment=RELAY_OPS_CHECKOUT={}\n",
                root.display()
            ));
            env_lines.push_str("Environment=RELAY_OPS_PREFER_LOCAL_FLEET=1\n");
            workdir = format!("WorkingDirectory={}\n", root.display());
        }
        let body = format!(
            "[Unit]\nDescription=Relay fleet ops (CLI + UI + tray)\nAfter=graphical-session.target network-online.target\n\n[Service]\n{workdir}{env_lines}ExecStart={} serve --gitlab {}\nRestart=always\nRestartSec=5\nKillMode=process\n\n[Install]\nWantedBy=default.target\n",
            dest.display(),
            gitlab
        );
        fs::write(&unit, body)?;

        {
            let brain_unit = unit_dir.join("relay-brain.service");
            let _ = fs::remove_file(&brain_unit);
        }

        let auto_dir = home.join(".config/autostart");
        fs::create_dir_all(&auto_dir)?;
        let desktop = auto_dir.join("relay-ops.desktop");
        fs::write(
            &desktop,
            format!(
                "[Desktop Entry]\nType=Application\nName=Relay ops\nComment=Relay fleet observer\nExec=systemctl --user start relay-ops.service\nIcon={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
                icon.display()
            ),
        )?;

        let _ = std::process::Command::new("systemctl")
            .args([
                "--user",
                "import-environment",
                "DISPLAY",
                "XAUTHORITY",
                "WAYLAND_DISPLAY",
                "DBUS_SESSION_BUS_ADDRESS",
            ])
            .status();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let st = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "relay-ops.service"])
            .status()?;
        if !st.success() {
            anyhow::bail!("systemctl --user enable --now relay-ops.service failed");
        }
        println!(
            "installed {} (tray + http://127.0.0.1:7780)",
            dest.display()
        );
        println!("unit {}", unit.display());
        println!("autostart {}", desktop.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_has_fleet_subcommands() {
        let cmd = Cli::command();
        let names: Vec<_> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        for n in [
            "hosts",
            "ssh-check",
            "status",
            "health",
            "ssl",
            "drift",
            "sync",
            "update",
            "deploy-all",
            "repair",
            "features-set",
            "reclone-repo",
            "fix-bare-head",
            "bootstrap-ssh",
            "fleet-dns",
            "reinstall",
            "serve",
            "inventory",
            "point-dns",
            "known-hosts-reset",
            "install-service",
        ] {
            assert!(
                names.iter().any(|x| x == n),
                "missing subcommand {n} in {names:?}"
            );
        }
    }
}

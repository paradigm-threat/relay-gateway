//! Local / CI check: same branch policy as `relay-hook-handler` pre-receive (`enforce_push_range`).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "relay-branch-policy-check",
    about = "Validate .relay.yaml / git.branchRules the same way relay pre-receive does"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Commits in `old..new` (Git range: exclusive old, inclusive new) — matches server for updates.
    Range {
        /// Path to repository (bare or non-bare)
        #[arg(value_name = "REPO")]
        repo: PathBuf,
        /// Old ref tip (40 hex) or 000…0 for new ref
        old_oid: String,
        /// New ref tip (40 hex)
        new_oid: String,
        /// Branch name (e.g. main), not refname
        branch: String,
    },
    /// Every commit reachable from `refs/heads/<branch>` (use after history rewrite).
    Walk {
        /// Path to repository
        #[arg(default_value = ".", value_name = "REPO")]
        repo: PathBuf,
        #[arg(default_value = "main", short = 'b', long = "branch")]
        branch: String,
        /// Use `git rev-list --first-parent` (linearized history)
        #[arg(long = "first-parent")]
        first_parent: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Range {
            repo,
            old_oid,
            new_oid,
            branch,
        } => {
            relay_server::git::enforce_push_range(&repo, &old_oid, &new_oid, &branch)
                .map_err(|e| anyhow::anyhow!(e))?;
            eprintln!(
                "OK: range {}..{} on {} passes branch policy",
                old_oid, new_oid, branch
            );
        }
        Commands::Walk {
            repo,
            branch,
            first_parent,
        } => {
            relay_server::git::enforce_all_commits_on_branch(&repo, &branch, first_parent)
                .map_err(|e| anyhow::anyhow!(e))?;
            eprintln!("OK: all commits on {} pass branch policy", branch);
        }
    }
    Ok(())
}

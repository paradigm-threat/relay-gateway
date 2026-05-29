use base64::engine::general_purpose::STANDARD as B64_ENGINE;
use base64::Engine;
use relay_server::git::{execute_repo_hook, HookContext};
use std::io::{self, Read};
use std::path::PathBuf;
use tracing::{debug, error, info, Level};
use tracing_subscriber::FmtSubscriber;

fn main() -> anyhow::Result<()> {
    // Force write to stderr so we see it in git daemon logs
    eprintln!("[relay-hook-handler] Hook started");

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();
    relay_server::notify::install_panic_hook();

    let args: Vec<String> = std::env::args().collect();
    let hook_path = args.get(0).map(PathBuf::from).unwrap_or_default();
    let hook_name = hook_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let git_dir = std::env::var("GIT_DIR")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_dir())?;

    // Git hooks like pre-receive and post-receive get input from stdin
    // Format: <old-value> SP <new-value> SP <ref-name> LF
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    let absolute_git_dir = if git_dir.is_absolute() {
        git_dir.clone()
    } else {
        std::env::current_dir()?.join(&git_dir)
    };

    let _repo_name = absolute_git_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown.git");

    for line in input.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }

        let old_commit = parts[0].to_string();
        let new_commit = parts[1].to_string();
        let refname = parts[2].to_string();
        let branch = refname
            .strip_prefix("refs/heads/")
            .unwrap_or(&refname)
            .to_string();

        let mut ctx = HookContext {
            repo_path: absolute_git_dir.clone(),
            old_commit: old_commit.clone(),
            new_commit: new_commit.clone(),
            refname: refname.clone(),
            branch: branch.clone(),
            is_verified: false,
            files: std::collections::HashMap::new(),
        };

        // Extract changed files using git CLI (quarantine-aware)
        if let Ok(fs) = extract_changed_files(&ctx.repo_path, &old_commit, &new_commit) {
            ctx.files = fs;
        }

        // Enforce branch rules for pre-receive BEFORE execute_repo_hook
        if hook_name == "pre-receive" {
            match enforce_branch_rules(&ctx) {
                Ok(_) => ctx.is_verified = true,
                Err(e) => {
                    error!("Branch rule violation: {}", e);
                    eprintln!("[relay-hook-handler] Branch rule violation: {}", e);
                    let repo_slug = relay_server::notify::normalize_repo_slug(
                        ctx.repo_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown.git"),
                    );
                    relay_server::notify::notify_pre_receive_rejection(
                        &repo_slug,
                        &ctx.branch,
                        &ctx.refname,
                        &ctx.old_commit,
                        &ctx.new_commit,
                        &e.to_string(),
                    );
                    std::process::exit(1);
                }
            }
        }

        if !execute_repo_hook(&ctx, hook_name)? {
            error!("Hook {} failed for {}", hook_name, ctx.refname);
            relay_server::notify::notify_hook_error(
                hook_name,
                &format!("configured hook failed for {}", ctx.refname),
            );
            std::process::exit(1);
        }

        // Special handling for post-receive (Auto-Push)
        if hook_name == "post-receive" {
            let repo_slug = relay_server::notify::normalize_repo_slug(
                ctx.repo_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown.git"),
            );
            let sid = std::env::var("RELAY_SERVER_ID").ok();
            let node = std::env::var("RELAY_PUBLIC_HOSTNAME")
                .or_else(|_| std::env::var("RELAY_NODE_FQDN"))
                .ok();
            relay_server::notify::commit_update_notify_blocking(
                "git_push",
                &repo_slug,
                &ctx.branch,
                &ctx.new_commit,
                sid.as_deref(),
                node.as_deref(),
            );
            if let Err(e) = handle_auto_push(&ctx) {
                error!("Auto-push failed: {}", e);
            }
        }
    }

    Ok(())
}

fn enforce_branch_rules(ctx: &HookContext) -> anyhow::Result<()> {
    if !ctx.refname.starts_with("refs/heads/") {
        return Ok(());
    }
    relay_server::git::enforce_push_range(
        &ctx.repo_path,
        &ctx.old_commit,
        &ctx.new_commit,
        &ctx.branch,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))
}

fn handle_auto_push(ctx: &HookContext) -> anyhow::Result<()> {
    // Avoid infinite loops if we are already in a sync operation
    if std::env::var("RELAY_SYNC_IN_PROGRESS").is_ok() {
        debug!(
            "Sync already in progress, skipping auto-push for {}",
            ctx.branch
        );
        return Ok(());
    }

    let repo = git2::Repository::open_bare(&ctx.repo_path)?;
    let config = match relay_server::git::read_relay_config(&repo, &ctx.new_commit) {
        Some(c) => c,
        None => return Ok(()),
    };

    let git_config = match config.git {
        Some(c) => c,
        None => return Ok(()),
    };

    let auto_push = match git_config.auto_push {
        Some(ap) => ap,
        None => return Ok(()),
    };

    if !auto_push.branches.contains(&ctx.branch) && !auto_push.branches.contains(&"*".to_string()) {
        return Ok(());
    }

    info!(
        "Starting auto-push for branch {} to {} peers",
        ctx.branch,
        auto_push.origin_list.len()
    );

    let repo_name = ctx
        .repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown.git");

    let mut any_failed = false;
    for origin in auto_push.origin_list {
        let push_url = if origin.contains("://") || origin.contains("@") {
            origin.clone()
        } else {
            format!("git://{}/{}", origin, repo_name)
        };

        info!("Pushing {} to {}", ctx.branch, push_url);
        eprintln!(
            "[relay-hook-handler] Pushing {} to {}",
            ctx.branch, push_url
        );

        // Construct git push command
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(&ctx.repo_path)
            .arg("push")
            .arg(&push_url)
            .arg(format!("{}:{}", ctx.branch, ctx.branch))
            .env("RELAY_SYNC_IN_PROGRESS", "1");

        match cmd.output() {
            Ok(output) => {
                if !output.status.success() {
                    any_failed = true;
                    let detail = String::from_utf8_lossy(&output.stderr).to_string();
                    error!("Failed to push to {}: {}", origin, detail);
                    relay_server::notify::notify_auto_push_error(&origin, &detail);
                } else {
                    info!("Successfully pushed to {}", origin);
                }
            }
            Err(e) => {
                any_failed = true;
                error!("Error executing git push to {}: {}", origin, e);
                relay_server::notify::notify_auto_push_error(&origin, &e.to_string());
            }
        }
    }

    if any_failed {
        anyhow::bail!("one or more auto-push targets failed (see logs / Slack per origin)");
    }
    Ok(())
}

fn extract_changed_files(
    repo_path: &std::path::Path,
    old_rev: &str,
    new_rev: &str,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut files = std::collections::HashMap::new();

    // Get list of changed files
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("diff-tree")
        .arg("-r")
        .arg("--no-commit-id")
        .arg("--name-only")
        .arg(old_rev)
        .arg(new_rev)
        .output()?;

    if !output.status.success() {
        return Ok(files);
    }

    let paths = String::from_utf8_lossy(&output.stdout);
    for path in paths.lines().filter(|l| !l.is_empty()) {
        // Extract content for each path
        let content_out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .arg("show")
            .arg(format!("{}:{}", new_rev, path))
            .output()?;

        if content_out.status.success() {
            files.insert(path.to_string(), B64_ENGINE.encode(&content_out.stdout));
        }
    }

    Ok(files)
}

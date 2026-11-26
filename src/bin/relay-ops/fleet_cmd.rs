use crate::config::OpsConfig;
use crate::inventory;
use crate::probe::{dead_ingress_ips, healthy_node_ips, probe_fleet, resolve_ips};
use crate::ssh::{checkout, output_text, SshCtx};
use crate::topology::FleetFile;
use crate::vercel::Vercel;
use anyhow::{anyhow, Context, Result};
use reqwest::Method;
use std::fs;
use std::io::Write;
use std::process::Command;
use std::time::Duration;

pub fn hosts_text(cfg: &OpsConfig, fleet: &FleetFile, source: &str) -> String {
    let inv = crate::inventory::Inventory::collect(cfg);
    let mut s = format!("topology_source={source}\n");
    s.push_str(&format!(
        "SSH_USER={}\nSSH_IDENTITY={}\n",
        cfg.ssh_user.as_deref().unwrap_or("(per-node / ubuntu)"),
        inv.identity_path.as_deref().unwrap_or("-")
    ));
    s.push_str("HOSTS=");
    s.push_str(
        &fleet
            .nodes
            .iter()
            .map(|n| n.fqdn.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    s.push('\n');
    s
}

pub fn ssh_check(cfg: &OpsConfig, fleet: &FleetFile, interactive: bool) -> Result<String> {
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let mut log = String::new();
    let mut fail = false;
    for n in &fleet.nodes {
        log.push_str(&format!("ssh-check begin {}\n", n.fqdn));
        match ssh.run(&n.fqdn, "true") {
            Ok(out) if out.status.success() => log.push_str(&format!("ssh-check ok {}\n", n.fqdn)),
            Ok(out) => {
                fail = true;
                log.push_str(&format!(
                    "ssh-check FAIL {}: {}\n",
                    n.fqdn,
                    output_text(&out).trim()
                ));
            }
            Err(e) => {
                fail = true;
                log.push_str(&format!("ssh-check FAIL {}: {e:#}\n", n.fqdn));
            }
        }
    }
    if fail {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

pub async fn health(cfg: &OpsConfig, fleet: &FleetFile, mcp: bool) -> Result<String> {
    let repos = fleet.health_repos();
    let q = fleet.health_query().to_string();
    let node_bin = inventory::which("node");
    let scripts = checkout(cfg).ok().and_then(|root| {
        let q_script = root.join("scripts/validate-relay-query.mjs");
        let m_script = root.join("scripts/validate-relay-mcp.mjs");
        if q_script.is_file() {
            Some((q_script, m_script))
        } else {
            None
        }
    });
    if mcp && scripts.as_ref().map(|s| !s.1.is_file()).unwrap_or(true) {
        anyhow::bail!("health --mcp needs scripts/validate-relay-mcp.mjs in the gateway checkout");
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let method = Method::from_bytes(b"QUERY").unwrap_or(Method::GET);
    let mut log = String::new();
    let mut fail = false;
    for n in &fleet.nodes {
        log.push_str(&format!("health begin {}\n", n.fqdn));
        let base = format!("https://{}", n.fqdn);
        if repos.is_empty() {
            let url = format!("{base}/api/config");
            match client.post(&url).json(&serde_json::json!({})).send().await {
                Ok(r) if r.status().is_success() => log.push_str("  POST /api/config ok\n"),
                Ok(r) => {
                    fail = true;
                    log.push_str(&format!("  FAIL /api/config {}\n", r.status()));
                }
                Err(e) => {
                    fail = true;
                    log.push_str(&format!("  FAIL /api/config {e}\n"));
                }
            }
        }
        for repo in &repos {
            log.push_str(&format!("  QUERY {repo} q={q}\n"));
            let ok =
                if let (Some((q_script, _)), Some(node)) = (scripts.as_ref(), node_bin.as_ref()) {
                    let out = Command::new(node)
                        .arg(q_script)
                        .arg(&base)
                        .arg(repo)
                        .arg(&q)
                        .output();
                    match out {
                        Ok(o) => {
                            log.push_str(&output_text(&o));
                            o.status.success()
                        }
                        Err(e) => {
                            log.push_str(&format!("    FAIL {e}\n"));
                            false
                        }
                    }
                } else {
                    let url = format!("{base}/{repo}");
                    let collection = if repo.contains("timeline") {
                        "events"
                    } else {
                        "index"
                    };
                    match client
                        .request(method.clone(), &url)
                        .header("content-type", "application/json")
                        .json(&serde_json::json!({ "collection": collection, "query": q }))
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => {
                            log.push_str("    ok\n");
                            true
                        }
                        Ok(r) => {
                            log.push_str(&format!("    FAIL {}\n", r.status()));
                            false
                        }
                        Err(e) => {
                            log.push_str(&format!("    FAIL {e}\n"));
                            false
                        }
                    }
                };
            if !ok {
                fail = true;
            }
            if mcp {
                if let (Some((_, m_script)), Some(node)) = (scripts.as_ref(), node_bin.as_ref()) {
                    log.push_str(&format!("  MCP {repo}\n"));
                    let out = Command::new(node)
                        .arg(m_script)
                        .arg(&base)
                        .arg(repo)
                        .output();
                    match out {
                        Ok(o) => {
                            log.push_str(&output_text(&o));
                            if !o.status.success() {
                                fail = true;
                            }
                        }
                        Err(e) => {
                            fail = true;
                            log.push_str(&format!("    FAIL {e}\n"));
                        }
                    }
                }
            }
        }
        log.push_str(&format!("health end {}\n", n.fqdn));
    }
    let skip_ssl = matches!(
        std::env::var("RELAY_FLEET_HEALTH_SSL")
            .unwrap_or_else(|_| "1".into())
            .to_ascii_lowercase()
            .as_str(),
        "0" | "no" | "false" | "off"
    );
    if !skip_ssl {
        match ssl_check(cfg, fleet).await {
            Ok(s) => log.push_str(&s),
            Err(e) => {
                fail = true;
                log.push_str(&format!("ssl FAIL {e:#}\n"));
            }
        }
    }
    if fail {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

pub async fn ssl_check(cfg: &OpsConfig, fleet: &FleetFile) -> Result<String> {
    let mut domains: Vec<String> = fleet.health.ssl_domains.clone();
    if let Ok(env_d) = std::env::var("RELAY_FLEET_SSL_DOMAINS") {
        if !env_d.trim().is_empty() {
            domains = env_d.split_whitespace().map(|s| s.to_string()).collect();
        }
    }
    if domains.is_empty() {
        domains = DEFAULT_SSL_DOMAINS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }
    // Prefer the checkout SSL script (per-node SNI checks catch park-tls gaps).
    if let Ok(root) = checkout(cfg) {
        let script = root.join("scripts/relay-fleet-ssl-check.sh");
        if script.is_file() {
            let nodes = fleet
                .nodes
                .iter()
                .map(|n| n.fqdn.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let out = Command::new("bash")
                .arg(&script)
                .current_dir(&root)
                .env("RELAY_FLEET_SSL_LABEL", "ssl")
                .env("RELAY_FLEET_SSL_NODES", nodes)
                .env("RELAY_FLEET_SSL_DOMAINS", domains.join(" "))
                .env(
                    "RELAY_FLEET_SSL_TIMEOUT",
                    std::env::var("RELAY_FLEET_SSL_TIMEOUT").unwrap_or_else(|_| "15".into()),
                )
                .output()?;
            let log = output_text(&out);
            if !out.status.success() {
                anyhow::bail!("{log}");
            }
            return Ok(log);
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut hosts: Vec<String> = fleet.nodes.iter().map(|n| n.fqdn.clone()).collect();
    hosts.extend(domains);
    if let Some(ing) = &fleet.fleet_ingress {
        hosts.push(ing.clone());
    }
    hosts.sort();
    hosts.dedup();
    let mut log = String::new();
    let mut fail = false;
    for h in hosts {
        let url = format!("https://{h}/");
        match client.get(&url).send().await {
            Ok(r) => {
                let code = r.status().as_u16();
                let ok = code < 500;
                log.push_str(&format!(
                    "ssl {} {} {}\n",
                    h,
                    code,
                    if ok { "ok" } else { "FAIL" }
                ));
                if !ok {
                    fail = true;
                }
            }
            Err(e) => {
                fail = true;
                log.push_str(&format!("ssl {h} FAIL {e}\n"));
            }
        }
    }
    if fail {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

/// Default parked Hosts for SSL jobs when `health.sslDomains` is empty.
pub const DEFAULT_SSL_DOMAINS: &[&str] = &[
    "paradigmthreat.net",
    "www.paradigmthreat.net",
    "relay.paradigmthreat.net",
    "clevertree.net",
    "www.clevertree.net",
    "relay.clevertree.net",
    "flashbackrepository.org",
    "www.flashbackrepository.org",
];

pub fn drift(cfg: &OpsConfig, fleet: &FleetFile, interactive: bool) -> Result<String> {
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let cat = fleet.catalog_path();
    let script = format!(
        "set -euo pipefail\nC={cat:?}\nif [[ ! -e \"$C/.git\" ]]; then echo NO_CATALOG; exit 0; fi\nsudo git -c \"safe.directory=$C\" -C \"$C\" rev-parse HEAD 2>/dev/null || echo NO_HEAD\n"
    );
    let mut log = String::new();
    let mut heads = Vec::new();
    for n in &fleet.nodes {
        log.push_str(&format!("drift probe {}\n", n.fqdn));
        let out = ssh.run_bash_s(&n.fqdn, &script, &[])?;
        let line = output_text(&out).trim().to_string();
        log.push_str(&format!("  catalog_HEAD {} -> {line}\n", n.fqdn));
        heads.push((n.fqdn.clone(), line));
    }
    let ref_head = heads
        .iter()
        .find(|(_, v)| !v.starts_with("NO_"))
        .map(|(_, v)| v.clone());
    let mut mismatch = false;
    if let Some(r) = &ref_head {
        for (h, v) in &heads {
            if !v.starts_with("NO_") && v != r {
                log.push_str(&format!("DRIFT: {h} has {v} (ref {r})\n"));
                mismatch = true;
            }
        }
    }
    if !mismatch {
        log.push_str("drift: catalog HEAD aligned for compared hosts.\n");
    } else {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

/// Run catalog sync + optional peer fetch on each filtered host. Fetch only; never wipe or force-push.
/// When a peer is set, also `ensure-bare` + fetch any catalog (`authorized_repos`) slugs missing locally.
const SYNC_REMOTE: &str = r#"set -euo pipefail
INSTALL=/opt/relay
SCRIPT="$INSTALL/bin/relay-startup-catalog-sync.sh"
PEER="${1:-}"
PORT="${2:-9418}"
DATA="${3:-/opt/relay/data}"
if [[ ! -x "$SCRIPT" ]]; then
  echo "NO_INSTALL: missing $SCRIPT — use Repair/update (Linode seed) on an empty VM, not Synchronize"
  exit 1
fi
echo "catalog-sync begin $(hostname -f 2>/dev/null || hostname)"
if command -v systemd-run >/dev/null 2>&1; then
  sudo systemd-run --quiet --wait --collect --pipe \
    --property=User=relay \
    --property=EnvironmentFile=-$INSTALL/relay.env \
    "$SCRIPT"
else
  sudo -u relay "$SCRIPT"
fi
if [[ -n "$PEER" && -d "$DATA" ]]; then
  echo "peer-fetch from $PEER"
  shopt -s nullglob
  # Catalog-missing repos: create empty bare then fill from peer (no GitLab needed).
  if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    AUTH_JSON=$(curl -fsS -m 30 -X POST http://127.0.0.1:8080/api/config 2>/dev/null || true)
    if [[ -n "${AUTH_JSON:-}" ]]; then
      while IFS= read -r name; do
        [[ -z "$name" ]] && continue
        bare="$DATA/${name}.git"
        if [[ ! -d "$bare" ]]; then
          echo "ensure-bare $name (in catalog, missing locally)"
          curl -fsS -m 60 -X POST "http://127.0.0.1:8080/api/git/ensure-bare/${name}" \
            || echo "WARN ensure-bare $name failed"
        fi
      done < <(printf '%s' "$AUTH_JSON" | python3 -c 'import json,sys
try:
  d=json.load(sys.stdin)
  for n in (d.get("authorized_repos") or []):
    if isinstance(n, str) and n.strip():
      print(n.strip())
except Exception:
  pass')
    fi
  fi
  for bare in "$DATA"/*.git; do
    name=$(basename "$bare" .git)
    if sudo -u relay git --git-dir="$bare" fetch "git://${PEER}:${PORT}/${name}.git" 'refs/heads/*:refs/heads/*' --prune \
      || sudo -u relay git --git-dir="$bare" fetch "git://${PEER}:${PORT}/${name}" 'refs/heads/*:refs/heads/*' --prune; then
      echo "fetched $name from $PEER"
    else
      echo "WARN fetch $name from $PEER failed"
    fi
  done
fi
echo "catalog-sync end"
"#;

pub fn sync_node(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    interactive: bool,
    peer: Option<&str>,
) -> Result<String> {
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let data = fleet.data_root();
    let port = fleet.git_port().to_string();
    let mut log = String::new();
    for n in &fleet.nodes {
        let peer = peer
            .map(str::trim)
            .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case(&n.fqdn));
        log.push_str(&format!("sync begin {}\n", n.fqdn));
        let out = ssh.run_bash_s(&n.fqdn, SYNC_REMOTE, &[peer.unwrap_or(""), &port, data])?;
        log.push_str(&output_text(&out));
        if !out.status.success() {
            anyhow::bail!("{log}sync failed on {}", n.fqdn);
        }
        log.push_str(&format!("sync end {}\n", n.fqdn));
    }
    Ok(log)
}

pub async fn fleet_dns(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    drop_unhealthy: bool,
    dry_run: bool,
    ingress_override: Option<&str>,
) -> Result<String> {
    let tok = cfg
        .vercel_token
        .as_deref()
        .ok_or_else(|| anyhow!("needs Vercel token"))?;
    let ingress = ingress_override
        .map(|s| s.to_string())
        .or_else(|| fleet.fleet_ingress.clone())
        .ok_or_else(|| anyhow!("no fleetIngress in topology"))?;
    let fleet_dns_fqdns: std::collections::HashSet<String> = fleet
        .nodes
        .iter()
        .filter(|n| n.participates_in_fleet_dns())
        .map(|n| n.fqdn.to_ascii_lowercase())
        .collect();
    let desired = if drop_unhealthy {
        let (nodes, _, _) = probe_fleet(fleet, None).await;
        let nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| fleet_dns_fqdns.contains(&n.fqdn.to_ascii_lowercase()))
            .collect();
        let keep = healthy_node_ips(&nodes);
        if keep.is_empty() {
            anyhow::bail!("all fleetDns nodes unhealthy; refusing to empty {ingress}");
        }
        keep
    } else {
        let mut ips = Vec::new();
        for n in fleet.nodes.iter().filter(|n| n.participates_in_fleet_dns()) {
            for ip in resolve_ips(&n.fqdn).await {
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
        if ips.is_empty() {
            anyhow::bail!(
                "no A records resolved for fleetDns nodes (mesh-only peers are excluded)"
            );
        }
        ips
    };
    let mut log = format!("desired {ingress} A {desired:?}\n");
    if dry_run {
        if drop_unhealthy {
            let (_, ingress_p, _) = probe_fleet(fleet, None).await;
            if let Some(ing) = ingress_p {
                log.push_str(&format!("stale {:?}\n", dead_ingress_ips(&ing, &desired)));
            }
        }
        log.push_str("dry-run: no Vercel writes\n");
        return Ok(log);
    }
    let v = Vercel::new(tok, cfg.vercel_team_id.as_deref())?;
    log.push_str(&v.reconcile_fleet_a(&ingress, &desired).await?);
    log.push('\n');
    Ok(log)
}

pub fn update(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    interactive: bool,
    build: bool,
    allow_dirty: bool,
) -> Result<String> {
    let root = checkout(cfg)?;
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let tgz = root.join("target/relay-linode-deploy.tgz");
    let mut log = String::new();
    if allow_dirty {
        std::env::set_var("RELAY_PACK_ALLOW_DIRTY", "1");
        log.push_str("allow-dirty: RELAY_PACK_ALLOW_DIRTY=1\n");
    } else {
        // Do not inherit a laptop RELAY_PACK_ALLOW_DIRTY=1 habit.
        std::env::remove_var("RELAY_PACK_ALLOW_DIRTY");
    }
    if build || !tgz.is_file() {
        log.push_str("pack-linode-deploy.sh\n");
        let st = Command::new("bash")
            .arg(root.join("scripts/pack-linode-deploy.sh"))
            .current_dir(&root)
            .status()?;
        if !st.success() {
            anyhow::bail!("pack failed");
        }
    }
    if !tgz.is_file() {
        anyhow::bail!("missing {}", tgz.display());
    }
    let seed = root.join("scripts/relay-linode-redeploy-seed.sh");
    if let Some(key) = cfg.identity_path() {
        std::env::set_var("RELAY_SSH_IDENTITY", key);
    }
    if let Some(user) = cfg.ssh_user.as_ref() {
        std::env::set_var("RELAY_SSH_USER", user);
    }
    std::env::set_var("RELAY_REDEPLOY_SKIP_SEED", "1");
    let health_gate = !matches!(
        std::env::var("RELAY_FLEET_UPDATE_HEALTH")
            .unwrap_or_else(|_| "1".into())
            .to_ascii_lowercase()
            .as_str(),
        "0" | "no" | "false" | "off"
    );
    for n in &fleet.nodes {
        log.push_str(&format!("update begin {}\n", n.fqdn));
        let st = Command::new("bash")
            .arg(&seed)
            .arg(&n.fqdn)
            .arg(&tgz)
            .current_dir(&root)
            .status()?;
        if !st.success() {
            anyhow::bail!("{log}redeploy-seed failed for {}", n.fqdn);
        }
        let out = ssh.run(
            &n.fqdn,
            "sudo systemctl restart relay-gateway relay-git-daemon relay-git-proxy",
        )?;
        log.push_str(&output_text(&out));
        if health_gate {
            let url = format!("https://{}/api/config", n.fqdn);
            let ok = Command::new("curl")
                .args(["-sfS", "--max-time", "25", "-X", "POST", &url])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                anyhow::bail!(
                    "{log}post-update health gate failed for {} — set RELAY_FLEET_UPDATE_HEALTH=0 to skip",
                    n.fqdn
                );
            }
            log.push_str(&format!("health gate OK {}\n", n.fqdn));
        }
        log.push_str(&format!("update end {}\n", n.fqdn));
    }
    Ok(log)
}

pub fn remote_install(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    interactive: bool,
    mode: &str,
    feat_env: Option<&str>,
) -> Result<String> {
    let root = checkout(cfg)?;
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let tgz = root.join("target/relay-linode-deploy.tgz");
    if !tgz.is_file() {
        let st = Command::new("bash")
            .arg(root.join("scripts/pack-linode-deploy.sh"))
            .current_dir(&root)
            .status()?;
        if !st.success() {
            anyhow::bail!("pack failed");
        }
    }
    let mut log = String::new();
    for n in &fleet.nodes {
        log.push_str(&format!("{mode} begin {}\n", n.fqdn));
        let staging = format!("relay-ops-st-{}", std::process::id());
        let remote_tgz = format!("/tmp/{staging}.tgz");
        ssh.scp_to(&n.fqdn, &tgz, &remote_tgz)?;
        if let Some(env_body) = feat_env {
            let mut tmp = tempfile::NamedTempFile::new()?;
            tmp.write_all(env_body.as_bytes())?;
            ssh.scp_to(&n.fqdn, tmp.path(), &format!("/tmp/{staging}.feat.env"))?;
        }
        if let Some(mesh) = mesh_env_body() {
            let mut tmp = tempfile::NamedTempFile::new()?;
            tmp.write_all(mesh.as_bytes())?;
            ssh.scp_to(&n.fqdn, tmp.path(), &format!("/tmp/{staging}.mesh.env"))?;
        }
        let remote = r#"set -euo pipefail
ST="$1" TGZ="$2" MODE="$3"
FEAT="/tmp/${ST}.feat.env"
MESH="/tmp/${ST}.mesh.env"
sudo rm -rf "/tmp/$ST"
sudo mkdir "/tmp/$ST"
sudo tar xzf "$TGZ" -C "/tmp/$ST"
sudo rm -f "$TGZ"
export RELAY_BIN_SOURCE="/tmp/$ST"
export RELAY_INSTALL_NONINTERACTIVE=1
sudo bash -c "
  set -euo pipefail
  set -a
  [[ -f \"$MESH\" ]] && . \"$MESH\"
  [[ -f \"$FEAT\" ]] && . \"$FEAT\"
  set +a
  export RELAY_INSTALL_NONINTERACTIVE=1 RELAY_BIN_SOURCE=\"$RELAY_BIN_SOURCE\"
  cd \"$RELAY_BIN_SOURCE\" && ./install.sh \"$MODE\"
"
sudo rm -f "$FEAT" "$MESH"
sudo rm -rf "/tmp/$ST"
"#;
        let out = ssh.run_bash_s(&n.fqdn, remote, &[&staging, &remote_tgz, mode])?;
        log.push_str(&output_text(&out));
        if !out.status.success() {
            anyhow::bail!("{log}{mode} failed on {}", n.fqdn);
        }
        log.push_str(&format!("{mode} end {}\n", n.fqdn));
    }
    Ok(log)
}

pub fn features_env(
    piper: Option<&str>,
    npm_pkgs: Option<&str>,
    translation: Option<&str>,
    translation_pkgs: Option<&str>,
    libretranslate: Option<&str>,
    inbound_mail: Option<&str>,
    mail_domains: Option<&str>,
    mail_user: Option<&str>,
    mail_hostname: Option<&str>,
) -> String {
    let mut s = String::from("RELAY_INSTALL_NONINTERACTIVE=1\n");
    // Fleet policy: Piper TTS (/voices + /tts) on every node unless explicitly disabled.
    s.push_str(&format!("RELAY_FEAT_PIPER={}\n", piper.unwrap_or("1")));
    s.push_str(&format!("RELAY_FEAT_NPM_PKGS={}\n", npm_pkgs.unwrap_or("")));
    s.push_str(&format!(
        "RELAY_FEAT_TRANSLATION={}\n",
        translation.unwrap_or("0")
    ));
    s.push_str(&format!(
        "RELAY_FEAT_TRANSLATION_PKGS={}\n",
        translation_pkgs.unwrap_or("")
    ));
    s.push_str(&format!(
        "RELAY_FEAT_LIBRETRANSLATE={}\n",
        libretranslate.unwrap_or("0")
    ));
    s.push_str(&format!(
        "RELAY_FEAT_INBOUND_MAIL={}\n",
        inbound_mail.unwrap_or("0")
    ));
    if let Some(d) = mail_domains.filter(|v| !v.is_empty()) {
        s.push_str(&format!("RELAY_MAIL_DOMAINS={d}\n"));
    }
    if let Some(u) = mail_user.filter(|v| !v.is_empty()) {
        s.push_str(&format!("RELAY_MAIL_USER={u}\n"));
    }
    if let Some(h) = mail_hostname.filter(|v| !v.is_empty()) {
        s.push_str(&format!("RELAY_MAIL_HOSTNAME={h}\n"));
    }
    s
}

fn mesh_env_body() -> Option<String> {
    let peers = std::env::var("RELAY_MASTER_PEER_LIST").ok();
    let peer_secret = std::env::var("RELAY_PEER_SECRET").ok();
    let mesh_secret = std::env::var("RELAY_MASTER_MESH_SECRET").ok();
    if peers.is_none() && peer_secret.is_none() && mesh_secret.is_none() {
        return None;
    }
    let mut s = String::from("RELAY_MERGE_MESH_ENV=1\n");
    if let Some(v) = peers {
        s.push_str(&format!("RELAY_MASTER_PEER_LIST=\"{v}\"\n"));
    }
    if let Some(v) = peer_secret {
        s.push_str(&format!("RELAY_PEER_SECRET={v}\n"));
    }
    if let Some(v) = mesh_secret {
        s.push_str(&format!("RELAY_MASTER_MESH_SECRET={v}\n"));
    }
    Some(s)
}

pub fn reclone_repo(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    interactive: bool,
    repo: &str,
    peer: Option<&str>,
) -> Result<String> {
    let root = checkout(cfg)?;
    let script = fs::read_to_string(root.join("scripts/relay-reclone-bare.sh"))
        .with_context(|| format!("missing {}/scripts/relay-reclone-bare.sh", root.display()))?;
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let mut log = String::new();
    for n in &fleet.nodes {
        log.push_str(&format!("reclone-repo {repo} on {}\n", n.fqdn));
        let peer = peer.unwrap_or("");
        let out = ssh.run_bash_s(
            &n.fqdn,
            &script,
            &[fleet.data_root(), &fleet.git_port().to_string(), repo, peer],
        )?;
        log.push_str(&output_text(&out));
        if !out.status.success() {
            anyhow::bail!("{log}reclone failed on {}", n.fqdn);
        }
    }
    Ok(log)
}

pub fn fix_bare_head(cfg: &OpsConfig, fleet: &FleetFile, interactive: bool) -> Result<String> {
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let data = fleet.data_root();
    let cat = fleet.catalog_path();
    let script = format!(
        r#"set -euo pipefail
DATA_ROOT={data:?}
CATALOG={cat:?}
sudo -u relay env DATA_ROOT="$DATA_ROOT" CATALOG="$CATALOG" bash -s <<'INNER'
set -euo pipefail
cd "$CATALOG"
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  branch=$(git config --file .gitmodules --get "submodule.${{name}}.branch" 2>/dev/null || echo main)
  bare="${{DATA_ROOT}}/${{name}}.git"
  [[ -d "$bare" ]] || continue
  ref="refs/heads/${{branch}}"
  git --git-dir="$bare" rev-parse --verify "$ref" >/dev/null 2>&1 || continue
  current=$(git --git-dir="$bare" symbolic-ref -q HEAD 2>/dev/null || true)
  if [[ "$current" != "$ref" ]]; then
    git --git-dir="$bare" symbolic-ref HEAD "$ref"
    echo "fixed ${{name}}: HEAD -> ${{ref}}"
  else
    echo "ok ${{name}}"
  fi
done < <(git config --file .gitmodules --name-only --get-regexp '^submodule\..*\.path$' | sed -n 's/^submodule\.\(.*\)\.path$/\1/p' | sort -u)
INNER
sudo systemctl restart relay-gateway relay-git-daemon relay-git-proxy 2>/dev/null || true
"#
    );
    let mut log = String::new();
    for n in &fleet.nodes {
        log.push_str(&format!("fix-bare-head {}\n", n.fqdn));
        let out = ssh.run_bash_s(&n.fqdn, &script, &[])?;
        log.push_str(&output_text(&out));
    }
    Ok(log)
}

/// Restore `relay:relay` ownership on bare repos under the fleet data root.
/// Do not paper over with `git config safe.directory` — the gateway must write as `relay`.
pub fn fix_repo_ownership(cfg: &OpsConfig, fleet: &FleetFile, interactive: bool) -> Result<String> {
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let data = fleet.data_root();
    let script = r#"set -euo pipefail
DATA="$1"
[[ -d "$DATA" ]] || { echo "missing $DATA"; exit 1; }
fixed=0
ok=0
shopt -s nullglob
for bare in "$DATA"/*.git; do
  [[ -d "$bare" ]] || continue
  name=$(basename "$bare" .git)
  owner=$(stat -c %U "$bare" 2>/dev/null || echo unknown)
  stray=$(find "$bare" -not -user relay -print -quit 2>/dev/null || true)
  if [[ "$owner" != "relay" || -n "$stray" ]]; then
    sudo chown -R relay:relay "$bare"
    sudo chmod -R u+rwX,g+rX,o+rX "$bare"
    echo "fixed $name (was owner=$owner)"
    fixed=$((fixed + 1))
  else
    echo "ok $name"
    ok=$((ok + 1))
  fi
done
echo "summary fixed=$fixed ok=$ok"
"#;
    let mut log = String::new();
    let mut fail = false;
    for n in &fleet.nodes {
        log.push_str(&format!("fix-repo-ownership {}\n", n.fqdn));
        match ssh.run_bash_s(&n.fqdn, script, &[data]) {
            Ok(out) => {
                log.push_str(&output_text(&out));
                if !out.status.success() {
                    fail = true;
                    log.push_str(&format!("ERROR {} ownership fix failed\n", n.fqdn));
                }
            }
            Err(e) => {
                fail = true;
                log.push_str(&format!("ERROR {}: {e:#}\n", n.fqdn));
            }
        }
    }
    if fail {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

pub fn bootstrap_ssh(cfg: &OpsConfig, fleet: &FleetFile) -> Result<String> {
    let pw = std::env::var("RELAY_ROOT_PASSWORD")
        .or_else(|_| std::env::var("SSHPASS"))
        .map_err(|_| anyhow!("set RELAY_ROOT_PASSWORD or SSHPASS"))?;
    let inv = crate::inventory::Inventory::collect(cfg);
    let key = inv
        .identity_path
        .as_ref()
        .ok_or_else(|| anyhow!("no identity file for pubkey"))?;
    let pub_path = format!("{key}.pub");
    let pk = fs::read_to_string(&pub_path).with_context(|| pub_path.clone())?;
    let mut log = String::new();
    for n in &fleet.nodes {
        let user = fleet.ssh_user(&n.fqdn, cfg.ssh_user.as_deref());
        log.push_str(&format!("bootstrap-ssh {}\n", n.fqdn));
        let mut c = Command::new("sshpass");
        c.env("SSHPASS", &pw)
            .args([
                "-e",
                "ssh",
                "-o",
                "PreferredAuthentications=password",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ConnectTimeout=20",
                &format!("root@{}", n.fqdn),
                "bash",
                "-s",
                user,
            ])
            .stdin(std::process::Stdio::piped());
        let mut child = c.spawn()?;
        use std::io::Write as _;
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(
                stdin,
                "set -euo pipefail\nU=\"$1\"\nLINE={pk:?}\ninstall -d -m 700 \"/home/$U/.ssh\"\nAK=\"/home/$U/.ssh/authorized_keys\"\ngrep -qF \"$LINE\" \"$AK\" 2>/dev/null || printf '%s\\n' \"$LINE\" >>\"$AK\"\nchmod 600 \"$AK\"\nchown -R \"$U:$U\" \"/home/$U/.ssh\"\necho ok"
            )?;
        }
        let out = child.wait_with_output()?;
        log.push_str(&output_text(&out));
        if !out.status.success() {
            anyhow::bail!("{log}bootstrap-ssh failed on {}", n.fqdn);
        }
    }
    log.push_str(&ssh_check(cfg, fleet, false)?);
    Ok(log)
}

pub fn reinstall_wipe(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    interactive: bool,
    confirm: &str,
) -> Result<String> {
    if fleet.nodes.len() != 1 {
        anyhow::bail!("destructive reinstall requires --only exactly one host");
    }
    let h = &fleet.nodes[0].fqdn;
    if h != confirm {
        anyhow::bail!("--confirm-wipe must match --only host ({h})");
    }
    let ssh = SshCtx {
        cfg,
        fleet,
        interactive,
    };
    ssh.require_admin()?;
    let data = fleet.data_root();
    let out = ssh.run(
        h,
        &format!(
            "sudo systemctl stop relay-gateway relay-git-daemon relay-git-proxy || true; sudo rm -rf {data}/*.git; echo wiped"
        ),
    )?;
    let log = output_text(&out);
    if !out.status.success() {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

/// POST `/api/index/warm/:repo` on each node for repos whose SearchStore lags git tip.
/// Re-probes between rounds so a git tip advance during warm is caught.
pub async fn warm_stale_indexes(
    fleet: &FleetFile,
    fqdn_filter: Option<&str>,
    repo_filter: Option<&str>,
) -> Result<String> {
    const MAX_ROUNDS: usize = 3;
    let gitlab = std::env::var("RELAY_OPS_GITLAB_URL").ok();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;
    let mut log = String::new();
    let mut fail = false;
    let mut warmed_total = 0usize;

    for round in 1..=MAX_ROUNDS {
        let (mut nodes, _, _) = probe_fleet(fleet, gitlab.as_deref()).await;
        if let Some(fqdn) = fqdn_filter.filter(|s| !s.trim().is_empty()) {
            nodes.retain(|n| n.fqdn.eq_ignore_ascii_case(fqdn.trim()));
            if nodes.is_empty() {
                anyhow::bail!("unknown or unreachable node {fqdn}");
            }
        }
        let mut round_warmed = 0usize;
        for n in &nodes {
            if !n.healthy {
                log.push_str(&format!("skip {} (down)\n", n.fqdn));
                continue;
            }
            let stale: Vec<_> = n
                .index_stale_repos
                .iter()
                .filter(|s| {
                    repo_filter
                        .map(|r| s.repo.eq_ignore_ascii_case(r.trim()))
                        .unwrap_or(true)
                })
                .collect();
            if stale.is_empty() {
                if round == 1 {
                    log.push_str(&format!("{} index ok (no stale repos)\n", n.fqdn));
                }
                continue;
            }
            log.push_str(&format!(
                "{} warming {} stale index(es){}\n",
                n.fqdn,
                stale.len(),
                if round > 1 {
                    format!(" (round {round})")
                } else {
                    String::new()
                }
            ));
            for s in stale {
                // Branch names from probe are safe path segments; pass through so warm
                // indexes the publish branch (e.g. clevertree-site@release), not only main.
                let url = format!(
                    "https://{}/api/index/warm/{}?branch={}",
                    n.fqdn, s.repo, s.branch
                );
                match client.post(&url).json(&serde_json::json!({})).send().await {
                    Ok(r) if r.status().is_success() => {
                        round_warmed += 1;
                        let body = r.text().await.unwrap_or_default();
                        let warmed_tip = serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|j| j.get("tip").and_then(|t| t.as_str()).map(str::to_string))
                            .unwrap_or_else(|| s.tip_sha.clone());
                        let warmed_short = warmed_tip.chars().take(12).collect::<String>();
                        let snapshot_short = s.tip_sha.chars().take(12).collect::<String>();
                        if warmed_tip.eq_ignore_ascii_case(&s.tip_sha) {
                            log.push_str(&format!(
                                "  ok {}@{} tip={}\n",
                                s.repo, s.branch, warmed_short
                            ));
                        } else {
                            log.push_str(&format!(
                                "  ok {}@{} warmed tip={} (probe snapshot was {} — git tip moved)\n",
                                s.repo, s.branch, warmed_short, snapshot_short
                            ));
                        }
                        match verify_repo_index_fresh(&client, &n.fqdn, &s.repo).await {
                            Ok(true) => {}
                            Ok(false) => {
                                log.push_str(&format!(
                                    "  WARN {} still stale on {} after warm — will retry if rounds remain\n",
                                    s.repo, n.fqdn
                                ));
                            }
                            Err(e) => {
                                log.push_str(&format!(
                                    "  WARN {} status check on {}: {e:#}\n",
                                    s.repo, n.fqdn
                                ));
                            }
                        }
                    }
                    Ok(r) => {
                        fail = true;
                        let status = r.status();
                        let body = r.text().await.unwrap_or_default();
                        log.push_str(&format!(
                            "  FAIL {} HTTP {} {}\n",
                            s.repo,
                            status,
                            body.trim()
                        ));
                    }
                    Err(e) => {
                        fail = true;
                        log.push_str(&format!("  FAIL {} {e:#}\n", s.repo));
                    }
                }
            }
        }
        warmed_total += round_warmed;
        if round_warmed == 0 {
            break;
        }
        let (nodes_after, _, _) = probe_fleet(fleet, gitlab.as_deref()).await;
        let any_stale = nodes_after.iter().any(|n| {
            n.healthy
                && !n.index_ok
                && n.index_stale_repos.iter().any(|s| {
                    repo_filter
                        .map(|r| s.repo.eq_ignore_ascii_case(r.trim()))
                        .unwrap_or(true)
                        && fqdn_filter
                            .map(|f| n.fqdn.eq_ignore_ascii_case(f.trim()))
                            .unwrap_or(true)
                })
        });
        if !any_stale {
            break;
        }
        if round < MAX_ROUNDS {
            log.push_str("still stale after warm — re-probing and retrying\n");
        }
    }
    log.push_str(&format!("warmed {warmed_total} repo index(es)\n"));
    if fail {
        anyhow::bail!("{log}");
    }
    Ok(log)
}

async fn verify_repo_index_fresh(
    client: &reqwest::Client,
    fqdn: &str,
    repo: &str,
) -> Result<bool> {
    let url = format!("https://{fqdn}/api/status");
    let st: serde_json::Value = client.get(&url).send().await?.error_for_status()?.json().await?;
    let fresh = st
        .get("repos")
        .and_then(|r| r.as_array())
        .and_then(|rows| {
            rows.iter().find(|row| row.get("name").and_then(|n| n.as_str()) == Some(repo))
        })
        .and_then(|row| row.get("index_fresh"))
        .and_then(|v| v.as_bool());
    Ok(fresh == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_env_defaults_piper_on() {
        let s = features_env(None, None, None, None, None, None, None, None, None);
        assert!(s.contains("RELAY_FEAT_PIPER=1"));
        assert!(s.contains("RELAY_FEAT_NPM_PKGS=\n"));
        assert!(s.contains("RELAY_FEAT_INBOUND_MAIL=0"));
        assert!(s.contains("RELAY_INSTALL_NONINTERACTIVE=1"));
    }

    #[test]
    fn features_env_sets_flags() {
        let s = features_env(
            Some("1"),
            Some("a,b"),
            Some("1"),
            Some("x"),
            Some("0"),
            Some("1"),
            Some("example.com mail.example.com"),
            Some("relaymail"),
            Some("mail.example.com"),
        );
        assert!(s.contains("RELAY_FEAT_PIPER=1"));
        assert!(s.contains("RELAY_FEAT_NPM_PKGS=a,b"));
        assert!(s.contains("RELAY_FEAT_TRANSLATION=1"));
        assert!(s.contains("RELAY_FEAT_LIBRETRANSLATE=0"));
        assert!(s.contains("RELAY_FEAT_INBOUND_MAIL=1"));
        assert!(s.contains("RELAY_MAIL_DOMAINS=example.com mail.example.com"));
        assert!(s.contains("RELAY_MAIL_USER=relaymail"));
        assert!(s.contains("RELAY_MAIL_HOSTNAME=mail.example.com"));
    }

    #[test]
    fn sync_remote_fetches_does_not_force_push() {
        assert!(SYNC_REMOTE.contains("relay-startup-catalog-sync.sh"));
        assert!(SYNC_REMOTE.contains("git --git-dir="));
        assert!(SYNC_REMOTE.contains(" fetch "));
        assert!(SYNC_REMOTE.contains("ensure-bare"));
        assert!(SYNC_REMOTE.contains("authorized_repos"));
        assert!(!SYNC_REMOTE.contains("push --force"));
        assert!(!SYNC_REMOTE.contains("+refs/heads"));
        assert!(!SYNC_REMOTE.contains("reset --hard"));
        assert!(!SYNC_REMOTE.contains("rm -rf"));
    }
}

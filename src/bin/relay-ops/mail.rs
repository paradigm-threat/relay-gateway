//! Inbound mailbox reader: SSH to the single MX host (pub1) and list/read Maildir.
//!
//! Remote Python is the shared helper at `scripts/maildir_inbox.py` (include_str!).
//! Same semantics as QuizGate batch SI inbound mail and the Inbox UI.
use crate::config::OpsConfig;
use crate::ssh::{output_text, SshCtx};
use crate::topology::FleetFile;
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAIL_HOST: &str = "pub1.relaygateway.net";
pub const DEFAULT_MAILDIR: &str = "/home/arl/Maildir";

/// Shared Maildir helper (canonical source: scripts/maildir_inbox.py).
const MAILDIR_INBOX_PY: &str = include_str!("../../../scripts/maildir_inbox.py");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailListItem {
    pub id: String,
    pub folder: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub unread: bool,
    pub mtime: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailMessage {
    pub id: String,
    pub folder: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub unread: bool,
    /// Best-effort plain text (HTML stripped / quoted-printable decoded).
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct MailTarget {
    pub host: String,
    pub maildir: String,
}

impl MailTarget {
    pub fn resolve(cfg: &OpsConfig, fleet: &FleetFile) -> Self {
        let host = cfg
            .mail_host
            .clone()
            .or_else(|| fleet.mail.as_ref().and_then(|m| m.host.clone()))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MAIL_HOST.to_string());
        let maildir = cfg
            .mail_maildir
            .clone()
            .or_else(|| fleet.mail.as_ref().and_then(|m| m.maildir.clone()))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MAILDIR.to_string());
        Self { host, maildir }
    }
}

/// Maildir filenames are safe tokens (timestamp.hash.host); reject path chars.
pub fn validate_mail_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 200 {
        bail!("invalid mail id");
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.contains('\0') {
        bail!("invalid mail id");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ':')
    {
        bail!("invalid mail id");
    }
    Ok(())
}

fn ssh_ctx<'a>(cfg: &'a OpsConfig, fleet: &'a FleetFile) -> SshCtx<'a> {
    SshCtx {
        cfg,
        fleet,
        interactive: false,
    }
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Run shared maildir_inbox.py via `sudo python3 -c` with script as base64.
/// Remote argv after decode: maildir_inbox.py <cmd> <maildir> [id…]
fn run_mail_py(cfg: &OpsConfig, fleet: &FleetFile, cmd: &str, extra: &[&str]) -> Result<String> {
    let target = MailTarget::resolve(cfg, fleet);
    for a in extra {
        validate_mail_id(a)?;
    }
    let enc = base64::engine::general_purpose::STANDARD.encode(MAILDIR_INBOX_PY.as_bytes());
    let mut remote = format!(
        "sudo python3 -c 'import base64,sys; code=base64.standard_b64decode(sys.argv[1]); sys.argv=[\"maildir_inbox.py\"]+sys.argv[2:]; exec(code.decode())' {} {} {}",
        shell_single_quote(&enc),
        shell_single_quote(cmd),
        shell_single_quote(&target.maildir),
    );
    for a in extra {
        remote.push(' ');
        remote.push_str(&shell_single_quote(a));
    }
    let ssh = ssh_ctx(cfg, fleet);
    let out = ssh
        .run(&target.host, &remote)
        .with_context(|| format!("ssh mail {}", target.host))?;
    let merged = output_text(&out);
    if !out.status.success() {
        bail!("mail remote failed on {}: {}", target.host, merged.trim());
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        bail!(
            "mail remote empty stdout on {}: {}",
            target.host,
            merged.trim()
        );
    }
    Ok(stdout)
}

pub fn list_messages(cfg: &OpsConfig, fleet: &FleetFile) -> Result<Vec<MailListItem>> {
    let stdout = run_mail_py(cfg, fleet, "list", &[])?;
    let items: Vec<MailListItem> =
        serde_json::from_str(&stdout).with_context(|| format!("parse mail list: {stdout}"))?;
    Ok(items)
}

pub fn get_message(cfg: &OpsConfig, fleet: &FleetFile, id: &str) -> Result<MailMessage> {
    validate_mail_id(id)?;
    let stdout = run_mail_py(cfg, fleet, "get", &[id])?;
    let msg: MailMessage =
        serde_json::from_str(&stdout).with_context(|| format!("parse mail message: {stdout}"))?;
    Ok(msg)
}

pub fn mark_read(cfg: &OpsConfig, fleet: &FleetFile, id: &str) -> Result<MailListItem> {
    validate_mail_id(id)?;
    let stdout = run_mail_py(cfg, fleet, "mark-read", &[id])?;
    let item: MailListItem =
        serde_json::from_str(&stdout).with_context(|| format!("parse mark-read: {stdout}"))?;
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_ids() {
        assert!(validate_mail_id("../etc/passwd").is_err());
        assert!(validate_mail_id("a/b").is_err());
        assert!(validate_mail_id("1792248367.6634f390.prod-instance-17901615903190413").is_ok());
    }

    #[test]
    fn resolve_defaults() {
        let cfg = OpsConfig::default();
        let fleet = FleetFile {
            schema: "relay.ops.fleet.v1".into(),
            fleet_ingress: None,
            git_port: None,
            nodes: vec![crate::topology::FleetNode {
                fqdn: "iad1.relaygateway.net".into(),
                ssh_user: Some("ubuntu".into()),
                deploy_target: None,
                fleet_dns: true,
                role: None,
            }],
            git_repos: vec![],
            health: Default::default(),
            catalog_path: None,
            data_root: None,
            mail: None,
        };
        let t = MailTarget::resolve(&cfg, &fleet);
        assert_eq!(t.host, DEFAULT_MAIL_HOST);
        assert_eq!(t.maildir, DEFAULT_MAILDIR);
    }

    #[test]
    fn shared_helper_mentions_commands() {
        assert!(MAILDIR_INBOX_PY.contains("cmd_list"));
        assert!(MAILDIR_INBOX_PY.contains("mark-read"));
    }
}

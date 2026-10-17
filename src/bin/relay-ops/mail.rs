//! Inbound mailbox reader: SSH to the single MX host (iad1) and list/read Maildir.
use crate::config::OpsConfig;
use crate::ssh::{output_text, SshCtx};
use crate::topology::FleetFile;
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAIL_HOST: &str = "iad1.relaygateway.net";
pub const DEFAULT_MAILDIR: &str = "/home/arl/Maildir";

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

/// Run remote python via `sudo python3 -c` with script as base64 (avoids bash -s heredoc).
fn run_mail_py(
    cfg: &OpsConfig,
    fleet: &FleetFile,
    py: &str,
    extra: &[&str],
) -> Result<String> {
    let target = MailTarget::resolve(cfg, fleet);
    for a in extra {
        validate_mail_id(a)?;
    }
    let enc = base64::engine::general_purpose::STANDARD.encode(py.as_bytes());
    // sys.argv[1]=b64, [2]=maildir, [3…]=ids
    let mut remote = format!(
        "sudo python3 -c 'import base64,sys; exec(base64.standard_b64decode(sys.argv[1]).decode())' {} {}",
        shell_single_quote(&enc),
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
    let stdout = run_mail_py(cfg, fleet, REMOTE_LIST_PY, &[])?;
    let items: Vec<MailListItem> =
        serde_json::from_str(&stdout).with_context(|| format!("parse mail list: {stdout}"))?;
    Ok(items)
}

pub fn get_message(cfg: &OpsConfig, fleet: &FleetFile, id: &str) -> Result<MailMessage> {
    validate_mail_id(id)?;
    let stdout = run_mail_py(cfg, fleet, REMOTE_GET_PY, &[id])?;
    let msg: MailMessage =
        serde_json::from_str(&stdout).with_context(|| format!("parse mail message: {stdout}"))?;
    Ok(msg)
}

pub fn mark_read(cfg: &OpsConfig, fleet: &FleetFile, id: &str) -> Result<MailListItem> {
    validate_mail_id(id)?;
    let stdout = run_mail_py(cfg, fleet, REMOTE_MARK_READ_PY, &[id])?;
    let item: MailListItem =
        serde_json::from_str(&stdout).with_context(|| format!("parse mark-read: {stdout}"))?;
    Ok(item)
}

const REMOTE_LIST_PY: &str = r#"
import json, os, sys, email
from email.header import decode_header, make_header
root = sys.argv[2]
items = []
def dec(v):
    if not v:
        return ""
    try:
        return str(make_header(decode_header(v)))
    except Exception:
        return v if isinstance(v, str) else str(v)
for folder in ("new", "cur"):
    d = os.path.join(root, folder)
    if not os.path.isdir(d):
        continue
    for name in os.listdir(d):
        path = os.path.join(d, name)
        if not os.path.isfile(path):
            continue
        try:
            st = os.stat(path)
            with open(path, "rb") as f:
                raw = f.read(65536)
            msg = email.message_from_bytes(raw)
            items.append({
                "id": name,
                "folder": folder,
                "from": dec(msg.get("From", "")),
                "to": dec(msg.get("To", "")),
                "subject": dec(msg.get("Subject", "")),
                "date": dec(msg.get("Date", "")),
                "unread": folder == "new",
                "mtime": float(st.st_mtime),
            })
        except Exception as e:
            items.append({
                "id": name,
                "folder": folder,
                "from": "",
                "to": "",
                "subject": "(unreadable: %s)" % e,
                "date": "",
                "unread": folder == "new",
                "mtime": 0.0,
            })
items.sort(key=lambda x: x["mtime"], reverse=True)
print(json.dumps(items))
"#;

const REMOTE_GET_PY: &str = r#"
import json, os, sys, email, re, shutil
from email.header import decode_header, make_header
from html.parser import HTMLParser
root, mid = sys.argv[2], sys.argv[3]
path = folder = None
for fol in ("new", "cur"):
    p = os.path.join(root, fol, mid)
    if os.path.isfile(p):
        path, folder = p, fol
        break
if not path:
    sys.stderr.write("not found\n")
    sys.exit(2)
# Opening a message marks it read: Maildir new/ → cur/ (same basename).
if folder == "new":
    dst_dir = os.path.join(root, "cur")
    os.makedirs(dst_dir, exist_ok=True)
    dst = os.path.join(dst_dir, mid)
    if not os.path.isfile(dst):
        shutil.move(path, dst)
    else:
        # Collision: drop the unread copy; keep existing cur.
        try:
            os.remove(path)
        except OSError:
            pass
    path, folder = dst, "cur"
def dec(v):
    if not v:
        return ""
    try:
        return str(make_header(decode_header(v)))
    except Exception:
        return v if isinstance(v, str) else str(v)
class Stripper(HTMLParser):
    def __init__(self):
        super().__init__()
        self.parts = []
    def handle_data(self, d):
        self.parts.append(d)
    def text(self):
        return re.sub(r"\s+", " ", "".join(self.parts)).strip()
def part_payload(part):
    data = part.get_payload(decode=True)
    if data is None:
        p = part.get_payload()
        if isinstance(p, str):
            return p.encode("utf-8", "replace")
        return b""
    return data
def as_text(part):
    ctype = (part.get_content_type() or "").lower()
    charset = part.get_content_charset() or "utf-8"
    raw = part_payload(part)
    try:
        text = raw.decode(charset, "replace")
    except Exception:
        text = raw.decode("utf-8", "replace")
    if ctype == "text/html":
        text = re.sub(r"(?is)<(script|style)[^>]*>.*?</\1>", " ", text)
        s = Stripper()
        try:
            s.feed(text)
            return s.text()
        except Exception:
            return re.sub(r"<[^>]+>", " ", text)
    return text
with open(path, "rb") as f:
    raw = f.read()
msg = email.message_from_bytes(raw)
plain, html = [], []
if msg.is_multipart():
    for part in msg.walk():
        if part.get_content_maintype() == "multipart":
            continue
        ctype = (part.get_content_type() or "").lower()
        if ctype == "text/plain":
            plain.append(as_text(part))
        elif ctype == "text/html":
            html.append(as_text(part))
else:
    ctype = (msg.get_content_type() or "text/plain").lower()
    t = as_text(msg)
    if ctype == "text/html":
        html.append(t)
    else:
        plain.append(t)
body = "\n\n".join(plain).strip() or "\n\n".join(html).strip()
print(json.dumps({
    "id": mid,
    "folder": folder,
    "from": dec(msg.get("From", "")),
    "to": dec(msg.get("To", "")),
    "subject": dec(msg.get("Subject", "")),
    "date": dec(msg.get("Date", "")),
    "unread": False,
    "body": body,
}))
"#;

const REMOTE_MARK_READ_PY: &str = r#"
import json, os, sys, email, shutil
from email.header import decode_header, make_header
root, mid = sys.argv[2], sys.argv[3]
src = os.path.join(root, "new", mid)
dst_dir = os.path.join(root, "cur")
dst = os.path.join(dst_dir, mid)
if not os.path.isfile(src):
    cur = os.path.join(dst_dir, mid)
    if os.path.isfile(cur):
        path, folder = cur, "cur"
    else:
        sys.stderr.write("not found\n")
        sys.exit(2)
else:
    os.makedirs(dst_dir, exist_ok=True)
    shutil.move(src, dst)
    path, folder = dst, "cur"
def dec(v):
    if not v:
        return ""
    try:
        return str(make_header(decode_header(v)))
    except Exception:
        return v if isinstance(v, str) else str(v)
st = os.stat(path)
with open(path, "rb") as f:
    raw = f.read(65536)
msg = email.message_from_bytes(raw)
print(json.dumps({
    "id": mid,
    "folder": folder,
    "from": dec(msg.get("From", "")),
    "to": dec(msg.get("To", "")),
    "subject": dec(msg.get("Subject", "")),
    "date": dec(msg.get("Date", "")),
    "unread": False,
    "mtime": float(st.st_mtime),
}))
"#;

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
                spot_cloudspace: None,
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
}

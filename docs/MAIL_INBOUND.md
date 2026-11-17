# Inbound mail (receive-only)

Relay does **not** send email. Outbound SMTP is old-internet baggage; this stack is **inbox-only**: accept mail for your domains, drop it in a Maildir, read it from **relay-ops → Inbox** (or SSH). Future hooks/triggers can watch that Maildir without ever becoming a mail *sender*.

**relay-ops Inbox** is a **standard** fleet-console feature on every deployment (SSH-read of the configured Maildir). The **OpenSMTPD catcher** on a node is **optional** at install time — enable it when you want that node to be the MX.

| Piece | Detail |
|-------|--------|
| MTA | OpenSMTPD (`opensmtpd` package) |
| Listen | TCP **25** only (`ufw` opens `25/tcp` when the feature is on) |
| Mailbox | Local UNIX user → Maildir (default user `relaymail`) |
| Relay | Denied (foreign domains rejected at RCPT) |
| Send | Not configured |
| Read | **relay-ops** UI → **Inbox**, or SSH |

DNS MX is **never** written by the installer — wire MX/A/SPF yourself after the catcher is up.

---

## Install / reconfigure

Interactive (`install` / `reconfigure-features`):

```text
Enable inbound mail catcher on this node? [y/N]
Catch-all domains (space-separated) [required]:
Local UNIX mailbox user [relaymail]:
SMTP banner hostname [mail.<first-domain>]:
```

Non-interactive:

```bash
sudo RELAY_INSTALL_NONINTERACTIVE=1 \
  RELAY_FEAT_INBOUND_MAIL=1 \
  RELAY_MAIL_DOMAINS="example.com mail.example.com" \
  RELAY_MAIL_USER=relaymail \
  RELAY_MAIL_HOSTNAME=mail.example.com \
  ./install.sh reconfigure-features
```

| Variable | Meaning |
|----------|---------|
| `RELAY_FEAT_INBOUND_MAIL=1` | Install/configure OpenSMTPD catch-all |
| `RELAY_MAIL_DOMAINS` | Space- or comma-separated domains (required when enabled) |
| `RELAY_MAIL_USER` | Local UNIX user (default `relaymail`; created if missing) |
| `RELAY_MAIL_HOSTNAME` | SMTP banner hostname (default `mail.<first-domain>`) |
| `RELAY_MAIL_MAILDIR` | Override Maildir path (default `/home/<user>/Maildir`) |

Fleet remote:

```bash
./scripts/relay-fleet.sh features-set --only=NODE \
  --inbound-mail=1 \
  --mail-domains='example.com mail.example.com' \
  --mail-user=relaymail \
  --mail-hostname=mail.example.com
```

State is recorded under **`features.inbound_mail`** in **`/opt/relay/state/features.json`** and advertised in **`POST /api/config` → `installed_features`**. Config copy: **`/opt/relay/state/opensmtpd-inbound.conf`**. Live MTA: **`/etc/smtpd.conf`**.

See **[INSTALL_FEATURES.md](./INSTALL_FEATURES.md)**.

---

## DNS (operator step)

On **one** host only (a single mailbox — never multi-MX the mesh):

1. **A** (or AAAA) for the SMTP name → that host’s public IP  
2. **MX** for the apex (and any other receiving domains) → that SMTP name, priority `10`  
3. **SPF** receive-only: `v=spf1 -all` (this domain does not send)

Do **not** point MX at multi-A ingress (`fleet.*`) or at every mesh peer.

---

## Read mail

### relay-ops Inbox (preferred)

```bash
cargo build --release --bin relay-ops
./target/release/relay-ops serve
# UI: http://127.0.0.1:7780 → Inbox
```

Config: [`ops/fleet.json`](../ops/fleet.json) `mail.host` / `mail.maildir`, or env `RELAY_OPS_MAIL_HOST` / `RELAY_OPS_MAIL_MAILDIR`.

APIs: `GET /api/mail/list`, `GET /api/mail/message?id=…` (**opening marks read**: Maildir `new/` → `cur/`), `POST /api/mail/mark-read` `{"id":"…"}` (idempotent; also applied automatically when a message is opened in the Inbox UI).

### SSH (fallback)

```bash
ssh -i ~/.ssh/id_ed25519_relay_gateway ubuntu@MX_HOST \
  'sudo ls -la /home/relaymail/Maildir/new'
```

---

## Shared Maildir helper

Canonical reader used by **relay-ops Inbox** and batch consumers (e.g. QuizGate):

| Path | Role |
|------|------|
| [`scripts/maildir_inbox.py`](../scripts/maildir_inbox.py) | `list` / `get` / `mark-read` → JSON |
| `relay-ops` `mail.rs` | `include_str!` of that script; SSH + sudo on MX host |
| Host copy (iad1) | `/usr/local/lib/relay/maildir_inbox.py` |

```bash
python3 scripts/maildir_inbox.py list /home/arl/Maildir
sudo python3 /usr/local/lib/relay/maildir_inbox.py get /home/arl/Maildir <id>
```

Do **not** invent a parallel IMAP stack — extend this helper.

---

## Production — iad1 (multi-domain)

Live mesh: **`iad1` / `ord1` / `hkg1`**. Only **iad1** runs the catcher. **One** Maildir for all inbound domains.

| | |
|--|--|
| MX host | `iad1.relaygateway.net` (`162.209.124.53`) |
| Banner | `mail.paradigmthreat.net` |
| User / Maildir | `arl` / `/home/arl/Maildir` |
| SPF | `v=spf1 -all` on each apex (receive-only) |

| Domain | DNS name | Delivery |
|--------|----------|----------|
| `@paradigmthreat.net` | `mail.paradigmthreat.net` | Maildir |
| `@clevertree.net` | `mail.clevertree.net` | MDA → Maildir (+ optional msmtp Gmail notify) |
| `@quizgate.app` | `mail.quizgate.app` | Maildir (`batch@quizgate.app`) |

Live `/etc/smtpd.conf` is **surgically** multi-table (PT / CT / QG). The installer single-domain template still works for greenfield nodes; do not blindly `reconfigure-features` on iad1 without merging those tables.

Installer shape (PT-only greenfield / rebuild baseline):

```bash
sudo RELAY_INSTALL_NONINTERACTIVE=1 \
  RELAY_FEAT_INBOUND_MAIL=1 \
  RELAY_MAIL_DOMAINS="paradigmthreat.net mail.paradigmthreat.net" \
  RELAY_MAIL_USER=arl \
  RELAY_MAIL_HOSTNAME=mail.paradigmthreat.net \
  ./install.sh reconfigure-features
```

`ops/fleet.json`:

```json
"mail": {
  "host": "iad1.relaygateway.net",
  "maildir": "/home/arl/Maildir"
}
```

### Prove

From another mesh node (many residential ISPs block **outbound** :25):

```bash
# SMTP to mail.paradigmthreat.net:25, RCPT someone@paradigmthreat.net
# or on iad1: SMTP 127.0.0.1:25 RCPT batch@quizgate.app
```

Expect delivery under `/home/arl/Maildir/new` and `journalctl -u opensmtpd`.

QuizGate batch: `cd data-plane && npm run inbox` (see quizgate `docs/EMAIL.md`).

---

## Not in scope (v1)

IMAP, full webmail, outbound SMTP, DKIM signing, MX backup on secondary mesh nodes. Triggers that react to Maildir drops are a planned consumer of this inbox — not part of the catcher itself.

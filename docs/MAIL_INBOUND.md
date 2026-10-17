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

APIs: `GET /api/mail/list`, `GET /api/mail/message?id=…`, `POST /api/mail/mark-read` `{"id":"…"}`.

### SSH (fallback)

```bash
ssh -i ~/.ssh/id_ed25519_relay_gateway ubuntu@MX_HOST \
  'sudo ls -la /home/relaymail/Maildir/new'
```

---

## Production example — paradigmthreat.net

Live mesh: **`iad1` / `ord1` / `hkg1`**. Only **iad1** runs the catcher.

| | |
|--|--|
| MX host | `iad1.relaygateway.net` (`162.209.124.53`) |
| DNS name | `mail.paradigmthreat.net` → that IP |
| Domains | `@paradigmthreat.net`, `@mail.paradigmthreat.net` |
| User / Maildir | `arl` / `/home/arl/Maildir` |
| SPF | `v=spf1 -all` on the apex |

Non-interactive shape (already applied on iad1; re-run only if rebuilding):

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
```

Expect delivery under `/home/arl/Maildir/new` and `journalctl -u opensmtpd`.

---

## Not in scope (v1)

IMAP, full webmail, outbound SMTP, DKIM signing, MX backup on secondary mesh nodes. Triggers that react to Maildir drops are a planned consumer of this inbox — not part of the catcher itself.

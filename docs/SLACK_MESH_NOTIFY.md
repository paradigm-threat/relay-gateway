# Slack notifications (Relay → Slack)

**`relay-server`** and **`relay-hook-handler`** can post to Slack using an **[Incoming Webhook](https://api.slack.com/messaging/webhooks)**. This is **outbound only**. You do **not** need a bot token for basic alerts.

**Related:** **[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)** (mesh vs external chat).

### Internal mesh vs this doc

- **Relay does not depend on Slack** for sync, trust, or **anti–over-ping** on the mesh. Those rules are **Git / HTTP mesh** only — see **[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)** § **Mesh anti–over-ping strategy (internal)** and § **Target event envelope**.
- **Slack** (including optional **history** dedup for **`commit_update`**) is **operator UX**: fewer duplicate lines in a channel. It is **not** a substitute for **`new_sha`** dedup, **`via`**, or **hop limits** between servers.

---

## What gets posted (when **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`** is set)

| Event | Source | Notes |
|--------|--------|--------|
| **`server_started`** | **`relay-server`** after HTTP bind | Version, optional node / `relay_server_id`. |
| **`process_panic`** | Any process using **`notify::install_panic_hook`** | Rust panic location + message (best-effort). |
| **`commit_update`** | **`post-receive`** (git push), **`POST /git-pull`**, **`POST /hooks/peer`** | Includes a **`relay_dedup`** line for cross-server dedup (below). Skipped for all-zero (delete) SHAs. |
| **`pull_validation_failed`** | Mesh path (peer hook / git-pull anchor failure) | Same as before; no commit dedup. |
| **`git_pull_fetch_failed`** | **`POST /git-pull`** when **`git fetch`** fails | |
| **`hook_error`** | **`relay-hook-handler`** | Pre-receive branch rules, configured hook failure, auto-push setup failure. |
| **`auto_push_failed`** | **`relay-hook-handler`** | Per-origin **`git push`** failure from Auto-Push. |
| **Startup catalog sync** | **`relay-startup-catalog-sync.sh`** (**`ExecStartPre`** for **`relay-gateway`**) | Real-time lines: start, catalog forge sync result, peer **`git fetch`** round (ok / partial failures), finished. **HTTP always starts** even if forge sync fails; failures are **`catalog forge sync failed`** (warning) plus **`POST /api/config` → `startup_catalog_sync`**. Only when **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`** is set and **`RELAY_STARTUP_CATALOG_SYNC`** is on. See **`scripts/relay-startup-catalog-sync.sh`**. |

**Not Slack:** Git state between nodes is still synchronized by **Git** (**`git fetch`**, **`POST /hooks/peer`**, anchors, Auto-Push). Slack is **observability only**; dedup only reduces **duplicate chat lines**, not replication logic.

---

## 1. Create a Slack app (e.g. workspace “CleverTree”)

1. Open **[https://api.slack.com/apps](https://api.slack.com/apps)** (sign in if needed).
2. **Create New App** → **From scratch**.
3. **App name:** e.g. `Relay mesh alerts`.
4. **Pick a workspace:** choose your workspace (e.g. **CleverTree**).
5. In the app’s left sidebar, open **Incoming Webhooks**.
6. Turn **Activate Incoming Webhooks** **On**.
7. Click **Add New Webhook to Workspace**.
8. Choose the channel where alerts should appear (e.g. **`#general`**, or create **`#relay-alerts`** first under **Channels** in Slack).
9. **Copy** the webhook URL. It looks like:
   `https://hooks.slack.com/services/T…/B…/xxxxx`

Treat this URL like a **password**: anyone with it can post to that channel.

---

## 2. Environment variables on the Relay node

Add to **`relay.env`** (or your process environment):

| Variable | Required | Meaning |
|----------|----------|---------|
| **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`** | For Slack | The full Incoming Webhook URL from step 1. |
| **`RELAY_MESH_EVENT_CHANNEL`** | Optional | Included in **`pull_validation_failed`** payloads as `channel_hint`. |
| **`RELAY_SLACK_BOT_TOKEN`** | Optional | Bot User OAuth token (`xoxb-…`). Needed only for **cross-server commit dedup** (see §4). |
| **`RELAY_SLACK_DEDUP_CHANNEL_ID`** | Optional | Channel ID (`C…`) for the **same** channel the webhook posts to. Used with **`conversations.history`**. |

Mesh fan-out to other relays is **separate** (see **`RELAY_MESH_EVENT_*`** / **`RELAY_PEER_SECRET`**). You can use **Slack only**, **mesh only**, or **both**.

Example snippet:

```bash
RELAY_NOTIFY_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/T000/B000/xxxxxxxx
# Optional metadata line in pull_validation_failed messages:
# RELAY_MESH_EVENT_CHANNEL=#relay-alerts
```

**Bash:** If you **`source relay.env`**, quote semicolon-separated values, e.g.  
`RELAY_MASTER_PEER_LIST="host1.example.net;host2.example.net"`.

Restart **`relay-server`** after changing env.

---

## 3. Verify

Trigger any event above, or use curl (replace the URL):

```bash
curl -sS -X POST -H 'Content-type: application/json' \
  --data '{"text":"Relay Slack webhook test"}' \
  'https://hooks.slack.com/services/T…/B…/…'
```

---

## 4. Commit-update dedup (optional, cross-server)

**Problem:** Several relays may all reach the **same new tip** for repo `R` and SHA `S` (e.g. after mesh fetch). Without dedup, Slack shows one line per node.

**Mechanism:**

- Every **`commit_update`** message ends with a stable line:
  `relay_dedup kind=commit_update repo=<slug> sha=<40-hex>`
- Before posting, Relay can skip if either:
  1. **In-process cache** — same repo + SHA was posted from this process within **15 minutes**, or
  2. **Slack history** — if **`RELAY_SLACK_BOT_TOKEN`** and **`RELAY_SLACK_DEDUP_CHANNEL_ID`** are set, Relay calls **`conversations.history`** (last 75 messages) and skips if that marker already appears.

**Slack app setup for (2):**

1. **OAuth & Permissions** → **Bot Token Scopes**: add **`channels:history`** (and **`channels:read`** if Slack asks).
2. **Install to Workspace** (re-install after adding scopes).
3. Copy **Bot User OAuth Token** → **`RELAY_SLACK_BOT_TOKEN`**.
4. Channel ID: open the channel in Slack → **View channel details** → scroll to bottom for **Channel ID** (`C…`) → **`RELAY_SLACK_DEDUP_CHANNEL_ID`**.

Incoming Webhooks **cannot** read history; the bot is **read-only** for dedup; posting still uses the **webhook** URL.

---

## 5. Other Slack env vars (reference)

| Approach | Typical variables | Use case |
|----------|-------------------|----------|
| **Incoming Webhook** | **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`** | One-way alerts; default in Relay. |
| **Bot + history** | **`RELAY_SLACK_BOT_TOKEN`**, **`RELAY_SLACK_DEDUP_CHANNEL_ID`** | Cross-server **`commit_update`** dedup only (today). |
| **Bot + `chat.postMessage`** | Not wired | Could replace webhook later. |
| **Slack → your server** | **`SLACK_SIGNING_SECRET`** | Events API / slash commands — **not** used for these alerts. |

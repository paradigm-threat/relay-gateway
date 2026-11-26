# Mesh IRC (optional Relay feature pack)

Treat Ergo + per-node **irc-agent** as a sidecar ops bus complementary to HTTP peer hooks and `relay-ops` probing.

Shared browser hosts + channel prefixes (`fb` / `ct` / `pt`, `#logs-*`): [`RELAY_CHAT.md`](RELAY_CHAT.md).

## Architecture

- **Ergo** stays external (Flashback: `fan-edits/ops/chat` on the chat primary). Do not embed a full IRCd in `relay-gateway`.
- **Topology:** single Ergo network. **`pub1.relaygateway.net`** runs Ergo + chat-edge + irc-agent. **`mesh1` / `mesh2`** are `--edge-only` (socat `:6667` → pub1, Caddy `/chat/*` → pub1). Set `CHAT_PRIMARY_FQDN` / `RELAY_CHAT_PRIMARY_FQDN=pub1.relaygateway.net`.
- Each fleet node runs **irc-agent** (nick = short hostname: `pub1` / `mesh1` / `mesh2`). Historical Rackspace nicks `iad1` / `ord1` / `hkg1` are retired for this fleet.
- **Trim policy:** journal mirror is **warn/error only** (`PRIORITY ≤ 4`) into `#logs`. Curated `/emit` info is allowed on `#relay` / `#relay-stats` (tip apply, heartbeats); info is **not** accepted into `#logs*`. Ergo durable history is for **chat**, not server INFO.
- **Web traffic + RUM** stay on HTTP: `/api/status` `http` (`http_stats.sqlite`) and `/api/rum` — not IRC.
- Tip sync authority remains **`POST /hooks/peer`**. Fleet health authority remains **`relay-ops` probe**.

## Enable on gateway nodes

```bash
# systemd drop-in or relay.env
RELAY_IRC_EVENTS=1
RELAY_MESH_EVENT_CHANNEL=#relay
# optional override:
# RELAY_IRC_EMIT_URL=http://127.0.0.1:8099/emit
```

When set, the gateway POSTs tip apply / mesh validation / park rewrite lines to the local agent emit API. Install the agent via the site `ops/chat/install.sh` (primary or `--edge-only`).

## Enable brain → IRC bridge (laptop)

```bash
RELAY_OPS_IRC=1
# RELAY_OPS_IRC_HOST=flashbackrepository.org
# RELAY_OPS_IRC_PORT=6667
# RELAY_OPS_IRC_CHANNELS=#relay,#logs
relay-ops brain serve   # or relay-brain
```

The brain joins as nick `RelayOps`, appends channel traffic into the JSONL feed, and accepts safe bot commands that hit the same `/chat` allowlist as CLI:

| IRC | Brain |
| --- | ----- |
| `!status` | overall brain status |
| `!status pub1` | last classification for that node |
| `!pause` / `!resume` / `!blocked` | same as `relay-ops brain say` |

UI **Brain** panel shows `IRC bus: up` when the subscriber is registered.

## Non-goals

- Replacing Ergo with an in-binary IRCd
- Dumping access logs / visitor IPs into public channels
- Using IRC as the git mesh transport

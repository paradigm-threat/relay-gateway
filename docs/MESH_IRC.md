# Mesh IRC (optional Relay feature pack)

Treat Ergo + per-node **irc-agent** as a sidecar ops bus complementary to HTTP peer hooks and `relay-ops` probing.

Shared browser hosts + channel prefixes (`fb` / `ct` / `pt`, `#logs-*`): [`RELAY_CHAT.md`](RELAY_CHAT.md).

## Architecture

- **Ergo** stays external (Flashback: `ops/chat` on the chat primary). Do not embed a full IRCd in `relay-gateway`.
- Each fleet node runs **irc-agent** (nick = short hostname: `iad1` / `ord1` / `hkg1`) publishing into `#logs`, `#relay`, `#relay-stats`.
- Tip sync authority remains **`POST /hooks/peer`**. Fleet health authority remains **`relay-ops` / `relay-brain` probe**.

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
| `!status iad1` | last classification for that node |
| `!pause` / `!resume` / `!blocked` | same as `relay-ops brain say` |

UI **Brain** panel shows `IRC bus: up` when the subscriber is registered.

## Non-goals

- Replacing Ergo with an in-binary IRCd
- Dumping access logs / visitor IPs into public channels
- Using IRC as the git mesh transport

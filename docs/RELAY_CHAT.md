# Relay mesh chat (shared hosts + channel prefixes)

Fleet chat uses one Ergo network (Flashback sidecar) and **shared park-native hosts** published in **`relay-web-assets`** at `assets/chat/*.host.js` (same commit pin as embed.js).

## Master channel inventory (IaC)

Product/ops channels that must exist after rebuild live in **`relay-master/catalog/irc-channels.json`**. Per-repo defaults are `catalog/repos/<repo>.json` → `chat.default_channel`. Chat install seeds Ergo/chat-edge from that inventory (`fan-edits/ops/chat/scripts/ensure-irc-channels.sh`). Clevertree room `channel-policy.json` `product_hosts` / `website_default_channels` are refreshed from the catalog via `clevertree-chat/scripts/generate_channel_policy_from_catalog.py`.

## Channel organization

Short **repo prefixes** avoid collisions; sites may join any channel.

| Prefix | Catalog repo | Lobby | Pages | Movies / docs | Ops logs |
|--------|--------------|-------|-------|---------------|----------|
| `fb` | fan-edits | `#flashback` (legacy) / `#fb` | `#page-*` (legacy) / `#fb-page-*` | `#movie-*` / `#fb-movie-*` | `#logs-fb` |
| `ct` | clevertree-site | `#ct` | `#ct-page-*` | — | `#logs-ct` |
| `pt` | paradigm-threat-files | `#pt` | `#pt-page-*` | `#pt-doc-*` | `#logs-pt` |

**Fleet-wide (not per-repo):** `#relay` (mesh tip / agent notices), `#relay-stats` (light usage heartbeats), `#logs` (**warn/error journal only** — not INFO dumps). Durable Ergo history is chat channels (`#flashback`, `#movie-*`, `#pt-*`, …); ops channels are ephemeral relative to chat. Web traffic/debug stats live on `/api/status` `http` + `/api/rum`, not IRC.

Placeholder attrs:

```html
<div
  data-relay-park-native="fb-live-chat"
  data-channel="ct"
  data-prefix="ct"
  data-path="/"
  data-chat-origin="" 
></div>
```

- `data-prefix` — short repo id (`fb` / `ct` / `pt`); used when `data-channel` omitted to build `prefix-page-…`
- `data-chat-origin` — optional absolute origin for `/chat/api` + WS (default: same-origin; park Caddy proxies `/chat/*` to the chat primary)

## Publishing hosts

Canonical sources: `relay-client-web/park-native/chat/`.  
`scripts/publish-web-assets.sh` copies them into `relay-web-assets/assets/chat/` on every embed publish.

## Etymology

Relay echoes IRC: servers hop messages and heal after netsplits — the same mental model as tip mesh and multi-A edges. Chat is the human/pubsub plane; `POST /hooks/peer` and `relay-ops` probe remain authoritative.

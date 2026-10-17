# Relay mesh chat (shared hosts + channel prefixes)

Fleet chat uses one Ergo network (Flashback sidecar) and **shared park-native hosts** published in **`relay-web-assets`** at `assets/chat/*.host.js` (same commit pin as embed.js).

## Channel organization

Short **repo prefixes** avoid collisions; sites may join any channel.

| Prefix | Catalog repo | Lobby | Pages | Movies / docs | Ops logs |
|--------|--------------|-------|-------|---------------|----------|
| `fb` | fan-edits | `#flashback` (legacy) / `#fb` | `#page-*` (legacy) / `#fb-page-*` | `#movie-*` / `#fb-movie-*` | `#logs-fb` |
| `ct` | clevertree-site | `#ct` | `#ct-page-*` | — | `#logs-ct` |
| `pt` | paradigm-threat-files | `#pt` | `#pt-page-*` | `#pt-doc-*` | `#logs-pt` |

**Fleet-wide (not per-repo):** `#relay`, `#relay-stats`, `#logs` (node journal aggregate).

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

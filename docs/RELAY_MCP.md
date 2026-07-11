# Relay MCP (Streamable HTTP)

Per-repo MCP over **`POST /api/mcp/:repo`**. Search tools **do not** reimplement ranking — they call the same **QUERY / search plugin** path as HTTP `QUERY`.

## Enable a repo

1. Add **`mcp.yaml`** at the repo root (tool catalog).
2. Wire **`.relay.yaml`**:

```yaml
server:
  hooks:
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs   # import { search } from '@relay/search/sqlite'
    mcp:
      path: hooks/server/mcp.cs     # enables MCP endpoint
  search:
    plugin: sqlite                  # default
```

3. Keep CleverScript **thin** — return `{ plugin, collection, query }`; never scan corpora in CS.

## Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/mcp/:repo?health=1` | Health JSON (`mcpEnabled`, `toolCount`) |
| `POST` | `/api/mcp/:repo` | JSON-RPC (`initialize`, `tools/list`, `tools/call`, `ping`) |
| `DELETE` | `/api/mcp/:repo` | Stateless session teardown (HTTP 204) |

Optional header: **`X-Relay-Branch`** (default `main`).

## `mcp.yaml`

```yaml
name: my-repo
version: "0.1.0"
tools:
  search_site:
    aliases: [search]
    via: query
    collection: index
  get_site_claims:
    via: json_match
    source: site-claims.json
  get_event_dates:
    via: events_lookup
    source: data/events.json
  research_status:
    via: research_status
    collection: index
  read_article:
    aliases: [fetch]
    via: git_read
```

| `via` | Behavior |
|-------|----------|
| `query` | [`execute_search_limited`](../src/git/query.rs) — same as HTTP QUERY / sqlite |
| `git_read` | Slice a git blob by `path` (rejects `..` traversal) |
| `json_match` | AND-match terms (or `id`) against a JSON array / `{claims\|entries}` file |
| `events_lookup` | Find timeline event by `eventId` / `titleQuery`; return signed year + calendar / blip-adjusted elapsed |
| `research_status` | Coverage envelope from QUERY hits (`not_in_corpus` / `partial`) |

`tools/list` ships per-`via` **inputSchema** (overridable with `inputSchema:` in yaml) and `readOnlyHint: true`. Tool failures use MCP **`isError: true`** content results (not JSON-RPC errors) when `tools/call` itself is valid.

## Cursor / HTTP client

```json
{
  "mcpServers": {
    "my-repo": {
      "url": "https://<relay-host>/api/mcp/my-repo"
    }
  }
}
```

## Relationship to paradigm-threat-site

The Next.js **`/api/mcp`** on paradigmthreat.net remains the aggregated public research MCP (claims + chronology + Neon submit). Relay MCP exposes **per-bare-repo** tools for hosts that talk to the gateway directly.

## Validate

```bash
node scripts/validate-relay-mcp.mjs http://127.0.0.1:8080 paradigm-threat-files
# or:
curl -sS 'http://127.0.0.1:8080/api/mcp/paradigm-threat-files?health=1'
```

See also [`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md), [`SERVER_HOOKS.md`](./SERVER_HOOKS.md).

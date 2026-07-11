# CleverScript QUERY hooks

When `.relay.yaml` sets **`server.hooks.query.path`** to a file ending in **`.cs`**, the relay-gateway runs that source with **relay-runtime** (CleverScript) for each QUERY request.

## Preferred contract: import the search plugin

CleverScript must **only route**. Heavy search/index work is a **Rust search plugin** exposed as a virtual module (same pattern as `@relay/git`):

```js
import { search } from '@relay/search/sqlite';

export default function run(ctx) {
  return search(ctx); // { rows: [...] } from Rust FTS / filter
}
```

Legacy **redb**: `import { search } from '@relay/search/redb'`.

```yaml
server:
  hooks:
    query:
      path: hooks/server/query.cs
    index:
      path: hooks/server/index.cs
  search:
    plugin: sqlite
```

**Do not** in CleverScript: scan `index.json`, rank hits, open SQLite, or rebuild indexes.

### Return shapes

| Return | Behavior |
|--------|----------|
| `{ rows: [...] }` / array | Result from `search(ctx)` (preferred) or legacy CS-built rows |
| `{ plugin, collection?, query?, limit? }` | Compat thin string router — Rust runs the named plugin |
| `null` | Fall through to `server.search.plugin` (default sqlite) |

Compat string router (still accepted):

```js
export default function run(ctx) {
  return {
    plugin: "sqlite",
    collection: ctx.collection,
    query: ctx.query
  };
}
```

## Default database: SQLite

On index / JIT QUERY, the gateway builds **`.relay_data/branches/<hash>/index.db`**:

- Syncs `data/**/meta.yaml` → `index.db.json` → SQLite (+ legacy redb)
- Ingests catalog **`index.json`** (flattened paths) into collection `index`
- Ingests **`data/events.json`** into collection `events`

String QUERY uses FTS (AND of terms). Object QUERY uses exact field match.

Legacy **`redb`** remains available via `@relay/search/redb` or `plugin: "redb"` / `server.search.plugin: redb`.

## Configuration

```yaml
server:
  hooks:
    query:
      path: hooks/server/query.cs
    # branches: [ "main" ]   # optional QUERY allowlist
```

If **`server.hooks.query.branches`** is omitted, QUERY branch policy falls back to **`server.hooks.index.branches`**.

## Script context

- First parameter: `branch`, `collection`, `query`, `repo`, `defaultPlugin`, `limit`
- Search: `import { search } from '@relay/search/sqlite'` (or `@relay/search/redb`)
- Git reads: `import { readFile } from '@relay/git'` (routing only — prefer search modules for QUERY)
- **`@relay/image`** is not available on QUERY

## MCP

`search_site` on **`POST /api/mcp/:repo`** calls the same [`execute_search`](../src/git/query.rs) path. See [`RELAY_MCP.md`](./RELAY_MCP.md).

## HTTP QUERY URLs

Both forms work after gateway **0.3.14+**:

```bash
# Repo subdomain (QUERY on `/`)
curl -X QUERY "https://paradigm-threat-files.dfw1.relaygateway.net/" \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","query":"venus"}'

# Node apex + /{repo} (same idea as /api/mcp/:repo)
curl -X QUERY "https://dfw1.relaygateway.net/paradigm-threat-files" \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","query":"venus"}'
```

Optional: `X-Relay-Branch` (default `main`).

## Related

- [ATLANTA3_QUERY_ROLLOUT.md](./ATLANTA3_QUERY_ROLLOUT.md) — staged deployment
- [REPODB_PLAN.md](./REPODB_PLAN.md) — per-branch `.relay_data` layout

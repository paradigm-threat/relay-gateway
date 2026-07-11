# CleverScript QUERY hooks

When `.relay.yaml` sets **`server.hooks.query.path`** to a file ending in **`.cs`**, the relay-gateway runs that source with **relay-runtime** (CleverScript) for each QUERY request.

## Preferred contract: thin plugin router

CleverScript must **only route**. Heavy search/index work is a **Rust search plugin** (default **`sqlite`**).

```js
export default function run(ctx) {
  return {
    plugin: "sqlite",       // or "redb" (legacy)
    collection: ctx.collection,
    query: ctx.query
  };
}
```

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
| `{ plugin, collection?, query?, limit? }` | Rust runs the named plugin |
| `null` | Fall through to `server.search.plugin` (default sqlite) |
| `{ rows: [...] }` or a JSON array | Legacy full CS result (compat only; avoid for new repos) |

## Default database: SQLite

On index / JIT QUERY, the gateway builds **`.relay_data/branches/<hash>/index.db`**:

- Syncs `data/**/meta.yaml` → `index.db.json` → SQLite (+ legacy redb)
- Ingests catalog **`index.json`** (flattened paths) into collection `index`
- Ingests **`data/events.json`** into collection `events`

String QUERY uses FTS (AND of terms). Object QUERY uses exact field match.

Legacy **`redb`** remains available via `plugin: "redb"` / `server.search.plugin: redb`.

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

- First parameter: `branch`, `collection`, `query`, `repo`, `defaultPlugin`
- Git reads: `import { readFile } from '@relay/git'` (routing only — prefer plugins for search)
- **`@relay/image`** is not available

## MCP

`search_site` on **`POST /api/mcp/:repo`** calls the same [`execute_search`](../src/git/query.rs) path. See [`RELAY_MCP.md`](./RELAY_MCP.md).

## Related

- [ATLANTA3_QUERY_ROLLOUT.md](./ATLANTA3_QUERY_ROLLOUT.md) — staged deployment
- [REPODB_PLAN.md](./REPODB_PLAN.md) — per-branch `.relay_data` layout

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
| `{ plugin, collection?, query?, limit?, order? }` | Compat thin string router — Rust runs the named plugin |
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

## Default database: SQLite SearchStore

Indexing runs **on commit** (post-receive, peer-fetch, HTTP PUT, `POST /api/index/warm/:repo`). QUERY / MCP are **read-only**: they never rebuild the catalog. Freshness is **SearchStore `docs_indexed_head` only** — leftover `index.db.json` metadata must not skip ingest. If the store lags the git tip, rows still return with **`indexFresh: false`**. Operators re-index with a push or `/api/index/warm` (warm always regenerates from git). Mesh hop cost and whether origin ACK waits on ingest: **[`RELAY_PUBLISH_ONCE.md`](./RELAY_PUBLISH_ONCE.md)** (plan, not implemented).

The v1 backend is **one SQLite file per node** (`RELAY_SEARCH_DB` or `{repo_root}/.relay_search.db`) with `repo` + `branch` columns and a long-lived WAL connection. Per-branch `.relay_data/branches/<hash>/index.db` is legacy and is deleted after a successful on-commit index. Do not import old files — regen from git.

On index / post-receive the gateway:

- Syncs `data/**/meta.yaml` → `index.db.json` → SearchStore (+ legacy redb)
- Ingests catalog sources from **`search.yaml`** (or builtins): `index.json`, `data/events.json`, `path_tree`, `content_events`
- QUERY JSON envelope includes `status`, `indexFresh`, `docCount`, `indexedHead`, `tookMs`

### `search.yaml` (recommended)

Repo-root declarative config (auto-loaded; override with `search({ ...ctx, config: "other.yaml" })`):

```yaml
plugin: sqlite
collections:
  index:
    sources:
      - type: meta_yaml
        glob: data/**/meta.yaml
      - type: path_index
        path: index.json
        mode: merge          # merge|replace
    idFields: [path, _meta_dir, id]
    ftsFields: [_title, title, _keywords, keywords, description]
    filterFields: [content_kind, type, path, id]   # portable object QUERY keys
    sortFields: [_updated, path, sort_key, md_path]
  events:
    sources:
      - type: events_json
        path: data/events.json
    idFields: [id]
    ftsFields: [title, keywords, name]
```

Without `search.yaml`, builtins still ingest meta.yaml + `index.json` (replace) + `data/events.json`. Default **`filterFields`**: `content_kind`, `type`, `path`, `id`, `channel`, `author`, `title`, `_keywords`. Default **`sortFields`**: `path`, `sort_key`, `md_path`, `created`, `_updated`. Undeclared object-filter keys yield an **empty result** (not a SQL `json_extract` of every key).

String QUERY uses FTS (AND of terms + prefix). Object QUERY uses exact field match on declared filter fields.

### Portable backends (SQLite now; Postgres / NoSQL later)

Indexing (git sources, on-commit, regen) does not depend on the engine. QUERY and upsert go through `SearchStore`. v1 is SQLite (`RELAY_SEARCH_STORE=sqlite`, default). `postgres` and `meilisearch` are reserved names — they must implement the **same** declared-schema contract:

- **Filter** only keys listed in `filterFields` (or builtins). Never “any JSON key.”
- **Sort** tokens must appear in the collection’s `sortFields` (plus `random`, which is a scan mode). HTTP tokens: `path`, `-path`, `sort_key`, `-sort_key`, `_updated`, `-_updated`, `random`.
- **`order=random`** is shuffled in Rust after a capped candidate fetch (not SQL `RANDOM()` / Meili ranking). Postgres may use SQL `random()` as an optimization behind the same limit/cap.
- **FTS** is AND of tokens + prefix (`iran*`). Engine-specific ranking is not part of the HTTP contract.
- Identity is `(repo slug, branch, collection)` — never hostname. Shared-catalog later is `RELAY_DATABASE_URL` plus a second backend on this trait.

Writes (post-receive index) may be slow; QUERY must stay a read.

Legacy **`redb`** remains available via `@relay/search/redb` or `plugin: "redb"` / `server.search.plugin: redb`.

## Probe

```bash
node scripts/validate-relay-query.mjs https://iad1.relaygateway.net paradigm-threat-files venus
```

Asserts `status != not_indexed`, `docCount > 0`, and prints `indexFresh` / `tookMs`.

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

- First parameter: `branch`, `collection`, `query`, `repo`, `defaultPlugin`, `limit`, `order`
- Search: `import { search } from '@relay/search/sqlite'` (or `@relay/search/redb`)
- Git reads: `import { readFile } from '@relay/git'` (routing only — prefer search modules for QUERY)
- **`@relay/image`** is not available on QUERY

## MCP

`search_site` on **`POST /api/mcp/:repo`** calls the same [`execute_search`](../src/git/query.rs) path. See [`RELAY_MCP.md`](./RELAY_MCP.md).

## HTTP QUERY URLs

Both forms work after gateway **0.3.14+**:

```bash
# Repo subdomain (QUERY on `/`)
curl -X QUERY "https://paradigm-threat-files.iad1.relaygateway.net/" \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","query":"venus"}'

# Node apex + /{repo} (same idea as /api/mcp/:repo)
curl -X QUERY "https://iad1.relaygateway.net/paradigm-threat-files" \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","query":"venus"}'
```

### URL query string (allowlisted)

`QUERY` also reads these query-string keys (JSON body overrides QS):

| Key | Meaning |
|-----|---------|
| `q` | FTS string (max 200 chars) |
| `collection` | Collection name (default `index`) |
| `limit` | Row cap (`1..=10000`; **`1..=100` when `order=random`**) |
| `order` | `path` (asc, default), `-path`, `sort_key`, `-sort_key`, `_updated`, `-_updated`, or `random`. Token must be in collection `sortFields` (`random` always allowed). Timeline events should use `sort_key` (falls back to `md_path`/`path` when unreindexed). |

Object `query` (JSON body) supports field predicates on **declared** `filterFields` (builtins if omitted):

| Shape | Meaning |
|-------|---------|
| `{ "content_kind": "article" }` | Exact string match |
| `{ "content_kind": ["article", "blog"] }` | `IN` (max 32 values) |
| `{ "content_kind": { "$in": ["article", "blog"] } }` | Same as array shorthand |
| `{ "content_kind": { "$ne": "exclude" } }` | Not equal (missing field matches) |
| `{ "type": { "$ne": ["manifest"] } }` | Not in list |

Predicates are applied on declared `filterFields` first, then `order=random` shuffles a capped candidate set in Rust so `limit=N` is N matching rows. Invalid / unsafe shapes yield an empty result set (never interpolated SQL).

Unknown keys are ignored. Invalid `order` → HTTP 400. `order` must parse as a known token **and** be listed in collection `sortFields` (`random` is always allowed).

```bash
# Curl-friendly: no body
curl -X QUERY 'https://paradigm-threat-files.iad1.relaygateway.net/?q=venus&limit=10&order=-path' \
  -H 'Accept: application/json'

curl -X QUERY 'https://paradigm-threat-files.iad1.relaygateway.net/?collection=index&order=random&limit=1' \
  -H 'Accept: application/json'

# Feeling Lucky-style: random articles only (JSON body)
curl -X QUERY 'https://paradigm-threat-files.iad1.relaygateway.net/' \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","order":"random","limit":40,"query":{"content_kind":["article","investigation","blog"]}}'
```

Optional: `X-Relay-Branch` (default `main`).

## Site-shell SSR vs QUERY (do not mix)

Park site-shell SSR must **not** drive DirNav / ChangeLog / article lists via QUERY (or same-Host HTTP `fetch`). Reasons:

1. **Worker deadlock / stall** — CleverScript site-shell render that self-HTTP QUERY’s the same Host blocks the gateway worker (index mutex / request loop). `RepoFetchHost` offers in-process QUERY only as an escape hatch for rare SSR needs; park tip policy still forbids P2 catalog on SSR.
2. **HTML cache miss cost** — `ssr_html_cache` is tip-OID keyed. Catalog work inside the miss path pays QUERY / card fan-out / cold WASM on every push (the old ~10s path). Keep SSR to **P0 article + P1 chrome**; placeholders for catalog (`data-pt-*-ssr`).
3. **Optimal load** — hydrate when `isClientLive()`, then QUERY the index (module-warm one HTTP per spec). Recency is `order=-_updated`; DirNav is `order=path` + `content_kind: { $ne: "exclude" }`; Feeling Lucky is `order=random`. Same-Host QUERY during SSR remains forbidden.

Normative tip tiers + cache table: park checkout `hooks/CLEVERSCRIPT_TIP.md`. Gateway HTML cache: [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md) § SSR HTML cache.

## Hydrate catalog = QUERY (2026-09-12)

Park ChangeLog / DirNav / Feeling Lucky / neighbors hydrate from QUERY. **Do not** ship a second catalog (`nav-index.txt`, `changelog-thumbs.json`) or fetch 10k rows to `sortBy` in CleverScript. **Do not** “fix” hydrate cost by moving catalog into SSR.

`search.yaml` `filterFields` / `sortFields` is the repo customization. `path_tree` already stamps `content_kind`, `_updated`, `_title`, `image_src`. Named extra collections that copy `path_tree` are not better.

### Hydrate QUERY specs (park tip)

| Surface | QUERY |
|---|---|
| ChangeLog Articles | `collection:index`, `query:{content_kind:article}`, `order:-_updated`, `limit:8` |
| Investigations / Blog | same, `investigation` / `blog` |
| Recent | `query:{content_kind:["article","investigation","blog"]}`, `order:-_updated`, `limit:8` |
| DirNav / neighbors / AutoIndex | `query:{content_kind:{$ne:exclude}}`, `order:path`, `limit:10000`, then `@relay/catalog.buildDirTree` |
| Feeling Lucky | `order:random`, `limit:40`, kind IN article/investigation/blog |

Tree helpers stay in `@relay/catalog` (`buildDirTree`, `dirExpandPaths`, `neighborPaths`). Do **not** import `@relay/search/sqlite` in client hooks.

### Still done / still out of scope

- **QUERY result cache (gateway)** — in-process LRU (`git/query_cache.rs`) keyed by `(repo, tipHead, collection, query, limit, order)`, TTL ~45s, max 64 entries; skipped for `order=random`.
- **Comment-scenario caching** — if/when comments are a QUERY collection, cache list-by-path in the same LRU; do not fold comment bodies into site-shell SSR HTML cache.
- **No `collection: articles` table** that copies `path_tree`.

ChangeLog card images use **`image_src`** on markdown `path_tree` docs (stem/slug match at ingest).

## Related

- [CS_HOST_HELPERS.md](./CS_HOST_HELPERS.md) — CS routes; Rust `@relay/search/*` (SQLite FTS on the gateway)
- [PARADIGM_PARK_LOCAL.md](./PARADIGM_PARK_LOCAL.md) — local QUERY/SSR hardening
- [CS_HOST_HELPERS.md](./CS_HOST_HELPERS.md) — CS routes; Rust `@relay/search/*`
- [RELAY_POLICY_AND_PERFORMANCE.md](./RELAY_POLICY_AND_PERFORMANCE.md) — SSR HTML cache vs hydrate catalog work
- Park tip: `hooks/CLEVERSCRIPT_TIP.md` — P0–P3; catalog SSR anti-pattern

*Last updated: 2026-09-12*

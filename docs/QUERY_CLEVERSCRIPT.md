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

## Default database: SQLite

On index / JIT QUERY / **post-receive**, the gateway builds **`.relay_data/branches/<hash>/index.db`**:

- Syncs `data/**/meta.yaml` → `index.db.json` → SQLite (+ legacy redb)
- Ingests catalog sources from **`search.yaml`** (or builtins): `index.json`, `data/events.json`
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
  events:
    sources:
      - type: events_json
        path: data/events.json
    idFields: [id]
    ftsFields: [title, keywords, name]
```

Without `search.yaml`, builtins still ingest meta.yaml + `index.json` (replace) + `data/events.json`.

String QUERY uses FTS (AND of terms). Object QUERY uses exact field match.

Legacy **`redb`** remains available via `@relay/search/redb` or `plugin: "redb"` / `server.search.plugin: redb`.

## Probe

```bash
node scripts/validate-relay-query.mjs https://dfw1.relaygateway.net paradigm-threat-files venus
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
curl -X QUERY "https://paradigm-threat-files.dfw1.relaygateway.net/" \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","query":"venus"}'

# Node apex + /{repo} (same idea as /api/mcp/:repo)
curl -X QUERY "https://dfw1.relaygateway.net/paradigm-threat-files" \
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
| `order` | `path` (asc, default), `-path`, `sort_key`, `-sort_key`, or `random`. Timeline events should use `sort_key` (falls back to `md_path`/`path` when unreindexed). |

Object `query` (JSON body) supports field predicates on allowlisted keys (`[A-Za-z0-9_]{1,64}`):

| Shape | Meaning |
|-------|---------|
| `{ "content_kind": "article" }` | Exact string match |
| `{ "content_kind": ["article", "blog"] }` | `IN` (max 32 values) |
| `{ "content_kind": { "$in": ["article", "blog"] } }` | Same as array shorthand |
| `{ "content_kind": { "$ne": "exclude" } }` | Not equal (missing field matches) |
| `{ "type": { "$ne": ["manifest"] } }` | Not in list |

Predicates are pushed into SQL (`json_extract` + bound params) so `order=random&limit=N` returns N matching rows, not N random rows filtered afterward. Invalid / unsafe shapes yield an empty result set (never interpolated SQL).

Unknown keys are ignored. Invalid `order` → HTTP 400. `order` selects fixed SQL `ORDER BY` templates only (never interpolated).

```bash
# Curl-friendly: no body
curl -X QUERY 'https://paradigm-threat-files.dfw1.relaygateway.net/?q=venus&limit=10&order=-path' \
  -H 'Accept: application/json'

curl -X QUERY 'https://paradigm-threat-files.dfw1.relaygateway.net/?collection=index&order=random&limit=1' \
  -H 'Accept: application/json'

# Feeling Lucky-style: random articles only (JSON body)
curl -X QUERY 'https://paradigm-threat-files.dfw1.relaygateway.net/' \
  -H 'Content-Type: application/json' -H 'Accept: application/json' \
  --data '{"collection":"index","order":"random","limit":40,"query":{"content_kind":["article","investigation","blog"]}}'
```

Optional: `X-Relay-Branch` (default `main`).

## Site-shell SSR vs QUERY (do not mix)

Park site-shell SSR must **not** drive DirNav / ChangeLog / article lists via QUERY (or same-Host HTTP `fetch`). Reasons:

1. **Worker deadlock / stall** — CleverScript site-shell render that self-HTTP QUERY’s the same Host blocks the gateway worker (index mutex / request loop). `RepoFetchHost` offers in-process QUERY only as an escape hatch for rare SSR needs; park tip policy still forbids P2 catalog on SSR.
2. **HTML cache miss cost** — `ssr_html_cache` is tip-OID keyed. Catalog work inside the miss path pays nav-index parse / card fan-out / cold WASM on every push (the old ~10s path). Keep SSR to **P0 article + P1 chrome**; placeholders for catalog (`data-pt-*-ssr`).
3. **Optimal load** — hydrate when `isClientLive()`, then one on-tree GET of `nav-index.txt` warmed by runtime `tip_cached_get` + `tip_cached_nav_index_rows` (and tip thumbs module warm). QUERY stays for search, comments, and optional opt-in `collection: articles` **after** live.

Normative tip tiers + cache table: park checkout `hooks/CLEVERSCRIPT_TIP.md`. Gateway HTML cache: [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md) § SSR HTML cache.

## Roadmap: repo-opt-in SQLite + tip catalog cache (2026-08-02)

Park ChangeLog / DirNav used to feel slow on hydrate when they re-parsed `nav-index.txt` (and thumbs) on every live shell run. **Do not “fix” that by moving catalog into SSR** — that is the anti-pattern above. Vercel’s sister path QUERY’s once and caches client-side; park tip stays **on-tree first** for SSR hygiene. QUERY enhancements are for search, comments, and optional warm indexes **on hydrate**.

### Recommended implementation order

1. **Tip catalog warm (P2 hydrate)** — **Done (2026-08-02):** `@relay/catalog.loadNavIndex` reuses one parsed `nav-index.txt` Rc per tip blob text within a RunState session; tip `loadChangelogThumbs` module-warms `changelog-thumbs.json`. Still on-tree GET first (no SSR QUERY). **Host theme (same ship):** `syncHtmlThemeClass` prefers `<html data-pt-theme>` over WASM `get_current_theme_name` so catalog remounts keep park dark/light (runtime JS + client-web vendor).
2. **QUERY result cache (gateway)** — **Done (2026-08-02):** in-process LRU (`git/query_cache.rs`) keyed by `(repo, tipHead, collection, query, limit, order)`, TTL ~45s, max 64 entries; skipped for `order=random`. Tip HEAD is part of the key (new tip → miss).
3. **Repo-opt-in SCHEMA / custom tables** — Allow tip `search.yaml` (or `.relay/schema.yaml`) to declare extra SQLite tables (e.g. `articles`: `path`, `title`, `content_kind`, `_updated`, `image_src`, `summary`) filled at index warm from `path_tree` + sidecar fields. QUERY `collection: articles` stays thin via `@relay/search/sqlite`. Default repos keep today’s single `docs` table.
4. **Front-page directory + article list** — Prefer on-tree GET of `nav-index.txt` / thumbs (CDN-friendly). After (3), optional hydrate QUERY `collection: articles` with `order=-_updated&limit=120` when the repo opts in — still never during SSR (same-Host deadlock).
5. **Comment-scenario caching** — If/when comments are a QUERY collection (thread id + article path), cache list-by-path and count aggregations in the same QUERY LRU; invalidate on comment write / tip bump. Do not fold comment bodies into site-shell SSR HTML cache.

### Out of scope for tip-only thumbs

ChangeLog card images now use **`changelog-thumbs.json`** (stem/slug map, Vercel parity). They do **not** require a custom articles table or gateway binary deploy.

## Related

- [REPODB_PLAN.md](./REPODB_PLAN.md) — per-branch `.relay_data` layout
- [PARADIGM_PARK_LOCAL.md](./PARADIGM_PARK_LOCAL.md) — local QUERY/SSR hardening
- [CS_HOST_HELPERS.md](./CS_HOST_HELPERS.md) — CS routes; Rust `@relay/search/*`
- [RELAY_POLICY_AND_PERFORMANCE.md](./RELAY_POLICY_AND_PERFORMANCE.md) — SSR HTML cache vs hydrate catalog work
- Park tip: `hooks/CLEVERSCRIPT_TIP.md` — P0–P3; catalog SSR anti-pattern

*Last updated: 2026-08-02*

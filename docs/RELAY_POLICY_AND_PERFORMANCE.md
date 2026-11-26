# Relay policy layering and performance

Normative companion to **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)** and **[`RELAY_BRANCH_POLICY.md`](./RELAY_BRANCH_POLICY.md)**. Describes **who enforces what**, request-cost profiles, and **config/hook caching** in `relay-gateway`.

## Policy layering

Three tiers — do not collapse security into CleverScript hooks.

| Tier | Responsibility | Examples |
|------|----------------|----------|
| **Rust (`relay-gateway`)** | Security + infrastructure invariants | `git.branchRules`, signature verification, HTTP PUT requires valid `.relay.yaml` + `branchRules`, hook dispatch, desktop **`RELAY_DESKTOP_MODE`** / **`RELAY_PUT_REQUIRES_PRECOMMIT`** (PUT rejected without `pre-commit`), image/IPFS disk caches |
| **`.relay.yaml` (versioned in git)** | Wiring + git policy knobs | `client.hooks.get.path`, `server.hooks.*.path`, `branchRules`, `autoPush`, `relayTrust`; optional future `server.put.requirePreCommit` |
| **CleverScript hooks (versioned in git)** | **Repo content policy** only | Path allowlists, reel/subtitle sanity, `index.json` shape — **not** signatures or branch governance |

### Rules

1. **Hooks must not re-implement `branchRules`** or decide whether unsigned commits are allowed — Rust already does that on `git push`; PUT uses `allowUnsigned` via [`enforce_unsigned_http_allowed`](../src/git/branch_policy.rs).
2. **Hooks must not scan the whole repo** on write paths — only iterate `ctx.files` (changed paths in the candidate commit).
3. **Share validation** between `pre-commit` (HTTP PUT) and `pre-receive` (`git push`) by pointing both hooks at the same `.cs` file (e.g. `hooks/server/validation.cs`). CleverScript server hooks do not support repo-relative `import` between `.cs` modules; use one file or load shared text via `@relay/git` `readFile` if you must split files.
4. **PUT is opt-in writes:** when `RELAY_PUT_REQUIRES_PRECOMMIT=1` or `RELAY_DESKTOP_MODE=1`, missing `server.hooks.pre-commit` → PUT rejected. Fleet repos can adopt the same rule per-repo via future yaml knob.

## Request cost model

| Path | Runs CleverScript? | High QPS? |
|------|-------------------|-----------|
| GET on-tree blob (`index.json`, `.md`, assets) | **No** — Rust `git_resolve_and_respond` | **Yes** (CDN-friendly) |
| GET missing path → `server.hooks.get` | **Yes** | Use sparingly; rate-limit |
| GET raster `?w=` | Rust pipeline + optional GET hook; **image disk cache** | OK with `RELAY_IMAGE_CACHE_ROOT` |
| HTTP PUT | **Always** `pre-commit` + ref update + `post-receive` | **Cold path** — debounce editor saves |
| `git push` | Rust `branchRules` + optional `pre-receive` | Normal git rates |

**Editor clients:** never autosave via PUT at keyboard-event rates. Batch or debounce (e.g. 1–2 s idle).

## Config and hook caching (shipped)

`relay-gateway` keeps an **in-process LRU** (FIFO eviction, bounded size):

| Cache | Key | Max entries |
|-------|-----|-------------|
| Parsed `.relay.yaml` | `(repo_path, commit_OID)` | 512 |
| Hook `.cs` source (shebang stripped) | `(repo_path, commit_OID, hook_path)` | 256 |

Implementation: [`src/git/relay_config_cache.rs`](../src/git/relay_config_cache.rs). Used by:

- [`read_relay_config`](../src/git/repo.rs) / [`read_git_config_from_revision`](../src/git/repo.rs)
- [`execute_repo_hook`](../src/git/hooks.rs) (config + hook source at **`new_commit`**, not branch tip)

**Invalidation:** automatic — new commits get new OIDs; no cross-ref cache bleed.

**Disable:** `RELAY_DISABLE_CONFIG_CACHE=1` (debug/tests).

**Not cached (today):** CleverScript parse/AST, `run_query_script` execution, client-side `ModuleMapHost` (per browser/desktop session only).

**Future:** optional AST cache keyed by hook blob OID if profiling shows parse dominates hook latency.

## HTTP caching headers

Immutable assets at a known commit may use long-lived `Cache-Control` at the CDN. Dynamic GET hook responses should set explicit cache headers in the hook `headers` object when safe.

**Framework web bundles** (`embed.js`, split chunks, WASM) use commit-pinned URLs from the **`relay-web-assets`** repo (`/assets/c/{oid}/…`, immutable cache) when the site imports **`@relay/web-assets`** in `server.hooks.get` — see [`RELAY_WEB_ASSETS.md`](./RELAY_WEB_ASSETS.md).

### Pre-rendered SSR catalog cache (parked Hosts)

Parked sites with catalog **`ssrCache.enabled`** do **not** run live request-path SSR. HTML is composed from the mesh bare **`relay-ssr-cache`** (shell + `*.body.html`). Implementation: [`src/git/ssr_static_cache.rs`](../src/git/ssr_static_cache.rs). Schema: [`RELAY_CATALOG.md`](./RELAY_CATALOG.md).

| Plane | Responsibility |
|-------|----------------|
| **Publisher `pre-receive`** | CleverScript SSR + [`ssr_html_quality`](../src/git/ssr_html_quality.rs) on changed pages (`requirePreReceiveQa`, default on). Fail → **reject push**. Bypass: `RELAY_SSR_PRE_RECEIVE_QA=0` |
| **Publisher bake** (fat only) | After ACK: enqueue delta bake; `relay-ssr-bake` + QA before cache commit; stamp `_meta.json`; push cache bare |
| **Request path** | Compose shell+body; miss → **standard CSR shell** (embed + catalog client hook); **never** `ssrClientHookFromGetContext` |
| **Live LRU (legacy)** | Same quality gate before `ssr_html_cache::put` |
| **Client hooks** | Hydration, navigation, open/closed UI, interactive QUERY / SearchStore |
| **Nanodes** | `git fetch` cache + serve compose; **no** bake worker |

Bake tools: `relay-ssr-bake` (CleverScript), `scripts/relay-ssr-bake.py`, `scripts/relay-ssr-bake-queue.sh`, `scripts/init-relay-ssr-cache.sh`. Set `RELAY_NODE_ROLE=mesh` on Nanodes so the queue refuses. Marker examples: [`examples/ssr-qa.markers.json`](./examples/ssr-qa.markers.json).

**QUERY / SearchStore:** OK on client hydrate and gateway QUERY endpoints. Banned inside request-path HTML render for `ssrCache` Hosts. Optional static `/search/…` landings may be materialised offline on the publisher only.

### Client hook + standard shell (all hosting repos)

Catalog policy ([`RELAY_CATALOG.md`](./RELAY_CATALOG.md)): every hosting repo (`domains[]`) must have **`clientHook`** or **`defaultClientHook`** (or inherit fleet `catalog/index.json` `defaultClientHook`). HTML document requests without a baked `ssrCache` hit still receive the **standard embed shell** so the client hook can mount; interactive chrome updates from **local state after hydrate**, not by remounting or QUERY-in-SSR on the request path.

### Legacy live SSR LRU (repos without `ssrCache`)

Repos that have not opted into `ssrCache` may still call `ssrClientHookFromGetContext` on **fat / park-local** publishers. Gateway keeps an **in-process LRU** keyed by:

`(contentBarePath, branch, tipCommitOID, initialPath, clientHookPath, webAssetsOID)`

| Behavior | Detail |
|----------|--------|
| **Hit** | Return cached document; `Cache-Control: public, s-maxage=60, stale-while-revalidate=3600` |
| **Miss** | Render once, store, same headers |
| **Invalidation** | New tip commit or new `relay-web-assets` tip → new key |
| **Disable** | `RELAY_DISABLE_SSR_HTML_CACHE=1` |

**Related SSR env (gateway):** `RELAY_SSR_TIMEOUT_SECS` (default **30**; `0` disables wall timeout), `RELAY_SSR_MAX_CONCURRENT` (default 2). Concurrent live SSR + accept-queue wedge (~10 GiB RSS) is why parked production Hosts must migrate to `ssrCache`. HEAD, prefetch, and known crawler UAs skip live SSR. Coalesce + cooperative cancel live in `ssr_limits` / `relay_runtime::ssr_cancel`.

**Runtime performance strategies** (preferred vs alternate, `RELAY_PERF_*`): [`relay-runtime/docs/PERF_FEATURE_FLAGS.md`](../../relay-runtime/docs/PERF_FEATURE_FLAGS.md).

Tip hygiene for any remaining live SSR: **P0 article + P1 static chrome** only; **P2 catalog / P3 interactive** on hydrate (see park `hooks/CLEVERSCRIPT_TIP.md`).

**Catalog inside site-shell SSR is an anti-pattern.** Tip OID is in the key, so every push is a miss — a miss path that QUERY’s the catalog recreates multi-second cold renders. Optimal:

| Layer | Role for park |
|-------|----------------|
| **`ssr_html_cache`** | Cheap P0+P1 HTML only (article + static chrome + catalog *placeholders*) |
| Tip module QUERY warm | Hydrate-session catalog (`loadArticles.jsx`: DirNav / ChangeLog / Lucky) |
| Gateway **QUERY LRU** | Search + hydrate catalog QUERY after live — never site-shell SSR |
| CDN blob GET | Article md, images, sitemap — high QPS, no CleverScript |

Same-Host HTTP `fetch` / QUERY from the site-shell worker still deadlocks (or stalls on index lock). Use in-process tip GET for P0 markdown only. Normative tip table: park `hooks/CLEVERSCRIPT_TIP.md` § SSR priority tiers. QUERY roadmap: [`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md).

Implementation: [`src/git/ssr_html_cache.rs`](../src/git/ssr_html_cache.rs). Related: [`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`](./RELAY_EMBED_CLIENT_GET_INTERCEPT.md) Phase D.

### Tip sitemap / robots + soft-404

Parked hosts serve **dynamic** `GET /sitemap.xml` and `GET /robots.txt` from the publish tip (markdown paths + pretty URLs), not a stale tip blob. Missing non-asset HTML shells that are not SPA virtual routes (`/`, `/search`) return **HTTP 404** with `noindex` (meta + `X-Robots-Tag`).

### Optional RUM (`/api/rum`)

Off by default unless **`RELAY_RUM_ENABLED=1`** (install/update writes that if the key is missing). Optional **`RELAY_RUM_DIR`**, **`RELAY_RUM_SAMPLE_RATE`**, **`RELAY_RUM_RETENTION_DAYS`**. Embed beacons TTFC + CWV-ish metrics to same-origin `POST /api/rum` (SQLite under the rum dir). Typed theater **error** events (`kind: theater`, `severity`, `code`) go to `rum_theater_events`; `GET /api/rum` returns a 24h p75 summary plus **`theater24h`** counts (`major` / `minor` / `byCode` / `bySeverityCode`). relay-ops Fleet shows seed lag from `/api/status` IPFS fields and theater major/minor strips when RUM is enabled. **Not** product analytics: no click/scroll/heatmap tracking and no analytics SaaS for parked theater sites — client monitoring is errors (and optional coarse vitals) only; see fan-edits `docs/TELEMETRY.md`. Do not ship gateway journals into Fleet.

### Origin HTTP traffic (`/api/status` → `http`)

On by default (disable with **`RELAY_HTTP_STATS=0`**). Axum middleware records status class + latency; minute buckets expose `req_1m` / `req_1h` / `req_24h`, `html_1h` / `html_24h`, p50/p95, **1h per-Host mix** (`hosts[]`: html/api/5xx), and **top HTML paths** (query stripped, no client IPs). Persist under **`RELAY_HTTP_STATS_DIR`** (default `/opt/relay/state/http_stats`). Schema **≥ 5**. relay-ops node cards show QPS/latency plus html/api mix, top hosts, and paths from this blob.

### Public affirmation hits (`/api/hits`)

Same-origin **GET/POST** for park sites (registered on the Axum router, not tip files). Public totals are **affirmation-gated** and localStorage-unique on the client:

- `POST { kind: "site_view" }` — welcome checkbox (front page)
- `POST { kind: "movie_view", path: "/slug" }` — title affirmation (movie page)

Not playback, not VLC opens, not raw HTML GETs. GET returns fleet-summed `site_unique_*` / per-path `unique_*` (peers via `?local=1`, ~60s cache). Disable with **`RELAY_HTTP_STATS=0`**. Park theater privacy notes: fan-edits `docs/TELEMETRY.md`.

## fan-edits / desktop profile

- Catalog reads: on-tree GETs (`index.json`, `hooks/client/get.jsx`, project `.md`) — no `server.hooks.get`.
- Writes: `validation.cs` for both `pre-commit` and `pre-receive`.
- Local embedded server: single-user; cache mainly helps repeated PUT during an editing session and OPTIONS/GET config reads at stable HEAD.

## Related docs

- **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)** — hook contracts and CleverScript surface
- **[`RELAY_BRANCH_POLICY.md`](./RELAY_BRANCH_POLICY.md)** — `branchRules` semantics
- **[`RELAY_BRANCH_POLICY_OPERATOR.md`](./RELAY_BRANCH_POLICY_OPERATOR.md)** — fleet operator checklist
- **[`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md)** — QUERY LRU; catalog hydrate warm (not SSR)
- **[`RELAY_PUBLISH_ONCE.md`](./RELAY_PUBLISH_ONCE.md)** — on-commit index vs replica policy skip (plan)
- **[`PARADIGM_PARK_LOCAL.md`](./PARADIGM_PARK_LOCAL.md)** — local SSR smoke
- Park tip: `hooks/CLEVERSCRIPT_TIP.md` (P0–P3 + cache layers)

*Last updated: 2026-09-12*

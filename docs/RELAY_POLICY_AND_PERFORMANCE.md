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

### SSR HTML cache (Phase D)

Site-shell HTML from `ssrClientHookFromGetContext` is expensive (full client-hook render). Gateway keeps an **in-process LRU** keyed by:

`(contentBarePath, branch, tipCommitOID, initialPath, clientHookPath, webAssetsOID)`

| Behavior | Detail |
|----------|--------|
| **Hit** | Return cached document; `Cache-Control: public, s-maxage=60, stale-while-revalidate=3600` |
| **Miss** | Render once, store, same headers |
| **Invalidation** | New tip commit or new `relay-web-assets` tip → new key (no stale cross-tip bleed) |
| **Disable** | `RELAY_DISABLE_SSR_HTML_CACHE=1` |

Tip hygiene remains authoritative: **P0 article + P1 static chrome** on SSR; **P2 catalog / P3 interactive** on hydrate only (see park `hooks/CLEVERSCRIPT_TIP.md`). Cache does not excuse heavy catalog work inside the hook.

**Catalog SSR is an anti-pattern** even with this cache. Tip OID is in the key, so every push is a miss — a miss path that parses `nav-index.txt`, fans out ChangeLog cards, or same-Host QUERY recreates the old multi-second (~10s) cold render. Optimal:

| Layer | Role for park |
|-------|----------------|
| **`ssr_html_cache`** | Cheap P0+P1 HTML only (article + static chrome + catalog *placeholders*) |
| Runtime **`tip_cached_get` / `tip_cached_nav_index_rows`** | Hydrate-session warm of on-tree `nav-index.txt` (shared by DirNav / ChangeLog) |
| Gateway **QUERY LRU** | Search / optional articles QUERY after live — never site-shell SSR |
| CDN blob GET | `nav-index.txt`, thumbs, assets — high QPS, no CleverScript |

Same-Host HTTP `fetch` / QUERY from the site-shell worker still deadlocks (or stalls on index lock). Use in-process tip GET for P0 markdown only. Normative tip table: park `hooks/CLEVERSCRIPT_TIP.md` § SSR priority tiers. QUERY roadmap: [`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md).

Implementation: [`src/git/ssr_html_cache.rs`](../src/git/ssr_html_cache.rs). Related: [`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`](./RELAY_EMBED_CLIENT_GET_INTERCEPT.md) Phase D.

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

*Last updated: 2026-08-02*

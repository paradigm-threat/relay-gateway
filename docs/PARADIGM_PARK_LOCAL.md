# Paradigm park — local hardening & regression

**Audience:** operators hardening `relay.paradigmthreat.net` (paradigm-threat-files tip + gateway) **before** fleet push.

**Canon tip hygiene:** park checkout `hooks/CLEVERSCRIPT_TIP.md`  
**Host helpers (Rust):** [`CS_HOST_HELPERS.md`](./CS_HOST_HELPERS.md) · `relay-runtime/docs/CLEVERSCRIPT.md`

## Goal

Local-first loop:

1. Edit tip CleverScript / content in the files checkout.
2. Mirror worktree → local bare (`park-local sync`).
3. Serve gateway on `:9080` (default; avoids host Kubo `:8080`) with park `Host`.
4. Run SSR smoke + Rust unit regressions.
5. Only then push GitLab / Relay remotes (or deploy gateway binary).

Fleet deploy and `autossh` tunnels are **confirmation**, not the debugger.

## Architecture reminder

| Layer | Owns |
|-------|------|
| **Rust** (`relay-gateway` / `relay-runtime`) | Park Host routing, SSR document assembly + HTML cache, `@relay/md|ssr|image|ipfs|web-assets`, `@relay/search/*`, tip ergonomics `@relay/catalog|path|date|util|ui|tts` |
| **CleverScript tip** | **Routing and composition** — thin `hooks/server/get.cs`, client shell JSX. Prefer host helpers over reimplementing parse/sort/path/date in tip. |

## Commands

From **`relay-gateway/`**:

```bash
chmod +x scripts/park-local.sh   # once

./scripts/park-local.sh prepare  # bares + parked-domains.json
./scripts/park-local.sh sync     # worktree snapshot → bare main (dirty OK)
./scripts/park-local.sh serve    # sync + gateway on :9080

# other terminal
./scripts/park-local.sh smoke    # Host: relay.paradigmthreat.net SSR checks
./scripts/park-local.sh unit     # cargo: stdlib helpers + park Host/SSR tests
./scripts/park-local.sh regress  # unit only
```

**Chrome toggles (theme / font):** after hydrate in the browser, theme and font must update via `documentElement` class / `data-font` only — **no** full article remount. Multi-second clicks almost always mean tip `setState` (or `setCurrentTheme`). Tip canon: park checkout `hooks/CLEVERSCRIPT_TIP.md` § Render rarity; product doctrine: [`RELAY_CLEVERSCRIPT_REACT_RENDERING.md`](./RELAY_CLEVERSCRIPT_REACT_RENDERING.md).

From **`paradigm-threat-files/`** (gateway already up):

```bash
SSR_SMOKE_PORT=9080 ./scripts/ssr-smoke.sh
# or (same port via park-local smoke)
# or
SSR_SMOKE_PORT=9080 npm run park:smoke
```

Repo paths come from `scripts/relay-local-repos.map` (`paradigm-threat-files`, `relay-web-assets`).

### Curl sanity

```bash
curl -sS -X POST 'http://127.0.0.1:9080/api/config' | head
curl -sS -H 'Host: relay.paradigmthreat.net' -H 'Accept: text/html' \
  'http://127.0.0.1:9080/' | head
```

Without the park `Host`, you are **not** exercising the parked site.

**Port note:** host Kubo often binds **`:8080`**. Park local defaults to **`:9080`**. Override with `RELAY_HTTP_PORT` / `SSR_SMOKE_PORT` if needed.

## Data layout

Default root: `docker-data/park-local/` (gitignored under `docker-data/`).

| Path | Role |
|------|------|
| `data/paradigm-threat-files.git` | Tip mirror (force-updated from worktree) |
| `data/relay-web-assets.git` | Embed/WASM pin source |
| `state/parked-domains.json` | `relay.paradigmthreat.net` → `paradigm-threat-files` |

No fleet `RELAY_AUTHORIZED_REPOS_PATH` / catalog trust root — local mirrors are disposable.

## Regression matrix

| Check | Command | Covers |
|-------|---------|--------|
| `@relay/*` tip helpers | `park-local.sh unit` → `stdlib_relay_modules` | path/date/catalog/util |
| PT shell fixture render | `paradigm_files_render` | get.jsx / Layout fixture |
| CS language / JSON | `cleverscript_sugar`, `json_builtins` | G-001/002/008 |
| Park Host → repo | `test_parked_domain_routes_to_repo` | routing |
| Unknown Host ≠ fallback | `test_unknown_host_does_not_fall_back_when_parked` | G-005 neighbor |
| SSR fail → 500 | `test_parked_host_ssr_failure_returns_500_not_unknown_host` | G-005 |
| Live SSR markers | `park-local.sh smoke` / `ssr-smoke.sh` | article-first SSR, TOC, DirNav placeholder |
| QUERY (optional) | `scripts/local-query-e2e.sh` | sqlite search |
| Thin IPFS (optional) | `scripts/docker-ipfs-dev.sh` | media GET / `?w=` |

## Troubleshooting

| Symptom | Likely cause |
|---------|----------------|
| `gateway not up` | Run `serve` first; confirm `POST /api/config` on the park port |
| Port already in use | Host Kubo often owns `:8080` — park default is `:9080` |
| Smoke `curl: (52) Empty reply` | SSR / GET-hook path aborted the connection (often an uncaught WASM/runtime panic). Gateway now catches panics and returns **500 Site shell / SSR failed** when possible — check logs for `relay_ssr` / `client hook SSR panicked`. Raw blob GET (e.g. `/README.md` without `Accept: text/html`) should still work. Fix tip or runtime, then re-`sync` + smoke |
| Homepage UTF-8 hazard | Keep root `page.md` ASCII-only (no em dashes / curly quotes). Host helpers must not mid-slice UTF-8 (`@relay/path.rewriteMarkdownAssetUrls` is char-safe) |
| `Unknown Host` | Park map missing — re-run `prepare`; confirm `state/parked-domains.json` |
| Smoke markers missing but 200 | Tip SSR regressed P0/P1 — see park `hooks/CLEVERSCRIPT_TIP.md` |

## Related

- [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md) — P0/P1 SSR vs P2/P3 hydrate; SSR HTML cache
- [`SERVER_HOOKS.md`](./SERVER_HOOKS.md) — heavy work in Rust; CS thin
- [`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md) — QUERY routes only
- [`CLEVERSCRIPT_GAP_LOG.md`](./CLEVERSCRIPT_GAP_LOG.md) — language / park gaps
- [`DOCKER_IPFS_DEV.md`](./DOCKER_IPFS_DEV.md) — thin media canary (separate from park SSR)
- [`CS_HOST_HELPERS.md`](./CS_HOST_HELPERS.md) — next Rust helper candidates

*Last updated: 2026-08-02*

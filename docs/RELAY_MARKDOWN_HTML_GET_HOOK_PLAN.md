# Markdown → HTML via GET hook (Accept negotiation) — **tabled**

**Status:** Implementation **paused** (2026). This document preserves the intended design so work can resume without re-discovery.

**Goal (when resumed):** When a `.md` blob is served from git and the request **`Accept`** prefers **`text/html`**, run a CleverScript GET hook path that converts markdown to HTML (via **`md2ast`** or an embedded markdown pipeline in `relay-gateway`), returning `Content-Type: text/html` and `Vary: Accept`.

**Non-goals:** This path is for **static HTML** (crawlers, `curl`, lightweight readers). It does **not** replace client-side rendering for pages that need **interactive or tagged React components**—those still require **markdown source** or structured AST + runtime.

**Related (active requirements):** An **HTML document** that bootstraps a **minimal** relay-client-web bundle (no default chrome), **preset** gateway, and **full-screen** `hooks/client/get.jsx` is specified in [**`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`**](RELAY_EMBED_CLIENT_GET_INTERCEPT.md). Gateway work (extend `resolve.rs` like raster `?w=`) overlaps; embed may ship **SPA-in-HTML** rather than CleverScript-only static HTML.

## Intended integration points (relay-gateway)

1. **`src/git/resolve.rs`** — Mirror the existing **raster `?w=`** hook injection: when the resolved blob is markdown (`text/markdown` or `.md`) **and** `Accept` negotiates HTML **and** `.relay.yaml` defines `server.hooks.get`, build hook context (`relPath`, `contentType`, `bodyBase64` / body text, `commitSha`, `blobOid`, `query`, **`requestHeaders`** via `augment_get_context_headers`) and run the CleverScript hook; if the JSON maps to a response (`get_hook_json_to_response`), return it; else fall through to raw markdown.

2. **`src/git/clever_script_host.rs`** — Add a virtual module (e.g. **`@relay/md`**) with a native such as **`markdownHtmlFromGetContext(ctx)`** delegating to **`md2ast`** (or `pulldown-cmark` / internal HTML writer).

3. **Repo hook** — e.g. `hooks/server/get.cs`: chain **`markdownHtmlFromGetContext`** → **`optimizeRasterFromGetContext`** → **`next()`** (same tail-return / ternary style as current image optimization).

4. **Caching (optional)** — Disk cache keyed by repo, branch, commit, blob OID, and converter version (similar pattern to image WebP cache).

## Operational follow-up (when resumed)

- Extend **`relay-fleet.sh` / install** as today for binary rollout.
- **paradigm-threat-files:** update `hooks/server/get.cs` after gateway ships support.
- Verification: `curl -H 'Accept: text/html' https://<host>/…/file.md` returns HTML; without it, raw body.

See also: [`SERVER_HOOKS.md`](SERVER_HOOKS.md), [`docs/RELAY_UNIVERSAL_CLIENT_AND_PARADIGM_SITE.md`](RELAY_UNIVERSAL_CLIENT_AND_PARADIGM_SITE.md) for the broader client/repo-component direction.

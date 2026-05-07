# Relay embed client + server GET intercept (requirements & plan)

**Status:** requirements **accepted**; implementation **not shipped** (plan only).

## Product requirement

For **direct browser navigation** to content on a Relay mirror (e.g. a markdown path in `paradigm-threat-files`), the experience should optionally be:

1. **Server-side interception** — The same **`server.hooks.get`** CleverScript (`.cs`) path used for the **image optimizer** may **intercept** the HTTP GET **before** returning the raw git blob.
2. **HTML instead of raw markdown** — When the interceptor handles the request, the response is **`Content-Type: text/html`** (not `text/markdown`). The document is a **minimal host page**: **no** full relay-client-web chrome (no Home / Settings / Test shell, no peer list, no tab bar).
3. **Pre-set target** — The Relay **gateway origin**, **repo**, and **branch** (and optionally initial path) are **fixed in the HTML** (or a single inline bootstrap object) so the user does not pick peers.
4. **Full-screen hook** — The **`<body>`** contains a single **full-viewport root**; the repo’s **client GET hook** (`hooks/client/get.jsx` per [OPTIONS default](./SERVER_HOOKS.md)) mounts **edge-to-edge** and owns **all** navigation and layout for that page.

This is **read/browse oriented**: crawlers and users get a **one-page** app feel; interactive multi-tab tooling remains the **default** relay-client-web app.

## Parallel: image optimizer (existing)

Today **`relay-gateway`** only runs the CleverScript GET hook for a **narrow** case: **raster** blobs when query **`?w=`** / **`?width=`** is set and **`server.hooks.get`** is configured. The hook receives **`optimizeRasterFromGetContext`‑style** context (`relPath`, `contentType`, `bodyBase64`, `commitSha`, branch, repo, etc.); the return value maps to HTTP via **`get_hook_json_to_response`** (see `src/git/get_hook_http.rs`).

**Reference:** `git_resolve_and_respond` in **`src/git/resolve.rs`** — raster branch with **`run_clever_hook`** → optional **`text/html` or image/*` response**; otherwise fall through to **raw bytes**.

The **embed** feature **reuses the same hook dispatch and JSON→HTTP contract**; it adds **new eligibility rules** (e.g. markdown + `Accept: text/html`, and/or explicit `?embed=1`) and a **different** CleverScript return: an HTML document that bootstraps the **embed** bundle.

## HTTP contract (already supported by gateway)

GET-hook returns are documented on **`get_hook_json_to_response`**: **`kind: "file"`**, **`contentType`**, **`bodyUtf8`** or **`bodyBase64`**, optional **`status`** and **`headers`**. Returning **`text/html`** is therefore **already valid** once the hook runs for the request.

**Gap:** `resolve.rs` does not yet invoke the GET hook for **markdown + HTML negotiation** (that design lives in [**`RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md`**](RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md)); the **embed** product needs that (or an equivalent branch) **plus** a **standard HTML template** that loads the embed client.

## Implementation plan (phased)

### Phase A — `relay-client-web` **embed** entry (minimal shell)

- Add a **second** HTML entry (e.g. **`embed.html`**) and **`src/embed-main.tsx`** (names TBD).
- Bootstrap **only** what is needed to run the **client GET hook** at full viewport: **`RepoFetchProvider`**, **`ThemedHookRenderer`** (or future WASM hook mount), **`relay-runtime` WASM**, CSS reset (`html, body, #root { height: 100%; margin: 0 }`).
- **Configuration** via build-time **`import.meta.env`** (e.g. **`RELAY_PUBLIC_EMBED_DEFAULT_ORIGIN`**) and/or **query/hash** parsed once at load — **no** peer discovery UI.
- **esbuild** (or Vite) **split**: **`embed.js`** must not pull **PeersView**, **TabBar**, **SettingsTab** (tree-shake / separate entry).
- **Deliverable:** static **`embed.html` + `embed-*.js` + CSS** suitable for placement under **`--static`** or next to the main SPA.

### Phase B — **Gateway** `resolve.rs` (or HTTP handler) conditions

- Mirror the **raster `?w=`** pattern: after resolving a **blob**, if **eligibility** holds (e.g. **`.md`** and **`Accept`** prefers **`text/html`**, or **`?embed=1`**), load **`hooks/server/get.cs`** from the commit, build **GET context** (same augmentation as raster: path, headers, body, commit, blob OID), **`run_clever_hook`**, **`get_hook_json_to_response`**.
- If hook returns **`None`**, **fall through** to **raw markdown** (current behavior).
- Set **`Vary: Accept`** when negotiation is used.

**Align** with [**`RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md`**](RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md) § integration points (paused tabled work); **embed** may **ship HTML** that loads the **JS client** rather than only CleverScript-generated static HTML.

### Phase C — **Repo** `hooks/server/get.cs`

- Chain: **if** embed / HTML response wanted → build document (or call a small native helper) that references **`/static/.../embed.js`** and passes **`window.__RELAY_EMBED_CONFIG__`** with **origin, repo, branch, initialPath**.
- **Else if** raster **`?w=`** → existing **`optimizeRasterFromGetContext`** path.
- **Else** → **`next()`** / default file body (conceptually; exact API per existing hook style).

### Phase D — Security, perf, ops

- **XSS:** HTML is generated server-side from repo hook — treat as **trusted** only for that repo; sanitize or restrict inline scripts if hooks are **tenant-supplied**.
- **Caching:** Key by repo, branch, commit, blob OID, **`Accept`**, embed flag, and **asset version** of embed bundle.
- **CORS / cookies:** Embed page origin may equal the gateway; document **same-origin** expectations for WASM and fetch.

### Verification

- **`curl -H 'Accept: text/html' https://host/repo-branch/path/file.md`** returns **HTML** with embed bootstrap when hook intercepts; without **`Accept: text/html`**, **raw `.md`**.
- **Browser:** one full-viewport hook, **no** default client chrome.

## See also

- [**`RELAY_CLIENT_WEB_SHELL.md`**](RELAY_CLIENT_WEB_SHELL.md) — full SPA shell (`relay-client-web`); embed is a **subset** entry.
- [**`RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md`**](RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md) — Accept negotiation + GET hook (tabled).
- [**`SERVER_HOOKS.md`**](SERVER_HOOKS.md) — `server.hooks.get`, client defaults.

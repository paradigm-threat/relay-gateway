# Relay embed client + server GET intercept (requirements & plan)

**Status:** **shipped** (gateway + client bundle). The relay template includes explicit
`@relay/ssr` routing; individual repositories may remove or narrow that route. Production still
needs `relay-client-web` **`dist/assets/embed.{js,css}`** deployed next to the main static bundle.

**SSR policy:** server rendering is explicitly enabled by repository CleverScript, not by the
gateway route. A `server.hooks.get` imports `ssrClientHookFromGetContext` from `@relay/ssr` and
calls it for selected requests (`ctx.embedMarkdownRequest` for markdown + Accept HTML / `?embed=1`,
or `ctx.siteShellRequest` for repo-host `/`). Repositories that omit the import/call retain the
empty client-rendered embed shell (when `?embed=1`) or static `@relay/md` HTML (Accept-only).

**App-shell model:** the client GET hook owns which markdown (if any) to load per route. The URL
path is an entry route, not necessarily the blob rendered as the page. If SSR succeeds but client
hydration fails, embed.js keeps the server DOM and prepends a light `.relay-hook-error` panel.

**Web/CDN assets:** framework bundles (`embed.js`, chunks, WASM) live in a dedicated signed
git repo with **commit-pinned** URLs (`/assets/c/{oid}/…`) so one page load never mixes two tips.
Sites opt in by importing **`@relay/web-assets`** in `server.hooks.get` — see [`RELAY_WEB_ASSETS.md`](RELAY_WEB_ASSETS.md).
Disk `/opt/relay/www` remains the local/dev fallback.

## Dependency policy (parked sites / embed)

Parked content sites (e.g. **clevertree.net**) and the **`/assets/embed.js`** bootloader must stay
**free of npm React / ReactDOM** (and other UI-framework deps). Site UI is CleverScript +
relay-runtime WASM only. Enforce with **`scripts/assert-embed-no-react.mjs`** on embed builds.

- **esbuild** is allowed as a packager for the slim WASM hydrate glue.
- **npm** is limited to build/runtime necessities for that glue (TypeScript, esbuild, wasm-bindgen
  output) — not application UI libraries.
- CleverScript JSX is **React-like syntax**, not the React npm package
  ([`RELAY_CLEVERSCRIPT_REACT_RENDERING.md`](RELAY_CLEVERSCRIPT_REACT_RENDERING.md)).

### Deferred: React as a native component

If a surface truly needs the React npm runtime (legacy widgets, third-party React trees), it must
be introduced the same way as other **host-owned native components** (Android / desktop pattern:
CleverScript configures a host widget; the host owns the heavy tree — see
[`relay-runtime/docs/CROSS_PLATFORM_OS_SUPPORT_PLAN.md`](../../relay-runtime/docs/CROSS_PLATFORM_OS_SUPPORT_PLAN.md)
and video/native-component notes). That work is **not** in scope for embed or parked sites; do
**not** pull React into `embed.js` as an interim CSR shell.

The optional **`main.js`** React tabbed shell is **not** part of the CDN (`relay-web-assets`)
deployment. That profile is embed + WASM only; a separate release may include React later
([`RELAY_WEB_ASSETS.md`](RELAY_WEB_ASSETS.md)).

## Host boundary (security + cross-platform UI)

Relay UI (CleverScript hooks) is **platform-agnostic**. The same hook tree runs under WASM (web),
Android JNI, and desktop hosts. **Web glue** (`relay-runtime-web.js` / `embed.js`) exists only to
map VNodes, handlers, fetch, and theme onto the browser — not to make hooks “web apps.”

### CleverScript must not touch the DOM (or other platform surfaces) directly

Hooks must **not** use `document`, `window`, `localStorage` (except via host APIs the runtime
exposes), or other browser globals to mutate UI. Effects that need the host (theme class, print,
navigation) go through:

- **declared props** (`onClick`, `dataTheme` → host attributes), and/or
- **host natives** (`@clevertree/style-internal` `setCurrentTheme`, `isClientLive`, …), and/or
- **host event delegation** in the web glue (`data-theme`, `data-relay-action`).

Reaching into `document` from a hook breaks Android/desktop and is a security footgun (XSS-shaped
capabilities inside untrusted-ish repo content).

### Web page scripts must not read WASM/runtime internals

Browser JavaScript outside the controlled glue must **not** inspect CleverScript run-state,
handler registries, session theme cells, or other WASM thread-local / module-private state.
The public contract is the **documented wasm-bindgen / glue API** (`renderHook`, `invoke_handler`,
`setCurrentTheme`, …) and DOM attributes the glue intentionally mirrors (e.g. `html.light` /
`html.dark` after a theme change). Do not export raw session stores or handler maps to `window`.

### Platform awareness is opt-in

By default, hooks should not branch on “am I on web?”. Prefer host features that work everywhere
(`fetch`, theme, navigation). Explicit platform probes (if any) are for authors who **choose** to
specialize — not required for parked sites like clevertree.net.

**Implementation (relay monorepo):**

- Gateway: `relay-gateway/src/git/embed_html.rs` (fallback HTML + `Vary` / cache / `X-Relay-*` parity), `relay-gateway/src/git/resolve.rs` (markdown + `Accept` / `?embed=` + `server.hooks.get`), `relay-gateway/src/handlers/general.rs` (`/` site shell), `relay-gateway/src/handlers/head.rs` (query map parity with GET).
- Client embed: `relay-client-web/src/embed-main.ts` → esbuild entry **`embed`** → `dist/assets/embed.js` + `embed.css`. Boot is **WASM-only** (`bootEmbedRoot` / `renderHook` / `hydrateVnode`): same `hooks/client/get.jsx` as SSR; **no React** on this graph (`scripts/assert-embed-no-react.mjs`).
- Template hook comments: `relay-template/hooks/server/get.cs` (`embedMarkdownRequest` / `siteShellRequest`).

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

**Shipped behavior:** `git_resolve_and_respond` invokes **`server.hooks.get`** for markdown blobs when **`Accept` prefers `text/html` or `?embed=1`**, with **`embedMarkdownRequest: true`**. Repo-host **`/`** uses **`try_site_shell_for_root`** + **`siteShellRequest: true`**. **Missing non-asset paths** with a `server.hooks.get` and HTML Accept / `?embed=1` also get **`siteShellRequest: true`** (SPA routes like `/search`, remote article paths) — HTML only when the hook opts in; asset-like thin misses stay on the IPFS/get-hook path. The hook may return `ssrClientHookFromGetContext(ctx)` for SSR. Non-HTML + **`?embed=1`** → CSR embed shell; Accept-only non-HTML → static `@relay/md` when the hook handles `markdownHtmlRequest`.

## Implementation plan (phased)

### Phase A — `relay-client-web` **embed** entry (minimal shell)

**Shipped (WASM-only, no npm React):** `src/embed-main.ts` → `/assets/embed.js` boots
`bootEmbedRoot` (hydrate SSR or CSR-mount the same client hook). Gate: `assert-embed-no-react.mjs`.

Historical notes (superseded for embed): React `EmbedApp` / `ThemedHookRenderer` were an interim
CSR shell and must not return to the embed graph. Any future React integration is **native-component
only** (see Dependency policy above) — not a shared chunk with parked sites.

Optional **`main.js`** admin chrome may keep React separately until retired.

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

# relay-client-web shell (Phase B)

The **relay-client-web** app (`npm run dev` / `npm run build`) is the evolving **universal Relay** UI.

## WASM build

1. `cd relay-client-web && npm run build:wasm` — runs `relay-gateway/relay-runtime/scripts/build-wasm-web.sh` (requires **wasm-pack**).
2. `npm run build` — copies `src/wasm/relay_runtime_pkg/` into `dist/wasm/relay_runtime_pkg/` for `markdown_to_ast_json` + CleverScript hooks.

## Content fetching

- **`webPlugin.fetchContent`** ([`src/plugins/web/index.ts`](../../relay-client-web/src/plugins/web/index.ts)) performs `fetch(path)` with optional **`X-Relay-Branch`** / **`X-Relay-Repo`** headers.
- Set **`RELAY_PUBLIC_FILES_BASE`** in **`.env`** (see **`.env.example`**) so the plugin’s `baseUrl` points at a live **files** mirror (e.g. `https://files.example.com`).

## Markdown rendering

- **`parseMarkdownToAstJson`** — `relay-runtime` WASM + **md2jsx**.
- **`MarkdownAstPreview`** — React view over the AST JSON.
- **Math / KaTeX** — design in [`RELAY_MARKDOWN_MATH_KATEX.md`](RELAY_MARKDOWN_MATH_KATEX.md); gate for full Phase B parity.

## Timelines + paradigm (Phase C+)

Same client should add **timeline** repo fetch (paired origin) per program decisions; not all wiring exists in this PR.

## Embed / full-page hook (planned)

**Requirement:** For **direct links**, the **server GET hook** may return **`text/html`** that loads a **stripped** `relay-client-web` build (*no* Home / Settings / peer UI): **preset** origin/repo/branch, **full-viewport** body mounting only **`hooks/client/get.jsx`**.

**Plan:** [**`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`**](RELAY_EMBED_CLIENT_GET_INTERCEPT.md) — mirrors the **image optimizer** interception model in **`resolve.rs`** (`?w=` + **`run_clever_hook`**), adds **`relay-client-web` embed entry**, and repo **`hooks/server/get.cs`** chaining.

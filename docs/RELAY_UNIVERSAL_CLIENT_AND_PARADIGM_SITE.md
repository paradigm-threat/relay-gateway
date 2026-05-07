# Universal Relay client & Paradigm Threat site (beyond Next.js)

**Status:** **Architecture / phased program** — not committed to a single release train.

**Intent:** Move toward a **Relay-native web client**: load **any remote repo** from the gateway, render **markdown → UI** with **CleverScript / relay-runtime**, and **reduce or eliminate** dependence on **Next.js** for **paradigm-threat-site**. **paradigm-threat-files** would supply **site-only** CleverScript components (changelog, chat shell, TOC, directory listings, etc.) alongside shared **core** components shipped with the universal client.

**Related:** The **GET-hook Markdown→HTML** work is **tabled**; see [`RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md`](RELAY_MARKDOWN_HTML_GET_HOOK_PLAN.md). **md2jsx placement** (crate + WASM API): [`RELAY_MD2JSX_PLACEMENT.md`](RELAY_MD2JSX_PLACEMENT.md).

---

## relay-runtime scan (this monorepo)

**Location:** [`relay-runtime/`](../relay-runtime/) (crate `relay-runtime`).

**What exists today**

- **CleverScript:** lexer + parser for a **JS-like + JSX** subset; analysis; [`Runtime`](relay-runtime/src/runtime.rs); [`Host` trait](relay-runtime/src/host.rs) for `createElement`, `fetch`, `navigate`, `load_module`.
- **Rendering:** VDOM + [`reconcile`](relay-runtime/src/vdom/reconcile.rs); style resolution via **themed-styler** (built-in dependency).
- **Server hooks (one-shot):** [`run_query_script`](relay-runtime/src/server_script.rs) for GET/QUERY CleverScript without full client run-state.
- **WASM:** [`wasm_api.rs`](relay-runtime/src/wasm_api.rs) — web builds, `bootstrap`-style integration; **browser JS as thin glue** per [`relay-runtime/README.md`](../relay-runtime/README.md).
- **Platforms:** `wasm32` + Android JNI hooks.

**What does *not* exist today**

- **No** markdown / **md2jsx** / CommonMark pipeline inside **relay-runtime** (confirmed: no `markdown`, `md2jsx`, `pulldown` in `relay-runtime/src`).
- Runtime is focused on **executing hooks** expressed in CleverScript + **VDOM**, not on authoring markdown parsers.

**Implication:** “Embed markdown → JSX” can mean:

1. **Rust crate** [md2jsx](https://github.com/clevertree/md2jsx) (or equivalent) as a **dependency** of **relay-runtime** *or* **relay-gateway** — new public API (e.g. parse MD → `Value`/VNode or JSX string) + tests; **or**
2. A **small WASM-friendly** markdown AST → **existing** `value_to_vnode` path — design work required.

**JavaScript package:** `relay-runtime` already ships **WASM + JS glue** (`relay-runtime-web.js` in README examples). A “full site” would extend that package with **routing**, **module loading from repo**, and **markdown pipeline** wiring.

---

## Two classes of components

| Class | Lives in | Purpose |
|--------|-----------|---------|
| **Core / universal** | **relay-runtime** (or companion **relay-client-web** package) | Primitives needed for **all** Relay sites: layout, nav shell, image handling, theme, fetch/git helpers, shared UI atoms, markdown host surface. |
| **Repo-local** | **paradigm-threat-files** (or any site repo) | **Changelog**, **chat room**, **TOC**, **directory / auto-index**, branding-only widgets—loaded via **`load_module` / repo-relative paths** after fetch. |

**Rule of thumb:** If only one product needs it, keep it **in the repo**; if every Relay customer needs it, move toward **core**.

---

## CleverScript vs current React (Paradigm) components

**paradigm-threat-site** today uses **React + next-mdx-remote**, rich **remark/rehype** plugins, and many patterns **CleverScript may not support** yet (language subset, ecosystem, devtools).

**Migration cost:** **High.** Expect to:

- **Re-implement** each surface in CleverScript + Host APIs, or **simplify** UX to match the language;
- **Extend CleverScript / Host** where the gap is real (file bugs, fix in **relay-runtime**);
- **Automate or manual-test** each component (matrix: web WASM first, then others if applicable).

This is explicitly **multi-phase**; do not assume a single cutover.

---

## Phased roadmap (suggested)

**Phase A — Foundation**

- Stabilize **relay-runtime WASM** bundle as the **only** hook/renderer for a **reference template** site (existing relay-template direction).
- Define **markdown → intermediate representation** contract (VNODES vs JSX string) and where **md2jsx** lives (**runtime** vs **gateway**).

**Phase B — Core “browser shell”**

- Repo picker / URL routing / fetch markdown from **relay** mirrors; no Next `app` router.
- Core components: chrome, typography, link handling, images (relay `?w=` where applicable).

**Phase C — paradigm-threat-files components**

- Port **changelog, chat, TOC, directory** as **repo-local** `.cs`/JSX hooks with clear **Host** APIs (fetch, navigate, auth if any).

**Phase D — Feature parity & decommission**

- **TTS / chat APIs** may remain **HTTP JSON** endpoints (separate from “page shell”) until/unless absorbed by Relay services.
- Decommission **Next.js** only when **routing, SEO, and author workflows** have acceptable replacements.

---

## paradigm-threat-files layout (conceptual)

- **`hooks/client/get.jsx`** (default GET hook path advertised in **`OPTIONS /`**; see [`SERVER_HOOKS.md`](SERVER_HOOKS.md)) — JSX layout + **`MarkdownRenderer`** from **`@clevertree/markdown`** for repo content. Repo-local **CleverScript** / JSX components can live under **`hooks/client/**`** and load via **Host** `load_module`.
- **Markdown content** unchanged as source of truth; **components** referenced from markdown according to whatever **md2jsx** + Host rules you adopt.

---

## Risks

- **SSO / cookies:** Browser auth models that assume **same-origin Next API** must be **redesigned** for static shell + relay origins (out of scope of this doc detail).
- **SSR / SEO:** Pure client render may regress crawlers unless **relay HTML** or **prerender** path exists.
- **Engineering bandwidth:** CleverScript extension + component porting is **parallel** to product work—**phasing** is mandatory.

---

## See also

- [`relay-runtime/README.md`](../relay-runtime/README.md) (links to CLEVERSCRIPT / WASM docs when present in checkout)
- [`CLIENT_HOOK_LANGUAGE.md`](CLIENT_HOOK_LANGUAGE.md)
- [`SERVER_HOOKS.md`](SERVER_HOOKS.md)

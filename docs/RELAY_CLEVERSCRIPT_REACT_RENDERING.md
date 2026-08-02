# CleverScript “React-like” rendering — design goals and handler registry

This document states **product and engineering requirements** for the CleverScript JSX / React-like layer in **relay-runtime** (WASM, Android JNI, and the web DOM adapter). It is **not** a goal to match React’s performance model or reconciliation sophistication.

## Goals (intentional differences from React)

1. **React-like behavior, not React’s performance agenda**  
   Hooks should feel familiar (`useState`, components, props, events), but we **do not** target Fiber-level incremental rendering, concurrent features, or fine-grained subscription models.

2. **Re-renders should be rare**  
   Design hooks and hosts so that **full hook re-runs and VDOM walks happen as infrequently as possible**. Prefer:
   - coalescing updates,
   - pushing high-frequency or heavy visual work to **native / host-controlled UI**,
   - keeping CleverScript trees small and shallow,
   - **CSS / host attribute flips** for appearance chrome (theme, font, tabs) so nothing CleverScript-owned needs to update.

   CleverScript is **interpreted** (WASM), not machine-code React. Treat each `setState` as expensive by default.

   **Web host reality today:** after hydrate, `doRender` re-runs the full site-shell hook and **replaces** `container.innerHTML` (incremental `vdom::reconcile` is not wired on web). So a tip `useState` whose only job is “show the other theme icon” remounts the **entire article**. That is an anti-pattern.

   **Canonical tip pattern (theme / font, web):** flip `document.documentElement` class / `data-font` / `data-pt-theme` against a dual stylesheet (`css_for_web_system` → `html.dark` / `html.light`). Keep **both** icons (or font face via CSS) in the DOM and show/hide with selectors. Do **not** call `setCurrentTheme` for park chrome (full CSS regen + `set_render_requested`). Do **not** `setState` solely to reflect light/dark. Sibling pattern: ChangeLog **CSS radio tabs** (no tab `setState`).

   **Android (prep):** themed-styler still owns light/dark tokens, but there is no CSS cascade. Use JNI `nativeSetThemeAndRender` (re-resolve stored VNode, no hook re-run). Do not port `html.dark` tip chrome to Android. See `relay-runtime/docs/STYLE_RESOLUTION.md`.

3. **Complex, state-heavy, or high-churn UI belongs in native components**  
   When a surface needs animation, lists with constant updates, media, maps, etc., the **host** should own the widget; CleverScript supplies configuration and high-level commands, not per-frame reconciliation.

4. **npm React is not the CleverScript renderer**  
   CleverScript JSX is executed by **relay-runtime** (WASM / JNI / desktop). Parked sites and
   **`embed.js`** must not depend on the React npm package
   ([`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`](RELAY_EMBED_CLIENT_GET_INTERCEPT.md) § Dependency policy).
   If npm React is ever required for a host feature, treat it as a **native component** (same
   pattern as Android/desktop host widgets) — planned later; not an embed dependency.

5. **Deliberately simple renderer**  
   The CleverScript → VNode → DOM/native path should stay **easy to reason about**: one hook run produces one tree; event handlers are registered in a predictable way. We avoid “JavaScript framework style” mitigations (weak maps, identity caches, manual GC tuning) as a **default** answer.

6. **Platform-agnostic UI; host owns the surface**  
   CleverScript must **not** call into `document` / `window` (or Android/desktop view APIs) directly.
   Web glue adapts VNodes and host natives to the browser; the same hooks should run without knowing
   they are on web unless the author explicitly opts into a platform probe. See
   [`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`](RELAY_EMBED_CLIENT_GET_INTERCEPT.md) § Host boundary.

7. **No leaking WASM/runtime state to page scripts**  
   Session theme, handler registries, and other runtime cells stay inside relay-runtime (WASM/native).
   Page JS only uses the published glue API — not ad-hoc `window.__relay_*` dumps of internal state.

## Handler registry and re-renders

### What happens today

When a hook run completes, the runtime walks the returned tree and **registers** each function-valued prop (`onClick`, `anchorOnClick`, etc.) in a thread-local **`HANDLER_REGISTRY`**, replacing them with numeric **handler IDs** in the serialized VNode. The host invokes handlers by ID after user input.

On **persistent** hook runs (same source, `run_hook_persistent`), **`run()`** can execute many times (e.g. after `setState`), but **`HANDLER_REGISTRY` is only cleared when hook source changes or RunState is reset**. Each **`value_to_vnode_with_handlers`** pass assigns **new** IDs for the **current** tree. Old IDs are **not** referenced by the newly built DOM, but they can **accumulate** in the registry until source changes — unbounded growth in long sessions with many re-renders.

### Recommended mitigation (primary)

**Reset the handler registry immediately before each `value_to_vnode_with_handlers` pass** that follows a successful hook run (same session, same source).

- After `run_hook_persistent` / equivalent returns the root `Value`, and **before** walking the tree to assign handler IDs, call a small API, e.g. `run_state::clear_handler_registry_for_vnode_pass()` that:
  - clears `HANDLER_REGISTRY`,
  - resets `NEXT_HANDLER_ID` to `1` (or the same sentinel used on fresh RunState).

**Why this is acceptable**

- The previous render’s DOM (or native view) is **replaced**; listeners must only reference IDs from the **latest** walk.
- No reliance on JS GC behavior: bounded registry size proportional to **current** tree only.
- Aligns with the goal that **re-renders are infrequent** — cost is “number of handlers in the current tree,” not “history of all past trees.”

**Call sites to keep in sync**

- WASM: `run_hook_with_host` (and any other export that runs `value_to_vnode_with_handlers` after a persistent run).
- Android JNI: path that builds the vnode after `run_hook_with_host`.
- Tests: any direct `value_to_vnode_with_handlers` after multiple `run()` cycles without clearing should either clear explicitly or assert bounded registry size.

### Secondary mitigations (design, not micro-optimization)

- **Fewer handlers in the tree:** prefer native surfaces for noisy interaction; avoid huge markdown or lists that register thousands of `onClick` handlers if a host-native list is available.
- **Per-link markdown:** still O(n) in link count for one pass; acceptable when n is moderate and registry is cleared each pass (see above).

### Non-goals (unless requirements change)

- Stable handler IDs across renders for the “same” logical button (identity-based reuse).
- Weak references or periodic compaction of the registry as a **substitute** for per-pass reset.

## Platform default actions (`<a>`, forms, …)

**Default:** Elements use **normal native / browser default behavior** (e.g. follow `href`, submit forms). The common path **does not** use `preventDefault`.

**Opt-in — `relayControlled` (working name):** A boolean vnode prop meaning *suppress the platform default for this node and route activation to CleverScript* (when a handler is registered). Web hosts call **`preventDefault` only in this opt-in case** (not on every anchor). Android and other hosts apply the analogous “don’t run default intent; invoke handler” rule.

For **markdown** anchors with **`anchorOnClick`**, the renderer should set **`relayControlled`** on generated `<a>` nodes so in-app routing works without requiring `preventDefault` as the default for all links.

The same **native default / `relayControlled`** rule applies to **other elements the host already implements** with a real platform default (**`<button>`**, **`<input>`** relevant types, **`<form>`**, etc.). Elements not yet wired on a given host are out of scope until that host adds them. Android parity checklist: [`relay-client-android/docs/RELAY_RUNTIME_PARITY_TODOS.md`](../../relay-client-android/docs/RELAY_RUNTIME_PARITY_TODOS.md).

## Relation to cross-platform events

Hosts should map native input into a **small, documented event record** (see plan / future **Relay event** spec), not a full DOM `Event` object. **Whether** to suppress the platform default is determined by **`relayControlled`** (and host policy for modified clicks, etc.), not by CleverScript calling `preventDefault`.

## See also

- [`RELAY_MARKDOWN_IR.md`](RELAY_MARKDOWN_IR.md) — markdown + `anchorOnClick` contract.
- [`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`](RELAY_EMBED_CLIENT_GET_INTERCEPT.md) — parked-site embed is WASM-only (no npm React); React-as-native deferred.
- [`relay-runtime/docs/CROSS_PLATFORM_OS_SUPPORT_PLAN.md`](../../relay-runtime/docs/CROSS_PLATFORM_OS_SUPPORT_PLAN.md) — native components and host-owned media/UI.

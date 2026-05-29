# md2ast placement (ADR)

**Status:** Accepted for the Universal Relay client program.

## Decision

1. **Single source of truth:** Use the **`md2ast`** crate ([crates.io](https://crates.io/crates/md2ast), repository [clevertree/md2ast](https://github.com/clevertree/md2ast)) as the **Rust Markdown → structured AST** implementation.

2. **relay-runtime (WASM):** **`relay-runtime` depends on `md2ast`** and exposes **`markdown_to_ast_json`** (and related helpers) from **`wasm_api.rs`** so **`relay-client-web`** (and any WASM host) can parse markdown **without a second WASM module**. This keeps one build pipeline for CleverScript + markdown.

3. **relay-gateway:** When the **GET-hook Markdown→HTML** plan is unpaused, **`relay-gateway` should depend on the same `md2ast` crate** (or, if HTML-only output is needed, call into shared helpers next to `md2ast::parse` → serialize HTML in gateway code). Avoid duplicating markdown parsing logic in `pulldown-cmark` alone unless `md2ast` cannot produce the required shape.

4. **Not chosen for default app bundling:** Shipping **`md2ast` as a standalone browser package** in parallel to relay-runtime WASM is **discouraged**—it doubles payload and diverges behavior; the **relay-runtime WASM** bundle remains the single client core.

## Rationale

- **md2ast** already targets **Web + Android** and uses **pulldown-cmark** with GFM-style options.
- **relay-runtime** already ships **wasm-bindgen** exports; adding AST JSON keeps the **IR contract** stable for the TypeScript shell to map to DOM or to future CleverScript VNodes.
- **Math/KaTeX:** Display math is **designed** in [`RELAY_MARKDOWN_MATH_KATEX.md`](RELAY_MARKDOWN_MATH_KATEX.md) (companion doc). Parsing `\( \)` / `$$` may extend **md2ast** or pre-process markdown before `parse`.

## Consequences

- **relay-runtime** release artifacts grow slightly (md2ast + pulldown-cmark transitive).
- **Version alignment:** Bump `md2ast` in **Cargo.toml** in lockstep when the upstream crate publishes fixes.

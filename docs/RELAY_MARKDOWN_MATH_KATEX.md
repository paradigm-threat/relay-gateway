# Markdown math and KaTeX (design)

**Purpose:** Satisfy the program gate: **Phase B** requires **remark-math / rehype-katex-class** *behavior* — display of LaTeX in articles — without relying on **Next.js** or **MDX**.

## Current state (paradigm-threat-site)

- **remark-math** detects `$inline$` and `$$block$$`.
- **rehype-katex** renders to HTML with KaTeX.

## Target architecture (Relay universal client)

1. **Parse path:** **`md2ast`** produces an AST. For math, one of:
   - **A)** Extend **md2ast** to emit **`span`/`div`** nodes with **`className: math-inline` / `math-display`** and **raw LaTeX** in children or `data-latex` prop when the parser sees `$…$` / `$$…$$`; or
   - **B)** A **preprocessing pass** (Rust or TS) that converts math spans to **HTML placeholders** before `md2ast::parse`, with **allowed_tags** including `span` and required `className`s.

2. **Render path:** **`relay-client-web`** loads **KaTeX** (JS + CSS) and runs **`katex.renderToString`** (or `render`) on elements matching those classes — same visual model as **rehype-katex**.

3. **SSG/SEO (later):** Optional prerender of KaTeX output for crawlers — **out of scope** until Phase D.

## Action items (implementation)

- Prototype **(B)** first in **TypeScript** next to AST JSON to validate KaTeX bundle size and CSP.
- Move stable preprocessing to **Rust** in **md2ast** or **relay-runtime** when patterns are fixed.

This document is the **design approval** for Phase B math parity; implementation can land incrementally after the shell fetch path exists.

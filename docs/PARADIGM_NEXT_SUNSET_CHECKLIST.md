# paradigm-threat-site Next.js sunset checklist (Phase D)

Use this when **relay-client-web** + repo CleverScript cover authoring, **TTS/chat APIs** remain stable, and SEO/SSO are acceptable.

## Preconditions

- [ ] **relay-runtime WASM** loads hooks + **md2ast** (`markdown_to_ast_json` / `MarkdownAstPreview`) for all article routes in the new shell.
- [ ] **Math / KaTeX** design in [`RELAY_MARKDOWN_MATH_KATEX.md`](RELAY_MARKDOWN_MATH_KATEX.md) is **implemented** (Phase B gate).
- [ ] **paradigm-threat-files** repo-local client hooks (changelog, chat, TOC, directory) ship and are exercised.
- [ ] **paradigm-threat-timeline** consumed from the **same** universal client (per program decision).

## SSO / cookies

- [ ] Same-site / cross-origin policy documented for **files mirror** vs **app shell**.
- [ ] **Chat / TTS** continue on **HTTPS `/api`** endpoints if still hosted on paradigm or adjacent hosts.

## SEO / crawlers

- [ ] Decide: negotiated **HTML from relay** (when GET-hook plan unpauses) vs prerender vs meta-only.

## Cutover

- [ ] Redirect or park **`paradigm-threat-site`** Next app; keep static fallbacks until confidence window ends.

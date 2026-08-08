# CleverScript ↔ Rust host helpers

**Direction (2026-08):** tip CleverScript is mostly **routing and UI composition**. Expensive or shared logic lives in **Rust** (`relay-runtime` `@relay/*` tip modules and gateway natives). Do not grow tip-local parsers, sort keys, date tables, or catalog walks when a host helper can own them.

## Shipped tip modules (`relay-runtime`)

Registered in `relay-runtime/src/relay_modules.rs` (SSR + WASM). Spec table: `relay-runtime/docs/CLEVERSCRIPT.md`.

| Module | Role |
|--------|------|
| `@relay/util` | `sortBy`, `take`, `prop`, `omitUndefined`, `slugify`, `uniqueId`, `includes`, `splitSentences`, `takeSentenceChunk` |
| `@relay/path` | path join/normalize, `siteHref`, asset URL rewrite, gallery `#img=` / DOM id / portrait index lookup |
| `@relay/date` | ChangeLog / feed date formats |
| `@relay/catalog` | nav-index parse, TOC extract, dir tree, Feeling Lucky |
| `@relay/ui` | `cachedGet`, `Link`, `RadioTabs`, `safeProps` |
| `@relay/tts` | Piper synthesize + audio player vnode |

## Shipped gateway natives (server GET / hooks)

| Module | Role |
|--------|------|
| `@relay/md` | Markdown → HTML from GET context |
| `@relay/ssr` | `ssrClientHookFromGetContext` |
| `@relay/web-assets` | Commit-pinned embed/WASM prefixes |
| `@relay/image` | Raster optimize / `?w=` / `portraitSiblingIfPresent` |
| `@relay/ipfs` | Manifest / CID fetch + disk cache |
| `@relay/search/sqlite` (etc.) | QUERY FTS — CS only `import { search }` |
| `@relay/git` | Server hook file reads |

Park tip already follows this for GET (`hooks/server/get.cs` is a thin chain) and QUERY (`query.cs`).

## Next helper candidates (prefer Rust)

When tip code grows loops that look like libraries, extract here and add **`relay-runtime` tests** (`tests/stdlib_relay_modules.rs` or new cases) **before** tip call sites.

| Candidate | Why |
|-----------|-----|
| Stronger markdown preprocess (math / KaTeX prep) | See `RELAY_MARKDOWN_MATH_KATEX.md` — keep CS out of tokenization |
| Share snippet / OG description builders | Same string rules on SSR + CSR; avoid tip duplication |
| Keyword / Option B token helpers | Align with `check-keywords` without CS regex |
| **Gallery hash / `#img=` / DOM id** | **Shipped** — `@relay/path` `readHashImgSrc` / `galleryImgHash` / `safeImgDomId` / `galleryDisplaySrc` / `portraitSiblingIfPresent(map)` |
| **TTS sentence chunking** | **Shipped** — `@relay/util.splitSentences` / `takeSentenceChunk` |
| **TOC ATX extract** | **Shipped** — tip is thin wrap over `@relay/catalog.extractMarkdownHeadings` |
| Sitemap / nav-index **writers** | Stay in Python/CI; **readers** already `@relay/catalog` |

## Rules for new helpers

1. Implement in **`relay-runtime`** (tip-visible) or **`relay-gateway`** (server-only).
2. Export via `@relay/…`; document in `CLEVERSCRIPT.md` + park `hooks/CLEVERSCRIPT_TIP.md`.
3. Add a **regression test** in the same PR/commit series.
4. Tip call sites must **prefer** the helper; delete tip-local copies.
5. Local park loop: `./scripts/park-local.sh unit` then `serve` + `smoke` — see [`PARADIGM_PARK_LOCAL.md`](./PARADIGM_PARK_LOCAL.md).

## Related

- Portrait / PopImage deploy checklist: [`PORTRAIT_SIBLING_DEPLOY.md`](./PORTRAIT_SIBLING_DEPLOY.md)
- Gap log: [`CLEVERSCRIPT_GAP_LOG.md`](./CLEVERSCRIPT_GAP_LOG.md) (G-007 closed = first helper wave)
- Policy: [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md)
- Server hooks: [`SERVER_HOOKS.md`](./SERVER_HOOKS.md)

*Last updated: 2026-08-08*

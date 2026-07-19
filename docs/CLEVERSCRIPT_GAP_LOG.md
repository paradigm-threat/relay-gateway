# CleverScript / relay-runtime parity gap log

Maintain this list while porting React (paradigm-threat-site) components to **CleverScript** and **relay-runtime WASM**.

| ID | Symptom | Repro | Tracker |
|----|---------|-------|---------|
| G-001 | Docs claimed fragments / object destructuring / try/catch / typeof unsupported or incomplete | Spec sync in `relay-runtime/docs/CLEVERSCRIPT.md` | Closed: language sugar + hardening tests (2026-07) |
| G-002 | Rest params / array+object spread / compound assign / `%` / prefix `++` / optional call / `for…of` / `switch` / `break`/`continue` / default params / `export function` missing vs ES6 habits | Template + demo hooks | Closed: implemented with unit tests in `relay-runtime/tests/cleverscript_sugar.rs` |
| G-003 | JSX prop spread `<Foo {...props}>` still unsupported | N/A | Open (intentionally deferred) |
| G-004 | Mutable `push`/`pop` still unsupported | N/A | Open (immutable design) |
| G-005 | Parked host SSR failure surfaced as **Unknown Host** (404) instead of 5xx | Broken client hook on parked domain GET `/` | Closed: gateway returns 500 HTML with SSR wording (2026-07) |
| G-006 | Same-host QUERY / `fetch` during site-shell SSR deadlocks gateway worker | ChangeLog full-index QUERY on GET `/` | Closed: tip uses `nav-index.txt` only in SSR paths; documented in tip hygiene |
| G-007 | Tip reinvented path/date/sort/nav-index parse (no ES Date/`parseInt`/Array#sort) | paradigm-threat-files ChangeLog/DirNav | Closed: ES builtins + `@relay/util\|path\|date\|catalog\|ui` (2026-07) |

**Process:** For each gap, open an issue in **relay** (relay-runtime) or **md2ast** as appropriate; link the issue URL in **Tracker**; fix the language or Host API; add a **regression test** in relay-runtime when possible.

**Related:** [`RELAY_UNIVERSAL_CLIENT_AND_PARADIGM_SITE.md`](RELAY_UNIVERSAL_CLIENT_AND_PARADIGM_SITE.md)

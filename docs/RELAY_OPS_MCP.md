# Relay ops / brain MCP tool catalog

Canonical list: [`ops/mcp-tools.json`](../ops/mcp-tools.json).

Use this when wiring **WebMCP** (in-browser agents on the ops UI), documenting Cursor/CLI usage, or adding a future Cursor MCP server — expose **this full list**, not a silent subset.

## How Cursor should work a blocked brain

1. `relay-ops brain requests` — read pending ids / fingerprints.
2. If unknown/BLOCKED: edit `ops/scenarios/*.yaml`, commit, then `relay-ops brain approve <id>` (or decline).
3. Or `relay-ops brain say …` to leave notes in the feed.
4. `relay-ops brain handoff <id>` prints a ready-made prompt.

## Conventions

| Field | Meaning |
| ----- | ------- |
| `cli` | Exact or near-exact CLI |
| `http` | Brain loopback path (ops UI proxies `/api/brain/*` with server-side token) |
| `job_kind` | `POST /api/job` `kind` in relay-ops UI |
| `brain_gated` | Refused while brain is online until request approved / allowlisted |
| `needs_token` | Bearer / `X-Relay-Brain-Token` |

## Brain tools

See JSON entries `brain_*`. High-risk: `brain_approve`, `brain_resume`.

## Fleet job tools

See JSON entries `ops_*`. Prefer Brain tab approval before `ops_repair` / `ops_reclaim` when brain is running. **`ops_sync`** is brain-gated for tip-drift (`catalog_sync`); missing catalog repos (`catalog_missing`) auto-sync when `RELAY_BRAIN_EXECUTE=1`.

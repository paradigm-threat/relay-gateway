# Relay brain (local)

Local, observe-first fleet brain: classify probe fingerprints against [`ops/scenarios/`](../ops/scenarios/), open **pending requests** when confirmation is needed, append a private JSONL feed, and talk via **relay-ops** CLI/UI.

Server deploy is out of scope here — see a later plan.

## Quick start

```bash
# From relay-gateway checkout (scenarios load from ops/scenarios)
cargo build --bin relay-brain --bin relay-ops
./target/debug/relay-brain          # or: relay-ops brain serve
# Safe remediations (catalog_sync, index_stale): --execute or RELAY_BRAIN_EXECUTE=1
# High-risk / BLOCKED still open pending requests for Approve

# User systemd (after installing binaries to ~/.local/bin)
cp ops/systemd/relay-brain.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now relay-brain.service
```

Default bind: `http://127.0.0.1:7781`  
Token: `RELAY_BRAIN_TOKEN` or auto-created `~/.local/state/relay-brain/token`  
Feed: `~/.local/state/relay-brain/feed.jsonl`

Brain stops when the laptop sleeps/off (user systemd).

## Operator workflow

1. Open relay-ops UI → **Brain** tab (or `relay-ops brain status` / `requests`).
2. If pending requests: **Approve**, **Decline**, or **Hand off to Cursor** (copies a prompt).
3. When BLOCKED/unknown: extend `ops/scenarios/*.yaml`, then approve/resume.
4. With brain online, gated jobs that **require confirmation** (`repair`, `reclaim`, tip-drift `catalog_sync`, …) are **refused** until the matching request is approved (or brain is offline — classic ops still works). **`catalog_missing`** (catalog allowlist repo absent on disk) and **`index_stale`** have `requires_confirm: false`: no Approve card, and with execute mode the brain runs them automatically (ensure-bare + peer fetch from a healthy master).

## CLI

| Command | Purpose |
| ------- | ------- |
| `relay-ops brain serve [--execute]` | Same as `relay-brain` |
| `relay-ops brain status` | Status JSON |
| `relay-ops brain log --tail N` | Feed tail |
| `relay-ops brain requests` | Pending requests |
| `relay-ops brain approve\|decline\|handoff <id>` | Resolve / copy prompt |
| `relay-ops brain say <text>` | Chat (`status`, `blocked`, `pause`, `resume`, or free text) |
| `relay-ops brain resume\|pause` | Control plane |

Env: `RELAY_BRAIN_URL` (default `http://127.0.0.1:7781`), `RELAY_BRAIN_TOKEN`, `RELAY_SCENARIOS_DIR`, `RELAY_BRAIN_BIND`, `RELAY_BRAIN_EXECUTE` (`1`/`true` enables auto-run of non-confirm scenarios).

## WebMCP + Gemini Nano (progressive)

Brain chat/requests/feed work with **zero** browser AI.

- **WebMCP:** feature-detect `document.modelContext.registerTool`. Tools wrap the same `brainClient` as UI buttons. Enable Chrome flag `chrome://flags/#enable-webmcp-testing` (or origin trial) for local testing. Tool catalog: [`ops/mcp-tools.json`](../ops/mcp-tools.json) and [`RELAY_OPS_MCP.md`](RELAY_OPS_MCP.md).
- **Gemini Nano (Prompt API):** optional **AI summarize feed** — advisor only; never auto-approves.

## Scenarios

YAML under `ops/scenarios/`. Shared matcher with UI recommendations (`recommend` → scenario id + specialized message). Tip-divergence parents include `replica_ahead_of_publisher` and `unknown_tip_divergence` (BLOCKED). Grow leaves from live incidents — do not invent encyclopedic stubs.

## Prove before growing

Validate against a few real fingerprints (node down, catalog drift, tip ahead) and at least one request card before expanding the corpus.

# Relay mesh ops — push latency, dead peers, deploy order

**Date:** 2026-07-24  
**Companion:** [`SERVER_HOOKS.md`](./SERVER_HOOKS.md), [`RELAY_FLEET_CONSOLE.md`](./RELAY_FLEET_CONSOLE.md), [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md).

## Push cost model

| Phase | What runs | Client waits? |
|-------|-----------|---------------|
| **`pre-receive`** | Rust `branchRules` + optional CleverScript | **Yes** (must — can reject) |
| **`update`** (catalog **publish_origin** only) | Publish tip, tip-notify peers, poll until fleet matches or timeout | **Yes** — timeout or lagging peer → **`ng`** (push fails, tip reverted) |
| **`post-receive`** | Index inline, Slack, optional autoPush | **Partial** (index blocks; notify skipped on publish_origin — done in `update`) |

**Policy stays fail-closed on `pre-receive` and publish-origin `update`.** Mesh sync timeout is a **hard publish failure**, not a warning after ACK.

### Env timeouts

| Variable | Default | Meaning |
|----------|---------|---------|
| **`RELAY_MESH_SYNC_WAIT_SECS`** | `45` | Max wait after tip-notify for all peers to show the pushed SHA (publish-origin `update` hook). |
| **`RELAY_MESH_SYNC_POLL_MS`** | `1000` | Poll interval during mesh wait. |
| **`RELAY_PEER_NOTIFY_TIMEOUT_SECS`** | `12` | Per-peer HTTP tip-notify. Ops often set **`3`**. |
| **`RELAY_SLACK_TIMEOUT_SECS`** | `12` (webhook) / `10` (history) | Slack Incoming Webhook + dedup history. |
| **`RELAY_AUTOPUSH_TIMEOUT_SECS`** | `60` | Per-origin `git push` under `timeout(1)`. |
| **`RELAY_PEER_FETCH_TIMEOUT_SECS`** | `60` (Rust) / `30` (startup script) | Peer `git fetch` / startup catalog peer fetch. |

Prefer **tip-notify** (`POST /hooks/peer`) over enabling **`git.autoPush`** until you accept bounded push timeouts.

### Post-push mesh sync (agents / go-live)

On catalog repos with **`publish_origin=iad1`**, **`git push iad1 HEAD:<branch>`** blocks in the **`update` hook** until **sjc1/hkg1** (from **`RELAY_MASTER_PEER_LIST`**) match the new tip, or the wait budget expires. **Success** = normal `git push` exit 0. **Timeout / partial** = push fails (`ng`) with `remote: mesh sync failed: …` on stderr.

Then push **GitLab** (forge backup). No client polling script required for normal agent go-live.

**Operator / debug** (after a failed push or when diagnosing lag):

```bash
relay-gateway/scripts/relay-push-and-wait.sh iad1 HEAD:main
# optional — same checks as the server hook; exit 0/1/2
```

API snapshot (does not replace server gate on push):

```bash
curl -sS -X POST "https://iad1.relaygateway.net/api/sync/mesh/paradigm-threat-files?branch=main&sha=$(git rev-parse HEAD)" \
  | jq '.mesh_sync.status, .mesh_sync.peers, .mesh_sync.last_notify'
```

Publisher state: **`/opt/relay/state/post_push_sync/<repo>.json`** (per-peer notify ok/fail + HTTP errors).

Content-repo agent canon: **`paradigm-threat-files/AGENTS.md`** and **`docs/GIT_REMOTE_STRATEGY.md`**.

### Hung client push (operator)

1. Push **one Relay at a time** with a client timeout (e.g. 90s).
2. If the client hangs: `git ls-remote <remote> refs/heads/main` — if the tip already moved, **kill the client**; the pack was accepted and `post-receive` side effects may still be running or a dead peer notify is timing out on an **old** binary.
3. Do **not** assume GitLab tip == Relay tip; check all three Relays.

## Dead peer (e.g. sjc1 Spot down)

Symptoms:

- `peer notify HTTP failed` toward that FQDN
- Laptop `git push sjc1` hangs / times out
- `POST /api/status?strict=1` → 503

Actions:

1. Push **`iad1` / `hkg1`** (and GitLab) so content is live elsewhere.
2. Exclude the dead Spot from **`fleet.relaygateway.net`** via `./scripts/relay-fleet.sh fleet-dns` (see [`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md)).
3. Repair or replace the node; redeploy with canary order below.
4. Tip-notify `notifyMode: next` **retries next+1 once** when the first hop fails (gateway ≥ this doc’s date).

## Deploy order (canary)

1. `./scripts/relay-fleet.sh ssh-check`
2. `./scripts/relay-fleet.sh status` (optional: `curl -sS -X POST 'https://<host>/api/status?strict=1'`)
3. Deploy **non-primary** peers first: e.g. `--only=sjc1…` then `hkg1…`, smoke `health`, then **iad1** (publisher)
4. Or full `./scripts/relay-fleet.sh --build update` — after each host update+restart, the script runs a **post-host health gate** (`POST /api/config`) before continuing when **`RELAY_FLEET_UPDATE_HEALTH=1`** (default **on**). Pre/post **`ssl`** checks report park Host HTTPS (warn by default; **`RELAY_FLEET_SSL_GATE=1`** to hard-fail).

Always **`--build`** after Rust changes so `relay-hook-handler` (push path) ships with `relay-gateway`.

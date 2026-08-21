# Relay mesh ops — push latency, dead peers, deploy order

**Date:** 2026-07-24  
**Companion:** [`SERVER_HOOKS.md`](./SERVER_HOOKS.md), [`RELAY_FLEET_CONSOLE.md`](./RELAY_FLEET_CONSOLE.md), [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md).

## Push cost model

| Phase | What runs | Client waits? |
|-------|-----------|---------------|
| **`pre-receive`** | Rust `branchRules` + optional CleverScript | **Yes** (must — can reject) |
| **`post-receive`** | Schedules Slack, index warm, peer notify, autoPush | **No** (side effects are spawned / timed out) |

**Policy stays fail-closed on `pre-receive`.** Index warm, Slack, and mesh tip-notify are **best-effort after** the ref updates. First QUERY after a push may wait briefly on the branch index mutex until warm finishes — preferred over hung `git push`.

### Env timeouts

| Variable | Default | Meaning |
|----------|---------|---------|
| **`RELAY_PEER_NOTIFY_TIMEOUT_SECS`** | `12` | Per-peer HTTP tip-notify. Ops often set **`3`**. |
| **`RELAY_SLACK_TIMEOUT_SECS`** | `12` (webhook) / `10` (history) | Slack Incoming Webhook + dedup history. |
| **`RELAY_AUTOPUSH_TIMEOUT_SECS`** | `60` | Per-origin `git push` under `timeout(1)`. |
| **`RELAY_PEER_FETCH_TIMEOUT_SECS`** | `60` (Rust) / `30` (startup script) | Peer `git fetch` / startup catalog peer fetch. |

Prefer **tip-notify** (`POST /hooks/peer`) over enabling **`git.autoPush`** until you accept bounded push timeouts.

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

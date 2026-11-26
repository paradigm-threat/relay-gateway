# Relay mesh ops — tip sync vs SearchStore

**Companion:** [`RELAY_PUBLISH_ONCE.md`](./RELAY_PUBLISH_ONCE.md), [`RELAY_OPS.md`](./RELAY_OPS.md), [`SERVER_HOOKS.md`](./SERVER_HOOKS.md).

Linode fleet topology: `pub1` (publisher) + `mesh1` / `mesh2` (mesh peers). See [`ops/fleet.json`](../ops/fleet.json) and [`ops/CREDIT_BURN.md`](../ops/CREDIT_BURN.md).

## Mesh sync ≠ index sync

| Layer | Meaning | Blocks push? |
|-------|---------|--------------|
| **Git tip** | Peer bare `refs/heads/<publish_branch>` matches publish_origin tip | Yes (publish-origin `update` mesh wait) |
| **SearchStore** | Local `docs_indexed_head` matches that tip | **No** — warmed per node after tip accept |

Operators: fleet UI “out of sync” = git tip / missing bare. Stale QUERY (`indexFresh: false`) = local index lag; use **Warm indexes** only as escape hatch (gateway should warm on every accepted tip).

## Push cost model

| Phase | What runs | Client waits? |
|-------|-----------|---------------|
| **`pre-receive`** | Rust `branchRules` + optional CleverScript; catalog `publish_branch` **fail-closed** SearchStore warm | **Yes** |
| **`update`** (catalog **publish_origin** only) | Tip-notify peers; poll until fleet git tips match or timeout | **Yes** — git tip only (not remote SearchStore) |
| **`post-receive` / peer-fetch** | Local SearchStore warm (retries; peer path async + `index_pending`) | Index after tip accept |

### Env timeouts

| Variable | Default | Meaning |
|----------|---------|---------|
| **`RELAY_MESH_SYNC_WAIT_SECS`** | `45` | Max wait after tip-notify for peers to show the pushed SHA |
| **`RELAY_MESH_SYNC_POLL_MS`** | `1000` | Poll interval during mesh wait |
| **`RELAY_PEER_NOTIFY_TIMEOUT_SECS`** | `12` | Per-peer HTTP tip-notify |
| **`RELAY_PEER_FETCH_TIMEOUT_SECS`** | `60` | Peer `git fetch` |

Prefer **tip-notify** (`POST /hooks/peer`) over `git.autoPush` until you accept bounded push timeouts.

### Post-push mesh sync

On catalog repos with **`publish_origin=pub1`**, **`git push pub1 HEAD:<branch>`** blocks in the **`update` hook** until mesh peers (from **`RELAY_MASTER_PEER_LIST`**) match the new tip, or the wait budget expires.

```bash
curl -sS -X POST "https://pub1.relaygateway.net/api/sync/mesh/paradigm-threat-files?branch=main&sha=$(git rev-parse HEAD)" \
  | jq '.mesh_sync.status, .mesh_sync.peers'
```

## Dead peer

Symptoms: `peer notify HTTP failed`, laptop push hangs, `POST /api/status?strict=1` → 503.

Actions:

1. Push remaining healthy peers (+ GitLab) so content is live elsewhere.
2. Keep **`fleet.relaygateway.net`** on publisher only (`RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net`).
3. Repair or replace the Linode; redeploy with canary order below.

## Deploy order (canary)

1. `relay-ops ssh-check` / `status`
2. Deploy **mesh** peers first (`linode-mesh1`, `linode-mesh2`), smoke health, then **pub1**
3. `./scripts/relay-master-deploy.sh --target linode-pub1 --install-only --build`

Always **`--build`** after Rust changes so `relay-hook-handler` ships with the gateway.

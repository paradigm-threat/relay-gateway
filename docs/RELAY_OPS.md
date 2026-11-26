# Relay ops (`relay-ops`)

Linode-first fleet **CLI + local UI** (no Spot reclaim, no brain scenarios). Topology: [`ops/fleet.json`](../ops/fleet.json) (`schema: relay.ops.fleet.v1`). Credit burn calendar: [`ops/CREDIT_BURN.md`](../ops/CREDIT_BURN.md).

`./scripts/relay-fleet.sh` remains a thin wrapper around `relay-ops`.

## Product shape

| Concern | Tool |
|---------|------|
| Observe nodes | `relay-ops serve` → `http://127.0.0.1:7780` |
| Tip / catalog lag | UI **Synchronize** / `relay-ops sync` |
| SearchStore lag | UI **Warm indexes** (escape hatch); gateway should warm on every accepted tip |
| DNS | `point-dns`, `fleet-dns` (Vercel); **fleet A = publisher only** (`fleetDns: true`) |
| Redeploy | `update` / Repair button → Linode seed redeploy |

**Not included:** Spot reclaim, brain Approve/Decline, scenario YAML.

## Topology roles

```json
{ "fqdn": "pub1.relaygateway.net", "role": "publisher", "fleetDns": true }
{ "fqdn": "mesh1.relaygateway.net", "role": "mesh", "fleetDns": false }
```

`RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net` — never put mesh nodes in `fleet.relaygateway.net` multi-A.

## Bootstrap

```bash
cargo build --release --bin relay-ops
./target/release/relay-ops status --gitlab https://gitlab.example/group/relay-gateway
./target/release/relay-ops serve
```

Fetch order for topology: GitLab `ops/fleet.json` → local checkout → env peer list → cache.

**Park TLS:** list park apexes in `ops/fleet.json` `health.sslDomains`; `ssl` probes per-node SNI.

## Inventory

- **Admin:** SSH + identity (`~/.ssh/id_ed25519_relay_gateway`)
- **DNS buttons:** Vercel token
- **Observer:** no SSH required for `status` / `serve` probes

## CLI (selected)

| Command | Notes |
|---------|--------|
| `hosts` / `status` / `health` / `ssl` | Observer probes |
| `serve` | Thin UI `:7780` |
| `sync` | Catalog ensure-bare + peer fetch |
| `update` / `deploy-all` | Pack + Linode redeploy seed |
| `features-set` | Reconfigure optional features; **Piper TTS on by default** (`--piper 0` to disable) |
| `index-warm` (via UI job) | POST `/api/index/warm` |
| `fleet-dns` / `point-dns` | Vercel; fat/publisher IPs only for fleet A |
| `fix-repo-ownership` | `chown relay:relay` on bares |
| `known-hosts-reset` | `ssh-keygen -R` |
| `brain *` | Stub — disabled |

## Index warm (gateway)

Accepted tips must leave SearchStore current:

- **pre-receive** (catalog `publish_branch`): fail-closed `ensure_indexed_with_retries`
- **post-receive**: retrying warm (idempotent)
- **peer-fetch**: `spawn_ensure_indexed` with retries + `index_pending` / `index_last_error` on `/api/status`
- **HTTP PUT/DELETE**: warm after tip update
- **reconcile**: head + catalog publish_branch

Manual warm remains an escape hatch only.

## Current Linode credit fleet (2026-11-26)

See [`ops/CREDIT_BURN.md`](../ops/CREDIT_BURN.md) + [`ops/credit_burn.json`](../ops/credit_burn.json): pub1 8 GB us-iad + mesh1/mesh2 4 GB.

Fleet UI shows **credit runway** and **warnings** (down meshes, ghost peers still in `RELAY_MASTER_PEER_LIST`, index pending/errors). Before destroying meshes:

```bash
./scripts/relay-scale-down-check.sh          # writes ops/scale-down-last-check.log
# shrink RELAY_MASTER_PEER_LIST on pub1 → then destroy mesh Linodes
./scripts/relay-scale-down-check.sh --post
```

Point FQDNs when Vercel token is available:

```bash
relay-ops point-dns --fqdn pub1.relaygateway.net --ip 172.234.148.70
relay-ops point-dns --fqdn mesh1.relaygateway.net --ip 172.237.134.22
relay-ops point-dns --fqdn mesh2.relaygateway.net --ip 172.236.146.220
export RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net
relay-ops fleet-dns --yes
```

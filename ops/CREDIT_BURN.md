# SUPPORT10060 credit burn calendar

Promo: **SUPPORT10060** — **$100.00** remaining, expires **2027-02-01T04:59:59Z**.  
Provisioned **2026-11-26** (no Nanodes). Machine-readable: [`credit_burn.json`](./credit_burn.json) (Fleet UI runway banner).

## Fleet

| Label | Role | Region | Type | $/mo | $/hr | IPv4 | Linode ID |
|-------|------|--------|------|------|------|------|-----------|
| pub1 | publisher + fleet A + bake | us-iad | g6-standard-4 (8 GB) | 48 | 0.072 | 172.234.148.70 | 111983927 |
| mesh1 | mesh | us-ord | g6-standard-2 (4 GB) | 24 | 0.036 | 172.237.134.22 | 111983931 |
| mesh2 | mesh | sg-sin-2 | g6-standard-2 (4 GB) | 24 | 0.036 | 172.236.146.220 | 111983933 |
| **Total** | | | | **96** | **0.144** | | |

Full 3-node runway: `$100 / $0.144 ≈ 29 days` from provision (~**2026-12-25** scale-down window).

## Calendar

1. **Day 0–29:** run all three; exercise mesh tip sync + SearchStore warm.
2. **~Day 29** (balance ≈ $0–5): destroy **mesh1** and **mesh2**. Keep **pub1** only if residual credit or paid continue; else power off before card charges.
3. Do **not** enable backups / NodeBalancers / Object Storage on this credit unless intentional.

## DNS / topology

- FQDNs: `pub1.relaygateway.net`, `mesh1.relaygateway.net`, `mesh2.relaygateway.net`
- `fleet.relaygateway.net` A → **pub1 only** (`RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net`)
- `ops/fleet.json`: pub1 `role=publisher` `fleetDns=true`; meshes `role=mesh` `fleetDns=false`
- **IRC / chat primary** stays on **pub1** (`CHAT_PRIMARY_FQDN=pub1.relaygateway.net`). Destroying meshes must not leave edge proxies aimed at dead hosts; shrink peer lists before destroy.
- **Piper TTS** (`features.piperTts`) on **all** nodes — deploy / `features-set` default `RELAY_FEAT_PIPER=1` (`/voices`, `/tts`)

---

## Scale-down runbook (promo → publisher-only)

**Hard rule:** shrink **`RELAY_MASTER_PEER_LIST` on pub1** (and catalog/env that feeds it) **before** destroying mesh Linodes. If pub1 still tip-notifies dead peers, **publish `update` mesh-wait fails** and authors cannot push.

### Pre-destroy checklist

```bash
# From laptop (relay-gateway/)
./scripts/relay-scale-down-check.sh              # readiness + log tails
relay-ops status                                 # mesh nodes should show DOWN only AFTER destroy
# Fleet UI :7780 — banner shows credit days left + ghost-peer warnings
```

1. Confirm **pub1** healthy, `index_ok`, tips sync, park TLS OK.
2. On **pub1**, set peer list to publisher-only (example):
   - `RELAY_MASTER_PEER_LIST=pub1.relaygateway.net`
   - Restart gateway / reconfigure-features so hooks pick it up.
3. Confirm `curl -sS https://pub1.relaygateway.net/api/status?peers=0 | jq '.master_peers_configured'` is only pub1 (or empty self-skip).
4. Smoke: `git push` a trivial tip to pub1 — must ACK without `mesh sync failed`.
5. Update local/GitLab **`ops/fleet.json`** → remove mesh1/mesh2 nodes (or leave them marked destroyed; UI will show DOWN).
6. `linode-cli linodes delete 111983931` / `111983933` (mesh1/mesh2). Keep `111983927` if continuing.
7. `relay-ops fleet-dns --yes` with `RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net`.
8. Re-run `./scripts/relay-scale-down-check.sh --post` and keep the log artifact.

### Visibility / logs (keep these)

| Where | What |
|-------|------|
| Fleet UI snapshot | Credit runway, down meshes, ghost peers still in publisher peer list, index stale/pending |
| `./scripts/relay-scale-down-check.sh` | Writes `ops/scale-down-last-check.log` + prints peer list / journal excerpts |
| On each node | `journalctl -u relay-gateway -u relay-hook-handler --since "2 hours ago"` — mesh notify, `ensure_indexed`, `index_pending` |
| On publisher | `/opt/relay/state/post_push_sync/*.json` — last mesh tip-notify per repo |
| `/api/status` | `repos[].index_fresh`, `index_pending`, `index_last_error`; `peers[].reachable`; `master_peers_configured` |

### Thin / solo publisher expectations

- Mesh tip sync is **N/A** with one node — do not leave dead FQDNs in peer list.
- SearchStore warm still required on pub1 for every accepted tip (fail-closed publish_branch).
- QUERY `indexFresh` is **local** — no replica warm after meshes are gone.
- 4 GB boxes (while still up): watch disk (`df -h /opt/relay`) and OOM (`dmesg | grep -i oom`); bake/park TLS stay on **pub1 8 GB**.

### Linode balance watch

```bash
linode-cli account view
# Destroy meshes when remaining credit ≈ few dollars, before hourly overage.
```

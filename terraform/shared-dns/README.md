# Shared DNS helpers (Vercel)

Provider-agnostic scripts used by **`relay-master-deploy.sh`**, **`relay-fleet.sh fleet-dns`**, and operators.

| Script | Purpose |
|--------|---------|
| **`scripts/vercel-dns-upsert.sh`** | Upsert apex **`{node}.relaygateway.net`** A record |
| **`scripts/cleanup-vercel-dns.sh`** | Delete stale A records for a subdomain |
| **`scripts/vercel-dns-wildcard.sh`** | Upsert **`\*.{node}`** on **`relaygateway.net`** for repo vhosts |
| **`scripts/vercel-dns-fleet-reconcile.sh`** | Reconcile multi-A/AAAA for **`fleet.relaygateway.net`** from node FQDNs |

Requires **`VERCEL_API_TOKEN`** (or Vercel CLI auth JSON). Optional **`VERCEL_TEAM_ID`**.

## Fleet ingress reconcile

After a Spot node IP change (and after the node A record is updated):

```bash
# From relay-gateway/ with relay.env loaded:
./scripts/relay-fleet.sh fleet-dns --dry-run
./scripts/relay-fleet.sh fleet-dns

# Or directly:
./terraform/shared-dns/scripts/vercel-dns-fleet-reconcile.sh \
  iad1.relaygateway.net ord1.relaygateway.net hkg1.relaygateway.net
```

`relay-master-deploy.sh` runs the same reconcile automatically after upserting the node + wildcard records (unless **`--skip-dns`**).

See **[`docs/RELAY_NETWORK_DNS.md`](../../docs/RELAY_NETWORK_DNS.md)**.

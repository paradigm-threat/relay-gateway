# Shared DNS helpers (Vercel)

Provider-agnostic scripts used by **`relay-master-deploy.sh`** and operators.

| Script | Purpose |
|--------|---------|
| **`scripts/vercel-dns-upsert.sh`** | Upsert apex **`{node}.relaygateway.net`** A record |
| **`scripts/cleanup-vercel-dns.sh`** | Delete stale A records for a subdomain |
| **`scripts/vercel-dns-wildcard.sh`** | Upsert **`\*.{node}`** on **`relaygateway.net`** for repo vhosts |

Requires **`VERCEL_API_TOKEN`** (or Vercel CLI auth JSON). Optional **`VERCEL_TEAM_ID`**.

#!/usr/bin/env bash
# Linode fat+Nanode cutover checklist (ops gate). Does not provision VMs —
# requires a Linode PAT / CLI for create. Prints and optionally verifies DNS.
#
# Usage:
#   linode-cutover-checklist.sh [--check-dns] [--fat-fqdn pub1.relaygateway.net]
set -euo pipefail

FAT="${RELAY_PUBLISH_ORIGIN_FQDN:-pub1.relaygateway.net}"
FLEET="${RELAY_FLEET_DNS_NAME:-fleet.relaygateway.net}"
CHECK_DNS=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check-dns) CHECK_DNS=1; shift ;;
    --fat-fqdn) FAT="$2"; shift 2 ;;
    --fleet) FLEET="$2"; shift 2 ;;
    *) echo "unknown: $1" >&2; exit 2 ;;
  esac
done

cat <<EOF
Linode cutover checklist
========================
1. Provision fat (~8GB) + Nanode mesh; install relay-gateway.
2. point-dns each node FQDN; seed park-tls on fat.
3. Catalog publish_origin → fat FQDN ($FAT); never push to $FLEET.
4. Init + bake relay-ssr-cache on fat; mesh peers git-fetch only.
5. ops/fleet.json: publisher fleetDns=true; Nanodes role=mesh fleetDns=false.
6. RELAY_FLEET_DNS_PEERS=$FAT  (fat only)
7. relay-ops fleet-dns / vercel-dns-fleet-reconcile.sh → A set = fat IP only.
8. Confirm dig $FLEET has no Spot/Nanode IPs.
9. Parked ALIAS/CNAME stay on $FLEET.
10. Drain Spot from peer list + fleet A; decommission after mesh green.
11. SSL: ops/fleet.json health.sslDomains against fat.
EOF

if [[ "$CHECK_DNS" == "1" ]]; then
  echo
  echo "[check] dig $FLEET A:"
  dig +short "$FLEET" A || true
  echo "[check] dig $FAT A:"
  dig +short "$FAT" A || true
  fat_ips=$(dig +short "$FAT" A | sort -u | tr '\n' ' ')
  fleet_ips=$(dig +short "$FLEET" A | sort -u | tr '\n' ' ')
  echo "[check] fat IPs:   $fat_ips"
  echo "[check] fleet IPs: $fleet_ips"
  if [[ -n "$fat_ips" && "$fleet_ips" == "$fat_ips" ]]; then
    echo "[check] OK: fleet A matches fat only (single-IP compare may need manual review for multi-fat)."
  else
    echo "[check] WARN: fleet A set may still include non-fat peers — reconcile with RELAY_FLEET_DNS_PEERS."
  fi
fi

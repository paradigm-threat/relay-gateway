#!/usr/bin/env bash
# Upsert a Vercel DNS A record (used after Rackspace spot bids succeed).
# Usage: VERCEL_API_TOKEN=... ./vercel-dns-upsert.sh atlanta3.relaygateway.net 203.0.113.10
set -euo pipefail

FQDN="${1:?FQDN required}"
IP="${2:?IPv4 required}"
TTL="${RELAY_DNS_TTL:-60}"
TOKEN="${VERCEL_API_TOKEN:-${VERCEL_TOKEN:-}}"
[[ -n "$TOKEN" ]] || { echo "Set VERCEL_API_TOKEN" >&2; exit 2; }

DOMAIN="${FQDN#*.}"
NAME="${FQDN%%.*}"
[[ "$DOMAIN" != "$FQDN" ]] || { echo "FQDN must include a subdomain: $FQDN" >&2; exit 2; }

auth="Authorization: Bearer ${TOKEN}"
base="https://api.vercel.com"
team_q=""
list_q="?"
if [[ -n "${VERCEL_TEAM_ID:-}" ]]; then
  team_q="?teamId=${VERCEL_TEAM_ID}"
  list_q="?teamId=${VERCEL_TEAM_ID}&"
fi

list_url="${base}/v4/domains/${DOMAIN}/records${list_q}type=A"
list_raw=$(curl -fsS -H "$auth" "$list_url")
mapfile -t rec_ids < <(echo "$list_raw" | jq -r --arg n "$NAME" '.records[]? | select(.name==$n and .type=="A") | .id // .uid')

for rec_id in "${rec_ids[@]}"; do
  [[ -z "$rec_id" || "$rec_id" == "null" ]] && continue
  current=$(echo "$list_raw" | jq -r --arg id "$rec_id" '.records[]? | select((.id // .uid)==$id) | .value')
  if [[ "$current" == "$IP" ]]; then
    echo "${FQDN} already -> ${IP} (${rec_id})"
    exit 0
  fi
  del_url="${base}/v4/domains/${DOMAIN}/records/${rec_id}${team_q}"
  curl -fsS -X DELETE -H "$auth" "$del_url" >/dev/null
  echo "Deleted stale ${FQDN} A record ${rec_id} (${current})"
done

create_url="${base}/v4/domains/${DOMAIN}/records${team_q}"
body=$(jq -n --arg n "$NAME" --arg v "$IP" --argjson ttl "$TTL" '{ name: $n, value: $v, type: "A", ttl: $ttl }')
curl -fsS -X POST -H "$auth" -H 'Content-Type: application/json' -d "$body" "$create_url" >/dev/null
echo "Created ${FQDN} -> ${IP}"

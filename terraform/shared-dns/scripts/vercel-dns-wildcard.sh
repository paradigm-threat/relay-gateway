#!/usr/bin/env bash
# Upsert wildcard A record *.{node} on relaygateway.net (repo vhosts: repo.{node}.relaygateway.net).
# Usage: VERCEL_API_TOKEN=... ./vercel-dns-wildcard.sh sjc1 203.0.113.10
set -euo pipefail

NODE="${1:?node slug required (e.g. sjc1)}"
IP="${2:?IPv4 required}"
TTL="${RELAY_DNS_TTL:-60}"
TOKEN="${VERCEL_API_TOKEN:-${VERCEL_TOKEN:-}}"
[[ -n "$TOKEN" ]] || { echo "Set VERCEL_API_TOKEN" >&2; exit 2; }

DOMAIN="relaygateway.net"
NAME="*.${NODE}"
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
    echo "${NAME}.${DOMAIN} already -> ${IP}"
    exit 0
  fi
  curl -fsS -X DELETE -H "$auth" "${base}/v4/domains/${DOMAIN}/records/${rec_id}${team_q}" >/dev/null
  echo "Deleted stale ${NAME}.${DOMAIN} (${current})"
done

body=$(jq -n --arg n "$NAME" --arg v "$IP" --argjson ttl "$TTL" '{ name: $n, value: $v, type: "A", ttl: $ttl }')
curl -fsS -X POST -H "$auth" -H 'Content-Type: application/json' -d "$body" \
  "${base}/v4/domains/${DOMAIN}/records${team_q}" >/dev/null
echo "Created ${NAME}.${DOMAIN} -> ${IP}"

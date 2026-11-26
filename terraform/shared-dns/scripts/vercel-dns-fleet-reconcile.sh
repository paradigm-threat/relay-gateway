#!/usr/bin/env bash
# Reconcile multi-A/AAAA records for the Relay network ingress name from node FQDNs.
#
# Resolves each configured master hostname, then makes fleet.relaygateway.net's
# A/AAAA set match exactly those addresses (create missing, delete stale).
#
# Usage:
#   VERCEL_API_TOKEN=... ./vercel-dns-fleet-reconcile.sh
#   ./vercel-dns-fleet-reconcile.sh --dry-run
#   ./vercel-dns-fleet-reconcile.sh --fleet=fleet.relaygateway.net pub1.relaygateway.net
#
# Env:
#   VERCEL_API_TOKEN / VERCEL_TOKEN  required (unless --dry-run and no API calls needed — still required)
#   VERCEL_TEAM_ID                   optional
#   RELAY_DNS_TTL                    default 60
#   RELAY_OPERATOR_FLEET             semicolon-separated node FQDNs (preferred source when no args)
#   RELAY_MASTER_PEER_LIST           fallback source of node FQDNs
#   RELAY_FLEET_DNS_NAME             default fleet.relaygateway.net
set -euo pipefail

DRY_RUN=0
FLEET_FQDN="${RELAY_FLEET_DNS_NAME:-fleet.relaygateway.net}"
TTL="${RELAY_DNS_TTL:-60}"
TOKEN="${VERCEL_API_TOKEN:-${VERCEL_TOKEN:-}}"
NODES=()

usage() {
  cat <<'EOF'
Usage: vercel-dns-fleet-reconcile.sh [--dry-run] [--fleet=FQDN] [node.fqdn ...]

Reconcile A/AAAA records for the fleet ingress name from node hostnames.
With no node args, uses RELAY_OPERATOR_FLEET or RELAY_MASTER_PEER_LIST
(default: pub1.relaygateway.net only — fat publisher; do not pass Nanodes).
EOF
  exit "${1:-0}"
}

while [[ "${1:-}" == -* ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --fleet=*) FLEET_FQDN="${1#*=}" ;;
    --fleet) shift; FLEET_FQDN="${1:?}" ;;
    -h|--help) usage 0 ;;
    *) echo "unknown flag: $1" >&2; usage 2 ;;
  esac
  shift
done

while [[ $# -gt 0 ]]; do
  NODES+=("$1")
  shift
done

if [[ ${#NODES[@]} -eq 0 ]]; then
  # Default fleet A set = publisher only (never Nanode mesh IPs).
  raw="${RELAY_FLEET_DNS_PEERS:-${RELAY_OPERATOR_FLEET:-${RELAY_MASTER_PEER_LIST:-pub1.relaygateway.net}}}"
  IFS=';' read -ra NODES <<<"$raw"
fi

clean_nodes=()
for n in "${NODES[@]}"; do
  n="${n//[[:space:]]/}"
  [[ -n "$n" ]] || continue
  clean_nodes+=("$n")
done
NODES=("${clean_nodes[@]}")
[[ ${#NODES[@]} -gt 0 ]] || { echo "no node FQDNs configured" >&2; exit 2; }

[[ -n "$TOKEN" ]] || { echo "Set VERCEL_API_TOKEN (or VERCEL_TOKEN)" >&2; exit 2; }
command -v dig >/dev/null || { echo "dig is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }

# fleet.relaygateway.net → DOMAIN=relaygateway.net NAME=fleet
DOMAIN="${FLEET_FQDN#*.}"
NAME="${FLEET_FQDN%%.*}"
[[ "$DOMAIN" != "$FLEET_FQDN" && -n "$NAME" ]] || {
  echo "fleet FQDN must be a subdomain (got: $FLEET_FQDN)" >&2
  exit 2
}

auth="Authorization: Bearer ${TOKEN}"
base="https://api.vercel.com"
team_q=""
list_q="?"
if [[ -n "${VERCEL_TEAM_ID:-}" ]]; then
  team_q="?teamId=${VERCEL_TEAM_ID}"
  list_q="?teamId=${VERCEL_TEAM_ID}&"
fi

resolve_addrs() {
  local fqdn="$1" type="$2"
  # dig +short can return CNAME lines then addresses; keep only address-shaped answers.
  dig +short "$fqdn" "$type" 2>/dev/null | awk '
    $0 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ { print; next }
    $0 ~ /^[0-9a-fA-F:]+$/ && index($0, ":") { print; next }
  ' | sort -u
}

desired_a=()
desired_aaaa=()
echo "[fleet-dns] nodes: ${NODES[*]}"
echo "[fleet-dns] target: ${FLEET_FQDN} (name=${NAME} domain=${DOMAIN})"

for node in "${NODES[@]}"; do
  mapfile -t a_addrs < <(resolve_addrs "$node" A)
  mapfile -t aaaa_addrs < <(resolve_addrs "$node" AAAA)
  if [[ ${#a_addrs[@]} -eq 0 && ${#aaaa_addrs[@]} -eq 0 ]]; then
    echo "[fleet-dns] ERROR: no A/AAAA for $node" >&2
    exit 1
  fi
  for ip in "${a_addrs[@]}"; do
    desired_a+=("$ip")
    echo "[fleet-dns] resolve $node A -> $ip"
  done
  for ip in "${aaaa_addrs[@]}"; do
    desired_aaaa+=("$ip")
    echo "[fleet-dns] resolve $node AAAA -> $ip"
  done
done

# Unique desired sets
if [[ ${#desired_a[@]} -gt 0 ]]; then
  mapfile -t desired_a < <(printf '%s\n' "${desired_a[@]}" | sort -u)
fi
if [[ ${#desired_aaaa[@]} -gt 0 ]]; then
  mapfile -t desired_aaaa < <(printf '%s\n' "${desired_aaaa[@]}" | sort -u)
fi

list_records() {
  local rtype="$1"
  curl -fsS -H "$auth" "${base}/v4/domains/${DOMAIN}/records${list_q}type=${rtype}&limit=100"
}

in_list() {
  local needle="$1"
  shift
  local x
  for x in "$@"; do
    [[ "$x" == "$needle" ]] && return 0
  done
  return 1
}

reconcile_type() {
  local rtype="$1"
  shift
  local -a desired=("$@")
  local list_raw
  list_raw="$(list_records "$rtype")"

  local -a current_values=()
  local -a current_ids=()
  local -a current_map_values=()
  local -a current_map_ids=()

  while IFS=$'\t' read -r id value; do
    [[ -z "$id" || "$id" == "null" ]] && continue
    [[ -z "$value" || "$value" == "null" ]] && continue
    current_map_ids+=("$id")
    current_map_values+=("$value")
    current_ids+=("$id")
    current_values+=("$value")
  done < <(echo "$list_raw" | jq -r --arg n "$NAME" --arg t "$rtype" '
    .records[]?
    | select(.name == $n and .type == $t)
    | [(.id // .uid), .value]
    | @tsv
  ')

  echo "[fleet-dns] current ${NAME}.${DOMAIN} ${rtype}: ${current_values[*]:-<none>}"
  echo "[fleet-dns] desired ${NAME}.${DOMAIN} ${rtype}: ${desired[*]:-<none>}"

  # Delete stale
  local i
  for i in "${!current_map_values[@]}"; do
    local val="${current_map_values[$i]}"
    local id="${current_map_ids[$i]}"
    if ! in_list "$val" "${desired[@]+"${desired[@]}"}"; then
      if [[ "$DRY_RUN" == 1 ]]; then
        echo "[fleet-dns] DRY-RUN delete ${rtype} ${val} (${id})"
      else
        curl -fsS -X DELETE -H "$auth" \
          "${base}/v4/domains/${DOMAIN}/records/${id}${team_q}" >/dev/null
        echo "[fleet-dns] deleted stale ${rtype} ${val} (${id})"
      fi
    fi
  done

  # Create missing
  local ip
  for ip in "${desired[@]+"${desired[@]}"}"; do
    if ! in_list "$ip" "${current_values[@]+"${current_values[@]}"}"; then
      if [[ "$DRY_RUN" == 1 ]]; then
        echo "[fleet-dns] DRY-RUN create ${rtype} ${ip}"
      else
        local body
        body=$(jq -n --arg n "$NAME" --arg v "$ip" --arg t "$rtype" --argjson ttl "$TTL" \
          '{ name: $n, value: $v, type: $t, ttl: $ttl }')
        curl -fsS -X POST -H "$auth" -H 'Content-Type: application/json' \
          -d "$body" "${base}/v4/domains/${DOMAIN}/records${team_q}" >/dev/null
        echo "[fleet-dns] created ${rtype} ${NAME}.${DOMAIN} -> ${ip}"
      fi
    else
      echo "[fleet-dns] ok ${rtype} ${ip}"
    fi
  done
}

reconcile_type A "${desired_a[@]+"${desired_a[@]}"}"
reconcile_type AAAA "${desired_aaaa[@]+"${desired_aaaa[@]}"}"

echo "[fleet-dns] done ${FLEET_FQDN}"

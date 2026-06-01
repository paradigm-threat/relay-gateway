#!/usr/bin/env bash
# External data source: emit public_ip JSON for Terraform outputs.
set -euo pipefail
STATE="${SPOT_VM_STATE_FILE:-$(cd "$(dirname "$0")/.." && pwd)/vm-state.json}"
ip=""
if [[ -f "$STATE" ]]; then
  ip="$(jq -r '.public_ip // empty' "$STATE")"
fi
if [[ -z "$ip" ]]; then
  ip="$(dig +short dfw1.relaygateway.net 2>/dev/null | head -1 || true)"
fi
[[ -n "$ip" ]] || { echo '{"public_ip":""}' ; exit 0; }
jq -n --arg ip "$ip" '{public_ip:$ip}'

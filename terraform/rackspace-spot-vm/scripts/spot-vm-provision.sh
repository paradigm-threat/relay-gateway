#!/usr/bin/env bash
# Spot VM provision helper (Terraform local-exec). See PROVIDER_SPIKE.md.
set -euo pipefail

ACTION="${1:-apply}"
STATE="${SPOT_VM_STATE_FILE:?SPOT_VM_STATE_FILE required}"
NODE="${RELAY_NODE:-dfw1}"
OVERRIDE="${SPOT_PUBLIC_IP_OVERRIDE:-}"

resolve_ip() {
  if [[ -n "$OVERRIDE" ]]; then
    echo "$OVERRIDE"
    return 0
  fi
  if [[ -f "$STATE" ]]; then
    jq -r '.public_ip // empty' "$STATE" 2>/dev/null || true
    return 0
  fi
  dig +short "${NODE}.relaygateway.net" 2>/dev/null | head -1
}

case "$ACTION" in
  apply)
    ip="$(resolve_ip | head -1)"
    if [[ -z "$ip" ]]; then
      cat >&2 <<EOF
[spot-vm] No public IP. Options:
  1) Create VM Cloudspace in Spot UI, copy VM IP to terraform.tfvars: public_ip_override = "x.x.x.x"
  2) Set RXTSPOT_TOKEN when Spot VM API is available (see PROVIDER_SPIKE.md)
  3) Ensure DNS ${NODE}.relaygateway.net resolves after UI create
EOF
      exit 1
    fi
    mkdir -p "$(dirname "$STATE")"
    jq -n \
      --arg ip "$ip" \
      --arg node "$NODE" \
      --arg cs "${SPOT_CLOUDSPACE_NAME:-}" \
      --arg pool "${SPOT_VM_POOL_NAME:-}" \
      '{public_ip:$ip, relay_node:$node, cloudspace_name:$cs, vm_pool_name:$pool, provisioned_at:(now|todate)}' \
      >"$STATE"
    echo "[spot-vm] state $STATE -> $ip"
    ;;
  *)
    echo "Usage: $0 apply" >&2
    exit 2
    ;;
esac

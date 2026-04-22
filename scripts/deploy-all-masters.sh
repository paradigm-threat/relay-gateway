#!/usr/bin/env bash
# Build Linode tarball and run install.sh update on both US masters (no seed / ensure-bare).
# Deploys all production masters (US + JP), then restarts relay-gateway on each.
#
# Usage (from relay-gateway/):
#   ./scripts/deploy-all-masters.sh
#
# Env (optional):
#   RELAY_SSH_USER     default: ari
#   RELAY_SSH_IDENTITY default: ~/.ssh/id_ed25519_relay_gateway
#   RELAY_JP_OSA_SSH_HOST — SSH target for the JP master (default: jp-osa.relaygateway.net). If the apex
#     A record still points at a retired IP, set this to the live IPv4 (e.g. 172.234.85.232) until DNS is fixed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GATEWAY_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$GATEWAY_ROOT"

KEY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
SSH_USER="${RELAY_SSH_USER:-ari}"
JP_OSA="${RELAY_JP_OSA_SSH_HOST:-jp-osa.relaygateway.net}"
MASTERS=(atlanta2.relaygateway.net atlanta3.relaygateway.net "$JP_OSA")

[[ -f "$KEY" ]] || { echo "Missing SSH key: $KEY" >&2; exit 1; }

"$SCRIPT_DIR/pack-linode-deploy.sh"

export RELAY_REDEPLOY_SKIP_SEED=1
for NODE in "${MASTERS[@]}"; do
  echo "[deploy-all-masters] --- $NODE ---"
  "$SCRIPT_DIR/relay-linode-redeploy-seed.sh" "$NODE"
done

SSH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
for NODE in "${MASTERS[@]}"; do
  echo "[deploy-all-masters] restart relay-gateway on $NODE"
  "${SSH[@]}" "${SSH_USER}@${NODE}" 'sudo systemctl restart relay-gateway'
done

echo "[deploy-all-masters] Done."

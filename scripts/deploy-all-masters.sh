#!/usr/bin/env bash
# Build Linode tarball and run install.sh update on both US masters (no seed / ensure-bare).
# Strips retired jp-osa.relaygateway.net from /opt/relay/relay.env if present, then restarts relay-gateway.
#
# Usage (from relay-gateway/):
#   ./scripts/deploy-all-masters.sh
#
# Env (optional):
#   RELAY_SSH_USER     default: ari
#   RELAY_SSH_IDENTITY default: ~/.ssh/id_ed25519_relay_gateway
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GATEWAY_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$GATEWAY_ROOT"

KEY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
SSH_USER="${RELAY_SSH_USER:-ari}"
MASTERS=(atlanta2.relaygateway.net atlanta3.relaygateway.net)

[[ -f "$KEY" ]] || { echo "Missing SSH key: $KEY" >&2; exit 1; }

"$SCRIPT_DIR/pack-linode-deploy.sh"

export RELAY_REDEPLOY_SKIP_SEED=1
for NODE in "${MASTERS[@]}"; do
  echo "[deploy-all-masters] --- $NODE ---"
  "$SCRIPT_DIR/relay-linode-redeploy-seed.sh" "$NODE"
done

SSH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
for NODE in "${MASTERS[@]}"; do
  echo "[deploy-all-masters] relay.env peer cleanup + restart on $NODE"
  "${SSH[@]}" "${SSH_USER}@${NODE}" bash -s <<'REMOTE'
set -euo pipefail
if [[ -f /opt/relay/relay.env ]]; then
  sudo sed -i 's/;jp-osa\.relaygateway\.net//g' /opt/relay/relay.env
fi
sudo systemctl restart relay-gateway
REMOTE
done

echo "[deploy-all-masters] Done."

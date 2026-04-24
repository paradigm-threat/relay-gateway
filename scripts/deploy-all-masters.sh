#!/usr/bin/env bash
# Wrapper around relay-fleet.sh (canonical operator path). Prefer: ./scripts/relay-fleet.sh deploy-all
#
# Usage (from relay-gateway/):
#   ./scripts/deploy-all-masters.sh
#   ./scripts/deploy-all-masters.sh   # same as relay-fleet deploy-all (uses ./relay.env for fleet)
#
# Optional env (legacy JP SSH override when apex DNS is wrong):
#   RELAY_JP_OSA_SSH_HOST — last fleet member becomes this host for this run only (default: jp-osa.relaygateway.net)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GATEWAY_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$GATEWAY_ROOT"

JP_OSA="${RELAY_JP_OSA_SSH_HOST:-jp-osa.relaygateway.net}"
export RELAY_OPERATOR_FLEET="atlanta2.relaygateway.net;atlanta3.relaygateway.net;${JP_OSA}"

# Match legacy behavior: always rebuild tarball before upload (use relay-fleet.sh deploy-all alone to skip pack).
exec "$SCRIPT_DIR/relay-fleet.sh" --build deploy-all "$@"

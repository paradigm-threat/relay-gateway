#!/usr/bin/env bash
# Wrapper around relay-fleet.sh (canonical operator path). Prefer: ./scripts/relay-fleet.sh deploy-all
#
# Usage (from relay-gateway/):
#   ./scripts/deploy-all-masters.sh
#   ./scripts/deploy-all-masters.sh   # same as relay-fleet deploy-all (uses ./relay.env for fleet)
#
# Fleet defaults to Linode mesh (pub1/mesh1/mesh2). Override with RELAY_OPERATOR_FLEET or ./relay.env.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GATEWAY_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$GATEWAY_ROOT"

export RELAY_OPERATOR_FLEET="${RELAY_OPERATOR_FLEET:-pub1.relaygateway.net;mesh1.relaygateway.net;mesh2.relaygateway.net}"

# Match legacy behavior: always rebuild tarball before upload (use relay-fleet.sh deploy-all alone to skip pack).
exec "$SCRIPT_DIR/relay-fleet.sh" --build deploy-all "$@"

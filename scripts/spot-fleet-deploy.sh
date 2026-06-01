#!/usr/bin/env bash
# Deploy bare relay on all Spot VM masters (dfw1, lon1, hkg1). No ord1 / no Linode.
#
# Usage (from relay-gateway/):
#   ./scripts/spot-fleet-deploy.sh
#   ./scripts/spot-fleet-deploy.sh --build
#   SPOT_LON1_IP=x SPOT_HKG1_IP=y ./scripts/spot-fleet-deploy.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export SPOT_DFW1_IP="${SPOT_DFW1_IP:-174.143.201.10}"
export SPOT_LON1_IP="${SPOT_LON1_IP:-134.213.138.234}"
export SPOT_HKG1_IP="${SPOT_HKG1_IP:-203.60.2.11}"

BUILD=0
[[ "${1:-}" == --build ]] && BUILD=1

[[ "$BUILD" == 1 ]] && ./scripts/pack-linode-deploy.sh

DEPLOY=(./scripts/relay-master-deploy.sh --install-only)

fail=0
for target in spot-dfw1 spot-lon1 spot-hkg1; do
  echo "========== $target =========="
  if ! "${DEPLOY[@]}" --target "$target"; then
    fail=1
  fi
  echo ""
done

echo "========== validate =========="
for fqdn in dfw1.relaygateway.net lon1.relaygateway.net hkg1.relaygateway.net; do
  if ! ./scripts/relay-master-validate.sh --fqdn "$fqdn"; then
    fail=1
  fi
done

if [[ "$fail" -eq 0 ]]; then
  echo "=== Spot fleet deploy + validate OK ==="
  exit 0
fi
echo "=== Spot fleet: some steps failed ===" >&2
exit 1

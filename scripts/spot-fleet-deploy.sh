#!/usr/bin/env bash
# Deploy bare relay on all Spot VM masters (iad1, sjc1, hkg1). No DFW / no Linode.
#
# Usage (from relay-gateway/):
#   ./scripts/spot-fleet-deploy.sh
#   ./scripts/spot-fleet-deploy.sh --build
#   SPOT_SJC1_IP=x SPOT_IAD1_IP=y SPOT_HKG1_IP=z ./scripts/spot-fleet-deploy.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export SPOT_IAD1_IP="${SPOT_IAD1_IP:-104.239.250.44}"
export SPOT_SJC1_IP="${SPOT_SJC1_IP:-}"
export SPOT_HKG1_IP="${SPOT_HKG1_IP:-203.60.1.107}"

BUILD=0
[[ "${1:-}" == --build ]] && BUILD=1

[[ "$BUILD" == 1 ]] && ./scripts/pack-linode-deploy.sh

DEPLOY=(./scripts/relay-master-deploy.sh --install-only)

fail=0
for target in spot-iad1 spot-sjc1 spot-hkg1; do
  echo "========== $target =========="
  if ! "${DEPLOY[@]}" --target "$target"; then
    fail=1
  fi
  echo ""
done

echo "========== validate =========="
for fqdn in iad1.relaygateway.net sjc1.relaygateway.net hkg1.relaygateway.net; do
  if ! ./scripts/relay-master-validate.sh --fqdn "$fqdn"; then
    fail=1
  fi
done

if [[ "$fail" -eq 0 ]]; then
  echo "spot-fleet-deploy: OK"
  exit 0
fi
echo "spot-fleet-deploy: FAILED" >&2
exit 1

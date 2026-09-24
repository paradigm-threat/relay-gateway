#!/usr/bin/env bash
# Smoke test iad1 + ord1 + hkg1.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail=0
for node in iad1 ord1 hkg1; do
  if ! "$ROOT/tests/${node}.relaygateway.net.sh" 2>/dev/null; then
    if ! "$ROOT/scripts/relay-master-validate.sh" --fqdn "${node}.relaygateway.net"; then
      fail=1
    fi
  fi
done
exit "$fail"

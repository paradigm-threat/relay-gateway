#!/usr/bin/env bash
# Smoke test dfw1 + lon1 + hkg1 (no ord1).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail=0
for node in dfw1 lon1 hkg1; do
  if ! "$ROOT/tests/${node}.relaygateway.net.sh" 2>/dev/null; then
    if ! "$ROOT/scripts/relay-master-validate.sh" --fqdn "${node}.relaygateway.net"; then
      fail=1
    fi
  fi
done
exit "$fail"

#!/usr/bin/env bash
# Retired: sjc1 moved to ord1. See tests/ord1.relaygateway.net.sh
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ord1.relaygateway.net.sh" "$@"

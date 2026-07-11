#!/usr/bin/env bash
# Smoke tests for lon1 Spot VM master (bare relay + Caddy).
#
# Usage (from relay-gateway/):
#   ./tests/lon1.relaygateway.net.sh
#   RELAY_VALIDATE_CURL_TIMEOUT=60 ./tests/lon1.relaygateway.net.sh
#
# Requires: curl, jq, nc (openbsd-netcat)
set -euo pipefail

FQDN="lon1.relaygateway.net"
NODE="lon1"
MIRROR="${RELAY_TEST_MIRROR:-paradigm-threat-files}"
MIRROR_HOST="${MIRROR}.${NODE}.relaygateway.net"
TIMEOUT="${RELAY_VALIDATE_CURL_TIMEOUT:-25}"
IP_EXPECT="${SPOT_LON1_IP:-134.213.138.182}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail=0

ok() { echo "OK  $*"; }
bad() { echo "FAIL $*" >&2; fail=1; }
warn() { echo "WARN $*" >&2; }

echo "=== lon1 relay smoke test ==="
echo "apex:   https://${FQDN}/"
echo "mirror: https://${MIRROR_HOST}/"
echo "git:    git://${FQDN}:9418/${MIRROR}.git"
echo ""

# DNS
dns="$(dig +short "$FQDN" A 2>/dev/null | head -1 || true)"
if [[ "$dns" == "$IP_EXPECT" ]]; then
  ok "DNS ${FQDN} -> ${dns}"
elif [[ -n "$dns" ]]; then
  warn "DNS ${FQDN} -> ${dns} (expected ${IP_EXPECT})"
else
  bad "DNS ${FQDN} (no A record)"
fi

# HTTPS mirror
if curl -fsSI --connect-timeout "$TIMEOUT" "https://${MIRROR_HOST}/" >/dev/null 2>&1; then
  ok "HTTPS https://${MIRROR_HOST}/"
else
  bad "HTTPS https://${MIRROR_HOST}/"
fi

# HTTPS apex
if curl -fsSI --connect-timeout "$TIMEOUT" "https://${FQDN}/" >/dev/null 2>&1; then
  ok "HTTPS https://${FQDN}/"
else
  warn "HTTPS apex https://${FQDN}/ (mirror host may still be authoritative)"
fi

# API status
if curl -fsS --connect-timeout "$TIMEOUT" -X POST "https://${MIRROR_HOST}/api/status" -d '{}' \
  | jq -e '.self.relay_server_id // .relay_server_id // .server_id' >/dev/null 2>&1; then
  ok "POST https://${MIRROR_HOST}/api/status"
  curl -fsS --connect-timeout "$TIMEOUT" -X POST "https://${MIRROR_HOST}/api/status" -d '{}' \
    | jq '{relay_server_id: (.self.relay_server_id // .relay_server_id), hostname: .self.public_hostname}'
else
  bad "POST https://${MIRROR_HOST}/api/status"
fi

# API config
if curl -fsS --connect-timeout "$TIMEOUT" -X POST "https://${MIRROR_HOST}/api/config" -d '{}' \
  | jq -e '.relay_server_id // .authorized_repos' >/dev/null 2>&1; then
  ok "POST https://${MIRROR_HOST}/api/config"
else
  bad "POST https://${MIRROR_HOST}/api/config"
fi

# git daemon
if timeout 3 bash -c "echo | nc -w 2 ${FQDN} 9418" >/dev/null 2>&1; then
  ok "TCP ${FQDN}:9418 (git)"
else
  bad "TCP ${FQDN}:9418 (git)"
fi

# WebP / static mirror sample
sample="https://${MIRROR_HOST}/influence/conspiracy/embedding.jpg?w=160"
ct="$(curl -fsSI --connect-timeout "$TIMEOUT" "$sample" 2>/dev/null | awk 'tolower($1)=="content-type:"{print $2}' | tr -d '\r' || true)"
if [[ "$ct" == image/webp* ]]; then
  ok "mirror WebP ${sample}"
elif [[ "$ct" == image/* ]]; then
  ok "mirror image (${ct}) ${sample}"
else
  bad "mirror ${sample} (content-type: ${ct:-none})"
fi

# Optional: delegate to canonical validator
if [[ -x "${ROOT}/scripts/relay-master-validate.sh" ]]; then
  echo ""
  echo "=== relay-master-validate.sh ==="
  "${ROOT}/scripts/relay-master-validate.sh" --fqdn "$FQDN" --mirror "$MIRROR" || fail=1
fi

echo ""
if [[ "$fail" -eq 0 ]]; then
  echo "=== All lon1 checks passed ==="
  exit 0
fi
echo "=== Some lon1 checks failed ===" >&2
exit 1

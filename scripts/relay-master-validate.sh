#!/usr/bin/env bash
# Validate relay master HTTPS, API, git, and optional mirror URLs.
#
# Usage:
#   ./scripts/relay-master-validate.sh --fqdn dfw1.relaygateway.net
#   ./scripts/relay-master-validate.sh --fqdn dfw1.relaygateway.net --mirror paradigm-threat-files
set -euo pipefail

FQDN=""
MIRROR=""
CURL_TIMEOUT="${RELAY_VALIDATE_CURL_TIMEOUT:-25}"

while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --fqdn=*) FQDN="${1#*=}" ;;
    --fqdn) shift; FQDN="${1:?}" ;;
    --mirror=*) MIRROR="${1#*=}" ;;
    --mirror) shift; MIRROR="${1:?}" ;;
    -h|--help)
      echo "Usage: $0 --fqdn <node.relaygateway.net> [--mirror <repo-name>]"
      exit 0
      ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
  shift
done

[[ -n "$FQDN" ]] || { echo "Missing --fqdn" >&2; exit 1; }

NODE="${FQDN%%.*}"
fail=0
ok() { echo "OK  $*"; }
bad() { echo "FAIL $*" >&2; fail=1; }

echo "=== Validate $FQDN (node=$NODE) ==="

if curl -fsSI --connect-timeout "$CURL_TIMEOUT" "https://${FQDN}/" >/dev/null 2>&1; then
  ok "HTTPS apex"
else
  bad "HTTPS apex https://${FQDN}/"
fi

if curl -fsS --connect-timeout "$CURL_TIMEOUT" -X POST "https://${FQDN}/api/status" -d '{}' | jq -e '.relay_server_id // .server_id // .hostname' >/dev/null 2>&1; then
  ok "POST /api/status"
else
  bad "POST /api/status"
fi

if curl -fsS --connect-timeout "$CURL_TIMEOUT" -X POST "https://${FQDN}/api/config" -d '{}' | jq -e '.relay_server_id // .authorized_repos' >/dev/null 2>&1; then
  ok "POST /api/config"
else
  bad "POST /api/config"
fi

# git:// probe via TCP (9418)
if timeout 3 bash -c "echo | nc -w 2 ${FQDN} 9418" >/dev/null 2>&1; then
  ok "git port 9418"
else
  bad "git port 9418 on ${FQDN}"
fi

MIRROR="${MIRROR:-paradigm-threat-files}"
mirror_url="https://${MIRROR}.${NODE}.relaygateway.net/influence/conspiracy/embedding.jpg?w=160"
ct=$(curl -fsSI --connect-timeout "$CURL_TIMEOUT" "$mirror_url" 2>/dev/null | awk 'tolower($1)=="content-type:"{print $2}' | tr -d '\r' || true)
if [[ "$ct" == image/webp* ]]; then
  ok "mirror WebP ${mirror_url}"
elif [[ "$ct" == image/* ]]; then
  ok "mirror image (${ct}) ${mirror_url}"
else
  bad "mirror ${mirror_url} (content-type: ${ct:-none})"
fi

if [[ "$fail" == 0 ]]; then
  echo "=== All checks passed ==="
  exit 0
fi
echo "=== Some checks failed ==="
exit 1

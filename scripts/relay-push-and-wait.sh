#!/usr/bin/env bash
# Push to Relay publish origin, then poll /api/sync/mesh until fleet matches or timeout.
#
# Usage:
#   relay-push-and-wait.sh <git-push-args...>
#   relay-push-and-wait.sh iad1 HEAD:main
#   relay-push-and-wait.sh gitlab iad1 HEAD:main   # only waits after last remote
#
# Env:
#   RELAY_PUBLISH_ORIGIN   default iad1.relaygateway.net
#   RELAY_MESH_REPO        inferred from remote URL path (paradigm-threat-files)
#   RELAY_MESH_BRANCH      default main
#   RELAY_MESH_POLL_SECS   default 2
#   RELAY_MESH_TIMEOUT_SECS default 90

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <git push remote> [refspec...]" >&2
  exit 2
fi

PUBLISH_ORIGIN="${RELAY_PUBLISH_ORIGIN:-iad1.relaygateway.net}"
POLL="${RELAY_MESH_POLL_SECS:-2}"
TIMEOUT="${RELAY_MESH_TIMEOUT_SECS:-90}"
BRANCH="${RELAY_MESH_BRANCH:-main}"

REMOTE="$1"
shift

echo "==> git push $REMOTE $*"
git push "$REMOTE" "$@"
PUSHED_SHA="$(git rev-parse HEAD)"

# Infer repo slug from git remote URL (…/paradigm-threat-files.git)
REPO="${RELAY_MESH_REPO:-}"
if [[ -z "$REPO" ]]; then
  url="$(git remote get-url "$REMOTE" 2>/dev/null || true)"
  base="$(basename "$url" .git)"
  REPO="${base}"
fi

API="https://${PUBLISH_ORIGIN}/api/sync/mesh/${REPO}?branch=${BRANCH}&sha=${PUSHED_SHA}"
deadline=$((SECONDS + TIMEOUT))

echo "==> polling mesh sync for ${REPO}@${BRANCH} (${PUSHED_SHA:0:8}) on ${PUBLISH_ORIGIN}"

while (( SECONDS < deadline )); do
  resp="$(curl -fsS -X POST "$API" 2>/dev/null || true)"
  status="$(printf '%s' "$resp" | python3 -c "import json,sys
try:
 d=json.load(sys.stdin)
 print(d.get('mesh_sync',{}).get('status',''))
except Exception:
 print('')" 2>/dev/null || true)"

  case "$status" in
    success)
      echo "mesh sync: success"
      printf '%s\n' "$resp" | python3 -m json.tool 2>/dev/null || printf '%s\n' "$resp"
      exit 0
      ;;
    fail)
      echo "mesh sync: fail" >&2
      printf '%s\n' "$resp" | python3 -m json.tool 2>/dev/null || printf '%s\n' "$resp"
      exit 1
      ;;
    partial)
      echo "mesh sync: partial (still polling…)"
      ;;
    pending|*)
      echo "mesh sync: pending…"
      ;;
  esac
  sleep "$POLL"
done

echo "mesh sync: timeout after ${TIMEOUT}s (last status=${status:-unknown})" >&2
curl -fsS -X POST "$API" | python3 -m json.tool 2>/dev/null || true
exit 2

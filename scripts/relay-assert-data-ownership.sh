#!/usr/bin/env bash
# Assert (or fix) that every bare repo under RELAY_REPO_PATH is owned by relay.
# Gateway, git-daemon, and git-proxy run as User=relay; ubuntu-owned trees cause
# "dubious ownership" on index warm and peer fetch.
#
# Usage:
#   RELAY_REPO_PATH=/opt/relay/data ./scripts/relay-assert-data-ownership.sh
#   RELAY_REPO_PATH=/opt/relay/data ./scripts/relay-assert-data-ownership.sh --fix
set -euo pipefail

DATA="${RELAY_REPO_PATH:-/opt/relay/data}"
FIX=0
for arg in "$@"; do
  case "$arg" in
    --fix) FIX=1 ;;
    -h|--help)
      sed -n '2,12p' "$0"
      exit 0
      ;;
  esac
done

if [[ ! -d "$DATA" ]]; then
  echo "missing data root: $DATA" >&2
  exit 1
fi

bad=0
shopt -s nullglob
for bare in "$DATA"/*.git; do
  [[ -d "$bare" ]] || continue
  name=$(basename "$bare" .git)
  owner=$(stat -c %U "$bare" 2>/dev/null || echo unknown)
  stray=$(find "$bare" -not -user relay -print -quit 2>/dev/null || true)
  if [[ "$owner" != "relay" || -n "$stray" ]]; then
    echo "BAD $name owner=$owner${stray:+ (stray files)}"
    bad=$((bad + 1))
    if [[ "$FIX" -eq 1 ]]; then
      if [[ "$(id -u)" -eq 0 ]]; then
        chown -R relay:relay "$bare"
        chmod -R u+rwX,g+rX,o+rX "$bare"
      else
        sudo chown -R relay:relay "$bare"
        sudo chmod -R u+rwX,g+rX,o+rX "$bare"
      fi
      echo "fixed $name -> relay:relay"
    fi
  else
    echo "ok $name"
  fi
done

if [[ "$bad" -gt 0 && "$FIX" -eq 0 ]]; then
  echo "$bad bare repo(s) not owned by relay (re-run with --fix)" >&2
  exit 2
fi
exit 0

#!/usr/bin/env bash
# Verify a thin bare repo: no large blobs, all binary OIDs are IPFS pointers, CIDs pinned locally.
set -euo pipefail

BARE="${1:?usage: relay-ipfs-verify-thin.sh /path/to/thin.git [oid-stubs.json]}"
STUBS="${2:-}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if ! ipfs id >/dev/null 2>&1; then
  echo "ipfs daemon not running" >&2
  exit 1
fi

echo "==> verify blobs in $BARE (all refs)"
python3 "$SCRIPT_DIR/relay-ipfs-verify-thin.py" "$BARE" ${STUBS:+"$STUBS"}

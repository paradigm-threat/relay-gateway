#!/usr/bin/env bash
# Pre-deploy gate: unit tests, release binaries, deploy tarball.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== relay-gateway pre-deploy check =="

echo ">> cargo test"
cargo test

echo ">> cargo build --release (relay-gateway + relay-hook-handler)"
cargo build --release --bin relay-gateway --bin relay-hook-handler

echo ">> pack linode deploy tarball"
./scripts/pack-linode-deploy.sh

TARBALL="$ROOT/target/relay-linode-deploy.tgz"
if [[ ! -f "$TARBALL" ]]; then
  echo "missing $TARBALL" >&2
  exit 1
fi

echo ">> verify tarball contents"
TAR_LIST="$(tar tzf "$TARBALL")"
for bin in relay-gateway relay-hook-handler install.sh; do
  if ! grep -qE "(^|/)${bin}$" <<< "$TAR_LIST"; then
    echo "tarball missing $bin" >&2
    exit 1
  fi
done

echo ">> binary versions"
tar xOf "$TARBALL" ./relay-gateway 2>/dev/null | strings | grep -m1 'relay-gateway' || true
ls -la "$TARBALL"

echo ""
echo "OK — deploy with:"
echo "  cd $ROOT && ./scripts/relay-fleet.sh --build deploy-all"
echo "Post-deploy smoke:"
echo "  ./scripts/relay-fleet.sh health"
echo "  ./scripts/relay-fleet.sh drift"

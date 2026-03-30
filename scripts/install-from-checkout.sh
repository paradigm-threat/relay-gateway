#!/usr/bin/env bash
# Install relay from a git checkout using prebuilt binaries under dist/<platform>/.
# Run as root from the relay-server repository root:
#   sudo ./scripts/install-from-checkout.sh install
#
# Build binaries on a machine matching the server CPU, then:
#   mkdir -p dist/linux-amd64 && cp target/release/relay-server target/release/relay-hook-handler dist/linux-amd64/
# See docs/DEPLOY_SELF_HOSTED.md
set -euo pipefail

[[ "$(id -u)" -eq 0 ]] || { echo "run as root: sudo $0 $*" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) SUB=linux-amd64 ;;
  aarch64) SUB=linux-arm64 ;;
  *) echo "unsupported uname -m: $ARCH (add mapping in $0)" >&2; exit 1 ;;
esac

BIN="$ROOT/dist/$SUB"
if [[ ! -f "$BIN/relay-server" || ! -f "$BIN/relay-hook-handler" ]]; then
  echo "Missing prebuilt binaries in $BIN/" >&2
  echo "On a dev machine with the same architecture (or cross-compile), run:" >&2
  echo "  ./scripts/pack-linode-deploy.sh" >&2
  echo "  mkdir -p $BIN && cp target/release/relay-server target/release/relay-hook-handler $BIN/" >&2
  echo "Then copy this repo to the server (including dist/) and re-run." >&2
  exit 1
fi

export RELAY_BIN_SOURCE="$BIN"
exec bash "$ROOT/scripts/relay-install.sh" "${@:-install}"

#!/usr/bin/env bash
# Thin wrapper: fleet CLI lives in `relay-ops` (same binary as the local UI).
# Run from relay-gateway/:  ./scripts/relay-fleet.sh <subcommand> [args]
# Docs: docs/RELAY_OPS.md
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN=""
if [[ -n "${RELAY_OPS_BIN:-}" && -x "${RELAY_OPS_BIN}" ]]; then
  BIN="$RELAY_OPS_BIN"
elif [[ -x "$ROOT/target/release/relay-ops" ]]; then
  BIN="$ROOT/target/release/relay-ops"
elif [[ -x "$ROOT/target/debug/relay-ops" ]]; then
  BIN="$ROOT/target/debug/relay-ops"
fi

if [[ -z "$BIN" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "[relay-fleet] ERROR: relay-ops binary not found and cargo is not on PATH. Build with: cargo build --release --bin relay-ops" >&2
    exit 1
  fi
  exec cargo run --quiet --bin relay-ops -- "$@"
fi

exec "$BIN" "$@"

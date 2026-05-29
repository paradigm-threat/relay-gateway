#!/usr/bin/env bash
# Clone path deps for CI/local Docker; fall back to crates.io in CI when clones fail.
set -euo pipefail

GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$GW_DIR"

if ! ./scripts/ci-clone-path-deps.sh; then
  echo "[ci-ensure-deps] clone script failed" >&2
fi

PARENT="$(cd "$GW_DIR/.." && pwd)"
GRAND="$(cd "$GW_DIR/../.." && pwd)"
if [[ -f "$PARENT/relay-runtime/Cargo.toml" ]]; then
  DEPS_ROOT="$PARENT"
elif [[ -f "$GRAND/relay-runtime/Cargo.toml" ]]; then
  DEPS_ROOT="$GRAND"
else
  DEPS_ROOT="$GRAND"
fi

have_runtime=false
have_md2ast=false
[[ -f "$DEPS_ROOT/relay-runtime/Cargo.toml" ]] && have_runtime=true
[[ -f "$DEPS_ROOT/md2ast/Cargo.toml" ]] && have_md2ast=true

if $have_runtime && $have_md2ast; then
  echo "[ci-ensure-deps] path deps ready under $DEPS_ROOT"
  exit 0
fi

if [[ -z "${CI:-}" ]]; then
  echo "[ci-ensure-deps] missing path deps (relay-runtime=$have_runtime md2ast=$have_md2ast)" >&2
  exit 1
fi

echo "[ci-ensure-deps] WARNING: using crates.io for missing path deps (relay-runtime=$have_runtime md2ast=$have_md2ast)"
if ! $have_runtime; then
  sed -i 's|^relay-runtime = .*|relay-runtime = "0.1.1"|' Cargo.toml
fi
if ! $have_md2ast; then
  sed -i 's|^md2ast = .*|md2ast = "1.0.2"|' Cargo.toml
fi
touch .ci-registry-deps
export PATH="${PATH:-/usr/local/cargo/bin}:$HOME/.cargo/bin"
cargo update -q -p relay-runtime -p md2ast 2>/dev/null || cargo update -q
echo "[ci-ensure-deps] updated lockfile for registry deps"
if ! $have_runtime; then
  echo "[ci-ensure-deps] relay-runtime clone missing — registry fallback for tests; Docker job needs clone" >&2
  echo "[ci-ensure-deps] GitLab: clevertree/relay-runtime → Settings → CI/CD → Job token permissions → add relay-server" >&2
fi

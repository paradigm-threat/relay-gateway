#!/usr/bin/env bash
# Resolve relay-gateway dependencies for CI/local builds.
set -euo pipefail

GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$GW_DIR"

use_registry() {
  echo "[ci-ensure-deps] using crates.io (relay-runtime 0.1.1, md2ast 1.0.2)"
  sed -i 's|^relay-runtime = .*|relay-runtime = "0.1.1"|' Cargo.toml
  sed -i 's|^md2ast = .*|md2ast = "1.0.2"|' Cargo.toml
  touch .ci-registry-deps
  export PATH="${PATH:-/usr/local/cargo/bin}:$HOME/.cargo/bin"
  cargo update -q -p relay-runtime -p md2ast 2>/dev/null || cargo update -q
}

# CI test/package jobs: registry deps only (no private clone required).
if [[ -n "${CI:-}" && "${RELAY_CI_PATH_DEPS:-}" != "1" ]]; then
  use_registry
  exit 0
fi

# Docker / local / explicit path-dep mode.
if ! ./scripts/ci-clone-path-deps.sh; then
  echo "[ci-ensure-deps] clone script reported errors" >&2
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

if [[ -n "${CI:-}" ]]; then
  echo "[ci-ensure-deps] path deps missing (relay-runtime=$have_runtime md2ast=$have_md2ast)" >&2
  echo "[ci-ensure-deps] For Docker builds set GitLab CI variable RELAY_RUNTIME_GIT_URL to a deploy-token URL, e.g.:" >&2
  echo "  https://gitlab+deploy-token-NNN:TOKEN@gitlab.com/clevertree/relay-runtime.git" >&2
  echo "  Or: relay-runtime → Settings → CI/CD → Job token permissions → allow clevertree/relay-server" >&2
  exit 1
fi

echo "[ci-ensure-deps] missing path deps (relay-runtime=$have_runtime md2ast=$have_md2ast)" >&2
exit 1

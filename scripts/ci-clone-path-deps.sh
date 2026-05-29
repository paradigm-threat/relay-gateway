#!/usr/bin/env bash
# Clone relay-runtime + md2ast for relay-gateway builds (monorepo or standalone checkout).
set -euo pipefail

GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PARENT="$(cd "$GW_DIR/.." && pwd)"
GRAND="$(cd "$GW_DIR/../.." && pwd)"
if [[ -n "${CI_PROJECT_DIR:-}" ]]; then
  DEPS_ROOT="${CI_PROJECT_DIR}/deps"
  mkdir -p "$DEPS_ROOT"
elif [[ -f "$PARENT/relay-runtime/Cargo.toml" ]]; then
  DEPS_ROOT="$PARENT"
elif [[ -f "$GRAND/relay-runtime/Cargo.toml" ]]; then
  DEPS_ROOT="$GRAND"
else
  DEPS_ROOT="$GRAND"
fi

RUNTIME_DIR="$DEPS_ROOT/relay-runtime"
MD2AST_DIR="$DEPS_ROOT/md2ast"
REF="${RELAY_PATH_DEPS_REF:-main}"

git_dep_url() {
  local default="$1"
  if [[ -n "${CI_JOB_TOKEN:-}" && "$default" == https://gitlab.com/* ]]; then
    local path="${default#https://gitlab.com/}"
    echo "https://gitlab-ci-token:${CI_JOB_TOKEN}@gitlab.com/${path}"
  else
    echo "$default"
  fi
}

RUNTIME_URL="$(git_dep_url "${RELAY_RUNTIME_GIT_URL:-https://gitlab.com/clevertree/relay-runtime.git}")"
MD2AST_URL="$(git_dep_url "${MD2AST_GIT_URL:-https://gitlab.com/clevertree/md2jsx.git}")"

clone_if_missing() {
  local url="$1" dest="$2"
  if [[ -f "$dest/Cargo.toml" ]]; then
    echo "[ci-clone-path-deps] ok: $dest"
    return 0
  fi
  echo "[ci-clone-path-deps] clone $url -> $dest (ref $REF)"
  if ! git clone --depth 1 --branch "$REF" "$url" "$dest" 2>/dev/null; then
    if ! git clone --depth 1 "$url" "$dest"; then
      echo "[ci-clone-path-deps] FAILED: $url (private repo? enable CI job token from relay-server on target project)" >&2
      return 1
    fi
  fi
}

clone_if_missing "$RUNTIME_URL" "$RUNTIME_DIR" || true
clone_if_missing "$MD2AST_URL" "$MD2AST_DIR" || true

TS_SRC="$GW_DIR/themed-styler"
TS_DST="$DEPS_ROOT/relay/relay-gateway/themed-styler"
if [[ -f "$TS_SRC/Cargo.toml" ]]; then
  mkdir -p "$(dirname "$TS_DST")"
  if [[ ! -f "$TS_DST/Cargo.toml" ]]; then
    if ln -sfn "$TS_SRC" "$TS_DST" 2>/dev/null; then
      :
    else
      mkdir -p "$TS_DST"
      cp -a "$TS_SRC/." "$TS_DST/"
    fi
  fi
fi

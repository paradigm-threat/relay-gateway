#!/usr/bin/env bash
# Stage a flat Docker build context: relay-gateway/, relay-runtime/, md2ast/, themed-styler path.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
if [[ -n "${CI_PROJECT_DIR:-}" ]]; then
  DEPS_ROOT="${CI_PROJECT_DIR}/deps"
else
  DEPS_ROOT="$(cd "$GW_DIR/../.." && pwd)"
fi
OUT="${1:?output directory}"

if [[ "${RELAY_DOCKER_REGISTRY_DEPS:-0}" == "1" ]]; then
  echo "[stage-docker-context] registry deps mode (no path-dep clones)"
else
  "$SCRIPT_DIR/ci-clone-path-deps.sh"
fi

rm -rf "$OUT"
mkdir -p "$OUT/relay-gateway" "$OUT/relay/relay-gateway"

rsync -a --delete \
  --exclude target \
  --exclude .cargo-cache \
  --exclude docker-ctx \
  --exclude docker-data \
  --exclude ci-data \
  --exclude data \
  --exclude .git \
  --exclude deps \
  --exclude terraform \
  --exclude 'strace*.txt' \
  "$GW_DIR/" "$OUT/relay-gateway/"
if [[ "${RELAY_DOCKER_REGISTRY_DEPS:-0}" != "1" ]]; then
  rsync -a --exclude .git --exclude target "$DEPS_ROOT/relay-runtime/" "$OUT/relay-runtime/"
  rsync -a --exclude .git --exclude target "$DEPS_ROOT/md2ast/" "$OUT/md2ast/"
fi
rsync -a --exclude .git --exclude target "$GW_DIR/themed-styler/" "$OUT/relay/relay-gateway/themed-styler/"

echo "[stage-docker-context] $OUT"

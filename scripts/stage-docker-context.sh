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

"$SCRIPT_DIR/ci-clone-path-deps.sh"

rm -rf "$OUT"
mkdir -p "$OUT/relay-gateway" "$OUT/relay/relay-gateway"

rsync -a --delete \
  --exclude target \
  --exclude docker-data \
  --exclude ci-data \
  --exclude data \
  --exclude .git \
  --exclude deps \
  "$GW_DIR/" "$OUT/relay-gateway/"
rsync -a "$DEPS_ROOT/relay-runtime/" "$OUT/relay-runtime/"
rsync -a "$DEPS_ROOT/md2ast/" "$OUT/md2ast/"
rsync -a "$GW_DIR/themed-styler/" "$OUT/relay/relay-gateway/themed-styler/"

echo "[stage-docker-context] $OUT"

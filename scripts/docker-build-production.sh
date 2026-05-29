#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
STAGE="${RELAY_DOCKER_STAGE:-$(mktemp -d)}"
IMAGE="${RELAY_IMAGE:-registry.gitlab.com/clevertree/relay-server/relay-gateway:latest}"
CLEANUP="${RELAY_DOCKER_STAGE:-}"

"$SCRIPT_DIR/stage-docker-context.sh" "$STAGE"

echo "[docker-build-production] build $IMAGE (context $STAGE)"
docker build -f "$GW_DIR/Dockerfile.production" -t "$IMAGE" "$STAGE"

if [[ -z "$CLEANUP" ]]; then
  rm -rf "$STAGE"
fi

echo "[docker-build-production] done: $IMAGE"

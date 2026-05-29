#!/usr/bin/env bash
# Build, tag, push relay-gateway image and roll Rackspace DaemonSet.
# Usage:
#   REGISTRY=registry.gitlab.com/clevertree/relay-server \
#   RELAY_IMAGE_TAG=main \
#   ./build-push-deploy.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
REGISTRY="${REGISTRY:-registry.gitlab.com/clevertree/relay-server}"
TAG="${RELAY_IMAGE_TAG:-main}"
IMAGE="${REGISTRY}/relay-gateway:${TAG}"

echo "Building image $IMAGE"
"$GW_DIR/scripts/docker-build-production.sh"
docker tag "${RELAY_IMAGE:-registry.gitlab.com/clevertree/relay-server/relay-gateway:latest}" "$IMAGE"

echo "Pushing image $IMAGE"
docker push "$IMAGE"

KUBECONFIG="${KUBECONFIG:-/tmp/kubeconfig}"
for manifest in relay-daemonset.yaml relay-daemonset-dfw2.yaml; do
  path="$SCRIPT_DIR/../k8s/$manifest"
  if [[ -f "$path" ]]; then
    sed -i.bak "s|image: registry.gitlab.com/clevertree/relay-server/relay-gateway:.*|image: ${IMAGE}|" "$path"
  fi
done

echo "Applying DaemonSet manifest(s)"
kubectl --kubeconfig="$KUBECONFIG" apply -f "$SCRIPT_DIR/../k8s/relay-daemonset.yaml"
if [[ -f "$SCRIPT_DIR/../k8s/relay-daemonset-dfw2.yaml" ]]; then
  kubectl --kubeconfig="$KUBECONFIG" apply -f "$SCRIPT_DIR/../k8s/relay-daemonset-dfw2.yaml"
fi

echo "Restarting rollout"
kubectl --kubeconfig="$KUBECONFIG" rollout restart daemonset/relay-daemon || true

echo "Done"

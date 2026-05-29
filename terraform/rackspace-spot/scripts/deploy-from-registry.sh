#!/usr/bin/env bash
# Roll Rackspace DaemonSet using image already published by GitLab CI.
# Usage:
#   docker login registry.gitlab.com
#   export KUBECONFIG=/path/to/relay-gateway/terraform/rackspace-spot/kubeconfig
#   REGISTRY=registry.gitlab.com/clevertree/relay-server RELAY_IMAGE_TAG=main \
#     ./deploy-from-registry.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
KUBECTL="${KUBECTL:-${GW_DIR}/terraform/kubectl}"
if [[ ! -x "$KUBECTL" ]]; then
  KUBECTL=kubectl
fi
REGISTRY="${REGISTRY:-registry.gitlab.com/clevertree/relay-server}"
TAG="${RELAY_IMAGE_TAG:-main}"
IMAGE="${REGISTRY}/relay-gateway:${TAG}"
KUBECONFIG="${KUBECONFIG:-${SCRIPT_DIR}/../kubeconfig}"

if [[ ! -f "$KUBECONFIG" ]]; then
  echo "kubeconfig not found: $KUBECONFIG" >&2
  echo "Run: cd terraform/rackspace-spot && terraform output -raw kubeconfig > kubeconfig" >&2
  exit 1
fi

export KUBECONFIG

echo "Deploying image $IMAGE"
for manifest in relay-configmap.yaml relay-boot-configmap.yaml relay-daemonset.yaml relay-daemonset-dfw2.yaml; do
  path="$SCRIPT_DIR/../k8s/$manifest"
  if [[ -f "$path" ]]; then
    sed -i.bak "s|image: registry.gitlab.com/clevertree/relay-server/relay-gateway:.*|image: ${IMAGE}|" "$path"
  fi
done

"$KUBECTL" apply -f "$SCRIPT_DIR/../k8s/relay-configmap.yaml"
"$KUBECTL" apply -f "$SCRIPT_DIR/../k8s/relay-boot-configmap.yaml"
"$KUBECTL" apply -f "$SCRIPT_DIR/../k8s/relay-daemonset.yaml"
if [[ -f "$SCRIPT_DIR/../k8s/relay-daemonset-dfw2.yaml" ]]; then
  "$KUBECTL" apply -f "$SCRIPT_DIR/../k8s/relay-daemonset-dfw2.yaml"
fi

"$KUBECTL" rollout restart daemonset/relay-daemon 2>/dev/null || true
"$KUBECTL" rollout status daemonset/relay-daemon --timeout=300s 2>/dev/null || true

echo "Done. Pods:"
"$KUBECTL" get pods -l app=relay -o wide 2>/dev/null || true

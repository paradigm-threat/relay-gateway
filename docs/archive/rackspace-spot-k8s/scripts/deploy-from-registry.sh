#!/usr/bin/env bash
# Roll Rackspace DaemonSet for one spot stack (dfw1, lon1, ord1).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TF_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
STACK="${RELAY_SPOT_STACK:?Set RELAY_SPOT_STACK=dfw1|lon1|ord1}"

KUBECTL="${KUBECTL:-${GW_DIR}/terraform/kubectl}"
if [[ ! -x "$KUBECTL" ]]; then
  KUBECTL=kubectl
fi
REGISTRY="${REGISTRY:-registry.gitlab.com/clevertree/relay-gateway}"
TAG="${RELAY_IMAGE_TAG:-main}"
IMAGE="${REGISTRY}/relay-gateway:${TAG}"

case "$STACK" in
  dfw1) KUBECONFIG="${KUBECONFIG:-${TF_ROOT}/kubeconfig}" ;;
  lon1|ord1) KUBECONFIG="${KUBECONFIG:-${TF_ROOT}/stacks/${STACK}/kubeconfig}" ;;
  *)
    echo "Unknown RELAY_SPOT_STACK=$STACK" >&2
    exit 1
    ;;
esac

K8S_DIR="${TF_ROOT}/k8s/${STACK}"
if [[ ! -d "$K8S_DIR" ]]; then
  echo "Missing k8s manifests: $K8S_DIR" >&2
  exit 1
fi

if [[ ! -f "$KUBECONFIG" ]]; then
  echo "kubeconfig not found: $KUBECONFIG" >&2
  exit 1
fi

export KUBECONFIG

echo "[$STACK] Deploying image $IMAGE"
for manifest in relay-daemonset.yaml; do
  path="$K8S_DIR/$manifest"
  if [[ -f "$path" ]]; then
    sed -i.bak "s|image: registry.gitlab.com/clevertree/relay-gateway:.*|image: ${IMAGE}|" "$path"
  fi
done

"$KUBECTL" apply -f "${TF_ROOT}/k8s/relay-boot-configmap.yaml"
"$KUBECTL" apply -f "$K8S_DIR/relay-configmap.yaml"
"$KUBECTL" apply -f "$K8S_DIR/relay-daemonset.yaml"

# Retire legacy dfw2 daemonset on dfw1 cluster
if [[ "$STACK" == "dfw1" ]]; then
  "$KUBECTL" delete daemonset relay-daemon-dfw2 --ignore-not-found=true 2>/dev/null || true
fi

"$KUBECTL" rollout restart daemonset/relay-daemon 2>/dev/null || true
"$KUBECTL" rollout status daemonset/relay-daemon --timeout=300s 2>/dev/null || true

echo "[$STACK] Pods:"
"$KUBECTL" get pods -l app=relay -o wide 2>/dev/null || true

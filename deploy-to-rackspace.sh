#!/bin/bash
# Deploy Relay to Rackspace Kubernetes cluster

set -e

# Use local "$KUBECTL" --kubeconfig "$KUBECONFIG" and kubeconfig
KUBECTL="/home/ari/dev/relay-server/terraform/kubectl"
KUBECONFIG="/home/ari/dev/relay-server/terraform/rackspace-spot/kubeconfig"

NAMESPACE="${1:-default}"
WAIT_TIMEOUT="${2:-300}"

echo "🚀 Deploying Relay to Rackspace"

# Verify "$KUBECTL" --kubeconfig "$KUBECONFIG" connection
echo "📋 Checking cluster connection..."
CONTEXT=$($KUBECTL --kubeconfig "$KUBECONFIG" config current-context) || {
    echo "✗ Failed to connect to cluster"
    exit 1
}
echo "✓ Connected to: $CONTEXT"

# Get k8s manifests directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
K8S_DIR="$SCRIPT_DIR/terraform/rackspace-spot/k8s"

if [[ ! -d "$K8S_DIR" ]]; then
    echo "✗ K8s manifests directory not found: $K8S_DIR"
    exit 1
fi

echo "📂 K8s manifests: $K8S_DIR"

# Apply daemonsets
echo ""
echo "📦 Applying relay-daemonset.yaml..."
"$KUBECTL" --kubeconfig "$KUBECONFIG" apply -f "$K8S_DIR/relay-daemonset.yaml"

echo "📦 Applying relay-daemonset-dfw2.yaml..."
"$KUBECTL" --kubeconfig "$KUBECONFIG" apply -f "$K8S_DIR/relay-daemonset-dfw2.yaml"

# Restart rollout
echo ""
echo "🔄 Restarting relay-daemon..."
"$KUBECTL" --kubeconfig "$KUBECONFIG" rollout restart daemonset/relay-daemon -n "$NAMESPACE"

echo "🔄 Restarting relay-daemon-dfw2..."
"$KUBECTL" --kubeconfig "$KUBECONFIG" rollout restart daemonset/relay-daemon-dfw2 -n "$NAMESPACE"

# Wait for rollout
echo ""
echo "⏳ Waiting for rollout (timeout: ${WAIT_TIMEOUT}s)..."
echo "  - relay-daemon"
"$KUBECTL" --kubeconfig "$KUBECONFIG" rollout status daemonset/relay-daemon -n "$NAMESPACE" --timeout="${WAIT_TIMEOUT}s" || {
    echo "✗ relay-daemon rollout failed"
    exit 1
}

echo "  - relay-daemon-dfw2"
"$KUBECTL" --kubeconfig "$KUBECONFIG" rollout status daemonset/relay-daemon-dfw2 -n "$NAMESPACE" --timeout="${WAIT_TIMEOUT}s" || {
    echo "✗ relay-daemon-dfw2 rollout failed"
    exit 1
}

# Verify deployment
echo ""
echo "✅ Deployment successful!"

echo ""
echo "📊 Pod Status:"
"$KUBECTL" --kubeconfig "$KUBECONFIG" get pods -n "$NAMESPACE" -l app=relay
"$KUBECTL" --kubeconfig "$KUBECONFIG" get pods -n "$NAMESPACE" -l app=relay-dfw2

echo ""
echo "🔍 Image Verification:"
echo "relay-daemon:"
"$KUBECTL" --kubeconfig "$KUBECONFIG" describe pod -n "$NAMESPACE" -l app=relay | grep "Image:" | head -1

echo "relay-daemon-dfw2:"
"$KUBECTL" --kubeconfig "$KUBECONFIG" describe pod -n "$NAMESPACE" -l app=relay-dfw2 | grep "Image:" | head -1

echo ""
echo "✨ Relay is now running the latest version!"

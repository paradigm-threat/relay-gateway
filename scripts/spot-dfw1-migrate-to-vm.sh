#!/usr/bin/env bash
# Migrate dfw1 from Spot Kubernetes (relay-network-cheapest) to a bare Spot VM + relay-master-deploy.
#
# VM Cloudspaces are UI-only today (no spotctl/terraform VM API). This script:
#   1) Shows current K8s cloudspace status (often still running if UI "delete" failed)
#   2) Prints your deploy SSH public key for Spot → Manage SSH Keys
#   3) Optionally deletes the K8s cloudspace (CONFIRM_DELETE_SPOT_K8S=relay-network-cheapest)
#   4) Waits for SPOT_DFW1_IP or terraform.tfvars public_ip_override
#   5) Runs relay-master-deploy.sh --target spot-dfw1
#
# Usage (from relay-gateway/):
#   ./scripts/spot-dfw1-migrate-to-vm.sh status
#   ./scripts/spot-dfw1-migrate-to-vm.sh print-ssh-key
#   CONFIRM_DELETE_SPOT_K8S=relay-network-cheapest ./scripts/spot-dfw1-migrate-to-vm.sh delete-k8s
#   SPOT_DFW1_IP=203.0.113.10 ./scripts/spot-dfw1-migrate-to-vm.sh deploy
#   ./scripts/spot-dfw1-migrate-to-vm.sh all   # delete-k8s only if CONFIRM set; then deploy
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

K8S_CS="${SPOT_K8S_CLOUDSPACE:-relay-network-cheapest}"
VM_TFVARS="$ROOT/terraform/rackspace-spot-vm/terraform.tfvars"
KEY_PUB="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}.pub"
LEGACY_TFVARS="$ROOT/terraform/rackspace-spot/terraform.tfvars"

spot_token() {
  if [[ -n "${RXTSPOT_TOKEN:-}" ]]; then
    echo "$RXTSPOT_TOKEN"
    return 0
  fi
  [[ -f "$LEGACY_TFVARS" ]] || return 1
  grep -E '^rackspace_spot_token\s*=' "$LEGACY_TFVARS" | sed -n 's/.*=\s*"\([^"]*\)".*/\1/p' | head -1
}

cmd_status() {
  export RXTSPOT_TOKEN="$(spot_token || true)"
  [[ -n "${RXTSPOT_TOKEN:-}" ]] || { echo "Set RXTSPOT_TOKEN or keep terraform/rackspace-spot/terraform.tfvars (local)" >&2; exit 1; }
  echo "=== Spot cloudspaces (spotctl) ==="
  spotctl cloudspaces list -o json | jq -r '.cloudspaces[] | "\(.name) region=\(.region) workers=\([.assignedServers[]?.IP] | join(","))"'
  echo ""
  echo "=== dfw1 DNS ==="
  dig +short dfw1.relaygateway.net || true
  echo ""
  if [[ -f "${HOME}/.kube/${K8S_CS}.yaml" ]]; then
    echo "=== K8s pods on ${K8S_CS} ==="
    KUBECONFIG="${HOME}/.kube/${K8S_CS}.yaml" kubectl get pods -A -l 'app in (relay,caddy-relay)' -o wide 2>/dev/null || true
  fi
  echo ""
  echo "K8s cloudspace '${K8S_CS}' is separate from a VM Cloudspace."
  echo "Deleting in the UI only removes what you selected — '${K8S_CS}' may still exist (see list above)."
  echo ""
  echo "IMPORTANT: K8s workers are often Ubuntu 20.04 (glibc 2.31). Bare relay tarball needs Ubuntu 22.04+"
  echo "  (Spot VM image ubuntu2404). Do not install tarball on K8s worker OS without upgrading."
}

cmd_print_ssh_key() {
  [[ -f "$KEY_PUB" ]] || { echo "Missing $KEY_PUB" >&2; exit 1; }
  echo "Add this public key in Spot → Manage SSH Keys, then create VM Cloudspace (dfw1):"
  echo "  Region: us-central-dfw-1"
  echo "  Pool: 1× gp.vs1.medium-dfw, bid 0.005, image ubuntu2404"
  echo ""
  cat "$KEY_PUB"
}

cmd_delete_k8s() {
  [[ "${CONFIRM_DELETE_SPOT_K8S:-}" == "$K8S_CS" ]] || {
    echo "Refusing delete: set CONFIRM_DELETE_SPOT_K8S=${K8S_CS}" >&2
    exit 1
  }
  export RXTSPOT_TOKEN="$(spot_token)"
  echo "Deleting Kubernetes cloudspace ${K8S_CS} (all workers + control plane)..."
  if [[ -f "${HOME}/.kube/${K8S_CS}.yaml" ]]; then
    KUBECONFIG="${HOME}/.kube/${K8S_CS}.yaml" kubectl delete daemonset caddy-relay relay-daemon --ignore-not-found=true 2>/dev/null || true
  fi
  spotctl cloudspaces delete --name "$K8S_CS" -y
  echo "Deleted ${K8S_CS}. Wait ~2–5 min for IPs to release; create VM Cloudspace in UI before deploy."
}

resolve_vm_ip() {
  local ip="${SPOT_DFW1_IP:-}"
  if [[ -z "$ip" && -f "$VM_TFVARS" ]]; then
    ip="$(grep -E '^public_ip_override\s*=' "$VM_TFVARS" | sed -n 's/.*=\s*"\([^"]*\)".*/\1/p' | head -1)"
  fi
  if [[ -z "$ip" ]]; then
    ip="$(dig +short dfw1.relaygateway.net | head -1)"
  fi
  [[ -n "$ip" ]] || return 1
  echo "$ip"
}

cmd_deploy() {
  local ip
  ip="$(resolve_vm_ip)" || {
    echo "No VM IP. Create VM Cloudspace in Spot UI, then:" >&2
    echo "  SPOT_DFW1_IP=x.x.x.x $0 deploy" >&2
    echo "  or set public_ip_override in terraform/rackspace-spot-vm/terraform.tfvars" >&2
    exit 1
  }
  echo "Deploying bare relay to ${ip} (dfw1)..."
  mkdir -p "$(dirname "$VM_TFVARS")"
  if [[ -f "$VM_TFVARS" ]]; then
    if grep -q '^public_ip_override' "$VM_TFVARS"; then
      sed -i "s|^public_ip_override.*|public_ip_override = \"${ip}\"|" "$VM_TFVARS"
    else
      echo "public_ip_override = \"${ip}\"" >>"$VM_TFVARS"
    fi
  else
    cp "$ROOT/terraform/rackspace-spot-vm/terraform.tfvars.example" "$VM_TFVARS"
    echo "public_ip_override = \"${ip}\"" >>"$VM_TFVARS"
  fi
  SPOT_VM_STATE_FILE="$ROOT/terraform/rackspace-spot-vm/vm-state.json" \
    SPOT_PUBLIC_IP_OVERRIDE="$ip" RELAY_NODE=dfw1 \
    "$ROOT/terraform/rackspace-spot-vm/scripts/spot-vm-provision.sh" apply
  "$ROOT/scripts/relay-master-deploy.sh" --target spot-dfw1 --install-only --build
  "$ROOT/scripts/relay-master-validate.sh" --fqdn dfw1.relaygateway.net
}

cmd_all() {
  cmd_status
  if [[ "${CONFIRM_DELETE_SPOT_K8S:-}" == "$K8S_CS" ]]; then
    cmd_delete_k8s
  else
    echo ""
    echo "Skip K8s delete (set CONFIRM_DELETE_SPOT_K8S=${K8S_CS} to delete before deploy)."
  fi
  cmd_deploy
}

SUB="${1:-status}"
shift || true
case "$SUB" in
  status) cmd_status ;;
  print-ssh-key) cmd_print_ssh_key ;;
  delete-k8s) cmd_delete_k8s ;;
  deploy) cmd_deploy ;;
  all) cmd_all ;;
  *)
    echo "Usage: $0 {status|print-ssh-key|delete-k8s|deploy|all}" >&2
    exit 1
    ;;
esac

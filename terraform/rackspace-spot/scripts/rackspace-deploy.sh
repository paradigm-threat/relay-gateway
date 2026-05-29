#!/usr/bin/env bash
# Provision Rackspace Spot cloudspace, apply k8s manifests, roll relay image from GitLab CI.
# When spot nodes win bids, relay entrypoint upserts Vercel DNS (node-dfw1 + atlanta3).
#
# Usage (from relay-gateway/):
#   ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
#
# Env:
#   VERCEL_API_TOKEN / VERCEL_TEAM_ID — k8s secret + post-deploy atlanta3 upsert
#   RELAY_FLEET_DNS_FQDN — default atlanta3.relaygateway.net
#   SKIP_TERRAFORM=1 — only roll DaemonSets (kubeconfig must exist)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TF_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
GW_DIR="$(cd "$TF_DIR/../.." && pwd)"
TERRAFORM="${TERRAFORM:-${GW_DIR}/terraform/terraform}"
KUBECTL="${KUBECTL:-${GW_DIR}/terraform/kubectl}"
KUBECONFIG="${KUBECONFIG:-${TF_DIR}/kubeconfig}"
FLEET_FQDN="${RELAY_FLEET_DNS_FQDN:-atlanta3.relaygateway.net}"
NODE_LABEL="${RELAY_RACKSPACE_NODE_LABEL:-relay-node=dfw1}"

vercel_token_from_cli() {
  local f
  for f in "${HOME}/.local/share/com.vercel.cli/auth.json" "${HOME}/.config/vercel/auth.json"; do
    if [[ -f "$f" ]]; then
      jq -r '.token // empty' "$f"
      return 0
    fi
  done
  return 1
}

ensure_vercel_tfvars() {
  local secrets="${TF_DIR}/secrets.auto.tfvars"
  [[ -f "$secrets" ]] && return 0
  local token team
  token="${VERCEL_API_TOKEN:-$(vercel_token_from_cli || true)}"
  [[ -n "$token" ]] || {
    echo "Set VERCEL_API_TOKEN or log in with Vercel CLI (secrets.auto.tfvars missing)" >&2
    return 1
  }
  team="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
  cat >"$secrets" <<EOF
vercel_api_token = "$(printf '%s' "$token" | sed 's/"/\\"/g')"
vercel_team_id   = "${team}"
EOF
  chmod 600 "$secrets"
  echo "Wrote ${secrets} (gitignored)"
}

label_spot_nodes() {
  export KUBECONFIG
  local nodes=()
  mapfile -t nodes < <("$KUBECTL" get nodes -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)
  [[ "${#nodes[@]}" -ge 1 ]] && "$KUBECTL" label node "${nodes[0]}" relay-node=dfw1 --overwrite >/dev/null 2>&1 || true
  [[ "${#nodes[@]}" -ge 2 ]] && "$KUBECTL" label node "${nodes[1]}" relay-node=dfw2 --overwrite >/dev/null 2>&1 || true
  echo "Labeled ${#nodes[@]} node(s) relay-node=dfw1/dfw2"
}

ensure_k8s_credentials_secret() {
  export KUBECONFIG
  local token team mesh peer git_user git_tok
  token="${VERCEL_API_TOKEN:-$(vercel_token_from_cli || true)}"
  team="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
  [[ -n "$token" ]] || { echo "Skip relay-credentials secret: no VERCEL_API_TOKEN" >&2; return 0; }

  # Optional: source relay.env for mesh + GitLab deploy token (gitignored locally)
  local env_file="${GW_DIR}/relay.env"
  if [[ -f "$env_file" ]]; then
    # shellcheck disable=SC1090
    set +u; source "$env_file" 2>/dev/null || true; set -u
  fi
  mesh="${RELAY_MASTER_MESH_SECRET:-}"
  peer="${RELAY_PEER_SECRET:-}"
  git_user="${GITLAB_DEPLOY_USER:-gitlab+deploy-token}"
  git_tok="${GITLAB_DEPLOY_TOKEN:-}"

  local args=(
    --from-literal=VERCEL_API_TOKEN="$token"
    --from-literal=VERCEL_TEAM_ID="$team"
  )
  [[ -n "$mesh" ]] && args+=(--from-literal=RELAY_MASTER_MESH_SECRET="$mesh")
  [[ -n "$peer" ]] && args+=(--from-literal=RELAY_PEER_SECRET="$peer")
  [[ -n "$git_tok" ]] && args+=(--from-literal=GITLAB_DEPLOY_USER="$git_user" --from-literal=GITLAB_DEPLOY_TOKEN="$git_tok")

  "$KUBECTL" create secret generic relay-credentials \
    "${args[@]}" \
    --dry-run=client -o yaml | "$KUBECTL" apply -f -
  echo "Applied relay-credentials secret"
}

wait_for_kube_api() {
  local timeout="${1:-600}" elapsed=0
  export KUBECONFIG
  while [[ "$elapsed" -lt "$timeout" ]]; do
    if "$KUBECTL" cluster-info >/dev/null 2>&1; then
      return 0
    fi
    sleep 15
    elapsed=$((elapsed + 15))
    echo "Waiting for Kubernetes API (${elapsed}s)..." >&2
  done
  return 1
}

wait_for_node_ip() {
  local timeout="${1:-900}" elapsed=0 ip=""
  export KUBECONFIG
  while [[ "$elapsed" -lt "$timeout" ]]; do
    ip=$("$KUBECTL" get nodes -l "$NODE_LABEL" -o jsonpath='{.items[0].status.addresses[?(@.type=="ExternalIP")].address}' 2>/dev/null || true)
    if [[ -z "$ip" ]]; then
      ip=$("$KUBECTL" get nodes -l "$NODE_LABEL" -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null || true)
    fi
    if [[ "$ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "$ip"
      return 0
    fi
    # Nodes may exist before spot pool labels are applied
    if [[ "$elapsed" -ge 60 ]]; then
      local unlabeled
      unlabeled=$("$KUBECTL" get nodes -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null || true)
      if [[ "$unlabeled" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "$unlabeled"
        return 0
      fi
    fi
    sleep 15
    elapsed=$((elapsed + 15))
    echo "Waiting for spot node ExternalIP (${elapsed}s)..." >&2
  done
  return 1
}

upsert_fleet_dns() {
  local ip="$1"
  export VERCEL_API_TOKEN="${VERCEL_API_TOKEN:-$(vercel_token_from_cli || true)}"
  export VERCEL_TEAM_ID="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
  [[ -n "${VERCEL_API_TOKEN:-}" ]] || { echo "Skip fleet DNS upsert: no VERCEL_API_TOKEN" >&2; return 0; }
  "$SCRIPT_DIR/vercel-dns-upsert.sh" "$FLEET_FQDN" "$ip"
}

main() {
  cd "$TF_DIR"
  if [[ "${SKIP_TERRAFORM:-0}" != "1" ]]; then
    ensure_vercel_tfvars || true
    echo "==> terraform apply (cloudspace + nodepools; may take several minutes)"
    "$TERRAFORM" init -input=false
    local attempt=0 out
    while [[ "$attempt" -lt 12 ]]; do
      if out=$("$TERRAFORM" apply -auto-approve -input=false 2>&1); then
        echo "$out"
        break
      fi
      echo "$out"
      if echo "$out" | grep -q 'object is being deleted'; then
        attempt=$((attempt + 1))
        echo "Cloudspace still deleting; retry in 30s ($attempt/12)..." >&2
        sleep 30
      else
        exit 1
      fi
    done
    [[ "$attempt" -lt 12 ]] || { echo "terraform apply failed after retries" >&2; exit 1; }
  fi

  if [[ ! -f "$KUBECONFIG" ]]; then
    echo "kubeconfig missing after apply: $KUBECONFIG" >&2
    exit 1
  fi

  echo "==> deploy relay DaemonSet from GitLab registry"
  export KUBECONFIG
  if ! wait_for_kube_api 600; then
    echo "Kubernetes API not reachable — cloudspace may still be provisioning (check Rackspace Spot UI)" >&2
    exit 1
  fi
  ensure_k8s_credentials_secret
  label_spot_nodes
  "$SCRIPT_DIR/deploy-from-registry.sh"

  echo "==> wait for spot node and upsert ${FLEET_FQDN}"
  if node_ip=$(wait_for_node_ip 900); then
    echo "Node ExternalIP: $node_ip"
    upsert_fleet_dns "$node_ip" || echo "WARN: fleet DNS upsert failed (entrypoint may still update on pod start)" >&2
    echo "Verify: dig +short ${FLEET_FQDN} @8.8.8.8"
  else
    echo "WARN: no ExternalIP within timeout — spot bid may still be pending; DNS updates when pod starts" >&2
  fi

  "$KUBECTL" get nodes -o wide 2>/dev/null || true
  "$KUBECTL" get pods -l app=relay -o wide 2>/dev/null || true
}

main "$@"

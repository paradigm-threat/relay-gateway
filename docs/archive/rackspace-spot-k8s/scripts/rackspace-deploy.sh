#!/usr/bin/env bash
# Provision Rackspace Spot cloudspaces and deploy relay on dfw1, lon1, ord1.
#
# Usage (from relay-gateway/):
#   ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
#   RELAY_SPOT_STACK=dfw1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
#   SKIP_TERRAFORM=1 RELAY_SPOT_STACKS="dfw1 lon1 ord1" ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
#
# Env:
#   RELAY_SPOT_STACK — deploy only this stack (dfw1|lon1|ord1)
#   RELAY_SPOT_STACKS — space-separated list (default: dfw1 lon1 ord1)
#   SKIP_TERRAFORM=1 — k8s roll only
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TF_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GW_DIR="$(cd "$TF_ROOT/../.." && pwd)"
TERRAFORM="${TERRAFORM:-${GW_DIR}/terraform/terraform}"
KUBECTL="${KUBECTL:-${GW_DIR}/terraform/kubectl}"

DEFAULT_STACKS=(dfw1 lon1 ord1)
if [[ -n "${RELAY_SPOT_STACK:-}" ]]; then
  STACKS=("$RELAY_SPOT_STACK")
else
  read -ra STACKS <<< "${RELAY_SPOT_STACKS:-dfw1 lon1 ord1}"
fi

# Spot provider accepts RXTSPOT_TOKEN; reuse token from dfw1 tfvars for all stacks.
if [[ -z "${RXTSPOT_TOKEN:-}" && -f "$TF_ROOT/terraform.tfvars" ]]; then
  RXTSPOT_TOKEN="$(grep -E '^rackspace_spot_token\s*=' "$TF_ROOT/terraform.tfvars" | sed -n 's/.*=\s*"\([^"]*\)".*/\1/p' | head -1)"
  export RXTSPOT_TOKEN
fi

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

stack_tf_dir() {
  case "$1" in
    dfw1) echo "$TF_ROOT" ;;
    lon1|ord1) echo "$TF_ROOT/stacks/$1" ;;
    *) echo "Unknown stack: $1" >&2; return 1 ;;
  esac
}

stack_kubeconfig() {
  case "$1" in
    dfw1) echo "$TF_ROOT/kubeconfig" ;;
    lon1|ord1) echo "$TF_ROOT/stacks/$1/kubeconfig" ;;
  esac
}

stack_fqdn() {
  echo "$1.relaygateway.net"
}

terraform_apply_stack() {
  local stack="$1" tf_dir
  tf_dir="$(stack_tf_dir "$stack")"
  cd "$tf_dir"
  echo "==> [$stack] terraform apply in $tf_dir"
  "$TERRAFORM" init -input=false
  local attempt=0 out var_file=()
  if [[ "$stack" == "dfw1" && -f "$TF_ROOT/terraform.tfvars" ]]; then
    var_file=(-var-file="$TF_ROOT/terraform.tfvars")
  fi
  while [[ "$attempt" -lt 12 ]]; do
    if out=$("$TERRAFORM" apply -auto-approve -input=false "${var_file[@]}" 2>&1); then
      echo "$out"
      break
    fi
    echo "$out"
    if echo "$out" | grep -q 'object is being deleted'; then
      attempt=$((attempt + 1))
      echo "[$stack] Cloudspace still deleting; retry in 30s ($attempt/12)..." >&2
      sleep 30
    else
      return 1
    fi
  done
  [[ "$attempt" -lt 12 ]] || { echo "[$stack] terraform apply failed after retries" >&2; return 1; }
}

wait_for_kube_api() {
  local kubeconfig="$1" timeout="${2:-600}" elapsed=0
  export KUBECONFIG="$kubeconfig"
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

ensure_k8s_credentials_secret() {
  local kubeconfig="$1"
  export KUBECONFIG="$kubeconfig"
  local token team mesh peer git_user git_tok
  # Prefer fresh Vercel CLI token (deploy secret 403s when relay.env token is stale).
  token="$(vercel_token_from_cli || true)"
  [[ -n "$token" ]] || token="${VERCEL_API_TOKEN:-}"
  team="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
  [[ -n "$token" ]] || { echo "Skip relay-credentials secret: no VERCEL_API_TOKEN" >&2; return 0; }

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
}

wait_for_node_ip() {
  local stack="$1" kubeconfig="$2"
  local timeout="${3:-900}" elapsed=0 ip=""
  export KUBECONFIG="$kubeconfig"
  while [[ "$elapsed" -lt "$timeout" ]]; do
    ip=$("$KUBECTL" get nodes -l component=relay-server -o jsonpath='{.items[0].status.addresses[?(@.type=="ExternalIP")].address}' 2>/dev/null || true)
    if [[ -z "$ip" ]]; then
      ip=$("$KUBECTL" get nodes -l component=relay-server -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null || true)
    fi
    if [[ -z "$ip" && "$elapsed" -ge 60 ]]; then
      ip=$("$KUBECTL" get nodes -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null || true)
    fi
    if [[ "$ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "$ip"
      return 0
    fi
    sleep 15
    elapsed=$((elapsed + 15))
    echo "[$stack] Waiting for node IP (${elapsed}s)..." >&2
  done
  return 1
}

upsert_stack_dns() {
  local stack="$1" ip="$2" fqdn
  fqdn="$(stack_fqdn "$stack")"
  export VERCEL_API_TOKEN="$(vercel_token_from_cli || true)"
  [[ -n "${VERCEL_API_TOKEN}" ]] || export VERCEL_API_TOKEN="${VERCEL_API_TOKEN:-}"
  export VERCEL_TEAM_ID="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
  [[ -n "${VERCEL_API_TOKEN:-}" ]] || { echo "[$stack] Skip DNS upsert: no token" >&2; return 0; }
  "$SCRIPT_DIR/vercel-dns-upsert.sh" "$fqdn" "$ip"
  # Repo mirrors use Host: {repo}.{stack}.relaygateway.net — need *.dfw1 on relaygateway.net (not dfw1.relaygateway.net zone).
  local team_q="" auth token
  token="${VERCEL_API_TOKEN}"
  auth="Authorization: Bearer ${token}"
  team_q=""
  [[ -n "${VERCEL_TEAM_ID:-}" ]] && team_q="?teamId=${VERCEL_TEAM_ID}"
  local body
  body=$(jq -n --arg n "*.${stack}" --arg v "$ip" --argjson ttl "${RELAY_DNS_TTL:-60}" \
    '{name:$n,value:$v,type:"A",ttl:$ttl}')
  if curl -fsS -X POST -H "$auth" -H 'Content-Type: application/json' -d "$body" \
    "https://api.vercel.com/v4/domains/relaygateway.net/records${team_q}" >/dev/null; then
    echo "[$stack] DNS *.${stack}.relaygateway.net -> ${ip} (relaygateway.net zone)"
  else
    echo "[$stack] WARN: wildcard DNS upsert failed" >&2
  fi
}

deploy_stack() {
  local stack="$1" kubeconfig fqdn
  kubeconfig="$(stack_kubeconfig "$stack")"
  fqdn="$(stack_fqdn "$stack")"

  if [[ "${SKIP_TERRAFORM:-0}" != "1" ]]; then
    terraform_apply_stack "$stack" || return 1
  fi

  if [[ ! -f "$kubeconfig" ]]; then
    echo "[$stack] kubeconfig missing: $kubeconfig" >&2
    return 1
  fi

  echo "==> [$stack] deploy relay DaemonSet"
  if ! wait_for_kube_api "$kubeconfig" 600; then
    echo "[$stack] Kubernetes API not reachable" >&2
    return 1
  fi
  ensure_k8s_credentials_secret "$kubeconfig"
  RELAY_SPOT_STACK="$stack" KUBECONFIG="$kubeconfig" "$SCRIPT_DIR/deploy-from-registry.sh"

  echo "==> [$stack] upsert $fqdn"
  if node_ip=$(wait_for_node_ip "$stack" "$kubeconfig" 900); then
    echo "[$stack] Node IP: $node_ip"
    upsert_stack_dns "$stack" "$node_ip" || echo "[$stack] WARN: DNS upsert failed" >&2
  else
    echo "[$stack] WARN: no node IP within timeout" >&2
  fi

  export KUBECONFIG="$kubeconfig"
  "$KUBECTL" get nodes -o wide 2>/dev/null || true
  "$KUBECTL" get pods -l app=relay -o wide 2>/dev/null || true
}

main() {
  local stack
  for stack in "${STACKS[@]}"; do
    deploy_stack "$stack" || echo "WARN: stack $stack deploy failed" >&2
  done
}

main "$@"

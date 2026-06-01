#!/usr/bin/env bash
# Provision (optional) and install a relay master on Linode or Rackspace Spot VM.
#
# Usage (from relay-gateway/):
#   ./scripts/relay-master-deploy.sh --target spot-dfw1
#   ./scripts/relay-master-deploy.sh --target linode-atlanta2 --install-only
#   ./scripts/relay-master-deploy.sh --target spot-dfw1 --install-only --build
#
# Targets: spot-dfw1 | linode-atlanta2
#
# Steps: terraform apply (unless --install-only) → SSH → pack tarball → install.sh →
#        Vercel apex + wildcard DNS → install-caddy-tls → reconfigure-features (Piper) → peer-clone PT repos
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET=""
INSTALL_ONLY=0
BUILD=0
SKIP_TERRAFORM=0
SKIP_DNS=0
SKIP_CADDY=0
SKIP_SEED=0
PEER_SOURCE="${RELAY_PEER_CLONE_SOURCE:-atlanta2.relaygateway.net}"
CADDY_EMAIL="${CADDY_EMAIL:-hostmaster@relaygateway.net}"
ENV_FILE="${RELAY_FLEET_ENV_FILE:-$ROOT/relay.env}"
DNS_DIR="$ROOT/terraform/shared-dns/scripts"
TERRAFORM="${TERRAFORM:-terraform}"

usage() {
  sed -n '1,35p' <<'EOF'
Usage: ./scripts/relay-master-deploy.sh --target <spot-dfw1|linode-atlanta2> [flags]

Flags:
  --install-only     Skip terraform apply; read IP from terraform output / DNS
  --build            Run pack-linode-deploy.sh before install
  --skip-terraform   Alias for --install-only
  --skip-dns         Skip Vercel DNS upsert
  --skip-caddy       Skip install-caddy-tls.sh on node
  --skip-seed        Skip peer-clone of PT bare repos
  --peer=FQDN        Peer for git:// clone (default atlanta2.relaygateway.net)
  --env-file=PATH    relay.env for mesh secrets (default ./relay.env)
EOF
  exit "${1:-0}"
}

while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --target=*) TARGET="${1#*=}" ;;
    --target) shift; TARGET="${1:?}" ;;
    --install-only|--skip-terraform) INSTALL_ONLY=1 ;;
    --build) BUILD=1 ;;
    --skip-dns) SKIP_DNS=1 ;;
    --skip-caddy) SKIP_CADDY=1 ;;
    --skip-seed) SKIP_SEED=1 ;;
    --peer=*) PEER_SOURCE="${1#*=}" ;;
    --env-file=*) ENV_FILE="${1#*=}" ;;
    -h|--help) usage 0 ;;
    *) echo "Unknown flag: $1" >&2; usage 1 ;;
  esac
  shift
done

[[ -n "$TARGET" ]] || usage 1

stack_dir() {
  case "$1" in
    spot-dfw1) echo "$ROOT/terraform/rackspace-spot-vm" ;;
    linode-atlanta2) echo "$ROOT/terraform/linode-relay" ;;
    *) echo "Unknown --target: $1 (use spot-dfw1 or linode-atlanta2)" >&2; return 1 ;;
  esac
}

default_ssh_user() {
  case "$1" in
    spot-dfw1) echo "ubuntu" ;;
    linode-atlanta2) echo "ari" ;;
  esac
}

vercel_token_from_cli() {
  local f
  for f in "${HOME}/.local/share/com.vercel.cli/auth.json" "${HOME}/.config/vercel/auth.json"; do
    [[ -f "$f" ]] && jq -r '.token // empty' "$f" && return 0
  done
  return 1
}

tf_output() {
  local dir="$1" key="$2"
  (cd "$dir" && "$TERRAFORM" output -raw "$key" 2>/dev/null) || true
}

resolve_node_meta() {
  local target="$1" dir fqdn ip relay_id ssh_user node_slug
  dir="$(stack_dir "$target")"
  fqdn="$(tf_output "$dir" fqdn)"
  relay_id="$(tf_output "$dir" relay_server_id)"
  ssh_user="$(tf_output "$dir" ssh_user)"
  ip="$(tf_output "$dir" public_ip)"
  [[ -n "$ssh_user" ]] || ssh_user="$(default_ssh_user "$target")"
  node_slug="${fqdn%%.*}"
  [[ -n "$fqdn" ]] || fqdn="${node_slug}.relaygateway.net"
  [[ -n "$relay_id" ]] || relay_id="relay-${node_slug}"
  if [[ -z "$ip" ]]; then
    ip="$(dig +short "$fqdn" 2>/dev/null | head -1 || true)"
  fi
  [[ -n "$ip" ]] || { echo "Could not resolve IP for $fqdn" >&2; return 1; }
  echo "$dir|$fqdn|$ip|$relay_id|$ssh_user|$node_slug"
}

upsert_dns() {
  local fqdn="$1" ip="$2" node_slug="$3"
  export VERCEL_API_TOKEN="$(vercel_token_from_cli || true)"
  [[ -n "${VERCEL_API_TOKEN:-}" ]] || VERCEL_API_TOKEN="${VERCEL_API_TOKEN:-}"
  [[ -n "${VERCEL_API_TOKEN:-}" ]] || { echo "[deploy] Skip DNS: no VERCEL_API_TOKEN" >&2; return 0; }
  export VERCEL_TEAM_ID="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
  "$DNS_DIR/vercel-dns-upsert.sh" "$fqdn" "$ip"
  "$DNS_DIR/vercel-dns-wildcard.sh" "$node_slug" "$ip"
}

wait_ssh() {
  local target="$1" timeout="${2:-300}" elapsed=0
  while [[ "$elapsed" -lt "$timeout" ]]; do
    if "${SSH[@]}" "$target" true 2>/dev/null; then
      return 0
    fi
    sleep 5
    elapsed=$((elapsed + 5))
  done
  return 1
}

KEY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
[[ -f "$KEY" ]] || { echo "Missing SSH key: $KEY" >&2; exit 1; }

if [[ "$BUILD" == 1 ]]; then
  echo "[deploy] pack-linode-deploy.sh"
  ./scripts/pack-linode-deploy.sh
fi

TGZ="${ROOT}/target/relay-linode-deploy.tgz"
[[ -f "$TGZ" ]] || { echo "Missing $TGZ (run with --build)" >&2; exit 1; }

IFS='|' read -r TF_DIR FQDN IP RELAY_ID SSH_USER NODE_SLUG < <(resolve_node_meta "$TARGET")
SSH_TARGET="${SSH_USER}@${IP}"

echo "[deploy] target=$TARGET fqdn=$FQDN ip=$IP ssh=$SSH_USER"

if [[ "$INSTALL_ONLY" != 1 ]]; then
  echo "[deploy] terraform apply in $TF_DIR"
  (cd "$TF_DIR" && "$TERRAFORM" init -input=false && "$TERRAFORM" apply -auto-approve -input=false)
  IFS='|' read -r TF_DIR FQDN IP RELAY_ID SSH_USER NODE_SLUG < <(resolve_node_meta "$TARGET")
  SSH_TARGET="${SSH_USER}@${IP}"
fi

SSH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes)
SCP=(scp -i "$KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes)

echo "[deploy] wait SSH $SSH_TARGET"
wait_ssh "$SSH_TARGET" 300 || { echo "SSH not ready" >&2; exit 1; }

if [[ "$SKIP_DNS" != 1 ]]; then
  echo "[deploy] Vercel DNS $FQDN + *.${NODE_SLUG}"
  upsert_dns "$FQDN" "$IP" "$NODE_SLUG" || echo "[deploy] WARN: DNS upsert failed" >&2
fi

echo "[deploy] upload tarball"
"${SCP[@]}" "$TGZ" "${SSH_TARGET}:/tmp/relay-linode-deploy.tgz"

# Mesh secrets from laptop relay.env
MESH_VARS=()
if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  set +u; source "$ENV_FILE" 2>/dev/null || true; set -u
fi
for v in RELAY_MASTER_MESH_SECRET RELAY_PEER_SECRET RELAY_MASTER_TRUST_ROOT \
  RELAY_MASTER_CATALOG_GIT_URL RELAY_MASTER_PEER_LIST RELAY_MASTER_CATALOG_PATH; do
  [[ -n "${!v:-}" ]] && MESH_VARS+=("$v=${!v}")
done

RELAY_MASTER_PEER_LIST="${RELAY_MASTER_PEER_LIST:-atlanta2.relaygateway.net;dfw1.relaygateway.net}"
MESH_EXPORT="RELAY_PUBLIC_HOSTNAME=${FQDN} RELAY_SERVER_ID=${RELAY_ID} RELAY_MASTER_PEER_LIST=${RELAY_MASTER_PEER_LIST}"

echo "[deploy] install.sh install"
if [[ "$SSH_USER" == "root" ]]; then
  "${SSH[@]}" "$SSH_TARGET" bash -s <<REMOTE
set -euo pipefail
cd /root && rm -rf .relay-deploy-staging && mkdir .relay-deploy-staging && cd .relay-deploy-staging
tar xzf /root/relay-linode-deploy.tgz 2>/dev/null || tar xzf /tmp/relay-linode-deploy.tgz
export RELAY_BIN_SOURCE=/root/.relay-deploy-staging RELAY_INSTALL_NONINTERACTIVE=1 RELAY_SKIP_VERCEL_DNS=1
${MESH_EXPORT}
${MESH_VARS[*]:+export ${MESH_VARS[*]}}
./install.sh install
REMOTE
else
  "${SSH[@]}" "$SSH_TARGET" sudo bash -s <<REMOTE
set -euo pipefail
cp -f /tmp/relay-linode-deploy.tgz /root/relay-linode-deploy.tgz
cd /root && rm -rf .relay-deploy-staging && mkdir .relay-deploy-staging && cd .relay-deploy-staging
tar xzf /root/relay-linode-deploy.tgz
export RELAY_BIN_SOURCE=/root/.relay-deploy-staging RELAY_INSTALL_NONINTERACTIVE=1 RELAY_SKIP_VERCEL_DNS=1
${MESH_EXPORT}
${MESH_VARS[*]:+export ${MESH_VARS[*]}}
./install.sh install
rm -f /tmp/relay-linode-deploy.tgz
REMOTE
fi

if [[ "$SKIP_CADDY" != 1 ]]; then
  echo "[deploy] install-caddy-tls.sh"
  "${SSH[@]}" "$SSH_TARGET" sudo bash -s <<REMOTE
set -euo pipefail
export RELAY_PUBLIC_HOSTNAME=${FQDN} CADDY_EMAIL=${CADDY_EMAIL}
bash /opt/relay/bin/install-caddy-tls.sh 2>/dev/null || bash /root/.relay-deploy-staging/install-caddy-tls.sh
REMOTE
fi

echo "[deploy] reconfigure-features (Piper on)"
"${SCP[@]}" "$TGZ" "${SSH_TARGET}:/tmp/relay-feat.tgz"
"${SSH[@]}" "$SSH_TARGET" sudo bash -s <<'REMOTE'
set -euo pipefail
install -d -m 700 /root/.relay-feat-staging
cd /root/.relay-feat-staging && tar xzf /tmp/relay-feat.tgz
export RELAY_INSTALL_NONINTERACTIVE=1 RELAY_FEAT_PIPER=1 RELAY_FEAT_TRANSLATION=0
./install.sh reconfigure-features
rm -f /tmp/relay-feat.tgz
REMOTE

if [[ "$SKIP_SEED" != 1 && "$TARGET" == spot-dfw1* ]]; then
  echo "[deploy] peer-clone PT repos from ${PEER_SOURCE}"
  REPOS=(paradigm-threat-files paradigm-threat-timeline paradigm-threat-site)
  for repo in "${REPOS[@]}"; do
    "${SSH[@]}" "$SSH_TARGET" sudo bash -s "$repo" "$PEER_SOURCE" <<'REMOTE' || true
set -euo pipefail
REPO="$1" PEER="$2"
DATA=/opt/relay/data
sudo -u relay bash -lc "curl -fsS -X POST http://127.0.0.1:8080/api/git/ensure-bare/${REPO}"
sudo -u relay bash -lc "cd ${DATA}/${REPO}.git && git fetch git://${PEER}:9418/${REPO}.git +refs/heads/*:refs/heads/*"
REMOTE
  done
fi

echo "[deploy] done — run: ./scripts/relay-master-validate.sh --fqdn ${FQDN}"

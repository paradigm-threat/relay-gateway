#!/usr/bin/env bash
# Install / seed a relay node (Linode-first). Spot targets are retired.
#
# Usage (from relay-gateway/):
#   RELAY_OPS_REPAIR_IP=172.x.x.x ./scripts/relay-master-deploy.sh --target linode-pub1 --install-only --build
#   ./scripts/relay-master-deploy.sh --target linode-mesh1 --install-only --build
#
# Targets: linode-pub1 | linode-mesh1 | linode-mesh2
# Env IP: RELAY_OPS_REPAIR_IP or LINODE_PUB1_IP / LINODE_MESH1_IP / LINODE_MESH2_IP
#
# Steps: SSH → pack tarball → install.sh → Vercel DNS (optional) → Caddy → seed park-tls → peer-clone
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
PEER_SOURCE="${RELAY_PEER_CLONE_SOURCE:-pub1.relaygateway.net}"
CADDY_EMAIL="${CADDY_EMAIL:-hostmaster@relaygateway.net}"
ENV_FILE="${RELAY_FLEET_ENV_FILE:-$ROOT/relay.env}"
DNS_DIR="$ROOT/terraform/shared-dns/scripts"
TERRAFORM="${TERRAFORM:-terraform}"

usage() {
  sed -n '1,35p' <<'EOF'
Usage: ./scripts/relay-master-deploy.sh --target <linode-pub1|linode-mesh1|linode-mesh2> [flags]

Flags:
  --install-only     Skip terraform (required for current Linode CLI-provisioned nodes)
  --build            Run pack-linode-deploy.sh before install
  --skip-terraform   Alias for --install-only
  --skip-dns         Skip Vercel DNS upsert
  --skip-caddy       Skip install-caddy-tls.sh on node
  --skip-seed        Skip peer-clone of PT bare repos
  --peer=FQDN        Peer for git:// clone (default pub1.relaygateway.net)
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
    linode-pub1|linode-mesh1|linode-mesh2) echo "$ROOT/terraform/linode-relay" ;;
    *) echo "Unknown --target: $1 (use linode-pub1|linode-mesh1|linode-mesh2)" >&2; return 1 ;;
  esac
}

default_ssh_user() {
  case "$1" in
    linode-pub1|linode-mesh1|linode-mesh2) echo "root" ;;
    *) echo "root" ;;
  esac
}

linode_node_slug() {
  case "$1" in
    linode-pub1) echo "pub1" ;;
    linode-mesh1) echo "mesh1" ;;
    linode-mesh2) echo "mesh2" ;;
    *) echo "" ;;
  esac
}

linode_ip_override() {
  case "$1" in
    linode-pub1) echo "${RELAY_OPS_REPAIR_IP:-${LINODE_PUB1_IP:-172.234.148.70}}" ;;
    linode-mesh1) echo "${RELAY_OPS_REPAIR_IP:-${LINODE_MESH1_IP:-172.237.134.22}}" ;;
    linode-mesh2) echo "${RELAY_OPS_REPAIR_IP:-${LINODE_MESH2_IP:-172.236.146.220}}" ;;
    *) echo "${RELAY_OPS_REPAIR_IP:-}" ;;
  esac
}

# Compat aliases used later in the script
spot_node_slug() { linode_node_slug "$@"; }
spot_ip_override() { linode_ip_override "$@"; }
spot_tfvars_file() { return 0; }

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
  node_slug="$(linode_node_slug "$target")"
  [[ -n "$node_slug" ]] || { echo "Unknown target: $target" >&2; return 1; }
  fqdn="${node_slug}.relaygateway.net"
  relay_id="relay-${node_slug}"
  ssh_user="$(default_ssh_user "$target")"
  ip="$(linode_ip_override "$target")"
  if [[ -z "$ip" ]]; then
    ip="$(dig +short "$fqdn" 2>/dev/null | head -1 || true)"
  fi
  [[ -n "$ip" ]] || { echo "Could not resolve IP for $fqdn (set LINODE_*_IP or RELAY_OPS_REPAIR_IP)" >&2; return 1; }
  echo "$dir|$fqdn|$ip|$relay_id|$ssh_user|$node_slug"
}

load_vercel_dns_creds() {
  # Prefer relay.env token (team-scoped); fall back to Vercel CLI auth.json.
  if [[ -f "$ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    set +u; source "$ENV_FILE" 2>/dev/null || true; set -u
  fi
  if [[ -z "${VERCEL_API_TOKEN:-}" ]]; then
    export VERCEL_API_TOKEN="$(vercel_token_from_cli || true)"
  fi
  export VERCEL_TEAM_ID="${VERCEL_TEAM_ID:-team_bB1RJkslury9ynlawD5kmMdG}"
}

upsert_dns() {
  local fqdn="$1" ip="$2" node_slug="$3"
  load_vercel_dns_creds
  [[ -n "${VERCEL_API_TOKEN:-}" ]] || { echo "[deploy] Skip DNS: no VERCEL_API_TOKEN (set in $ENV_FILE or vercel login)" >&2; return 0; }
  "$DNS_DIR/vercel-dns-upsert.sh" "$fqdn" "$ip"
  "$DNS_DIR/vercel-dns-wildcard.sh" "$node_slug" "$ip"
}

# After a node A record changes, refresh fleet.relaygateway.net.
# Only fat / publisher FQDNs belong in the multi-A set — never mesh-only Nanodes.
# Prefer RELAY_FLEET_DNS_PEERS (fat only); do not fall back to the full mesh peer list.
reconcile_fleet_dns() {
  load_vercel_dns_creds
  [[ -n "${VERCEL_API_TOKEN:-}" ]] || {
    echo "[deploy] Skip fleet DNS: no VERCEL_API_TOKEN" >&2
    return 0
  }
  local peers="${RELAY_FLEET_DNS_PEERS:-${RELAY_PUBLISH_ORIGIN_FQDN:-pub1.relaygateway.net}}"
  # shellcheck disable=SC2086
  IFS=';' read -ra peer_arr <<<"$peers"
  echo "[deploy] reconcile fleet.relaygateway.net from fat/publisher only: ${peer_arr[*]}"
  "$DNS_DIR/vercel-dns-fleet-reconcile.sh" --fleet="${RELAY_FLEET_DNS_NAME:-fleet.relaygateway.net}" "${peer_arr[@]}"
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
  TFVARS_FILE="$(spot_tfvars_file "$TARGET" || true)"
  if [[ -n "$TFVARS_FILE" ]]; then
    echo "[deploy] using -var-file=$(basename "$TFVARS_FILE")"
    (cd "$TF_DIR" && "$TERRAFORM" init -input=false && "$TERRAFORM" apply -auto-approve -input=false -var-file="$TFVARS_FILE")
  else
    (cd "$TF_DIR" && "$TERRAFORM" init -input=false && "$TERRAFORM" apply -auto-approve -input=false)
  fi
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
  reconcile_fleet_dns || echo "[deploy] WARN: fleet DNS reconcile failed" >&2
fi

echo "[deploy] upload tarball"
"${SCP[@]}" "$TGZ" "${SSH_TARGET}:/tmp/relay-linode-deploy.tgz"

# Mesh secrets from laptop relay.env (also loaded for DNS in upsert_dns)
MESH_VARS=()
load_vercel_dns_creds
for v in RELAY_MASTER_MESH_SECRET RELAY_PEER_SECRET RELAY_MASTER_TRUST_ROOT \
  RELAY_MASTER_CATALOG_GIT_URL RELAY_MASTER_PEER_LIST RELAY_MASTER_CATALOG_PATH; do
  [[ -n "${!v:-}" ]] && MESH_VARS+=("$v=${!v}")
done

RELAY_MASTER_PEER_LIST="${RELAY_MASTER_PEER_LIST:-pub1.relaygateway.net;mesh1.relaygateway.net;mesh2.relaygateway.net}"
# Ensure this node is in the mesh peer list.
_mesh_self="${NODE_SLUG}.relaygateway.net"
case ";${RELAY_MASTER_PEER_LIST};" in
  *";${_mesh_self};"*) ;;
  *) RELAY_MASTER_PEER_LIST="${RELAY_MASTER_PEER_LIST};${_mesh_self}" ;;
esac
unset _mesh_self

write_remote_env() {
  local dest="$1" v val
  {
    printf 'RELAY_MERGE_MESH_ENV=1\n'
    printf 'RELAY_PUBLIC_HOSTNAME=%q\n' "$FQDN"
    printf 'RELAY_SERVER_ID=%q\n' "$RELAY_ID"
    printf 'RELAY_MASTER_PEER_LIST=%q\n' "$RELAY_MASTER_PEER_LIST"
    for kv in "${MESH_VARS[@]}"; do
      v="${kv%%=*}"
      val="${kv#*=}"
      printf '%s=%q\n' "$v" "$val"
    done
  } >"$dest"
  chmod 600 "$dest"
}

echo "[deploy] install.sh install"
ENV_LOCAL="$(mktemp)"
write_remote_env "$ENV_LOCAL"
"${SCP[@]}" "$ENV_LOCAL" "${SSH_TARGET}:/tmp/relay-install.env"
rm -f "$ENV_LOCAL"

if [[ "$SSH_USER" == "root" ]]; then
  "${SSH[@]}" "$SSH_TARGET" bash -s <<'REMOTE'
set -euo pipefail
set -a
# shellcheck disable=SC1091
source /tmp/relay-install.env
set +a
cd /root && rm -rf .relay-deploy-staging && mkdir .relay-deploy-staging && cd .relay-deploy-staging
tar xzf /root/relay-linode-deploy.tgz 2>/dev/null || tar xzf /tmp/relay-linode-deploy.tgz
export RELAY_BIN_SOURCE=/root/.relay-deploy-staging RELAY_INSTALL_NONINTERACTIVE=1 RELAY_SKIP_VERCEL_DNS=1
if [[ -f /opt/relay/state/features.json ]]; then ./install.sh update; else ./install.sh install; fi
rm -f /tmp/relay-install.env
REMOTE
else
  "${SSH[@]}" "$SSH_TARGET" sudo bash -s <<'REMOTE'
set -euo pipefail
cp -f /tmp/relay-install.env /root/relay-install.env
set -a
# shellcheck disable=SC1091
source /root/relay-install.env
set +a
cp -f /tmp/relay-linode-deploy.tgz /root/relay-linode-deploy.tgz
cd /root && rm -rf .relay-deploy-staging && mkdir .relay-deploy-staging && cd .relay-deploy-staging
tar xzf /root/relay-linode-deploy.tgz
export RELAY_BIN_SOURCE=/root/.relay-deploy-staging RELAY_INSTALL_NONINTERACTIVE=1 RELAY_SKIP_VERCEL_DNS=1
if [[ -f /opt/relay/state/features.json ]]; then ./install.sh update; else ./install.sh install; fi
rm -f /tmp/relay-linode-deploy.tgz /root/relay-install.env
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

# Seed park-tls file certs from a live peer. New VMs otherwise fall through to
# Caddy on-demand TLS for multi-A apex names (paradigmthreat.net → fleet), which
# fails ACME tls-alpn against other masters that already use park-tls file certs.
if [[ "$SKIP_CADDY" != 1 && -n "$PEER_SOURCE" && "$PEER_SOURCE" != "$FQDN" ]]; then
  echo "[deploy] seed park-tls from ${PEER_SOURCE}"
  if "${SSH[@]}" -o ConnectTimeout=15 "${SSH_USER}@${PEER_SOURCE}" \
      "sudo test -f /opt/relay/state/park-tls/Caddyfile.parked && sudo tar -C /opt/relay/state/park-tls -czf - certs Caddyfile.parked" \
      > /tmp/relay-park-tls-seed.tgz; then
    "${SCP[@]}" /tmp/relay-park-tls-seed.tgz "${SSH_TARGET}:/tmp/relay-park-tls-seed.tgz"
    "${SSH[@]}" "$SSH_TARGET" sudo bash -s <<'REMOTE'
set -euo pipefail
if [[ -f /tmp/relay-park-tls-seed.tgz ]]; then
  install -d -m 755 /opt/relay/state/park-tls
  tar -C /opt/relay/state/park-tls -xzf /tmp/relay-park-tls-seed.tgz
  chown -R relay:relay /opt/relay/state/park-tls
  chmod -R a+rX /opt/relay/state/park-tls
  rm -f /tmp/relay-park-tls-seed.tgz
  systemctl reload caddy 2>/dev/null || systemctl restart caddy
  echo "[deploy] park-tls seeded and caddy reloaded"
fi
REMOTE
    rm -f /tmp/relay-park-tls-seed.tgz
  else
    rm -f /tmp/relay-park-tls-seed.tgz
    echo "[deploy] WARN: could not seed park-tls from ${PEER_SOURCE} (peer down or no certs yet)" >&2
  fi
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

if [[ "$SKIP_SEED" != 1 ]]; then
  echo "[deploy] peer-clone PT repos from ${PEER_SOURCE}"
  REPOS=(paradigm-threat-files paradigm-threat-timeline)
  for repo in "${REPOS[@]}"; do
    "${SSH[@]}" "$SSH_TARGET" bash -s "$repo" "$PEER_SOURCE" <<'REMOTE' || true
set -euo pipefail
REPO="$1" PEER="$2"
DATA=/opt/relay/data
if [[ "$(id -u)" -eq 0 ]]; then
  sudo -u relay bash -lc "curl -fsS -X POST http://127.0.0.1:8080/api/git/ensure-bare/${REPO}" || curl -fsS -X POST "http://127.0.0.1:8080/api/git/ensure-bare/${REPO}" || true
  sudo -u relay bash -lc "cd ${DATA}/${REPO}.git && git fetch git://${PEER}:9418/${REPO}.git +refs/heads/*:refs/heads/*" || true
else
  sudo -u relay bash -lc "curl -fsS -X POST http://127.0.0.1:8080/api/git/ensure-bare/${REPO}"
  sudo -u relay bash -lc "cd ${DATA}/${REPO}.git && git fetch git://${PEER}:9418/${REPO}.git +refs/heads/*:refs/heads/*"
fi
REMOTE
  done
fi

echo "[deploy] done — run: ./scripts/relay-master-validate.sh --fqdn ${FQDN}"

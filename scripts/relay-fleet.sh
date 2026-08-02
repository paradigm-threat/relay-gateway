#!/usr/bin/env bash
# relay-fleet.sh — primary operator CLI for deploy, status, drift, and remote admin (non-interactive by default).
# Run from relay-gateway/:  ./scripts/relay-fleet.sh <subcommand> [args]
#
# Loads env from ./relay.env (gitignored) unless --env-file=PATH. Fleet hosts:
#   RELAY_OPERATOR_FLEET (preferred) or RELAY_MASTER_PEER_LIST — semicolon-separated FQDNs (quote for bash source).
#
# SSH: RELAY_SSH_USER (default ari), RELAY_SSH_IDENTITY (default ~/.ssh/id_ed25519_relay_gateway).
# Docs: docs/RELAY_FLEET_CONSOLE.md
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_FILE="${RELAY_FLEET_ENV_FILE:-$ROOT/relay.env}"
ONLY_HOST=""
BUILD=0
INTERACTIVE=0
CURL_TIMEOUT="${RELAY_FLEET_CURL_TIMEOUT:-25}"
MAX_PARALLEL="${RELAY_FLEET_MAX_PARALLEL:-4}"
GIT_PORT="${RELAY_GIT_PORT:-9418}"
DATA_ROOT="${RELAY_REPO_PATH:-/opt/relay/data}"
CATALOG_PATH="${RELAY_MASTER_CATALOG_PATH:-/opt/relay/src/relay-master}"

DANGER_WIPE=0
CONFIRM_WIPE=""

usage() {
  sed -n '1,130p' <<'EOF'
Usage: ./scripts/relay-fleet.sh [global flags] <subcommand> [args]

Global flags (before subcommand):
  --env-file=PATH     Load fleet/env from PATH (default: ./relay.env)
  --only=FQDN         Limit to one host (must match a fleet member)
  --build             With update/deploy-all: run pack-linode-deploy.sh first
  --interactive       Allow prompts (default: BatchMode, no stdin)

Trailing --build (only for update / deploy-all):
  ./relay-fleet.sh update --build   → same intent as ./relay-fleet.sh --build update

Subcommands:
  hosts               Print fleet hosts, SSH user, identity path
  ssh-check           Parallel SSH probe (fails if any host fails)
  status              POST https://<host>/api/config per host (parallel)
  health              QUERY (+ optional MCP) smoke + SSL report per host
  ssl                 Parked-domain + node HTTPS report (pre/post deploy friendly)
  drift               Compare catalog git HEAD across fleet
  update              relay-linode-redeploy-seed per host (RELAY_REDEPLOY_SKIP_SEED=1)
  deploy-all          Same as update (canonical fleet deploy entry)
  repair              Remote: sudo install.sh repair
  features-set        Remote install.sh reconfigure-features (non-interactive)
  reclone-repo REPO   Remote bare reset + catalog sync; optional --peer for git:// fetch
  fix-bare-head      Point each catalog bare repo symbolic HEAD at its .gitmodules branch (default main)
  bootstrap-ssh       Install deploy pubkey for ari via root+sshpass (SSHPASS or RELAY_ROOT_PASSWORD)
  fleet-dns           Reconcile fleet.relaygateway.net A/AAAA from node FQDNs (Vercel DNS)
  reinstall           Safe default = update. Destructive: --danger-wipe-relay-data --confirm-wipe=<fqdn>

reinstall (destructive) global flags — must appear before subcommand, with reinstall:
  --danger-wipe-relay-data   Delete bare repos under RELAY_REPO_PATH on target(s)
  --confirm-wipe=FQDN        Must match host being wiped exactly

features-set optional flags (after subcommand):
  --piper=0|1  --npm-pkgs=LIST  --translation=0|1  --translation-pkgs=LIST  --libretranslate=0|1
Or export RELAY_FEAT_* in the environment before invoking.

reclone-repo flags:
  --peer=FQDN         After sync, git fetch from git://peer:GIT_PORT/REPO.git

fleet-dns flags:
  --dry-run           Print creates/deletes without calling Vercel
  --fleet=FQDN        Override ingress name (default fleet.relaygateway.net / RELAY_FLEET_DNS_NAME)

Examples:
  ./scripts/relay-fleet.sh ssh-check
  ./scripts/relay-fleet.sh --only=atlanta3.relaygateway.net status
  ./scripts/relay-fleet.sh health
  ./scripts/relay-fleet.sh health --mcp
  ./scripts/relay-fleet.sh ssl
  ./scripts/relay-fleet.sh --build deploy-all
  ./scripts/relay-fleet.sh --only=atlanta3.relaygateway.net --build update
  ./scripts/relay-fleet.sh --only=atlanta3.relaygateway.net features-set --piper=0 --npm-pkgs=
  ./scripts/relay-fleet.sh fleet-dns
  ./scripts/relay-fleet.sh fleet-dns --dry-run
EOF
  exit "${1:-0}"
}

die() { echo "[relay-fleet] ERROR: $*" >&2; exit 1; }
log() { echo "[relay-fleet] $*" >&2; }

# --- parse global flags (including destructive reinstall flags) ---
while [[ "${1:-}" == -* ]]; do
  case "$1" in
    --env-file=*) ENV_FILE="${1#*=}" ;;
    --only=*) ONLY_HOST="${1#*=}" ;;
    --only) shift; ONLY_HOST="${1:?}" ;;
    --build) BUILD=1 ;;
    --interactive) INTERACTIVE=1 ;;
    --danger-wipe-relay-data) DANGER_WIPE=1 ;;
    --confirm-wipe=*) CONFIRM_WIPE="${1#*=}" ;;
    -h|--help|help) usage 0 ;;
    *) die "unknown flag: $1 (try --help)" ;;
  esac
  shift
done

SUB="${1:-}"; shift || true
[[ -n "$SUB" ]] || usage 1

# Ergonomic: `update --build` / `deploy-all --build` works even though global flags normally precede subcommand.
if [[ "$SUB" == update || "$SUB" == deploy-all ]] && [[ "${1:-}" == --build ]]; then
  BUILD=1
  shift || true
fi

RELAY_SSH_IDENTITY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
RELAY_SSH_USER="${RELAY_SSH_USER:-ari}"
KEY="$RELAY_SSH_IDENTITY"
[[ -f "$KEY" ]] || die "SSH private key missing: $KEY"
PUBKEY="${KEY}.pub"
[[ -f "$PUBKEY" ]] || die "SSH public key missing: $PUBKEY"

SSH_BATCH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=20)
[[ "$INTERACTIVE" -eq 1 ]] || SSH_BATCH+=( -o BatchMode=yes )
SSH_BATCH+=( -o "StrictHostKeyChecking=${RELAY_FLEET_STRICT_HOST_KEY:-accept-new}" )

SCP_BATCH=(scp -i "$KEY" -o IdentitiesOnly=yes -o "StrictHostKeyChecking=${RELAY_FLEET_STRICT_HOST_KEY:-accept-new}")
[[ "$INTERACTIVE" -eq 1 ]] || SCP_BATCH+=( -o BatchMode=yes )

source_env() {
  if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
  else
    log "WARN: env file missing ($ENV_FILE) — set RELAY_OPERATOR_FLEET or RELAY_MASTER_PEER_LIST in environment"
  fi
}

fleet_raw() {
  local r="${RELAY_OPERATOR_FLEET:-}"
  if [[ -z "$r" ]]; then
    r="${RELAY_MASTER_PEER_LIST:-}"
  fi
  r="${r//\"/}"
  r="${r//\'/}"
  echo "$r"
}

parse_hosts_to_array() {
  FLEET_HOSTS=()
  local r h
  r="$(fleet_raw)"
  [[ -n "$r" ]] || die "no fleet hosts (set RELAY_OPERATOR_FLEET or RELAY_MASTER_PEER_LIST in $ENV_FILE)"
  IFS=';' read -ra parts <<<"$r"
  for h in "${parts[@]}"; do
    h="${h//[[:space:]]/}"
    [[ -z "$h" ]] && continue
    if [[ -n "$ONLY_HOST" && "$h" != "$ONLY_HOST" ]]; then
      continue
    fi
    FLEET_HOSTS+=("$h")
  done
  [[ ${#FLEET_HOSTS[@]} -gt 0 ]] || die "fleet empty after --only=$ONLY_HOST filter"
}

ssh_target() { echo "${RELAY_SSH_USER}@$1"; }

parallel_wait_cap() {
  while [[ "$(jobs -pr 2>/dev/null | wc -l)" -ge "$MAX_PARALLEL" ]]; do
    wait -n 2>/dev/null || wait || true
  done
}

cmd_hosts() {
  source_env
  parse_hosts_to_array
  echo "ENV_FILE=$ENV_FILE"
  echo "SSH_USER=$RELAY_SSH_USER"
  echo "SSH_IDENTITY=$KEY"
  echo "HOSTS=${FLEET_HOSTS[*]}"
}

cmd_ssh_check() {
  source_env
  parse_hosts_to_array
  local h fail=0 pid
  for h in "${FLEET_HOSTS[@]}"; do
    parallel_wait_cap
    (
      echo "[relay-fleet] ssh-check begin $h"
      if "${SSH_BATCH[@]}" "$(ssh_target "$h")" 'true'; then
        echo "[relay-fleet] ssh-check ok $h"
      else
        echo "[relay-fleet] ssh-check FAIL $h"
        exit 1
      fi
    ) &
  done
  for pid in $(jobs -p); do
    wait "$pid" || fail=1
  done
  [[ "$fail" -eq 0 ]] || exit 1
}

_one_status() {
  local h="$1" url out
  url="https://${h}/api/config"
  echo "[relay-fleet] status begin $h ($url)"
  if ! out="$(curl -fsS -m "$CURL_TIMEOUT" -X POST "$url" 2>&1)"; then
    echo "[relay-fleet] status FAIL $h: $out"
    return 1
  fi
  printf '%s' "$out" | python3 -c 'import json,sys
d=json.load(sys.stdin)
s=d.get("startup_catalog_sync") or {}
print("  relay_server_id:", d.get("relay_server_id"))
print("  catalog_forge_sync_ok:", s.get("catalog_forge_sync_ok"))
ar=d.get("authorized_repos") or []
print("  authorized_repos:", ",".join(ar)[:220])'
  echo "[relay-fleet] status end $h"
}

cmd_status() {
  source_env
  parse_hosts_to_array
  local h fail=0
  for h in "${FLEET_HOSTS[@]}"; do
    parallel_wait_cap
    ( _one_status "$h" ) &
  done
  for pid in $(jobs -p); do
    wait "$pid" || fail=1
  done
  [[ "$fail" -eq 0 ]] || exit 1
}

# Parked Host + node FQDN HTTPS report (scripts/relay-fleet-ssl-check.sh).
# mode=strict → exit 1 on FAIL; mode=warn → always continue (deploy soft gate).
# Env: RELAY_FLEET_SSL_DOMAINS, RELAY_FLEET_SSL_STRICT, RELAY_FLEET_SSL_GATE (deploy hard-fail).
run_fleet_ssl() {
  local label="${1:-ssl}" mode="${2:-strict}"
  local script="$ROOT/scripts/relay-fleet-ssl-check.sh"
  [[ -f "$script" ]] || die "missing $script"
  export RELAY_FLEET_SSL_LABEL="$label"
  # Prefer current --only / FLEET_HOSTS over raw env peer list.
  export RELAY_FLEET_SSL_NODES="${FLEET_HOSTS[*]}"
  set +e
  "$script"
  local rc=$?
  set -e
  if [[ "$rc" -eq 0 ]]; then
    return 0
  fi
  if [[ "$mode" == warn ]]; then
    echo "[relay-fleet] WARN SSL check incomplete ($label) — deploy continues; set RELAY_FLEET_SSL_GATE=1 to hard-fail" >&2
    return 0
  fi
  return "$rc"
}

cmd_ssl() {
  source_env
  parse_hosts_to_array
  local arg
  for arg in "$@"; do
    case "$arg" in
      -h|--help)
        echo "usage: relay-fleet.sh ssl"
        echo "  Env: RELAY_FLEET_SSL_DOMAINS RELAY_FLEET_SSL_STRICT=1 RELAY_FLEET_SSL_TIMEOUT"
        return 0
        ;;
      *) die "ssl: unknown arg: $arg" ;;
    esac
  done
  run_fleet_ssl "ssl" strict
}

# QUERY/MCP smoke against live HTTPS apex. Repos default to PT files + timeline.
# Optional: health --mcp   also run validate-relay-mcp.mjs per host/repo
# Env: RELAY_FLEET_HEALTH_REPOS="paradigm-threat-files paradigm-threat-timeline"
#      RELAY_FLEET_HEALTH_QUERY=venus
#      RELAY_FLEET_HEALTH_SSL=0 to skip park/node HTTPS report (default on)
cmd_health() {
  source_env
  parse_hosts_to_array
  local with_mcp=0 arg
  for arg in "$@"; do
    case "$arg" in
      --mcp) with_mcp=1 ;;
      -h|--help) echo "usage: relay-fleet.sh health [--mcp]"; return 0 ;;
      *) die "health: unknown arg: $arg (try --mcp)" ;;
    esac
  done
  local q_script="$ROOT/scripts/validate-relay-query.mjs"
  local m_script="$ROOT/scripts/validate-relay-mcp.mjs"
  [[ -f "$q_script" ]] || die "missing $q_script"
  command -v node >/dev/null || die "node required for health (validate-relay-query.mjs)"
  local repos="${RELAY_FLEET_HEALTH_REPOS:-paradigm-threat-files paradigm-threat-timeline}"
  local q="${RELAY_FLEET_HEALTH_QUERY:-venus}"
  local h repo fail=0 base
  for h in "${FLEET_HOSTS[@]}"; do
    base="https://${h}"
    echo "[relay-fleet] health begin $h"
    for repo in $repos; do
      echo "  QUERY $repo q=$q"
      if ! node "$q_script" "$base" "$repo" "$q"; then
        echo "[relay-fleet] health FAIL QUERY $h $repo"
        fail=1
      fi
      if [[ "$with_mcp" -eq 1 ]]; then
        [[ -f "$m_script" ]] || die "missing $m_script"
        echo "  MCP $repo"
        if ! node "$m_script" "$base" "$repo"; then
          echo "[relay-fleet] health FAIL MCP $h $repo"
          fail=1
        fi
      fi
    done
    echo "[relay-fleet] health end $h"
  done
  case "${RELAY_FLEET_HEALTH_SSL:-1}" in
    0|no|false|off) ;;
    *)
      if ! run_fleet_ssl "health" strict; then
        fail=1
      fi
      ;;
  esac
  [[ "$fail" -eq 0 ]] || exit 1
}

cmd_drift() {
  source_env
  parse_hosts_to_array
  declare -A heads
  local h line v ref mismatch=0
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] drift probe $h …"
    line="$("${SSH_BATCH[@]}" "$(ssh_target "$h")" bash -s "$CATALOG_PATH" <<'EOS'
set -euo pipefail
C="$1"
if [[ ! -e "$C/.git" ]]; then echo "NO_CATALOG"; exit 0; fi
sudo git -c "safe.directory=$C" -C "$C" rev-parse HEAD 2>/dev/null || echo "NO_HEAD"
EOS
)"
    heads["$h"]="$line"
    echo "  catalog_HEAD $h -> $line"
  done
  ref=""
  for h in "${FLEET_HOSTS[@]}"; do
    v="${heads[$h]:-}"
    [[ "$v" == NO_* ]] && continue
    [[ -z "$ref" ]] && ref="$v"
    if [[ -n "$ref" && "$v" != "$ref" ]]; then
      echo "[relay-fleet] DRIFT: $h has $v (ref $ref)"
      mismatch=1
    fi
  done
  if [[ "$mismatch" -eq 0 ]]; then
    echo "[relay-fleet] drift: catalog HEAD aligned for compared hosts."
  fi
}

default_tgz() { echo "$ROOT/target/relay-linode-deploy.tgz"; }

write_mesh_install_env() {
  local dest="$1"
  [[ -n "${RELAY_MASTER_PEER_LIST:-}${RELAY_PEER_SECRET:-}${RELAY_MASTER_MESH_SECRET:-}" ]] || return 1
  {
    printf 'RELAY_MERGE_MESH_ENV=1\n'
    [[ -n "${RELAY_MASTER_PEER_LIST:-}" ]] && printf 'RELAY_MASTER_PEER_LIST="%s"\n' "$RELAY_MASTER_PEER_LIST"
    [[ -n "${RELAY_PEER_SECRET:-}" ]] && printf 'RELAY_PEER_SECRET=%s\n' "$RELAY_PEER_SECRET"
    [[ -n "${RELAY_MASTER_MESH_SECRET:-}" ]] && printf 'RELAY_MASTER_MESH_SECRET=%s\n' "$RELAY_MASTER_MESH_SECRET"
  } >"$dest"
  chmod 600 "$dest"
}

# Run install.sh repair|reconfigure-features from a fresh tarball extract (install.sh is not shipped under /opt/relay/bin).
run_remote_install_cmd() {
  local h="$1" mode="$2" feat_env_local="${3:-}" tgz staging rf_remote mesh_env=""
  tgz="$(default_tgz)"
  if [[ ! -f "$tgz" ]]; then
    log "packing tarball for remote $mode …"
    "$ROOT/scripts/pack-linode-deploy.sh"
  fi
  [[ -f "$tgz" ]] || die "missing tarball: $tgz"
  staging="relay-fleet-st-$$"
  rf_remote="/tmp/${staging}.tgz"
  "${SCP_BATCH[@]}" "$tgz" "$(ssh_target "$h"):$rf_remote"
  if [[ -n "$feat_env_local" && -f "$feat_env_local" ]]; then
    "${SCP_BATCH[@]}" "$feat_env_local" "$(ssh_target "$h"):/tmp/${staging}.feat.env"
  fi
  mesh_env="$(mktemp)"
  if write_mesh_install_env "$mesh_env"; then
    "${SCP_BATCH[@]}" "$mesh_env" "$(ssh_target "$h"):/tmp/${staging}.mesh.env"
  else
    rm -f "$mesh_env"
    mesh_env=""
  fi
  "${SSH_BATCH[@]}" "$(ssh_target "$h")" bash -s "$staging" "$rf_remote" "$mode" <<'EOS'
set -euo pipefail
ST="$1" TGZ="$2" MODE="$3"
FEAT="/tmp/${ST}.feat.env"
MESH="/tmp/${ST}.mesh.env"
sudo rm -rf "/tmp/$ST"
sudo mkdir "/tmp/$ST"
sudo tar xzf "$TGZ" -C "/tmp/$ST"
sudo rm -f "$TGZ"
export RELAY_BIN_SOURCE="/tmp/$ST"
export RELAY_INSTALL_NONINTERACTIVE=1
sudo bash -c "
  set -euo pipefail
  set -a
  [[ -f \"$MESH\" ]] && . \"$MESH\"
  [[ -f \"$FEAT\" ]] && . \"$FEAT\"
  set +a
  export RELAY_INSTALL_NONINTERACTIVE=1 RELAY_BIN_SOURCE=\"$RELAY_BIN_SOURCE\"
  cd \"$RELAY_BIN_SOURCE\" && ./install.sh \"$MODE\"
"
sudo rm -f "$FEAT" "$MESH"
sudo rm -rf "/tmp/$ST"
EOS
  [[ -n "$mesh_env" && -f "$mesh_env" ]] && rm -f "$mesh_env"
}

cmd_update() {
  source_env
  parse_hosts_to_array
  local h tgz health_gate ssl_mode
  health_gate="${RELAY_FLEET_UPDATE_HEALTH:-1}"
  case "${RELAY_FLEET_SSL_GATE:-0}" in
    1|yes|true|on) ssl_mode=strict ;;
    *) ssl_mode=warn ;;
  esac
  tgz="$(default_tgz)"
  if [[ "$BUILD" -eq 1 || ! -f "$tgz" ]]; then
    log "running pack-linode-deploy.sh …"
    "$ROOT/scripts/pack-linode-deploy.sh"
  fi
  [[ -f "$tgz" ]] || die "missing tarball: $tgz"
  echo "[relay-fleet] pre-deploy SSL check (mode=$ssl_mode)"
  run_fleet_ssl "pre-deploy" "$ssl_mode"
  export RELAY_REDEPLOY_SKIP_SEED=1
  export RELAY_SSH_IDENTITY="$KEY"
  export RELAY_SSH_USER
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] update begin $h"
    "$ROOT/scripts/relay-linode-redeploy-seed.sh" "$h" "$tgz"
    echo "[relay-fleet] restart relay-gateway relay-git-daemon $h"
    "${SSH_BATCH[@]}" "$(ssh_target "$h")" 'sudo systemctl restart relay-gateway relay-git-daemon'
    case "${health_gate,,}" in
      0|no|false|off) ;;
      *)
        echo "[relay-fleet] post-update health gate $h (POST /api/config)"
        local curl_t="${RELAY_FLEET_CURL_TIMEOUT:-25}"
        if ! curl -sfS --max-time "$curl_t" -X POST "https://${h}/api/config" >/dev/null; then
          die "post-update health gate failed for $h — fix node before continuing (set RELAY_FLEET_UPDATE_HEALTH=0 to skip)"
        fi
        echo "[relay-fleet] health gate OK $h"
        ;;
    esac
    echo "[relay-fleet] update end $h"
  done
  echo "[relay-fleet] post-deploy SSL check (mode=$ssl_mode)"
  run_fleet_ssl "post-deploy" "$ssl_mode"
}

cmd_deploy_all() { cmd_update; }

cmd_repair() {
  source_env
  parse_hosts_to_array
  local h
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] repair begin $h"
    run_remote_install_cmd "$h" repair ""
    echo "[relay-fleet] repair end $h"
  done
}

cmd_features_set() {
  source_env
  parse_hosts_to_array
  export RELAY_INSTALL_NONINTERACTIVE=1
  while [[ "${1:-}" == --* ]]; do
    case "$1" in
      --piper=*) export "RELAY_FEAT_PIPER=${1#*=}" ;;
      --npm-pkgs=*) export "RELAY_FEAT_NPM_PKGS=${1#*=}" ;;
      --translation=*) export "RELAY_FEAT_TRANSLATION=${1#*=}" ;;
      --translation-pkgs=*) export "RELAY_FEAT_TRANSLATION_PKGS=${1#*=}" ;;
      --libretranslate=*) export "RELAY_FEAT_LIBRETRANSLATE=${1#*=}" ;;
      *) die "unknown features-set flag: $1" ;;
    esac
    shift
  done
  : "${RELAY_FEAT_PIPER:=0}"
  : "${RELAY_FEAT_TRANSLATION:=0}"
  : "${RELAY_FEAT_LIBRETRANSLATE:=0}"
  export RELAY_FEAT_PIPER RELAY_FEAT_TRANSLATION RELAY_FEAT_LIBRETRANSLATE
  [[ -n "${RELAY_FEAT_NPM_PKGS+x}" ]] || export RELAY_FEAT_NPM_PKGS=""
  local h rf
  rf=$(mktemp)
  {
    echo "RELAY_INSTALL_NONINTERACTIVE=1"
    env | grep -E '^(RELAY_FEAT_|RELAY_LIBRETRANSLATE_)=' || true
  } >"$rf"
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] features-set begin $h"
    run_remote_install_cmd "$h" reconfigure-features "$rf"
    echo "[relay-fleet] features-set end $h"
  done
  rm -f "$rf"
}

cmd_reclone_repo() {
  source_env
  parse_hosts_to_array
  local repo="${1:?Usage: reclone-repo <repo> [--peer=FQDN]}"; shift
  local peer=""
  while [[ "${1:-}" == --* ]]; do
    case "$1" in
      --peer=*) peer="${1#*=}" ;;
      *) die "unknown reclone-repo flag: $1" ;;
    esac
    shift
  done
  local h
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] reclone-repo $repo on $h …"
    "${SSH_BATCH[@]}" "$(ssh_target "$h")" \
      bash -s -- "$DATA_ROOT" "$GIT_PORT" "$repo" "${peer:-}" \
      < "$ROOT/scripts/relay-reclone-bare.sh"
    echo "[relay-fleet] reclone-repo end $h"
  done
}

cmd_bootstrap_ssh() {
  local pw="${RELAY_ROOT_PASSWORD:-${SSHPASS:-}}"
  [[ -n "$pw" ]] || die "set RELAY_ROOT_PASSWORD or SSHPASS for bootstrap-ssh (never commit)"
  command -v sshpass >/dev/null || die "sshpass not installed (apt install sshpass)"
  source_env
  parse_hosts_to_array
  local h line pk
  pk="$(cat "$PUBKEY")"
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] bootstrap-ssh begin $h (root password auth)"
    line="$(SSHPASS="$pw" sshpass -e ssh -o PreferredAuthentications=password -o PubkeyAuthentication=no \
      -o "StrictHostKeyChecking=${RELAY_FLEET_STRICT_HOST_KEY:-accept-new}" -o ConnectTimeout=20 "root@$h" bash -s "$RELAY_SSH_USER" "$pk" <<'REMOTE'
set -euo pipefail
U="$1" LINE="$2"
install -d -m 755 "/home/$U" || true
install -d -m 700 "/home/$U/.ssh"
AK="/home/$U/.ssh/authorized_keys"
if [[ -f "$AK" ]] && grep -qF "$LINE" "$AK" 2>/dev/null; then
  echo "key_already_present"
else
  printf '%s\n' "$LINE" >>"$AK"
fi
chmod 600 "$AK"
chown -R "$U:$U" "/home/$U/.ssh"
getent group sudo >/dev/null && usermod -aG sudo "$U" 2>/dev/null || true
echo ok
REMOTE
)"
    echo "[relay-fleet] bootstrap-ssh root session: $line"
    echo "[relay-fleet] bootstrap-ssh end $h"
  done
  unset RELAY_ROOT_PASSWORD SSHPASS
  cmd_ssh_check
}

cmd_reinstall() {
  source_env
  parse_hosts_to_array
  if [[ "$DANGER_WIPE" -eq 1 ]]; then
    [[ -n "$CONFIRM_WIPE" ]] || die "destructive reinstall requires --confirm-wipe=<exact-fqdn>"
    [[ ${#FLEET_HOSTS[@]} -eq 1 ]] || die "destructive reinstall: use --only=<one-host> so exactly one target is selected"
    local h
    for h in "${FLEET_HOSTS[@]}"; do
      [[ "$h" == "$CONFIRM_WIPE" ]] || die "CONFIRM_WIPE ($CONFIRM_WIPE) must match --only host ($h)"
      echo "[relay-fleet] DANGER: wiping bare repos under $DATA_ROOT on $h"
      "${SSH_BATCH[@]}" "$(ssh_target "$h")" bash -s "$DATA_ROOT" <<'EOS'
set -euo pipefail
DR="$1"
sudo systemctl stop relay-gateway relay-git-daemon || true
sudo rm -rf "${DR}"/*.git
echo wiped
EOS
    done
  fi
  cmd_update
}

cmd_fix_bare_head() {
  source_env
  parse_hosts_to_array
  local h
  for h in "${FLEET_HOSTS[@]}"; do
    echo "[relay-fleet] fix-bare-head begin $h"
    "${SSH_BATCH[@]}" "$(ssh_target "$h")" bash -s "$DATA_ROOT" "$CATALOG_PATH" <<'EOS'
set -euo pipefail
DATA_ROOT="$1" CATALOG="$2"
sudo -u relay env DATA_ROOT="$DATA_ROOT" CATALOG="$CATALOG" bash -s <<'INNER'
set -euo pipefail
cd "$CATALOG"
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  branch=$(git config --file .gitmodules --get "submodule.${name}.branch" 2>/dev/null || echo main)
  bare="${DATA_ROOT}/${name}.git"
  [[ -d "$bare" ]] || continue
  ref="refs/heads/${branch}"
  if ! git --git-dir="$bare" rev-parse --verify "$ref" >/dev/null 2>&1; then
    echo "skip ${name}: missing ${ref}" >&2
    continue
  fi
  current=$(git --git-dir="$bare" symbolic-ref -q HEAD 2>/dev/null || true)
  if [[ "$current" != "$ref" ]]; then
    git --git-dir="$bare" symbolic-ref HEAD "$ref"
    echo "fixed ${name}: HEAD -> ${ref} (was ${current:-<unset>})"
  else
    echo "ok ${name}: HEAD already ${ref}"
  fi
done < <(git config --file .gitmodules --name-only --get-regexp '^submodule\..*\.path$' | sed -n 's/^submodule\.\(.*\)\.path$/\1/p' | sort -u)
INNER
sudo systemctl restart relay-gateway relay-git-daemon 2>/dev/null || true
EOS
    echo "[relay-fleet] fix-bare-head end $h"
  done
}

cmd_fleet_dns() {
  source_env
  local dry=0
  local fleet_name="${RELAY_FLEET_DNS_NAME:-fleet.relaygateway.net}"
  local args=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run) dry=1 ;;
      --fleet=*) fleet_name="${1#*=}" ;;
      --fleet) shift; fleet_name="${1:?}" ;;
      -h|--help) usage 0 ;;
      *) die "fleet-dns: unknown arg: $1" ;;
    esac
    shift
  done
  [[ -n "${VERCEL_API_TOKEN:-${VERCEL_TOKEN:-}}" ]] || die "fleet-dns needs VERCEL_API_TOKEN in $ENV_FILE or environment"
  # Always reconcile from the full configured fleet, ignoring --only.
  local saved_only="$ONLY_HOST"
  ONLY_HOST=""
  parse_hosts_to_array
  ONLY_HOST="$saved_only"
  [[ "$dry" == 1 ]] && args+=(--dry-run)
  args+=(--fleet="$fleet_name")
  args+=("${FLEET_HOSTS[@]}")
  log "fleet-dns begin ${fleet_name} from ${FLEET_HOSTS[*]}"
  "$ROOT/terraform/shared-dns/scripts/vercel-dns-fleet-reconcile.sh" "${args[@]}"
  log "fleet-dns end"
}

case "$SUB" in
  hosts) cmd_hosts ;;
  ssh-check) cmd_ssh_check ;;
  status) cmd_status ;;
  health) cmd_health "$@" ;;
  ssl) cmd_ssl "$@" ;;
  drift) cmd_drift ;;
  update) cmd_update ;;
  deploy-all) cmd_deploy_all ;;
  repair) cmd_repair ;;
  features-set) cmd_features_set "$@" ;;
  reclone-repo) cmd_reclone_repo "$@" ;;
  fix-bare-head) cmd_fix_bare_head ;;
  bootstrap-ssh) cmd_bootstrap_ssh ;;
  fleet-dns) cmd_fleet_dns "$@" ;;
  reinstall) cmd_reinstall ;;
  -h|--help|help) usage 0 ;;
  *) die "unknown subcommand: $SUB (try ./scripts/relay-fleet.sh help)" ;;
esac

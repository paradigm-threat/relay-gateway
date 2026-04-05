#!/usr/bin/env bash
# Pull relay-master default branch, verify signed commits + manifest, download relay-gateway
# tarball (optional), run install.sh update. See docs/RELAY_RECONCILE_PLAN.md.
#
# Environment (systemd EnvironmentFile=-/opt/relay/relay.env):
#   RELAY_RECONCILE_ENABLED=1          required to do work
#   RELAY_INSTALL_ROOT=/opt/relay
#   RELAY_MASTER_CATALOG_PATH=...      worktree with relay-master
#   RELAY_MASTER_CATALOG_BRANCH=main
#   RELAY_PUBLIC_HOSTNAME=...          for canary_hostname gate
#
set -euo pipefail

INSTALL="${RELAY_INSTALL_ROOT:-/opt/relay}"
STATE_DIR="$INSTALL/state"
STATE_FILE="$STATE_DIR/last_reconcile.json"
ALLOWLIST="$INSTALL/etc/reconcile-allowlist"
MANIFEST_REL="deploy/relay-gateway.json"
CANARY_REL="deploy/canary-complete.json"
LOG_TAG="[relay-reconcile]"

log() { echo "$LOG_TAG $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

[[ "$(id -u)" -eq 0 ]] || die "run as root"

if [[ "${RELAY_RECONCILE_ENABLED:-0}" != "1" ]]; then
  log "RELAY_RECONCILE_ENABLED is not 1 — skipping"
  exit 0
fi

command -v jq >/dev/null || die "jq required"
command -v curl >/dev/null || die "curl required"
command -v git >/dev/null || die "git required"

CATALOG="${RELAY_MASTER_CATALOG_PATH:-$INSTALL/src/relay-master}"
BRANCH="${RELAY_MASTER_CATALOG_BRANCH:-main}"

[[ -d "$CATALOG/.git" ]] || die "not a git catalog: $CATALOG"

mkdir -p "$STATE_DIR" "$INSTALL/etc"
[[ -f "$ALLOWLIST" ]] || {
  printf '# path<TAB>sha256 — see docs/RELAY_RECONCILE_PLAN.md\n' >"$ALLOWLIST"
  chmod 600 "$ALLOWLIST"
  chown root:root "$ALLOWLIST"
}

run_git() {
  if command -v runuser >/dev/null 2>&1; then
    runuser -u relay -- git -C "$CATALOG" "$@"
  else
    sudo -u relay git -C "$CATALOG" "$@"
  fi
}

before="$(run_git rev-parse HEAD)"

run_git fetch origin "$BRANCH" || die "git fetch failed"
run_git merge --ff-only "origin/$BRANCH" || die "git merge --ff-only failed (local changes or non-FF?)"

after="$(run_git rev-parse HEAD)"

if [[ "$before" != "$after" ]]; then
  for c in $(run_git rev-list --reverse "${before}..${after}"); do
    if ! run_git verify-commit "$c" 2>/dev/null; then
      die "commit not verifiable (signing policy): $c — fix catalog or signing setup"
    fi
  done
else
  if ! run_git verify-commit HEAD 2>/dev/null; then
    die "HEAD not verifiable (signing policy) — fix catalog or signing setup"
  fi
fi

mf="$CATALOG/$MANIFEST_REL"
[[ -f "$mf" ]] || { log "no manifest $MANIFEST_REL — nothing to apply"; exit 0; }

git_tag="$(jq -r '.relayGateway.git_tag // empty' "$mf")"
artifact_url="$(jq -r '.relayGateway.artifact_url // empty' "$mf")"
artifact_sha256="$(jq -r '.relayGateway.artifact_sha256 // empty' "$mf" | tr '[:upper:]' '[:lower:]')"
reconcile_script="$(jq -r '.relayGateway.reconcile_script // empty' "$mf")"
canary_hostname="$(jq -r '.relayGateway.canary_hostname // empty' "$mf")"
skip_binary="$(jq -r '.relayGateway.skip_binary_download // false' "$mf")"

if [[ -z "${artifact_sha256:-}" ]]; then
  log "relayGateway.artifact_sha256 empty — configure $MANIFEST_REL to enable binary reconcile"
  exit 0
fi

HOST="${RELAY_PUBLIC_HOSTNAME:-}"
if [[ -n "$canary_hostname" && -n "$HOST" && "$canary_hostname" != "$HOST" ]]; then
  canary_file="$CATALOG/$CANARY_REL"
  if [[ ! -f "$canary_file" ]]; then
    log "canary_hostname=$canary_hostname (this host=$HOST) — waiting for $CANARY_REL in repo"
    exit 0
  fi
  cc_sha="$(jq -r '.artifact_sha256 // empty' "$canary_file" | tr '[:upper:]' '[:lower:]')"
  if [[ "$cc_sha" != "$artifact_sha256" ]]; then
    log "canary-complete artifact_sha256 mismatch — waiting for fleet approval in $CANARY_REL"
    exit 0
  fi
fi

# Idempotent: skip script + download if this catalog tip + artifact pin already applied
if [[ -f "$STATE_FILE" ]]; then
  st_head="$(jq -r '.catalog_head // empty' "$STATE_FILE")"
  st_sha="$(jq -r '.artifact_sha256 // empty' "$STATE_FILE" | tr '[:upper:]' '[:lower:]')"
  if [[ "$st_head" == "$after" && "$st_sha" == "$artifact_sha256" ]]; then
    log "already applied catalog $after artifact $artifact_sha256"
    exit 0
  fi
fi

# Optional script path (must match allowlist + git blob hash)
if [[ -n "$reconcile_script" ]]; then
  line="$(awk -F'\t' -v p="$reconcile_script" '$1 == p {print}' "$ALLOWLIST" 2>/dev/null | head -1)"
  [[ -n "$line" ]] || die "reconcile_script $reconcile_script not in $ALLOWLIST"
  want_hash="$(echo "$line" | awk -F'\t' '{print $2}' | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
  [[ -n "$want_hash" ]] || die "allowlist line missing sha256 for $reconcile_script"
  got_hash="$(run_git show "HEAD:${reconcile_script}" | sha256sum | awk '{print $1}')"
  if [[ "$got_hash" != "$want_hash" ]]; then
    die "reconcile_script blob sha256 mismatch: want $want_hash got $got_hash"
  fi
  log "running reconcile_script $reconcile_script"
  export RELAY_INSTALL_ROOT="$INSTALL"
  ( cd "$CATALOG" && bash "./$reconcile_script" ) || die "reconcile_script failed"
fi

if [[ "$skip_binary" == "true" ]]; then
  [[ -n "$reconcile_script" ]] || die "skip_binary_download true requires reconcile_script"
  ts="$(date -Iseconds 2>/dev/null || date -u)"
  jq -n \
    --arg head "$after" \
    --arg sha "$artifact_sha256" \
    --arg tag "$git_tag" \
    --arg ts "$ts" \
    '{catalog_head:$head, artifact_sha256:$sha, git_tag:$tag, applied_at:$ts, mode:"script_only"}' >"$STATE_FILE"
  chmod 600 "$STATE_FILE"
  log "skip_binary_download — recorded state catalog=$after"
  exit 0
fi

if [[ -z "${artifact_url:-}" ]]; then
  die "relayGateway.artifact_url empty — set URL or set skip_binary_download with reconcile_script"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/relay-linode-deploy.tgz" "$artifact_url"
got="$(sha256sum "$tmp/relay-linode-deploy.tgz" | awk '{print $1}' | tr '[:upper:]' '[:lower:]')"
if [[ "$got" != "$artifact_sha256" ]]; then
  die "sha256 mismatch: manifest $artifact_sha256 actual $got"
fi

mkdir -p "$tmp/staging"
tar -xzf "$tmp/relay-linode-deploy.tgz" -C "$tmp/staging"

export RELAY_BIN_SOURCE="$tmp/staging"
export RELAY_INSTALL_NONINTERACTIVE=1
INS_SCRIPT="$tmp/staging/install.sh"
[[ -f "$INS_SCRIPT" ]] || die "missing install.sh inside tarball"
chmod +x "$INS_SCRIPT"
"$INS_SCRIPT" update

ts="$(date -Iseconds 2>/dev/null || date -u)"
jq -n \
  --arg head "$after" \
  --arg sha "$artifact_sha256" \
  --arg tag "$git_tag" \
  --arg ts "$ts" \
  '{catalog_head:$head, artifact_sha256:$sha, git_tag:$tag, applied_at:$ts}' >"$STATE_FILE"
chmod 600 "$STATE_FILE"
log "applied $git_tag catalog=$after"

exit 0

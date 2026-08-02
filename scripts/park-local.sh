#!/usr/bin/env bash
# Local relay.paradigmthreat.net park — prepare / sync / serve / smoke / regress.
# Heavy GET/QUERY/image work stays in Rust; this loop hardens tip CleverScript + gateway.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MAP_FILE="${RELAY_LOCAL_REPOS_MAP:-$SCRIPT_DIR/relay-local-repos.map}"

PARK_ROOT="${RELAY_PARK_LOCAL_ROOT:-$GW_DIR/docker-data/park-local}"
DATA_DIR="$PARK_ROOT/data"
STATE_DIR="$PARK_ROOT/state"
HTTP_PORT="${RELAY_HTTP_PORT:-9080}"
PARK_HOST="${SSR_SMOKE_HOST:-relay.paradigmthreat.net}"
FILES_REPO="${RELAY_PARK_FILES_REPO:-paradigm-threat-files}"
ASSETS_REPO="${RELAY_WEB_ASSETS_REPO:-relay-web-assets}"

log() { echo "[park-local] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

map_path() {
  local name="$1"
  [[ -f "$MAP_FILE" ]] || die "missing repo map: $MAP_FILE"
  awk -v n="$name" '$1 == n { print $2; exit }' "$MAP_FILE"
}

FILES_CHECKOUT="${RELAY_PARK_FILES_CHECKOUT:-$(map_path "$FILES_REPO")}"
ASSETS_CHECKOUT="${RELAY_PARK_ASSETS_CHECKOUT:-$(map_path "$ASSETS_REPO")}"
SMOKE_SCRIPT="${RELAY_PARK_SMOKE_SCRIPT:-$FILES_CHECKOUT/scripts/ssr-smoke.sh}"

ensure_dirs() {
  mkdir -p "$DATA_DIR" "$STATE_DIR"
}

write_parked_map() {
  ensure_dirs
  cat >"$STATE_DIR/parked-domains.json" <<EOF
{
  "${PARK_HOST}": "${FILES_REPO}"
}
EOF
  printf '%s\n' "# park-local Host allowlist" "$PARK_HOST" >"$STATE_DIR/parked-domains.txt"
  log "Park map: ${PARK_HOST} → ${FILES_REPO}"
}

ensure_bare_from_checkout() {
  local name="$1"
  local checkout="$2"
  local bare="$DATA_DIR/${name}.git"
  [[ -d "$checkout/.git" || -f "$checkout/HEAD" ]] || die "checkout missing for $name: $checkout"
  if [[ ! -d "$bare" ]]; then
    log "Init bare $bare from $checkout"
    git clone --bare "$checkout" "$bare"
  fi
  # Local park bares are disposable mirrors — allow non-FF tip refresh.
  git --git-dir="$bare" config receive.denyNonFastForwards false || true
  git --git-dir="$bare" config receive.denyCurrentBranch ignore || true
}

# Push worktree snapshot (committed + uncommitted + untracked) into bare main without
# touching the operator's index. Uses a temp GIT_INDEX_FILE.
sync_worktree_to_bare() {
  local name="$1"
  local checkout="$2"
  local bare="$DATA_DIR/${name}.git"
  local branch="${3:-main}"
  ensure_bare_from_checkout "$name" "$checkout"

  local tmpindex status=0
  tmpindex="$(mktemp)"
  (
    cd "$checkout"
    export GIT_INDEX_FILE="$tmpindex"
    git read-tree HEAD
    git add -A
    local tree parent commit
    tree="$(git write-tree)"
    parent="$(git rev-parse HEAD)"
    commit="$(git commit-tree "$tree" -p "$parent" -m "park-local: worktree snapshot")"
    git push --force "$bare" "${commit}:refs/heads/${branch}"
    log "$name ${branch} → $(git --git-dir="$bare" rev-parse --short "${branch}") (from worktree)"
  ) || status=$?
  rm -f "$tmpindex"
  return "$status"
}

cmd_prepare() {
  ensure_dirs
  write_parked_map
  [[ -n "$FILES_CHECKOUT" ]] || die "no path for $FILES_REPO in $MAP_FILE"
  ensure_bare_from_checkout "$FILES_REPO" "$FILES_CHECKOUT"
  if [[ -n "${ASSETS_CHECKOUT:-}" && -d "$ASSETS_CHECKOUT/.git" ]]; then
    ensure_bare_from_checkout "$ASSETS_REPO" "$ASSETS_CHECKOUT"
  else
    log "WARN: $ASSETS_REPO checkout missing — SSR embed pins may fall back"
  fi
  log "Prepared under $PARK_ROOT"
}

cmd_sync() {
  cmd_prepare
  sync_worktree_to_bare "$FILES_REPO" "$FILES_CHECKOUT" main
  if [[ -n "${ASSETS_CHECKOUT:-}" && -d "$ASSETS_CHECKOUT/.git" ]]; then
    sync_worktree_to_bare "$ASSETS_REPO" "$ASSETS_CHECKOUT" main
  fi
}

gateway_env() {
  export RELAY_REPO_PATH="$DATA_DIR"
  export RELAY_INSTALL_ROOT="$PARK_ROOT"
  export RELAY_PARKED_DOMAINS_MAP_PATH="$STATE_DIR/parked-domains.json"
  export RELAY_PARKED_DOMAINS_PATH="$STATE_DIR/parked-domains.txt"
  export RELAY_HTTP_PORT="$HTTP_PORT"
  export RELAY_PUBLIC_HOSTNAME="${RELAY_PUBLIC_HOSTNAME:-localhost}"
  # Local park: no fleet allowlist / trust root — tip is the operator's mirror.
  unset RELAY_AUTHORIZED_REPOS_PATH RELAY_MASTER_CATALOG_PATH RELAY_SERVER_ID || true
}

ensure_binary() {
  if [[ ! -x "$GW_DIR/target/release/relay-gateway" ]]; then
    log "Building release relay-gateway…"
    (cd "$GW_DIR" && cargo build --release --bin relay-gateway --bin relay-hook-handler)
  fi
}

cmd_serve() {
  cmd_sync
  ensure_binary
  gateway_env
  log "Serving park on http://127.0.0.1:${HTTP_PORT}/  (Host: ${PARK_HOST})"
  log "Smoke: ./scripts/park-local.sh smoke"
  cd "$GW_DIR"
  exec ./target/release/relay-gateway serve --repo "$DATA_DIR"
}

cmd_smoke() {
  gateway_env
  [[ -x "$SMOKE_SCRIPT" || -f "$SMOKE_SCRIPT" ]] || die "smoke script missing: $SMOKE_SCRIPT"
  if ! curl -sf --max-time 2 -X POST "http://127.0.0.1:${HTTP_PORT}/api/config" >/dev/null; then
    die "gateway not up on :${HTTP_PORT} — run: ./scripts/park-local.sh serve"
  fi
  SSR_SMOKE_HOST="$PARK_HOST" SSR_SMOKE_PORT="$HTTP_PORT" bash "$SMOKE_SCRIPT"
}

cmd_unit() {
  log "relay-runtime host helpers + paradigm fixture render"
  (cd "$GW_DIR/../relay-runtime" && cargo test --locked \
    --test stdlib_relay_modules \
    --test paradigm_files_render \
    --test json_builtins \
    --test cleverscript_sugar)
  log "relay-gateway park Host / SSR failure regressions"
  (cd "$GW_DIR" && cargo test --locked --lib parked -- --nocapture)
  (cd "$GW_DIR" && cargo test --locked --lib unknown_host_does_not_fall_back -- --nocapture)
}

cmd_regress() {
  cmd_unit
  log "Unit park regressions OK. For live SSR: ./scripts/park-local.sh serve  # other terminal: smoke"
}

cmd_help() {
  cat <<EOF
Usage: $0 <command>

  prepare   Bare mirrors + parked-domains map under docker-data/park-local/
  sync      prepare + push worktree snapshot (incl. dirty) → bare main
  serve     sync + run relay-gateway on :${HTTP_PORT} with Host ${PARK_HOST}
  smoke     ./scripts/ssr-smoke.sh against local gateway (gateway must be up)
  unit      cargo tests: @relay/* helpers + park Host/SSR regressions
  regress   unit only (no live server)
  help      this text

Env:
  RELAY_PARK_LOCAL_ROOT     default: <gateway>/docker-data/park-local
  RELAY_HTTP_PORT           default: 9080 (avoids host Kubo on :8080)
  SSR_SMOKE_HOST            default: relay.paradigmthreat.net
  RELAY_LOCAL_REPOS_MAP     default: scripts/relay-local-repos.map
  RELAY_PARK_FILES_CHECKOUT override files checkout path
  RELAY_PARK_ASSETS_CHECKOUT override relay-web-assets checkout

Docs: docs/PARADIGM_PARK_LOCAL.md
EOF
}

cmd="${1:-help}"
case "$cmd" in
  prepare) cmd_prepare ;;
  sync) cmd_sync ;;
  serve) cmd_serve ;;
  smoke) cmd_smoke ;;
  unit) cmd_unit ;;
  regress) cmd_regress ;;
  help|--help|-h) cmd_help ;;
  *) cmd_help; exit 1 ;;
esac

#!/usr/bin/env bash
# Local multi-park SSR loop — paradigmthreat / flashback / clevertree + QUERY.
# No fleet catalog → live CleverScript SSR (ssrCache compose stays off).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MAP_FILE="${RELAY_LOCAL_REPOS_MAP:-$SCRIPT_DIR/relay-local-repos.map}"

PARK_ROOT="${RELAY_PARK_LOCAL_ROOT:-$GW_DIR/docker-data/park-local}"
DATA_DIR="$PARK_ROOT/data"
STATE_DIR="$PARK_ROOT/state"
HTTP_PORT="${RELAY_HTTP_PORT:-9080}"
ASSETS_REPO="${RELAY_WEB_ASSETS_REPO:-relay-web-assets}"
# Assets tip used by park SSR stamps (catalog uses release).
ASSETS_BRANCH="${RELAY_WEB_ASSETS_BRANCH:-release}"

log() { echo "[park-local] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

map_path() {
  local name="$1"
  [[ -f "$MAP_FILE" ]] || die "missing repo map: $MAP_FILE"
  awk -v n="$name" '$1 == n { print $2; exit }' "$MAP_FILE"
}

ASSETS_CHECKOUT="${RELAY_PARK_ASSETS_CHECKOUT:-$(map_path "$ASSETS_REPO")}"

# name|branch|host1,host2,...
SITES=(
  "paradigm-threat-files|main|paradigmthreat.net,www.paradigmthreat.net,relay.paradigmthreat.net"
  "fan-edits|main|flashbackrepository.org,www.flashbackrepository.org"
  "clevertree-site|release|clevertree.net,www.clevertree.net,relay.clevertree.net"
)

ensure_dirs() {
  mkdir -p "$DATA_DIR" "$STATE_DIR" "$PARK_ROOT/ssr-out"
}

write_parked_map() {
  ensure_dirs
  local json="{"
  local txt="# park-local Host allowlist"$'\n'
  local first=1
  local entry name hosts host
  for entry in "${SITES[@]}"; do
    IFS='|' read -r name _ hosts <<<"$entry"
    IFS=',' read -ra host_arr <<<"$hosts"
    for host in "${host_arr[@]}"; do
      [[ -n "$host" ]] || continue
      if [[ $first -eq 1 ]]; then
        first=0
      else
        json+=","
      fi
      json+=$'\n'"  \"${host}\": \"${name}\""
      txt+="${host}"$'\n'
    done
  done
  json+=$'\n'"}"$'\n'
  printf '%s' "$json" >"$STATE_DIR/parked-domains.json"
  printf '%s' "$txt" >"$STATE_DIR/parked-domains.txt"
  log "Park map → $STATE_DIR/parked-domains.json"
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
  git --git-dir="$bare" config receive.denyNonFastForwards false || true
  git --git-dir="$bare" config receive.denyCurrentBranch ignore || true
}

sync_worktree_to_bare() {
  local name="$1"
  local checkout="$2"
  local branch="${3:-main}"
  local bare="$DATA_DIR/${name}.git"
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
    git --git-dir="$bare" symbolic-ref HEAD "refs/heads/${branch}"
    log "$name ${branch} → $(git --git-dir="$bare" rev-parse --short "${branch}")"
  ) || status=$?
  rm -f "$tmpindex"
  return "$status"
}

cmd_prepare() {
  ensure_dirs
  write_parked_map
  local entry name branch checkout
  for entry in "${SITES[@]}"; do
    IFS='|' read -r name branch _ <<<"$entry"
    checkout="$(map_path "$name")"
    [[ -n "$checkout" ]] || die "no path for $name in $MAP_FILE"
    ensure_bare_from_checkout "$name" "$checkout"
  done
  if [[ -n "${ASSETS_CHECKOUT:-}" && -d "$ASSETS_CHECKOUT/.git" ]]; then
    ensure_bare_from_checkout "$ASSETS_REPO" "$ASSETS_CHECKOUT"
  else
    log "WARN: $ASSETS_REPO checkout missing — SSR embed pins may fall back"
  fi
  log "Prepared under $PARK_ROOT"
}

cmd_sync() {
  cmd_prepare
  local entry name branch checkout
  for entry in "${SITES[@]}"; do
    IFS='|' read -r name branch _ <<<"$entry"
    checkout="$(map_path "$name")"
    sync_worktree_to_bare "$name" "$checkout" "$branch"
  done
  if [[ -n "${ASSETS_CHECKOUT:-}" && -d "$ASSETS_CHECKOUT/.git" ]]; then
    # Prefer release tip; also refresh main if it exists for older env defaults.
    sync_worktree_to_bare "$ASSETS_REPO" "$ASSETS_CHECKOUT" "$ASSETS_BRANCH"
    if git -C "$ASSETS_CHECKOUT" show-ref --verify --quiet refs/heads/main; then
      sync_worktree_to_bare "$ASSETS_REPO" "$ASSETS_CHECKOUT" main || true
    fi
  fi
}

gateway_env() {
  export RELAY_REPO_PATH="$DATA_DIR"
  export RELAY_INSTALL_ROOT="$PARK_ROOT"
  export RELAY_PARKED_DOMAINS_MAP_PATH="$STATE_DIR/parked-domains.json"
  export RELAY_PARKED_DOMAINS_PATH="$STATE_DIR/parked-domains.txt"
  export RELAY_HTTP_PORT="$HTTP_PORT"
  export RELAY_PUBLIC_HOSTNAME="${RELAY_PUBLIC_HOSTNAME:-localhost}"
  export RELAY_WEB_ASSETS_REPO="${RELAY_WEB_ASSETS_REPO:-relay-web-assets}"
  export RELAY_WEB_ASSETS_BRANCH="$ASSETS_BRANCH"
  # Live SSR for local iteration — do not load fleet catalog (ssrCache would short-circuit).
  unset RELAY_AUTHORIZED_REPOS_PATH RELAY_MASTER_CATALOG_PATH RELAY_SERVER_ID || true
  unset RELAY_CATALOG_PATH || true
  # Keep pre-receive QA off for dirty local mirrors.
  export RELAY_SSR_PRE_RECEIVE_QA=0
  export RUST_LOG="${RUST_LOG:-relay_ssr=info,relay_server=info,warn}"
}

ensure_binary() {
  if [[ ! -x "$GW_DIR/target/release/relay-gateway" ]]; then
    log "Building release relay-gateway…"
    (cd "$GW_DIR" && cargo build --release --bin relay-gateway --bin relay-hook-handler --bin relay-ssr-bake)
  fi
}

cmd_serve() {
  cmd_sync
  ensure_binary
  gateway_env
  log "Serving parks on http://127.0.0.1:${HTTP_PORT}/"
  log "  Host: paradigmthreat.net | flashbackrepository.org | clevertree.net"
  log "  Dump SSR: ./scripts/park-local.sh dump"
  log "  Smoke PT:  ./scripts/park-local.sh smoke"
  cd "$GW_DIR"
  exec ./target/release/relay-gateway serve --repo "$DATA_DIR"
}

# Warm SearchStore so client QUERY works after serve.
cmd_warm() {
  gateway_env
  if ! curl -sf --max-time 2 -X POST "http://127.0.0.1:${HTTP_PORT}/api/config" >/dev/null; then
    die "gateway not up on :${HTTP_PORT}"
  fi
  local entry name
  for entry in "${SITES[@]}"; do
    IFS='|' read -r name _ _ <<<"$entry"
    log "Warm index $name"
    curl -sf --max-time 120 -X POST "http://127.0.0.1:${HTTP_PORT}/api/index/warm/${name}" \
      -H "Host: ${name}.localhost" >/dev/null \
      || curl -sf --max-time 120 -X POST "http://127.0.0.1:${HTTP_PORT}/api/index/warm/${name}" >/dev/null \
      || log "WARN: warm $name failed (endpoint may need Host)"
  done
  log "Warm done"
}

# Fetch HTML for home + a few paths; write under ssr-out/ for inspection.
cmd_dump() {
  gateway_env
  ensure_dirs
  if ! curl -sf --max-time 2 -X POST "http://127.0.0.1:${HTTP_PORT}/api/config" >/dev/null; then
    die "gateway not up on :${HTTP_PORT} — run: ./scripts/park-local.sh serve"
  fi
  local out="$PARK_ROOT/ssr-out"
  rm -rf "$out"
  mkdir -p "$out"

  dump_one() {
    local host="$1" path="$2" label="$3"
    local dest="$out/${label}.html"
    local code
    code="$(curl -sS -o "$dest" -w '%{http_code}' --max-time 120 \
      -H "Host: $host" -H 'Accept: text/html' \
      "http://127.0.0.1:${HTTP_PORT}${path}")"
    local size markers=""
    size="$(wc -c <"$dest" | tr -d ' ')"
    local qa="$SCRIPT_DIR/ssr-qa-check-html.sh"
    if [[ -x "$qa" ]] && ! "$qa" --label "$label" "$dest" >/dev/null 2>&1; then
      markers=" FAIL_BOUNDARY"
    fi
    if rg -q 'data-relay-ssr="1"' "$dest"; then
      markers+=" SSR"
    elif rg -q 'data-relay-ssr-fallback|data-relay-ssr-cache="miss"' "$dest"; then
      markers+=" FALLBACK"
    else
      markers+=" CSR?"
    fi
    log "dump ${label}: HTTP ${code} ${size}b${markers} → ${dest}"
    [[ "$code" =~ ^2 ]]
    # Shared forbid list (docs/examples/ssr-qa.markers.json) — fail dump if boundary leaked.
    if [[ -x "$qa" ]]; then
      "$qa" --label "$label" "$dest" >&2
    fi
  }

  # Homes + real article/content paths (avoid /events/ JSON tree listing).
  dump_one paradigmthreat.net / pt-home
  dump_one paradigmthreat.net /science/health/get-ready-for-trumpcare.md pt-article
  dump_one flashbackrepository.org / fb-home
  dump_one flashbackrepository.org /content/home.md fb-content || dump_one flashbackrepository.org /README.md fb-readme
  dump_one clevertree.net / ct-home
  dump_one clevertree.net /resume.md ct-resume || dump_one clevertree.net /index.md ct-index

  # Marker summary
  {
    echo "# park-local SSR dump $(date -Is)"
    echo
    for f in "$out"/*.html; do
      [[ -f "$f" ]] || continue
      local base
      base="$(basename "$f")"
      echo "## $base"
      echo "- bytes: $(wc -c <"$f" | tr -d ' ')"
      echo -n "- ssr: "
      rg -q 'data-relay-ssr="1"' "$f" && echo yes || echo no
      echo -n "- error-boundary: "
      rg -q 'relay-error-boundary|data-error-boundary|Runtime error' "$f" && echo YES || echo no
      echo -n "- title: "
      rg -o '<title>[^<]+</title>' "$f" | head -1 || echo "(none)"
      echo
    done
  } >"$out/SUMMARY.md"
  log "Summary: $out/SUMMARY.md"
  cat "$out/SUMMARY.md" >&2
}

cmd_smoke() {
  gateway_env
  local files_checkout
  files_checkout="$(map_path paradigm-threat-files)"
  local smoke="${RELAY_PARK_SMOKE_SCRIPT:-$files_checkout/scripts/ssr-smoke.sh}"
  [[ -f "$smoke" ]] || die "smoke script missing: $smoke"
  if ! curl -sf --max-time 2 -X POST "http://127.0.0.1:${HTTP_PORT}/api/config" >/dev/null; then
    die "gateway not up on :${HTTP_PORT} — run: ./scripts/park-local.sh serve"
  fi
  SSR_SMOKE_HOST="${SSR_SMOKE_HOST:-relay.paradigmthreat.net}" SSR_SMOKE_PORT="$HTTP_PORT" bash "$smoke"
  local hydrate="${RELAY_PARK_HYDRATE_SCRIPT:-$files_checkout/scripts/park-hydrate-smoke.mjs}"
  if [[ -f "$hydrate" ]]; then
    log "hydrate smoke (Playwright / system Chrome)"
    SSR_SMOKE_HOST="${SSR_SMOKE_HOST:-relay.paradigmthreat.net}" SSR_SMOKE_PORT="$HTTP_PORT" node "$hydrate"
  else
    log "WARN: hydrate smoke missing: $hydrate"
  fi
}

cmd_unit() {
  log "relay-runtime host helpers + paradigm fixture render"
  (cd "$GW_DIR/../relay-runtime" && cargo test --locked \
    --test stdlib_relay_modules \
    --test paradigm_files_render \
    --test dirnav_expand \
    --test livechat_stubs \
    --test navlink_props_eval \
    --test json_builtins \
    --test cleverscript_sugar)
  log "relay-gateway park Host / SSR failure regressions"
  (cd "$GW_DIR" && cargo test --locked --lib parked -- --nocapture)
  (cd "$GW_DIR" && cargo test --locked --lib unknown_host_does_not_fall_back -- --nocapture)
}

cmd_regress() {
  cmd_unit
  log "Unit park regressions OK. For live SSR: ./scripts/park-local.sh serve"
}

cmd_help() {
  cat <<EOF
Usage: $0 <command>

  prepare   Bare mirrors + multi-host parked-domains map
  sync      prepare + push worktree snapshots → bares
  serve     sync + relay-gateway on :${HTTP_PORT} (live SSR, QUERY)
  warm      POST /api/index/warm for each park repo (gateway must be up)
  dump      curl Accept:html homes + article paths → docker-data/park-local/ssr-out/
  smoke     paradigmthreat SSR + hydrate smoke
  unit      cargo tip/host regressions
  regress   unit only
  help      this text

Hosts (Accept: text/html):
  paradigmthreat.net / relay.paradigmthreat.net
  flashbackrepository.org
  clevertree.net / relay.clevertree.net

Env:
  RELAY_HTTP_PORT           default 9080
  RELAY_WEB_ASSETS_BRANCH   default release
  RELAY_PARK_LOCAL_ROOT     default docker-data/park-local

Docs: docs/PARADIGM_PARK_LOCAL.md
EOF
}

cmd="${1:-help}"
case "$cmd" in
  prepare) cmd_prepare ;;
  sync) cmd_sync ;;
  serve) cmd_serve ;;
  warm) cmd_warm ;;
  dump) cmd_dump ;;
  smoke) cmd_smoke ;;
  unit) cmd_unit ;;
  regress) cmd_regress ;;
  help|--help|-h) cmd_help ;;
  *) cmd_help; exit 1 ;;
esac

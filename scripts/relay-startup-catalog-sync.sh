#!/usr/bin/env bash
# Run before relay-gateway: sync relay-master catalog bare mirrors from forge (GitLab, etc.),
# then best-effort git fetch from other master peers over git:// (same repos as relay-catalog.json).
#
# Environment (typical: EnvironmentFile=/opt/relay/relay.env + systemd defaults):
#   RELAY_STARTUP_CATALOG_SYNC   1|yes|true to run (default: 1)
#   RELAY_MASTER_CATALOG_PATH    catalog worktree (default: /opt/relay/src/relay-master if URL set)
#   RELAY_MASTER_CATALOG_GIT_URL optional HTTPS clone URL for relay-master — clones on first start if path missing
#   RELAY_MASTER_CATALOG_BRANCH  branch for clone (default: main)
#   RELAY_MASTER_CATALOG_CLONE_DEPTH  shallow depth for initial catalog clone (default: 128; must reach RELAY_MASTER_TRUST_ROOT)
#   RELAY_REPO_PATH              bare repo root (default: /opt/relay/data)
#   RELAY_GIT_PORT               peer git daemon port (default: 9418)
#   RELAY_PUBLIC_HOSTNAME        this node FQDN — used to skip self in RELAY_MASTER_PEER_LIST
#   RELAY_MASTER_PEER_LIST       semicolon-separated master FQDNs (other peers to fetch from)
#   RELAY_PEER_FETCH_TIMEOUT_SECS  per peer `git fetch` timeout seconds (default: 30)
#   RELAY_SERVER_ID              optional, included in Slack lines
#   RELAY_NOTIFY_SLACK_WEBHOOK_URL  optional; progress posted as Incoming Webhook JSON
#
# Always exits 0 so relay-gateway HTTP still starts; failures are recorded in
#   $RELAY_INSTALL_ROOT/state/startup_catalog_sync.json
# and surfaced in POST /api/config → startup_catalog_sync. Peer fetch errors are warned only.
set -euo pipefail

# Do not `source` relay.env here: values like RELAY_MASTER_PEER_LIST contain `;` and break bash unless
# every such line is quoted. systemd already applies EnvironmentFile=-/opt/relay/relay.env to ExecStartPre.
INSTALL="${RELAY_INSTALL_ROOT:-/opt/relay}"

RELAY_STARTUP_CATALOG_SYNC="${RELAY_STARTUP_CATALOG_SYNC:-1}"
case "${RELAY_STARTUP_CATALOG_SYNC,,}" in
  1|yes|true|on) ;;
  *) exit 0 ;;
esac

repo_path="${RELAY_REPO_PATH:-$INSTALL/data}"
git_port="${RELAY_GIT_PORT:-9418}"
self="${RELAY_PUBLIC_HOSTNAME:-${RELAY_NODE_FQDN:-}}"
sid="${RELAY_SERVER_ID:-}"
node_label="$self"
[[ -n "$sid" ]] && node_label="${node_label:+$node_label }($sid)"

skip_sync_state() {
  local reason="$1"
  echo "[relay-startup-catalog-sync] skip: $reason" >&2
  INSTALL_Q="$INSTALL" REASON="$reason" python3 -c '
import json, os, pathlib, time
root = os.environ["INSTALL_Q"]
p = pathlib.Path(root) / "state" / "startup_catalog_sync.json"
p.parent.mkdir(parents=True, exist_ok=True)
obj = {
  "skipped": True,
  "skip_reason": os.environ["REASON"],
  "catalog_forge_sync_ok": None,
  "finished_at_unix": int(time.time()),
}
p.write_text(json.dumps(obj, indent=2))
' || true
}

slack() {
  [[ -n "${RELAY_NOTIFY_SLACK_WEBHOOK_URL:-}" ]] || return 0
  local text="$1" payload
  if command -v jq >/dev/null 2>&1; then
    payload="$(jq -n --arg t "$text" '{text:$t}')"
  else
    payload="$(python3 -c "import json,sys; print(json.dumps({'text': sys.argv[1]}))" "$text")"
  fi
  curl -fsS -X POST -H 'Content-type: application/json' --data "$payload" \
    "${RELAY_NOTIFY_SLACK_WEBHOOK_URL}" 2>/dev/null || true
}

write_startup_sync_state() {
  mkdir -p "$INSTALL/state"
  local path="$INSTALL/state/startup_catalog_sync.json"
  local forge_ok="$1"
  RELAY_SYNC_PATH="$path" RELAY_SYNC_FORGE_OK="$forge_ok" \
    RELAY_SYNC_ERR="${2:-}" RELAY_SYNC_TAIL="${3:-}" \
    RELAY_SYNC_POK="${4:-0}" RELAY_SYNC_PFAIL="${5:-0}" \
    python3 -c '
import json, os, time
path = os.environ["RELAY_SYNC_PATH"]
forge_ok_b = os.environ["RELAY_SYNC_FORGE_OK"] == "1"
err = (os.environ.get("RELAY_SYNC_ERR") or "").strip()
tail = os.environ.get("RELAY_SYNC_TAIL") or ""
if len(tail) > 4000:
  tail = tail[-4000:]
pok = int(os.environ.get("RELAY_SYNC_POK") or "0")
pfail = int(os.environ.get("RELAY_SYNC_PFAIL") or "0")
obj = {"catalog_forge_sync_ok": forge_ok_b, "finished_at_unix": int(time.time())}
if err:
  obj["error_summary"] = err[:1200]
if tail:
  obj["catalog_output_tail"] = tail
if pok or pfail:
  obj["peer_fetch"] = {"ok": pok, "failed": pfail}
with open(path, "w", encoding="utf-8") as f:
  json.dump(obj, f, indent=2)
'
}

catalog="${RELAY_MASTER_CATALOG_PATH:-}"
catalog_url="${RELAY_MASTER_CATALOG_GIT_URL:-}"
catalog_branch="${RELAY_MASTER_CATALOG_BRANCH:-main}"
if [[ -z "$catalog" && -n "$catalog_url" ]]; then
  catalog="$INSTALL/src/relay-master"
fi

if [[ -z "$catalog" ]]; then
  skip_sync_state "no_catalog_path_or_relay_catalog_json_or_clone_url"
  exit 0
fi

sync_script="${catalog}/scripts/sync-relay-catalog.sh"
catalog_json="${catalog}/relay-catalog.json"

# Ensure catalog worktree: clone when RELAY_MASTER_CATALOG_GIT_URL is set and path has no usable checkout.
if [[ ! -f "$catalog_json" || ! -f "$sync_script" ]]; then
  if [[ -z "$catalog_url" ]]; then
    skip_sync_state "no_catalog_path_or_relay_catalog_json_or_clone_url"
    exit 0
  fi
  if [[ -d "$catalog/.git" ]]; then
    echo "[relay-startup-catalog-sync] catalog at $catalog is git but missing relay-catalog.json or sync script" >&2
    skip_sync_state "catalog_checkout_incomplete"
    slack ":warning: *Relay startup sync* — \`${node_label:-unknown}\` — catalog dir has \`.git\` but no \`relay-catalog.json\` / \`scripts/sync-relay-catalog.sh\` (fix checkout or remove dir)"
    exit 0
  fi
  if [[ -d "$catalog" ]]; then
    if [[ -n "$(ls -A "$catalog" 2>/dev/null)" ]]; then
      echo "[relay-startup-catalog-sync] ERROR: $catalog exists and is not a git repo (non-empty)" >&2
      skip_sync_state "catalog_path_obstructed"
      slack ":x: *Relay startup sync* — \`${node_label:-unknown}\` — \`RELAY_MASTER_CATALOG_PATH\` exists but is not a git checkout: \`$catalog\`"
      exit 0
    fi
    rmdir "$catalog" 2>/dev/null || true
  fi
  mkdir -p "$(dirname "$catalog")"
  slack ":construction: *Relay startup sync* — \`${node_label:-unknown}\` — cloning catalog → \`$catalog\` (\`$catalog_branch\`)"
  clone_out=""
  # Shallow depth 1 omits ancestors: RELAY_MASTER_TRUST_ROOT merge-base check in relay-gateway then fails.
  clone_depth="${RELAY_MASTER_CATALOG_CLONE_DEPTH:-128}"
  if ! clone_out="$(git clone --depth "$clone_depth" --branch "$catalog_branch" --single-branch "$catalog_url" "$catalog" 2>&1)"; then
    echo "$clone_out" >&2
    slack ":warning: *Relay startup sync* — \`${node_label:-unknown}\` — *catalog git clone failed* (HTTP still starts)\n\`\`\`${clone_out:0:1200}\`\`\`"
    write_startup_sync_state 0 "git clone RELAY_MASTER_CATALOG_GIT_URL failed" "$clone_out" 0 0
    exit 0
  fi
  echo "$clone_out" >&2
  if [[ "$(git -C "$catalog" rev-parse --is-shallow-repository 2>/dev/null)" == "true" ]]; then
    deepen_out=""
    deepen_out="$(git -C "$catalog" fetch --deepen 2048 origin 2>&1)" || true
    [[ -n "$deepen_out" ]] && echo "$deepen_out" >&2
  fi
  sync_script="${catalog}/scripts/sync-relay-catalog.sh"
  slack ":white_check_mark: *Relay startup sync* — \`${node_label:-unknown}\` — catalog clone OK → \`$catalog\`"
fi

if [[ ! -f "$catalog_json" || ! -f "$sync_script" ]]; then
  skip_sync_state "no_catalog_path_or_relay_catalog_json_after_clone"
  exit 0
fi

# Fast-forward an existing catalog checkout (clone path above already fetched).
if [[ -d "$catalog/.git" && -n "$catalog_url" ]]; then
  git -C "$catalog" remote get-url origin >/dev/null 2>&1 || git -C "$catalog" remote add origin "$catalog_url"
  git -C "$catalog" remote set-url origin "$catalog_url" || true
  fetch_out="$(git -C "$catalog" fetch origin "$catalog_branch" 2>&1)" || echo "[relay-startup-catalog-sync] WARN: catalog fetch failed: $fetch_out" >&2
  if git -C "$catalog" merge --ff-only FETCH_HEAD >/dev/null 2>&1; then
    echo "[relay-startup-catalog-sync] catalog worktree $(git -C "$catalog" rev-parse --short HEAD)" >&2
  else
    echo "[relay-startup-catalog-sync] WARN: catalog worktree not fast-forwardable; leaving $(git -C "$catalog" rev-parse --short HEAD)" >&2
  fi
fi

# Nodes have no GitLab deploy key; rewrite git@ submodule URLs before sync-relay-catalog.sh clones.
if [[ -f "$catalog/.gitmodules" ]]; then
  sed -i 's|url = git@gitlab.com:|url = https://gitlab.com/|' "$catalog/.gitmodules" || true
fi

list_catalog_repo_names() {
  python3 - "$catalog_json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    data = json.load(f)
for name in sorted((data.get("repos") or {}).keys()):
    print(name)
PY
}

peer_fetch_other_masters() {
  local peers_raw="${RELAY_MASTER_PEER_LIST:-}"
  local -a peers=()
  local p
  IFS=';' read -ra parts <<< "$peers_raw"
  for p in "${parts[@]}"; do
    p="${p//[[:space:]]/}"
    [[ -z "$p" ]] && continue
    if [[ -n "$self" ]] && [[ "${p,,}" == "${self,,}" ]]; then
      continue
    fi
    peers+=("$p")
  done
  if [[ ${#peers[@]} -eq 0 ]]; then
    echo "[relay-startup-catalog-sync] no peer masters to fetch (list empty or only self)" >&2
    slack ":information_source: *Relay startup sync* — \`${node_label:-unknown}\` — no other masters in \`RELAY_MASTER_PEER_LIST\`; skipping peer \`git fetch\`"
    PEER_FETCH_OK=0
    PEER_FETCH_FAIL=0
    return 0
  fi

  slack ":arrows_counterclockwise: *Relay startup sync* — \`${node_label:-unknown}\` — fetching catalog repos from peers: \`${peers[*]}\`"

  local name peer url summary_ok summary_fail
  local fetch_timeout="${RELAY_PEER_FETCH_TIMEOUT_SECS:-30}"
  summary_ok=0
  summary_fail=0
  while IFS= read -r name; do
    [[ -z "$name" ]] && continue
    local bare="${repo_path}/${name}.git"
    if [[ ! -d "$bare" ]]; then
      echo "[relay-startup-catalog-sync] WARN: no bare at $bare; skip peer fetch for $name" >&2
      continue
    fi
    for peer in "${peers[@]}"; do
      url="git://${peer}:${git_port}/${name}.git"
      if timeout "${fetch_timeout}s" git --git-dir="$bare" fetch "$url" '+refs/heads/*:refs/heads/*' --prune 2>/dev/null; then
        echo "[relay-startup-catalog-sync] fetched $name from $peer" >&2
        ((summary_ok++)) || true
      else
        echo "[relay-startup-catalog-sync] WARN: fetch $name from $peer failed (peer may be down or repo missing)" >&2
        ((summary_fail++)) || true
      fi
    done
  done < <(list_catalog_repo_names)

  if [[ "$summary_fail" -gt 0 ]]; then
    slack ":warning: *Relay startup sync* — \`${node_label:-unknown}\` — peer fetch finished with *${summary_fail}* failed attempt(s), *${summary_ok}* ok (see journalctl -u relay-gateway)"
  else
    slack ":white_check_mark: *Relay startup sync* — \`${node_label:-unknown}\` — peer \`git fetch\` round complete (*${summary_ok}* ok)"
  fi
  PEER_FETCH_OK=$summary_ok
  PEER_FETCH_FAIL=$summary_fail
}

slack ":rocket: *Relay startup sync* — \`${node_label:-unknown}\` — starting (catalog → forge mirrors, then peers)"

export RELAY_REPO_PATH="$repo_path"
PEER_FETCH_OK=0
PEER_FETCH_FAIL=0
if ! out="$(bash "$sync_script" 2>&1)"; then
  echo "$out" >&2
  slack ":warning: *Relay startup sync* — \`${node_label:-unknown}\` — *catalog forge sync failed* (HTTP still starts). Fix catalog/GitLab, then \`systemctl restart relay-gateway\`.\n\`\`\`${out:0:1500}\`\`\`"
  write_startup_sync_state 0 "sync-relay-catalog.sh exited non-zero" "$out" 0 0
  slack ":checkered_flag: *Relay startup sync* — \`${node_label:-unknown}\` — finished (with errors); starting relay-gateway"
  git -C "$catalog" checkout -q -- .gitmodules 2>/dev/null || true
  exit 0
fi
echo "$out" >&2
git -C "$catalog" checkout -q -- .gitmodules 2>/dev/null || true
slack ":package: *Relay startup sync* — \`${node_label:-unknown}\` — catalog mirrors updated from forge (relay-catalog.json + relay-master per sync-relay-catalog)"

peer_fetch_other_masters

write_startup_sync_state 1 "" "$out" "${PEER_FETCH_OK:-0}" "${PEER_FETCH_FAIL:-0}"

slack ":checkered_flag: *Relay startup sync* — \`${node_label:-unknown}\` — finished; starting relay-gateway"

exit 0

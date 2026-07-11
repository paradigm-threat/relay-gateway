#!/usr/bin/env bash
# Add git:// remotes for Spot fleet masters (dfw1, lon1, hkg1) on local catalog checkouts.
#
# Usage (from relay-gateway/):
#   ./scripts/relay-local-remotes.sh              # add missing remotes
#   ./scripts/relay-local-remotes.sh --probe      # ls-remote each remote
#   ./scripts/relay-local-remotes.sh --ensure-bare  # POST ensure-bare on fleet (HTTPS)
#   ./scripts/relay-local-remotes.sh --all        # ensure-bare + remotes + probe
#
# Map file: scripts/relay-local-repos.map (repo-name<TAB>path)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MAP="${RELAY_LOCAL_REPOS_MAP:-$ROOT/scripts/relay-local-repos.map}"
GIT_PORT="${RELAY_GIT_PORT:-9418}"
NODES=(dfw1 lon1 hkg1)

DO_PROBE=0
DO_ENSURE=0

while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --probe) DO_PROBE=1 ;;
    --ensure-bare) DO_ENSURE=1 ;;
    --all) DO_PROBE=1; DO_ENSURE=1 ;;
    -h|--help)
      sed -n '1,12p' "$0"
      exit 0
      ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
  shift
done

[[ -f "$MAP" ]] || { echo "Missing map: $MAP" >&2; exit 1; }

relay_url() {
  local node="$1" repo="$2"
  printf 'git://%s.relaygateway.net:%s/%s.git' "$node" "$GIT_PORT" "$repo"
}

ensure_bare_fleet() {
  local repo="$1"
  for node in "${NODES[@]}"; do
    local fqdn="${node}.relaygateway.net"
    local code
    code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
      "https://${fqdn}/api/git/ensure-bare/${repo}" --connect-timeout 20 || echo 000)"
    if [[ "$code" == "201" || "$code" == "200" ]]; then
      echo "  ensure-bare ${fqdn}/${repo} -> ${code}"
    else
      echo "  WARN ensure-bare ${fqdn}/${repo} -> ${code}" >&2
    fi
  done
}

add_remotes() {
  local repo="$1" dir="$2"
  [[ -d "$dir/.git" ]] || { echo "SKIP $repo (no git dir): $dir" >&2; return 0; }
  for node in "${NODES[@]}"; do
    local url
    url="$(relay_url "$node" "$repo")"
    if git -C "$dir" remote get-url "$node" &>/dev/null; then
      local cur
      cur="$(git -C "$dir" remote get-url "$node")"
      if [[ "$cur" != "$url" ]]; then
        git -C "$dir" remote set-url "$node" "$url"
        echo "SET  $repo @$node -> $url"
      else
        echo "OK   $repo @$node"
      fi
    else
      git -C "$dir" remote add "$node" "$url"
      echo "ADD  $repo @$node -> $url"
    fi
  done
}

probe_remotes() {
  local repo="$1" dir="$2"
  [[ -d "$dir/.git" ]] || return 0
  for node in "${NODES[@]}"; do
    if ! git -C "$dir" remote get-url "$node" &>/dev/null; then
      echo "MISS remote $node in $dir" >&2
      continue
    fi
    local url
    url="$(git -C "$dir" remote get-url "$node")"
    if git ls-remote "$url" HEAD &>/dev/null; then
      echo "LIVE $repo $node"
    else
      echo "EMPTY $repo $node (bare missing or no refs — push or reclone)" >&2
    fi
  done
}

echo "=== relay-local-remotes (nodes: ${NODES[*]}) ==="
while IFS=$'\t' read -r repo path _; do
  [[ -z "${repo:-}" || "$repo" =~ ^# ]] && continue
  path="${path%%#*}"
  path="${path// /}"
  [[ -n "$path" ]] || continue
  echo "--- $repo ($path) ---"
  if [[ "$DO_ENSURE" -eq 1 ]]; then
    ensure_bare_fleet "$repo"
  fi
  add_remotes "$repo" "$path"
  if [[ "$DO_PROBE" -eq 1 ]]; then
    probe_remotes "$repo" "$path"
  fi
done < "$MAP"
echo "=== done ==="

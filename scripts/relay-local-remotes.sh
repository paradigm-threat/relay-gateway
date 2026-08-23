#!/usr/bin/env bash
# Add git:// remotes for Spot fleet masters (iad1, sjc1, hkg1) on local catalog checkouts.
# Renames GitLab origin → gitlab (backup); default push targets first mesh origin (iad1).
#
# Usage (from relay-gateway/):
#   ./scripts/relay-local-remotes.sh              # add missing remotes + push defaults
#   ./scripts/relay-local-remotes.sh --probe      # ls-remote each remote
#   ./scripts/relay-local-remotes.sh --ensure-bare  # POST ensure-bare on fleet (HTTPS)
#   ./scripts/relay-local-remotes.sh --all        # ensure-bare + remotes + probe
#
# Map file: scripts/relay-local-repos.map (repo-name<TAB>path)
# Override primary mesh remote: RELAY_PRIMARY_ORIGIN=iad1
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MAP="${RELAY_LOCAL_REPOS_MAP:-$ROOT/scripts/relay-local-repos.map}"
GIT_PORT="${RELAY_GIT_PORT:-9418}"
NODES=(iad1 sjc1 hkg1)
STALE_NODES=(lon1)
PRIMARY_NODE="${RELAY_PRIMARY_ORIGIN:-iad1}"
GITLAB_REMOTE="${RELAY_GITLAB_REMOTE_NAME:-gitlab}"

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

# First git.origins[].name from .relay.yaml, else PRIMARY_NODE.
primary_node_for_repo() {
  local dir="$1"
  local yaml="$dir/.relay.yaml" name=""
  if [[ -f "$yaml" ]]; then
    name="$(awk '
      /^[[:space:]]*origins:[[:space:]]*$/ { in_origins=1; next }
      in_origins && /^[[:space:]]*-[[:space:]]name:[[:space:]]*/ {
        sub(/^[[:space:]]*-[[:space:]]name:[[:space:]]*/, "")
        gsub(/["'\'']/, "")
        print
        exit
      }
      in_origins && /^[^[:space:]]/ { exit }
    ' "$yaml" 2>/dev/null || true)"
  fi
  [[ -n "$name" ]] && echo "$name" || echo "$PRIMARY_NODE"
}

# GitLab forge remote: rename origin → gitlab (idempotent).
promote_gitlab_backup_remote() {
  local repo="$1" dir="$2"
  [[ -d "$dir/.git" ]] || return 0
  if ! git -C "$dir" remote get-url origin &>/dev/null; then
    return 0
  fi
  local url
  url="$(git -C "$dir" remote get-url origin)"
  if [[ "$url" != *gitlab.com* ]]; then
    echo "SKIP $repo origin is not GitLab ($url)"
    return 0
  fi
  if git -C "$dir" remote get-url "$GITLAB_REMOTE" &>/dev/null; then
    local cur
    cur="$(git -C "$dir" remote get-url "$GITLAB_REMOTE")"
    if [[ "$cur" == "$url" ]]; then
      git -C "$dir" remote remove origin
      echo "DROP $repo @origin (duplicate of $GITLAB_REMOTE)"
    else
      echo "WARN $repo: origin is GitLab but $GITLAB_REMOTE exists with different URL" >&2
    fi
    return 0
  fi
  git -C "$dir" remote rename origin "$GITLAB_REMOTE"
  echo "REN  $repo origin -> $GITLAB_REMOTE ($url)"
}

# Default push: first mesh origin; main/master track relay, other branches keep gitlab if set.
set_push_defaults() {
  local repo="$1" dir="$2"
  [[ -d "$dir/.git" ]] || return 0
  local primary ref remote
  primary="$(primary_node_for_repo "$dir")"
  if ! git -C "$dir" remote get-url "$primary" &>/dev/null; then
    echo "WARN $repo: missing primary remote $primary — run add_remotes first" >&2
    return 0
  fi
  git -C "$dir" config remote.pushDefault "$primary"
  echo "PUSH $repo default -> $primary (gitlab backup: git push $GITLAB_REMOTE <branch>)"

  for ref in $(git -C "$dir" for-each-ref --format='%(refname:short)' refs/heads/); do
    remote="$(git -C "$dir" config --get "branch.${ref}.remote" 2>/dev/null || true)"
    case "$ref" in
      main|master)
        git -C "$dir" config "branch.${ref}.remote" "$primary"
        git -C "$dir" config "branch.${ref}.merge" "refs/heads/${ref}"
        echo "UP   $repo branch $ref -> ${primary}/${ref}"
        ;;
      *)
        if [[ -z "$remote" ]]; then
          continue
        fi
        if [[ "$remote" == "origin" ]]; then
          if git -C "$dir" remote get-url "$GITLAB_REMOTE" &>/dev/null; then
            git -C "$dir" config "branch.${ref}.remote" "$GITLAB_REMOTE"
            echo "UP   $repo branch $ref -> ${GITLAB_REMOTE} (was origin)"
          fi
        fi
        ;;
    esac
  done
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
  for stale in "${STALE_NODES[@]}"; do
    if git -C "$dir" remote get-url "$stale" &>/dev/null; then
      git -C "$dir" remote remove "$stale"
      echo "DROP $repo @$stale"
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
  promote_gitlab_backup_remote "$repo" "$path"
  set_push_defaults "$repo" "$path"
  if [[ "$DO_PROBE" -eq 1 ]]; then
    probe_remotes "$repo" "$path"
  fi
done < "$MAP"
echo "=== done ==="

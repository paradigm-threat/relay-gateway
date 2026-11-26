#!/usr/bin/env bash
# Bootstrap an empty relay-ssr-cache bare + optional worktree (publisher laptop or fat node).
# Usage:
#   init-relay-ssr-cache.sh [--bare-root /opt/relay/repos] [--worktree /opt/relay/worktrees/relay-ssr-cache]
set -euo pipefail

BARE_ROOT="${RELAY_REPO_PATH:-/opt/relay/repos}"
WORKTREE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bare-root) BARE_ROOT="$2"; shift 2 ;;
    --worktree) WORKTREE="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

BARE="$BARE_ROOT/relay-ssr-cache.git"
mkdir -p "$BARE_ROOT"
if [[ ! -d "$BARE" ]]; then
  git init --bare -b main "$BARE"
  echo "[init-ssr-cache] created $BARE"
else
  echo "[init-ssr-cache] bare exists: $BARE"
fi

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

git clone "$BARE" "$TMP/wt"
git -C "$TMP/wt" checkout -B main 2>/dev/null || true
# Seed README so main exists for mesh fetch.
if [[ ! -f "$TMP/wt/README.md" ]]; then
  cat >"$TMP/wt/README.md" <<'EOF'
# relay-ssr-cache

Pre-rendered HTML fragments for parked Relay Hosts (`sites/<apex>/_shell.html` + `*.body.html`).
Bake only on the publisher; Nanodes git-fetch this bare.
EOF
  mkdir -p "$TMP/wt/sites"
  git -C "$TMP/wt" add -A
  git -C "$TMP/wt" -c user.email=relay@localhost -c user.name=relay commit -m "init relay-ssr-cache stub" || true
  git -C "$TMP/wt" push -u origin HEAD:main
  git --git-dir="$BARE" symbolic-ref HEAD refs/heads/main 2>/dev/null || true
fi

if [[ -n "$WORKTREE" ]]; then
  mkdir -p "$(dirname "$WORKTREE")"
  if [[ ! -d "$WORKTREE/.git" ]]; then
    git clone -b main "$BARE" "$WORKTREE"
    echo "[init-ssr-cache] worktree $WORKTREE"
  fi
fi

echo "[init-ssr-cache] tip=$(git --git-dir="$BARE" rev-parse refs/heads/main 2>/dev/null || echo none)"

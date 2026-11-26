#!/usr/bin/env bash
# Publisher-only async SSR bake queue.
# Nanodes must not enable this unit — they only git-fetch relay-ssr-cache.
#
# Env:
#   RELAY_SSR_BAKE_QUEUE_DIR   default /opt/relay/state/ssr-bake-queue
#   RELAY_SSR_CACHE_WORKTREE   worktree checkout of relay-ssr-cache
#   RELAY_NODE_ROLE            publisher (default) | mesh → refuse
#   RELAY_SSR_BAKE_PUSH=1      git push cache after commit
#   RELAY_SSR_CACHE_REMOTE     remote for push (default origin)
#
# Usage:
#   relay-ssr-bake-queue.sh once|daemon|enqueue <job.json>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BAKE_PY="${RELAY_SSR_BAKE_PY:-$SCRIPT_DIR/relay-ssr-bake.py}"
QUEUE_DIR="${RELAY_SSR_BAKE_QUEUE_DIR:-/opt/relay/state/ssr-bake-queue}"
CACHE_WT="${RELAY_SSR_CACHE_WORKTREE:-}"
ROLE="${RELAY_NODE_ROLE:-publisher}"

if [[ "$ROLE" == "mesh" ]]; then
  echo "[ssr-bake-queue] refuse: RELAY_NODE_ROLE=mesh (Nanodes do not bake)" >&2
  exit 1
fi

mkdir -p "$QUEUE_DIR/pending" "$QUEUE_DIR/done" "$QUEUE_DIR/failed"

run_bake_from_job() {
  local job="$1"
  python3 - "$job" "$BAKE_PY" <<'PY'
import json, os, subprocess, sys
from pathlib import Path
job_path, bake_py = sys.argv[1], sys.argv[2]
j = json.load(open(job_path, encoding="utf-8"))

def resolve_repo(value):
    if not value:
        return None
    p = Path(value)
    if p.is_dir():
        return str(p)
    # Catalog slugs → RELAY_REPO_PATH/<slug>.git
    data = Path(os.environ.get("RELAY_REPO_PATH", "/opt/relay/data"))
    bare = data / f"{value}.git"
    if bare.is_dir():
        return str(bare)
    return value

args = [
    sys.executable, bake_py,
    "--content", resolve_repo(j["content"]) or j["content"],
    "--cache-worktree", j["cacheWorktree"],
    "--prefix", j["prefix"],
    "--site-name", j.get("siteName") or "",
    "--content-branch", j.get("contentBranch") or "main",
    "--commit",
]
if j.get("full", True):
    args.append("--full")
else:
    args.append("--delta")
if j.get("httpOrigin"):
    args += ["--http-origin", j["httpOrigin"]]
hook = resolve_repo(j.get("hookAssets"))
if hook:
    args += [
        "--hook-assets", hook,
        "--hook-assets-branch", j.get("hookAssetsBranch") or "release",
    ]
# Prefer installed binary when present
bake_bin = os.environ.get("RELAY_SSR_BAKE_BIN", "").strip()
if bake_bin:
    os.environ["RELAY_SSR_BAKE_BIN"] = bake_bin
raise SystemExit(subprocess.call(args))
PY
}

process_job() {
  local job="$1"
  local base
  base="$(basename "$job")"
  echo "[ssr-bake-queue] process $base"
  if ! run_bake_from_job "$job"; then
    mv -f "$job" "$QUEUE_DIR/failed/$base"
    return 1
  fi
  if [[ "${RELAY_SSR_BAKE_PUSH:-0}" == "1" && -n "$CACHE_WT" ]]; then
    # Publish-origin update hook advances the bare tip early, so receive-pack
    # often reports "failed to update ref" even after a successful mesh sync.
    # Treat push as OK when worktree HEAD already matches the bare tip.
    if ! git -C "$CACHE_WT" push "${RELAY_SSR_CACHE_REMOTE:-origin}" HEAD:main; then
      local head bare_tip remote_url
      head="$(git -C "$CACHE_WT" rev-parse HEAD)"
      remote_url="$(git -C "$CACHE_WT" remote get-url "${RELAY_SSR_CACHE_REMOTE:-origin}")"
      if [[ -d "$remote_url" ]]; then
        bare_tip="$(git --git-dir="$remote_url" rev-parse refs/heads/main 2>/dev/null || true)"
      else
        bare_tip="$(git ls-remote "$remote_url" refs/heads/main 2>/dev/null | awk '{print $1}')"
      fi
      if [[ -n "$bare_tip" && "$head" == "$bare_tip" ]]; then
        echo "[ssr-bake-queue] push race ok tip=$head"
      else
        echo "[ssr-bake-queue] push failed for $base (head=$head bare=${bare_tip:-none})" >&2
        mv -f "$job" "$QUEUE_DIR/failed/$base"
        return 1
      fi
    fi
  fi
  mv -f "$job" "$QUEUE_DIR/done/$base"
}

cmd="${1:-once}"
case "$cmd" in
  enqueue)
    shift
    dest="$QUEUE_DIR/pending/$(date +%s)-$$.json"
    if [[ $# -eq 1 && -f "$1" ]]; then
      cp -f "$1" "$dest"
    else
      echo "usage: $0 enqueue <job.json>" >&2
      exit 2
    fi
    echo "[ssr-bake-queue] enqueued $dest"
    ;;
  once)
    shopt -s nullglob
    for j in "$QUEUE_DIR/pending"/*.json; do
      process_job "$j" || true
    done
    ;;
  daemon)
    echo "[ssr-bake-queue] daemon watching $QUEUE_DIR/pending"
    while true; do
      shopt -s nullglob
      for j in "$QUEUE_DIR/pending"/*.json; do
        process_job "$j" || true
      done
      sleep "${RELAY_SSR_BAKE_POLL_SEC:-5}"
    done
    ;;
  *)
    echo "usage: $0 once|daemon|enqueue <job.json>" >&2
    exit 2
    ;;
esac

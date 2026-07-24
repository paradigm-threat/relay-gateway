#!/usr/bin/env bash
# Prepare bare repos + catalog for docker-compose.ipfs-dev.yml
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${RELAY_DEV_DATA_DIR:-$GW_DIR/docker-data/ipfs-dev/data}"
# Prefer dated 2026-07-22 rebuild (centralDir); fall back to legacy Jul-18 paths.
_DEFAULT_FILES_BARE="$GW_DIR/../../pt/thin-migrate/paradigm-threat-files-20260722/thin.git"
_DEFAULT_FILES_CHECKOUT="$GW_DIR/../../pt/thin-migrate/paradigm-threat-files-20260722/checkout"
[[ -d "$_DEFAULT_FILES_BARE" ]] || _DEFAULT_FILES_BARE="$GW_DIR/../../pt/thin-migrate/paradigm-threat-files/thin.git"
[[ -d "$_DEFAULT_FILES_CHECKOUT" ]] || _DEFAULT_FILES_CHECKOUT="$GW_DIR/../../pt/thin-migrate/paradigm-threat-files/checkout"
THIN_FILES_SRC="${THIN_FILES_BARE:-$_DEFAULT_FILES_BARE}"
THIN_FILES_CHECKOUT="${THIN_FILES_CHECKOUT:-$_DEFAULT_FILES_CHECKOUT}"
_DEFAULT_TL_BARE="$GW_DIR/../../pt/thin-migrate/paradigm-threat-timeline-20260722/thin.git"
[[ -d "$_DEFAULT_TL_BARE" ]] || _DEFAULT_TL_BARE="$GW_DIR/../../pt/thin-migrate/paradigm-threat-timeline/thin.git"
THIN_TIMELINE_SRC="${THIN_TIMELINE_BARE:-$_DEFAULT_TL_BARE}"

log() { echo "[docker-ipfs-dev-prepare] $*" >&2; }

mkdir -p "${RELAY_DEV_LOGS_DIR:-$GW_DIR/docker-data/ipfs-dev/logs}"

prepare_bare_with_hook_overlay() {
  local name="$1"
  local src_bare="$2"
  local checkout="$3"
  local dest="$DATA_DIR/${name}.git"

  if [[ ! -d "$src_bare" ]]; then
    log "SKIP $name: missing bare $src_bare"
    return 0
  fi

  mkdir -p "$DATA_DIR"
  log "Sync bare $name -> $dest"
  rsync -a --delete "$src_bare/" "$dest/"

  if [[ ! -d "$checkout/.git" ]]; then
    log "No checkout at $checkout; using bare HEAD as-is"
    return 0
  fi

  local hook="$checkout/hooks/server/get.cs"
  if [[ ! -f "$hook" ]]; then
    log "No $hook; using bare HEAD as-is"
    return 0
  fi

  log "Overlay get.cs from checkout onto $name bare (commit-tree, checkout branch unchanged)"
  git -C "$checkout" add -f hooks/server/get.cs 2>/dev/null || true
  if [[ -f "$checkout/.gitignore" ]]; then
    git -C "$checkout" add -f .gitignore 2>/dev/null || true
  fi
  local tree parent commit
  tree=$(git -C "$checkout" write-tree)
  parent=$(git -C "$checkout" rev-parse HEAD)
  commit=$(git -C "$checkout" commit-tree "$tree" -p "$parent" -m "dev: IPFS get hook overlay for docker test")
  git -C "$checkout" push "$dest" "${commit}:refs/heads/main"
  git -C "$checkout" reset HEAD >/dev/null 2>&1 || true
  log "$name main -> $(git --git-dir="$dest" rev-parse --short main)"
}

mkdir -p "$DATA_DIR"
prepare_bare_with_hook_overlay "paradigm-threat-files" "$THIN_FILES_SRC" "$THIN_FILES_CHECKOUT"

if [[ -d "$THIN_TIMELINE_SRC" ]]; then
  dest="$DATA_DIR/paradigm-threat-timeline.git"
  log "Sync bare paradigm-threat-timeline -> $dest"
  rsync -a --delete "$THIN_TIMELINE_SRC/" "$dest/"
fi

log "Data dir: $DATA_DIR"
ls -la "$DATA_DIR"

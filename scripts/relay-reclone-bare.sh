#!/usr/bin/env bash
# Transactionally replace one catalog bare repository on a relay node.
#
# Invoked remotely by relay-fleet.sh as a sudo-capable operator user:
#   relay-reclone-bare.sh DATA_ROOT GIT_PORT REPO [PEER]
set -euo pipefail

DATA_ROOT="${1:?data root required}"
GIT_PORT="${2:?git port required}"
REPO="${3:?repo slug required}"
PEER="${4:-}"

CATALOG_BARE="${DATA_ROOT}/relay-master.git"
DEST="${DATA_ROOT}/${REPO}.git"
STAGING="${DATA_ROOT}/.${REPO}.git.relay-staging.$$"
BACKUP="${DATA_ROOT}/.${REPO}.git.relay-backup.$$"
DAEMON_STOPPED=0

[[ "$REPO" =~ ^[A-Za-z0-9._-]+$ ]] || {
  echo "invalid repo slug: $REPO" >&2
  exit 2
}
sudo test -d "$CATALOG_BARE" || {
  echo "catalog bare missing: $CATALOG_BARE" >&2
  exit 1
}

cleanup() {
  if sudo test -d "$BACKUP" && ! sudo test -d "$DEST"; then
    sudo -u relay mv "$BACKUP" "$DEST" || true
  elif sudo test -d "$BACKUP" && sudo test -d "$DEST"; then
    sudo -u relay rm -rf "$BACKUP"
  fi
  sudo -u relay rm -rf "$STAGING"
  if [[ "$DAEMON_STOPPED" -eq 1 ]]; then
    sudo systemctl start relay-git-daemon 2>/dev/null || true
  fi
}
trap cleanup EXIT

# Read the exact catalog currently served by this node. This does not depend on
# an optional relay-master worktree being installed under /opt/relay/src.
entry="$(
  sudo -u relay git --git-dir="$CATALOG_BARE" show HEAD:relay-catalog.json |
    python3 -c '
import json, sys
repo = sys.argv[1]
entry = json.load(sys.stdin).get("repos", {}).get(repo)
if not entry:
    raise SystemExit(f"{repo!r} is not present in relay-catalog.json")
for key in ("url", "relayRootCommit", "branch"):
    if not entry.get(key):
        raise SystemExit(f"{repo!r} catalog entry is missing {key}")
print(entry["url"], entry["relayRootCommit"], entry["branch"], sep="\t")
' "$REPO"
)"
IFS=$'\t' read -r URL ROOT_COMMIT BRANCH <<<"$entry"

sudo -u relay rm -rf "$STAGING" "$BACKUP"
if ! sudo -u relay git clone --bare "$URL" "$STAGING"; then
  if [[ -z "$PEER" ]]; then
    echo "forge clone failed and no --peer fallback was provided" >&2
    exit 1
  fi
  sudo -u relay rm -rf "$STAGING"
  sudo -u relay git init --bare "$STAGING"
fi

if [[ -n "$PEER" ]]; then
  sudo -u relay git --git-dir="$STAGING" fetch \
    "git://${PEER}:${GIT_PORT}/${REPO}.git" \
    '+refs/heads/*:refs/heads/*' --prune
fi

# A candidate is installable only when the configured trust root belongs to
# the configured publish branch. Preserve that branch's current tip.
sudo -u relay git --git-dir="$STAGING" cat-file -e "${ROOT_COMMIT}^{commit}"
sudo -u relay git --git-dir="$STAGING" rev-parse --verify "refs/heads/${BRANCH}"
sudo -u relay git --git-dir="$STAGING" merge-base --is-ancestor \
  "$ROOT_COMMIT" "refs/heads/${BRANCH}"
sudo -u relay git --git-dir="$STAGING" symbolic-ref HEAD "refs/heads/${BRANCH}"
sudo -u relay git --git-dir="$STAGING" config receive.denyNonFastForwards true

# Keep the healthy mirror online until its replacement is ready. The daemon is
# stopped only for the same-filesystem rename window.
sudo systemctl stop relay-git-daemon
DAEMON_STOPPED=1
if sudo test -d "$DEST"; then
  sudo -u relay mv "$DEST" "$BACKUP"
fi
if ! sudo -u relay mv "$STAGING" "$DEST"; then
  if sudo test -d "$BACKUP"; then
    sudo -u relay mv "$BACKUP" "$DEST"
  fi
  sudo systemctl start relay-git-daemon
  DAEMON_STOPPED=0
  echo "failed to install replacement; previous mirror restored" >&2
  exit 1
fi

sudo -u relay rm -rf "$BACKUP"
sudo systemctl restart relay-gateway relay-git-daemon
DAEMON_STOPPED=0
trap - EXIT

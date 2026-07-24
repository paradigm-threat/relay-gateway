#!/usr/bin/env bash
# Create a thin repo with full history (binaries -> IPFS pointers, all CIDs pinned locally).
set -euo pipefail

REPO_SRC="${1:?usage: relay-ipfs-thin-repo.sh SOURCE_REPO [WORKDIR]}"
WORKDIR="${2:-/home/ari/dev/pt/thin-migrate/$(basename "$(cd "$REPO_SRC" && pwd)")}"
BRANCH="${BRANCH:-main}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GIT_FILTER_REPO="${GIT_FILTER_REPO:-}"
if [[ -z "$GIT_FILTER_REPO" ]]; then
  if command -v git-filter-repo >/dev/null 2>&1; then
    GIT_FILTER_REPO="git-filter-repo"
  elif [[ -x /tmp/ipfs-migrate-venv/bin/git-filter-repo ]]; then
    GIT_FILTER_REPO="/tmp/ipfs-migrate-venv/bin/git-filter-repo"
  else
    echo "install git-filter-repo (apt install git-filter-repo or pip in venv)" >&2
    exit 1
  fi
fi

SRC="$(cd "$REPO_SRC" && pwd)"
WORK="$(mkdir -p "$WORKDIR" && cd "$WORKDIR" && pwd)"
BARE="$WORK/source.git"
THIN="$WORK/thin.git"
CHECKOUT="$WORK/checkout"
MIGRATE="$WORK/migrate"

echo "==> source: $SRC"
echo "==> work:   $WORK"

if ! ipfs id >/dev/null 2>&1; then
  echo "ipfs daemon not running; start: ipfs daemon" >&2
  exit 1
fi

if [[ ! -d "$BARE" ]]; then
  echo "==> bare clone"
  git clone --bare "$SRC" "$BARE"
fi

REUSE_MANIFEST="${REUSE_MANIFEST:-}"
if [[ -z "$REUSE_MANIFEST" && -f "$SRC/assets.manifest.json" ]]; then
  REUSE_MANIFEST="$SRC/assets.manifest.json"
fi
echo "==> pin all historical binary blobs (resume with --resume on re-run)"
MIGRATE_ARGS=(--workdir "$MIGRATE" --branch "$BRANCH" --resume)
if [[ -n "$REUSE_MANIFEST" ]]; then
  echo "==> reuse-manifest: $REUSE_MANIFEST"
  MIGRATE_ARGS+=(--reuse-manifest "$REUSE_MANIFEST")
fi
python3 "$SCRIPT_DIR/relay-ipfs-migrate-history.py" "$BARE" "${MIGRATE_ARGS[@]}"

echo "==> git filter-repo on bare mirror"
cd "$BARE"
# Callback loads oid-stubs.json from disk (keeps argv small).
"$GIT_FILTER_REPO" --force --blob-callback "$MIGRATE/blob-callback.py"

echo "==> non-bare checkout for manifest commit"
rm -rf "$CHECKOUT"
git clone "$BARE" "$CHECKOUT"
cd "$CHECKOUT"
git checkout "$BRANCH"
if [[ -f "$MIGRATE/assets.manifest.json" ]]; then
  cp "$MIGRATE/assets.manifest.json" assets.manifest.json
  git add assets.manifest.json
  if ! git diff --cached --quiet; then
    git commit -m "chore: add assets.manifest.json for IPFS-backed media"
  fi
fi

rm -rf "$THIN"
git clone --bare "$CHECKOUT" "$THIN"

echo "==> large blobs remaining on $BRANCH (should be empty):"
git -C "$THIN" rev-list --objects "$BRANCH" \
  | git -C "$THIN" cat-file --batch-check='%(objecttype) %(objectname) %(objectsize)' \
  | awk '/^blob/ { if ($3 > 262144) print $2, $3 }' | head -20 || true

echo "==> verify thin history (no raw binaries, pins present)"
"$SCRIPT_DIR/relay-ipfs-verify-thin.sh" "$THIN" "$MIGRATE/oid-stubs.json"

echo ""
echo "Done."
echo "  Bare thin repo:  $THIN"
echo "  Checkout:        $CHECKOUT"
echo "  IPFS stubs map:  $MIGRATE/oid-stubs.json"
echo ""
echo "Push to new GitLab:"
echo "  cd $CHECKOUT && git remote add origin <NEW_GITLAB_URL>"
echo "  git push -u origin --all && git push origin --tags"

#!/usr/bin/env bash
# Client-side mirror of relay pre-receive: verify each commit in the push range with the same
# rules the server applies (per-commit .relay.yaml, branchRules, git verify-commit).
#
# Usage (after fetching remote ref):
#   ./scripts/verify-push-range.sh /path/to/repo REMOTE BRANCH
# Example:
#   ./scripts/verify-push-range.sh . origin main
#
# Requires: git 2.34+ (SSH signing), gpg.ssh.allowedSignersFile or trust from git verify-commit.
# For full parity with relay, build allowed_signers from .relay.yaml allowedKeys (see docs).

set -euo pipefail
REPO="${1:?repo path}"
REMOTE="${2:?remote name}"
BRANCH="${3:?branch name}"

cd "$REPO"
REMOTE_SHA=$(git rev-parse "$REMOTE/$BRANCH" 2>/dev/null || true)
if [[ -z "${REMOTE_SHA:-}" ]]; then
  echo "No $REMOTE/$BRANCH — nothing to verify against."
  exit 0
fi
LOCAL_SHA=$(git rev-parse "$BRANCH")
if [[ "$REMOTE_SHA" == "$LOCAL_SHA" ]]; then
  echo "Already up to date."
  exit 0
fi

mapfile -t COMMITS < <(git rev-list --reverse "$REMOTE_SHA..$LOCAL_SHA")
if [[ ${#COMMITS[@]} -eq 0 ]]; then
  echo "No commits to push in range (unexpected)."
  exit 1
fi

echo "Verifying ${#COMMITS[@]} commit(s) for push to $BRANCH ..."
for c in "${COMMITS[@]}"; do
  if ! git verify-commit "$c" 2>/dev/null; then
    echo "verify-commit failed: $c"
    exit 1
  fi
done
echo "OK (signatures verify). Server may still enforce branchRules / allowlist — use relay policy docs."

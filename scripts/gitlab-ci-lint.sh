#!/usr/bin/env bash
# Lint .gitlab-ci.yml via GitLab API (requires a project access token with api scope).
# Usage (from repo root):
#   export GITLAB_TOKEN="glpat-…"
#   export GITLAB_PROJECT_ID="12345678"   # numeric id from project → Settings → General
#   ./scripts/gitlab-ci-lint.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
: "${GITLAB_TOKEN:?set GITLAB_TOKEN}"
: "${GITLAB_PROJECT_ID:?set GITLAB_PROJECT_ID (numeric project id)}"
HOST="${GITLAB_HOST:-https://gitlab.com}"
CONTENT="$(jq -Rs . < "$ROOT/.gitlab-ci.yml")"
curl -sS -X POST "${HOST}/api/v4/projects/${GITLAB_PROJECT_ID}/ci/lint?dry_run=true" \
  -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"content\":${CONTENT}}" | jq .

#!/usr/bin/env bash
# Apply shared SSR HTML forbid markers (docs/examples/ssr-qa.markers.json) to a
# file or stdin. Used by park-local / smoke so shell checks match Rust
# ssr_html_quality::FORBIDDEN_HARD.
#
# Usage:
#   ssr-qa-check-html.sh [--markers path.json] [--label name] <html-file|->
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MARKERS="${SSR_QA_MARKERS:-$SCRIPT_DIR/../docs/examples/ssr-qa.markers.json}"
LABEL="html"
HTML_PATH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --markers) MARKERS="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,8p' "$0"
      exit 0
      ;;
    --) shift; break ;;
    -)
      # stdin sentinel (must precede -* so bare "-" is not "unknown")
      HTML_PATH="-"
      shift
      ;;
    -*)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
    *)
      HTML_PATH="$1"
      shift
      ;;
  esac
done

if [[ -z "$HTML_PATH" ]]; then
  echo "usage: $0 [--markers path] [--label name] <html-file|->" >&2
  exit 2
fi

if [[ ! -f "$MARKERS" ]]; then
  echo "FAIL: markers file missing: $MARKERS" >&2
  exit 1
fi

BODY="$(
  if [[ "$HTML_PATH" == "-" ]]; then
    cat
  else
    cat "$HTML_PATH"
  fi
)"

# Prefer Python for JSON; fall back to hard-coded forbids matching Rust.
FORBIDS="$(python3 - "$MARKERS" <<'PY' 2>/dev/null || true
import json, sys
p = sys.argv[1]
d = json.load(open(p, encoding="utf-8"))
for n in d.get("forbid_always") or []:
    if n:
        print(n)
PY
)"
if [[ -z "$FORBIDS" ]]; then
  FORBIDS=$'relay-error-boundary\ndata-error-boundary\nRuntime error\nProperty \'origin\' not found\nCleverScript hook panicked\nssrClientHookFromGetContext disabled'
fi

while IFS= read -r needle; do
  [[ -z "$needle" ]] && continue
  case "$BODY" in
    *"$needle"*)
      echo "FAIL: ${LABEL}: forbidden marker present: ${needle}" >&2
      exit 1
      ;;
  esac
done <<<"$FORBIDS"

echo "ok: ${LABEL} (forbid markers clean)"

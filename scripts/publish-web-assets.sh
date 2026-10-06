#!/usr/bin/env bash
# Publish relay-client-web embed+WASM (no React) into relay-web-assets.
# See docs/RELAY_WEB_ASSETS.md.
set -euo pipefail

DIST=""
ASSETS_REPO=""
MESSAGE=""
ALLOW_DIRTY=0
PUSH=1
BRANCH="release"

usage() {
  cat <<'EOF'
Usage: publish-web-assets.sh --dist DIR --assets-repo DIR [options]

Publishes the WASM embed graph only — never main.js / React SPA.
React belongs in a separate future release line, not this deployment.

  --dist DIR           Built tree (relay-client-web/dist)
  --assets-repo DIR    Checkout of relay-web-assets (not bare)
  --message MSG        Commit message (default: publish web assets)
  --branch NAME        Checkout/push this branch (default: release)
  --allow-dirty        Allow uncommitted monorepo sources
  --no-push            Commit only; do not push
  -h, --help           This help

Requires: rsync, git, node. Prefer signed commits (gpg.format=ssh + commit.gpgsign).
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dist) DIST="${2:?}"; shift 2 ;;
    --assets-repo) ASSETS_REPO="${2:?}"; shift 2 ;;
    --message) MESSAGE="${2:?}"; shift 2 ;;
    --branch) BRANCH="${2:?}"; shift 2 ;;
    --allow-dirty) ALLOW_DIRTY=1; shift ;;
    --no-push) PUSH=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ -z "$DIST" || -z "$ASSETS_REPO" ]]; then
  usage
  exit 2
fi
if [[ ! -f "$DIST/assets/embed.js" ]]; then
  echo "error: missing $DIST/assets/embed.js — run npm run build in relay-client-web" >&2
  exit 1
fi
if [[ ! -d "$DIST/wasm/relay_runtime_pkg" ]]; then
  echo "error: missing $DIST/wasm/relay_runtime_pkg" >&2
  exit 1
fi
if [[ ! -d "$ASSETS_REPO/.git" ]]; then
  echo "error: $ASSETS_REPO is not a git checkout" >&2
  exit 1
fi

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
GATEWAY_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
MONOREPO=$(cd "$GATEWAY_ROOT/.." && pwd)
CLIENT_WEB="$MONOREPO/relay-client-web"

# Fail closed: embed graph must not contain React (same gate as client-web build).
if [[ -f "$CLIENT_WEB/scripts/assert-embed-no-react.mjs" ]]; then
  (cd "$CLIENT_WEB" && node scripts/assert-embed-no-react.mjs)
else
  echo "error: missing assert-embed-no-react.mjs" >&2
  exit 1
fi

if [[ "$ALLOW_DIRTY" -ne 1 ]]; then
  if git -C "$MONOREPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if [[ -n "$(git -C "$MONOREPO" status --porcelain -- relay-client-web relay-runtime 2>/dev/null)" ]]; then
      echo "error: dirty relay-client-web / relay-runtime — commit or pass --allow-dirty" >&2
      exit 1
    fi
  fi
fi

git -C "$ASSETS_REPO" checkout "$BRANCH"

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/assets" "$STAGE/wasm"

# Copy WASM package wholesale (strip wasm-pack .gitignore that ignores `*`).
cp -a "$DIST/wasm/relay_runtime_pkg" "$STAGE/wasm/"
rm -f "$STAGE/wasm/relay_runtime_pkg/.gitignore"

# Optional relay-video demux WASM (built via relay-video/apps/web `npm run build:wasm`).
mkdir -p "$STAGE/wasm/relay_video_wasm_pkg"
if [[ -d "$DIST/wasm/relay_video_wasm_pkg" ]]; then
  cp -a "$DIST/wasm/relay_video_wasm_pkg/." "$STAGE/wasm/relay_video_wasm_pkg/"
elif [[ -d "$MONOREPO/relay-web-assets/wasm/relay_video_wasm_pkg" ]]; then
  cp -a "$MONOREPO/relay-web-assets/wasm/relay_video_wasm_pkg/." "$STAGE/wasm/relay_video_wasm_pkg/"
elif [[ -d "$MONOREPO/relay-video/apps/web/pkg" ]]; then
  cp -a "$MONOREPO/relay-video/apps/web/pkg/." "$STAGE/wasm/relay_video_wasm_pkg/"
fi
if [[ -f "$MONOREPO/relay-video/apps/web/src/player.js" ]]; then
  cp -a "$MONOREPO/relay-video/apps/web/src/player.js" "$STAGE/wasm/relay_video_wasm_pkg/player.js"
fi
if [[ -f "$STAGE/wasm/relay_video_wasm_pkg/relay_video_wasm_bg.wasm" && -f "$STAGE/wasm/relay_video_wasm_pkg/player.js" ]]; then
  rm -f "$STAGE/wasm/relay_video_wasm_pkg/.gitignore"
  echo "[publish-web-assets] staged relay_video_wasm_pkg"
else
  rm -rf "$STAGE/wasm/relay_video_wasm_pkg"
  echo "[publish-web-assets] note: relay_video_wasm_pkg not present — skipping"
fi

# Copy embed entry CSS + walk embed.js import graph (relative + /assets/).
DIST="$DIST" STAGE="$STAGE" node --input-type=module <<'EOF'
import fs from 'fs'
import path from 'path'

const dist = process.env.DIST
const stage = process.env.STAGE
const assetsDir = path.join(dist, 'assets')
const stageAssets = path.join(stage, 'assets')
const entry = path.join(assetsDir, 'embed.js')

function resolveImport(fromFile, spec) {
  if (spec.startsWith('/assets/')) {
    return path.normalize(path.join(dist, spec.slice(1)))
  }
  if (spec.startsWith('./') || spec.startsWith('../')) {
    return path.normalize(path.join(path.dirname(fromFile), spec))
  }
  return null
}

const seen = new Set()
const queue = [entry]
const importRe =
  /(?:from|import)\s*\(\s*["']([^"']+)["']\s*\)|from\s*["']([^"']+)["']|import\s*["']([^"']+)["']/g

while (queue.length) {
  const file = queue.shift()
  if (seen.has(file)) continue
  if (!fs.existsSync(file)) throw new Error('missing module: ' + file)
  seen.add(file)
  const text = fs.readFileSync(file, 'utf8')
  for (const m of text.matchAll(importRe)) {
    const spec = m[1] || m[2] || m[3]
    if (!spec) continue
    const next = resolveImport(file, spec)
    if (!next || !next.startsWith(assetsDir)) continue
    if (!seen.has(next)) queue.push(next)
  }
}

for (const file of seen) {
  const rel = path.relative(assetsDir, file)
  const dest = path.join(stageAssets, rel)
  fs.mkdirSync(path.dirname(dest), { recursive: true })
  fs.copyFileSync(file, dest)
  const map = file + '.map'
  if (fs.existsSync(map)) fs.copyFileSync(map, dest + '.map')
}

for (const name of ['embed.css', 'embed.css.map']) {
  const src = path.join(assetsDir, name)
  if (fs.existsSync(src)) fs.copyFileSync(src, path.join(stageAssets, name))
}

for (const forbidden of ['main.js', 'main.css', 'index.css']) {
  const p = path.join(stageAssets, forbidden)
  if (fs.existsSync(p)) throw new Error('refusing to publish React SPA file: ' + forbidden)
}
console.log('[publish-web-assets] staged', seen.size, 'JS module(s) + embed.css (no main.js)')
EOF

# Replace published tree (keep .relay.yaml / README / .git*)
# Delete previous assets/wasm content, then copy stage.
rm -rf "$ASSETS_REPO/assets" "$ASSETS_REPO/wasm" "$ASSETS_REPO/index.html" "$ASSETS_REPO/hooks"
mkdir -p "$ASSETS_REPO"
rsync -a "$STAGE"/ "$ASSETS_REPO"/

test -f "$ASSETS_REPO/assets/embed.js"
test ! -e "$ASSETS_REPO/assets/main.js"
test -d "$ASSETS_REPO/wasm/relay_runtime_pkg"

CLIENT_SHA=$(git -C "$MONOREPO" rev-parse HEAD 2>/dev/null || echo "unknown")
RUNTIME_SHA=$(git -C "$MONOREPO/relay-runtime" rev-parse HEAD 2>/dev/null || echo "$CLIENT_SHA")
cat >"$ASSETS_REPO/MANIFEST.json" <<EOF
{
  "schema": "relay.web-assets.v1",
  "profile": "embed-no-react",
  "builtAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "source": {
    "monorepoCommit": "$CLIENT_SHA",
    "relayRuntimeCommit": "$RUNTIME_SHA"
  }
}
EOF

MESSAGE=${MESSAGE:-"chore(web-assets): publish embed+wasm (no React)"}

git -C "$ASSETS_REPO" add -A
if git -C "$ASSETS_REPO" diff --cached --quiet; then
  echo "no changes to publish"
  exit 0
fi

git -C "$ASSETS_REPO" commit -m "$MESSAGE"
OID=$(git -C "$ASSETS_REPO" rev-parse HEAD)
echo "published commit $OID (profile=embed-no-react)"

if [[ "$PUSH" -eq 1 ]]; then
  git -C "$ASSETS_REPO" remote | while read -r remote; do
    echo "pushing to $remote…"
    git -C "$ASSETS_REPO" push "$remote" "HEAD:refs/heads/$BRANCH" || true
  done
fi

echo "HTML should stamp /assets/c/$OID/embed.js after mesh fetch."

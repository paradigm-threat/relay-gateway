#!/usr/bin/env bash
# Build release binaries and pack for Linode bare-metal install.
# Local deps: stable Rust (Cargo.lock v4+), build-essential, pkg-config, libssl-dev (or OpenSSL dev kit).
#
# By default refuses dirty or unpushed gateway / relay-runtime / relay-client-web trees
# so fleet artifacts are built from published SHAs. Override with RELAY_PACK_ALLOW_DIRTY=1.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
CLIENT_WEB="$ROOT/../relay-client-web"
RUNTIME="$ROOT/../relay-runtime"
if [[ ! -f "$CLIENT_WEB/package.json" ]]; then
  echo "relay-client-web not found at $CLIENT_WEB" >&2
  exit 1
fi

require_clean_pushed() {
  local dir="$1" label="$2"
  [[ -d "$dir/.git" ]] || { echo "missing git repo: $dir" >&2; return 1; }
  if [[ -n "$(git -C "$dir" status --porcelain)" ]]; then
    echo "ERROR: $label has uncommitted changes (set RELAY_PACK_ALLOW_DIRTY=1 to override)" >&2
    git -C "$dir" status -sb >&2
    return 1
  fi
  if ! git -C "$dir" rev-parse --abbrev-ref '@{upstream}' >/dev/null 2>&1; then
    echo "ERROR: $label has no upstream tracking branch" >&2
    return 1
  fi
  local behind ahead
  read -r behind ahead < <(git -C "$dir" rev-list --left-right --count '@{upstream}...HEAD')
  if [[ "$behind" != 0 || "$ahead" != 0 ]]; then
    echo "ERROR: $label not synced with upstream (behind=$behind ahead=$ahead); push/pull first" >&2
    return 1
  fi
}

if [[ "${RELAY_PACK_ALLOW_DIRTY:-0}" != "1" ]]; then
  require_clean_pushed "$ROOT" "relay-gateway"
  require_clean_pushed "$RUNTIME" "relay-runtime"
  require_clean_pushed "$CLIENT_WEB" "relay-client-web"
fi

GW_SHA="$(git -C "$ROOT" rev-parse HEAD)"
RT_SHA="$(git -C "$RUNTIME" rev-parse HEAD)"
WEB_SHA="$(git -C "$CLIENT_WEB" rev-parse HEAD)"
BUILT_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# `npm run build` runs build:wasm first (WASM is gitignored) so fleet packs always hydrate.
npm --prefix "$CLIENT_WEB" run build
test -f "$CLIENT_WEB/dist/assets/embed.js"
test -f "$CLIENT_WEB/dist/wasm/relay_runtime_pkg/relay_runtime_bg.wasm"
cargo build --release --bin relay-gateway --bin relay-hook-handler --bin relay-git-proxy --bin relay-ssr-bake
DIST="$ROOT/target/linode-deploy"
rm -rf "$DIST"
mkdir -p "$DIST"
cp target/release/relay-gateway target/release/relay-hook-handler target/release/relay-git-proxy target/release/relay-ssr-bake "$DIST/"
cp "$ROOT/scripts/relay-install.sh" "$DIST/install.sh"
cp "$ROOT/scripts/piper-tts-http.py" "$DIST/piper-tts-http.py"
cp "$ROOT/scripts/relay-probe-features.py" "$DIST/relay-probe-features.py"
cp "$ROOT/scripts/relay-bootstrap.sh" "$DIST/relay-bootstrap.sh"
cp "$ROOT/scripts/relay-startup-catalog-sync.sh" "$DIST/relay-startup-catalog-sync.sh"
cp "$ROOT/scripts/relay-gateway-liveness.sh" "$DIST/relay-gateway-liveness.sh"
cp "$ROOT/scripts/relay-reconcile.sh" "$DIST/relay-reconcile.sh"
cp "$ROOT/scripts/install-caddy-tls.sh" "$DIST/install-caddy-tls.sh"
cp "$ROOT/scripts/caddy-ask.py" "$DIST/caddy-ask.py"
cp -a "$CLIENT_WEB/dist" "$DIST/www"
cp "$ROOT/scripts/relay-ipfs-pin-catalog" "$DIST/relay-ipfs-pin-catalog"
chmod +x "$DIST/install.sh" "$DIST/relay-bootstrap.sh" "$DIST/relay-startup-catalog-sync.sh" "$DIST/relay-gateway-liveness.sh" "$DIST/relay-reconcile.sh" "$DIST/install-caddy-tls.sh" "$DIST/relay-probe-features.py" "$DIST/relay-ipfs-pin-catalog"

GW_HASH="$(sha256sum "$DIST/relay-gateway" | awk '{print $1}')"
HOOK_HASH="$(sha256sum "$DIST/relay-hook-handler" | awk '{print $1}')"
PROXY_HASH="$(sha256sum "$DIST/relay-git-proxy" | awk '{print $1}')"
cat >"$DIST/deploy-manifest.json" <<EOF
{
  "schema": "relay.deploy-manifest.v1",
  "built_at_utc": "$BUILT_AT",
  "gateway_version": "$(cargo metadata --no-deps --format-version 1 2>/dev/null | jq -r '.packages[] | select(.name=="relay-gateway") | .version' 2>/dev/null || echo 0.3.15)",
  "source": {
    "relay_gateway": "$GW_SHA",
    "relay_runtime": "$RT_SHA",
    "relay_client_web": "$WEB_SHA"
  },
  "artifacts": {
    "relay_gateway_sha256": "$GW_HASH",
    "relay_hook_handler_sha256": "$HOOK_HASH",
    "relay_git_proxy_sha256": "$PROXY_HASH"
  }
}
EOF
cp "$DIST/deploy-manifest.json" "$DIST/www/deploy-manifest.json" 2>/dev/null || true

tar -czvf "$ROOT/target/relay-linode-deploy.tgz" -C "$DIST" .
TGZ_HASH="$(sha256sum "$ROOT/target/relay-linode-deploy.tgz" | awk '{print $1}')"
jq --arg h "$TGZ_HASH" '.artifacts.tarball_sha256=$h' "$DIST/deploy-manifest.json" >"$DIST/deploy-manifest.json.tmp"
mv "$DIST/deploy-manifest.json.tmp" "$DIST/deploy-manifest.json"
# Refresh tarball with final manifest (small; acceptable second pack of same tree).
tar -czf "$ROOT/target/relay-linode-deploy.tgz" -C "$DIST" .

echo "Created $ROOT/target/relay-linode-deploy.tgz"
echo "Deploy manifest:"
cat "$DIST/deploy-manifest.json"
echo "Fleet deploy: ./scripts/relay-fleet.sh --build deploy-all   (or: RELAY_SSH_USER=ari ./scripts/relay-linode-redeploy-seed.sh YOUR_FQDN)"
echo "  (or first-boot root image: scp … root@IP:/root/ && ssh root@IP 'cd /root && tar xzf relay-linode-deploy.tgz && ./install.sh install')"

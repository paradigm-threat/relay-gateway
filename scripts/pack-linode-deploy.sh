#!/usr/bin/env bash
# Build release binaries and pack for Linode bare-metal install.
# Local deps: stable Rust (Cargo.lock v4+), build-essential, pkg-config, libssl-dev (or OpenSSL dev kit).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build --release --bin relay-gateway --bin relay-hook-handler
DIST="$ROOT/target/linode-deploy"
rm -rf "$DIST"
mkdir -p "$DIST"
cp target/release/relay-gateway target/release/relay-hook-handler "$DIST/"
cp "$ROOT/scripts/relay-install.sh" "$DIST/install.sh"
cp "$ROOT/scripts/piper-tts-http.py" "$DIST/piper-tts-http.py"
cp "$ROOT/scripts/relay-probe-features.py" "$DIST/relay-probe-features.py"
cp "$ROOT/scripts/relay-bootstrap.sh" "$DIST/relay-bootstrap.sh"
cp "$ROOT/scripts/relay-startup-catalog-sync.sh" "$DIST/relay-startup-catalog-sync.sh"
cp "$ROOT/scripts/relay-reconcile.sh" "$DIST/relay-reconcile.sh"
cp "$ROOT/scripts/install-caddy-tls.sh" "$DIST/install-caddy-tls.sh"
cp "$ROOT/scripts/caddy-ask.py" "$DIST/caddy-ask.py"
chmod +x "$DIST/install.sh" "$DIST/relay-bootstrap.sh" "$DIST/relay-startup-catalog-sync.sh" "$DIST/relay-reconcile.sh" "$DIST/install-caddy-tls.sh" "$DIST/relay-probe-features.py"
tar -czvf "$ROOT/target/relay-linode-deploy.tgz" -C "$DIST" .
echo "Created $ROOT/target/relay-linode-deploy.tgz"
echo "Fleet deploy: ./scripts/relay-fleet.sh --build deploy-all   (or: RELAY_SSH_USER=ari ./scripts/relay-linode-redeploy-seed.sh YOUR_FQDN)"
echo "  (or first-boot root image: scp … root@IP:/root/ && ssh root@IP 'cd /root && tar xzf relay-linode-deploy.tgz && ./install.sh install')"

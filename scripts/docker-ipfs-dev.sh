#!/usr/bin/env bash
# Local relay-gateway + IPFS dev stack.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GW_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$GW_DIR/docker-compose.ipfs-dev.yml"
HOST="${RELAY_DEV_HOSTNAME:-localhost}"
HTTP_PORT="${RELAY_DEV_HTTP_PORT:-18080}"
REPO="${RELAY_DEV_REPO:-paradigm-threat-files}"
TEST_PATH="${RELAY_DEV_TEST_PATH:-governance/genocide/agenda.jpg}"

log() { echo "[docker-ipfs-dev] $*" >&2; }

cmd="${1:-help}"

check_host_ipfs() {
  if ! curl -sf --max-time 3 "http://127.0.0.1:8080/ipfs/bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi" >/dev/null 2>&1; then
    log "WARN: host Kubo gateway :8080 did not return a known test CID"
    log "      Start: ipfs daemon   (pins from thin migration must be on this node)"
    return 1
  fi
  log "OK: host Kubo gateway :8080 serves pinned content"
  return 0
}

case "$cmd" in
  prepare)
    "$SCRIPT_DIR/docker-ipfs-dev-prepare.sh"
    ;;
  up)
    "$SCRIPT_DIR/docker-ipfs-dev-prepare.sh"
    check_host_ipfs || true
    if [[ ! -x "$GW_DIR/target/release/relay-gateway" ]]; then
      log "Building relay-gateway on host (cargo build --release)..."
      (cd "$GW_DIR" && cargo build --release --bin relay-gateway --bin relay-hook-handler)
    fi
    export RELAY_IPFS_GATEWAY="${RELAY_IPFS_GATEWAY:-http://127.0.0.1:8080}"
    cd "$GW_DIR"
    docker compose -f docker-compose.ipfs-dev.yml up -d --build relay-gateway
    log "Waiting for relay-gateway health..."
    for _ in $(seq 1 40); do
      if curl -sf "http://127.0.0.1:${HTTP_PORT}/api/config" >/dev/null; then
        break
      fi
      sleep 2
    done
    log "Relay: http://127.0.0.1:${HTTP_PORT}/api/config"
    log "Test:  ./scripts/docker-ipfs-dev.sh test"
    ;;
  up-bundled)
    export RELAY_IPFS_GATEWAY=http://ipfs:8080
    "$SCRIPT_DIR/docker-ipfs-dev-prepare.sh"
    cd "$GW_DIR"
    docker compose -f docker-compose.ipfs-dev.yml --profile bundled-ipfs up -d --build
    log "Bundled Kubo on :8081; run: ./scripts/docker-ipfs-dev.sh sync-pins"
    ;;
  down)
    cd "$GW_DIR"
    docker compose -f docker-compose.ipfs-dev.yml --profile bundled-ipfs down
    ;;
  logs)
    cd "$GW_DIR"
    docker compose -f docker-compose.ipfs-dev.yml logs -f relay-gateway
    ;;
  sync-pins)
    # Pin one test CID into bundled container Kubo from host gateway
    CID="${2:-bafkreicpntnqc3fqmkoebsk6jnsai6tjc5kuftyhw2r5qxyh7ghsnsuwpq}"
    log "Pinning $CID into relay-ipfs-dev via host gateway..."
    docker exec relay-ipfs-dev ipfs pin add "/ipfs/$CID" 2>/dev/null || \
      curl -sf "http://127.0.0.1:8080/ipfs/$CID" | docker exec -i relay-ipfs-dev ipfs add --pin --quieter | tail -1
    ;;
  test)
    URL="http://127.0.0.1:${HTTP_PORT}/${TEST_PATH}"
    log "GET $URL (Host: ${REPO}.${HOST})"
    curl -sSI -H "Host: ${REPO}.${HOST}" "$URL" | head -20
    log "---"
    curl -sSI -H "Host: ${REPO}.${HOST}" "${URL}?w=128" | head -20
    ;;
  help|*)
    cat <<EOF
Usage: $0 <command>

  prepare     Sync thin bare repos into docker-data/; overlay IPFS get.cs from checkout
  up          prepare + build relay-gateway (uses host Kubo at host.docker.internal:8080)
  up-bundled  Same with Kubo in Docker (profile bundled-ipfs); run sync-pins after
  sync-pins [cid]  Pin a CID into bundled Kubo (for up-bundled mode)
  test        curl asset + ?w= resize via GET hook
  down        docker compose down
  logs        follow relay-gateway logs

Env:
  RELAY_DEV_HTTP_PORT   default 18080
  RELAY_DEV_HOSTNAME    default localhost  (Host: {repo}.localhost)
  RELAY_DEV_REPO        default paradigm-threat-files
  RELAY_DEV_TEST_PATH   default governance/genocide/agenda.jpg
  RELAY_IPFS_GATEWAY    override gateway URL for relay container

Before 'up': run \`ipfs daemon\` on the host with thin-repo pins.
EOF
    ;;
esac

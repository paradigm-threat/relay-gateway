#!/usr/bin/env bash
# Loopback liveness for relay-gateway. SSR can fill the :8080 accept queue
# (~10Gi RSS) while Caddy :443 still answers — UptimeRobot then sees timeout/502
# on the parked apex. MemoryHigh does not restart; this timer does.
#
# Two consecutive loopback failures, or Recv-Q >= 64, → systemctl restart.
# Installed as relay-gateway-liveness.timer (every 60s). Run as root.
set -euo pipefail

UNIT="${RELAY_GATEWAY_UNIT:-relay-gateway}"
PORT="${RELAY_HTTP_PORT:-8080}"
URL="${RELAY_LIVENESS_URL:-http://127.0.0.1:${PORT}/api/status}"
STATE="${RELAY_LIVENESS_STATE:-/run/relay-gateway-liveness.fail}"
RECVQ_LIMIT="${RELAY_LIVENESS_RECVQ:-64}"

log() { logger -t relay-gateway-liveness "$*"; echo "[relay-gateway-liveness] $*" >&2; }

# park_tls (old binary) rewrites Caddyfile.parked with a short upstream timeout.
# 8s 504s directory SSR (~18s). Raise anything under 35s and fill a missing block.
_repatch_caddy_timeouts() {
  [[ "$(id -u)" -eq 0 ]] || return 0
  local p t changed=0
  for p in /etc/caddy/Caddyfile /opt/relay/state/park-tls/Caddyfile.parked; do
    [[ -f "$p" ]] || continue
    if grep -qE 'response_header_timeout (8|25)s' "$p"; then
      sed -i -e 's/response_header_timeout 8s/response_header_timeout 35s/g' \
        -e 's/response_header_timeout 25s/response_header_timeout 35s/g' "$p"
      changed=1
      log "raised Caddy response_header_timeout in $p"
      continue
    fi
    grep -q response_header_timeout "$p" && continue
    t="$(sed 's/reverse_proxy 127.0.0.1:8080/reverse_proxy 127.0.0.1:8080 {\n\t\ttransport http {\n\t\t\tdial_timeout 2s\n\t\t\tresponse_header_timeout 35s\n\t\t}\n\t}/g' "$p")"
    if [[ "$t" != "$(cat "$p")" ]]; then
      printf '%s' "$t" >"$p"
      changed=1
      log "re-applied Caddy upstream timeouts in $p"
    fi
  done
  if [[ "$changed" -eq 1 ]]; then
    caddy reload --config /etc/caddy/Caddyfile >/dev/null 2>&1 || true
  fi
}

sub="$(systemctl show "$UNIT" -p SubState --value 2>/dev/null || true)"
case "$sub" in
  start|start-pre|start-post|auto-restart|reload|stop-sigterm|stop-sigkill|final-sigterm)
    exit 0
    ;;
esac

if ! systemctl is-active --quiet "$UNIT"; then
  rm -f "$STATE"
  exit 0
fi

recvq="$(ss -ltn "sport = :${PORT}" 2>/dev/null | awk 'NR==2 {print $2; exit}')"
recvq="${recvq:-0}"
if [[ "$recvq" =~ ^[0-9]+$ ]] && (( recvq >= RECVQ_LIMIT )); then
  log "Recv-Q=${recvq} on :${PORT} (limit ${RECVQ_LIMIT}) — restarting ${UNIT}"
  rm -f "$STATE"
  systemctl restart "$UNIT"
  exit 0
fi

if curl -sS -m 3 -o /dev/null -X POST "$URL"; then
  rm -f "$STATE"
  _repatch_caddy_timeouts
  exit 0
fi

if [[ -f "$STATE" ]]; then
  log "loopback ${URL} failed twice — restarting ${UNIT}"
  rm -f "$STATE"
  systemctl restart "$UNIT"
  exit 0
fi

: >"$STATE"
log "loopback ${URL} failed (1/2)"

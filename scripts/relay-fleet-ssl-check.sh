#!/usr/bin/env bash
# Fleet / parked-domain SSL health report for relay-gateway operators.
# Used by relay-fleet.sh (ssl / health / pre+post update). Does not mutate fleet state.
#
# Exit codes:
#   0 — all required checks green (warnings allowed unless RELAY_FLEET_SSL_STRICT=1)
#   1 — one or more required checks failed (or warnings treated as fail under STRICT)
#
# Env:
#   RELAY_FLEET_SSL_DOMAINS   space-separated park Hosts (default: catalog-style PT + clevertree)
#   RELAY_FLEET_SSL_NODES     space-separated node FQDNs (default: RELAY_OPERATOR_FLEET / peers)
#   RELAY_FLEET_SSL_STRICT=1  treat WARN as FAIL
#   RELAY_FLEET_SSL_TIMEOUT   curl/openssl timeout seconds (default 20)
#   RELAY_FLEET_SSL_LABEL     optional prefix for log lines (e.g. pre-deploy / post-deploy)
set -euo pipefail

TIMEOUT="${RELAY_FLEET_SSL_TIMEOUT:-20}"
STRICT="${RELAY_FLEET_SSL_STRICT:-0}"
LABEL="${RELAY_FLEET_SSL_LABEL:-ssl}"
DOMAINS="${RELAY_FLEET_SSL_DOMAINS:-relay.paradigmthreat.net clevertree.net www.clevertree.net relay.clevertree.net}"
NODES="${RELAY_FLEET_SSL_NODES:-}"

if [[ -z "$NODES" ]]; then
  NODES="${RELAY_OPERATOR_FLEET:-${RELAY_MASTER_PEER_LIST:-}}"
  NODES="${NODES//;/ }"
  NODES="${NODES//,/ }"
fi

fail=0
warn=0

log() { echo "[relay-fleet:$LABEL] $*" >&2; }
ok() { log "OK  $*"; }
warn_msg() { log "WARN $*"; warn=$((warn + 1)); }
fail_msg() { log "FAIL $*"; fail=$((fail + 1)); }

# HTTPS GET/HEAD must complete TLS + HTTP. Returns 0 on success.
check_https_url() {
  local url="$1" expect_ct="${2:-}"
  local code
  code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT" -L "$url" 2>/dev/null || echo 000)"
  if [[ "$code" == "000" ]]; then
    return 1
  fi
  if [[ -n "$expect_ct" ]]; then
    local ct
    ct="$(curl -sSI --max-time "$TIMEOUT" "$url" 2>/dev/null | tr -d '\r' | awk -F': ' 'tolower($1)=="content-type"{print $2; exit}')"
    case "$ct" in
      *"$expect_ct"*) ;;
      *) return 2 ;;
    esac
  fi
  return 0
}

# openssl: peer cert present for SNI name (via DNS or --resolve IP).
check_tls_handshake() {
  local host="$1" ip="${2:-}"
  local out
  if [[ -n "$ip" ]]; then
    out="$(echo | openssl s_client -connect "${ip}:443" -servername "$host" 2>/dev/null | head -30)" || true
  else
    out="$(echo | openssl s_client -connect "${host}:443" -servername "$host" 2>/dev/null | head -30)" || true
  fi
  if echo "$out" | grep -q 'BEGIN CERTIFICATE'; then
    return 0
  fi
  return 1
}

cert_summary() {
  local host="$1"
  echo | openssl s_client -connect "${host}:443" -servername "$host" 2>/dev/null \
    | openssl x509 -noout -subject -dates -ext subjectAltName 2>/dev/null \
    | tr '\n' ' ' | sed 's/  */ /g' || true
}

log "begin domains=[${DOMAINS}] nodes=[${NODES}] strict=${STRICT}"

# --- node FQDN HTTPS (fleet ingress always needs these) ---
for h in $NODES; do
  [[ -n "$h" ]] || continue
  if check_https_url "https://${h}/api/config"; then
    # POST is the real config API; GET may 404 — still proves TLS.
    if curl -sfS --max-time "$TIMEOUT" -X POST "https://${h}/api/config" >/dev/null; then
      ok "node https+config $h"
    else
      warn_msg "node TLS up but POST /api/config failed $h"
    fi
  else
    fail_msg "node HTTPS broken $h (tls or reachability)"
  fi
done

# --- parked catalog domains (public HTTPS) ---
for d in $DOMAINS; do
  [[ -n "$d" ]] || continue
  # Plain HTTP should work even when TLS is broken (routing vs cert).
  http_code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT" -L "http://${d}/" 2>/dev/null || echo 000)"
  if [[ "$http_code" == "000" ]]; then
    warn_msg "park HTTP unreachable $d"
  elif [[ "$http_code" =~ ^[45] ]]; then
    warn_msg "park HTTP $http_code $d"
  else
    ok "park HTTP $http_code $d"
  fi

  if check_tls_handshake "$d"; then
    if check_https_url "https://${d}/"; then
      ok "park HTTPS $d ($(cert_summary "$d" | cut -c1-120))"
    else
      fail_msg "park TLS handshake ok but HTTPS HTTP failed $d"
    fi
  else
    fail_msg "park HTTPS TLS broken $d (no peer cert / handshake alert — often missing server.tls.plugin: park or caddy-ask 403)"
    # Per-node force: useful when DNS points at one master but peers should still terminate.
    for h in $NODES; do
      [[ -n "$h" ]] || continue
      ip="$(dig +short "$h" A 2>/dev/null | head -1 || true)"
      [[ -n "$ip" ]] || continue
      if check_tls_handshake "$d" "$ip"; then
        ok "park TLS via $h ($ip) for SNI $d"
      else
        warn_msg "park TLS via $h ($ip) also broken for SNI $d"
      fi
    done
  fi
done

log "summary fail=$fail warn=$warn"

if [[ "$fail" -gt 0 ]]; then
  exit 1
fi
case "${STRICT,,}" in
  1|yes|true|on)
    [[ "$warn" -eq 0 ]] || exit 1
    ;;
esac
exit 0

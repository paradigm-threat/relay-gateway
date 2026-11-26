#!/usr/bin/env bash
# Pre/post credit scale-down readiness: peer list, status indexes, journal tails.
# Usage (from relay-gateway/):
#   ./scripts/relay-scale-down-check.sh
#   ./scripts/relay-scale-down-check.sh --post
#   PUB1_IP=… MESH1_IP=… MESH2_IP=… ./scripts/relay-scale-down-check.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KEY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
OUT="${RELAY_SCALE_DOWN_LOG:-$ROOT/ops/scale-down-last-check.log}"
POST=0
for a in "$@"; do
  case "$a" in
    --post) POST=1 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
  esac
done

PUB1_IP="${PUB1_IP:-${LINODE_PUB1_IP:-172.234.148.70}}"
MESH1_IP="${MESH1_IP:-${LINODE_MESH1_IP:-172.237.134.22}}"
MESH2_IP="${MESH2_IP:-${LINODE_MESH2_IP:-172.236.146.220}}"
SSH=(ssh -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8)

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{
  echo "=== relay-scale-down-check $ts post=$POST ==="
  if [[ -f "$ROOT/ops/credit_burn.json" ]]; then
    python3 - "$ROOT/ops/credit_burn.json" <<'PY'
import json, sys, datetime
p = json.load(open(sys.argv[1]))
exp = datetime.datetime.fromisoformat(p["expiresAt"].replace("Z", "+00:00"))
prov = datetime.datetime.fromisoformat(p["provisionedAt"] + "T00:00:00+00:00")
now = datetime.datetime.now(datetime.timezone.utc)
days_left_credit = (exp - now).total_seconds() / 86400
elapsed = (now - prov).total_seconds() / 86400
runway = float(p.get("fullFleetDaysApprox", 29))
days_to_scale = runway - elapsed
print(f"promo={p.get('promo')} credit_usd={p.get('creditUsd')} expires={p.get('expiresAt')}")
print(f"days_until_credit_expiry={days_left_credit:.1f}")
print(f"days_since_provision={elapsed:.1f}  approx_days_to_scale_down_target={days_to_scale:.1f}")
print(f"note={p.get('notes','')}")
PY
  fi
  echo
} | tee "$OUT"

remote_probe() {
  local label="$1" ip="$2" expect_keep="$3"
  echo "--- $label ($ip) keep=$expect_keep ---" | tee -a "$OUT"
  if ! "${SSH[@]}" "root@$ip" true 2>/dev/null; then
    echo "SSH: DOWN" | tee -a "$OUT"
    if [[ "$expect_keep" == "1" && "$POST" == "1" ]]; then
      echo "FAIL: publisher unreachable after scale-down" | tee -a "$OUT"
      return 1
    fi
    return 0
  fi
  "${SSH[@]}" "root@$ip" bash -s <<'REMOTE' | tee -a "$OUT"
set -euo pipefail
echo "hostname=$(hostname -f 2>/dev/null || hostname)"
echo "uptime=$(uptime -p 2>/dev/null || uptime)"
df -h /opt/relay 2>/dev/null | tail -1 || df -h / | tail -1
free -m 2>/dev/null | awk '/Mem:/{printf "mem_used_mb=%s mem_total_mb=%s\n",$3,$2}'
echo "-- peer env --"
grep -E '^RELAY_MASTER_PEER_LIST=' /opt/relay/env /opt/relay/state/*.env /etc/relay/*.env 2>/dev/null | head -5 || echo "(no RELAY_MASTER_PEER_LIST file hit)"
systemctl show -p Environment relay-gateway 2>/dev/null | tr ' ' '\n' | grep RELAY_MASTER_PEER_LIST || true
echo "-- /api/status (local) --"
if curl -fsS --max-time 8 'http://127.0.0.1:8080/api/status?peers=1' -o /tmp/relay-status.json 2>/dev/null; then
  python3 - <<'PY'
import json
st=json.load(open("/tmp/relay-status.json"))
print("schema", st.get("schema_version"), "healthy", st.get("healthy"), "status", st.get("status"))
print("master_peers_configured", st.get("master_peers_configured"))
peers=st.get("peers") or []
for p in peers:
    print(f"  peer {p.get('host') or p.get('fqdn')}: reachable={p.get('reachable')} err={p.get('error')}")
for r in st.get("repos") or []:
    if not r.get("on_disk"):
        continue
    print(
        f"  repo {r.get('name')}: tip={ (r.get('publish_sha') or r.get('head_sha') or '')[:12] }"
        f" indexed={(r.get('indexed_head_sha') or '')[:12]}"
        f" fresh={r.get('index_fresh')} pending={r.get('index_pending')} err={r.get('index_last_error')}"
    )
PY
else
  echo "WARN: local /api/status failed"
fi
echo "-- recent gateway / hook logs (mesh|index|peer) --"
journalctl -u relay-gateway -u relay-hook-handler --no-pager -n 80 2>/dev/null \
  | grep -Ei 'mesh|peer notify|ensure_indexed|index_pending|SearchStore|spawn_ensure' \
  | tail -30 || echo "(no matching journal lines)"
echo "-- post_push_sync --"
ls -la /opt/relay/state/post_push_sync 2>/dev/null | tail -10 || echo "(none)"
for f in /opt/relay/state/post_push_sync/*.json; do
  [[ -f "$f" ]] || continue
  echo "file=$f"
  python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(json.dumps(d, indent=2)[:1200])" "$f" 2>/dev/null || head -c 400 "$f"
  echo
done
REMOTE
}

rc=0
remote_probe pub1 "$PUB1_IP" 1 || rc=1
if [[ "$POST" != "1" ]]; then
  remote_probe mesh1 "$MESH1_IP" 0 || true
  remote_probe mesh2 "$MESH2_IP" 0 || true
else
  echo "--- post mode: skipping mesh SSH (expect destroyed) ---" | tee -a "$OUT"
fi

{
  echo
  echo "=== checklist ==="
  echo "1. On pub1, RELAY_MASTER_PEER_LIST must NOT list destroyed mesh FQDNs."
  echo "2. git push to pub1 must succeed (no mesh sync failed)."
  echo "3. Fleet DNS A = pub1 only; ops/fleet.json meshes removed or marked gone."
  echo "4. Artifact: $OUT"
} | tee -a "$OUT"

exit "$rc"

#!/usr/bin/env bash
# Re-apply install-caddy-tls.sh on fleet nodes so /voices and /tts proxy to Piper (5590).
# Usage (from relay-gateway/): ./scripts/relay-fleet-refresh-caddy-piper.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_FILE="${RELAY_FLEET_ENV_FILE:-$ROOT/relay.env}"
[[ -f "$ENV_FILE" ]] && set -a && # shellcheck disable=SC1090
  source "$ENV_FILE" && set +a

FLEET_RAW="${RELAY_OPERATOR_FLEET:-${RELAY_MASTER_PEER_LIST:-}}"
[[ -n "$FLEET_RAW" ]] || { echo "Set RELAY_OPERATOR_FLEET in relay.env" >&2; exit 1; }

SSH_USER="${RELAY_SSH_USER:-ubuntu}"
KEY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
EMAIL="${CADDY_EMAIL:-hostmaster@relaygateway.net}"
SCRIPT="$ROOT/scripts/install-caddy-tls.sh"

HOST_KEY="${RELAY_FLEET_STRICT_HOST_KEY:-accept-new}"
SSH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=25 -o BatchMode=yes -o "StrictHostKeyChecking=${HOST_KEY}")
SCP=(scp -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=25 -o BatchMode=yes -o "StrictHostKeyChecking=${HOST_KEY}")

IFS=';' read -ra HOSTS <<< "$FLEET_RAW"
for h in "${HOSTS[@]}"; do
  h="${h//[[:space:]]/}"
  [[ -n "$h" ]] || continue
  echo "=== Caddy+Piper routes: $h (user=$SSH_USER) ==="
  "${SCP[@]}" "$SCRIPT" "$ROOT/scripts/caddy-ask.py" "${SSH_USER}@${h}:/tmp/"
  "${SSH[@]}" "${SSH_USER}@${h}" sudo env \
    RELAY_PUBLIC_HOSTNAME="$h" \
    CADDY_EMAIL="$EMAIL" \
    bash /tmp/install-caddy-tls.sh
  if curl -fsS --connect-timeout 15 "https://${h}/voices" | head -c 80 | grep -q voices; then
    echo "OK  https://${h}/voices"
  else
    echo "WARN https://${h}/voices (check relay-tts-piper.service)" >&2
  fi
done

echo "Done."

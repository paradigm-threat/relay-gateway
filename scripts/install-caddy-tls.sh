#!/usr/bin/env bash
# Install Caddy on :80 and :443 → relay-gateway on 127.0.0.1:8080 with Let's Encrypt (on-demand TLS).
# Removes the legacy UFW PREROUTING :80→:8080 redirect block (relay NAT hack) if present.
#
# Required env:
#   RELAY_PUBLIC_HOSTNAME — node FQDN, e.g. atlanta3.relaygateway.net
#   CADDY_EMAIL          — email for Let's Encrypt (e.g. ops@example.com)
#
# Optional:
#   RELAY_HTTP_PORT      — upstream port (default 8080)
#   RELAY_PIPER_HTTP_PORT — Piper TTS HTTP (default 5590); /voices and /tts proxy here when enabled
#   RELAY_INSTALL_ROOT   — copy caddy-ask.py from here if set (default: directory of this script)
#
# Run: sudo -E bash scripts/install-caddy-tls.sh
set -euo pipefail

need_root() {
  [[ "$(id -u)" == "0" ]] || { echo "run as root" >&2; exit 1; }
}

NODE_FQDN="${RELAY_PUBLIC_HOSTNAME:?set RELAY_PUBLIC_HOSTNAME}"
EMAIL="${CADDY_EMAIL:?set CADDY_EMAIL (ACME registration email)}"
UPSTREAM="${RELAY_HTTP_PORT:-8080}"
PIPER_PORT="${RELAY_PIPER_HTTP_PORT:-5590}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

remove_ufw_nat_redirect_80() {
  command -v python3 >/dev/null || return 0
  for f in /etc/ufw/before.rules /etc/ufw/before6.rules; do
    [[ -f "$f" ]] || continue
    python3 - "$f" <<'PY' || true
import sys
from pathlib import Path
p = Path(sys.argv[1])
t = p.read_text()
for label, old in (
    ("relay-server", """# Redirect HTTP :80 -> relay-server :8080 (URLs without :8080)
*nat
:PREROUTING ACCEPT [0:0]
-A PREROUTING -p tcp --dport 80 -j REDIRECT --to-ports 8080
COMMIT

"""),
    ("relay-gateway", """# Redirect HTTP :80 -> relay-gateway :8080 (URLs without :8080)
*nat
:PREROUTING ACCEPT [0:0]
-A PREROUTING -p tcp --dport 80 -j REDIRECT --to-ports 8080
COMMIT

"""),
):
    if old in t:
        t = t.replace(old, "", 1)
        p.write_text(t)
        print("removed NAT redirect (%s) from" % label, p)
        break
PY
  done
}

install_caddy_repo() {
  apt-get update -qq
  apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl
  if [[ ! -f /usr/share/keyrings/caddy-stable-archive-keyring.gpg ]]; then
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  fi
  if [[ ! -f /etc/apt/sources.list.d/caddy-stable.list ]]; then
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
  fi
  apt-get update -qq
  apt-get install -y caddy
}

install_ask_service() {
  install -d -m 755 /usr/local/lib/relay
  install -m 755 "$SCRIPT_DIR/caddy-ask.py" /usr/local/lib/relay/caddy-ask.py

  cat >/etc/systemd/system/relay-caddy-ask.service <<EOF
[Unit]
Description=Allowlist for Caddy on-demand TLS (relay)
After=network.target

[Service]
Type=simple
Environment=RELAY_NODE_FQDN=${NODE_FQDN}
Environment=RELAY_PARKED_DOMAINS_PATH=${RELAY_PARKED_DOMAINS_PATH:-/opt/relay/state/parked-domains.txt}
ExecStart=/usr/bin/python3 /usr/local/lib/relay/caddy-ask.py
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
}

write_caddyfile() {
  # Piper TTS (relay-tts-piper.service) listens on PIPER_PORT; expose /voices and /tts on the same TLS front door.
  # Parked domains with server.tls.plugin=park get file-based TLS via import of
  # /opt/relay/state/park-tls/Caddyfile.parked (written by relay-gateway).
  local park_caddy="${RELAY_INSTALL_ROOT:-/opt/relay}/state/park-tls/Caddyfile.parked"
  mkdir -p "$(dirname "$park_caddy")"
  if [[ ! -f "$park_caddy" ]]; then
    printf '# park_tls: awaiting certificates\n' >"$park_caddy"
  fi
  cat >/etc/caddy/Caddyfile <<EOF
{
	email ${EMAIL}
	on_demand_tls {
		ask http://127.0.0.1:3999/allow
	}
}

:80 {
	# Park plugin HTTP-01 (and any other gateway-owned challenges) must reach relay-gateway.
	# Node/repo on_demand issuance uses TLS-ALPN on :443 instead of HTTP-01.
	handle /.well-known/acme-challenge/* {
		reverse_proxy 127.0.0.1:${UPSTREAM} {
			transport http {
				dial_timeout 2s
				response_header_timeout 8s
			}
		}
	}
	handle /voices* {
		reverse_proxy 127.0.0.1:${PIPER_PORT}
	}
	handle /tts* {
		reverse_proxy 127.0.0.1:${PIPER_PORT}
	}
	handle {
		redir https://{host}{uri} permanent
	}
}

# Opt-in parked domains (file TLS from park plugin) — must precede catch-all on_demand.
import ${park_caddy}

https:// {
	tls {
		on_demand
		issuer acme {
			disable_http_challenge
		}
	}
	# Compress markdown / QUERY JSON for browsers (no extra origin requests).
	encode zstd gzip
	handle /voices* {
		reverse_proxy 127.0.0.1:${PIPER_PORT}
	}
	handle /tts* {
		reverse_proxy 127.0.0.1:${PIPER_PORT}
	}
	handle {
		reverse_proxy 127.0.0.1:${UPSTREAM} {
			transport http {
				dial_timeout 2s
				response_header_timeout 8s
			}
		}
	}
}
EOF
}

ufw_allow_web() {
  command -v ufw >/dev/null || return 0
  ufw allow 80/tcp comment 'Caddy HTTP' 2>/dev/null || true
  ufw allow 443/tcp comment 'Caddy HTTPS' 2>/dev/null || true
  ufw reload
}

need_root
remove_ufw_nat_redirect_80
install_caddy_repo
install_ask_service
write_caddyfile

# Open 80/443 before starting Caddy so a failed first start still leaves firewall ready.
ufw_allow_web

install -d -m 755 /etc/systemd/system/caddy.service.d
cat >/etc/systemd/system/caddy.service.d/relay-ask.conf <<'EOF'
[Unit]
After=relay-caddy-ask.service
Requires=relay-caddy-ask.service
EOF

systemctl daemon-reload
systemctl enable relay-caddy-ask
systemctl restart relay-caddy-ask
systemctl enable caddy
systemctl restart caddy

echo "Caddy + Lets Encrypt (on-demand TLS) installed."
echo "Test HTTP:  curl -sI http://paradigm-threat-files.${NODE_FQDN}/index.json | head -3"
echo "Test HTTPS: curl -sI https://paradigm-threat-files.${NODE_FQDN}/index.json | head -5"
echo "Test Piper: curl -fsS https://${NODE_FQDN}/voices | head -c 120"

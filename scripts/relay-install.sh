#!/usr/bin/env bash
# Relay gateway: install | update | repair | reconfigure-features
# Works on common Linux distros (apt, dnf/yum, apk, pacman).
# Feature selection (Piper TTS, npm packages) is recorded in state/features.json
# and exposed at runtime via relay-gateway /api/config.
#
# Non-interactive: RELAY_INSTALL_NONINTERACTIVE=1 RELAY_FEAT_PIPER=1 RELAY_FEAT_NPM_PKGS="songwalker-js"
# Vercel DNS (first step of install unless skipped):
#   RELAY_PUBLIC_FQDN=atlanta1.relaygateway.net VERCEL_API_TOKEN=... sudo -E ./install.sh install
#   Optional: VERCEL_TEAM_ID, RELAY_VERCEL_DOMAIN (zone), RELAY_SKIP_VERCEL_DNS=1
#
# Master mesh peers: RELAY_MASTER_PEER_LIST defaults to DEFAULT_RELAY_MASTER_PEER_LIST (override per node).
# See docs/RELAY_MESH_NETWORKING_PLAN.md — this list is the bootstrap set of mesh / “master” relay FQDNs.
set -euo pipefail

INSTALL="${RELAY_INSTALL_ROOT:-/opt/relay}"
# Installed HTTP binary and systemd unit (relay-gateway.service). Legacy relay-server is retired on install/repair/update.
RELAY_HTTP_BIN="${RELAY_HTTP_BIN:-relay-gateway}"
HTTP_PORT="${RELAY_HTTP_PORT:-8080}"
GIT_PORT="${RELAY_GIT_PORT:-9418}"
# Loopback-only stock git daemon. Public git:// is relay-git-proxy on GIT_PORT.
GIT_BACKEND_PORT="${RELAY_GIT_DAEMON_PORT:-9419}"
PIPER_HTTP_PORT="${RELAY_PIPER_HTTP_PORT:-5590}"
# LibreTranslate: segment HTTP /translate API (optional); venv under RELAY_LIBRETRANSLATE_ROOT.
LIBRETRANSLATE_PORT="${RELAY_LIBRETRANSLATE_PORT:-5588}"
LIBRETRANSLATE_ROOT="${RELAY_LIBRETRANSLATE_ROOT:-/opt/libretranslate}"
# LibreTranslate built-in translation cache TTL (seconds). Upstream default is 7d; we patch site-packages to honor this env.
LIBRETRANSLATE_CACHE_TTL="${RELAY_LIBRETRANSLATE_CACHE_TTL:-2592000}"
STATE_DIR="$INSTALL/state"
FEATURES_JSON="$STATE_DIR/features.json"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION_MARKER="2"
PIPER_DEFAULT_VOICE_ID_DEFAULT="${RELAY_FEAT_PIPER_DEFAULT_VOICE:-en_GB-cori-medium}"
PIPER_DEFAULT_VOICE_IDS="${RELAY_FEAT_PIPER_VOICES:-en_GB-cori-medium en_US-lessac-medium}"
PIPER_CACHE_TTL_SECONDS="${RELAY_FEAT_PIPER_CACHE_TTL_SECONDS:-86400}"
PIPER_CACHE_MAX_FILES="${RELAY_FEAT_PIPER_CACHE_MAX_FILES:-1000}"
KUBO_VERSION="${RELAY_KUBO_VERSION:-v0.32.1}"
# dist.ipfs.tech sha512 for default Kubo (linux); other versions verified via dist.json.
KUBO_SHA512_LINUX_AMD64="${RELAY_KUBO_SHA512_LINUX_AMD64:-36b2951de759710ad81f63f3ec3a75c72323be4c6599e737d7f27b437dce02ed0a1662d676137e98f2e88f0b96e41c4b46b2e1667d874f90e3a7ee437912fda4}"
KUBO_SHA512_LINUX_ARM64="${RELAY_KUBO_SHA512_LINUX_ARM64:-2f7b8589f5a1d715ed853766b0eed206be5bbdc9fed6df9e6b350546336cf9d8395e8f5b167ae05d073ab032ca9bfaa441d022b95b1ebb5bc52413276a22c6c2}"
IPFS_API_PORT="${RELAY_IPFS_API_PORT:-5001}"
IPFS_GATEWAY_PORT="${RELAY_IPFS_GATEWAY_PORT:-8081}"
IPFS_SWARM_PORT="${RELAY_IPFS_SWARM_PORT:-4001}"
IPFS_PATH_DIR="${RELAY_IPFS_PATH:-$INSTALL/ipfs}"

# Semicolon-separated FQDNs; used when RELAY_MASTER_PEER_LIST is unset (non-interactive) or when the operator accepts the interactive default (Enter).
DEFAULT_RELAY_MASTER_PEER_LIST="${DEFAULT_RELAY_MASTER_PEER_LIST:-iad1.relaygateway.net;ord1.relaygateway.net;hkg1.relaygateway.net}"
DEFAULT_RELAY_MASTER_CATALOG_PATH="${DEFAULT_RELAY_MASTER_CATALOG_PATH:-/opt/relay/src/relay-master}"
DEFAULT_RELAY_MASTER_CATALOG_GIT_URL="${DEFAULT_RELAY_MASTER_CATALOG_GIT_URL:-https://gitlab.com/clevertree/relay-master.git}"

log() { echo "[relay-install] $*" >&2; }
die() { echo "[relay-install] ERROR: $*" >&2; exit 1; }

need_root() { [[ "$(id -u)" -eq 0 ]] || die "run as root"; }

detect_pm() {
  if command -v apt-get >/dev/null 2>&1; then echo apt
  elif command -v dnf >/dev/null 2>&1; then echo dnf
  elif command -v yum >/dev/null 2>&1; then echo yum
  elif command -v apk >/dev/null 2>&1; then echo apk
  elif command -v pacman >/dev/null 2>&1; then echo pacman
  elif command -v zypper >/dev/null 2>&1; then echo zypper
  else die "no supported package manager (apt, dnf, yum, apk, pacman, zypper)"
  fi
}

install_packages() {
  local pm="$1"; shift
  case "$pm" in
    apt) apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$@" ;;
    dnf) dnf install -y "$@" ;;
    yum) yum install -y "$@" ;;
    apk) apk add --no-cache "$@" ;;
    pacman) pacman -Sy --noconfirm "$@" ;;
    zypper) zypper install -y "$@" ;;
  esac
}

map_pkg_node() {
  case "$(detect_pm)" in
    apt) echo "nodejs" ;;
    dnf|yum|zypper) echo "nodejs npm" ;;
    apk) echo "nodejs npm" ;;
    pacman) echo "nodejs npm" ;;
  esac
}

# Debian/Ubuntu use package xz-utils; others typically use xz.
map_pkg_xz() {
  case "$(detect_pm)" in
    apt) echo xz-utils ;;
    *) echo xz ;;
  esac
}

ensure_user_relay() {
  id relay &>/dev/null || useradd -r -m -d /var/lib/relay -s /bin/bash relay
}

ensure_base_deps() {
  local pm xz_pkg
  pm="$(detect_pm)"
  xz_pkg="$(map_pkg_xz)"
  local node_pkgs
  node_pkgs="$(map_pkg_node)"
  install_packages "$pm" git curl ca-certificates tar gzip "$xz_pkg" python3 ${node_pkgs}
  command -v jq >/dev/null || install_packages "$pm" jq 2>/dev/null || {
    log "jq missing; installing via pip fallback"
    python3 -m pip install --break-system-packages jq 2>/dev/null || true
    command -v jq >/dev/null || die "install jq manually"
  }
}

refresh_features_inventory() {
  [[ -f "$SCRIPT_DIR/relay-probe-features.py" ]] || return 0
  python3 "$SCRIPT_DIR/relay-probe-features.py" merge "$INSTALL" || log "WARN: feature inventory refresh failed"
}

piper_model_path_for_voice() {
  local voice_id="$1"
  printf '%s/lib/piper/models/%s.onnx' "$INSTALL" "$voice_id"
}

piper_normalize_voice_ids() {
  local raw="$1"
  local voice norm out=() seen=" "
  read -ra raw_arr <<<"$raw"
  for voice in "${raw_arr[@]}"; do
    norm="${voice//,/ }"
    read -ra norm_arr <<<"$norm"
    for voice in "${norm_arr[@]}"; do
      [[ -n "$voice" ]] || continue
      if [[ "$seen" != *" $voice "* ]]; then
        out+=("$voice")
        seen+="$voice "
      fi
    done
  done
  printf '%s' "${out[*]}"
}

piper_voice_url_base() {
  local voice_id="$1"
  if [[ ! "$voice_id" =~ ^([a-z]{2}_[A-Z]{2})-(.+)-(low|medium|high|x_low)$ ]]; then
    die "unsupported Piper voice id format: $voice_id"
  fi
  local lang="${BASH_REMATCH[1]}"
  local voice_name="${BASH_REMATCH[2]}"
  local quality="${BASH_REMATCH[3]}"
  local family="${lang%%_*}"
  printf 'https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/%s/%s/%s/%s' "$family" "$lang" "$voice_name" "$quality"
}

install_piper_voice_model() {
  local voice_id="$1"
  local base target_json
  base="$(piper_voice_url_base "$voice_id")"
  log "Downloading Piper voice model: $voice_id"
  curl -fsSL "$base/$voice_id.onnx" -o "$INSTALL/lib/piper/models/$voice_id.onnx"
  target_json="$INSTALL/lib/piper/models/$voice_id.onnx.json"
  curl -fsSL "$base/$voice_id.onnx.json" -o "$target_json" || rm -f "$target_json"
}

piper_install_ok() {
  [[ -x "$INSTALL/lib/piper/piper" ]] || return 1
  [[ -f "$INSTALL/lib/piper/libpiper_phonemize.so.1" ]] || return 1
  [[ -f "$INSTALL/lib/piper/libespeak-ng.so.1" ]] || return 1
  [[ -f "$INSTALL/lib/piper/libonnxruntime.so.1.14.1" ]] || return 1
  [[ -d "$INSTALL/lib/piper/espeak-ng-data" ]] || return 1
  LD_LIBRARY_PATH="$INSTALL/lib/piper${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    "$INSTALL/lib/piper/piper" --version >/dev/null 2>&1
}

write_features_json() {
  local piper_en="$1" piper_voices_json="$2" piper_default_voice="$3" npm_en="$4" npm_pkgs_json="$5" trans_en="$6" trans_pkgs_json="$7" lt_en="$8"
  mkdir -p "$STATE_DIR"
  local model
  model="$(piper_model_path_for_voice "$piper_default_voice")"
  RELAY_WFJ_INSTALL="$INSTALL" \
  RELAY_WFJ_HTTP_PORT="$HTTP_PORT" \
  RELAY_WFJ_GIT_PORT="$GIT_PORT" \
  RELAY_WFJ_PIPER_PORT="$PIPER_HTTP_PORT" \
  RELAY_WFJ_LT_PORT="$LIBRETRANSLATE_PORT" \
  RELAY_WFJ_LT_ROOT="$LIBRETRANSLATE_ROOT" \
  RELAY_WFJ_VERSION="$VERSION_MARKER" \
  RELAY_WFJ_PIPER_EN="$piper_en" \
  RELAY_WFJ_PIPER_VOICES_JSON="$piper_voices_json" \
  RELAY_WFJ_PIPER_DEFAULT_VOICE="$piper_default_voice" \
  RELAY_WFJ_NPM_EN="$npm_en" \
  RELAY_WFJ_NPM_PKGS_JSON="$npm_pkgs_json" \
  RELAY_WFJ_TRANS_EN="$trans_en" \
  RELAY_WFJ_TRANS_PKGS_JSON="$trans_pkgs_json" \
  RELAY_WFJ_LT_EN="$lt_en" \
  RELAY_WFJ_LT_LOAD="${RELAY_LIBRETRANSLATE_LOAD_ONLY:-en,ru}" \
  RELAY_WFJ_FEATURES_JSON="$FEATURES_JSON" \
  RELAY_WFJ_MODEL="$model" \
    python3 <<'PY'
import json
import os

install = os.environ["RELAY_WFJ_INSTALL"]
features_path = os.environ["RELAY_WFJ_FEATURES_JSON"]
model = os.environ["RELAY_WFJ_MODEL"]
http_port = int(os.environ["RELAY_WFJ_HTTP_PORT"])
git_port = int(os.environ["RELAY_WFJ_GIT_PORT"])
piper_port = int(os.environ["RELAY_WFJ_PIPER_PORT"])
lt_port = int(os.environ["RELAY_WFJ_LT_PORT"])
lt_root = os.environ["RELAY_WFJ_LT_ROOT"]
ver = int(os.environ["RELAY_WFJ_VERSION"])

piper = os.environ["RELAY_WFJ_PIPER_EN"] == "1"
piper_voices = json.loads(os.environ["RELAY_WFJ_PIPER_VOICES_JSON"])
piper_default_voice = os.environ["RELAY_WFJ_PIPER_DEFAULT_VOICE"]
npm_on = os.environ["RELAY_WFJ_NPM_EN"] == "1"
trans = os.environ["RELAY_WFJ_TRANS_EN"] == "1"
lt = os.environ["RELAY_WFJ_LT_EN"] == "1"
lt_load = os.environ.get("RELAY_WFJ_LT_LOAD", "en,ru").strip() or "en,ru"
pkgs = json.loads(os.environ["RELAY_WFJ_NPM_PKGS_JSON"])
trans_pkgs = json.loads(os.environ["RELAY_WFJ_TRANS_PKGS_JSON"])

data = {
    "schema_version": ver,
    "install_root": install,
    "http_port": http_port,
    "git_port": git_port,
    "features": {
        "piper_tts": {
            "enabled": piper,
            "expected": piper,
            "binary": f"{install}/lib/piper/piper" if piper else None,
            "models_dir": f"{install}/lib/piper/models" if piper else None,
            "default_model": model if piper else None,
            "default_voice_id": piper_default_voice if piper else None,
            "http_port": piper_port if piper else None,
            "health_path": "/health" if piper else None,
            "tts_path": "/tts" if piper else None,
            "service": "relay-tts-piper.service" if piper else None,
            "voices": piper_voices if piper else [],
            "languages": [],
        },
        "npm_extensions": {
            "enabled": npm_on,
            "expected": npm_on,
            "directory": f"{install}/node_extensions" if npm_on else None,
            "packages": pkgs if npm_on else [],
        },
        "text_translation": {
            "enabled": trans,
            "expected": trans,
            "backend": "argos_translate_local" if trans else None,
            "description": "Offline neural translation (no cloud); install language packs via argospm.",
            "venv_dir": f"{install}/lib/argos-venv" if trans else None,
            "venv_python": f"{install}/lib/argos-venv/bin/python3" if trans else None,
            "cli": "argospm" if trans else None,
            "install_argos_packages": trans_pkgs if trans else [],
            "language_pairs": [],
            "from_languages": [],
            "to_languages": [],
        },
        "libretranslate_api": {
            "enabled": lt,
            "expected": lt,
            "description": "LibreTranslate HTTP API for chunked text/html translation (self-hosted).",
            "http_port": lt_port if lt else None,
            "load_only": lt_load if lt else None,
            "venv_dir": f"{lt_root}/venv" if lt else None,
            "service": "libretranslate.service" if lt else None,
            "translate_path": "/translate" if lt else None,
            "languages_path": "/languages" if lt else None,
        },
        "core": {
            "relay_server": True,
            "relay_git_daemon": True,
            "ports": {"http": http_port, "git_daemon": git_port},
        },
    },
}


def strip_none(d):
    if isinstance(d, dict):
        return {k: strip_none(v) for k, v in d.items() if v is not None}
    return d


with open(features_path, "w", encoding="utf-8") as f:
    json.dump(strip_none(data), f, indent=2)
PY
  chown relay:relay "$FEATURES_JSON" 2>/dev/null || true
}

install_piper_artifacts() {
  local requested_voices_raw="${1:-$PIPER_DEFAULT_VOICE_IDS}"
  local default_voice_id="${2:-$PIPER_DEFAULT_VOICE_ID_DEFAULT}"
  local requested_voices
  requested_voices="$(piper_normalize_voice_ids "$requested_voices_raw")"
  [[ -n "$requested_voices" ]] || requested_voices="$PIPER_DEFAULT_VOICE_IDS"
  if [[ " $requested_voices " != *" $default_voice_id "* ]]; then
    requested_voices="$default_voice_id $requested_voices"
    requested_voices="$(piper_normalize_voice_ids "$requested_voices")"
  fi

  mkdir -p "$INSTALL/lib/piper" "$INSTALL/lib/piper/models" "$INSTALL/cache/piper-tts"
  local arch url
  arch="$(uname -m)"
  case "$arch" in
    x86_64) url="https://github.com/rhasspy/piper/releases/download/v1.2.0/piper_amd64.tar.gz" ;;
    aarch64|arm64) url="https://github.com/rhasspy/piper/releases/download/v1.2.0/piper_arm64.tar.gz" ;;
    *) die "unsupported arch for Piper: $arch" ;;
  esac
  log "Downloading Piper..."
  curl -fsSL "$url" -o /tmp/piper.tgz
  tar -xzf /tmp/piper.tgz -C /tmp
  # release layout: piper/ directory with binary, libraries, and espeak-ng-data
  # Copy entire piper directory (binary + all .so libraries + espeak-ng-data)
  if [[ -d /tmp/piper ]]; then
    # Copy all files from piper directory (binary, libs, data)
    cp -r /tmp/piper/* "$INSTALL/lib/piper/"
  elif [[ -d /tmp/piper_amd64 ]]; then
    cp -r /tmp/piper_amd64/* "$INSTALL/lib/piper/"
  elif [[ -d /tmp/piper_arm64 ]]; then
    cp -r /tmp/piper_arm64/* "$INSTALL/lib/piper/"
  else
    die "could not find piper directory in archive"
  fi
  chmod +x "$INSTALL/lib/piper/piper"
  chown -R relay:relay "$INSTALL/lib/piper"
  rm -rf /tmp/piper.tgz /tmp/piper /tmp/piper_amd64 /tmp/piper_arm64 /tmp/piper_linux_* 2>/dev/null || true

  local voice_id
  for voice_id in $requested_voices; do
    install_piper_voice_model "$voice_id"
  done
  chown -R relay:relay "$INSTALL/lib/piper/models" "$INSTALL/cache/piper-tts"

  if ! piper_install_ok; then
    die "Piper install is incomplete after extraction (binary or shared libraries missing)"
  fi

  if [[ -f "$SCRIPT_DIR/piper-tts-http.py" ]]; then
    cp "$SCRIPT_DIR/piper-tts-http.py" "$INSTALL/bin/piper-tts-http.py"
  elif [[ -f ./piper-tts-http.py ]]; then
    cp ./piper-tts-http.py "$INSTALL/bin/piper-tts-http.py"
  else
    log "WARN: piper-tts-http.py not beside script; TTS HTTP disabled until copied"
    return 0
  fi
  chmod +x "$INSTALL/bin/piper-tts-http.py"

  cat >/etc/systemd/system/relay-tts-piper.service <<EOF
[Unit]
Description=Relay Piper TTS HTTP ($PIPER_HTTP_PORT)
After=network.target

[Service]
Type=simple
User=relay
Group=relay
Environment=PIPER_BIN=$INSTALL/lib/piper/piper
Environment=PIPER_MODEL=$(piper_model_path_for_voice "$default_voice_id")
Environment=PIPER_MODELS_DIR=$INSTALL/lib/piper/models
Environment=LD_LIBRARY_PATH=$INSTALL/lib/piper
Environment=PIPER_HTTP_PORT=$PIPER_HTTP_PORT
Environment=PIPER_CACHE_DIR=$INSTALL/cache/piper-tts
Environment=PIPER_CACHE_TTL_SECONDS=$PIPER_CACHE_TTL_SECONDS
Environment=PIPER_CACHE_MAX_FILES=$PIPER_CACHE_MAX_FILES
ExecStart=/usr/bin/python3 $INSTALL/bin/piper-tts-http.py
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable relay-tts-piper
  systemctl restart relay-tts-piper || log "relay-tts-piper start failed (check python3 + model)"
}

install_npm_extensions() {
  local pkgs="$1"
  [[ -z "$pkgs" ]] && return 0
  mkdir -p "$INSTALL/node_extensions"
  chown relay:relay "$INSTALL/node_extensions"
  sudo -u relay bash -c "cd '$INSTALL/node_extensions' && (test -f package.json || npm init -y >/dev/null) && npm install --no-save $pkgs"
}

install_translation_artifacts() {
  local pm venv_dir pkg
  pm="$(detect_pm)"
  case "$pm" in
    apt) install_packages apt python3-venv python3-pip ;;
    dnf|yum) install_packages "$pm" python3 python3-pip || true ;;
    apk) install_packages apk python3 py3-pip python3-dev ;;
    pacman) install_packages pacman python python-pip ;;
    zypper) install_packages zypper python311 python311-pip || install_packages zypper python3 python3-pip ;;
  esac
  venv_dir="$INSTALL/lib/argos-venv"
  mkdir -p "$INSTALL/lib"
  if [[ ! -x "$venv_dir/bin/python3" ]]; then
    log "Creating Argos Translate venv at $venv_dir"
    sudo -u relay python3 -m venv "$venv_dir"
  fi
  sudo -u relay "$venv_dir/bin/pip" install --upgrade pip setuptools wheel
  sudo -u relay "$venv_dir/bin/pip" install argostranslate || die "pip install argostranslate failed"
  chown -R relay:relay "$venv_dir"
  for pkg in "$@"; do
    [[ -z "$pkg" ]] && continue
    log "argospm install $pkg"
    sudo -u relay "$venv_dir/bin/argospm" install "$pkg" || log "WARN: argospm install $pkg failed (check package name / network)"
  done
}

write_libretranslate_systemd() {
  local load="${1:-en,ru}"
  local lt_shared_env=""
  [[ -n "${RELAY_LIBRETRANSLATE_SHARED_STORAGE:-}" ]] && lt_shared_env="Environment=LT_SHARED_STORAGE=${RELAY_LIBRETRANSLATE_SHARED_STORAGE}
"
  mkdir -p "$LIBRETRANSLATE_ROOT"
  chown relay:relay "$LIBRETRANSLATE_ROOT" 2>/dev/null || true
  cat >/etc/systemd/system/libretranslate.service <<EOF
[Unit]
Description=LibreTranslate HTTP API ($load)
After=network.target

[Service]
Type=simple
User=relay
Group=relay
WorkingDirectory=$LIBRETRANSLATE_ROOT
ExecStart=$LIBRETRANSLATE_ROOT/venv/bin/libretranslate --host 0.0.0.0 --port $LIBRETRANSLATE_PORT --load-only $load --char-limit -1 --req-limit -1 --hourly-req-limit -1 --daily-req-limit -1 --disable-web-ui --threads 2 --translation-cache all
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1
Environment=LT_TRANSLATION_CACHE_TTL=$LIBRETRANSLATE_CACHE_TTL
${lt_shared_env}
[Install]
WantedBy=multi-user.target
EOF
  if [[ -x "$LIBRETRANSLATE_ROOT/venv/bin/python3" ]] || [[ -x "$LIBRETRANSLATE_ROOT/venv/bin/python" ]]; then
    patch_libretranslate_cache_ttl
  fi
  systemctl daemon-reload
}

# Upstream LibreTranslate hardcodes translation cache expiry to 7 days; honor LT_TRANSLATION_CACHE_TTL (systemd sets it).
patch_libretranslate_cache_ttl() {
  local py="$LIBRETRANSLATE_ROOT/venv/bin/python3"
  [[ -x "$py" ]] || py="$LIBRETRANSLATE_ROOT/venv/bin/python"
  [[ -x "$py" ]] || return 0
  sudo -u relay env LIBRETRANSLATE_ROOT="$LIBRETRANSLATE_ROOT" "$py" <<'PY'
import os
import pathlib
import re

root = pathlib.Path(os.environ["LIBRETRANSLATE_ROOT"])
paths = list(root.glob("venv/lib/python*/site-packages/libretranslate/cache.py"))
if not paths:
    raise SystemExit(0)
p = paths[0]
text = p.read_text(encoding="utf-8")
if "LT_TRANSLATION_CACHE_TTL" in text:
    raise SystemExit(0)
if not re.search(r"^\s*self\.expire\s*=\s*\d+", text, re.M):
    raise SystemExit(0)
if "\nimport os\n" not in text:
    text = text.replace("import gzip\n", "import gzip\nimport os\n", 1)
text_new, n = re.subn(
    r"(?m)^(\s*)self\.expire\s*=\s*\d+(\s*#.*)?$",
    r'\1self.expire = int(os.environ.get("LT_TRANSLATION_CACHE_TTL", "2592000"))',
    text,
    count=1,
)
if n != 1:
    raise SystemExit(0)
p.write_text(text_new, encoding="utf-8")
PY
}

install_libretranslate_artifacts() {
  local pm load
  pm="$(detect_pm)"
  case "$pm" in
    apt) install_packages apt python3-venv python3-pip ;;
    dnf|yum) install_packages "$pm" python3 python3-pip || true ;;
    apk) install_packages apk python3 py3-pip python3-dev ;;
    pacman) install_packages pacman python python-pip ;;
    zypper) install_packages zypper python3 python3-pip || true ;;
    *) install_packages "$pm" python3 python3-pip 2>/dev/null || true ;;
  esac
  load="${RELAY_LIBRETRANSLATE_LOAD_ONLY:-}"
  if [[ -z "$load" && -f "$FEATURES_JSON" ]]; then
    load="$(jq -r '.features.libretranslate_api.load_only // empty' "$FEATURES_JSON" 2>/dev/null || true)"
  fi
  [[ -z "$load" ]] && load="en,ru"
  mkdir -p "$LIBRETRANSLATE_ROOT"
  chown relay:relay "$LIBRETRANSLATE_ROOT"
  if [[ ! -x "$LIBRETRANSLATE_ROOT/venv/bin/python3" ]]; then
    log "Creating LibreTranslate venv at $LIBRETRANSLATE_ROOT/venv"
    sudo -u relay python3 -m venv "$LIBRETRANSLATE_ROOT/venv"
  fi
  sudo -u relay "$LIBRETRANSLATE_ROOT/venv/bin/pip" install --upgrade pip setuptools wheel
  sudo -u relay "$LIBRETRANSLATE_ROOT/venv/bin/pip" install "libretranslate>=1.6,<2" || die "pip install libretranslate failed"
  chown -R relay:relay "$LIBRETRANSLATE_ROOT"
  write_libretranslate_systemd "$load"
  systemctl enable libretranslate 2>/dev/null || true
  systemctl restart libretranslate || log "WARN: libretranslate start failed (first run downloads models; check journalctl -u libretranslate)"
}

# Copy binary into INSTALL/bin unless it is already that file (repair often uses RELAY_BIN_SOURCE=$INSTALL/bin).
# Accepts relay-gateway or legacy relay-server in the source dir (tarballs / old trees).
install_binaries() {
  local src="${RELAY_BIN_SOURCE:-$SCRIPT_DIR}" gw_src=""
  if [[ -f "$src/$RELAY_HTTP_BIN" ]]; then
    gw_src="$src/$RELAY_HTTP_BIN"
  elif [[ -f "$src/relay-server" ]]; then
    gw_src="$src/relay-server"
  else
    src="."
    if [[ -f "$src/$RELAY_HTTP_BIN" ]]; then
      gw_src="$src/$RELAY_HTTP_BIN"
    elif [[ -f "$src/relay-server" ]]; then
      gw_src="$src/relay-server"
    fi
  fi
  [[ -n "$gw_src" ]] || die "$RELAY_HTTP_BIN binary not found (place $RELAY_HTTP_BIN or legacy relay-server in $RELAY_BIN_SOURCE, $SCRIPT_DIR, or cwd)"
  [[ -f "$src/relay-hook-handler" ]] || die "relay-hook-handler binary not found beside HTTP binary in $src"
  [[ -f "$src/relay-git-proxy" ]] || die "relay-git-proxy binary not found beside HTTP binary in $src"
  mkdir -p "$INSTALL/bin" "$INSTALL/hooks" "$INSTALL/data" "$INSTALL/logs" "$INSTALL/www" "$INSTALL/src"
  _copy_bin_if_needed() {
    local s="$1" d="$2"
    if [[ -f "$d" ]] && [[ "$(stat -c '%d:%i' "$s")" == "$(stat -c '%d:%i' "$d")" ]]; then
      return 0
    fi
    cp -f "$s" "$d"
  }
  _copy_bin_if_needed "$gw_src" "$INSTALL/bin/$RELAY_HTTP_BIN"
  _copy_bin_if_needed "$src/relay-hook-handler" "$INSTALL/bin/relay-hook-handler"
  _copy_bin_if_needed "$src/relay-git-proxy" "$INSTALL/bin/relay-git-proxy"
  if [[ -d "$src/www" ]]; then
    rm -rf "$INSTALL/www"
    mkdir -p "$INSTALL/www"
    cp -a "$src/www/." "$INSTALL/www/"
  fi
  mkdir -p "$INSTALL/state"
  if [[ -f "$src/deploy-manifest.json" ]]; then
    cp -f "$src/deploy-manifest.json" "$INSTALL/state/deploy-manifest.json"
    chown root:root "$INSTALL/state/deploy-manifest.json" 2>/dev/null || true
    chmod 644 "$INSTALL/state/deploy-manifest.json" 2>/dev/null || true
  fi
  mkdir -p "$INSTALL/state/park-tls/challenges" "$INSTALL/state/park-tls/certs"
  chown -R relay:relay "$INSTALL/state/park-tls" 2>/dev/null || true

  if [[ -f "$src/relay-startup-catalog-sync.sh" ]]; then
    _copy_bin_if_needed "$src/relay-startup-catalog-sync.sh" "$INSTALL/bin/relay-startup-catalog-sync.sh"
  elif [[ -f "$SCRIPT_DIR/relay-startup-catalog-sync.sh" ]]; then
    cp -f "$SCRIPT_DIR/relay-startup-catalog-sync.sh" "$INSTALL/bin/relay-startup-catalog-sync.sh"
  fi
  chmod +x "$INSTALL/bin/$RELAY_HTTP_BIN" "$INSTALL/bin/relay-hook-handler" "$INSTALL/bin/relay-git-proxy"
  [[ -f "$INSTALL/bin/relay-startup-catalog-sync.sh" ]] && chmod +x "$INSTALL/bin/relay-startup-catalog-sync.sh"
  if [[ -f "$src/relay-gateway-liveness.sh" ]]; then
    _copy_bin_if_needed "$src/relay-gateway-liveness.sh" "$INSTALL/bin/relay-gateway-liveness.sh"
  elif [[ -f "$SCRIPT_DIR/relay-gateway-liveness.sh" ]]; then
    cp -f "$SCRIPT_DIR/relay-gateway-liveness.sh" "$INSTALL/bin/relay-gateway-liveness.sh"
  fi
  [[ -f "$INSTALL/bin/relay-gateway-liveness.sh" ]] && chmod +x "$INSTALL/bin/relay-gateway-liveness.sh"
  if [[ -f "$src/relay-reconcile.sh" ]]; then
    _copy_bin_if_needed "$src/relay-reconcile.sh" "$INSTALL/bin/relay-reconcile.sh"
  elif [[ -f "$SCRIPT_DIR/relay-reconcile.sh" ]]; then
    cp -f "$SCRIPT_DIR/relay-reconcile.sh" "$INSTALL/bin/relay-reconcile.sh"
  fi
  [[ -f "$INSTALL/bin/relay-reconcile.sh" ]] && chmod +x "$INSTALL/bin/relay-reconcile.sh"
  if [[ -f "$src/relay-ipfs-pin-catalog" ]]; then
    _copy_bin_if_needed "$src/relay-ipfs-pin-catalog" "$INSTALL/bin/relay-ipfs-pin-catalog"
  elif [[ -f "$SCRIPT_DIR/relay-ipfs-pin-catalog" ]]; then
    cp -f "$SCRIPT_DIR/relay-ipfs-pin-catalog" "$INSTALL/bin/relay-ipfs-pin-catalog"
  fi
  [[ -f "$INSTALL/bin/relay-ipfs-pin-catalog" ]] && chmod +x "$INSTALL/bin/relay-ipfs-pin-catalog"
  rm -f "$INSTALL/bin/relay-server"
  chown -R relay:relay "$INSTALL"
  lock_relay_env_permissions
  for h in pre-receive post-receive post-update update; do
    ln -sf "$INSTALL/bin/relay-hook-handler" "$INSTALL/hooks/$h"
  done
}

write_gitconfig_hooks() {
  git config --file /etc/gitconfig core.hooksPath "$INSTALL/hooks" 2>/dev/null || \
    printf '[core]\n\thooksPath = %s\n' "$INSTALL/hooks" >>/etc/gitconfig
  # Belt-and-suspenders: anonymous receive-pack matches --enable=receive-pack on git daemon.
  git config --file /etc/gitconfig daemon.receivepack true 2>/dev/null || \
    printf '\n[daemon]\n\treceivepack = true\n' >>/etc/gitconfig
  # Partial clone for observer browse / thin clients (trees without  multi-GB blobs).
  git config --file /etc/gitconfig uploadpack.allowFilter true
  git config --file /etc/gitconfig uploadpack.allowReachableSHA1InWant true
}

kubo_linux_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo amd64 ;;
    aarch64|arm64) echo arm64 ;;
    *) die "unsupported arch for Kubo: $(uname -m)" ;;
  esac
}

kubo_expected_sha512() {
  local arch="$1"
  if [[ "$KUBO_VERSION" == "v0.32.1" ]]; then
    case "$arch" in
      amd64) echo "$KUBO_SHA512_LINUX_AMD64"; return 0 ;;
      arm64) echo "$KUBO_SHA512_LINUX_ARM64"; return 0 ;;
    esac
  fi
  local dj expect
  dj="$(mktemp)"
  curl -fsSL "https://dist.ipfs.tech/kubo/${KUBO_VERSION}/dist.json" -o "$dj"
  expect="$(jq -r --arg a "$arch" '.platforms.linux.archs[$a].sha512 // empty' "$dj")"
  rm -f "$dj"
  [[ "$expect" =~ ^[0-9a-f]{128}$ ]] || die "no linux/$arch sha512 in Kubo ${KUBO_VERSION} dist.json"
  echo "$expect"
}

install_kubo_binary() {
  local arch tgz url dest expect got
  arch="$(kubo_linux_arch)"
  dest="$INSTALL/lib/kubo"
  mkdir -p "$dest"
  if [[ -x "$dest/ipfs" ]]; then
    if "$dest/ipfs" version 2>/dev/null | grep -q "${KUBO_VERSION#v}"; then
      return 0
    fi
  fi
  tgz="$(mktemp)"
  url="https://dist.ipfs.tech/kubo/${KUBO_VERSION}/kubo_${KUBO_VERSION}_linux-${arch}.tar.gz"
  log "Downloading Kubo ${KUBO_VERSION} ($arch)"
  curl -fsSL "$url" -o "$tgz"
  expect="$(kubo_expected_sha512 "$arch")"
  got="$(sha512sum "$tgz" | awk '{print $1}')"
  if [[ "$got" != "$expect" ]]; then
    rm -f "$tgz"
    die "Kubo tarball sha512 mismatch for ${KUBO_VERSION} linux-${arch}"
  fi
  tar -xzf "$tgz" -C "$dest" --strip-components=1 kubo/ipfs
  rm -f "$tgz"
  chmod +x "$dest/ipfs"
  chown -R relay:relay "$dest"
}

configure_kubo_repo() {
  local ipfs="$INSTALL/lib/kubo/ipfs"
  mkdir -p "$IPFS_PATH_DIR" "$INSTALL/ipfs-cache"
  chown -R relay:relay "$IPFS_PATH_DIR" "$INSTALL/ipfs-cache"
  if [[ ! -f "$IPFS_PATH_DIR/config" ]]; then
    log "ipfs init $IPFS_PATH_DIR"
    sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" init --profile server >/dev/null
  fi
  sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config Addresses.API "/ip4/127.0.0.1/tcp/${IPFS_API_PORT}"
  sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config Addresses.Gateway "/ip4/127.0.0.1/tcp/${IPFS_GATEWAY_PORT}"
  sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config --json Addresses.Swarm \
    "[\"/ip4/0.0.0.0/tcp/${IPFS_SWARM_PORT}\",\"/ip4/0.0.0.0/udp/${IPFS_SWARM_PORT}/quic-v1\"]"
  sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config --json Gateway.Writable false
  sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config --json Swarm.RelayService.Enabled false
  local gw_addr api_addr
  gw_addr="$(sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config Addresses.Gateway)"
  api_addr="$(sudo -u relay env IPFS_PATH="$IPFS_PATH_DIR" "$ipfs" config Addresses.API)"
  if grep -Eq '/tcp/8080([^0-9]|$)' <<<"$gw_addr$api_addr"; then
    die "Kubo API/gateway still references 8080 (relay-gateway): api=$api_addr gw=$gw_addr"
  fi
}

write_ipfs_systemd() {
  cat >/etc/systemd/system/relay-ipfs.service <<EOF
[Unit]
Description=Relay Kubo IPFS daemon (API :${IPFS_API_PORT} loopback, gateway :${IPFS_GATEWAY_PORT} loopback, swarm :${IPFS_SWARM_PORT})
After=network.target

[Service]
Type=simple
User=relay
Group=relay
Environment=IPFS_PATH=$IPFS_PATH_DIR
ExecStart=$INSTALL/lib/kubo/ipfs daemon --migrate=true --routing=dht
Restart=always
RestartSec=5
LimitNOFILE=65536
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=$IPFS_PATH_DIR
RestrictSUIDSGID=yes

[Install]
WantedBy=multi-user.target
EOF
  cat >/etc/systemd/system/relay-ipfs-pin-catalog.service <<EOF
[Unit]
Description=Pin catalog thin-repo IPFS CIDs on this node
After=relay-ipfs.service
Requires=relay-ipfs.service

[Service]
Type=oneshot
User=relay
Group=relay
Environment=IPFS_PATH=$IPFS_PATH_DIR
Environment=RELAY_REPO_PATH=$INSTALL/data
Environment=RELAY_IPFS_BIN=$INSTALL/lib/kubo/ipfs
Environment=PYTHONUNBUFFERED=1
EnvironmentFile=-$INSTALL/relay.env
ExecStart=$INSTALL/bin/relay-ipfs-pin-catalog
Nice=10
TimeoutStartSec=30min
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=$IPFS_PATH_DIR $INSTALL/data
EOF
  cat >/etc/systemd/system/relay-ipfs-pin-catalog.timer <<EOF
[Unit]
Description=Periodic catalog IPFS pin (seed)

[Timer]
OnBootSec=2min
OnUnitActiveSec=15min
Persistent=true

[Install]
WantedBy=timers.target
EOF
  systemctl daemon-reload
}

merge_ipfs_features_json() {
  [[ -f "$FEATURES_JSON" ]] || return 0
  local tmp
  tmp="$(mktemp)"
  jq --arg path "$IPFS_PATH_DIR" \
     --arg api "http://127.0.0.1:${IPFS_API_PORT}" \
     --arg gw "http://127.0.0.1:${IPFS_GATEWAY_PORT}" \
     --argjson swarm "$IPFS_SWARM_PORT" \
     --argjson gwp "$IPFS_GATEWAY_PORT" \
     --argjson apip "$IPFS_API_PORT" \
     '.features.ipfs_kubo = {
        enabled: true,
        expected: true,
        ipfs_path: $path,
        binary: "'"$INSTALL"'/lib/kubo/ipfs",
        service: "relay-ipfs.service",
        api: $api,
        gateway: $gw,
        api_port: $apip,
        gateway_port: $gwp,
        swarm_port: $swarm
      }' "$FEATURES_JSON" >"$tmp" && mv "$tmp" "$FEATURES_JSON"
  chown relay:relay "$FEATURES_JSON" 2>/dev/null || true
}

upsert_ipfs_relay_env() {
  local f="$INSTALL/relay.env"
  touch "$f"
  upsert_relay_env_var "$f" RELAY_IPFS_API "http://127.0.0.1:${IPFS_API_PORT}"
  upsert_relay_env_var "$f" RELAY_IPFS_GATEWAY "http://127.0.0.1:${IPFS_GATEWAY_PORT}"
  upsert_relay_env_var "$f" RELAY_IPFS_GATEWAY_ALLOW_PRIVATE "1"
  upsert_relay_env_var "$f" RELAY_IPFS_CACHE_ROOT "$INSTALL/ipfs-cache"
  lock_relay_env_permissions
}

# Default on for install/update/repair unless RELAY_INSTALL_IPFS=0.
ensure_ipfs_kubo() {
  case "${RELAY_INSTALL_IPFS:-1}" in
    0|false|FALSE|no|NO) log "Skipping Kubo (RELAY_INSTALL_IPFS=0)"; return 0 ;;
  esac
  install_kubo_binary
  configure_kubo_repo
  write_ipfs_systemd
  upsert_ipfs_relay_env
  merge_ipfs_features_json
  systemctl enable relay-ipfs.service relay-ipfs-pin-catalog.timer 2>/dev/null || true
  systemctl restart relay-ipfs.service 2>/dev/null || true
  systemctl start relay-ipfs-pin-catalog.timer 2>/dev/null || true
  systemctl start --no-block relay-ipfs-pin-catalog.service 2>/dev/null || true
}

retire_relay_server_http_unit() {
  if command -v systemctl >/dev/null 2>&1; then
    if systemctl list-unit-files relay-server.service 2>/dev/null | grep -q '^relay-server.service'; then
      systemctl disable --now relay-server.service 2>/dev/null || true
    fi
  fi
  # Orphan: relay-server.service was removed earlier but the process can still be running (not in unit-files list).
  if command -v pgrep >/dev/null 2>&1 && pgrep -f "${INSTALL}/bin/relay-server serve" >/dev/null 2>&1; then
    pkill -f "${INSTALL}/bin/relay-server serve" 2>/dev/null || true
    sleep 1
  fi
  rm -f /etc/systemd/system/relay-server.service
  rm -f "$INSTALL/bin/relay-server"
}

write_systemd_core() {
  retire_relay_server_http_unit
  if [[ "$GIT_BACKEND_PORT" == "$GIT_PORT" ]]; then
    die "RELAY_GIT_DAEMON_PORT ($GIT_BACKEND_PORT) must differ from RELAY_GIT_PORT ($GIT_PORT)"
  fi
  cat >/etc/systemd/system/relay-gateway.service <<EOF
[Unit]
Description=Relay HTTP gateway
After=network.target relay-git-daemon.service relay-git-proxy.service relay-ipfs.service
Wants=relay-git-daemon.service relay-git-proxy.service relay-ipfs.service

[Service]
Type=simple
User=relay
Group=relay
WorkingDirectory=$INSTALL
Environment=RELAY_REPO_PATH=$INSTALL/data
Environment=RELAY_INSTALL_ROOT=$INSTALL
Environment=RELAY_STATIC_DIR=$INSTALL/www
Environment=RELAY_HTTP_PORT=$HTTP_PORT
Environment=RELAY_BIND=0.0.0.0:$HTTP_PORT
Environment=RELAY_FEATURES_STATE_PATH=$FEATURES_JSON
Environment=RUST_LOG=info
EnvironmentFile=-$INSTALL/relay.env
# Soft/hard memory caps scale with the VM (Spot medium ~4Gi vs large ~16Gi).
# 80% was too high: SSR wedges at ~10Gi RSS on Large (Recv-Q 129) before MemoryMax.
MemoryHigh=45%
MemoryMax=55%
OOMPolicy=stop
# Forge sync only in Pre (peers run after HTTP is up). Allow greenfield catalog clone.
TimeoutStartSec=300
ExecStartPre=$INSTALL/bin/relay-startup-catalog-sync.sh --phase=pre
ExecStart=$INSTALL/bin/$RELAY_HTTP_BIN serve
# Leading '-' : peer-fetch failures must not fail the unit; HTTP already serves.
ExecStartPost=-$INSTALL/bin/relay-startup-catalog-sync.sh --phase=peers
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

  cat >/etc/systemd/system/relay-git-daemon.service <<EOF
[Unit]
Description=Relay git daemon (loopback only)
After=network.target

[Service]
Type=simple
User=relay
Group=relay
EnvironmentFile=-$INSTALL/relay.env
ExecStart=/usr/bin/git daemon --reuseaddr --listen=127.0.0.1 --base-path=$INSTALL/data --export-all --informative-errors --enable=receive-pack --max-connections=64 --port=$GIT_BACKEND_PORT $INSTALL/data
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

  cat >/etc/systemd/system/relay-git-proxy.service <<EOF
[Unit]
Description=Relay git:// proxy (first-push provision)
After=network.target relay-git-daemon.service
Requires=relay-git-daemon.service
PartOf=relay-git-daemon.service

[Service]
Type=simple
User=relay
Group=relay
WorkingDirectory=$INSTALL
Environment=RELAY_REPO_PATH=$INSTALL/data
Environment=RELAY_GIT_DAEMON_PORT=$GIT_BACKEND_PORT
Environment=RELAY_GIT_BACKEND=127.0.0.1:$GIT_BACKEND_PORT
Environment=RUST_LOG=info
EnvironmentFile=-$INSTALL/relay.env
ExecStart=$INSTALL/bin/relay-git-proxy
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
  write_reconcile_systemd
  write_gateway_liveness_systemd
  systemctl daemon-reload
  systemctl enable relay-gateway relay-git-daemon relay-git-proxy 2>/dev/null || true
  systemctl enable --now relay-gateway-liveness.timer 2>/dev/null || true
}

ensure_reconcile_seed() {
  mkdir -p "$INSTALL/etc"
  local rf="$INSTALL/etc/reconcile-allowlist"
  if [[ ! -f "$rf" ]]; then
    cat >"$rf" <<'RECONEOF'
# relay-master paths (relative to repo root) + SHA-256 of the blob at catalog HEAD.
# Format: path<TAB>sha256_hex
# Example: scripts/deploy/example.sh	abc123deadbeef...
RECONEOF
    chmod 600 "$rf"
    chown root:root "$rf"
  fi
}

write_reconcile_systemd() {
  ensure_reconcile_seed
  [[ -f "$INSTALL/bin/relay-reconcile.sh" ]] || return 0
  cat >/etc/systemd/system/relay-reconcile.service <<EOF
[Unit]
Description=Relay Helm-style reconcile (relay-master manifest)
After=network.target

[Service]
Type=oneshot
ExecStart=$INSTALL/bin/relay-reconcile.sh
EnvironmentFile=-$INSTALL/relay.env
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

  cat >/etc/systemd/system/relay-reconcile.timer <<EOF
[Unit]
Description=Timer for relay-reconcile (Helm-style deploy from relay-master)

[Timer]
OnBootSec=5min
OnUnitActiveSec=15min
RandomizedDelaySec=3min
Unit=relay-reconcile.service
Persistent=true

[Install]
WantedBy=timers.target
EOF
}

write_gateway_liveness_systemd() {
  [[ -f "$INSTALL/bin/relay-gateway-liveness.sh" ]] || return 0
  cat >/etc/systemd/system/relay-gateway-liveness.service <<EOF
[Unit]
Description=Relay gateway loopback liveness (SSR accept-queue wedge)
After=relay-gateway.service

[Service]
Type=oneshot
ExecStart=$INSTALL/bin/relay-gateway-liveness.sh
EnvironmentFile=-$INSTALL/relay.env
EOF
  cat >/etc/systemd/system/relay-gateway-liveness.timer <<'EOF'
[Unit]
Description=Relay gateway loopback liveness every minute

[Timer]
OnBootSec=90s
OnUnitActiveSec=60s
AccuracySec=15s
Persistent=true
Unit=relay-gateway-liveness.service

[Install]
WantedBy=timers.target
EOF
}

maybe_enable_reconcile_timer() {
  command -v systemctl >/dev/null 2>&1 || return 0
  case "${RELAY_RECONCILE_ENABLE_TIMER:-0}" in
    1|yes|true|TRUE) ;;
    *) return 0 ;;
  esac
  systemctl enable relay-reconcile.timer 2>/dev/null || true
  systemctl start relay-reconcile.timer 2>/dev/null || true
  log "relay-reconcile.timer enabled (RELAY_RECONCILE_ENABLE_TIMER=1)"
}

prompt_features() {
  local piper=0 piper_voices="$PIPER_DEFAULT_VOICE_IDS" piper_default_voice="$PIPER_DEFAULT_VOICE_ID_DEFAULT" npm_en=0 npm_pkgs="songwalker-js" trans=0 trans_pkgs="" lt=0
  if [[ "${RELAY_INSTALL_NONINTERACTIVE:-}" == "1" ]]; then
    [[ "${RELAY_FEAT_PIPER:-0}" == "1" ]] && piper=1
    [[ -n "${RELAY_FEAT_PIPER_VOICES:-}" ]] && piper_voices="$(piper_normalize_voice_ids "${RELAY_FEAT_PIPER_VOICES}")"
    [[ -n "${RELAY_FEAT_PIPER_DEFAULT_VOICE:-}" ]] && piper_default_voice="${RELAY_FEAT_PIPER_DEFAULT_VOICE}"
    if [[ "$piper" == "1" && " $piper_voices " != *" $piper_default_voice "* ]]; then
      piper_voices="$(piper_normalize_voice_ids "$piper_default_voice $piper_voices")"
    fi
    if [[ -n "${RELAY_FEAT_NPM_PKGS:-}" ]]; then npm_en=1; npm_pkgs="${RELAY_FEAT_NPM_PKGS}"; fi
    [[ "${RELAY_FEAT_TRANSLATION:-0}" == "1" ]] && trans=1
    trans_pkgs="${RELAY_FEAT_TRANSLATION_PKGS:-}"
    [[ "${RELAY_FEAT_LIBRETRANSLATE:-0}" == "1" ]] && lt=1
    printf '%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0' "$piper" "$piper_voices" "$piper_default_voice" "$npm_en" "$npm_pkgs" "$trans" "$trans_pkgs" "$lt"
    return
  fi
  if [[ ! -t 0 ]]; then
    log "non-TTY: set RELAY_INSTALL_NONINTERACTIVE=1 … RELAY_FEAT_LIBRETRANSLATE=0/1 RELAY_LIBRETRANSLATE_LOAD_ONLY=en,ru"
    printf '%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0' "0" "$PIPER_DEFAULT_VOICE_IDS" "$PIPER_DEFAULT_VOICE_ID_DEFAULT" "0" "" "0" "" "0"
    return
  fi
  read -r -p "Enable Piper TTS (HTTP on port $PIPER_HTTP_PORT)? [y/N] " a
  [[ "${a,,}" == "y" ]] && piper=1
  if [[ "$piper" == "1" ]]; then
    echo "Available starter Piper voices: en_GB-cori-medium en_GB-cori-high en_GB-alba-medium en_US-lessac-medium"
    read -r -p "Voice ids to install (space-separated) [$PIPER_DEFAULT_VOICE_IDS]: " pv
    [[ -n "$pv" ]] && piper_voices="$(piper_normalize_voice_ids "$pv")"
    read -r -p "Default Piper voice id [$PIPER_DEFAULT_VOICE_ID_DEFAULT]: " pd
    [[ -n "$pd" ]] && piper_default_voice="$pd"
    if [[ " $piper_voices " != *" $piper_default_voice "* ]]; then
      piper_voices="$(piper_normalize_voice_ids "$piper_default_voice $piper_voices")"
    fi
  fi
  read -r -p "Install npm packages into node_extensions (e.g. songwalker-js)? [y/N] " b
  if [[ "${b,,}" == "y" ]]; then
    npm_en=1
    read -r -p "Package names (space-separated) [songwalker-js]: " p
    [[ -n "$p" ]] && npm_pkgs="$p"
  fi
  read -r -p "Enable offline text translation (Argos Translate local, no cloud)? [y/N] " c
  [[ "${c,,}" == "y" ]] && trans=1
  if [[ "$trans" == "1" ]]; then
    read -r -p "Argos package ids to install now (space-separated, e.g. translate-en_es), or leave empty: " t
    trans_pkgs="$t"
  fi
  read -r -p "Enable LibreTranslate HTTP API (port $LIBRETRANSLATE_PORT; POST /translate for chunked book jobs)? [y/N] " z
  [[ "${z,,}" == "y" ]] && lt=1
  if [[ "$lt" == "1" ]]; then
    read -r -p "LibreTranslate --load-only language codes [en,ru]: " lo
    [[ -n "$lo" ]] && RELAY_LIBRETRANSLATE_LOAD_ONLY="$lo"
  fi
  printf '%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0' "$piper" "$piper_voices" "$piper_default_voice" "$npm_en" "$npm_pkgs" "$trans" "$trans_pkgs" "$lt"
}

pkgs_to_json_array() {
  local s="$1"
  [[ -z "$s" ]] && echo '[]' && return
  local a=() i
  read -ra i <<<"$s"
  printf '['
  local first=1
  for x in "${i[@]}"; do
    [[ -z "$x" ]] && continue
    [[ $first -eq 1 ]] || printf ','
    first=0
    printf '"%s"' "${x//\"/}"
  done
  printf ']'
}

# If binaries exist but state was never written (manual copy / old install), create minimal features.json.
bootstrap_features_json_if_missing() {
  [[ -f "$FEATURES_JSON" ]] && return 0
  [[ -f "$INSTALL/bin/$RELAY_HTTP_BIN" ]] || return 0
  log "No $FEATURES_JSON — bootstrapping minimal state (Piper/npm/translation off). Use reconfigure-features to enable."
  write_features_json 0 0 "[]" 0 "$(pkgs_to_json_array "")" 0
}

# --- Vercel DNS (install-time; same token env as Docker/K8s: VERCEL_API_TOKEN, optional VERCEL_TEAM_ID) ---
RELAY_CONFIGURED_PUBLIC_FQDN=""

ensure_minimal_network_tools() {
  local pm
  pm="$(detect_pm)"
  command -v curl >/dev/null || install_packages "$pm" curl ca-certificates
  if ! command -v jq >/dev/null; then
    install_packages "$pm" jq 2>/dev/null || {
      command -v python3 >/dev/null || install_packages "$pm" python3
      python3 -m pip install --break-system-packages jq 2>/dev/null || true
    }
    command -v jq >/dev/null || die "jq required for Vercel DNS; install jq then re-run"
  fi
  if ! command -v dig >/dev/null && ! command -v host >/dev/null; then
    case "$pm" in
      apt) install_packages apt dnsutils ;;
      dnf|yum) install_packages "$pm" bind-utils ;;
      zypper) install_packages zypper bind-utils ;;
      apk) install_packages apk bind-tools ;;
      pacman) install_packages pacman bind-tools ;;
      *) ;;
    esac
  fi
}

relay_get_public_ipv4() {
  local ip url
  for url in "https://api.ipify.org" "https://ipv4.icanhazip.com" "https://ifconfig.me/ip"; do
    ip=$(curl -fsS -m 15 "$url" 2>/dev/null | tr -d '\r\n' || true)
    if [[ "$ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "$ip"
      return 0
    fi
  done
  return 1
}

vercel_dns_records_list_url() {
  local domain="$1" name="$2" type="$3"
  local base="https://api.vercel.com/v4/domains/${domain}/records"
  if [[ -z "$name" ]]; then
    if [[ -n "${VERCEL_TEAM_ID:-}" ]]; then
      echo "${base}?teamId=${VERCEL_TEAM_ID}&type=${type}"
    else
      echo "${base}?type=${type}"
    fi
  else
    if [[ -n "${VERCEL_TEAM_ID:-}" ]]; then
      echo "${base}?teamId=${VERCEL_TEAM_ID}&name=${name}&type=${type}"
    else
      echo "${base}?name=${name}&type=${type}"
    fi
  fi
}

# Upsert A record; name may be empty for apex.
vercel_dns_upsert_a() {
  local domain="$1" name="$2" value="$3"
  local ttl="${RELAY_DNS_TTL:-60}"
  local token="${VERCEL_API_TOKEN:-${VERCEL_TOKEN:-}}"
  [[ -n "$token" ]] || die "vercel_dns_upsert_a: missing VERCEL_API_TOKEN"
  local auth_header="Authorization: Bearer ${token}"
  local base="https://api.vercel.com"
  local list_url team_q
  list_url="$(vercel_dns_records_list_url "$domain" "$name" A)"
  local list_raw rec_id
  list_raw=$(curl -sS -H "$auth_header" "$list_url" || true)
  echo "$list_raw" | jq -e . >/dev/null 2>&1 || die "Vercel DNS list failed for ${domain}: ${list_raw:0:400}"
  if [[ -z "$name" ]]; then
    rec_id=$(echo "$list_raw" | jq -r '.records[]? | select((.name==null or .name=="" or .name=="@") and .type=="A") | .id // .uid' | head -n1 || true)
  else
    rec_id=$(echo "$list_raw" | jq -r --arg n "$name" '.records[]? | select(.name==$n and .type=="A") | .id // .uid' | head -n1 || true)
  fi

  if [[ -n "$rec_id" && "$rec_id" != "null" ]]; then
    local patch_url="${base}/v4/domains/${domain}/records/${rec_id}"
    [[ -n "${VERCEL_TEAM_ID:-}" ]] && patch_url="${patch_url}?teamId=${VERCEL_TEAM_ID}"
    local body
    body=$(jq -n --arg v "$value" --argjson t "$ttl" '{ value: $v, ttl: $t }')
    curl -fsS -X PATCH -H "$auth_header" -H 'Content-Type: application/json' -d "$body" "$patch_url" >/dev/null && return 0
  else
    local create_url="${base}/v4/domains/${domain}/records"
    [[ -n "${VERCEL_TEAM_ID:-}" ]] && create_url="${create_url}?teamId=${VERCEL_TEAM_ID}"
    local body
    if [[ -z "$name" ]]; then
      body=$(jq -n --arg v "$value" --arg t "A" --argjson ttl "$ttl" '{ name: "", value: $v, type: $t, ttl: $ttl }')
    else
      body=$(jq -n --arg n "$name" --arg v "$value" --arg t "A" --argjson ttl "$ttl" '{ name: $n, value: $v, type: $t, ttl: $ttl }')
    fi
    curl -fsS -X POST -H "$auth_header" -H 'Content-Type: application/json' -d "$body" "$create_url" >/dev/null && return 0
  fi
  return 1
}

vercel_list_domain_names() {
  local token="$1"
  local url="https://api.vercel.com/v5/domains"
  [[ -n "${VERCEL_TEAM_ID:-}" ]] && url="${url}?teamId=${VERCEL_TEAM_ID}"
  local raw
  raw=$(curl -fsS -H "Authorization: Bearer ${token}" "$url")
  echo "$raw" | jq -r '.domains[]?.name | select(.!=null)'
}

# Sets RESOLVED_VERCEL_DOMAIN and RESOLVED_VERCEL_RECORD_NAME (may be empty for apex).
resolve_vercel_zone_for_fqdn() {
  local fqdn="$1" token="$2"
  RESOLVED_VERCEL_DOMAIN=""
  RESOLVED_VERCEL_RECORD_NAME=""
  if [[ -n "${RELAY_VERCEL_DOMAIN:-}" ]]; then
    local d="${RELAY_VERCEL_DOMAIN}"
    if [[ "$fqdn" == "$d" ]]; then
      RESOLVED_VERCEL_DOMAIN="$d"
      RESOLVED_VERCEL_RECORD_NAME=""
      return 0
    fi
    local suffix=".$d"
    if [[ "$fqdn" == *"$suffix" ]]; then
      RESOLVED_VERCEL_DOMAIN="$d"
      RESOLVED_VERCEL_RECORD_NAME="${fqdn%"$suffix"}"
      return 0
    fi
    die "RELAY_VERCEL_DOMAIN=$d does not match FQDN=$fqdn"
  fi

  local raw best="" best_len=0 d len
  raw=$(curl -fsS -H "Authorization: Bearer ${token}" "https://api.vercel.com/v5/domains$([[ -n "${VERCEL_TEAM_ID:-}" ]] && echo "?teamId=${VERCEL_TEAM_ID}")")
  echo "$raw" | jq -e . >/dev/null 2>&1 || die "Vercel domains list failed: ${raw:0:500}"

  while IFS= read -r d; do
    [[ -z "$d" ]] && continue
    if [[ "$fqdn" == "$d" ]]; then
      RESOLVED_VERCEL_DOMAIN="$d"
      RESOLVED_VERCEL_RECORD_NAME=""
      return 0
    fi
    if [[ "$fqdn" == *".$d" ]]; then
      len=${#d}
      if [[ $len -gt $best_len ]]; then
        best_len=$len
        best="$d"
      fi
    fi
  done < <(echo "$raw" | jq -r '.domains[]?.name | select(.!=null)')

  [[ -n "$best" ]] || die "No Vercel domain matches FQDN=$fqdn. Add the domain in Vercel or set RELAY_VERCEL_DOMAIN."
  RESOLVED_VERCEL_DOMAIN="$best"
  RESOLVED_VERCEL_RECORD_NAME="${fqdn%.$best}"
}

wait_dns_a_record() {
  local fqdn="$1" want_ip="$2"
  local max="${RELAY_DNS_WAIT_ATTEMPTS:-36}"
  local sleep_s="${RELAY_DNS_WAIT_SLEEP:-5}"
  local i=0 got
  while [[ $i -lt "$max" ]]; do
    got=""
    if command -v dig >/dev/null 2>&1; then
      got=$(dig +short A "$fqdn" @8.8.8.8 2>/dev/null | head -n1 | tr -d '\r\n' || true)
    elif command -v host >/dev/null 2>&1; then
      got=$(host -t A "$fqdn" 8.8.8.8 2>/dev/null | awk '/has address/ { print $4; exit }' | tr -d '\r\n' || true)
    else
      got=$(python3 -c "import socket; print(socket.gethostbyname('$fqdn'))" 2>/dev/null | tr -d '\r\n' || true)
    fi
    if [[ "$got" == "$want_ip" ]]; then
      log "DNS OK: $fqdn -> $got"
      return 0
    fi
    log "Waiting for DNS: want A $fqdn -> $want_ip (got '${got:-empty}') [$((i + 1))/$max]"
    sleep "$sleep_s"
    i=$((i + 1))
  done
  die "DNS did not resolve $fqdn to $want_ip after $max attempts (${sleep_s}s apart). Check Vercel dashboard and TTL."
}

configure_vercel_dns_first() {
  RELAY_CONFIGURED_PUBLIC_FQDN=""
  if [[ "${RELAY_SKIP_VERCEL_DNS:-}" == "1" ]]; then
    log "Skipping Vercel DNS (RELAY_SKIP_VERCEL_DNS=1)"
    return 0
  fi

  local fqdn="" token=""
  fqdn="${RELAY_PUBLIC_FQDN:-${RELAY_DNS_FQDN:-}}"
  token="${VERCEL_API_TOKEN:-${VERCEL_TOKEN:-}}"

  if [[ "${RELAY_INSTALL_NONINTERACTIVE:-}" == "1" ]]; then
    [[ -n "$fqdn" && -n "$token" ]] || die "Non-interactive install needs RELAY_PUBLIC_FQDN (or RELAY_DNS_FQDN) and VERCEL_API_TOKEN for DNS, or set RELAY_SKIP_VERCEL_DNS=1"
  else
    if [[ ! -t 0 ]]; then
      if [[ -z "$fqdn" || -z "$token" ]]; then
        log "Non-TTY install: set RELAY_PUBLIC_FQDN + VERCEL_API_TOKEN, or RELAY_SKIP_VERCEL_DNS=1 — skipping DNS"
        return 0
      fi
    else
      read -r -p "Update Vercel DNS to this server's public IPv4? [Y/n] " dns_yn
      if [[ "${dns_yn,,}" == "n" ]]; then
        log "Skipping Vercel DNS (user declined)"
        return 0
      fi
      while [[ -z "$fqdn" ]]; do
        read -r -p "Full hostname (e.g. atlanta1.relaygateway.net): " fqdn
        fqdn="${fqdn// /}"
      done
      if [[ -z "$token" ]]; then
        read -r -s -p "Vercel API token (same as VERCEL_API_TOKEN in Kubernetes/Docker): " token
        echo "" >&2
      fi
      [[ -n "$token" ]] || die "Vercel API token required"
    fi
  fi

  fqdn="${fqdn,,}"
  [[ "$fqdn" =~ ^[a-z0-9.-]+$ ]] || die "Invalid FQDN: $fqdn"

  local pub_ip
  pub_ip="$(relay_get_public_ipv4)" || die "Could not detect public IPv4 (need outbound HTTPS)"
  log "Public IPv4 (detected): $pub_ip"

  export VERCEL_API_TOKEN="$token"
  resolve_vercel_zone_for_fqdn "$fqdn" "$token"
  log "Vercel zone: $RESOLVED_VERCEL_DOMAIN  record name: '${RESOLVED_VERCEL_RECORD_NAME:-@}'"

  vercel_dns_upsert_a "$RESOLVED_VERCEL_DOMAIN" "$RESOLVED_VERCEL_RECORD_NAME" "$pub_ip" || die "Vercel DNS upsert failed (check token, domain on account, and team id)"

  wait_dns_a_record "$fqdn" "$pub_ip"
  RELAY_CONFIGURED_PUBLIC_FQDN="$fqdn"
  log "Vercel DNS configured and verified for $fqdn"
}

# relay.env: only root (install/repair) may edit; relay user receives vars via systemd EnvironmentFile.
lock_relay_env_permissions() {
  local f="$INSTALL/relay.env"
  [[ -f "$f" ]] || return 0
  chown root:root "$f"
  chmod 600 "$f"
}

# Writes RELAY_PUBLIC_HOSTNAME from Vercel-configured FQDN and/or RELAY_PUBLIC_HOSTNAME env
# (needed when RELAY_SKIP_VERCEL_DNS=1 but Host-based routing still requires the node name).
append_relay_env_public_host() {
  local f="$INSTALL/relay.env"
  local h="${RELAY_CONFIGURED_PUBLIC_FQDN:-}"
  [[ -z "$h" ]] && h="${RELAY_PUBLIC_HOSTNAME:-}"
  [[ -z "$h" ]] && return 0
  grep -q '^RELAY_PUBLIC_HOSTNAME=' "$f" 2>/dev/null && return 0
  echo "RELAY_PUBLIC_HOSTNAME=$h" >>"$f"
}

# Optional unique id for /api/config and ops (e.g. relay-dallas1).
append_relay_env_server_id() {
  local f="$INSTALL/relay.env"
  local id=""
  if [[ "${RELAY_INSTALL_NONINTERACTIVE:-}" == "1" ]]; then
    id="${RELAY_SERVER_ID:-}"
  elif [[ -t 0 ]]; then
    read -r -p "RELAY_SERVER_ID — unique name for this node (e.g. relay-dallas1; empty to skip): " id
    id="${id// /}"
  fi
  [[ -z "$id" ]] && return 0
  grep -q '^RELAY_SERVER_ID=' "$f" 2>/dev/null && return 0
  echo "RELAY_SERVER_ID=$id" >>"$f"
}

# Semicolon-separated peer hostnames for /api/config and mesh sync (RELAY_MASTER_PEER_LIST).
# This is also the bootstrap list of designated “master” / mesh anchor relays unless overridden.
append_relay_env_peer_list() {
  local f="$INSTALL/relay.env"
  local peers=""
  if [[ "${RELAY_INSTALL_NONINTERACTIVE:-}" == "1" ]]; then
    peers="${RELAY_MASTER_PEER_LIST:-$DEFAULT_RELAY_MASTER_PEER_LIST}"
  elif [[ -t 0 ]]; then
    read -r -p "Master mesh relay peers — semicolon-separated FQDNs [${DEFAULT_RELAY_MASTER_PEER_LIST}]: " peers
    peers="${peers// /}"
    [[ -z "$peers" ]] && peers="$DEFAULT_RELAY_MASTER_PEER_LIST"
  else
    peers="${RELAY_MASTER_PEER_LIST:-$DEFAULT_RELAY_MASTER_PEER_LIST}"
  fi
  peers="${peers// /}"
  [[ -z "$peers" ]] && return 0
  grep -q '^RELAY_MASTER_PEER_LIST=' "$f" 2>/dev/null && return 0
  echo "RELAY_MASTER_PEER_LIST=$peers" >>"$f"
}

# Default on: first git push (receive-pack) and POST /api/git/ensure-bare/:name may create RELAY_REPO_PATH/<name>.git.
append_relay_env_git_ensure_bare_default() {
  local f="$INSTALL/relay.env"
  grep -q '^RELAY_GIT_ENSURE_BARE_ENABLED=' "$f" 2>/dev/null && return 0
  echo "RELAY_GIT_ENSURE_BARE_ENABLED=1" >>"$f"
}

# Catalog worktree + clone URL + trust root (write-if-missing). Never pair catalog path with RELAY_AUTHORIZED_REPOS_PATH.
append_relay_env_catalog_defaults() {
  local f="$INSTALL/relay.env"
  touch "$f"
  if grep -q '^RELAY_AUTHORIZED_REPOS_PATH=' "$f" 2>/dev/null; then
    return 0
  fi
  if ! grep -q '^RELAY_MASTER_CATALOG_PATH=' "$f" 2>/dev/null; then
    local path="${RELAY_MASTER_CATALOG_PATH:-$DEFAULT_RELAY_MASTER_CATALOG_PATH}"
    [[ -n "$path" ]] && echo "RELAY_MASTER_CATALOG_PATH=$path" >>"$f"
  fi
  if ! grep -q '^RELAY_MASTER_CATALOG_GIT_URL=' "$f" 2>/dev/null; then
    local url="${RELAY_MASTER_CATALOG_GIT_URL:-$DEFAULT_RELAY_MASTER_CATALOG_GIT_URL}"
    [[ -n "$url" ]] && echo "RELAY_MASTER_CATALOG_GIT_URL=$url" >>"$f"
  fi
  if ! grep -q '^RELAY_MASTER_TRUST_ROOT=' "$f" 2>/dev/null; then
    local root="${RELAY_MASTER_TRUST_ROOT:-}"
    [[ -n "$root" ]] && echo "RELAY_MASTER_TRUST_ROOT=$root" >>"$f"
  fi
}

# Commit-pinned embed/WASM CDN (`docs/RELAY_WEB_ASSETS.md`). Write-if-missing so
# reclaimed nodes get the same catalog as iad1/hkg1 without clobbering overrides.
append_relay_env_web_assets_defaults() {
  local f="$INSTALL/relay.env"
  touch "$f"
  if ! grep -q '^RELAY_WEB_ASSETS_REPO=' "$f" 2>/dev/null; then
    echo "RELAY_WEB_ASSETS_REPO=${RELAY_WEB_ASSETS_REPO:-relay-web-assets}" >>"$f"
  fi
  if ! grep -q '^RELAY_WEB_ASSETS_BRANCH=' "$f" 2>/dev/null; then
    echo "RELAY_WEB_ASSETS_BRANCH=${RELAY_WEB_ASSETS_BRANCH:-release}" >>"$f"
  fi
}

# Parked SSR budget. Write-if-missing so reclaim/update get the wedge-safe defaults.
append_relay_env_ssr_defaults() {
  local f="$INSTALL/relay.env"
  touch "$f"
  # Article SSR is ~4s; directory pages ~18s. A 4s cap cancels before the body
  # exists, and the client keeps that empty/homepage shell instead of refetching.
  if grep -q '^RELAY_SSR_TIMEOUT_SECS=' "$f" 2>/dev/null; then
    sed -i 's/^RELAY_SSR_TIMEOUT_SECS=.*/RELAY_SSR_TIMEOUT_SECS=30/' "$f"
  else
    echo "RELAY_SSR_TIMEOUT_SECS=${RELAY_SSR_TIMEOUT_SECS:-30}" >>"$f"
  fi
  if ! grep -q '^RELAY_SSR_MAX_CONCURRENT=' "$f" 2>/dev/null; then
    echo "RELAY_SSR_MAX_CONCURRENT=${RELAY_SSR_MAX_CONCURRENT:-2}" >>"$f"
  fi
  # Drop the homepage-for-every-URL rewrite if a previous install wrote it.
  if grep -q '^RELAY_SSR_SHELL_ONLY=' "$f" 2>/dev/null; then
    sed -i '/^RELAY_SSR_SHELL_ONLY=/d' "$f"
  fi
}

# Park/embed RUM (TTFC + CWV-ish beacons → /api/rum). Write-if-missing so operators can set 0.
append_relay_env_rum_defaults() {
  local f="$INSTALL/relay.env"
  touch "$f"
  if ! grep -q '^RELAY_RUM_ENABLED=' "$f" 2>/dev/null; then
    echo "RELAY_RUM_ENABLED=${RELAY_RUM_ENABLED:-1}" >>"$f"
  fi
}

# Set or replace KEY=VALUE in relay.env.
upsert_relay_env_var() {
  local f="$1" key="$2" val="$3"
  touch "$f"
  if grep -q "^${key}=" "$f" 2>/dev/null; then
    local tmp
    tmp="$(mktemp)"
    grep -v "^${key}=" "$f" >"$tmp" || true
    printf '%s=%s\n' "$key" "$val" >>"$tmp"
    mv "$tmp" "$f"
  else
    printf '%s=%s\n' "$key" "$val" >>"$f"
  fi
}

# Merge mesh peer list + secrets from install environment (fleet redeploy / relay-master-deploy).
merge_relay_env_mesh_from_env() {
  local f="$INSTALL/relay.env"
  touch "$f"
  local merge="${RELAY_MERGE_MESH_ENV:-0}"
  local peers="${RELAY_MASTER_PEER_LIST:-}"
  local peer_secret="${RELAY_PEER_SECRET:-}"
  local mesh_secret="${RELAY_MASTER_MESH_SECRET:-}"

  if [[ -n "$peers" ]]; then
    if [[ "$merge" == "1" ]] || ! grep -q '^RELAY_MASTER_PEER_LIST=' "$f" 2>/dev/null; then
      upsert_relay_env_var "$f" RELAY_MASTER_PEER_LIST "$peers"
    fi
  fi
  if [[ -n "$peer_secret" ]]; then
    if [[ "$merge" == "1" ]] || ! grep -q '^RELAY_PEER_SECRET=' "$f" 2>/dev/null; then
      upsert_relay_env_var "$f" RELAY_PEER_SECRET "$peer_secret"
    fi
  fi
  if [[ -n "$mesh_secret" ]]; then
    if [[ "$merge" == "1" ]] || ! grep -q '^RELAY_MASTER_MESH_SECRET=' "$f" 2>/dev/null; then
      upsert_relay_env_var "$f" RELAY_MASTER_MESH_SECRET "$mesh_secret"
    fi
  fi
  if ! grep -q '^RELAY_AUTHORIZED_REPOS_PATH=' "$f" 2>/dev/null; then
    local catalog_path="${RELAY_MASTER_CATALOG_PATH:-}"
    local catalog_url="${RELAY_MASTER_CATALOG_GIT_URL:-}"
    local trust_root="${RELAY_MASTER_TRUST_ROOT:-}"
    if [[ -n "$catalog_path" ]]; then
      if [[ "$merge" == "1" ]] || ! grep -q '^RELAY_MASTER_CATALOG_PATH=' "$f" 2>/dev/null; then
        upsert_relay_env_var "$f" RELAY_MASTER_CATALOG_PATH "$catalog_path"
      fi
    fi
    if [[ -n "$catalog_url" ]]; then
      if [[ "$merge" == "1" ]] || ! grep -q '^RELAY_MASTER_CATALOG_GIT_URL=' "$f" 2>/dev/null; then
        upsert_relay_env_var "$f" RELAY_MASTER_CATALOG_GIT_URL "$catalog_url"
      fi
    fi
    if [[ -n "$trust_root" ]]; then
      if [[ "$merge" == "1" ]] || ! grep -q '^RELAY_MASTER_TRUST_ROOT=' "$f" 2>/dev/null; then
        upsert_relay_env_var "$f" RELAY_MASTER_TRUST_ROOT "$trust_root"
      fi
    fi
  fi
  lock_relay_env_permissions
}

do_install() {
  need_root
  if [[ -f "$FEATURES_JSON" ]] && [[ "${RELAY_INSTALL_FRESH:-}" != "1" ]]; then
    log "Already installed ($FEATURES_JSON). Commands: update | repair | reconfigure-features"
    exit 0
  fi
  ensure_minimal_network_tools
  configure_vercel_dns_first
  ensure_base_deps
  ensure_user_relay
  { IFS= read -r -d '' piper_en && IFS= read -r -d '' piper_voices && IFS= read -r -d '' piper_default_voice && IFS= read -r -d '' npm_en && IFS= read -r -d '' npm_pkgs && IFS= read -r -d '' trans_en && IFS= read -r -d '' trans_pkgs && IFS= read -r -d '' lt_en; } < <(prompt_features)
  local piper_voices_json npm_json trans_json
  piper_voices_json="$(pkgs_to_json_array "$piper_voices")"
  npm_json="$(pkgs_to_json_array "$npm_pkgs")"
  trans_json="$(pkgs_to_json_array "$trans_pkgs")"
  write_features_json "$piper_en" "$piper_voices_json" "$piper_default_voice" "$npm_en" "$npm_json" "$trans_en" "$trans_json" "$lt_en"
  install_binaries
  write_gitconfig_hooks
  write_systemd_core
  [[ "$piper_en" == "1" ]] && install_piper_artifacts "$piper_voices" "$piper_default_voice"
  [[ "$npm_en" == "1" && -n "$npm_pkgs" ]] && install_npm_extensions "$npm_pkgs"
  if [[ "$trans_en" == "1" ]]; then
    install_translation_artifacts $trans_pkgs
  fi
  if [[ "$lt_en" == "1" ]]; then
    install_libretranslate_artifacts
  fi
  refresh_features_inventory

  touch "$INSTALL/relay.env"
  chown root:root "$INSTALL/relay.env"
  chmod 600 "$INSTALL/relay.env"
  append_relay_env_public_host
  append_relay_env_server_id
  append_relay_env_peer_list
  append_relay_env_git_ensure_bare_default
  append_relay_env_catalog_defaults
  append_relay_env_web_assets_defaults
  append_relay_env_ssr_defaults
  append_relay_env_rum_defaults
  merge_relay_env_mesh_from_env
  lock_relay_env_permissions
  ensure_ipfs_kubo
  systemctl enable relay-gateway relay-git-daemon relay-git-proxy
  systemctl restart relay-git-daemon relay-git-proxy relay-gateway
  maybe_enable_reconcile_timer
  [[ "$piper_en" == "1" ]] && systemctl restart relay-tts-piper 2>/dev/null || true
  [[ "$lt_en" == "1" ]] && systemctl restart libretranslate 2>/dev/null || true

  maybe_validate_deploy_ssh_key
  maybe_ufw
  log "Install complete. HTTP :$HTTP_PORT  git :$GIT_PORT  LibreTranslate :$LIBRETRANSLATE_PORT (if enabled)  config: POST /api/config"
}

do_update() {
  need_root
  bootstrap_features_json_if_missing
  [[ -f "$FEATURES_JSON" ]] || die "run install first (or ensure $INSTALL/bin/$RELAY_HTTP_BIN exists)"
  local piper_en piper_voices piper_default_voice
  piper_en="$(jq -r '.features.piper_tts.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  piper_voices="$(jq -r '.features.piper_tts.voices[]?.id' "$FEATURES_JSON" 2>/dev/null | tr '\n' ' ' | xargs echo -n)"
  piper_default_voice="$(jq -r '.features.piper_tts.default_voice_id // empty' "$FEATURES_JSON" 2>/dev/null || true)"
  [[ -n "$piper_default_voice" ]] || piper_default_voice="$PIPER_DEFAULT_VOICE_ID_DEFAULT"
  [[ -n "$piper_voices" ]] || piper_voices="$piper_default_voice"
  install_binaries
  write_systemd_core
  write_gitconfig_hooks
  if [[ "$piper_en" == "true" ]]; then
    install_piper_artifacts "$piper_voices" "$piper_default_voice"
  fi
  refresh_features_inventory
  append_relay_env_catalog_defaults
  append_relay_env_web_assets_defaults
  append_relay_env_ssr_defaults
  append_relay_env_rum_defaults
  merge_relay_env_mesh_from_env
  ensure_ipfs_kubo
  systemctl restart relay-git-daemon relay-git-proxy relay-gateway
  maybe_enable_reconcile_timer
  systemctl try-restart relay-tts-piper 2>/dev/null || true
  systemctl try-restart libretranslate 2>/dev/null || true
  maybe_ufw
  log "Binaries updated"
}

do_repair() {
  need_root
  bootstrap_features_json_if_missing
  [[ -f "$FEATURES_JSON" ]] || die "run install first, or deploy binaries to $INSTALL/bin first"
  ensure_base_deps
  ensure_user_relay
  local piper_en npm_en trans_en
  piper_en="$(jq -r '.features.piper_tts.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  npm_en="$(jq -r '.features.npm_extensions.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  trans_en="$(jq -r '.features.text_translation.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  local lt_en
  lt_en="$(jq -r '.features.libretranslate_api.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  chown -R relay:relay "$INSTALL"
  install_binaries
  write_gitconfig_hooks
  write_systemd_core
  if [[ "$piper_en" == "true" ]]; then
    local piper_voices piper_default_voice
    piper_voices="$(jq -r '.features.piper_tts.voices[]?.id' "$FEATURES_JSON" 2>/dev/null | tr '\n' ' ' | xargs echo -n)"
    piper_default_voice="$(jq -r '.features.piper_tts.default_voice_id // empty' "$FEATURES_JSON" 2>/dev/null || true)"
    [[ -n "$piper_default_voice" ]] || piper_default_voice="$PIPER_DEFAULT_VOICE_ID_DEFAULT"
    [[ -n "$piper_voices" ]] || piper_voices="$piper_default_voice"
    piper_install_ok || install_piper_artifacts "$piper_voices" "$piper_default_voice"
    systemctl enable relay-tts-piper 2>/dev/null || true
    systemctl restart relay-tts-piper 2>/dev/null || true
  fi
  if [[ "$npm_en" == "true" ]]; then
    local pkgs
    pkgs="$(jq -r '.features.npm_extensions.packages | join(" ")' "$FEATURES_JSON")"
    [[ -n "$pkgs" ]] && install_npm_extensions "$pkgs"
  fi
  if [[ "$trans_en" == "true" ]]; then
    if [[ ! -x "$INSTALL/lib/argos-venv/bin/python3" ]]; then
      local apks
      apks="$(jq -r '.features.text_translation.install_argos_packages | join(" ")' "$FEATURES_JSON" 2>/dev/null || true)"
      install_translation_artifacts $apks
    fi
  fi
  if [[ "$lt_en" == "true" ]]; then
    if [[ ! -x "$LIBRETRANSLATE_ROOT/venv/bin/libretranslate" ]]; then
      install_libretranslate_artifacts
    else
      local lo
      lo="$(jq -r '.features.libretranslate_api.load_only // "en,ru"' "$FEATURES_JSON" 2>/dev/null || echo en,ru)"
      write_libretranslate_systemd "$lo"
      systemctl enable libretranslate 2>/dev/null || true
      systemctl restart libretranslate 2>/dev/null || true
    fi
  fi
  refresh_features_inventory
  append_relay_env_catalog_defaults
  append_relay_env_web_assets_defaults
  append_relay_env_ssr_defaults
  append_relay_env_rum_defaults
  merge_relay_env_mesh_from_env
  lock_relay_env_permissions
  ensure_ipfs_kubo
  systemctl restart relay-git-daemon relay-git-proxy relay-gateway
  maybe_enable_reconcile_timer
  maybe_validate_deploy_ssh_key
  maybe_ufw
  log "Repair complete"
}

do_reconfigure_features() {
  need_root
  bootstrap_features_json_if_missing
  [[ -f "$FEATURES_JSON" ]] || die "run install first, or deploy binaries to $INSTALL/bin first"
  log "Reconfigure will replace feature installs. Continue? (features are only changed via this script)"
  if [[ "${RELAY_INSTALL_NONINTERACTIVE:-}" != "1" ]] && [[ -t 0 ]]; then
    read -r -p "[y/N] " c
    [[ "${c,,}" == "y" ]] || exit 0
  fi
  { IFS= read -r -d '' piper_en && IFS= read -r -d '' piper_voices && IFS= read -r -d '' piper_default_voice && IFS= read -r -d '' npm_en && IFS= read -r -d '' npm_pkgs && IFS= read -r -d '' trans_en && IFS= read -r -d '' trans_pkgs && IFS= read -r -d '' lt_en; } < <(prompt_features)
  local piper_voices_json npm_json trans_json
  piper_voices_json="$(pkgs_to_json_array "$piper_voices")"
  npm_json="$(pkgs_to_json_array "$npm_pkgs")"
  trans_json="$(pkgs_to_json_array "$trans_pkgs")"
  systemctl stop relay-tts-piper 2>/dev/null || true
  systemctl stop libretranslate 2>/dev/null || true
  write_features_json "$piper_en" "$piper_voices_json" "$piper_default_voice" "$npm_en" "$npm_json" "$trans_en" "$trans_json" "$lt_en"
  if [[ "$piper_en" == "1" ]]; then
    rm -rf "$INSTALL/lib/piper" 2>/dev/null || true
    install_piper_artifacts "$piper_voices" "$piper_default_voice"
    systemctl enable relay-tts-piper
    systemctl start relay-tts-piper
  else
    systemctl disable relay-tts-piper 2>/dev/null || true
    systemctl stop relay-tts-piper 2>/dev/null || true
  fi
  if [[ "$npm_en" == "1" && -n "$npm_pkgs" ]]; then
    rm -rf "$INSTALL/node_extensions/node_modules" 2>/dev/null || true
    install_npm_extensions "$npm_pkgs"
  else
    rm -rf "$INSTALL/node_extensions" 2>/dev/null || true
  fi
  if [[ "$trans_en" == "1" ]]; then
    rm -rf "$INSTALL/lib/argos-venv" 2>/dev/null || true
    install_translation_artifacts $trans_pkgs
  else
    rm -rf "$INSTALL/lib/argos-venv" 2>/dev/null || true
  fi
  if [[ "$lt_en" == "1" ]]; then
    install_libretranslate_artifacts
  else
    systemctl disable libretranslate 2>/dev/null || true
    systemctl stop libretranslate 2>/dev/null || true
  fi
  refresh_features_inventory
  ensure_ipfs_kubo
  systemctl restart relay-gateway
  maybe_ufw
  log "Features reconfigured"
}

# Optional: enforce that RELAY_DEPLOY_SSH_USER's authorized_keys includes the deploy key (see docs/SECRETS_POLICY.md).
maybe_validate_deploy_ssh_key() {
  case "${RELAY_VALIDATE_DEPLOY_SSH_KEY:-}" in
    1|yes|true|TRUE|Yes|YES) ;;
    *) return 0 ;;
  esac
  local user="${RELAY_DEPLOY_SSH_USER:-ari}"
  local want="${RELAY_DEPLOY_SSH_KEY_FINGERPRINT:-}"
  [[ -n "$want" ]] || die "RELAY_VALIDATE_DEPLOY_SSH_KEY is set but RELAY_DEPLOY_SSH_KEY_FINGERPRINT is empty (see docs/SECRETS_POLICY.md)"
  local home
  home="$(getent passwd "$user" | cut -d: -f6)"
  [[ -n "$home" ]] || die "RELAY_DEPLOY_SSH_USER not found: $user"
  local ak="$home/.ssh/authorized_keys"
  [[ -f "$ak" ]] || die "missing $ak — cannot validate deploy SSH key for $user"
  if ! ssh-keygen -lf "$ak" 2>/dev/null | grep -qF "$want"; then
    die "authorized_keys for $user has no key matching RELAY_DEPLOY_SSH_KEY_FINGERPRINT (run: ssh-keygen -lf -E sha256 $ak)"
  fi
  log "Deploy SSH key policy OK ($user: fingerprint present)"
}

maybe_ufw() {
  command -v ufw >/dev/null || return 0
  ufw allow 22/tcp 2>/dev/null || true
  ufw allow "$HTTP_PORT/tcp" 2>/dev/null || true
  ufw allow "$GIT_PORT/tcp" 2>/dev/null || true
  local piper_en
  piper_en="$(jq -r '.features.piper_tts.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  [[ "$piper_en" == "true" ]] && ufw allow "$PIPER_HTTP_PORT/tcp" 2>/dev/null || true
  local lt_u
  lt_u="$(jq -r '.features.libretranslate_api.enabled // false' "$FEATURES_JSON" 2>/dev/null || echo false)"
  [[ "$lt_u" == "true" ]] && ufw allow "$LIBRETRANSLATE_PORT/tcp" 2>/dev/null || true
  case "${RELAY_INSTALL_IPFS:-1}" in
    0|false|FALSE|no|NO) ;;
    *)
      ufw allow "${IPFS_SWARM_PORT}/tcp" 2>/dev/null || true
      ufw allow "${IPFS_SWARM_PORT}/udp" 2>/dev/null || true
      ;;
  esac
  ufw --force enable 2>/dev/null || true
}

do_refresh_features() {
  need_root
  bootstrap_features_json_if_missing
  [[ -f "$FEATURES_JSON" ]] || die "run install first (no $FEATURES_JSON)"
  refresh_features_inventory
  systemctl try-restart relay-gateway 2>/dev/null || true
  log "Feature inventory refreshed (Piper voices / Argos language pairs → $FEATURES_JSON)"
}

case "${1:-install}" in
  install) shift; do_install "$@" ;;
  update) shift; do_update "$@" ;;
  repair) shift; do_repair "$@" ;;
  reconfigure-features) shift; do_reconfigure_features "$@" ;;
  refresh-features) shift; do_refresh_features "$@" ;;
  -h|--help)
    echo "Usage: $0 {install|update|repair|reconfigure-features|refresh-features}"
    echo "  install   — Vercel DNS first (unless skipped), then base deps; Piper + npm + translation prompts (or RELAY_INSTALL_NONINTERACTIVE=1)"
    echo "  DNS env: RELAY_PUBLIC_FQDN, VERCEL_API_TOKEN, optional VERCEL_TEAM_ID, RELAY_VERCEL_DOMAIN, RELAY_SKIP_VERCEL_DNS=1"
    echo "  Features env: RELAY_FEAT_PIPER, RELAY_FEAT_PIPER_VOICES, RELAY_FEAT_PIPER_DEFAULT_VOICE, RELAY_FEAT_PIPER_CACHE_TTL_SECONDS, RELAY_FEAT_PIPER_CACHE_MAX_FILES, RELAY_FEAT_NPM_PKGS, RELAY_FEAT_TRANSLATION, RELAY_FEAT_TRANSLATION_PKGS, RELAY_FEAT_LIBRETRANSLATE, RELAY_LIBRETRANSLATE_LOAD_ONLY (see docs/INSTALL_FEATURES.md)"
    echo "  Node env: RELAY_SERVER_ID, RELAY_PUBLIC_HOSTNAME (if RELAY_SKIP_VERCEL_DNS=1), RELAY_MASTER_PEER_LIST (override; default mesh masters: $DEFAULT_RELAY_MASTER_PEER_LIST)"
    echo "  Deploy SSH policy (optional): RELAY_VALIDATE_DEPLOY_SSH_KEY=1, RELAY_DEPLOY_SSH_USER (default ari), RELAY_DEPLOY_SSH_KEY_FINGERPRINT (substring from ssh-keygen -lf -E sha256) — docs/SECRETS_POLICY.md"
    echo "  Startup catalog sync (optional): RELAY_STARTUP_CATALOG_SYNC=0 to skip ExecStartPre (default 1 when RELAY_MASTER_CATALOG_PATH has relay-catalog.json + scripts/sync-relay-catalog.sh); Slack: RELAY_NOTIFY_SLACK_WEBHOOK_URL"
    echo "  Reconcile (optional): RELAY_RECONCILE_ENABLED=1, RELAY_RECONCILE_ENABLE_TIMER=1 (systemd timer); deploy/relay-gateway.json in relay-master — docs/RELAY_RECONCILE_PLAN.md"
    echo "  Kubo/IPFS (default on): RELAY_INSTALL_IPFS=0 to skip; RELAY_KUBO_VERSION (default v0.32.1). API 127.0.0.1:5001, gateway 127.0.0.1:8081, swarm 4001/tcp+udp. Status: GET /api/status on the node FQDN."
    echo "  update    — refresh relay-gateway binaries and systemd unit from this directory; rescan feature inventory; install Kubo if missing"
    echo "  repair    — fix perms, reinstall features from state/features.json (bootstraps minimal state if missing)"
    echo "  reconfigure-features — change Piper/npm/translation (only supported way to add/remove optional features)"
    echo "  refresh-features — rescan Piper models + Argos packs into features.json (after adding voices or translation packages)"
    exit 0
    ;;
  *) die "unknown command: ${1:-}; try --help" ;;
esac

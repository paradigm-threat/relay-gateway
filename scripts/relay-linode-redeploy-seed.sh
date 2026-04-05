#!/usr/bin/env bash
# Redeploy relay-gateway from a local Linode tarball over SSH, then create a bare repo slot
# (POST /api/git/ensure-bare) and optionally git push from a local clone (git:// only).
#
# Prerequisites: SSH key auth (default user **ari** with sudo; override RELAY_SSH_USER); local tarball.
#
# Usage:
#   ./scripts/relay-linode-redeploy-seed.sh 192.0.2.1
#   ./scripts/relay-linode-redeploy-seed.sh 192.0.2.1 /path/to/relay-linode-deploy.tgz
#
# Optional env:
#   RELAY_SSH_USER     SSH login (default: **ari**). Use **root** only on images that allow root SSH.
#   RELAY_SSH_IDENTITY SSH private key (default: ~/.ssh/id_ed25519_relay_gateway)
#   RELAY_SEED_REPO    bare name without .git (default: paradigm-threat-files)
#   RELAY_LOCAL_REPO   if set, cd here and git push to git://NODE:9418/$RELAY_SEED_REPO.git
#   RELAY_SEED_BRANCH  branch to push (default: master)
#   RELAY_REDEPLOY_SKIP_SEED=1  skip POST ensure-bare and git push (binary update only; use for production masters)
set -euo pipefail

NODE="${1:?Usage: $0 <node-ip-or-hostname> [relay-linode-deploy.tgz]}"
TGZ="${2:-$(cd "$(dirname "$0")/.." && pwd)/target/relay-linode-deploy.tgz}"
KEY="${RELAY_SSH_IDENTITY:-$HOME/.ssh/id_ed25519_relay_gateway}"
SSH_USER="${RELAY_SSH_USER:-ari}"
REPO="${RELAY_SEED_REPO:-paradigm-threat-files}"
BRANCH="${RELAY_SEED_BRANCH:-master}"
SSH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes)
SCP=(scp -i "$KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes)
TARGET="${SSH_USER}@${NODE}"

[[ -f "$TGZ" ]] || { echo "Missing tarball: $TGZ (run ./scripts/pack-linode-deploy.sh)" >&2; exit 1; }
[[ -f "$KEY" ]] || { echo "Missing SSH key: $KEY" >&2; exit 1; }

echo "[relay-redeploy] Uploading tarball (user=${SSH_USER})..."
if [[ "$SSH_USER" == "root" ]]; then
  "${SCP[@]}" "$TGZ" "${TARGET}:/root/relay-linode-deploy.tgz"
else
  "${SCP[@]}" "$TGZ" "${TARGET}:/tmp/relay-linode-deploy.tgz"
fi

echo "[relay-redeploy] Running install.sh update on node..."
if [[ "$SSH_USER" == "root" ]]; then
  "${SSH[@]}" "$TARGET" bash -s <<'REMOTE'
set -euo pipefail
cd /root
rm -rf .relay-deploy-staging
mkdir .relay-deploy-staging
cd .relay-deploy-staging
tar xzf ../relay-linode-deploy.tgz
export RELAY_BIN_SOURCE=/root/.relay-deploy-staging
export RELAY_INSTALL_NONINTERACTIVE=1
./install.sh update
cd /root
rm -rf .relay-deploy-staging
REMOTE
else
  "${SSH[@]}" "$TARGET" sudo bash -s <<'REMOTE'
set -euo pipefail
install -d -m 700 /root
cp -f /tmp/relay-linode-deploy.tgz /root/relay-linode-deploy.tgz
cd /root
rm -rf .relay-deploy-staging
mkdir .relay-deploy-staging
cd .relay-deploy-staging
tar xzf ../relay-linode-deploy.tgz
export RELAY_BIN_SOURCE=/root/.relay-deploy-staging
export RELAY_INSTALL_NONINTERACTIVE=1
./install.sh update
cd /root
rm -rf .relay-deploy-staging
rm -f /tmp/relay-linode-deploy.tgz
REMOTE
fi

echo "[relay-redeploy] Waiting for HTTP..."
ok=0
for _ in $(seq 1 20); do
  if curl -fsS --connect-timeout 3 -X POST "http://${NODE}:8080/api/config" >/dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 2
done
[[ "$ok" == 1 ]] || { echo "HTTP did not come back on :8080" >&2; exit 1; }

if [[ "${RELAY_REDEPLOY_SKIP_SEED:-0}" == "1" ]]; then
  echo "[relay-redeploy] RELAY_REDEPLOY_SKIP_SEED=1 — skipping ensure-bare and git push"
  exit 0
fi

echo "[relay-redeploy] POST /api/git/ensure-bare/${REPO}"
curl -fsS -X POST "http://${NODE}:8080/api/git/ensure-bare/${REPO}"
echo

if [[ -n "${RELAY_LOCAL_REPO:-}" ]]; then
  echo "[relay-redeploy] Pushing ${BRANCH} from ${RELAY_LOCAL_REPO} via git://"
  git -C "$RELAY_LOCAL_REPO" push -u "git://${NODE}:9418/${REPO}.git" "$BRANCH"
else
  echo "[relay-redeploy] Done. To push from a clone:"
  echo "  RELAY_LOCAL_REPO=/path/to/repo $0 ...   # re-run with env set"
  echo "  or: git push -u git://${NODE}:9418/${REPO}.git ${BRANCH}"
fi

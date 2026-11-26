# Operator runbook: bare repo reset and catalog recovery

**Audience:** administrators with **SSH** and **sudo** on the relay master (not end users). Relay does not expose an authenticated HTTP API to wipe or reclone bare mirrors; recovery is **on-box** as the **`relay`** user (or **`sudo -u relay`**).

**Related:** [DEPLOY_LINODE.md](./DEPLOY_LINODE.md) (DNS, stale bare dirs, ensure-bare), [RELAY_FLEET_CONSOLE.md](./RELAY_FLEET_CONSOLE.md) (**`relay-fleet.sh reclone-repo`** / fleet status), [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md) (catalog updates), [RELAY_MASTER_DEPLOY.md](./RELAY_MASTER_DEPLOY.md) (after relay-master history reset), [DEPLOY_WEB_AND_CI.md](./DEPLOY_WEB_AND_CI.md) (pushes use **`git://`** on **`RELAY_GIT_PORT`**, default **9418**).

## Ownership: bare repos must be `relay:relay`

Gateway, `relay-git-daemon`, and `relay-git-proxy` run as **`User=relay`**. If any `/opt/relay/data/*.git` tree is owned by **`ubuntu`** (or has stray non-`relay` files), index warm fails with Git **dubious ownership** (HTTP 409) and peers may not fetch. **Do not** add `git config --global safe.directory` — that only silences the check; the service still needs to write as `relay`.

```bash
# From laptop (Admin inventory):
./scripts/relay-fleet.sh --only=NODE.relaygateway.net fix-repo-ownership
# Or UI: Fix ownership / brain scenario repo_ownership_drift (auto with RELAY_BRAIN_EXECUTE=1)

# On the node:
sudo RELAY_REPO_PATH=/opt/relay/data /opt/relay/… # or from checkout:
RELAY_REPO_PATH=/opt/relay/data ./scripts/relay-assert-data-ownership.sh --fix
```

Never point app checkouts (e.g. QuizGate `/var/quizgate/users`) `origin` at the filesystem path of a Relay bare; publish only over **`git://127.0.0.1:9418/<repo>.git`**. Never `chown -R ubuntu:ubuntu /opt/relay/data/…`.

## Why `sync-relay-catalog.sh` does not “reclone” by itself

[`relay-master/scripts/sync-relay-catalog.sh`](../../relay-master/scripts/sync-relay-catalog.sh) runs **`git clone --bare`** only when **`$RELAY_REPO_PATH/<name>.git`** is **missing**. If the directory exists, it **fetch**es only. To force a fresh mirror from the forge URL in the catalog, **remove the bare directory first**, then run the sync script again.

## Full reclone of one catalog mirror (same forge URL)

Use the fleet CLI from **`relay-gateway/`**:

```bash
./scripts/relay-fleet.sh --only=NODE.relaygateway.net reclone-repo REPO
# If the forge is unavailable/private but another node has a healthy mirror:
./scripts/relay-fleet.sh --only=NODE.relaygateway.net \
  reclone-repo REPO --peer=HEALTHY-PEER.relaygateway.net
```

The command reads the repo URL, trusted root, and branch from the node’s currently
served **`$RELAY_REPO_PATH/relay-master.git`**. It prepares and validates a staging
clone while the existing mirror remains online, then briefly stops
**`relay-git-proxy`** and **`relay-git-daemon`** for a same-filesystem swap. A failed clone, peer fetch, or
trust-root check leaves the existing mirror untouched. It does not depend on an
optional **`/opt/relay/src/relay-master`** worktree.

Confirm with **`QUERY`** against the affected repo and
**`POST /api/config`** on the node.

## Changing upstream (different forge URL or new root)

Policy is defined in **relay-master** in two places that must stay aligned: **`relay-catalog.json`** (read by **`relay-gateway`** at startup) and **`.gitmodules`** (used by **`sync-relay-catalog.sh`** for clone/fetch). Update **`url`**, **`relayRootCommit`**, and **`branch`** in both, merge to your default branch, push to the forge, then on **each** affected master run the same **`git pull` + `sync-relay-catalog.sh`** block as in [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md) § “Updating a catalog node”, then **`systemctl restart relay-gateway`**.

See also [relay-master/README.md](../../relay-master/README.md) for submodule helpers.

## When `POST /api/config` shows `catalog_forge_sync_ok: false`

Inspect **`startup_catalog_sync.catalog_output_tail`**. Common cases:

- **`Could not resolve host: gitlab.com`** (or another forge host): the VM **resolver** is broken or blocked. Fix host DNS (**`/etc/resolv.conf`**, **`systemd-resolved`**, or Linode networking) until **`sudo -u relay getent hosts gitlab.com`** succeeds, then **`sudo systemctl restart relay-gateway`** so **`ExecStartPre`** runs **`sync-relay-catalog.sh`** again, or run the manual **`git pull` + sync** block above.
- **`could not read Username for 'https://…'`**: the **`relay`** user needs non-interactive read access to private HTTPS URLs (token, deploy key, or public stub repo). See [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md). Long-term fix: **`~/.git-credentials`** or **`url.*.insteadOf`** for **`relay`**, or make stub repos **public** read on the forge.

- **Workaround (mesh peers):** **`relay-startup-catalog-sync.sh`** skips the peer **`git fetch`** round when **`sync-relay-catalog.sh`** fails first. If another master already has good bare mirrors, refresh this node from the peer over **`git://`** (no GitLab password). Example for one repo (**`PEER`** = e.g. **`atlanta3.relaygateway.net`**, **`GIT_PORT`** usually **9418**):

  ```bash
  sudo -u relay git --git-dir=/opt/relay/data/REPO.git fetch "git://${PEER}:${GIT_PORT}/REPO.git" '+refs/heads/*:refs/heads/*' --prune
  ```

  Repeat for each catalog **`REPO`**. **`POST /api/config` → `catalog_forge_sync_ok`** may remain **`false`** until forge HTTPS works again; **`git ls-remote git://this-node…`** can still succeed after a successful peer fetch.

**Note:** **`git push`** to relay uses **`git://this-node:9418/<repo>.git`**, not SSH. If **`git ls-remote git://…`** works but catalog sync fails, the git daemon is fine; fix forge reachability from the **same** host as **`sync-relay-catalog.sh`**.

## `POST /api/git/ensure-bare/:repo` is not an admin-only reset

**`ensure-bare`** only creates an **empty** bare repo when the path is missing; it is **not** HTTP-authenticated. Restrict it at the network layer if the route is reachable, or set **`RELAY_GIT_ENSURE_BARE_ENABLED=0`** in **`relay.env`** and create bare repos only via SSH. See [README.md](../README.md) and `post_ensure_bare` in the gateway source. **Deleting** or **recloning** mirrors remains an **SSH + filesystem** operation.

## Removing catalog repos

Applying a **relay-master catalog tip change** removes the corresponding bare
mirror when a repo entry is deleted. Running **`sync-relay-catalog.sh`** by itself
only syncs entries that still exist and does not discover stale, unlisted bare
directories. Prefer catalog tip application for removals; inspect and remove
legacy stale directories manually only when they predate that mechanism.

## After a relay-master force-push or history rewrite

Follow [RELAY_MASTER_DEPLOY.md](./RELAY_MASTER_DEPLOY.md): **`git fetch`**, **`git reset --hard origin/main`** (or equivalent) in the catalog worktree on each node, then **`sync-relay-catalog.sh`** and service restarts.

---

## SSH: deploy key, multiplexing (“auto SSH”), and root hardening

Use this when **`ari@…` public key auth fails** or you only have a **one-time root password** (e.g. new Linode). **Do not commit passwords or private keys to git.**

### 1. Regain a shell (Linode / cloud console)

If **`ssh root@<FQDN>`** with the dashboard password fails, use **LISH / Glish / serial console** or **Reset root password** in the provider UI, then try SSH again. Prefer **`ssh root@jp-osa.relaygateway.net`** (or your node’s **FQDN**), not a copied **IPv4** from docs or old screenshots—IPs drift; DNS should track the live Linode.

### 2. Install the operator deploy key for `ari` (once, as root on the box)

On your laptop the normative key is **`~/.ssh/id_ed25519_relay_gateway`** (see [`.cursor/rules/relay-production-ops.mdc`](../.cursor/rules/relay-production-ops.mdc)). On the server as **root**:

```bash
install -d -m 755 /home/ari
install -d -m 700 /home/ari/.ssh
# Append ONE line: the contents of id_ed25519_relay_gateway.pub from the operator machine
printf '%s\n' 'ssh-ed25519 AAAA…your_pubkey… comment' >> /home/ari/.ssh/authorized_keys
chmod 600 /home/ari/.ssh/authorized_keys
chown -R ari:ari /home/ari/.ssh
getent group sudo >/dev/null && usermod -aG sudo ari || true
```

Verify from the operator machine: **`ssh -o BatchMode=yes -o IdentitiesOnly=yes -i ~/.ssh/id_ed25519_relay_gateway ari@SERVER 'echo ok'`**.

### 3. Multiplexing and long-lived forwards (operator laptop)

**Connection multiplexing** (faster repeat `ssh`/`scp`, fewer handshakes): in **`~/.ssh/config`**, for the host use **`ControlMaster auto`**, **`ControlPath ~/.ssh/cm-%r@%h:%p`**, **`ControlPersist 10m`**, plus **`ServerAliveInterval` / `ServerAliveCountMax`** (see production ops rule).

**`autossh`** for a stable **local forward** to relay HTTP (e.g. debug **`127.0.0.1:8080`**): see [`.cursor/rules/relay-production-ops.mdc`](../.cursor/rules/relay-production-ops.mdc) § “autossh vs plain ssh”.

### 4. Full relay deploy after SSH works (`ari` + sudo)

Fix **forge DNS** for user **`relay`** if **`catalog_forge_sync_ok`** is false (e.g. **`getent hosts gitlab.com`** as **`relay`**). Then catalog sync and optional binary update:

```bash
sudo -u relay bash -lc 'set -e; cd /opt/relay/src/relay-master && git pull origin main && RELAY_REPO_PATH=/opt/relay/data ./scripts/sync-relay-catalog.sh'
sudo systemctl restart relay-gateway relay-git-daemon relay-git-proxy
# Optional: new tarball from a trusted build — see DEPLOY_LINODE.md / production-ops (install.sh update).
```

### 5. Rotate root password and stop using a shared root password

After **`ari` + sudo + deploy key** works:

1. Set a **new strong root password** known only to you: **`sudo passwd root`** (or **`passwd`** as root in console). **Do not store it in the repo.**
2. Prefer **`PermitRootLogin prohibit-password`** or **`no`** in **`sshd`**, and rely on **`sudo`** from **`ari`**. Test **second** SSH session before closing the first.
3. Optional: **`sudo passwd -l root`** only if your recovery path does not require root password login (Linode LISH can still reset via control panel).

**Order of operations:** verify **`ssh ari@…`** (key) → run deploy/sync → then tighten **`sshd`** / root.

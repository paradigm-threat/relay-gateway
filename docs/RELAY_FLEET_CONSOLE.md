# Relay fleet console (`relay-fleet.sh`)

**Canonical operator tool** for deployment, health checks, catalog drift, and remote admin across Linode masters. Prefer this over ad hoc `ssh` / `curl` fragments. **Non-interactive by default** (no stdin prompts); use **`--interactive`** only when you need an open TTY.

**Prerequisites:** run from **`relay-gateway/`** with a local **`relay.env`** (gitignored) or **`--env-file`**. SSH key **`RELAY_SSH_IDENTITY`** (default **`~/.ssh/id_ed25519_relay_gateway`**) must authenticate as **`RELAY_SSH_USER`** (default **`ari`**) on each host.

## Fleet list (operator laptop)

| Variable | Meaning |
|----------|---------|
| **`RELAY_OPERATOR_FLEET`** | Semicolon-separated **FQDNs** (quote for `source relay.env`). Primary list for this script. |
| **`RELAY_MASTER_PEER_LIST`** | If **`RELAY_OPERATOR_FLEET`** is unset, the script falls back to this (same format). On servers, this is the mesh contract — see **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**. |

**Do not** pin stale IPv4s in docs; use hostnames. Temporary SSH-by-IP is an exception for broken DNS only.

## Global flags

| Flag | Meaning |
|------|---------|
| **`--env-file=PATH`** | Load env from PATH instead of **`./relay.env`**. |
| **`--only=FQDN`** | Restrict commands to one fleet member. **Required** for destructive **`reinstall`** wipe. |
| **`--build`** | Before **`update`** / **`deploy-all`**: run **`pack-linode-deploy.sh`**. Also runs if **`target/relay-linode-deploy.tgz`** is missing. |
| **`--interactive`** | Omit **`ssh -o BatchMode=yes`** (allows password prompts — rarely needed). |

Destructive **`reinstall`** additionally requires **`--danger-wipe-relay-data`** and **`--confirm-wipe=<exact-fqdn>`** matching the single **`--only`** host.

## Subcommands

| Command | Description |
|---------|-------------|
| **`hosts`** | Print resolved fleet, SSH user, identity path. |
| **`ssh-check`** | Parallel **`ssh … true`**; fails if any host fails. |
| **`status`** | Parallel **`POST https://<host>/api/config`**; prints **`relay_server_id`**, **`catalog_forge_sync_ok`**, **`authorized_repos`** summary. |
| **`drift`** | **`git -C $RELAY_MASTER_CATALOG_PATH rev-parse HEAD`** per host (default path **`/opt/relay/src/relay-master`**); flags mismatches. |
| **`update`** | **`relay-linode-redeploy-seed.sh`** per host with **`RELAY_REDEPLOY_SKIP_SEED=1`**, then **`systemctl restart relay-gateway relay-git-daemon`**. |
| **`deploy-all`** | Same as **`update`** ( **`deploy-all-masters.sh`** is a thin **`--build`** wrapper). |
| **`repair`** | Uploads **`target/relay-linode-deploy.tgz`**, extracts under **`/tmp`**, runs **`./install.sh repair`** (same pattern as **`relay-linode-redeploy-seed`** — **`install.sh` is not kept under `/opt/relay/bin`** on nodes). |
| **`features-set`** | Same tarball staging, then **`./install.sh reconfigure-features`** with **`RELAY_INSTALL_NONINTERACTIVE=1`** and **`RELAY_FEAT_*`** from flags or your environment: **`--piper=0|1`**, **`--npm-pkgs=`**, **`--translation=0|1`**, **`--translation-pkgs=`**, **`--libretranslate=0|1`**. |
| **`reclone-repo REPO`** | Stops **`relay-git-daemon`**, removes **`$RELAY_REPO_PATH/$REPO.git`**, **`git pull` + `sync-relay-catalog.sh`**, optional **`--peer=FQDN`** **`git fetch`** from **`git://`**, restarts daemons. See **[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)**. |
| **`bootstrap-ssh`** | One-shot **`root`** password (**`SSHPASS`** or **`RELAY_ROOT_PASSWORD`** env — never commit) + **`sshpass`** to append deploy **`*.pub`** to **`ari`’s `authorized_keys`**, then **`ssh-check`**. Not the same as laptop **`autossh`** tunnels — see production-ops rule. |
| **`reinstall`** | **Safe default:** same as **`update`**. **Destructive:** with **`--danger-wipe-relay-data`**, **`--confirm-wipe=`**, and **`--only=`** exactly one host — stops units and **`rm -rf $RELAY_REPO_PATH/*.git`**, then **`update`**. |

## Tunables

| Env | Default | Meaning |
|-----|---------|---------|
| **`RELAY_FLEET_CURL_TIMEOUT`** | 25 | Seconds for **`status`** HTTPS. |
| **`RELAY_FLEET_MAX_PARALLEL`** | 4 | Parallel **`ssh-check`** / **`status`** jobs. |
| **`RELAY_FLEET_STRICT_HOST_KEY`** | accept-new | Passed to **`ssh` / `scp`**. |

## Examples

```bash
cd relay-gateway
./scripts/relay-fleet.sh hosts
./scripts/relay-fleet.sh ssh-check
./scripts/relay-fleet.sh status --only=atlanta3.relaygateway.net
./scripts/relay-fleet.sh drift
./scripts/relay-fleet.sh deploy-all --build
./scripts/relay-fleet.sh features-set --only=atlanta3.relaygateway.net --piper=0 --npm-pkgs=
./scripts/relay-fleet.sh reclone-repo paradigm-threat-files --peer=atlanta3.relaygateway.net --only=jp-osa.relaygateway.net
```

## Related

- **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** — full install / DNS / secrets.
- **[`.cursor/rules/relay-production-ops.mdc`](../.cursor/rules/relay-production-ops.mdc)** — SSH, autossh, tarball flow.
- **[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)** — catalog sync, peer fetch, **`ensure-bare`**.

## Follow-ups (mesh health)

1. **GitLab read-only credential** for user **`relay`** on masters for **`https://gitlab.com/clevertree/*`** so **`sync-relay-catalog.sh`** succeeds and **`catalog_forge_sync_ok`** is true (**[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**).
2. **`songwalker-library`:** ensure forge or a peer hosts the bare repo so all masters can sync or peer-fetch (**[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)**).

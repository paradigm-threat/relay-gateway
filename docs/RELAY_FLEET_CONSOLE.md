# Relay fleet console

**Canonical operator tool:** [`relay-ops`](./RELAY_OPS.md) (`cargo run --bin relay-ops`). CLI + local UI. Topology comes from the GitLab project URL → [`ops/fleet.json`](../ops/fleet.json). `./scripts/relay-fleet.sh` is a thin wrapper around the same binary.

**First-time master (provision + install):** use **[`RELAY_INFRA_OPS.md`](./RELAY_INFRA_OPS.md)** and **`./scripts/relay-master-deploy.sh`** (Terraform + tarball + Caddy). **Ongoing** binary/config deploys: **`relay-ops --build update`** / **`deploy-all`** below.

**Local park SSR / tip hardening (before fleet push):** **[`PARADIGM_PARK_LOCAL.md`](./PARADIGM_PARK_LOCAL.md)** · **`./scripts/park-local.sh`**. Prefer that loop over rapid `deploy-all` when iterating CleverScript tip or park Host SSR.

**Prerequisites:** run from **`relay-gateway/`**. Observer commands need only a GitLab URL (or a git remote on this checkout). Admin commands need SSH (`RELAY_SSH_IDENTITY`, default **`~/.ssh/id_ed25519_relay_gateway`**) as the per-node `sshUser` in `ops/fleet.json` (typically **`ubuntu`**). Optional **`relay.env`** / **`--env-file`** still supplies Vercel/Spot tokens.

The tables below keep the historical `relay-fleet.sh` flag names; they are now clap flags on **`relay-ops`**.

## Fleet list (operator laptop)

| Variable | Meaning |
|----------|---------|
| **`RELAY_OPERATOR_FLEET`** | Semicolon-separated **FQDNs** (quote for `source relay.env`). Primary list for this script. |
| **`RELAY_MASTER_PEER_LIST`** | If **`RELAY_OPERATOR_FLEET`** is unset, the script falls back to this (same format). On servers, this is the mesh contract — see **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**. |

**Do not** pin stale IPv4s in docs; use hostnames. Temporary SSH-by-IP is an exception for broken DNS only.

**Network ingress / parked domains:** node FQDNs are the source of truth; `fleet.relaygateway.net` is the stable network target materialized from their current addresses. Parked apex domains use ALIAS/flattening to the fleet name, and parked subdomains use CNAME. See **[`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md)**. Reconcile the fleet A/AAAA set with **`./scripts/relay-fleet.sh fleet-dns`** (also runs from **`relay-master-deploy.sh`** after node DNS upsert). Coordinated multi-node TLS is not yet a fleet subcommand.

**Local git remotes (developer laptop):** add **`git://dfw1|lon1|hkg1.relaygateway.net:9418/<repo>.git`** to every checkout listed in **`relay-master/catalog/repos/`**:

```bash
./scripts/relay-local-remotes.sh --all   # ensure-bare + fleet remotes + push defaults + probe
git push                                 # default → dfw1 (first mesh origin); mesh propagates via notify
git push gitlab main                     # optional GitLab backup (origin renamed to gitlab)
```

Edit paths in **`scripts/relay-local-repos.map`**. Legacy **atlanta2** may still use old bare names (e.g. **`snesology.git`**); catalog name **`snesology-library`** is what Spot masters use.

## Global flags

| Flag | Meaning |
|------|---------|
| **`--env-file=PATH`** | Load env from PATH instead of **`./relay.env`**. |
| **`--only=FQDN`** | Restrict commands to one fleet member. **Required** for destructive **`reinstall`** wipe. |
| **`--build`** | Before **`update`** / **`deploy-all`**: run **`pack-linode-deploy.sh`** ( **`cargo build --release`** for **`relay-gateway`** + **`relay-hook-handler`** ), then recreate **`target/relay-linode-deploy.tgz`**. Also runs if that tarball is **missing**. |
| **`--interactive`** | Omit **`ssh -o BatchMode=yes`** (allows password prompts — rarely needed). |

**`--build` placement (read this before production deploys):** Global flags are parsed **before** the subcommand. The safe, canonical forms are **`./scripts/relay-fleet.sh --build update`** and **`./scripts/relay-fleet.sh --build deploy-all`** (optionally with **`--only=…`** before the subcommand). For **`update`** and **`deploy-all` only**, a **trailing** **`--build`** is also accepted (**`./scripts/relay-fleet.sh deploy-all --build`**) so a mistaken ordering still rebuilds — do not rely on that for other subcommands.

**Stale tarball hazard:** If **`target/relay-linode-deploy.tgz`** already exists and you run **`update`** / **`deploy-all`** **without** triggering a pack (**`--build`** or delete the tarball), the fleet uploads **whatever is on disk** — including **weeks-old** binaries. After changing **`relay-gateway`** Rust code, always use **`--build`**, or run **`./scripts/pack-linode-deploy.sh`** manually first, then **`ls -la target/relay-linode-deploy.tgz`** to confirm the mtime.

**`reinstall` (destructive):** **`--danger-wipe-relay-data`**, **`--confirm-wipe=<exact-fqdn>`**, and **`--only=<matching-fqdn>`** are required; see **`reinstall`** in the Subcommands table.

## Subcommands

| Command | Description |
|---------|-------------|
| **`hosts`** | Print resolved fleet, SSH user, identity path. |
| **`ssh-check`** | Parallel **`ssh … true`**; fails if any host fails. |
| **`status`** | Parallel **`POST https://<host>/api/config`**; prints **`relay_server_id`**, **`catalog_forge_sync_ok`**, **`authorized_repos`** summary. |
| **`health`** | Per-host HTTP **QUERY** smoke via **`scripts/validate-relay-query.mjs`** (default repos **`paradigm-threat-files`**, **`paradigm-threat-timeline`**, query **`venus`**). Optional **`--mcp`** also runs **`validate-relay-mcp.mjs`**. Override with **`RELAY_FLEET_HEALTH_REPOS`**, **`RELAY_FLEET_HEALTH_QUERY`**. Requires local **`node`**. |
| **`drift`** | **`git -C $RELAY_MASTER_CATALOG_PATH rev-parse HEAD`** per host (default path **`/opt/relay/src/relay-master`**); flags mismatches. |
| **`update`** | Per host: **`relay-linode-redeploy-seed.sh`**, restart **`relay-gateway`** + **`relay-git-daemon`**, then **post-update health gate** (`POST /api/config`) unless **`RELAY_FLEET_UPDATE_HEALTH=0`**. Prefer canary order lon1 → hkg1 → dfw1 (`--only=`). See **[`RELAY_MESH_OPS.md`](./RELAY_MESH_OPS.md)**. |
| **`deploy-all`** | Same as **`update`** (**`relay-linode-redeploy-seed.sh`** …). **`./scripts/deploy-all-masters.sh`** wraps **`relay-fleet.sh --build deploy-all`** (always repacks — use **`relay-fleet.sh deploy-all`** alone only if you intend to reuse an existing tarball). |
| **`repair`** | Uploads **`target/relay-linode-deploy.tgz`**, extracts under **`/tmp`**, runs **`./install.sh repair`** (same pattern as **`relay-linode-redeploy-seed`** — **`install.sh` is not kept under `/opt/relay/bin`** on nodes). |
| **`features-set`** | Same tarball staging, then **`./install.sh reconfigure-features`** with **`RELAY_INSTALL_NONINTERACTIVE=1`** and **`RELAY_FEAT_*`** from flags or your environment: **`--piper=0|1`**, **`--npm-pkgs=`**, **`--translation=0|1`**, **`--translation-pkgs=`**, **`--libretranslate=0|1`**. |
| **`fleet-dns`** | Resolve configured fleet FQDNs and reconcile **`fleet.relaygateway.net`** A/AAAA via Vercel DNS (**`--dry-run`**, **`--fleet=`**). Ignores **`--only`**. Requires **`VERCEL_API_TOKEN`**. |
| **`reclone-repo REPO`** | Reads the repo URL/root/branch from the node’s served **`relay-master.git`**, clones and validates a staging bare (optional **`--peer=FQDN`** fetch), then briefly stops **`relay-git-daemon`** for an atomic swap. The existing mirror remains live and is preserved if preparation fails; no catalog worktree is required. See **[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)**. |
| **`bootstrap-ssh`** | One-shot **`root`** password (**`SSHPASS`** or **`RELAY_ROOT_PASSWORD`** env — never commit) + **`sshpass`** to append deploy **`*.pub`** to **`ari`’s `authorized_keys`**, then **`ssh-check`**. Not the same as laptop **`autossh`** tunnels — see production-ops rule. |
| **`reinstall`** | **Safe default:** same as **`update`**. **Destructive:** with **`--danger-wipe-relay-data`**, **`--confirm-wipe=`**, and **`--only=`** exactly one host — stops units and **`rm -rf $RELAY_REPO_PATH/*.git`**, then **`update`**. |

## Tunables

| Env | Default | Meaning |
|-----|---------|---------|
| **`RELAY_FLEET_CURL_TIMEOUT`** | 25 | Seconds for **`status`** HTTPS. |
| **`RELAY_FLEET_MAX_PARALLEL`** | 4 | Parallel **`ssh-check`** / **`status`** jobs. |
| **`RELAY_FLEET_STRICT_HOST_KEY`** | accept-new | Passed to **`ssh` / `scp`**. |
| **`RELAY_FLEET_UPDATE_HEALTH`** | `1` | After each host update+restart, require **`POST /api/config`** before next host (`0` to skip). |
| **`RELAY_FLEET_SSL_GATE`** | `0` | With **`update` / `deploy-all`**: when `1`, pre/post **`ssl`** failures abort deploy (default: report WARN and continue). |
| **`RELAY_FLEET_SSL_DOMAINS`** | park Hosts | Space-separated public Hosts checked by **`ssl`** / health (default includes `relay.paradigmthreat.net` + clevertree). |
| **`RELAY_FLEET_HEALTH_SSL`** | `1` | When `0`, **`health`** skips the park/node HTTPS report. |
| **`RELAY_FLEET_SSL_STRICT`** | `0` | Treat SSL WARN lines as FAIL inside **`relay-fleet-ssl-check.sh`**. |

## Examples

```bash
cd relay-gateway
./scripts/relay-fleet.sh hosts
./scripts/relay-fleet.sh ssh-check
./scripts/relay-fleet.sh --only=atlanta3.relaygateway.net status
./scripts/relay-fleet.sh ssl          # parked Host + node HTTPS report
./scripts/relay-fleet.sh health       # QUERY smoke + SSL report
./scripts/relay-fleet.sh health --mcp
./scripts/relay-fleet.sh drift
# Rebuild tarball + push to whole fleet (preferred flag order):
./scripts/relay-fleet.sh --build deploy-all
# Hard-fail deploy on broken park HTTPS:
RELAY_FLEET_SSL_GATE=1 ./scripts/relay-fleet.sh --build deploy-all
# One node only:
./scripts/relay-fleet.sh --only=atlanta3.relaygateway.net --build update
./scripts/relay-fleet.sh --only=atlanta3.relaygateway.net features-set --piper=0 --npm-pkgs=
./scripts/relay-fleet.sh --only=jp-osa.relaygateway.net reclone-repo paradigm-threat-files --peer=atlanta3.relaygateway.net
```

## Mesh tip-notify (`git.origins`)

Relay mesh is **relay↔relay only** (no GitLab integration in the product path). Per-repo **`.relay.yaml`** can declare an ordered ring:

```yaml
git:
  notifyMode: next   # next (default) | all
  origins:
    - name: dfw1
      kind: relay
      host: dfw1.relaygateway.net
      fetch: true
      notify: true
    - name: lon1
      kind: relay
      host: lon1.relaygateway.net
      notify: true
```

| Field | Meaning |
|-------|---------|
| **`notifyMode: next`** | After validated **`post-receive`**, notify **one** next relay in the ring (wrap). Peer-fetch hop-forwards with **`via`** / **`hops_remaining`** loop guard. |
| **`notifyMode: all`** | Fan-out tip-notify to every eligible origin (excluding self). |
| **`kind: relay`** | Only relay peers participate in mesh notify. |
| **`host`** | Must appear in server **`RELAY_MASTER_PEER_LIST`** when that list is non-empty. |
| **`notify: false`** | Exclude from notify ring (still allow fetch if configured elsewhere). |

If **`git.origins`** omits relays, notify targets fall back to **`RELAY_MASTER_PEER_LIST`** (minus self). Legacy **`git.autoPush.originList`** remains optional push shorthand to **`git://`** peers.

**Index warm:** peer-fetch and catalog apply spawn async **`ensure_indexed`**; operators can also **`POST /api/index/warm/:repo`** (default branch **`main`**).

**Catalog rescue:** signed **`relay-master`** tip → rm/reclone bare mirrors — **[`RELAY_CATALOG_RESCUE.md`](./RELAY_CATALOG_RESCUE.md)**.

## Pre-deploy checklist

From **`relay-gateway/`** on the operator laptop:

```bash
./scripts/pre-deploy-check.sh          # cargo test + release build + pack + tarball verify
./scripts/relay-fleet.sh ssh-check     # SSH to fleet
./scripts/relay-fleet.sh ssl           # baseline HTTPS (or rely on deploy pre-check)
./scripts/relay-fleet.sh --build deploy-all   # runs SSL pre + post (warn by default)
./scripts/relay-fleet.sh status
./scripts/relay-fleet.sh health        # QUERY + SSL
./scripts/relay-fleet.sh drift         # relay-master tips aligned
```

After deploy, smoke mesh tip-notify: signed push to a repo with **`git.origins`** configured; confirm peer **`journalctl -u relay-gateway`** shows `peer notify delivered` on the next hop. Catalog apply: signed **`relay-master`** push with a catalog entry change; confirm Slack audit and bare mirror update on receiving master.

## Related

- **[`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md)** — node names, `fleet.relaygateway.net`, parked-domain ALIAS/CNAME strategy, Spot replacement, TLS gate, and rollback.
- **[`RELAY_BRANCH_POLICY_OPERATOR.md`](./RELAY_BRANCH_POLICY_OPERATOR.md)** — `.relay.yaml` **pre-receive** policy, why forge vs fleet differ, **`relay-branch-policy-check`** validation.
- **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** — full install / DNS / secrets.
- **[`.cursor/rules/relay-production-ops.mdc`](../.cursor/rules/relay-production-ops.mdc)** — SSH, autossh, tarball flow.
- **[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)** — catalog sync, peer fetch, **`ensure-bare`**.
- **[`RELAY_CATALOG_RESCUE.md`](./RELAY_CATALOG_RESCUE.md)** — lost admin / mirror replace via signed **`relay-master`** tip.

## Follow-ups (mesh health)

1. **GitLab read-only credential** for user **`relay`** on masters for **`https://gitlab.com/clevertree/*`** so **`sync-relay-catalog.sh`** succeeds and **`catalog_forge_sync_ok`** is true (**[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**).
2. **`songwalker-library`:** ensure forge or a peer hosts the bare repo so all masters can sync or peer-fetch (**[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)**).

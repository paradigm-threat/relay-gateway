# Deploy relay-gateway to Linode (bare metal — no Docker)

**Repo replication (normative):** Content pushed **into** relay’s bare repos MUST use the **git protocol** — **`git://HOST:9418/{repo}.git`** (or **`RELAY_GIT_PORT`**). Do **not** use **`git@host:…`** SSH remotes for relay sync; SSH is for **VM admin** only. Optional rules live in per-repo **`.relay.yaml`** / hooks.

**See also:** **[`DEPLOY_SELF_HOSTED.md`](./DEPLOY_SELF_HOSTED.md)** (build tarball, optional vendored binaries, no cloud CI), **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** — static sites (pre-built HTML/Next export), reverse proxy, off-box builds, multi-site on one VM. **Secrets / `relay.env` / rotation / deploy SSH key:** **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**. **SSH (non-root, agents):** **[`CURSOR_AGENTIC_SSH.md`](./CURSOR_AGENTIC_SSH.md)** §5. **Helm-style reconcile (relay-master → masters):** **[`RELAY_RECONCILE_PLAN.md`](./RELAY_RECONCILE_PLAN.md)**.

**`relay.env` permissions:** **`install.sh`** sets **`$INSTALL/relay.env`** to **`root:root`** **`chmod 600`**. Only root (installer, **`install.sh update`**, **`repair`**, or **`relay-bootstrap.sh`** run as root) should edit it; the **`relay`** user gets variables from systemd **`EnvironmentFile`**.

**DNS / IPs:** For the current phase, **public IPs can be set manually**; automated DNS (Vercel API or other) remains optional via **`RELAY_SKIP_VERCEL_DNS=1`** or the existing Vercel flow below. **Wildcard** **`*.jp-osa.relaygateway.net`** (repo vhosts) must point at the JP master; the apex **`jp-osa.relaygateway.net`** **A** record must match that host’s **public IPv4** (if the A record is stale, HTTPS to the apex misroutes until Vercel DNS is updated).

## Recommended plan

| Step | What |
|------|------|
| **1. VM** | Linode **Ubuntu 22.04/24.04 LTS**, open **22, 8080, 9418** (and **5590** if Piper). |
| **2. Binaries** | On a trusted machine: **`./scripts/pack-linode-deploy.sh`** → **`target/relay-linode-deploy.tgz`**. Host the tarball on **any** HTTPS you control (GitLab generic package, internal nginx, USB copy + `file:` path not supported by curl — use **`scp`** or **`RELAY_BIN_SOURCE`**). See **[`DEPLOY_SELF_HOSTED.md`](./DEPLOY_SELF_HOSTED.md)**. |
| **3. Install** | **`relay-curl.sh`** one-liner with **`RELAY_DEPLOY_TGZ_URL`**, **or** `scp` the tgz and **`sudo ./install.sh install`**. The installer’s **first step** is **Vercel DNS** (unless skipped): detect public IPv4, upsert **A** record for your **FQDN**, wait until DNS resolves, then install packages and binaries. Uses **`VERCEL_API_TOKEN`** (optional **`VERCEL_TEAM_ID`**) like Docker/K8s. See **[Vercel DNS (first step of `install`)](#vercel-dns-first-step-of-install)**. |
| **4. Trust** | **`relay.env`**: **`RELAY_SERVER_ID`** + catalog (**[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**). Prefer **`RELAY_MASTER_CATALOG_GIT_URL`** + **`RELAY_MASTER_TRUST_ROOT`** so a **new** node needs **no manual git clone** — **`ExecStartPre`** clones **`relay-master`** then runs **`sync-relay-catalog.sh`**. *Or* **`RELAY_AUTHORIZED_REPOS_PATH`** (YAML) — never both catalog path and YAML. |
| **5. Data** | With **`RELAY_MASTER_CATALOG_GIT_URL`**, bare mirrors are created on **first start** (and each restart refreshes). Otherwise: clone **`relay-master`** to **`RELAY_MASTER_CATALOG_PATH`**, run **`sync-relay-catalog.sh`**; optional **`relay-bootstrap.sh`** / **`git push`** as needed. |
| **6. Updates** | **Binary upgrade:** new tgz → **`install.sh update`** or **`relay-curl.sh`** with new **`RELAY_DEPLOY_TGZ_URL`**. **Script/hardening only:** **`relay-curl.sh … repair`** (pulls **`relay-install.sh`** from **`RELAY_RAW_BASE`** / GitLab raw by default). |

**Example hosted tarball URL** (you choose the host):

`https://git.example.com/api/v4/projects/…/packages/generic/relay-gateway/v0.3.10/relay-linode-deploy.tgz`

---

## Minimum launch (required)

1. **`RELAY_SERVER_ID`** — unique name (e.g. `relay-atlanta1`).
2. **Allowlist** — **either** point **`relay-gateway`** at the catalog worktree **or** at YAML (never both). **Policy source of truth:** **[`relay-master`](../../relay-master)** (**`relay-catalog.json`** + **`relayRootCommit`**).

**Preferred — catalog path:**

```bash
sudo -u relay bash -lc 'cd /opt/relay/src/relay-master && git pull origin main && \
  RELAY_REPO_PATH=/opt/relay/data ./scripts/sync-relay-catalog.sh'
sudo systemctl restart relay-gateway
```

**Catalog URLs and shallow clones:** each **`url`** in **`relay-catalog.json`** should use forge repos whose **first commit is minimal** (README-only stub) so **`sync-relay-catalog.sh`** can **`git clone --bare --depth 1`** and keep GitLab/GitHub free of private payloads. Full trees arrive via **`git push git://…:9418/…`** to the node. See **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** § “Public forge URL vs private body”.

Add to **`/opt/relay/relay.env`** (example):

```
RELAY_SERVER_ID=relay-atlanta3
RELAY_MASTER_CATALOG_PATH=/opt/relay/src/relay-master
RELAY_MASTER_TRUST_ROOT=14f88ca11ce88f580c1a3cab1dfa5493eace34e2
RELAY_DEFAULT_REPO=relay-master
```

Use your real root: `git -C /opt/relay/src/relay-master rev-list --max-parents=0 main`.

**Legacy — YAML:**

```bash
sudo -u relay bash -lc 'cd /opt/relay/src/relay-master && git pull origin main && \
  RELAY_REPO_PATH=/opt/relay/data ./scripts/sync-relay-catalog.sh && \
  ./scripts/sync-relay-catalog.sh --write-authorized-repos relay-atlanta1 /opt/relay/authorized-repos.yaml'
```

Then set **`RELAY_AUTHORIZED_REPOS_PATH=/opt/relay/authorized-repos.yaml`** (omit **`RELAY_MASTER_CATALOG_PATH`**).

Catalog checkout needs **`remote.origin`** if you mirror **`relay-master.git`** under **`RELAY_REPO_PATH`**. For a disposable YAML snapshot only: **`sudo cp docs/examples/authorized-repos.yaml …`** — not for production drift.

If **`install.sh`** completed Vercel DNS, it may have appended **`RELAY_PUBLIC_HOSTNAME=your.fqdn`** (non-secret). That value is the **node** hostname; each bare repo is served at **`{repo-name}.{RELAY_PUBLIC_HOSTNAME}`** (HTTP **`Host`** header). Add a **wildcard** DNS record **`*.your.fqdn`** → same server IP so every repo subdomain resolves. Trust settings are still required regardless.

**Startup catalog sync:** **`relay-gateway.service`** (and legacy **`relay-server.service`**) may run **`relay-startup-catalog-sync.sh`** as **`ExecStartPre`** when **`/opt/relay/bin/relay-startup-catalog-sync.sh`** exists. If **`RELAY_MASTER_CATALOG_GIT_URL`** is set and the catalog worktree is missing or empty, it **`git clone`**s into **`RELAY_MASTER_CATALOG_PATH`** (default **`/opt/relay/src/relay-master`** when only the URL is set). Then it runs **`sync-relay-catalog.sh`**, then best-effort **`git fetch`** from other **`RELAY_MASTER_PEER_LIST`** masters over **`git://`**. **HTTP always starts** even if clone/forge sync fails; status is in **`state/startup_catalog_sync.json`** and **`POST /api/config` → `startup_catalog_sync`**. Optional Slack progress: **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`**. Disable with **`RELAY_STARTUP_CATALOG_SYNC=0`**. Private forge: use a token embedded in the HTTPS URL or **`git` credential** for user **`relay`**. See **`scripts/relay-startup-catalog-sync.sh`**, **`docs/SLACK_MESH_NOTIFY.md`**, **`docs/RELAY_TRUST_AND_BOOTSTRAP.md`** (stub README for new repos).

### Greenfield checklist: smooth first boot when `relay.env` is complete

After **`install.sh`** (or an equivalent tarball install) has created **`/opt/relay`**, systemd, and **`relay-gateway.service`**, a **new** node should need **no manual git steps** on the catalog if **`/opt/relay/relay.env`** is filled out consistently with your other masters.

| Area | Set in `relay.env` (or installer) | Notes |
|------|-------------------------------------|--------|
| **HTTP routing** | **`RELAY_PUBLIC_HOSTNAME`**, **`RELAY_SERVER_ID`** | Wildcard DNS **`*.node`** → server IP for **`{repo}.{node}`** hosts. |
| **Catalog clone** | **`RELAY_MASTER_CATALOG_GIT_URL`** (HTTPS), **`RELAY_MASTER_TRUST_ROOT`** (40 hex) | **`RELAY_MASTER_CATALOG_PATH`** defaults to **`/opt/relay/src/relay-master`** when the URL is set. Branch: **`RELAY_MASTER_CATALOG_BRANCH`** (default **`main`**). |
| **Site apex** | **`RELAY_DEFAULT_REPO=relay-master`** | Optional but typical for catalog-as-homepage. |
| **Masters** | **`RELAY_MASTER_PEER_LIST`** | Semicolon-separated FQDNs of **every** master (installer default includes **`atlanta2`**, **`atlanta3`**, **`jp-osa`**). **Listing this host is fine** — startup sync **skips self** for peer fetch. Align list + mesh secrets across masters per **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**. |
| **Network** | UFW / cloud firewall | **22**, **8080**, **9418** (and **5590** if Piper). Masters must reach each other on **`RELAY_GIT_PORT`** (**9418**) for peer **`git fetch`**. |

**Verify after first start:** `curl -sS -X POST 'http://127.0.0.1:8080/api/config'` → **`startup_catalog_sync.catalog_forge_sync_ok`** should be **`true`**; **`peer_fetch`** reports **`ok` / `failed`** counts (peer round is best-effort). If **`catalog_forge_sync_ok`** is **`false`**, fix forge/auth, then **`sudo systemctl restart relay-gateway`**.

**Example deployed masters (default installer `RELAY_MASTER_PEER_LIST`):** **`atlanta2.relaygateway.net`**, **`atlanta3.relaygateway.net`**, and **`jp-osa.relaygateway.net`**. Use your real FQDNs if you override the list.

**`/api/config`** and **`/api/status`** use **POST** (not GET) so paths like **`/api/status`** are never served as repo files from the **`/*path`** wildcard. Call with **`curl -X POST`**. HTML status: `curl -sS -X POST 'https://atlanta2.relaygateway.net/api/status?format=html'` (pipe to a file or **`less`**; browsers do not send POST from a plain bookmark).

| Node | HTML status (`POST`, `?format=html`) | JSON status (`POST`) | JSON config (`POST`) |
|------|--------------------------------------|----------------------|----------------------|
| **Atlanta2** | `curl -sS -X POST 'https://atlanta2.relaygateway.net/api/status?format=html'` | `curl -sS -X POST 'https://atlanta2.relaygateway.net/api/status'` | `curl -sS -X POST 'https://atlanta2.relaygateway.net/api/config'` |
| **Atlanta3** | `curl -sS -X POST 'https://atlanta3.relaygateway.net/api/status?format=html'` | `curl -sS -X POST 'https://atlanta3.relaygateway.net/api/status'` | `curl -sS -X POST 'https://atlanta3.relaygateway.net/api/config'` |
| **JP (Osaka)** | `curl -sS -X POST 'https://jp-osa.relaygateway.net/api/status?format=html'` | `curl -sS -X POST 'https://jp-osa.relaygateway.net/api/status'` | `curl -sS -X POST 'https://jp-osa.relaygateway.net/api/config'` |

**`/api/config`** returns **JSON only** (no HTML). **`POST /api/status`** defaults to JSON; use **`?format=html`** or **`Accept: text/html`** for the operator page body. For load checks, **`?strict=1`** returns HTTP **503** when any configured peer probe fails.

**Redeploy to masters (e.g. atlanta2 / atlanta3 / jp-osa):** build **`relay-linode-deploy.tgz`** on a trusted machine (**`./scripts/pack-linode-deploy.sh`**; see **[`DEPLOY_SELF_HOSTED.md`](./DEPLOY_SELF_HOSTED.md)**). Copy to each host and run **`sudo ./install.sh update`** from the unpacked bundle (or replace **`/opt/relay/bin/relay-gateway`** and **`relay-hook-handler`**, then **`sudo systemctl restart relay-gateway relay-git-daemon`**). Confirm with **`curl -sS -X POST 'http://127.0.0.1:8080/api/config' | head -c 400`** on the VM.

**`RELAY_MASTER_TRUST_ROOT` vs shallow catalog:** **`relay-gateway`** checks that the trust root is an ancestor of catalog **`HEAD`**. The startup script clones with **`RELAY_MASTER_CATALOG_CLONE_DEPTH`** (default **128** in **`relay-startup-catalog-sync.sh`**) and **deepens** a shallow catalog when needed. If the service still errors on **`merge-base`** / missing object, run **`sudo -u relay git -C /opt/relay/src/relay-master fetch --deepen 2048 origin`** (adjust path/remote if yours differ), then **`sudo systemctl restart relay-gateway`**.

**Every catalog `url`** in **`relay-catalog.json`** must be **clonable without an interactive password** as user **`relay`** (public **`https://`** forge project, or read-only token / **`git` credential**). Private GitLab URLs produce **`could not read Username for 'https://…'`** until fixed.

**Catalog tree copied or repaired without `origin`:** plain **`git pull`** can fail (“no tracking information” / no remote). Repair: **`sudo -u relay git -C /opt/relay/src/relay-master remote add origin <same URL as RELAY_MASTER_CATALOG_GIT_URL>`**, **`git fetch origin`**, **`git branch --set-upstream-to=origin/main main`** (or your branch). Then run sync as in **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** § “Updating a catalog node”.

**Stale bare directories:** Removing a repo from **`relay-catalog.json`** does **not** remove **`/opt/relay/data/<name>.git`**. **`POST /api/config` → `repos`** may list extra names on one node; safe to leave, or remove the bare directory manually after you no longer need it.

### Secrets (mesh / future)

**Normative policy:** how to place **`relay.env`**, keep **`RELAY_MASTER_PEER_LIST`** complete and aligned on masters, rotate mesh + webhook secrets together, and enforce a **shared deploy SSH key** — **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)** (sanitized template **[`../relay.env.example`](../relay.env.example)**).

**Today:** mesh and install secrets live in **`relay.env`** on the VM (or your own export pattern). **Inter-master** auth is **one secret set at install** for now — no time-based rolling codes (**[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)** § Inter-master secrets).

**Future / central secrets:** Akamai Cloud does **not** expose a separate “Linode Secrets Manager” product (the old product page is gone). Typical options: run **[OpenBao](https://openbao.org/)** or **[HashiCorp Vault](https://www.linode.com/docs/marketplace-docs/guides/hashicorp-vault/)** on a Compute instance or LKE (Linode Marketplace / guides), use **Vault Agent** (or a small sync script) to render **`relay.env`** or **`systemd` `Environment=`** drops; or use **another** cloud’s secret store (AWS/GCP/Azure) if your org already has one. Same targets: **`RELAY_PEER_SECRET`**, **`RELAY_MASTER_MESH_SECRET`**, webhooks, etc. Not required for initial bring-up.

---

## Vercel DNS (first step of `install`)

**[`scripts/relay-install.sh`](../scripts/relay-install.sh)** (shipped as **`install.sh`** in the tarball) asks for a **full hostname** (e.g. **`atlanta1.relaygateway.net`**), uses the **Vercel API** to create or update an **A** record to this machine’s **detected public IPv4**, then **polls public DNS** (e.g. `dig @8.8.8.8`) until that name resolves to the same address. If resolution never matches, **install aborts** before package/binary setup.

**Authentication** matches the Docker entrypoint and legacy Rackspace/Kubernetes path: **`VERCEL_API_TOKEN`** (Bearer). Optional **`VERCEL_TEAM_ID`** for team-scoped API calls (same as **`relay-credentials`** / daemonset env in **`terraform/rackspace-spot`**).

| Variable | Purpose |
|----------|---------|
| **`RELAY_PUBLIC_FQDN`** or **`RELAY_DNS_FQDN`** | Full DNS name to publish and verify (e.g. `atlanta1.relaygateway.net`). |
| **`VERCEL_API_TOKEN`** | Required for DNS upsert (unless skipping). **`VERCEL_TOKEN`** is accepted as an alias. |
| **`VERCEL_TEAM_ID`** | Optional; append **`?teamId=`** on Vercel API requests. |
| **`RELAY_VERCEL_DOMAIN`** | Optional; **Vercel zone** (e.g. `relaygateway.net`) if you want to skip auto-detection. The FQDN must end with **`.${RELAY_VERCEL_DOMAIN}`** (or equal the zone for an apex record). |
| **`RELAY_SKIP_VERCEL_DNS=1`** | Skip DNS entirely (CI, air-gapped, or DNS managed elsewhere). |
| **`RELAY_DNS_TTL`** | Record TTL in seconds (default **60**). |
| **`RELAY_DNS_WAIT_ATTEMPTS`** | Max poll attempts (default **36**). |
| **`RELAY_DNS_WAIT_SLEEP`** | Seconds between polls (default **5**). |

**Zone auto-detection:** the script calls **`GET /v5/domains`** and picks the **longest** domain name on the account that matches the suffix of your FQDN (so `atlanta1.relaygateway.net` maps to zone **`relaygateway.net`**, record **`atlanta1`**). The domain must already exist under your Vercel team/account.

**Interactive (SSH TTY):** you are prompted to confirm DNS setup, then for **hostname** and **API token** if the token is not already in the environment.

**Non-interactive / cloud-init:** set **`RELAY_INSTALL_NONINTERACTIVE=1`** and either:

- **With DNS:** export **`RELAY_PUBLIC_FQDN`** and **`VERCEL_API_TOKEN`** (and **`VERCEL_TEAM_ID`** if needed), **or**
- **Without DNS:** set **`RELAY_SKIP_VERCEL_DNS=1`**.

**Non-TTY without env:** if neither FQDN+token nor **`RELAY_SKIP_VERCEL_DNS=1`** is set, the installer **skips** the DNS phase and continues (same pattern as other optional prompts).

---

## Binary install (`install.sh` = install / update / repair)

The tarball includes **`install.sh`** (`relay-install.sh`), **`piper-tts-http.py`**, binaries, and **`relay-bootstrap.sh`**.

```bash
./scripts/pack-linode-deploy.sh
scp target/relay-linode-deploy.tgz root@IP:/root/
ssh root@IP 'cd /root && tar xzf relay-linode-deploy.tgz && sudo ./install.sh install'
```

**Hardened SSH (no root login):** upload as a sudo user, extract under **`/root`** (installer expectation), run **`install.sh`** with **`sudo`**—same pattern as **`relay-linode-redeploy-seed.sh`** when **`RELAY_SSH_USER`** is not **`root`**:

```bash
scp target/relay-linode-deploy.tgz ari@IP:/tmp/
ssh -o BatchMode=yes ari@IP 'sudo cp /tmp/relay-linode-deploy.tgz /root/ && cd /root && sudo tar xzf relay-linode-deploy.tgz && sudo env RELAY_BIN_SOURCE=/root RELAY_INSTALL_NONINTERACTIVE=1 ./install.sh update'
```

Or **`./scripts/relay-linode-redeploy-seed.sh IP`** (defaults to **`ari@IP`**; see script header). Agents: **[`CURSOR_AGENTIC_SSH.md`](./CURSOR_AGENTIC_SSH.md)** §5.

| Command | Purpose |
|--------|---------|
| `sudo ./install.sh install` | First-time install: **Vercel DNS** first (unless skipped), then deps + binaries; interactive feature prompts for Piper TTS and npm (unless non-interactive). Fails if already installed unless `RELAY_INSTALL_FRESH=1`. |
| `sudo ./install.sh update` | Refresh binaries from tarball dir; restart services. |
| `sudo ./install.sh repair` | Re-apply systemd, hooks, npm extensions, Piper service from **`state/features.json`**. If **`features.json`** is missing but **`/opt/relay/bin/relay-server`** exists, a minimal state file is created (Piper/npm off) so repair/update work on manually seeded hosts. |
| `sudo ./install.sh reconfigure-features` | Re-run feature prompts and rewrite **`features.json`** (only supported way to add/change optional features). |
| `sudo ./install.sh refresh-features` | Rescan Piper models + Argos packs into **`features.json`** (after adding voices or translation packages). |

**Ports:** HTTP **8080**, git daemon **9418**, Piper HTTP **5590** (if enabled). Data **`/opt/relay/data`**.

**Port 80 / 443 (recommended):** **`relay-server` listens on :8080**. For **`http://` and `https://` without a port** and for **Let’s Encrypt**, use **Caddy** in front:

1. From the deploy tarball (or repo **`scripts/`**): **`sudo -E bash install-caddy-tls.sh`** with **`RELAY_PUBLIC_HOSTNAME`** (same as **`relay.env`**) and **`CADDY_EMAIL`** (Let’s Encrypt account). Run this on **every** master peer host so **:80** and **:443** terminate in Caddy with Let’s Encrypt; use the **same** **`CADDY_EMAIL`** on all masters (e.g. **`hostmaster@relaygateway.net`**) so ACME account and expiry notices stay consistent.
2. **On-demand TLS:** the first **`https://{repo}.{node}/…`** request obtains a certificate for that hostname (HTTP-01). A small local service (**`relay-caddy-ask`**) only approves names matching **`{node}`** or **`*.{node}`** (one repo label), same rule as relay **`Host`** routing.
3. **UFW:** the script allows **80** and **443** (before starting Caddy, so ports open even if the first config fails); **8080** can stay allowed for debugging or be restricted later.

4. **IPv6:** if **`https://…`** hangs from some networks but **`curl -4 https://…`** works, the host’s **AAAA** path may be broken (Linode routing/firewall or no listener on v6). Fix server IPv6 or temporarily drop **AAAA** for that name in DNS.

If you previously used the **iptables :80 → :8080** redirect, **`install-caddy-tls.sh`** removes that block so **Caddy** can bind **:80**.

**Legacy one-liner (no TLS):** UFW **`*nat` `REDIRECT`** **:80 → :8080** is documented in git history only; prefer Caddy for production.

**State:** **`/opt/relay/state/features.json`** records enabled features, ports, and **inventories** (Piper voices, Argos language pairs, npm package list). **`relay-gateway`** reads it (`RELAY_FEATURES_STATE_PATH` in the unit file) and exposes it under **`POST /api/config`** → **`installed_features`** (`manifest` + `summary`). Peers and clients use that to see what each node offers.

**Feature catalog (Piper TTS, offline translation, npm extensions, subfeatures):** **[INSTALL_FEATURES.md](./INSTALL_FEATURES.md)**.

**Non-interactive install (CI/cloud-init):**

With **Vercel DNS** (recommended when the hostname should point at this VM):

```bash
sudo RELAY_INSTALL_NONINTERACTIVE=1 \
  RELAY_PUBLIC_FQDN="atlanta1.relaygateway.net" \
  VERCEL_API_TOKEN="your-token" \
  RELAY_FEAT_PIPER=0 \
  RELAY_FEAT_TRANSLATION=0 \
  ./install.sh install
```

Optional: **`VERCEL_TEAM_ID=...`**, **`RELAY_VERCEL_DOMAIN=relaygateway.net`** (if you do not want API-based zone detection).

Skipping DNS (no token on host, or DNS managed manually):

Set **`RELAY_PUBLIC_HOSTNAME`** to this node’s FQDN (required for **`Host: {repo}.{node}`** routing when Vercel did not append it). Set **`RELAY_SERVER_ID`**. **`RELAY_MASTER_PEER_LIST`** defaults in **`install.sh`** to the canonical mesh / **master** FQDNs (**`atlanta2.relaygateway.net;atlanta3.relaygateway.net;jp-osa.relaygateway.net`**); override only when you need a different set. Roadmap for mesh events, **internal** anti–over-ping (**SHA-first**, optional **`via` / hops**; **Slack-independent**), and subscriptions: **[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)**. Optional operator Slack: **[`SLACK_MESH_NOTIFY.md`](./SLACK_MESH_NOTIFY.md)**.

```bash
sudo RELAY_INSTALL_NONINTERACTIVE=1 RELAY_SKIP_VERCEL_DNS=1 \
  RELAY_PUBLIC_HOSTNAME="dallas1.relaygateway.net" \
  RELAY_SERVER_ID="relay-dallas1" \
  RELAY_FEAT_PIPER=0 RELAY_FEAT_TRANSLATION=0 \
  ./install.sh install
```

---

## One-liner from GitLab raw (any Linux)

**[`scripts/relay-curl.sh`](../scripts/relay-curl.sh)** installs `curl`, `tar`, `bash`, etc. if missing, pulls **`relay-install.sh`** + helpers from **`RELAY_RAW_BASE`** (default: GitLab **`main`**), then runs the subcommand.

**Repair or update** (server already has `/opt/relay/bin/relay-server`; pulls latest **`relay-install.sh`** from **`RELAY_RAW_BASE`**, reapplies systemd/hooks; binaries stay in place unless you set **`RELAY_DEPLOY_TGZ_URL`** / **`RELAY_BIN_SOURCE`** for **update**):

```bash
curl -fsSL "https://gitlab.com/clevertree/relay-server/-/raw/main/scripts/relay-curl.sh" | sudo bash -s -- repair
```

Swap **`repair`** for **`update`** after you’ve published new binaries (see **[`DEPLOY_SELF_HOSTED.md`](./DEPLOY_SELF_HOSTED.md)**).

**First install** (needs **`relay-linode-deploy.tgz`** on **your** HTTPS, or **`RELAY_BIN_SOURCE`** on disk):

```bash
export RELAY_DEPLOY_TGZ_URL="https://YOUR_HOST/relay-linode-deploy.tgz"
curl -fsSL "https://gitlab.com/clevertree/relay-server/-/raw/main/scripts/relay-curl.sh" | sudo -E bash -s -- install
```

**Update from a new tarball:**

```bash
curl -fsSL "https://gitlab.com/clevertree/relay-server/-/raw/main/scripts/relay-curl.sh" | \
  sudo env RELAY_DEPLOY_TGZ_URL="https://YOUR_HOST/relay-linode-deploy.tgz" bash -s -- update
```

Non-interactive: `export RELAY_INSTALL_NONINTERACTIVE=1` before the same **`curl | sudo -E bash`** line. For **`install`**, also set **`RELAY_PUBLIC_FQDN`** + **`VERCEL_API_TOKEN`**, or **`RELAY_SKIP_VERCEL_DNS=1`**, plus any Piper/npm vars (see **[Vercel DNS (first step of `install`)](#vercel-dns-first-step-of-install)**).

**Self-hosted raw mirror:** `export RELAY_RAW_BASE="https://git.example.com/org/relay-server/-/raw/main"` before **`curl`**.

**Binaries on disk instead of URL:**

```bash
export RELAY_BIN_SOURCE=/root/deploy   # relay-server + relay-hook-handler here
curl -fsSL "https://gitlab.com/clevertree/relay-server/-/raw/main/scripts/relay-curl.sh" | sudo -E bash -s -- install
```

---

## Seed repos (push from laptop; no third-party forge required on server)

Relay’s git daemon uses the **`git://`** protocol: there is **no transport-level authentication**. Policy (signed commits, custom hooks, etc.) is optional and comes from each repo’s **`.relay.yaml`** and **`relay-hook-handler`**, not from the server requiring SSH keys or HTTP basic auth.

**New bare repo (today):** stock **`git daemon`** does **not** create missing paths; **`git push git://…`** fails until the bare directory exists. Create the slot over HTTP (no SSH needed), then push. **`RELAY_GIT_ENSURE_BARE_ENABLED`** defaults to **`1`** on install; set **`RELAY_GIT_ENSURE_BARE_ENABLED=0`** in **`relay.env`** to disable and use manual init only.

**Planned:** Relay will provision **`RELAY_REPO_PATH/<repo>.git`** on first allowed push (same trust rules as today’s whitelist—see **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**), so the **`curl …/ensure-bare/…`** step becomes optional for supported clients.

```bash
# Laptop (replace IP and repo name) — required until push-time provisioning ships
curl -fsS -X POST "http://IP:8080/api/git/ensure-bare/snesology-presets"
```

```bash
# Laptop
git remote add relay git://IP:9418/snesology-presets.git
git push -u relay main
```

**Alternative (SSH on server):** create the bare repo by hand if you disabled ensure-bare:

```bash
sudo -u relay git init --bare /opt/relay/data/snesology-presets.git
```

With **`relay-master`**, add or update **`relay-catalog.json`** (**`relayRootCommit`**, **`url`**, **`branch`**), push the catalog, on the node **`git pull`** + **`sync-relay-catalog.sh`** + **`--write-authorized-repos`**, then **`systemctl restart relay-server`**. Ad hoc **`anchor_commit`** edits to YAML alone (without catalog) **contradict** the single-source-of-truth model.

---

## Extra nodes (`RELAY_SERVER_ID` + repos)

**Interactive (TTY):** run **`relay-bootstrap.sh`** with no manifest — you get a checkbox-style list (**`[x]`** = clone). Defaults: **relay-template**, **relay-server**, **songwalker-library** (GitLab `clevertree/*` URLs in **`relay-bootstrap.sh`**). Toggle with **`1` `2` `3`**, **`a`** = all, **`n`** = none, **Enter** = proceed.

```bash
export RELAY_SERVER_ID=relay-dallas1
sudo -u relay bash /opt/relay/.../relay-bootstrap.sh   # or from deploy tarball directory
```

**Non-interactive / cloud-init:** clones **all** catalog repos unless excluded:

```bash
RELAY_BOOTSTRAP_NONINTERACTIVE=1 RELAY_CATALOG_EXCLUDE=relay-server ./relay-bootstrap.sh
```

**Manifest URL** (anchors + optional npm) still supported:

```bash
export RELAY_SERVER_ID=relay-dallas1
export RELAY_BOOTSTRAP_MANIFEST_URL=https://…/bootstrap.json
./relay-bootstrap.sh
```

Manifest **`relay_server_id`** must match. Copy the same **`relay-master`** revision (or a **`sync-relay-catalog.sh`**-generated **`authorized-repos.yaml`**) to the new node before starting **`relay-server`**. To add catalog repos, change **`relay-master`** **`relay-catalog.json`**—not only **`relay-bootstrap.sh`** lists.

---

## Operator checklist: redeploy binary + seed a repo (git://)

Use this after **SSH key auth** works for your operator user (**`user@IP`**, often **not** **`root`** on hardened nodes—see **[`CURSOR_AGENTIC_SSH.md`](./CURSOR_AGENTIC_SSH.md)** §5; use Linode Lish if you only have console). Pushes use **`git://` only**, not **`git@…`**.

| Step | Action |
|------|--------|
| 1 | Build tarball: **`./scripts/pack-linode-deploy.sh`** (or GitLab **`package:linode-tgz`** artifact). |
| 2 | **Automated:** **`./scripts/relay-linode-redeploy-seed.sh IP`** (default SSH user **`ari`**; **`RELAY_SSH_USER=root`** for first-boot images). Optional **`RELAY_SSH_IDENTITY`**, **`RELAY_LOCAL_REPO`**, **`RELAY_SEED_*`**. |
| 3 | **Manual:** **`root`:** `scp … root@IP:/root/` → **`ssh root@IP`** → **`tar xzf … && ./install.sh update`**. **Non-root:** same flow as **hardened** binary install § above (**`/tmp` → `/root` → `sudo ./install.sh update`**). |
| 4 | **`curl -fsS -X POST "http://IP:8080/api/git/ensure-bare/your-repo"`** (skip if script ran; **planned:** optional once first-push provisioning exists). |
| 5 | **`git push -u git://IP:9418/your-repo.git your-branch`** |
| 6 | Confirm: **`curl -s http://IP:8080/api/config \| jq .git_ensure_bare_enabled,.repos`** |

Example push **paradigm-threat-files** from a laptop (default branch **`master`**):

```bash
RELAY_LOCAL_REPO="$HOME/dev/old/paradigm-threat-files" \
RELAY_SEED_BRANCH=master \
  ./scripts/relay-linode-redeploy-seed.sh 192.155.92.23
```

---

## Verify

```bash
curl -s http://IP:8080/api/config | jq .
# Optional features (if configured): jq .installed_features
```

---

## Docs

- **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** — full trust model.

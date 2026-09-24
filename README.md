# <img src="icon.png" width="32" height="32" align="center" /> Relay Gateway (Rust)

Implements the Relay API over bare Git repositories with static directory serving and detailed error reporting.

**This repo is [clevertree/relay-gateway](https://gitlab.com/clevertree/relay-gateway)** (clone folder `relay-gateway/`). The old monorepo **[clevertree/relay](https://gitlab.com/clevertree/relay) is archived**.

**Android / web tester:** sibling [`relay-runtime/docs/ANDROID_WEB_TEST.md`](../relay-runtime/docs/ANDROID_WEB_TEST.md) — do not start in `relay-client-android`. Glossary: [`TERMINOLOGY.md`](../relay-runtime/docs/TERMINOLOGY.md).

| Piece | GitLab | Local folder |
|-------|--------|----------------|
| Gateway | `clevertree/relay-gateway` | `relay-gateway/` |
| Runtime | `clevertree/relay-runtime` | `relay-runtime/` |
| Web client | `clevertree/relay-client-web` | `relay-client-web/` |
| Android client | `clevertree/react-client-android` | `relay-client-android/` |

Operator tool: **[`docs/RELAY_OPS.md`](docs/RELAY_OPS.md)**. Doc index: **[`docs/README.md`](docs/README.md)**. Historical notes: **[`docs/archive/`](docs/archive/)**.

## Features

- Serves files from multiple bare Git repositories (`.git` suffix)
- Static file serving from configurable directories
- Catalog-driven bare mirrors (`ExecStartPre` sync + peer `git fetch`); not an hourly pull loop
- Detailed 404 error messages showing searched paths, repos, and branches
- CleverScript client hooks via **relay-runtime** (WASM on web, JNI on Android)
- Optional HTTPS support with automatic certificate loading
- CORS-enabled; responses may include informational headers (e.g. echo of resolved repo/branch — **not** input channels). **Routing is Host-only.** **`X-Relay-Branch`** is **deprecated for routing** (do not send it to select a repo/branch) but **relay-ops Browse** still uses it as a display hint. Directory listings use **`X-Relay-Tree: 1`** — see GET below and **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §8–§11.

Install-time **optional capabilities** (Piper TTS, offline Argos translation, npm hook packages) plus **Kubo IPFS** (default on at `install`/`update`) are recorded in **`state/features.json`** and advertised in **`GET`/`POST /api/config` → `installed_features`** so peers know what each node offers. See **[docs/INSTALL_FEATURES.md](docs/INSTALL_FEATURES.md)**.

**`GET /api/status`** (also **HEAD**/**POST**) on the **node FQDN** (or loopback) returns this node’s version, mesh role, feature summary, **IPFS seed** (`self.ipfs`, `ipfs_ok`, per-repo CID/cache counts), a **per-repo table** (catalog vs on-disk, `publish_branch` SHA — from **local** git, not `git ls-remote`), and reachability of each **`RELAY_MASTER_PEER_LIST`** host (probes **`POST https://{host}/api/config`** by default). JSON by default; **`Accept: text/html`** or **`?format=html`** for a dashboard; **`?strict=1`** returns **503** when any **git-mesh peer** is down (IPFS-down does **not** 503). Missing catalog bares set **`repos_ok: false`** but do not affect **`strict`**. Override probe URL with **`RELAY_STATUS_PEER_URL_TEMPLATE`** (use **`{host}`**). On **parked domains** and **`{repo}.{node}`** Hosts, `/api/status` is a **site file / GET hook**, not the operator dashboard.

**Examples:** `curl -sS 'https://iad1.relaygateway.net/api/status?format=html'` · JSON: **`curl -sS -X POST 'https://iad1.relaygateway.net/api/config'`**. Full table: **[`docs/DEPLOY_LINODE.md`](docs/DEPLOY_LINODE.md)** (operator URL table).

## Operations (deployed masters)

**Primary tool:** **`relay-ops`** — CLI + local UI for any Relay fleet. Bootstrap from a GitLab project URL (`ops/fleet.json`). Wrapper: **`./scripts/relay-fleet.sh`**. See **[`docs/RELAY_OPS.md`](docs/RELAY_OPS.md)** and **[`ops/fleet.json`](ops/fleet.json)**. **`./scripts/deploy-all-masters.sh`** is **`relay-ops --build deploy-all`**.

**CleverScript runtime:** **`relay-runtime`** lives as a **sibling crate** at **`../relay-runtime`** (see `Cargo.toml` `path = …`). **`cargo test --locked`** in **relay-gateway** uses that path dependency.

**Vendored for CI:** a **Rust-only `src/` + `Cargo.toml` snapshot** of **`themed-styler/`** ships in this repo so tests do not depend on an external themed-styler checkout. The published **`themed-styler`** crate may lag this snapshot; refresh **`themed-styler/`** from your checkout when APIs diverge, then run **`cargo test --locked`**.

## Fresh Ubuntu server from GitLab tarball

We only test this path on **Ubuntu** 22.04/24.04. **One-line install** (latest successful **`main`** pipeline artifact **`package:linode-tgz`**, non-interactive installer, Vercel DNS skipped—add **`RELAY_PUBLIC_FQDN`** + **`VERCEL_API_TOKEN`** and drop **`RELAY_SKIP_VERCEL_DNS=1`** to register DNS; private GitLab needs **`PRIVATE-TOKEN`** / tokenized URLs on **`curl`**):

```bash
curl -fsSL 'https://gitlab.com/clevertree/relay-gateway/-/raw/main/scripts/relay-curl.sh' | sudo env RELAY_DEPLOY_TGZ_URL='https://gitlab.com/clevertree/relay-gateway/-/jobs/artifacts/main/raw/target/relay-linode-deploy.tgz?job=package%3Alinode-tgz' RELAY_SKIP_VERCEL_DNS=1 RELAY_INSTALL_NONINTERACTIVE=1 bash -s -- install
```

Configure trust and catalog in **`/opt/relay/relay.env`** afterward (**[`docs/DEPLOY_LINODE.md`](docs/DEPLOY_LINODE.md)**). Forks: set **`RELAY_RAW_BASE`** to your raw base and point **`RELAY_DEPLOY_TGZ_URL`** at your artifact or generic-package URL.

## Repository Policy

- Never import or reference files directly from `apps/` other than code in this crate; prefer shared assets and data in
  shared crates.

Endpoints

- OPTIONS / and OPTIONS /* — discovery endpoint returning capabilities, branches, repos, current selections, and branch
  HEAD commits (branchHeads). The **current repo** is derived from the HTTP **`Host`** header (see below).
- GET /{path} — read file from the bare repo and branch implied by **`Host`**: `{repo}.{RELAY_PUBLIC_HOSTNAME}` or
  `{repo}{DELIM}{branch-slug}.{RELAY_PUBLIC_HOSTNAME}` (see **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §8).
  When **`Host`** is the **node apex** only (no `{repo}.` prefix), the server uses **`RELAY_DEFAULT_REPO`** (default bare **`relay-master`**) if that repo exists under **`RELAY_REPO_PATH`**; missing paths return **404**.
  **Default branch** when the host label has **no** delimiter: the bare repo’s symbolic **`HEAD`** (e.g. **`master`** after `git push` if that is the default), else **`main`**. **Do not** send **`X-Relay-Branch`** to select a branch (deprecated for routing; Host branch slug when §8 delimiter is in use). **`X-Relay-Tree: 1`** is the directory-listing opt-in (not a routing header). Browsers send **`Host`** automatically for `http://repo.node/...` **if DNS** resolves **`repo.node`** (often a wildcard **`*.your-node-fqdn`** → server IP).
    - **File listing:** send **`X-Relay-Tree: 1`** on **`GET /`** or **`GET /{dir}`** (also **HEAD**). Response is **`application/vnd.relay.tree+json`** (`schema: relay.tree.v1`) even when **`index.html`** / **`index.json`** exist in that tree. Without the header, **`GET /`** stays the site shell; **`GET /index.html`** and **`GET /index.json`** stay ordinary blobs (the header is ignored for files).
    - If the path is a directory and the tree header is **not** set, the response is a legacy JSON map of children (not HTML).
    - If the file/dir is missing, returns 404. Repo-host **`GET /`** without the tree header runs **`server.hooks.get`** (site shell).
- PUT /{path} — write file and commit; same **`Host`** / branch rules as GET.
    - Commits are validated by the `server.hooks.pre-commit` script defined in `.relay.yaml`. Rejected commits return
      400/500 with error text.
- DELETE /{path} — delete file and commit; same **`Host`** / branch rules as GET.
- QUERY * — Custom method for YAML-driven query using the local PoloDB index built by hooks (no POST alias).
    - Pagination defaults: pageSize=25, page=0; can override via request body
    - **Branch / `all`:** moving to **request body** (no **`X-Relay-Branch`**); see **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §9.
    - Request body (generic): `{ filter?: object, page?: number, pageSize?: number, sort?: [{ field, dir }], branch?: string }` (exact schema TBD)
    - Response: `{ total, page, pageSize, items }`

## Repository Infrastructure

Relay repositories use a unified configuration file `.relay.yaml` at the root to control security and automation.

### Catalog repo (`relay-master`)

**[`relay-master`](../relay-master)** is the **allowlist** for which Git repositories a node may mirror: each entry in **`relay-catalog.json`** supplies **`url`** (clone source) and **`relayRootCommit`** (trusted root; **`scripts/sync-relay-catalog.sh`** verifies it is an ancestor of the branch tip). **`relay-gateway`** loads policy from **`RELAY_MASTER_CATALOG_PATH`** (path to a **`relay-master` checkout** with **`relay-catalog.json`**) or, alternatively, from **`RELAY_AUTHORIZED_REPOS_PATH`** (YAML, e.g. emitted by **`sync-relay-catalog.sh`**—never set both). Use **`RELAY_MASTER_TRUST_ROOT`** (40 hex) so startup fails if the checkout is the wrong Git history. See **[`docs/RELAY_TRUST_AND_BOOTSTRAP.md`](./docs/RELAY_TRUST_AND_BOOTSTRAP.md)**. Set **`RELAY_DEFAULT_REPO=relay-master`** for the catalog apex. **User-facing clone URLs** like **`git://relaygateway.net/<repo>.git`** are documented there (routing/DNS, not HTTP redirects).

### Git daemon (`git://`, default port **9418**)

The bundled **`git daemon`** enables **`receive-pack`**: there is **no git transport authentication** (no SSH keys / HTTP basic auth on that path). Optional enforcement (signed commits, custom **`pre-receive`**, etc.) is per repository via **`.relay.yaml`** and **`relay-hook-handler`**.

Public **`git://`** is **`relay-git-proxy`** on **`RELAY_GIT_PORT`** (default **9418**). Stock **`git daemon`** listens on loopback only (**`RELAY_GIT_DAEMON_PORT`**, default **9419**). The proxy parses the first pkt-line; on **`git-receive-pack`** for a missing **`RELAY_REPO_PATH/<repo>.git`** it runs the same create path as **`POST /api/git/ensure-bare/:repo`** (including **`receive.denyNonFastForwards=true`**), honors catalog **`publish_origin`** (does **not** create on a replica), then forwards to the daemon. **`git-upload-pack`** of a missing repo does **not** create. Set **`RELAY_GIT_ENSURE_BARE_ENABLED=0`** to disable both HTTP **`ensure-bare`** and first-push create. **`POST /api/config`** includes **`git_ensure_bare_enabled`**. HTTP **`ensure-bare`** remains available for operators who want to pre-create the slot. See **[`docs/RELAY_TRUST_AND_BOOTSTRAP.md`](./docs/RELAY_TRUST_AND_BOOTSTRAP.md)** for how **repository identity** (root commit) gates **updates** after the first commits land.

### Git Hooks (`relay-hook-handler`)
The server includes a native `relay-hook-handler` binary that should be symlinked to `hooks/pre-receive` and `hooks/post-receive` in your bare repositories.

Operators can validate the **same** branch rules locally or in CI with **`relay-branch-policy-check`** (`range` / `walk` subcommands). See **[`docs/RELAY_BRANCH_POLICY_OPERATOR.md`](docs/RELAY_BRANCH_POLICY_OPERATOR.md)**.

- **Branch Protection**: Enforces GPG/SSH signature requirements natively in Rust.
- **Custom validation**: Runs CleverScript (`.cs`) hooks from `.relay.yaml` with full commit context when configured.
- **Auto-Push**: Synchronizes successful pushes to a list of peer servers with **fast-forward** `git push` (no `--force`).

**Mesh (shipped pieces):** **`RELAY_MASTER_PEER_LIST`** (semicolon-separated FQDNs) is exposed in **`POST /api/config` → `peers`**. **`RELAY_NODE_ROLE`** (`master` \| `edge`) or inference from **`RELAY_PUBLIC_HOSTNAME`** in that list selects behavior: **only masters** fan out **`POST /hooks/mesh-event`** HTTP to peers (**edges** still get Slack if configured). **`RELAY_MASTER_MESH_SECRET`** (masters only): inter-master **`x-relay-mesh-secret`** / **`X-Relay-Peer-Secret`** when set; **`POST /hooks/peer`** on a master with this set accepts **only** that secret (not **`RELAY_PEER_SECRET`**). **`POST /hooks/peer`** on **edges** uses **`RELAY_PEER_SECRET`** (masters poke edges with the same value). See **[`docs/RELAY_MESH_NETWORKING_PLAN.md`](docs/RELAY_MESH_NETWORKING_PLAN.md)**. **mTLS** between masters is a **reverse-proxy** concern, not yet in-process. **Internal** anti–over-ping remains **Slack-independent**; optional Slack: **[`docs/SLACK_MESH_NOTIFY.md`](docs/SLACK_MESH_NOTIFY.md)**.

### Webhooks
- **GitHub**: `POST /hooks/github/{repo}` (repo name without `.git`) — see `.relay.yaml` `server.git.github` for enable/path metadata.
- **Peer notify → pull**: `POST /hooks/peer` with header **`X-Relay-Peer-Secret`** (see mesh section above) and JSON **`{ "repo", "peer", "branch"?, "new_sha"? }`**. If **`RELAY_MASTER_PEER_LIST`** is non-empty, **`peer`** must match one of those hostnames (case-insensitive).

## Configuration and secrets

**`relay.env`** on the server (typically **`/opt/relay/relay.env`**, **`EnvironmentFile`** for systemd) holds trust, mesh, and optional webhook values. **Sanitized template:** **[`relay.env.example`](relay.env.example)**. **Ops policy** (manual install per node, full master peer list, coordinated rotation including webhooks, shared deploy SSH key + optional install check): **[`docs/SECRETS_POLICY.md`](docs/SECRETS_POLICY.md)**. **Longer annotated reference:** **[`docs/examples/relay.env`](docs/examples/relay.env)**. **New node with catalog URL:** fill **`relay.env`** per **[`docs/DEPLOY_LINODE.md`](docs/DEPLOY_LINODE.md)** § “Greenfield checklist” so first boot clones the catalog, runs **`sync-relay-catalog.sh`**, and peer-fetches without extra SSH steps.

## Environment Variables

### Server Configuration
- `RELAY_REPO_PATH`: Root directory containing bare repos (default: `./data`)
- **`RELAY_PUBLIC_HOSTNAME`** or **`RELAY_NODE_FQDN`**: Node FQDN for HTTP routing. Clients must use **`Host: {repo}.{this-value}`** so each bare repo `RELAY_REPO_PATH/{repo}.git` is unambiguous (e.g. node `atlanta1.example.net`, repo host `snesology-library.atlanta1.example.net`). For many repos, use a **wildcard DNS** record `*.{node}` → server IP.
- `RELAY_STATIC_DIR`: Comma-separated static directories to serve (default: none)
- `RELAY_HTTP_PORT`: HTTP port (default: 80)
- `RELAY_HTTPS_PORT`: HTTPS port (default: 443)
- `RELAY_BIND`: Override bind address (format: `host:port`)
- **`RELAY_MASTER_PEER_LIST`**: Semicolon-separated peer FQDNs for **`POST /api/config`**, mesh bootstrap, and **`POST /hooks/peer`** allowlisting. **`install.sh`** defaults this to **`iad1.relaygateway.net;ord1.relaygateway.net;hkg1.relaygateway.net`** (designated **master** mesh nodes) unless you set **`RELAY_MASTER_PEER_LIST`** or **`DEFAULT_RELAY_MASTER_PEER_LIST`**. If empty after install, **`POST /hooks/peer`** allows any **`peer`** hostname once the shared secret matches.
- **`RELAY_NODE_ROLE`**: Optional **`master`** or **`edge`**. If unset, **master** when **`RELAY_MASTER_PEER_LIST`** is empty (dev), else **master** when **`RELAY_PUBLIC_HOSTNAME`** is listed, otherwise **edge**.
- **`RELAY_MASTER_MESH_SECRET`**: Same value on all **masters**; omit on **edges**. When set on a master, that node accepts **only** this secret for **`POST /hooks/mesh-event`** ingest and for **`POST /hooks/peer`** (inter-master inject). Outbound mesh POSTs to hosts in **`RELAY_MASTER_PEER_LIST`** use this header when set.
- **`RELAY_PEER_SECRET`**: **Edge** **`POST /hooks/peer`**; also fallback mesh-event secret when **`RELAY_MESH_EVENT_SECRET`** unset. On **masters** without **`RELAY_MASTER_MESH_SECRET`**, peer hook still uses this (legacy).
- **`RELAY_GIT_PORT`**: Public **`git://`** port (**`relay-git-proxy`**, default **9418**). Also used when building **`git://`** fetch URLs in the peer hook.
- **`RELAY_GIT_DAEMON_PORT`**: Loopback **`git daemon`** port (default **9419**). Must differ from **`RELAY_GIT_PORT`**.
- **`RELAY_GIT_BACKEND`**: Override backend `host:port` for the proxy (default **`127.0.0.1:${RELAY_GIT_DAEMON_PORT}`**).

### Repository Management
- **`RELAY_DEFAULT_REPO`**: Bare repo name (no `.git`) served at the **node apex** when **`Host`** is the node FQDN only (defaults to **`relay-master`** if unset and that bare exists). Use **`relay-master`** when following the catalog model (**[`docs/RELAY_TRUST_AND_BOOTSTRAP.md`](./docs/RELAY_TRUST_AND_BOOTSTRAP.md)**).
- **`RELAY_MASTER_CATALOG_PATH`**: Filesystem path to a **`relay-master` worktree** (contains **`relay-catalog.json`**). Builds the same allowlist **`relay-gateway`** would read from YAML. **Mutually exclusive** with **`RELAY_AUTHORIZED_REPOS_PATH`**.
- **`RELAY_MASTER_TRUST_ROOT`**: Optional but **recommended in production**: 40-character commit id—the **initial (root) commit** of your canonical **`relay-master`** (`git rev-list --max-parents=0 main`). Startup verifies it is an **ancestor** of the checkout **`HEAD`** so a mistaken clone path cannot point at an unrelated repo.
- **`RELAY_MASTER_BRANCH`**: If the catalog checkout is in **detached HEAD**, set this branch name for the synthetic **`relay-master`** policy row (default **`main`**).
- **`RELAY_AUTHORIZED_REPOS_PATH`**: YAML allowlist (legacy / CI); do not set together with **`RELAY_MASTER_CATALOG_PATH`**.
- `RELAY_MASTER_REPO_LIST`: Semicolon-separated list of repos to clone on startup (e.g., `https://gitlab.com/clevertree/relay-template.git`)
- `DEFAULT_REPOS`: Alternative to RELAY_MASTER_REPO_LIST for Docker entrypoint
- **`RELAY_GIT_ENSURE_BARE_ENABLED`**: When enabled (default: **on** if unset), first **`git push`** (`git-receive-pack`) may create **`RELAY_REPO_PATH/<repo>.git`**, and **`POST /api/git/ensure-bare/:repo`** may do the same over HTTP. Set to **`0`** / **`false`** / **`no`** to turn off both.
- **`POST /api/config` → `startup_catalog_sync`**: JSON from **`state/startup_catalog_sync.json`** (last **`relay-startup-catalog-sync.sh`** run: forge sync ok, optional error tail, peer fetch counts). Omitted if the file is missing. Forge sync failure does **not** block **`relay-gateway`**; HTTP serves whatever bare repos exist.

### TLS/HTTPS Configuration
- `RELAY_TLS_CERT`: Path to TLS certificate file
- `RELAY_TLS_KEY`: Path to TLS private key file

### ACME Challenge Support
- `RELAY_ACME_DIR`: Directory for ACME HTTP-01 challenges (default: `/var/www/certbot`)

## Headers

- **`Host`**: Selects bare repo and optional branch via **one** DNS label before **`{RELAY_PUBLIC_HOSTNAME}`** — see **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §8. Repo name without `.git`; must match `{repo}.git` under `RELAY_REPO_PATH`.
- **`X-Relay-Tree: 1`**: Directory listing (`GET`/`HEAD` `/` or `/{dir}`) → `application/vnd.relay.tree+json`. Not a routing header.
- **`X-Relay-Branch`:** **deprecated for routing** (do not send it to select repo/branch; use **Host**). Still echoed / used as a browse hint in **relay-ops**. Removal of routing use: §11 in the same doc.

## Env

- Repository rules and validation are defined in `.relay.yaml`; server hooks are **CleverScript** (`.cs`) executed in-process (no Node).
- See [SERVER_HOOKS.md](docs/SERVER_HOOKS.md) for detailed documentation on configuration and hook execution.
- `relay_index.json` is maintained by pre-commit/pre-receive scripts to track metadata changes and build queryable
  indexes.
- Rules are enforced for new commits by the respective hook scripts. Repositories can customize validation by
  implementing these scripts.

Server hook scripts (paths from `.relay.yaml` under `server.hooks.*`)

- `hooks/server/pre-commit.cs` — optional; runs on HTTP `PUT` before the ref advances.
- `hooks/server/pre-receive.cs` — optional; runs on `git push` after native branch rules.
- `hooks/server/index.cs` — optional; runs after Rust syncs `data/**/meta.yaml` into `index.db.json`.
- `hooks/server/get.cs` — optional; dynamic GET when the path is missing from the tree (see `server.hooks.get` in **[`docs/SERVER_HOOKS.md`](docs/SERVER_HOOKS.md)**): Express-style **`run(ctx, next)`**, Rust **`optimizeRasterFromGetContext`** for rasters, **`next()`** for plain files and 404 JSON, uncaught hook errors → HTTP 500 JSON with full technical detail.

## CLI & Run

### 1. Build
```bash
cargo build --release --bin relay-gateway --bin relay-hook-handler --bin relay-branch-policy-check
```
Binaries: **`target/release/relay-gateway`**, **`target/release/relay-hook-handler`**, **`target/release/relay-branch-policy-check`** (optional operator / CI validator — not installed on masters by default).

### 2. Run server
```bash
cargo run --release -- serve [OPTIONS]
```
(You can omit **`serve`** and run **`cargo run --release`** with the same env vars; use **`query`** for the CLI query helper.)

**Options:**
- `--repo <PATH>`: Bare Git repository root directory (default: from `RELAY_REPO_PATH` env or `./data`)
- `--static <DIR>`: Additional static directory to serve files from (repeatable)
- `--bind <HOST:PORT>`: Bind address (default: from `RELAY_BIND` env or `0.0.0.0:80`)

**Example:**
```bash
RELAY_REPO_PATH=/srv/relay/data \
RELAY_STATIC_DIR=/srv/relay/www \
RELAY_HTTP_PORT=8080 \
cargo run --release -- serve
```

**GET serving order:** `--static` paths first, then the selected bare repo/branch. **Logs:** stdout and rolling files under **`./logs/server.log*`** (method, path, status, latency).

**Quick local checks** (use **`Host`** for repo selection; **`X-Relay-Branch` deprecated** — see **`docs/DEPLOY_WEB_AND_CI.md`**). Loopback **`GET`** is the node operator surface:
```bash
curl -sS "http://127.0.0.1:8080/api/status" | head
curl -sS "http://127.0.0.1:8080/api/config" | head
curl -sS -H "Host: relay-template.localhost" "http://127.0.0.1:8080/" | head
```

### 3. Docker

**Build:**
```bash
docker build -t relay-gateway .
```

**Run:**
```bash
docker run -d \
  -p 8080:8080 \
  -e DEFAULT_REPOS="https://gitlab.com/clevertree/relay-template.git" \
  -e RELAY_HTTP_PORT=8080 \
  relay-gateway
```

The entrypoint script will:
1. Clone repositories from `DEFAULT_REPOS` (bare) to `/srv/relay/data/<name>.git`
2. Copy prebuilt static assets from `/srv/relay/prebuilt` to `/srv/relay/www`
3. Start **`relay-gateway`** on the configured port (catalog / peer sync is `ExecStartPre` on systemd hosts; the image is not an hourly pull loop)

## Detailed Error Messages

When a file is not found, the server returns a detailed 404 error showing:

```
Not Found

Path: hooks/client/get.jsx
Branch: main
Repo: relay-template

No `server.hooks.get` CleverScript (.cs) configured in .relay.yaml for dynamic GET.
```

This helps developers debug:
- Which repository was searched
- Which branch was checked
- Which path was requested
- What static directories were checked (if no repo selected)

## Container Deployment

The server is designed to run in containers with automatic SSL termination via nginx proxy. The entrypoint script
handles:

1. **Repository Initialization**: Clones template repository if none exists
2. **SSL Certificate Provisioning**: Uses Let's Encrypt when `RELAY_CERTBOT_EMAIL` is set
3. **DNS Registration**: Registers with Vercel DNS when `VERCEL_API_TOKEN` is provided
4. **Nginx Proxy Configuration**: Automatically configures nginx to proxy HTTPS to **`relay-gateway`**

**Bare metal (Linode tarball):** **`install.sh`** runs the same Vercel DNS flow **first** (FQDN + token, or skip with **`RELAY_SKIP_VERCEL_DNS=1`**). See **[`docs/DEPLOY_LINODE.md`](docs/DEPLOY_LINODE.md)** and **[`docs/DEPLOY_SELF_HOSTED.md`](docs/DEPLOY_SELF_HOSTED.md)** (build/publish tarball without cloud CI). Planned **static multi-site** layout (no Next/Node runtime on relay; builds off-box): **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)**.

Example container run with SSL:

```bash
docker run -p 80:80 -p 443:443 \
  -e RELAY_CERTBOT_EMAIL="admin@example.com" \
  -e RELAY_DNS_SUBDOMAIN="node1" \
  -e VERCEL_API_TOKEN="your-token" \
  relay-gateway
```

The server will be accessible at `https://iad1.relaygateway.net` with automatic HTTP to HTTPS redirection.

## Developing and deploying a UI for repositories

**Universal client (relay-runtime + md2ast):** see **[`docs/CS_HOST_HELPERS.md`](docs/CS_HOST_HELPERS.md)**, **[`docs/PARADIGM_PARK_LOCAL.md`](docs/PARADIGM_PARK_LOCAL.md)**, **[`docs/RELAY_MD2AST_PLACEMENT.md`](docs/RELAY_MD2AST_PLACEMENT.md)**, and **`../relay-client-web`** (`npm run build:wasm` then `npm run build`).

A. Build/export all HTML/JS/asset files of your UI (e.g., into `template-ui/dist`).
B. Test locally: run **`relay-gateway`** and pass **`--static`** (repeatable) for your UI folder.

```bash
cargo run --release -- serve --repo ./data --static ./template-ui/dist
```

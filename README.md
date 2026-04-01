# <img src="icon.png" width="32" height="32" align="center" /> Relay Server (Rust)

Implements the Relay API over bare Git repositories with static directory serving and detailed error reporting.

## Features

- Serves files from multiple bare Git repositories (`.git` suffix)
- Static file serving from configurable directories
- Automatic hourly git pulls for repository updates
- Detailed 404 error messages showing searched paths, repos, and branches
- JSX/TSX transpilation via hook-transpiler WASM
- Optional HTTPS support with automatic certificate loading
- CORS-enabled; responses may include informational headers (e.g. echo of resolved repo/branch — **not** input channels). **`X-Relay-Branch` is deprecated**; branch selection is **Host-only** — see **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §8–§11.

Install-time **optional capabilities** (Piper TTS, offline Argos translation, npm hook packages) are recorded in **`state/features.json`** and advertised in **`GET /api/config` → `installed_features`** so peers know what each node offers. See **[docs/INSTALL_FEATURES.md](docs/INSTALL_FEATURES.md)**.

## Repository Policy

- Never import or reference files directly from `apps/` other than code in this crate; prefer shared assets and data in
  shared crates.

Endpoints

- OPTIONS / and OPTIONS /* — discovery endpoint returning capabilities, branches, repos, current selections, and branch
  HEAD commits (branchHeads). The **current repo** is derived from the HTTP **`Host`** header (see below).
- GET /{path} — read file from the bare repo and branch implied by **`Host`**: `{repo}.{RELAY_PUBLIC_HOSTNAME}` or
  `{repo}{DELIM}{branch-slug}.{RELAY_PUBLIC_HOSTNAME}` (see **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §8).
  **Default branch** when the host label has **no** delimiter: the bare repo’s symbolic **`HEAD`** (e.g. **`master`** after `git push` if that is the default), else **`main`**. Optional override: **`X-Relay-Branch`** (deprecated; prefer Host branch slug when §8 delimiter is in use). Browsers send **`Host`** automatically for `http://repo.node/...` **if DNS** resolves **`repo.node`** (often a wildcard **`*.your-node-fqdn`** → server IP).
    - If the path resolves to a directory, returns a markdown listing with breadcrumbs and links.
    - If the file/dir is missing, returns 404 with `text/html` body. If `/site/404.md` exists on that branch, it is
      rendered; otherwise a default page plus a parent directory listing is returned. Global and per-directory CSS are
      auto-linked when present.
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

**[`relay-master`](../relay-master)** is the **allowlist** for which Git repositories a node may mirror: each **submodule** supplies **`url`** (clone source) and **`relayRootCommit`** (trusted root; **`scripts/sync-relay-catalog.sh`** verifies it is an ancestor of the branch tip). **`relay-server`** loads policy from **`RELAY_MASTER_CATALOG_PATH`** (path to a **`relay-master` checkout** with **`.gitmodules`**) or, alternatively, from **`RELAY_AUTHORIZED_REPOS_PATH`** (YAML, e.g. emitted by **`sync-relay-catalog.sh`**—never set both). Use **`RELAY_MASTER_TRUST_ROOT`** (40 hex) so startup fails if the checkout is the wrong Git history. See **[`docs/RELAY_TRUST_AND_BOOTSTRAP.md`](./docs/RELAY_TRUST_AND_BOOTSTRAP.md)**. Set **`RELAY_DEFAULT_REPO=relay-master`** for the catalog apex. **User-facing clone URLs** like **`git://relaygateway.net/<repo>.git`** are documented there (routing/DNS, not HTTP redirects).

### Git daemon (`git://`, default port **9418**)

The bundled **`git daemon`** enables **`receive-pack`**: there is **no git transport authentication** (no SSH keys / HTTP basic auth on that path). Optional enforcement (signed commits, custom **`pre-receive`**, etc.) is per repository via **`.relay.yaml`** and **`relay-hook-handler`**.

**Today:** `git daemon` only serves **existing** bare repos under **`RELAY_REPO_PATH`**. It validates the path **before** running **`--access-hook`**, so a hook cannot create a missing bare repo on first connect. Operators must create the directory first—typically **`POST /api/git/ensure-bare/{repo}`** (returns **201** when created)—then **`git push git://…/{repo}.git`**. This is **on by default**; set **`RELAY_GIT_ENSURE_BARE_ENABLED=0`** to disable and create **`*.git`** directories only manually. **`GET /api/config`** includes **`git_ensure_bare_enabled`**.

**Planned:** First **`git push`** should be able to **create** the bare repo when policy allows, without a separate HTTP step. That requires relay code (or another front) to run **before** stock `git receive-pack`—for example a small **`git://` gateway** that parses the requested repo, calls the same provisioning logic as **`ensure-bare`**, then hands off to Git, or a **smart HTTP** git endpoint that can provision and delegate. See **[`docs/RELAY_TRUST_AND_BOOTSTRAP.md`](./docs/RELAY_TRUST_AND_BOOTSTRAP.md)** for how **repository identity** (root commit) should gate creation and updates.

### Git Hooks (`relay-hook-handler`)
The server includes a native `relay-hook-handler` binary that should be symlinked to `hooks/pre-receive` and `hooks/post-receive` in your bare repositories.
- **Branch Protection**: Enforces GPG/SSH signature requirements natively in Rust.
- **Custom Validation**: Dispatches to Node.js scripts (`pre-receive`) with full commit context.
- **Auto-Push**: Synchronizes successful pushes to a list of peer servers automatically.

**Mesh (shipped pieces):** **`RELAY_MASTER_PEER_LIST`** (semicolon-separated FQDNs) is exposed in **`GET /api/config` → `peers`**. **`RELAY_NODE_ROLE`** (`master` \| `edge`) or inference from **`RELAY_PUBLIC_HOSTNAME`** in that list selects behavior: **only masters** fan out **`POST /hooks/mesh-event`** HTTP to peers (**edges** still get Slack if configured). **`RELAY_MASTER_MESH_SECRET`** (masters only): inter-master **`x-relay-mesh-secret`** / **`X-Relay-Peer-Secret`** when set; **`POST /hooks/peer`** on a master with this set accepts **only** that secret (not **`RELAY_PEER_SECRET`**). **`POST /hooks/peer`** on **edges** uses **`RELAY_PEER_SECRET`** (masters poke edges with the same value). See **[`docs/RELAY_MESH_NETWORKING_PLAN.md`](docs/RELAY_MESH_NETWORKING_PLAN.md)**. **mTLS** between masters is a **reverse-proxy** concern, not yet in-process. **Internal** anti–over-ping remains **Slack-independent**; optional Slack: **[`docs/SLACK_MESH_NOTIFY.md`](docs/SLACK_MESH_NOTIFY.md)**.

### Webhooks
- **GitHub**: `POST /hooks/github/{repo}` (repo name without `.git`) — see `.relay.yaml` `server.git.github` for enable/path metadata.
- **Peer notify → pull**: `POST /hooks/peer` with header **`X-Relay-Peer-Secret`** (see mesh section above) and JSON **`{ "repo", "peer", "branch"?, "new_sha"? }`**. If **`RELAY_MASTER_PEER_LIST`** is non-empty, **`peer`** must match one of those hostnames (case-insensitive).

## Configuration and secrets

**`relay.env`** on the server (typically **`/opt/relay/relay.env`**, **`EnvironmentFile`** for systemd) holds trust, mesh, and optional webhook values. **Sanitized template:** **[`relay.env.example`](relay.env.example)**. **Ops policy** (manual install per node, full master peer list, coordinated rotation including webhooks, shared deploy SSH key + optional install check): **[`docs/SECRETS_POLICY.md`](docs/SECRETS_POLICY.md)**. **Longer annotated reference:** **[`docs/examples/relay.env`](docs/examples/relay.env)**. **New node with catalog URL:** fill **`relay.env`** per **[`docs/DEPLOY_LINODE.md`](docs/DEPLOY_LINODE.md)** § “Greenfield checklist” so first boot clones the catalog, syncs submodules, and peer-fetches without extra SSH steps.

## Environment Variables

### Server Configuration
- `RELAY_REPO_PATH`: Root directory containing bare repos (default: `./data`)
- **`RELAY_PUBLIC_HOSTNAME`** or **`RELAY_NODE_FQDN`**: Node FQDN for HTTP routing. Clients must use **`Host: {repo}.{this-value}`** so each bare repo `RELAY_REPO_PATH/{repo}.git` is unambiguous (e.g. node `atlanta1.example.net`, repo host `snesology-library.atlanta1.example.net`). For many repos, use a **wildcard DNS** record `*.{node}` → server IP.
- `RELAY_STATIC_DIR`: Comma-separated static directories to serve (default: none)
- `RELAY_HTTP_PORT`: HTTP port (default: 80)
- `RELAY_HTTPS_PORT`: HTTPS port (default: 443)
- `RELAY_BIND`: Override bind address (format: `host:port`)
- **`RELAY_MASTER_PEER_LIST`**: Semicolon-separated peer FQDNs for **`/api/config`**, mesh bootstrap, and **`POST /hooks/peer`** allowlisting. **`install.sh`** defaults this to **`atlanta3.relaygateway.net;jp-osa.relaygateway.net`** (designated **master** mesh nodes) unless you set **`RELAY_MASTER_PEER_LIST`** or **`DEFAULT_RELAY_MASTER_PEER_LIST`**. If empty after install, **`POST /hooks/peer`** allows any **`peer`** hostname once the shared secret matches.
- **`RELAY_NODE_ROLE`**: Optional **`master`** or **`edge`**. If unset, **master** when **`RELAY_MASTER_PEER_LIST`** is empty (dev), else **master** when **`RELAY_PUBLIC_HOSTNAME`** is listed, otherwise **edge**.
- **`RELAY_MASTER_MESH_SECRET`**: Same value on all **masters**; omit on **edges**. When set on a master, that node accepts **only** this secret for **`POST /hooks/mesh-event`** ingest and for **`POST /hooks/peer`** (inter-master inject). Outbound mesh POSTs to hosts in **`RELAY_MASTER_PEER_LIST`** use this header when set.
- **`RELAY_PEER_SECRET`**: **Edge** **`POST /hooks/peer`**; also fallback mesh-event secret when **`RELAY_MESH_EVENT_SECRET`** unset. On **masters** without **`RELAY_MASTER_MESH_SECRET`**, peer hook still uses this (legacy).
- **`RELAY_GIT_PORT`**: Git daemon port for building **`git://`** fetch URLs in the peer hook (default **9418**).

### Repository Management
- **`RELAY_DEFAULT_REPO`**: Bare repo name (no `.git`) served at the **node apex** when **`Host`** is the node FQDN only — use **`relay-master`** when following the catalog model (**[`docs/RELAY_TRUST_AND_BOOTSTRAP.md`](./docs/RELAY_TRUST_AND_BOOTSTRAP.md)**).
- **`RELAY_MASTER_CATALOG_PATH`**: Filesystem path to a **`relay-master` worktree** (contains **`.gitmodules`**). Builds the same allowlist **`relay-server`** would read from YAML. **Mutually exclusive** with **`RELAY_AUTHORIZED_REPOS_PATH`**.
- **`RELAY_MASTER_TRUST_ROOT`**: Optional but **recommended in production**: 40-character commit id—the **initial (root) commit** of your canonical **`relay-master`** (`git rev-list --max-parents=0 main`). Startup verifies it is an **ancestor** of the checkout **`HEAD`** so a mistaken clone path cannot point at an unrelated repo.
- **`RELAY_MASTER_BRANCH`**: If the catalog checkout is in **detached HEAD**, set this branch name for the synthetic **`relay-master`** policy row (default **`main`**).
- **`RELAY_AUTHORIZED_REPOS_PATH`**: YAML allowlist (legacy / CI); do not set together with **`RELAY_MASTER_CATALOG_PATH`**.
- `RELAY_MASTER_REPO_LIST`: Semicolon-separated list of repos to clone on startup (e.g., `https://github.com/clevertree/relay-template`)
- `DEFAULT_REPOS`: Alternative to RELAY_MASTER_REPO_LIST for Docker entrypoint
- **`RELAY_GIT_ENSURE_BARE_ENABLED`**: When enabled (default: **on** if unset), **`POST /api/git/ensure-bare/:repo`** may create **`RELAY_REPO_PATH/<repo>.git`**. Set to **`0`** / **`false`** / **`no`** to turn off.
- **`GET /api/config` → `startup_catalog_sync`**: JSON from **`state/startup_catalog_sync.json`** (last **`relay-startup-catalog-sync.sh`** run: forge sync ok, optional error tail, peer fetch counts). Omitted if the file is missing. Forge sync failure does **not** block **`relay-server`**; HTTP serves whatever bare repos exist.

### TLS/HTTPS Configuration
- `RELAY_TLS_CERT`: Path to TLS certificate file
- `RELAY_TLS_KEY`: Path to TLS private key file

### ACME Challenge Support
- `RELAY_ACME_DIR`: Directory for ACME HTTP-01 challenges (default: `/var/www/certbot`)

## Headers

- **`Host`**: Selects bare repo and optional branch via **one** DNS label before **`{RELAY_PUBLIC_HOSTNAME}`** — see **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)** §8. Repo name without `.git`; must match `{repo}.git` under `RELAY_REPO_PATH`.
- **`X-Relay-Branch`:** **not supported** (deprecated; removal planned — §11 in same doc). Use **Host** label form only.

## Env

- Repository rules and validation are defined in `.relay.yaml` and implemented via Node.js scripts.
- See [SERVER_HOOKS.md](docs/SERVER_HOOKS.md) for detailed documentation on configuration and hook execution.
- `relay_index.json` is maintained by pre-commit/pre-receive scripts to track metadata changes and build queryable
  indexes.
- Rules are enforced for new commits by the respective hook scripts. Repositories can customize validation by
  implementing these scripts.

Repository Scripts

- `.relay/pre-commit.mjs` — executed by the server during PUT operations to validate file changes before committing.
- `.relay/pre-receive.mjs` — executed during git push operations (if using git-based workflows) to validate commits.
- `.relay/validation.mjs` — optional custom validation logic that can be invoked by pre-commit/pre-receive scripts in a
  sandboxed environment.
- `.relay/lib/*.mjs` — shared utility modules for common validation and index management tasks.

## CLI & Run

### 1. Build
```bash
cargo build --release
```

### 2. Run Server
```bash
cargo run --release -- serve [OPTIONS]
```

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

### 3. Docker

**Build:**
```bash
docker build -t relay-server .
```

**Run:**
```bash
docker run -d \
  -p 8080:8080 \
  -e DEFAULT_REPOS="https://github.com/clevertree/relay-template" \
  -e RELAY_HTTP_PORT=8080 \
  relay-server
```

The entrypoint script will:
1. Clone repositories from `DEFAULT_REPOS` (bare) to `/srv/relay/data/<name>.git`
2. Copy prebuilt static assets from `/srv/relay/prebuilt` to `/srv/relay/www`
3. Start hourly git fetch loop in background
4. Start relay-server on specified port

## CLI & Run

1) Build everything
   cargo build --workspace

2) Run server (ensure .relay/pre-commit.mjs is available in your template repo)
   cargo run --manifest-path apps/server/Cargo.toml -- serve

   Options:
   --repo <PATH>        Path to the bare Git repository (default from RELAY_REPO_PATH or ./data/repo.git)
   --static <DIR>       Additional static directory to serve files from (can repeat)
   --bind <HOST:PORT>   Bind address (default from RELAY_BIND or 0.0.0.0:8088)

3) Create an IPFS hash for a built UI directory and seed it locally
   # This runs `ipfs add -r <dir>` and prints the root CID
   cargo run --manifest-path apps/server/Cargo.toml -- ipfs-add template-ui/dist

4) Try requests
    - Root listing with breadcrumbs:
      curl -i "http://localhost:8080/?branch=main"
    - Put a file (validates via .relay/pre-commit.mjs if present):
      curl -i -X PUT "http://localhost:8080/README.md?branch=main" \
      -H "Content-Type: application/octet-stream" \
      --data-binary @-
      Hello Relay!
      [Ctrl+D]
    - Fetch it:
      curl -i "http://localhost:8080/README.md?branch=main"
    - Query (QUERY method; use **Host** `repo.localhost` and body for branch — **`X-Relay-Branch` deprecated**):
      curl -i -X QUERY "http://localhost:8080" -H "Host: relay-template.localhost" -H "Content-Type: application/json"
      --data '{"branch":"main","filter":{"title":"Inception"}}'

Serving order (GET requests)

1. Static directories: if `--static` paths are provided, the server will first try to serve the file from these
   directories.
2. Git repository: the server reads from the selected bare repo/branch.

Notes:

- The server never serves `.html`, `.htm`, or `.js` files from the Git repository. HTML/JS must come from static
  directories during development.

Logs

- Structured logs are written to stdout and to rolling daily files under `./logs/server.log*`.
- HTTP request/response spans are included (method, path, status, latency).

## Detailed Error Messages

When a file is not found, the server returns a detailed 404 error showing:

```
Not Found

Path: hooks/client/get-client.jsx
Branch: main
Repo: relay-template

File not found in git repository. No hooks/get.mjs found to handle dynamic routing.
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
4. **Nginx Proxy Configuration**: Automatically configures nginx to proxy HTTPS to the relay-server

**Bare metal (Linode tarball):** **`install.sh`** runs the same Vercel DNS flow **first** (FQDN + token, or skip with **`RELAY_SKIP_VERCEL_DNS=1`**). See **[`docs/DEPLOY_LINODE.md`](docs/DEPLOY_LINODE.md)** and **[`docs/DEPLOY_SELF_HOSTED.md`](docs/DEPLOY_SELF_HOSTED.md)** (build/publish tarball without cloud CI). Planned **Next.js + CI + multi-site** layout: **[`docs/DEPLOY_WEB_AND_CI.md`](docs/DEPLOY_WEB_AND_CI.md)**.

Example container run with SSL:

```bash
docker run -p 80:80 -p 443:443 \
  -e RELAY_CERTBOT_EMAIL="admin@example.com" \
  -e RELAY_DNS_SUBDOMAIN="node1" \
  -e VERCEL_API_TOKEN="your-token" \
  relay-server
```

The server will be accessible at `https://node-dfw1.relaynet.online` with automatic HTTP to HTTPS redirection.

Developing and deploying a UI for repositories

A. Build/export all HTML/JS/asset files of your UI (e.g., into `template-ui/dist`).
B. Test locally: run the server and add your UI folder as an additional static directory, then exercise the app.

- Example:
  cargo run --manifest-path apps/server/Cargo.toml -- serve --repo ./data/repo.git --static ./template-ui/dist

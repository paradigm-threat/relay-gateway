# Relay ops (`relay-ops`)

One generic tool for any Relay fleet: **CLI + local UI**. Bootstrap from a **GitLab project URL** (not baked hostnames). The committed topology file is [`ops/fleet.json`](../ops/fleet.json) (`schema: relay.ops.fleet.v1`).

Local fleet brain (scenarios, pending requests, private feed): [`RELAY_BRAIN.md`](RELAY_BRAIN.md). MCP/WebMCP tool catalog: [`RELAY_OPS_MCP.md`](RELAY_OPS_MCP.md).

`./scripts/relay-fleet.sh` is a thin exec wrapper around this binary so existing muscle memory still works.

## Bootstrap

A GitLab URL is enough for **read-only** observer commands (`hosts`, `status`, `health`, `ssl`, `inventory`, `serve`):

```bash
cargo build --release --bin relay-ops
./target/release/relay-ops status --gitlab https://gitlab.example/group/relay-gateway
# from a checkout, git remotes are enough:
./target/release/relay-ops hosts
./scripts/relay-fleet.sh --only=node.example.net status
```

Fetch order for topology:

1. GitLab `ops/fleet.json`
2. Local checkout `ops/fleet.json` (so unpublished topology still works)
3. GitLab `relay.env.example` (`RELAY_MASTER_PEER_LIST` / `RELAY_OPERATOR_FLEET`)
4. Last-good cache
5. Local `relay.env.example`

If any node answers `POST /api/config`, its `peers` list is unioned into the working set. GitLab is **desired** topology, not current IPs. Do not treat forge `public_ip_override` as live.

## Inventory (Admin vs read-only)

`relay-ops inventory` reports local ingredients only (no live SSH):

- Admin: `ssh` on PATH and (`SSH_AUTH_SOCK` or identity file; default `~/.ssh/id_ed25519_relay_gateway` if present)
- DNS buttons: Vercel token
- Spot drift: Spot token
- Checkout: local gateway tree (unlocks update / repair / reclaim)

Observer commands must **not** require an SSH key.

## CLI (fleet + observer)

Global flags (before or with clap long opts): `--gitlab`, `--env-file`, `--only=FQDN`, `--build`, `--allow-dirty`, `--interactive`.

| Command | Needs | Notes |
|---------|--------|--------|
| `hosts` | GitLab URL | Print FQDNs / SSH user / identity |
| `status` | GitLab URL | JSON probes: DNS, TCP, HTTPS `/api/config` + `/api/status` (local repo tips; git:// ls-remote only on old nodes) |
| `health` | GitLab URL | QUERY smoke (+ `--mcp` if checkout has validators) |
| `ssl` | GitLab URL | HTTPS report |
| `inventory` | — | Local capability JSON |
| `serve` | GitLab URL | UI on `127.0.0.1:7780` |
| `ssh-check` | Admin | Parallel `ssh true` |
| `drift` | Admin | Catalog HEAD alignment (report only) |
| `sync` | Admin | On-node `relay-startup-catalog-sync.sh` + peer `git fetch`; when `--peer` is set (or brain auto-sync), **ensure-bare** + fetch for catalog slugs missing on disk. No wipe, no force-push. UI **Synchronize** for drift cells. Brain auto-runs **`catalog_missing`** without approval when execute mode is on. |
| `update` / `deploy-all` | Admin + checkout | Pack optional `--build`; tarball + restart. Pack **refuses a dirty/unpushed tree** unless **`--allow-dirty`**. |
| `repair` | Admin + checkout | Remote `install.sh repair` on an **existing** node |
| `reclaim` | Admin + checkout | Spot **install-only** via `relay-master-deploy.sh` (`--fqdn` `--yes`) |
| `features-set` | Admin + checkout | `--piper` `--npm-pkgs` `--translation` … |
| `reclone-repo REPO` | Admin + checkout | Optional `--peer` |
| `fix-bare-head` | Admin | |
| `bootstrap-ssh` | sshpass + root password | CLI only (not in UI) |
| `fleet-dns` | Vercel | Reconcile ingress A from node DNS; `--dry-run`; `--drop-unhealthy --yes` |
| `point-dns` | Vercel | `--fqdn` `--ip` |
| `reinstall` | Admin | Default = update. Wipe: `--danger-wipe-relay-data --confirm-wipe=FQDN --only=FQDN` |
| `known-hosts-reset` | — | `ssh-keygen -R` |
| `install-service` | — | systemd `--user` or Windows Startup |

Safe auto (serve loop only): drop **dead** IPs from `fleetIngress` A set (never empty the set). Never auto-point node DNS, never auto reclaim, never wipe.

## Spot node replaced (example: iad1)

If DNS still points at a dead IP and Spot gave you a **new empty Ubuntu** (SSH works, no `/opt/relay`), do **not** use Repair — that runs `install.sh repair` on an existing install.

**UI:** tray → Open fleet UI → **Point DNS to Spot IP** (shown only when DNS A ≠ Spot IP) → **Reclaim** (empty Spot with working SSH only) → **Drop dead fleet A**. **Reset known_hosts** is one Admin header button for the whole fleet (FQDN + DNS/Spot IPs) after Spot replacement.

**CLI:**

```bash
./target/release/relay-ops status --only=iad1.relaygateway.net
./target/release/relay-ops known-hosts-reset --fqdn iad1.relaygateway.net --ip 104.239.250.44
./target/release/relay-ops reclaim --fqdn iad1.relaygateway.net --ip 104.239.250.44 --yes
./target/release/relay-ops point-dns --fqdn iad1.relaygateway.net --ip 104.239.250.44
./target/release/relay-ops fleet-dns --drop-unhealthy --yes
```

`point-dns` / `fleet-dns` need `VERCEL_API_TOKEN` in the environment (or `~/.config/relay-ops/config.toml`). Reclaim does not point node DNS by itself if that token is missing.

## Serve (login daemon)

```bash
cp ops/config.toml.example ~/.config/relay-ops/config.toml
# set gitlab_url; optional op:// secrets
./target/release/relay-ops serve --gitlab https://gitlab.example/group/relay-gateway
# UI: http://127.0.0.1:7780
./target/release/relay-ops install-service
```

Bind is loopback-only by default.

UI **Synchronize** = catalog sync + peer fetch (shown only on drifted / `sync` cells). UI **Repair** = remote `install.sh repair` on an **existing** install that is down (hidden for empty VMs, foreign/stale IP, host-key change, or mere drift). UI **Reclaim** = Spot install-only when SSH works and `/opt/relay` is missing (hidden without a Spot IP). Invalid per-cell actions are **omitted**, not greyed out. **Reset known_hosts** is a single Admin header action (`ssh-keygen -R` on user `~/.ssh/known_hosts`, no sudo) for every fleet FQDN + DNS/Spot IP. Each cell shows a **recommendation** and highlights the one primary button that is visible. `bootstrap-ssh` and destructive `reinstall` stay CLI-only.

Each node card shows the **bare repo count** and SHAs from that host’s **`GET`/`POST /api/status`** (`schema_version` ≥ 2: local git tips, no git-daemon; ≥ 3: **IPFS seed** `self.ipfs` / `ipfs_ok`). Operator APIs are the **node FQDN** only (parked Hosts keep `/api/*` as site files). Older nodes fall back to `POST /api/config` plus `git ls-remote --heads` with **4 concurrent connections per host** (under git daemon `--max-connections`, default **64**). **Missing** means a catalog name is not on disk. An `ls-remote` timeout is **not** treated as missing. SHA drift still marks `sync`. Tray lines look like `iad1  up  12 repos` (or `missing` / `drift` when out of sync). **Quit** stops the user service (`systemctl --user stop relay-ops`) or exits a foreground `serve`. **Browse** lists files over **HTTPS** (`GET https://{repo}.{node}/` with **`X-Relay-Tree: 1`**, blob GET for file bodies). Older nodes fall back to `git://` partial clone (`blob:none`). Observer; no SSH.

Background probes are **300s** while the fleet is up (`probe_interval_secs`). If any node is down or DNS-empty (typical right after login, before the resolver is ready), retry every **10s** (`probe_interval_down_secs`): DNS lookup only when names fail, then TCP 22/443/git, and **HTTPS / git-daemon only after 443 is open**. Empty-DNS retries never leave the laptop.

On Linux, `serve` shows a GNOME AppIndicator using the canonical Relay mark (`media/icon.png`; red dot if any node is down). The menu lists each node (up / DOWN, DNS≠Spot, https/git) plus fleet A. Click opens the UI. `install-service` copies the binary to `~/.local/bin/relay-ops`, enables a user systemd unit, and writes `~/.config/autostart/relay-ops.desktop` so it returns at login.

```bash
./target/release/relay-ops install-service
# UI: http://127.0.0.1:7780
```

## Secrets

Optional values in `~/.config/relay-ops/config.toml` may be `op://Vault/item/field` (resolved with `op read`). Also reads `VERCEL_API_TOKEN`, `RXTSPOT_TOKEN`, `GITLAB_TOKEN`, `RELAY_SSH_IDENTITY`, `RELAY_FLEET_ENV_FILE`, and laptop `relay.env`.

Windows: install OpenSSH and Git for Windows for SSH/git. Observer HTTPS/DNS/TCP works without them.

## Deploy pitfalls

Global flags are parsed **before** the subcommand. Safe forms: `./scripts/relay-fleet.sh --build update` and `--build deploy-all` (optional `--only=` before the subcommand). For `update` / `deploy-all` only, a trailing `--build` is also accepted.

If `target/relay-linode-deploy.tgz` already exists and you run `update` / `deploy-all` **without** `--build`, the fleet uploads **whatever is on disk**. After changing gateway Rust, always `--build`, or run `./scripts/pack-linode-deploy.sh` and check the tarball mtime.

**Clean pack (default):** `pack-linode-deploy.sh` refuses dirty or unpushed `relay-gateway` / `relay-runtime` / `relay-client-web` trees. Production path: **commit and push** (or CI `package:linode-tgz`) → **`./scripts/relay-fleet.sh --build deploy-all`**. **`--allow-dirty`** (or `RELAY_PACK_ALLOW_DIRTY=1` on the pack script only) is an explicit override; `deploy-all` does **not** set that env unless the flag is present, and it **unsets** a leftover laptop `RELAY_PACK_ALLOW_DIRTY`. Optional later: fill `relay-master/deploy/relay-gateway.json` and enable `RELAY_RECONCILE_ENABLED` / timer ([`RELAY_RECONCILE_PLAN.md`](./RELAY_RECONCILE_PLAN.md)) — not required for this path.

**First-push provision** is live: `git push git://{node}:9418/{repo}.git` creates the missing bare on **`git-receive-pack`** when **`RELAY_GIT_ENSURE_BARE_ENABLED`** is on and catalog **`publish_origin`** allows this node. HTTP **`POST /api/git/ensure-bare/:repo`** remains for pre-create. Replica skip (`enforce_push_range` on peer-fetch) is the next cut.

Laptop remotes for catalog repos: `./scripts/relay-local-remotes.sh --all` (see `scripts/relay-local-repos.map`).


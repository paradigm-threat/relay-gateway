# Relay ops (`relay-ops`)

One generic tool for any Relay fleet: **CLI + local UI**. Bootstrap from a **GitLab project URL** (not baked hostnames). The committed topology file is [`ops/fleet.json`](../ops/fleet.json) (`schema: relay.ops.fleet.v1`).

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

Global flags (before or with clap long opts): `--gitlab`, `--env-file`, `--only=FQDN`, `--build`, `--interactive`.

| Command | Needs | Notes |
|---------|--------|--------|
| `hosts` | GitLab URL | Print FQDNs / SSH user / identity |
| `status` | GitLab URL | JSON probes: DNS, TCP, HTTPS `/api/config`, git:// |
| `health` | GitLab URL | QUERY smoke (+ `--mcp` if checkout has validators) |
| `ssl` | GitLab URL | HTTPS report |
| `inventory` | — | Local capability JSON |
| `serve` | GitLab URL | UI on `127.0.0.1:7780` |
| `ssh-check` | Admin | Parallel `ssh true` |
| `drift` | Admin | Catalog HEAD alignment |
| `update` / `deploy-all` | Admin + checkout | Pack optional `--build`; tarball + restart |
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

## Serve (login daemon)

```bash
cp ops/config.toml.example ~/.config/relay-ops/config.toml
# set gitlab_url; optional op:// secrets
./target/release/relay-ops serve --gitlab https://gitlab.example/group/relay-gateway
# UI: http://127.0.0.1:7780
./target/release/relay-ops install-service
```

Bind is loopback-only by default.

UI **Repair** = remote `install.sh repair`. UI **Reclaim** = Spot install-only (confirm-gated). `bootstrap-ssh` and destructive `reinstall` stay CLI-only.

On Linux, `serve` shows a GNOME AppIndicator (cube + funnel; red dot if any node is down). Click opens the UI. `install-service` copies the binary to `~/.local/bin/relay-ops`, enables a user systemd unit, and writes `~/.config/autostart/relay-ops.desktop` so it returns at login.

```bash
./target/release/relay-ops install-service
# UI: http://127.0.0.1:7780
```

## Secrets

Optional values in `~/.config/relay-ops/config.toml` may be `op://Vault/item/field` (resolved with `op read`). Also reads `VERCEL_API_TOKEN`, `RXTSPOT_TOKEN`, `GITLAB_TOKEN`, `RELAY_SSH_IDENTITY`, `RELAY_FLEET_ENV_FILE`, and laptop `relay.env`.

Windows: install OpenSSH and Git for Windows for SSH/git. Observer HTTPS/DNS/TCP works without them.

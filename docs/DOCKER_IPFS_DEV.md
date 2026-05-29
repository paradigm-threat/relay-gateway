# Docker IPFS dev stack

Test **relay-gateway** IPFS GET hooks against **thin** bare repos before fleet deploy.

## How relay-gateway uses IPFS

- Resolves `relay-ipfs-v1` stubs / `.relay/assets/*.ipfs` / `assets.manifest.json` in CleverScript (`get.cs`).
- Fetches bytes via **HTTP** `RELAY_IPFS_GATEWAY` (default `https://ipfs.io`), plus disk cache under `RELAY_IPFS_CACHE_ROOT`.
- Does **not** call Kubo API `:5001` directly.

## Image build

The dev compose file uses **`Dockerfile.ipfs-dev-runtime`**: a small Debian image that **mounts** `target/release/relay-gateway` from your host (after `cargo build --release`). Full in-Docker rustc builds need the whole monorepo (`Dockerfile.ipfs-dev`); use the host binary path for day-to-day IPFS testing.

## Recommended: host Kubo + Docker relay

Your machine already pins thin-migration CIDs (`ipfs daemon`).

```bash
# Terminal 1
ipfs daemon

# Terminal 2
cd relay-gateway
./scripts/docker-ipfs-dev.sh up
./scripts/docker-ipfs-dev.sh test
```

- Relay listens on **http://127.0.0.1:18080** (`network_mode: host` on Linux so it can reach Kubo on **127.0.0.1:8080**).
- Requests use **`Host: paradigm-threat-files.localhost`**.
- Allowlist: **`docker-data/ipfs-dev/authorized-repos.yaml`** (not a bind-mounted `relay-master` git tree — avoids libgit2 ownership issues in Docker).
- **`prepare`** overlays IPFS-aware **`get.cs`** from your thin checkout onto the copied bare repo.

## Prepare step

`docker-ipfs-dev-prepare.sh` copies `thin.git` into `docker-data/ipfs-dev/data/` and overlays **`hooks/server/get.cs`** from the thin **checkout** onto bare `main` (via `commit-tree`; checkout branch ref unchanged).

## Bundled Kubo (optional)

If you do not want the host daemon:

```bash
./scripts/docker-ipfs-dev.sh up-bundled
./scripts/docker-ipfs-dev.sh sync-pins bafkreicpntnqc3fqmkoebsk6jnsai6tjc5kuftyhw2r5qxyh7ghsnsuwpq
./scripts/docker-ipfs-dev.sh test
```

Bundled gateway: **http://127.0.0.1:8081/ipfs/{cid}**

## Env overrides

| Variable | Default |
|----------|---------|
| `RELAY_DEV_HTTP_PORT` | `18080` |
| `RELAY_DEV_HOSTNAME` | `localhost` |
| `RELAY_IPFS_GATEWAY` | `http://127.0.0.1:8080` (host network; Kubo must be on localhost) |

## Troubleshooting

| Symptom | Check |
|---------|--------|
| 404 / `ipfs-not-found` | Host `curl -I http://127.0.0.1:8080/ipfs/{cid}`; `ipfs pin ls` |
| Raw JPEG instead of WebP for `?w=` | `get.cs` in bare repo; `./scripts/docker-ipfs-dev.sh prepare` |
| No repo in response | Bare at `docker-data/.../paradigm-threat-files.git`; catalog mount `../relay-master` |
| `X-Relay-Asset-Error` | `docker compose logs relay-gateway` |

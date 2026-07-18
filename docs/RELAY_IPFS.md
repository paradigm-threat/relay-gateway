# Relay IPFS thin repos

Content repos can keep **logical asset paths** (e.g. `media/hero.png`) out of Git while serving them on the site. Git stores **text-only** metadata under **`.relay/assets/{path}.ipfs`** plus optional **`assets.manifest.json`**. Authors keep **real binaries** locally (gitignored).

## Pointer format (`.relay/assets/.../*.ipfs`)

```text
relay-ipfs-v1
bafybei...
image/png
sha256=...
```

## Author workflow

```bash
ipfs daemon   # Kubo on :5001 — required for ingest and pre-commit pin checks
relay-ipfs-ingest --install-hooks   # pre-commit: --ingest-staged then --check-staged
cp ./new-shot.png media/hero.png    # logical path, gitignored
relay-ipfs-ingest media/hero.png    # or rely on pre-commit --ingest-staged after git add
git add assets.manifest.json .relay/assets/
git commit -S
```

Pre-commit (after `--install-hooks`):

1. **`--ingest-staged`** — staged real binary → `ipfs add --pin` + `.relay/assets/{path}.ipfs`; staged legacy inline stub at public path → central stub (reuse CID, hydrate `.relay/local-assets/` from Kubo).
2. **`--check-staged`** — reject raw binaries, inline stubs at public paths, and unpinned CIDs.

Do **not** add a blanket `.relay/` line to `.gitignore` (it hides `.relay/assets/*.ipfs` from Git). Ingest only ignores `.relay/asset-inbox/` and `.relay/local-assets/`.

`relay-ipfs-ingest` does **not** commit the logical-path binary; it stages **`.relay/assets/...`** and **`assets.manifest.json`**. Commit fails if the CID is not pinned locally.

## `resolveIpfsFromGetContext(ctx, props?)`

Opt-in from `hooks/server/get.cs`. Optional second argument configures lookup (defaults shown):

| Prop | Default | Role |
|------|---------|------|
| `sources` | `["centralDir"]` | Ordered: `centralDir`, `inline`, `manifest` |
| `assetsDir` | `".relay/assets"` | Root for per-asset `.ipfs` stubs |
| `ipfsSuffix` | `".ipfs"` | Suffix on mirrored logical path |
| `manifestPath` | `"assets.manifest.json"` | Manifest JSON path when `manifest` is in `sources` |

- **`centralDir`** — read `.relay/assets/media/foo.png.ipfs` for request `media/foo.png`.
- **`inline`** — parse stub at `ctx.relPath` (legacy migrated repos).
- **`manifest`** — map `ctx.relPath` via `assets.manifest.json`.

## `fetchIpfsBytesFromGetContext(ctx, props?)`

Same resolution as **`resolveIpfsFromGetContext`**, but returns **`{ bodyBase64, contentType, cacheKey }`** or **`null`** (no HTTP file wrapper). Use with **`optimizeRasterFromGetContext`** when **`queryWidth(ctx)`** is set — **`@relay/image` does not fetch IPFS**.

## Raster resize (explicit in `get.cs`)

Width comes only from **`queryWidth(ctx)`** (`ctx.query`, default keys **`w`**, **`width`**, **`maxWidth`**). The gateway does **not** inject **`ctx.w`** or auto-resize on **`?w=`**.

**Legacy thin repos** (inline stubs at public paths until re-migrated):

```cs
var ipfsOpts = { sources: ["inline", "manifest"], manifestPath: "assets.manifest.json" };
var maxW = queryWidth(ctx);
if (maxW !== null) {
  var bytes = fetchIpfsBytesFromGetContext(ctx, ipfsOpts);
  var webp = optimizeRasterFromGetContext(ctx, bytes, { width: maxW });
  if (webp !== null) { return webp; }
}
return resolveIpfsFromGetContext(ctx, ipfsOpts);
```

**Target layout** (defaults):

```cs
var file = resolveIpfsFromGetContext(ctx);
```

For catalog `thin_ipfs_assets` repos, **GET** on a logical path **missing from the Git tree** still runs `server.hooks.get` (so central-dir + manifest resolution works).

## Gateway env

| Env | Default |
|-----|---------|
| `RELAY_IPFS_CACHE_ROOT` | `/tmp/ipfs-cache` |
| `RELAY_IPFS_CACHE_MAX_BYTES` | 2 GiB |
| `RELAY_IPFS_GATEWAY` | `https://ipfs.io` |
| `RELAY_IPFS_API` | *(unset)* — Kubo HTTP API for `block/stat` + `routing/findprovs` seed probes |
| `RELAY_IPFS_GATEWAY_ALLOWLIST` | *(unset)* — comma-separated allowed gateway hosts |
| `RELAY_IPFS_GATEWAY_ALLOW_PRIVATE` | `0` — set `1` for `http://127.0.0.1:8080` (local Kubo / sidecar) |
| `RELAY_IPFS_MAX_FETCH_BYTES` | 100 MiB (max per CID fetch) |
| `RELAY_IPFS_FETCH_TIMEOUT` | 30s |
| `RELAY_IPFS_WAIT_MAX` | 120s |
| `RELAY_IPFS_GC_GRACE_DAYS` | 14 |

Gateway URLs must be `http`/`https` without embedded credentials. Private/loopback hosts are rejected unless `RELAY_IPFS_GATEWAY_ALLOW_PRIVATE=1`. CIDs must match `bafy`/`bafk`/`Qm` and cannot contain path separators.

- **GET** assets — hook chooses **`resolveIpfsFromGetContext`** (full file) vs **`fetchIpfsBytesFromGetContext`** + **`optimizeRasterFromGetContext(ctx, bytes, { width })`** when **`queryWidth(ctx)`** is set.
- **GET** `/ipfs/{cid}` — cache-backed route on the gateway node.

### Error / seed status

When a logical asset (or `GET /ipfs/{cid}`) cannot return bytes yet, Relay responds with **JSON** (not a bare empty 404):

| Code | HTTP | Meaning |
|------|------|---------|
| `ipfs-pending` | **425** Too Early | CID known (git pointer) but not on this gateway yet — **retryable** |
| `ipfs-busy` | **503** | Another fetch for this CID is in flight — **retryable** |
| `ipfs-gateway-timeout` | **504** | Upstream gateway timed out — **retryable** |
| `ipfs-gateway-error` | **502** | Upstream error — usually **retryable** |
| `ipfs-not-found` | **410** Gone | No providers (when `RELAY_IPFS_API` probe finds zero) — not retryable |

`Content-Type`: `application/vnd.relay.asset-status+json`

Body schema `relay.asset-status.v1`:

```json
{
  "schema": "relay.asset-status.v1",
  "error": "ipfs-pending",
  "message": "CID not on IPFS gateway yet (still seeding)",
  "cid": "bafy…",
  "path": "book/cover.png",
  "retryable": true,
  "retryAfterSecs": 5,
  "seed": {
    "state": "missing",
    "progressPct": 0,
    "bytesReceived": null,
    "bytesTotal": null,
    "providers": 3,
    "localBlock": false,
    "hint": "not on this gateway yet; retry while peers seed"
  }
}
```

`seed.state`: `cached` | `fetching` | `missing` | `unavailable` | `unknown`.

While a gateway fetch is streaming, `seed.state` is `fetching` and `progressPct` reflects bytes received vs `Content-Length` when known.

**Status-only probe** (no long `wait`): append `?relayAssetStatus=1` or send  
`Accept: application/vnd.relay.asset-status+json`.  
Cached CIDs return **200** with `seed.state: "cached"` / `progressPct: 100`.

Headers: `X-Relay-Asset-Error`, `X-Relay-Asset-Seed-State`, `X-Relay-Asset-Progress` (0–100), `Retry-After`.

Optional Kubo API for richer probes: `RELAY_IPFS_API=http://127.0.0.1:5001` (same SSRF rules as the gateway URL; use `RELAY_IPFS_GATEWAY_ALLOW_PRIVATE=1` for loopback).

### Opt-in UI: `RelayImage` vs `<img>`

- **`<img src={url}>`** — browser behaves normally; a 425 JSON body simply fails to paint. No Relay chrome is injected.
- **`RelayImage`** — site must import and render the component explicitly. It polls `?relayAssetStatus=1`, shows seed/progress copy, retries with backoff, then shows `<img>` when ready (or a failed state with manual Retry).

Canonical CleverScript example (copy into your site’s `hooks/client/components/` and register in `hooks.json`):
`paradigm-threat-web/hooks/client/components/RelayImage.jsx` (see park canary).

```jsx
import RelayImage from 'hooks/client/components/RelayImage.jsx'
// …
<RelayImage src="/book/cover.png" alt="Cover" className="pt-img" />
```

## Migration (operators)

Initial history rewrite (binaries → inline stubs): `relay-ipfs-thin-repo.sh`, `relay-ipfs-migrate-history.py`.

**Move inline stubs to central dir** (remove fake `.png` text at public paths):

```bash
./relay-ipfs-migrate-to-central-dir.py /path/to/bare-or-checkout.git
```

Then `relay-ipfs-verify-thin.sh`, update catalog `relay_root_commit`, `relay-fleet.sh reclone-repo`.

## Cache GC

```bash
RELAY_REPO_PATH=/var/lib/relay/repos RELAY_MASTER_CATALOG_PATH=/path/to/relay-master \
  relay-ipfs-cache-gc [--dry-run]
```

## Catalog

```json
"limits": {
  "allow_binary_files": false,
  "thin_ipfs_assets": true
}
```

Pre-receive rejects raw binaries at public paths; `.relay/assets/**/*.ipfs` text stubs are allowed.

## Client

See `relay-client-web/src/lib/relayAssetFetch.ts` — retries from `X-Relay-Asset-Error`, `?wait=` long poll.

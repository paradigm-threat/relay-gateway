# Relay web / CDN assets repository

**Status:** plan + initial gateway support. Catalog listing and fleet cutover are gated on operator choices below.

**Related:** [`RELAY_EMBED_CLIENT_GET_INTERCEPT.md`](./RELAY_EMBED_CLIENT_GET_INTERCEPT.md), [`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md), [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md), [`pack-linode-deploy.sh`](../scripts/pack-linode-deploy.sh).

## Problem (today)

`relay-client-web` builds to `dist/`. Fleet deploy packs that tree into the Linode tarball as **`/opt/relay/www`** and serves it via **`RELAY_STATIC_DIR`**. Framework paths **`/assets/*`** and **`/wasm/*`** are reserved so repo hooks cannot shadow them.

Consequences:

- UI bundles ship **coupled to the gateway binary** tarball (slow, wrong layer).
- Stable names (`embed.js`, `main.js`) + `max-age=3600` allow **stale or mixed** loads across a deploy.
- No git-signed, reviewable artifact history for the web client itself.

## Goal

A **dedicated catalog git repo** (working name **`relay-web-assets`**) holds **pre-built** web/CDN trees. Developers build on a laptop, **signed-commit**, push to the mesh — same instant-deploy path as content repos. Gateway HTML **pins one commit OID** for the whole page load so a client never mixes files from two tips.

## Repo layout

**This deployment profile is embed + WASM only — no npm React.** A future separate release
line may add a React-containing tree; it must not land on the current `release` tip used by
parked sites.

```text
assets/embed.js
assets/embed.css
assets/chunk-*.js          # only modules reachable from embed.js
wasm/relay_runtime_pkg/…
MANIFEST.json              # profile: embed-no-react + source SHAs
.relay.yaml                # branchRules + allowedKeys (signed pushes)
README.md
```

**Never published here:** `main.js`, `main.css`, React SPA `index.html`, or any chunk only
reachable from the React shell. `publish-web-assets.sh` walks the embed import graph and runs
`assert-embed-no-react.mjs` before commit.

**Do not** commit intermediate source. Builds are **local (or CI off-box)** only — never `npm run build` on the relay VM ([`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)).

## Trust / signing

Same pattern as other hosted catalog repos:

- **`.relay.yaml`** `git.branchRules` with **`requireSigned: true`** on publish branches.
- **`allowedKeys`**: catalog / release signing key(s) (typically the same family as `relay-master` / fleet publishers).
- Catalog entry: **`relay_root_commit`**, **`forge_url`**, **`publish_branch`** (e.g. `main` or `release`).
- Push path: **`git://{node}/relay-web-assets.git`** (daemon), not SSH for content.

Signed tip updates are the **client update channel**: future web clients trust mesh git policy, not an unsigned disk overwrite inside `install.sh update`.

## Atomic load (same commit for one page)

### Why tip URLs alone are unsafe

If HTML references `/assets/embed.js` and that file imports `/assets/chunk-….js`, a tip move between those GETs can serve **entry from commit A** and **chunk from commit B** (missing chunk or wrong code).

### Normative URL scheme

| Role | URL | Blob in assets repo |
|------|-----|---------------------|
| Pinned asset | `/assets/c/{40hex}/{rel}` | `assets/{rel}` |
| Pinned WASM | `/wasm/c/{40hex}/{rel}` | `wasm/{rel}` |
| Dev / legacy disk | `/assets/{rel}`, `/wasm/{rel}` | **`RELAY_STATIC_DIR` only** (not tip) |

`{40hex}` is a full git commit object id of **`relay-web-assets`**.

**HTML generation** (embed shell / SSR document) resolves the assets tip **once** and stamps:

```html
<link rel="stylesheet" href="/assets/c/{oid}/embed.css"/>
<script type="module" src="/assets/c/{oid}/embed.js"></script>
```

### Bundle requirement: relative chunk URLs

esbuild **`publicPath` must be relative** (`./`), so split chunks resolve under the same `/assets/c/{oid}/` directory as the entry. Absolute `/assets/chunk-…` imports defeat pinning.

WASM is loaded with a path derived from the running module URL (same `{oid}`) or from embed config — never a floating tip `/wasm/…` when the JS came from `/assets/c/{oid}/…`.

### Cache headers

| Path | `Cache-Control` |
|------|-----------------|
| `/assets/c/{oid}/…`, `/wasm/c/{oid}/…` | `public, max-age=31536000, immutable` |
| Disk `/assets/…` (dev) | short TTL (e.g. `max-age=3600`) as today |
| Embed **HTML** document | `private, no-store` (already) |

Browsers may keep a pinned commit forever; new HTML points at a new oid.

## Publish workflow (developer)

```bash
# from monorepo
cd relay-client-web && npm run build
../relay-gateway/scripts/publish-web-assets.sh \
  --assets-repo /path/to/relay-web-assets \
  --dist ./dist
# script: rsync dist → worktree, signed commit, push mesh remotes
```

Optional: refuse dirty `relay-client-web` / `relay-runtime` unless `--allow-dirty`.

**Instant deploy:** after mesh fetch of the new tip, the **next** HTML response pins the new oid. No gateway binary redeploy.

## Gateway configuration

| Env | Meaning | Default |
|-----|---------|---------|
| `RELAY_WEB_ASSETS_REPO` | Bare repo slug under `RELAY_REPO_PATH` | `relay-web-assets` (empty disables git assets) |
| `RELAY_WEB_ASSETS_BRANCH` | Branch tip used when stamping HTML | `release` |

Disk **`RELAY_STATIC_DIR=/opt/relay/www`** remains the **fallback** for local/dev and during migration until the catalog repo is live on all nodes.

## Migration

1. Create forge + catalog stub; add submodule / `catalog/repos/relay-web-assets.json`.
2. Publish first signed tree; sync bare on fleet.
3. Ship gateway that serves `/assets/c/…` + stamps HTML (this work).
4. Verify clevertree embed loads only `/assets/c/{oid}/…` + matching `/wasm/c/{oid}/…`.
5. Stop requiring full `www/` in the Linode pack (optional slim pack / empty www). Keep pack assert until cutover complete.
6. Remove long-term dependence on tarball UI bits.

## Out of scope (current `release` profile)

- Third-party CDN hostname (Cloudflare, etc.) — mesh nodes are the CDN.
- **npm React / `main.js` SPA** — not in this CDN deployment; a **separate release** may add React later.
- Signing individual files (git commit signature is enough).
- Automatic GC of unreachable old commit blobs (git gc / ops later).

## Decisions (locked)

| Item | Choice |
|------|--------|
| **Repo name** | `relay-web-assets` |
| **Publish branch** | **`release`** (rebuild/replace history later is OK) |
| **Signing** | Same catalog key as other hosted repos (`relay-catalog-signing`) for now |
| **Pack / disk `www`** | Keep as fallback until catalog + first publish are live on fleet |
| **React** | **Not shipped** in this CDN profile. Separate release when/if React is needed. |

### Atomic loads: commit pin vs release folders

**Already designed (preferred for tip deploys):** HTML stamps one git commit:

`/assets/c/{40hex}/embed.js` → all relative chunks + `/wasm/c/{40hex}/…` share that OID.

A page never mixes tip A and tip B, even if `release` moves mid-load.

**Release folders (also fine, complementary):** keep immutable trees in-repo, e.g.

```text
releases/2026-07-17T20Z/assets/embed.js
releases/2026-07-17T20Z/wasm/…
# or releases/v0.3.1/...
```

HTML would stamp `/assets/r/2026-07-17T20Z/embed.js` (gateway maps that prefix → `releases/…` blobs). Same atomicity rule: **one folder prefix per HTML document**.

| Approach | Pros | Cons |
|----------|------|------|
| **Commit pin `/assets/c/{oid}/`** | Tip stays small; history is git; no duplicate trees | OID not human-friendly; needs tip→OID at HTML time |
| **Folder `releases/{id}/`** | Named, greppable versions; easy “pin app to v0.3.1” | Repo tip grows unless you delete old folders; publish copies a full tree each time |

**Recommendation:** ship **commit-pin first** (already in gateway). Add **folder releases later** if you want stable named pins (e.g. an Android WebView that must not float with `release` tip). Either way, apps must never load unpinned `/assets/embed.js` + floating chunks across a tip move.

### What is published

Only the **WASM embed** client (`embed.js` + its chunks + `wasm/`). The legacy React tabbed shell (`main.js` from `relay-client-web`) stays out of `relay-web-assets` for this deployment. If a React-containing CDN tree is ever required, publish it as a **different release profile/branch/folder**, not mixed into the no-React `release` tip.

## Open (ops only)

1. **Forge URL** — create `gitlab.com/clevertree/relay-web-assets` (or confirm other).
2. Catalog submodule + first signed push to mesh `release`.
3. Set **`RELAY_WEB_ASSETS_REPO=relay-web-assets`** and **`RELAY_WEB_ASSETS_BRANCH=release`** on fleet after bare exists.

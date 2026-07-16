# Server-Side Hooks and Repository Configuration

Relay repositories are governed by a unified configuration file `.relay.yaml` located at the root of the repository. This file defines how the repository behaves on the server, including hook dispatching, branch protection rules, and P2P synchronization.

## Trust, authorized servers, and bootstrap (normative)

**Minimum server launch:** **`RELAY_SERVER_ID`** + allowlist via **`RELAY_MASTER_CATALOG_PATH`** ( **`relay-master` worktree** ) or **`RELAY_AUTHORIZED_REPOS_PATH`** (YAML). Optional **`RELAY_MASTER_TRUST_ROOT`** pins the catalog checkout. See **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**.

**Planned ops overlap:** Installer-managed site deploy config (Caddy/Next) and a **50 MB default-branch working-tree cap** for **non-authorized** repos are described in **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)**; per-repo **`.relay.yaml`** remains the source of truth for hook/trust behavior **except** paths/files owned solely by **`install.sh` / `update`** (to be listed when implemented).

| Concern | Mechanism |
|---------|-----------|
| **Pull validation** | Only repos listed in **`authorized-repos.yaml`**; after fetch, **`anchor_commit`** must be ancestor of branch tip. Rows are normally emitted from **`relay-master`** (**`relayRootCommit`**). |
| **Push / anti-squat (whitelisted names)** | **`anchor_commit`** / **`relayRootCommit`** must be recorded **before** the bare repo exists; **`pre-receive`** should reject pushes whose branch tips do not contain that anchor (same ancestry rule as pull) — **planned**; see **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**. |
| **Branch names vs host slugs** | Planned: **`pre-receive`** rejects **`refs/heads/*`** updates that violate **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §9 (non-empty canonical slug; **unique** slug across branches) so **`Host`**-based branch selection cannot collide. |
| Who may push **content** | Signed commits + **`allowedKeys`** / **`allowedKeyFingerprints`**; server hooks validate every change. |
| Which **Relay nodes** may participate | **`git.relayTrust.authorizedServerIds`**; **`autoPush.originList`** aligned with that list. |
| New nodes | **`RELAY_SERVER_ID`** + same **`relay-master`** revision → **`sync-relay-catalog.sh --write-authorized-repos`**. **`relay-bootstrap.sh`** is optional for extra dev clones, not the catalog allowlist. |

Until **`allowedKeys` / fingerprint matching** is fully enforced in Rust, combine **`requireSigned: true`** with strict firewall rules on **9418**.

## OPTIONS default (client GET hook)

`OPTIONS /` merges `.relay.yaml` from the selected bare repo, then **`relay-gateway` fills `client.hooks.get.path`** when it is missing or empty. The universal default is repo-relative **`hooks/client/get.jsx`** (HTTP: **`/hooks/client/get.jsx`**). Repositories such as Paradigm Threat **files** and **timeline** ship this file so the Relay web client can render the repo shell without mandatory config.

## Configuration Schema (`.relay.yaml`)

```yaml
name: "My Repository"
version: "1.0.0"
description: "A description of the repository"

# Client-side JSX hook mapping (optional — see “OPTIONS default” below)
client:
  hooks:
    get:
      path: hooks/client/get.jsx
    query:
      path: hooks/client/query.jsx

# Server-side CleverScript hooks (.cs only; executed in-process)
server:
  hooks:
    get:
      path: hooks/server/get.cs
    pre-commit:
      path: hooks/server/pre-commit.cs
    pre-receive:
      path: hooks/server/pre-receive.cs
    index:
      path: hooks/server/index.cs
    query:
      path: hooks/server/query.cs   # thin router → search plugin
    mcp:
      path: hooks/server/mcp.cs     # enables POST /api/mcp/:repo
  search:
    plugin: sqlite                  # default; optional "redb"

# Git-level infrastructure settings (Rust-enforced)
git:
  # P2P Synchronization between peers
  autoPush:
    branches: [ "main" ] # Branches to sync on successful push
    originList:
      - "peer1.relay.online"
      - "peer2.relay.online"
    debounceSeconds: 2
  
  # Native Branch Protection Rules
  branchRules:
    default:
      requireSigned: true
      allowedKeys: [ ".ssh/*" ]
    branches:
      - name: main
        rule:
          requireSigned: true
          allowedKeys: [ ".ssh/admin.pub" ]
      - name: public
        rule:
          allowUnsigned: true

  # GitHub Integration
  github:
    enabled: true
    path: "/hooks/github/{repo}" # Documentary; server route is POST `/hooks/github/:repo`
    events: [ "push" ]
```

## Hook Flow

### 1. Pre-Commit Hook (Server `PUT`)
When a file is uploaded via the Relay `PUT` API:
1.  The server checks `.relay.yaml` for `server.hooks.pre-commit`.
2.  If found, it loads the CleverScript file (must end with `.cs`) and runs its default export with a JSON `context` (`old_commit`, `new_commit`, `branch`, `files`, `is_verified`, etc.).
3.  Git APIs are available only via **`import { readFile, readFileBase64 } from '@relay/git'`** (reads from the **new** commit on the hook branch). There is **no** global `Relay`.
4.  If the runtime throws or the hook returns `false` / `{ ok: false }`, the commit is rejected.

### 2. Pre-Receive Hook (`git push`)
When a commit is pushed via the Git protocol:
1.  The native `relay-hook-handler` binary is triggered.
2.  It reads `.relay.yaml` from the **new** commit being pushed.
3.  **Native Rules**: It enforces `branchRules` (e.g., signature verification) natively in Rust.
4.  It then runs the CleverScript `pre-receive` hook if configured (same contract as pre-commit).
5.  If any step fails, the push is rejected.

### Index hook (`server.hooks.index`)
When QUERY (or MCP search) needs an up-to-date index, the gateway runs **Rust** indexing of `data/**/meta.yaml` into `index.db.json`, syncs **SQLite** `index.db` (default search plugin) and legacy **redb**, ingests catalog `index.json` / `data/events.json` when present, then optionally runs `server.hooks.index` CleverScript for thin extra logic. Paths must be `.cs`. See [`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md).

### QUERY hook (`server.hooks.query`)
Thin CleverScript that **`import { search } from '@relay/search/sqlite'`** (or redb) and returns `search(ctx)`. Do not implement corpus scan/rank in CS. Normative details: [`QUERY_CLEVERSCRIPT.md`](./QUERY_CLEVERSCRIPT.md).

### MCP hook (`server.hooks.mcp`)
When configured (and/or `mcp.yaml` is present), **`POST /api/mcp/:repo`** serves Streamable HTTP JSON-RPC. `search_site` calls the same QUERY plugin path. See [`RELAY_MCP.md`](./RELAY_MCP.md).

### Dynamic GET (`server.hooks.get`)

**Product requirements (normative for this gateway):**

1. **Heavy work stays in Rust.** Raster decode, resize, WebP encoding, IPFS fetch, and disk caches are implemented in the gateway binary, not duplicated in CleverScript. Hooks call **`queryWidth(ctx)`** then **`optimizeRasterFromGetContext(ctx, bytes?, { width })`** from **`@relay/image`** (bytes from Git **`ctx`** or from **`fetchIpfsBytesFromGetContext`**); **`resolveIpfsFromGetContext(ctx)`** from **`@relay/ipfs`** for full-file thin-repo assets (omit IPFS imports to disable). **`toWebp`** remains for low-level use.
2. **`next()` is the built-in file handler.** For GET hooks only, the runtime injects a second parameter: **`export default function run(ctx, next)`**. Calling **`return next()`** runs the same Rust path as the relay template: read **`ctx.relPath`** from the repo at the request branch, choose UTF‑8 vs base64 by content type, or return a **404** JSON body (with a short message for readers plus a debugging hint) if the blob is missing. **Do not** blanket `try/catch` the whole hook to fake 404s — use **`next()`** for “file not in tree” semantics; let uncaught script errors surface (see below).
3. **Uncaught CleverScript errors → HTTP 500 with full detail.** On hook failure, the gateway responds with **`application/json; charset=utf-8`**, status **500**, including a layperson **`summaryForReaders`**, the full **`technicalReport`** (`anyhow` `{:#}` chain — **not** censored in production), structured **`cleverScript`** when the root cause is a **`RuntimeError`**, and **`rustBacktrace`** when a Rust backtrace was captured (e.g. **`RUST_BACKTRACE=1`**). The same contract applies to the **raster + `?w=`** path when the hook fails: the gateway does **not** fall back to serving the raw image after a hook error.
4. **HTML site shell + SSR are hook-routed.** For markdown blobs when **`Accept`** prefers **`text/html`** or **`?embed=1`**, the gateway runs this hook with **`embedMarkdownRequest: true`**. Repo-host **`/`** runs the hook with **`siteShellRequest: true`** (before the gateway SPA `index.html`). A site opts into SSR by calling **`ssrClientHookFromGetContext`** from **`@relay/ssr`**. The helper runs the client GET hook, emits a hydration document, and returns **`null`** on failure. If the hook returns no HTML: **`?embed=1`** falls back to the empty CSR embed shell; Accept-only falls through to static **`@relay/md`** HTML when applicable. Merely configuring `client.hooks.get` never enables SSR. Client hydration must **not** wipe SSR markup on failure — show a light error panel and keep the server page.

If a path is missing from the Git tree, the gateway may run `server.hooks.get` CleverScript. The default export receives a JSON **`ctx`** including (when applicable): **`relPath`**, **`branch`**, **`repo`**, **`query`** (string map of query params — use **`queryWidth(ctx)`** for resize width), **`requestHeaders`** (sanitized copy of the request — sensitive names like **`Cookie`** / **`Authorization`** are omitted), **`contentType`**, **`bodyBase64`** (resolved blob when on-tree), **`commitSha`**, **`blobOid`**.

**Desktop local mode:** when the gateway runs embedded in **`relay-client-desktop --repo`**, GET resolution tries the **live working tree** (uncommitted + gitignored) before branch HEAD. That policy is implemented in Rust ([**`DESKTOP_LOCAL_MODE.md`**](DESKTOP_LOCAL_MODE.md)); hooks stay unchanged.

**Image pipeline:** **`queryWidth(ctx, { keys? })`** reads max width from **`ctx.query`** (default keys **`w`**, **`width`**, **`maxWidth`**). **`optimizeRasterFromGetContext(ctx, bytes?, { width })`** requires explicit **`width`**; optional **`bytes`** from **`fetchIpfsBytesFromGetContext`**, else reads the Git blob from **`ctx`**. Returns **`null`** when not a raster or no body. Optional: **`RELAY_IMAGE_CACHE_ROOT`**, **`RELAY_IMAGE_CACHE_MAX_BYTES`**.

**IPFS pipeline (opt-in):** **`fetchIpfsBytesFromGetContext(ctx, props?)`** and **`resolveIpfsFromGetContext(ctx, props?)`** — same **`props`** (`sources`, `assetsDir`, `manifestPath`, …). Default source: **`.relay/assets/{relPath}.ipfs`**. Thin catalog repos invoke the GET hook even when **`ctx.relPath`** is absent from the Git tree. See [**`RELAY_IPFS.md`**](RELAY_IPFS.md).

**SSR pipeline (opt-in):** **`ssrClientHookFromGetContext(ctx, { clientHookPath?, assetBaseUrl? })`** from **`@relay/ssr`**. Calling it is the opt-in boundary; the gateway never runs client-hook SSR automatically. Typical routing:

```js
import { ssrClientHookFromGetContext } from '@relay/ssr';

function wantsSiteShell(ctx) {
  try { if (ctx.embedMarkdownRequest === true) return true; } catch (e) {}
  try { if (ctx.siteShellRequest === true) return true; } catch (e) {}
  return false;
}

if (wantsSiteShell(ctx)) {
  var ssr = ssrClientHookFromGetContext(ctx);
  if (ssr !== null) { return ssr; }
}
```

**CleverScript control flow:** **`return`** inside **`if` / `for` / `while`** in the default-export **`run`** body exits **`run`** (relay-runtime propagates early returns from blocks).

**Gateway binary vs hooks:** Raster resize via **`bodyBase64`** + **`optimizeRasterFromGetContext`** runs on a **Rust** path inside **`relay-gateway`**; **`cargo test`** can pass while production still serves **raw** **`image/jpeg`** if masters run an **old** binary or a deploy reused a **stale** **`relay-linode-deploy.tgz`**. After upgrading **`relay-gateway`**, verify with **`curl -sSI -H 'Host: …' 'https://…/some.jpg?w=400'`** → **`content-type: image/webp`** (see **`docs/RELAY_FLEET_CONSOLE.md`**).

**Migration:** Replace `Relay.git.*` with **`import { … } from '@relay/git'`**, and legacy image helpers with **`@relay/image`** as above. The global **`Relay`** is always **`null`** in server CleverScript.

**Response shape** (JSON returned from the hook’s default export; HTTP mapping lives in **`relay-gateway/src/git/get_hook_http.rs`**):

- **`kind: "file"`** — required: `contentType`, and either **`bodyUtf8`** or **`bodyBase64`**. Optional: **`status`** (200–599, default 200), **`headers`** (object: header names → string or number values).
- **`kind: "dir"`** — body is the JSON value itself; optional **`status`** / **`headers`**.

The gateway always sets **`X-Relay-Branch`** and **`X-Relay-Repo`** after your headers; values in `headers` for those names are ignored.

Valid module specifiers: **`@relay/git`**, **`@relay/search/sqlite`**, **`@relay/search/redb`**, and (when the host allows GET native modules) **`@relay/image`**, **`@relay/ipfs`**, **`@relay/md`**, **`@relay/ssr`**. Anything else fails at load time.

**Node.js / `RelayHost.mjs` is not supported.**

## Security notes and hardening checklist

CleverScript hooks run **in-process** on the gateway with a deliberately small virtual module surface (**`@relay/git`**, **`@relay/search/*`**, plus **`@relay/image`** / **`@relay/ipfs`** / **`@relay/md`** / **`@relay/ssr`** on GET paths when enabled). They are still **untrusted repo code** relative to the host. Recommended follow-ups:

1. **CPU and wall-clock limits** — `run_query_script` has no timeout today; a malicious or buggy hook can pin a worker thread. Add a per-hook **instruction budget** or **wall-clock timeout** (with process isolation if you need stronger guarantees).
2. **Memory caps for images** — **`optimizeRasterFromGetContext`** / **`toWebp`** from **`@relay/image`** decode full rasters in memory. Enforce **max input bytes** (from `bodyBase64` decoded length), **max width/height** after decode, and **max output WebP size** before returning bytes; optional **disk cache** size is bounded by **`RELAY_IMAGE_CACHE_MAX_BYTES`** (see Dynamic GET above).
3. **Rate limiting** — Dynamic GET and QUERY paths that run CleverScript should share **per-repo / per-IP** limits so enumeration cannot turn into a DoS.
4. **Error payloads** — By design, failed GET hooks return HTTP **500** JSON with **`technicalReport`** and optional Rust backtrace text enabled via **`RUST_BACKTRACE`**; ops teams treat this like any other internal error payload (avoid logging secrets from client requests elsewhere).
5. **`@relay/git` readFile scope** — Hooks can read **any** path visible in the commit (including `.relay.yaml`, secrets committed by mistake, other branches’ paths if misconfigured). Document **least-privilege** repo layout; consider an optional **path prefix allowlist** in `.relay.yaml` for `readFile`.
6. **Fail closed on migration** — Repos still pointing `server.hooks.*` at **`.mjs`** get a hard error; monitor logs for that string after upgrades.
7. **Observability** — Log **hook name**, **repo**, **branch**, **duration**, and **outcome** (no full `bodyBase64` in logs).

### 3. Post-Receive Hook (Synchronization)
After a successful push:
1.  The `relay-hook-handler` checks for `git.autoPush`.
2.  It automatically pushes the updated branch to all listed peers.
3.  Circular sync is avoided using the `RELAY_SYNC_IN_PROGRESS` environment variable.

## Policy layering and performance

**Normative detail:** **[`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md)**.

| Tier | Role |
|------|------|
| **Rust** | `branchRules`, signatures, PUT infra, hook dispatch, image/IPFS caches |
| **`.relay.yaml`** | Hook paths, `branchRules`, `autoPush`, `relayTrust` |
| **CleverScript hooks** | Repo **content policy** only (`ctx.files` on write paths) |

**Caching (gateway):** parsed `.relay.yaml` and hook `.cs` source are cached in-process by `(repo_path, commit_OID)` — see **`RELAY_POLICY_AND_PERFORMANCE.md`**. Disable with **`RELAY_DISABLE_CONFIG_CACHE=1`**.

## Shared validation

Point **`server.hooks.pre-commit`** and **`server.hooks.pre-receive`** at the **same** CleverScript file when both gates should share rules (e.g. `hooks/server/validation.cs`). Server hooks do not support repo-relative `import` between `.cs` files.

Alternatively, put shared logic under `.relay/validation.cs` and load it with **`readFile` from `@relay/git`** inside a thin wrapper — only if you need separate entrypoint files.

## GitHub Webhooks

The server provides a native endpoint for GitHub push webhooks. When enabled in `.relay.yaml`:
1.  Configure GitHub to POST to **`https://{your-server}/hooks/github/{repo}`** where **`{repo}`** is the bare directory name without `.git` (same name as the first label of your per-repo HTTP host if you use **`{repo}.{RELAY_PUBLIC_HOSTNAME}`**).
2.  Signature verification is not wired in this path yet; treat the URL as secret or restrict by network policy.

## Testing Hooks

Use a real `git push` against a test bare repo with **`relay-hook-handler`** installed, or exercise **`PUT`** against the HTTP API. CleverScript hooks share the same JSON `context` shape as documented above.

**GET / `server.hooks.get`:** from **`relay-gateway/`**, run **`cargo test`** — see **`src/git/clever_hooks.rs`** and **`src/git/get_hook_http.rs`** for coverage of **`run(ctx, next)`**, native **`next()`** structured 404 payloads, **`optimizeRasterFromGetContext`**, and HTTP 500 hook failure responses.

### Docker Peer Verification
To test the full "Push -> Protect -> Sync" flow:
1.  Ensure you have at least two peer containers running.
2.  Configure `.relay.yaml` with the peer's hostname in `originList`.
3.  Push a commit and check the logs of both peers:
    ```bash
    docker logs relay-peer-1
    docker exec relay-peer-2 git rev-parse HEAD
    ```
3.  The server automatically performs a `git fetch` and update for the target repository.

# Relay Server Launch & Test Plan

**Target window:** March 18–31, 2025 (Linode $100 credit expires end of month)  
**Scope:** relay-server, SNESology + paradigm-threat preset staging, multi-repo rendering, dynamic runtime libraries (songwalker, Piper TTS), multi-node staging, stress tests, auth & repo rules

**Trust & provisioning:** **Min launch:** **`RELAY_SERVER_ID`** + **`RELAY_AUTHORIZED_REPOS_PATH`** (per-repo **`anchor_commit`**). See **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**, **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)**, **[`examples/authorized-repos.yaml`](./examples/authorized-repos.yaml)**. Omit **`RELAY_AUTHORIZED_REPOS_PATH`** only for local dev.

---

## Part 0: Multi-repo rendering & repo selection (product spec)

When multiple bare repos are deployed on one server, **GET/HEAD/OPTIONS and client-hook rendering** must resolve which repo to use without stealing URL query parameters from user applications.

### Resolution order (strict priority) — **Host only** (no `X-Relay-Branch`)

**Normative:** **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §8 (repo + optional delimiter + branch slug), §7 (default repo on node apex), §9 (push-time slug rules), §11 (remove **`X-Relay-Branch`** everywhere).

1. **`Host` label** before **`{RELAY_PUBLIC_HOSTNAME}`**  
   Either **`{repo}`** (default branch) or **`{repo}{DELIM}{branch-slug}`** (non-default branch). **No** **`X-Relay-Repo`** / **`X-Relay-Branch`** — avoids conflicting protocol inputs.

2. **Node apex** (`Host` = node FQDN only)  
   **Default repo** + default branch — **`RELAY_DEFAULT_REPO`** or first in bootstrap list until that env exists (**§7**).

3. **Query string `?repo=` / `?branch=`**  
   **Deprecated** for end-user flows; reserve for M2M only if kept temporarily. Prefer **Host** encoding.

4. **`QUERY` method**  
   Branch / **`all`** → **request body**, not **`X-Relay-Branch`** (**§9** in DEPLOY_WEB_AND_CI).

### Implementation notes for relay-server

- **Rollout:** implement **Host** parsing (§8), then remove **`branch_from` header** usage; migrate **QUERY** body; update clients (**§11**).
- Add explicit **`RELAY_DEFAULT_REPO`** (repo name without `.git`) so “first in list” is not the only story.
- **`post_git_pull` / GitHub webhook:** may still accept `?repo=` for **machine-to-machine** calls where needed.
- **OPTIONS** responses should list all repos and echo **Host-derived** repo/branch resolution.

---

## Part 1: Codebase Analysis & Recommendations

### 1.1 relay-server Architecture Summary

| Component | Tech | Status |
|-----------|------|--------|
| HTTP/HTTPS | Axum 0.7 | ✅ Solid |
| Git ops | libgit2 (git2 crate) | ✅ Solid |
| Hooks | Node.js + RelayHost.mjs sandbox | ⚠️ Partial sandbox |
| Transpile | hook-transpiler WASM | ✅ Working |
| Deployment | Docker, K8s (terraform/rackspace-spot) | ✅ Working |

### 1.2 Code & Stack Recommendations

**High priority:**
- **Part 0 repo resolution:** `RELAY_DEFAULT_REPO`; **Host-only** repo + branch (**[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §8); **`?repo=` only for admin/M2M** if retained (never primary app UX — see Part 0).
- **Fix `post_git_pull` multi-repo:** Resolve repo via header or `?repo=` for ops; fetch `{repo_path}/{repo}.git`.
- **Implement `allowedKeys` enforcement**: `branchRules.allowedKeys` is parsed but never enforced. Add SSH public key verification against the commit signer.
- **Add GitHub webhook signature verification**: `post_github_hook` accepts any POST. Use `X-Hub-Signature-256` and `GITHUB_WEBHOOK_SECRET`.
- **OpenAPI spec**: Replace placeholder with real endpoints for `/api/config`, `/git-pull`, `/hooks/github`, `/transpile`, and path-based GET/PUT/DELETE/QUERY.

**Medium priority:**
- **Concurrent indexing wait**: `indexing.rs` notes "better to wait" for concurrent indexing; implement a simple mutex or queue.
- **Align docs with code**: `SERVER_HOOKS.md` says `/api/git/webhook/github` but implementation uses `/hooks/github`.
- **Extend `/api/config`**: `services`, `dynamicModules`, `piperVoices`, `defaultRepo`, `repos` (supersedes a separate `/api/services`).

**Stack improvements:**
- Consider `tower` rate limiting for stress-test resilience.
- Add structured health checks (repo accessibility, hook execution) beyond `/api/config`.
- Log request IDs for traceability under load.

### 1.3 SNESology & paradigm-threat preset repos (primary staging)

**First staging repos** (before generic relay-template demos):

| Repo role | Content |
|-----------|---------|
| **SNESology library** | Presets, instruments, patterns; interface and software for **creating and playing** SNES-style songs. |
| **Paradigm Threat preset libraries** | Shared preset packs and themed content aligned with Paradigm Threat product lines. |

These repos are the **canonical staging targets** for Linode: multi-repo servers clone SNESology + paradigm-threat preset repos; **default repo** may be SNESology web shell or a thin router repo that sets expectations via headers for secondary repos.

**relay-template** remains useful for generic protocol tests (hooks, validation) but is **not** the primary user-facing staging content.

---

### 1.4 Dynamic runtime libraries (recommendation)

**Goal:** Some nodes host preset-library repos that need **optional** server-side/runtime packages (npm and native tooling) so in-repo applications can load shared code **without vendoring everything into Git**.

**Recommended architecture: layered “node extensions”**

| Layer | What | Who loads it |
|-------|------|--------------|
| **A. Server extension directory** | `RELAY_NODE_PATH` or `/srv/relay/node_extensions` — `npm install` at deploy time (e.g. `songwalker-js`, shared UI kit). Read-only at runtime. | Transpile/hook pipeline or a small **import map** / bundle step that resolves `songwalker-js` from this path when building/serving web assets. |
| **B. In-repo app** | App code in Git imports `songwalker-js` by package name; server resolves against (A) when `RELAY_DYNAMIC_MODULES` includes that package. | Web: ESM import maps or server-injected preambles. Native: see (C). |
| **C. songwalker-core (native)** | **Not** loaded from npm on device — **embedded** in Android / iOS / desktop shells (relay-client-android, relay-client-web wrapper, desktop). Same API surface as documented for songwalker-js where applicable. | App bundles link against songwalker-core; repos reference **API contracts** and optional WASM/assets delivered OTA from relay repos. |
| **D. Piper TTS (full deployment)** | Optional server feature: Piper binary + voice models on disk; HTTP API for synthesis. See Part 5.1. | Nodes with `RELAY_SERVICES=piper-tts` expose `/api/tts` (local Piper) instead of or in addition to remote TTS. |

**Principles**

- **Preset-library nodes** opt in via env: `RELAY_ENABLE_NODE_EXTENSIONS=1`, `RELAY_NODE_EXTENSIONS_PACKAGES=songwalker-js,@scope/foo`.
- **Isolation:** Extensions run in the same trust boundary as today’s Node hooks; long term, run extension resolves in a chroot or separate user with read-only FS except `.relay_data`.
- **Reproducibility:** Lockfile (`package-lock.json` or pnpm-lock) checked into a **server config repo** or baked into Docker image layer — not `npm install` on every request.
- **Clients without extensions:** Web client falls back to CDN or bundled subset of songwalker-js if server does not advertise `songwalker-js` in `/api/config.dynamicModules`.

**Android / iOS / desktop**

- **songwalker-core** ships inside the native app binary (or as a versioned native SDK). Repos declare **minimum songwalker-core version** in `.relay.yaml`; client refuses or warns if mismatch.
- OTA content from relay repos: presets, samples, scripts — not the core engine unless you add optional downloadable native modules (higher complexity).

---

### 1.5 relay-template (secondary)

- Remove debug comments in `get-client.jsx` (e.g. "Test change Tue Jan 6...").
- Add `query` hook path to `.relay.yaml` if `query-client.jsx` is used.
- Ensure `relay_index.json` vs `.relay_data/branches/` layout is documented; template uses flat index.
- Add integration test for `pre-commit` (PUT flow) in addition to `pre-receive`.

---

## Part 2: Risks & Unfinished Code

### 2.1 Security Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| PUT/DELETE unauthenticated | High | Add optional API key or token auth; document as "trusted network" deployment |
| GitHub webhook unverified | Medium | Implement `X-Hub-Signature-256` verification |
| `allowedKeys` not enforced | Medium | Implement key whitelist check in `enforce_branch_rules` |
| Hook sandbox escape | Medium | REPODB_PLAN: remove `fs`, `path`, `child_process` from RelayHost; use Relay.fs only |

### 2.2 Unfinished / Incomplete Features

| Feature | Location | Status |
|---------|----------|--------|
| Auto-sync (fetch from origin) | `docker-entrypoint.sh` | Hourly loop only; no webhook-triggered fetch |
| Auto-push (P2P sync) | `relay-hook-handler` | ✅ Implemented; uses `git push --force` to originList |
| `allowedKeys` enforcement | `relay-hook-handler` | ❌ Parsed, not enforced |
| Per-branch index.db | REPODB_PLAN | Planned; current uses `relay_index.json` |
| Relay.fs sandbox | REPODB_PLAN | Planned; hooks still have full Node access |
| git-pull multi-repo | `post_git_pull` | ❌ Assumes single repo |
| Default repo + **Host** label (repo + optional branch slug) | `helpers.rs` | Partial: single-label repo + `X-Relay-Branch` today; needs §8 parsing + **`RELAY_DEFAULT_REPO`**; remove header (**[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §11) |
| Server capabilities API | N/A | ❌ Not implemented |
| Dynamic node extensions + Piper | N/A | ❌ Planned (this doc) |

### 2.3 Auto-Sync Git Feature: Current vs Desired

**Current behavior:**
1. **Fetch from origin (Docker):** Hourly `git fetch` for each repo in `RELAY_MASTER_REPO_LIST`.
2. **Push to peers (post-receive):** After successful push, `handle_auto_push` pushes to `originList` in `.relay.yaml`.
3. **GitHub webhook:** Fetches from origin when `?repo=` is provided; no signature verification.

**Gaps:**
- No **bidirectional sync** between peers: A receives from B, but B doesn't auto-fetch from A. Each peer must be pushed to directly or via GitHub.
- **Hourly** is slow for rapid iteration; webhook is faster but GitHub-specific.
- **Git daemon** runs with `--enable=receive-pack`; peers push via `git://` but need to be reachable. Linode nodes behind NAT/firewall need proper port exposure.

**Desired auto-sync flow:**
1. **Primary source:** GitHub (or similar) as canonical. Webhook triggers immediate fetch.
2. **P2P mesh:** When server A receives a push (from any source), it auto-pushes to peers B, C. When B receives, it could optionally notify A/C to fetch—but that risks loops. Simpler: each peer pulls from a designated "upstream" on a schedule or webhook.
3. **Peer webhook:** Add `POST /hooks/peer` that accepts a push notification from a trusted peer and triggers fetch from that peer. Secured by shared secret.

---

## Part 3: Auto-Sync Git Completion Plan

### Phase A: Fix existing (1–2 days)

1. **Repo resolution (Part 0):** `RELAY_DEFAULT_REPO`; header-first for app traffic; query string admin-only fallback.
2. **Fix `post_git_pull`** for multi-repo: resolve repo (header or `?repo=` for admin); fetch `origin` for `{repo_path}/{repo}.git`.
3. **Add GitHub webhook signature verification** using `GITHUB_WEBHOOK_SECRET`.
4. **Reduce fetch interval** (optional): Add `RELAY_FETCH_INTERVAL_SEC` (default 3600) for faster sync during testing.

### Phase B: Peer-triggered fetch (2–3 days)

1. **Add `POST /hooks/peer`**:
   - Body: `{ "repo": "relay-template", "branch": "main", "peer": "node2.example.com" }`
   - Header: `X-Relay-Peer-Secret: <shared_secret>`
   - If valid, run `git fetch` from `git://{peer}/{repo}.git` for that repo.
2. **Extend `handle_auto_push`** (optional): After pushing to peer, send HTTP POST to `https://{peer}/hooks/peer` to trigger immediate fetch on the peer. Requires `RELAY_PEER_SECRET` and peer URL list.

### Phase C: SNESology + paradigm-threat preset repos

1. Add both repos to `RELAY_MASTER_REPO_LIST` (order defines clone list; set `RELAY_DEFAULT_REPO` to the desired landing repo).
2. Update `.relay.yaml` `originList` on each repo with Linode node hostnames/IPs.
3. `RELAY_MASTER_PEER_LIST` + `RELAY_PEER_SECRET` on all nodes.

---

## Part 4: Linode Staging Plan

### 4.1 Budget & Instance Sizing

- **Credit:** $100, expires end of March 2025.
- **Suggested instances:** 3× Nanode 1GB ($5/mo) or 2× Shared 2GB ($12/mo) for ~$15–36/mo. Plenty of headroom.
- **Regions:** Pick one region (e.g. Newark) for low latency between nodes, or spread for geo testing.

### 4.2 Node Layout

| Node | Role | Repos / features | Purpose |
|------|------|------------------|---------|
| node1 | Primary preset hub | SNESology + paradigm-threat presets; **node extensions** (npm: songwalker-js); **Piper TTS** | Full stack; GitHub webhooks |
| node2 | Lean mirror | Same repos **or** subset; **no** Piper / no node_extensions | Capability discovery; **Host** `repo.node` to pick repo |
| node3 | Stress / secondary | Optional Piper-only or extension-only | Load tests |

**Client access pattern:** Use **`Host: {repo}.{node}`** (and optional **`{repo}{DELIM}{branch-slug}`**) when switching repos — **not** `?repo=` in user-facing URLs (**[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §8).

### 4.3 Deployment Steps

1. **Create Linodes** (3× Nanode 1GB, Debian 12). Piper nodes may need **2GB RAM** for concurrent synthesis.
2. **Install Docker** on each.
3. **Build & push image** to GHCR (extend Dockerfile on node1 for Piper + optional `node_extensions` layer).
4. **Configure each node (example):**
   ```bash
   # Node 1 — preset hub + Piper + dynamic modules
   RELAY_MASTER_REPO_LIST=https://github.com/org/snesology;https://github.com/org/paradigm-threat-presets
   RELAY_DEFAULT_REPO=snesology
   RELAY_ENABLE_NODE_EXTENSIONS=1
   RELAY_NODE_EXTENSIONS_PACKAGES=songwalker-js
   RELAY_SERVICES=base,piper-tts,songwalker-js
   RELAY_PIPER_MODELS=/srv/relay/piper-models
   RELAY_MASTER_PEER_LIST=node2-ip;node3-ip
   RELAY_PEER_SECRET=<shared-secret>
   GITHUB_WEBHOOK_SECRET=<from-github>

   # Node 2 — lean (no Piper, optional no extensions)
   RELAY_MASTER_REPO_LIST=... # same or subset
   RELAY_DEFAULT_REPO=snesology
   RELAY_SERVICES=base
   RELAY_MASTER_PEER_LIST=node1-ip;node3-ip

   # Node 3
   RELAY_SERVICES=base,piper-tts
   ```
5. **DNS:** Use Linode DNS or Vercel for `node1.relaynet.online`, etc., or test with IPs + `/etc/hosts`.
6. **Firewall:** Open 80, 443, 9418 (git) on each node.
7. **SSL:** Use `RELAY_SSL_MODE=certbot-required`, `RELAY_CERTBOT_EMAIL`, and `RELAY_DNS_*` if using Vercel DNS. For quick tests, use `RELAY_SSL_MODE=none` and HTTP.

---

## Part 5: Server Features (Piper TTS, dynamic modules & capability discovery)

### 5.1 Piper TTS — full on-server deployment (recommended)

**Piper** ([rhasspy/piper](https://github.com/rhasspy/piper)) is a fast local neural TTS suitable for self-hosted relay nodes: no external API dependency, predictable latency, offline-capable.

**Deployment checklist**

1. **Binary:** Ship `piper` CLI (or embed via subprocess) in Docker image on selected nodes.
2. **Voice models:** Download `.onnx` + `.onnx.json` into `RELAY_PIPER_MODELS` (e.g. `en_US-lessac-medium`).
3. **API:** `POST /api/tts` with JSON `{ "text": "...", "voice": "en_US-lessac-medium" }` → WAV/OGG stream. Rate-limit per IP; cap max input length.
4. **Advertise:** `RELAY_SERVICES` includes `piper-tts`; `/api/config` lists available voice IDs from disk scan.
5. **Security:** Piper runs as subprocess with timeout; no shell interpolation on user text.

Nodes **without** Piper omit `piper-tts` from services; clients use `/api/config` to show narration only when available.

### 5.2 Optional remote TTS proxy

If you still want **paradigm-threat-site** or another hosted API as a fallback:

- Second env: `RELAY_TTS_REMOTE_URL` + optional auth header.
- Client or server chooses **local Piper first**, then remote when `piper-tts` absent.

### 5.3 songwalker-js & songwalker-core integration

| Surface | Mechanism |
|---------|-----------|
| **Web (in-repo apps)** | Server with `songwalker-js` in node extensions resolves imports during transpile/serve; client uses **`Host`** so the correct repo’s hooks run. |
| **Android / iOS / desktop** | **songwalker-core** linked in native relay clients; repos target a declared **minCoreVersion**; presets and scripts sync via relay Git. |

### 5.4 Capability discovery (`/api/config`)

Extend responses so clients can branch UI without query hacks:

```json
{
  "peers": ["node2", "node3"],
  "defaultRepo": "snesology",
  "repos": ["snesology", "paradigm-threat-presets"],
  "services": ["base", "piper-tts", "songwalker-js"],
  "dynamicModules": ["songwalker-js"],
  "piperVoices": ["en_US-lessac-medium"]
}
```

- **`RELAY_SERVICES`:** e.g. `base,piper-tts,songwalker-js`.
- **Per-peer:** Clients may query each peer and merge “where is Piper available?” for smart routing.

---

## Part 6: Stress Testing Plan

### 6.1 Tools

- **k6** or **wrk** for HTTP load.
- **Apache Bench (ab)** for quick checks.
- **Custom script** for QUERY + GET mix.

### 6.2 Scenarios

| Scenario | Target | Duration | Success criteria |
|----------|--------|----------|-------------------|
| GET burst | `/` with `Host: repo.node` (optional branch slug) | 60s, 100 req/s | <200ms p95, 0% errors |
| QUERY burst | QUERY on index | 60s, 20 req/s | <500ms p95, valid JSON |
| Transpile burst | POST /transpile | 30s, 10 req/s | <1s p95 |
| Mixed | 70% GET, 20% QUERY, 10% config | 120s, 50 req/s | <300ms p95 |
| Sustained | 10 req/s for 10 min | 600s | No memory growth, no OOM |

### 6.3 Stress Test Script (k6)

```javascript
// stress.js
import http from 'k6/http';
import { check, sleep } from 'k6';

export const options = {
  stages: [
    { duration: '1m', target: 50 },
    { duration: '2m', target: 100 },
    { duration: '1m', target: 0 },
  ],
};

export default function () {
  const res = http.get('http://node1-ip/', {
    headers: { 'Host': 'snesology.atlanta1.example.net' },
  });
  check(res, { 'status 200 or 204': (r) => r.status < 300 });
  sleep(0.1);
}
```

Run: `k6 run stress.js`

---

## Part 7: Server-Side Scripts Testing

### 7.1 Pre-commit (PUT flow)

1. PUT a file via Relay API (e.g. `data/test/meta.yaml`).
2. Verify `pre-commit.mjs` runs and validation passes.
3. PUT an invalid path (e.g. `unauthorized.file`) and verify rejection.
4. PUT invalid `meta.yaml` (bad schema) and verify rejection.

### 7.2 Pre-receive (git push)

1. Clone relay-template, make a valid change, push to node's git daemon.
2. Verify `pre-receive.mjs` runs.
3. Push invalid change (e.g. disallowed path) and verify rejection.
4. Run `tests/test_hooks.mjs` locally before pushing.

### 7.3 Index hook

1. After a push, send QUERY request.
2. Verify JIT indexing runs and returns correct data.
3. Check `index.db.json` or `.relay_data/branches/*/` for expected structure.

### 7.4 Test Matrix

| Hook | Trigger | Test method |
|------|---------|-------------|
| pre-commit | PUT | curl -X PUT with body |
| pre-receive | git push | git push git://node/repo.git |
| index | QUERY | QUERY request after push |
| post-receive | After push | Verify auto-push to peers |

---

## Part 8: Private Key Auth & Repo Rules Testing

### 8.1 Branch Rules (requireSigned)

1. **Setup:** Generate GPG or SSH signing key; configure git to sign commits.
2. **relay-template:** Set `requireSigned: true` for `main` in `.relay.yaml`.
3. **Push signed commit:** Should succeed.
4. **Push unsigned commit:** Should fail with "Commit must be signed and verified".
5. **allowUnsigned on `public` branch:** Push unsigned to `public`; should succeed.

### 8.2 allowedKeys (when implemented)

1. Add `.ssh/admin.pub` to repo.
2. Configure `allowedKeys: [".ssh/admin.pub"]` for `main`.
3. Push with key that matches; should succeed.
4. Push with different key; should fail (after implementation).

### 8.3 Current Limitation

`allowedKeys` is **not enforced** today. Plan includes implementing it in Phase A or B.

---

## Part 9: Execution Timeline

| Day | Focus |
|-----|-------|
| 1 | Part 0 repo resolution (`RELAY_DEFAULT_REPO`, header-first); post_git_pull multi-repo; Linodes |
| 2 | Deploy SNESology + paradigm-threat repos; peer sync; test **Host** repo selection (no `?repo=` in apps) |
| 3 | `/api/config` + Piper on node1; node_extensions + songwalker-js resolve path |
| 4 | Stress tests (k6), tune timeouts/limits |
| 5 | Server hooks full test (pre-commit, pre-receive, index) |
| 6 | Private key auth, branch rules, signed commits |
| 7 | Peer webhook (`/hooks/peer`), bidirectional sync test |
| 8+ | Buffer for fixes, docs, cleanup |

---

## Part 10: Checklist Before Launch

- [ ] **Part 0:** `RELAY_DEFAULT_REPO`; multi-repo render uses **Host** only; query `?repo=` documented as admin-only; **`X-Relay-Branch` removed** per **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §11
- [ ] `post_git_pull` multi-repo (header or `?repo=` for ops)
- [ ] GitHub webhook signature verification
- [ ] `/api/config`: `services`, `dynamicModules`, `piperVoices`, `defaultRepo`, `repos`
- [ ] **Piper TTS** on at least one node; voice models on disk
- [ ] **Optional:** node_extensions + songwalker-js resolve for preset repos
- [ ] Linode nodes created; **SNESology + paradigm-threat preset** repos cloned first
- [ ] `.relay.yaml` `originList` updated with node hostnames
- [ ] `RELAY_MASTER_PEER_LIST` set on all nodes
- [ ] Firewall: 80, 443, 9418 open
- [ ] SSL: certbot or `RELAY_SSL_MODE=none` for testing
- [ ] Stress test script run and results recorded
- [ ] Server hooks integration test (`test_hooks.mjs`) passes
- [ ] Signed commit test with `requireSigned: true`

---

## Appendix A: Environment Reference

| Variable | Purpose |
|----------|---------|
| `RELAY_REPO_PATH` | Root for bare repos (e.g. `/srv/relay/data`) |
| `RELAY_MASTER_REPO_LIST` | Semicolon-separated repos to clone (**SNESology; paradigm-threat presets first**) |
| **`RELAY_DEFAULT_REPO`** | Bare repo name (no `.git`) for landing when no header/subdomain match |
| `RELAY_MASTER_PEER_LIST` | Peers for `/api/config` and auto-push targets |
| `RELAY_PEER_SECRET` | Shared secret for `/hooks/peer` (to add) |
| `GITHUB_WEBHOOK_SECRET` | For GitHub webhook verification (to add) |
| `RELAY_SERVICES` | e.g. `base`, `piper-tts`, `songwalker-js` |
| **`RELAY_PIPER_MODELS`** | Directory of Piper `.onnx` + `.onnx.json` voice models |
| **`RELAY_ENABLE_NODE_EXTENSIONS`** | `1` to enable dynamic npm resolution path |
| **`RELAY_NODE_EXTENSIONS_PACKAGES`** | Packages installed under server extension dir (e.g. `songwalker-js`) |
| **`RELAY_TTS_REMOTE_URL`** | Optional fallback remote TTS API (paradigm-threat-site, etc.) |
| `RELAY_SSL_MODE` | `auto`, `none`, `certbot-required` |
| `RELAY_CERTBOT_EMAIL` | Let's Encrypt |
| `RELAY_DNS_DOMAIN`, `RELAY_DNS_SUBDOMAIN` | For Vercel DNS (Docker entrypoint) |
| `VERCEL_API_TOKEN`, `VERCEL_TEAM_ID` | Vercel DNS API |
| **`RELAY_PUBLIC_FQDN`** / **`RELAY_DNS_FQDN`**, **`RELAY_SKIP_VERCEL_DNS`** | Bare-metal **`install.sh`**: full hostname for Vercel A record + verify, or skip DNS (see **`DEPLOY_LINODE.md`**) |
| **`RELAY_VERCEL_DOMAIN`**, **`RELAY_DNS_TTL`**, **`RELAY_DNS_WAIT_*`** | Optional **`install.sh`** DNS tuning (zone override, TTL, propagation wait) |

---

## Appendix B: Piper & remote TTS

- **Primary:** Piper local synthesis (Part 5.1).
- **Remote:** If paradigm-threat-site (or other) exposes TTS, configure `RELAY_TTS_REMOTE_URL` and optional auth; server or client falls back when `piper-tts` is not advertised.

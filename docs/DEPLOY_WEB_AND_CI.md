# Web hosting, Next.js, and self-hosted CI (plan)

Normative **product / ops** notes for serving **relay-server**, **multiple Next.js sites**, and **relay-client-web** builds on **Linode**, with a **simple self-hosted runner** and **no artifact registry or cloud secrets** for the first iteration.

**Code status:** This document describes **planned** behavior. Implementations should update the “Implementation status” section at the bottom as pieces land.

**Related:** **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** (VM install), **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)** (**`relay.env`**, rotation, shared deploy SSH key), **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** (**`relay-master`** catalog, submodules + **`relayRootCommit`**, emitted **`authorized-repos.yaml`**), **[`LAUNCH_AND_TEST_PLAN.md`](./LAUNCH_AND_TEST_PLAN.md)** (default repo resolution, **`RELAY_DEFAULT_REPO=relay-master`**), **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)** (per-repo `.relay.yaml`, hooks).

### Git sync to relay nodes (normative)

Pushes that **land in relay’s bare repos** MUST use the **git daemon URL** — **`git://{node}:{port}/{repo}.git`** (default port **9418**). Do not use **SSH (`git@…`)** for that path; SSH is for **server administration**, not relay’s published git service. **Normative human-facing form:** **`git://relaygateway.net/{repo}.git`** — implement with **DNS / L4 routing** to the right node (**[`relay-master/README.md`](../../relay-master/README.md)**). See **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** (seed repos, **`POST /api/git/ensure-bare/:repo`**). Until push-time bare provisioning exists, **`git daemon`** rejects unknown paths first; see **[`README.md`](../README.md)** (Git daemon).

---

## 1. Addressing and DNS (current decision)

- **Everything runs on Linode.** Public IPs are set **manually** for now.
- **No requirement** to automate DNS or Linode APIs in the first pass; operators point A/AAAA (and later wildcards) at the correct instance IPs when ready.

### 1.1 Wildcard DNS (single label before the node)

Relay traffic uses **one** DNS label before the node FQDN, e.g. **`{something}.atlanta1.relaygateway.net`**. A single wildcard is enough:

- **`*.atlanta1.relaygateway.net`** → node IP (zone `relaygateway.net`, record name `*.atlanta1`).

How **`{something}`** is interpreted is defined in **§8** (repo only → default branch; repo + optional delimiter + branch slug → that branch).

**Optional alternative:** **per-repo** wildcard `*.somerepo.atlanta1.relaygateway.net` if you prefer two labels (`main.somerepo.atlanta1...`) without folding; not the default scheme.

**Apex marketing domains** (`paradigmthreat.com`, `relaygateway.net` for Next) stay **explicit** A/AAAA (or apex + `www`) as needed; wildcards are mainly for **node + repo** naming under your relay zone.

---

## 2. Reverse proxy and TLS

- **TLS terminates on each Linode** (e.g. **Caddy** or **nginx**).
- Routing (by `Host`):
  - Domains registered as **CICD / Next.js websites** → local **Next.js** process for that repo/site.
  - **API** routes: configured per app (e.g. Next rewrites); APIs may live on the same instance as that Next app or elsewhere (e.g. legacy Vercel) until migrated.
  - **Node apex** (e.g. `atlanta1.relaygateway.net`) → **relay-server** HTTP: **default repo** on the default branch (static file serving from git). *Planned:* if no suitable index exists, optionally fall back to a **relay-client-web** build (same note as in product threads — not implemented until specified).
  - **`{label}.{node}`** → **relay-server**: parse **`label`** per **§8** — either **repo only** (**default branch**) or **`repo{DELIM}branchSlug`** (non-default branch).

- **relay-server** remains a **separate systemd service** from any Next.js units so routine site rebuilds do not restart or reconfigure the relay binary.

---

## 3. Multiple Next.js apps on one Linode

**Yes — many Next.js instances can run on a single VM.**

| Approach | Notes |
|----------|--------|
| **One Node process per site** | Each repo’s production deploy gets its own **`next start`** (or standalone output) on a **dedicated port** (e.g. 3001, 3002, …). |
| **systemd** | One unit per site, or a **template unit** (`nextjs@paradigm-threat-site.service`) with instance-specific `EnvironmentFile`. |
| **Reverse proxy** | Caddy/nginx maps **`Host`** → `127.0.0.1:PORT`. |

Separate repos **should** map to **separate processes** when they are separate products (e.g. **paradigm-threat-site** vs **relay-client-web**). Sharing one Next process for unrelated repos is discouraged unless they are intentionally one deployable monorepo.

---

## 4. Self-hosted CI (no GitLab CI for these sites)

- **GitLab** remains the **mirror / file browser** for repos; **CI for the marketing Next apps is not GitLab CI** in this plan.
- Use a **simple self-hosted runner** on Linode that works from a **clean checkout** of the target branch (same machine as deploy target or a build user on that host).
- **Build command (v1):** only **`npm run build`** is supported. **`package.json`** (and any **`scripts.build`**) live **in the repo**; dependency install uses normal **`npm ci`** or **`npm install`** as a prerequisite step — no `pnpm`/`yarn`/custom build entrypoints in automation for now.
- **No artifact publishing** and **no cloud secrets** in v1.

### Singleton build queue (all CICD sites, one at a time)

Multiple repos may request a rebuild close together; **only one build should run at a time** on a given host (CPU, disk, Node memory).

- Add a **small coordinator**: either a **root/relay-owned shell script** + **`flock`**/PID file, or a **dedicated Rust binary** (e.g. `relay-cicd-queue`) that:
  - Accepts **enqueue** requests (stdin, Unix socket, or `relay-deploy-site --queue`).
  - Runs **`npm ci`/`npm install` → `npm run build` → promote** for **one** site, then drains the next job.
- **relay-server** and **install hooks** should call this coordinator instead of spawning overlapping builds.

### Promote only after a successful build

1. Clone or fetch the target branch into a **staging directory** (or build workspace).
2. Run **`npm ci`** or **`npm install`**, then **`npm run build`**.
3. If any step **fails**, **stop** — do not touch the live symlink/directory.
4. If the build **succeeds**, **atomically** switch “live” to the new tree (e.g. `rsync` to versioned dir + `ln -sfn`, or replace a `current` symlink). The invariant is **no partial live state**.

---

## 5. Install / update script vs relay-server (CICD mapping)

- **`install.sh` / `update` flow** (see **[`INSTALL_FEATURES.md`](./INSTALL_FEATURES.md)**, **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)**) should **prompt** whether any hosted bare repos are **CICD websites** and write **only** installer-managed config (e.g. under `/opt/relay/state/`, root-owned) mapping **repo → domain(s) → branch → systemd/Caddy fragment**.
- **relay-server** must **not** be able to change which repos are CICD sites; it may only invoke a **fixed** helper (e.g. `relay-deploy-site`) after a **successful** git update on a **watched branch**, which re-runs the same promote path as CI.
- Optional **post-receive / webhook** path: validate branch → run build script → promote; failures leave live unchanged.

---

## 6. Trust, anchors, and repo size (planned)

Aligned with **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** and **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)**:

- **Anchor checks** remain **standard** for repos in **`authorized-repos.yaml`** (`POST /git-pull`, bootstrap, etc.). **Allowlist** rows are defined in **`relay-master`** (**submodules** + **`relayRootCommit`**) and emitted to YAML; see **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**. **`git push`** over the daemon should enforce the same rule in **`pre-receive`** (planned).
- **Per-repo policy** continues to come from **`.relay.yaml`** (and hooks) **in the repository**, except for **machine-local or installer-written** snippets under paths owned by **`install.sh`** / **`update`** (document those paths when implemented so operators know what is not from Git).

### Working-tree size cap for non-authorized repos (planned)

- **Metric:** total size of **files that would be served from the default branch’s tree** (working-tree content), **ignoring git object database / history**. Unit: **50 MB** maximum for repos **not** listed in **`authorized-repos.yaml`** (or equivalent allowlist).
- Repos **in** **`authorized-repos.yaml`** (including emitted catalog entries): **no** this cap (subject to disk and ops limits); anchor and hook rules still apply.
- Enforcement point: likely **pre-receive** and/or periodic check after push; exact rules TBD.

---

## 7. Default repo for node apex (`atlanta1.…` → relay-server)

When the HTTP `Host` is the **node FQDN** only (no `{repo}.` prefix), relay should serve the **default repo** — normatively **`relay-master`** when using the catalog model (**[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**).

**Until `RELAY_DEFAULT_REPO` is implemented in relay-server** (see **[`LAUNCH_AND_TEST_PLAN.md`](./LAUNCH_AND_TEST_PLAN.md)** Part 0):

- **Catalog deployments:** set **`RELAY_DEFAULT_REPO=relay-master`** and do **not** rely on bootstrap ordering for the allowlist (**[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**).
- **Legacy / pre-catalog:** operational rule: use the **first repo** in the **ordered clone/bootstrap list**:
  - **`RELAY_MASTER_REPO_LIST`** / Docker **`DEFAULT_REPOS`**: first **semicolon- (or comma-) separated** remote → bare repo **basename** is the default landing repo.
  - **`RELAY_SEED_REPOS`**: first **`name=url`** segment in order.
  - **`relay-bootstrap.sh` manifest**: first object in **`bareRepos[]`** (array order as in JSON; clone loop uses index `0..n-1`).
  - **Interactive / non-interactive catalog**: first entry in **`BOOTSTRAP_CATALOG_NAMES`** that gets cloned for this run (same order as the array in **`relay-bootstrap.sh`**).

When **`RELAY_DEFAULT_REPO`** exists, it **overrides** the “first in list” rule for this behavior.

---

## 8. Host label: repo, optional delimiter, branch slug (normative)

The leftmost label of **`{label}.{RELAY_PUBLIC_HOSTNAME}`** selects the bare repo and optionally the branch. **DNS allows one label** (max 63 octets, letters/digits/hyphens); Git branch names are mapped to a **branch slug** for the segment after the delimiter.

### Delimiter

- **Default delimiter:** `--` (two hyphens).
- **Configurable:** e.g. **`RELAY_REPO_BRANCH_HOST_DELIM`** in **`relay.env`** / install state (must be a string that is valid inside a DNS label — use ASCII that does not break hostname rules; document the chosen value on each node).
- **Operator constraint:** pick a delimiter that does **not** appear in **branch slugs** after normalization (see below). Repo names **may** contain the delimiter; parsing uses the **last** occurrence (see below).

### Parsing `label`

Let **`D`** be the configured delimiter.

1. If **`label`** contains **no** **`D`**:  
   - **Repo** = `label` (must match a bare repo name on the node).  
   - **Branch** = server **default branch** (e.g. **`main`** — server config only, **not** a client header).

2. If **`label`** contains **`D`**: split on the **last** **`D`** (rightmost):  
   - **Repo** = substring **before** that separator (must be non-empty, must match a bare repo).  
   - **Branch slug** = substring **after** (must be non-empty).  
   - Resolve **branch slug** → Git branch name (see below).  
   - *Example (default `D = --`):* `my-app--feature-auth` → repo `my-app`, slug `feature-auth` → branch `feature/auth` after reverse mapping.

3. **`X-Relay-Branch` is not supported.** Branch selection is **only** from the **Host** rules above plus the **default branch**. Do not send **`X-Relay-Branch`**; servers will **ignore** it during transition, then remove it from code and docs. See **§11**.

### Branch slug ↔ Git branch

- **Forward (Git → slug):** lowercase, replace **`/`** with **`-`**, collapse repeated **`-`**, trim leading/trailing **`-`**. The label **`repo` + `D` + slug** must fit **≤ 63** octets for host-based addressing.
- **Reverse (slug → Git):** server finds a branch whose canonical slug equals the segment, or returns **404** if ambiguous or missing.
- Slug **collisions** between branches are **prevented at push time** (**§9**).

### Security

Branch ACLs / signing are **deferred** (explicitly out of scope for v1), as before.

---

## 9. Minimum branch-name protocol (push / `pre-receive`)

**Goal:** allow **almost all** normal Git branch strings while keeping **§8** unambiguous.

**Canonical slug** `S(name)` for `refs/heads/<name>`: same normalization as **§8** (lowercase, **`/` → `-`**, collapse runs of **`-`**, trim **`-`**).

**Enforced on push** (planned: **`relay-hook-handler`** / `pre-receive`):

| Rule | Action |
|------|--------|
| **`S(name)` non-empty** | Reject the ref update if the slug is empty after normalization. |
| **Uniqueness** | No two distinct **`refs/heads/*`** may share the same **`S(name)`**. Reject a push that would create a duplicate slug. |

Other characters (unicode, `_`, multiple `/`, etc.) remain allowed unless they normalize to a **duplicate** slug. Optional later: strict **63-octet** host label check per repo for branches that must be reachable via **`{repo}{D}{slug}`**.

**`QUERY` / multi-branch:** legacy **`X-Relay-Branch: all`** is retired with the header; use **request body** (e.g. **`branch`** field, value **`all`** or a branch name) — exact JSON in OpenAPI when implemented.

---

## 10. Two-node testing

- Run **two** Linode instances to validate **sync**, **deploy**, and **client-side load balancing** (peer lists from **`/api/config`** / runtime), without requiring global edge caches.

---

## 11. Removal plan: `X-Relay-Branch`

Remove support in **all** maintained repos so the protocol has a **single** branch source (**Host** + default).

| Area | Action |
|------|--------|
| **relay-server** | Stop reading **`X-Relay-Branch`**; derive branch only from **§8** + default; remove **`HEADER_BRANCH`** from the public contract; migrate **QUERY** to body; update tests and examples. |
| **relay-client-web** | Stop sending **`X-Relay-Branch`**; encode branch only in **Host** (`repo` vs `repo{D}slug`). Review **`X-Relay-Repo`** — routing is **Host-only** on the server today. |
| **relay2 / relay-old / archives** | Historical; mark Postman + docs **legacy** when edited. |
| **Operators** | Replace **`curl -H X-Relay-Branch`** with the appropriate **`Host`** label. |

**Optional later:** emit resolved branch in a **new read-only response header** for debugging only; it must never be treated as input.

---

## 12. Remaining open questions (for implementers)

1. **Trigger mechanism** for enqueueing the singleton queue (GitLab **push webhook**, **relay post-receive**, or **polling**) — any is fine as long as promote rules in §4 hold.
2. **Coordinator shape:** shell+`flock` vs Rust binary (shared logging, backoff, priority).
3. **`QUERY` body schema** for **`branch`** / **`all`** (exact JSON shape).

---

## Implementation status

| Item | Status |
|------|--------|
| GitLab CI **`package:linode-tgz`** + tests/smoke | **Done** (`.gitlab-ci.yml`) |
| **`relay-master`** catalog (**`.gitmodules`** + **`relayRootCommit`**) + **`sync-relay-catalog.sh`** | **Done** (workspace **[`relay-master`](../../relay-master)**); **`relay-server`** still consumes emitted YAML |
| **`POST /api/git/ensure-bare/:repo`** + normative **`git://`** sync docs | **Done** |
| First **`git push`** creates bare when allowed (gateway / smart HTTP; root-commit identity) | **Planned** (see **`README`**, **`RELAY_TRUST_AND_BOOTSTRAP`**) |
| **`scripts/relay-linode-redeploy-seed.sh`** + checklist in **`DEPLOY_LINODE.md`** | **Done** |
| Caddy/nginx + multi-Next + relay split | Planned |
| **`npm run build`** only (+ `npm ci`/`npm install`) | Planned |
| Singleton CICD queue (script or Rust binary) | Planned |
| Installer prompts + state file for CICD repos | Planned |
| relay-triggered `relay-deploy-site` (no CICD mapping edits) | Planned |
| 50 MB default-branch working-tree cap (non-authorized only) | Planned |
| `RELAY_DEFAULT_REPO` in code | See LAUNCH_AND_TEST_PLAN (partial / pending) |
| Host label `repo` or `repo{DELIM}branchSlug` (§8) | Planned |
| **No `X-Relay-Branch`**; removal per §11 | Planned |
| Push-time slug uniqueness (**§9**) | Planned |
| relay-client-web fallback when no index on node apex | Noted only |

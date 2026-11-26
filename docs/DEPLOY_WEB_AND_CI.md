# Static web hosting and deploy pipeline (plan)

Normative **product / ops** notes for serving **relay-server** and **multiple static websites** (plain HTML, or **pre-built** Next.js / React / Vite output) on **Linode**, with **no site build toolchain on the relay VM**.

**Decision (relay Linodes):** Do **not** run **Next.js** (`next start`), a **Node.js API server**, or **any `npm run build` / install** for marketing or app sites **on** the relay server. **Goals:** lower CPU, RAM, and disk requirements; predictable load; smaller attack surface. **Sites** are built **elsewhere** (developer machine, GitHub Actions, GitLab CI, Vercel export step, etc.); the relay host only receives **already-compiled** static trees and serves them via **Caddy** / **nginx** `root` / `file_server`, or via **relay-server** git-backed static paths where that model already applies.

**Code status:** This document describes **planned** behavior. Implementations should update the “Implementation status” section at the bottom as pieces land.

**Related:** **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** (VM install), **[`RELAY_WEB_ASSETS.md`](./RELAY_WEB_ASSETS.md)** (signed git CDN for `embed.js` / WASM), **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)** (**`relay.env`**, rotation, shared deploy SSH key), **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** (**`relay-master`** catalog, submodules + **`relayRootCommit`**, emitted **`authorized-repos.yaml`**), **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)** (per-repo `.relay.yaml`, hooks), **[`PARADIGM_PARK_LOCAL.md`](./PARADIGM_PARK_LOCAL.md)** (local park SSR hardening), **[`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md)** (pre-rendered `ssrCache` + publisher bake).

**Park HTML:** production parked Hosts use catalog **`ssrCache`** — bake on the fat publisher only (`scripts/relay-ssr-bake.py` / bake queue); Nanodes serve composed fragments from **`relay-ssr-cache`**. Do not run live CleverScript SSR on Nanode request paths. Fleet multi-A is fat-only ([`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md)).

**Note:** **Git hooks** may run **CleverScript** (`.cs`) validation scripts defined **in** repositories — that is separate from hosting a long-lived Next/Node **HTTP** stack for public sites.

### Git sync to relay nodes (normative)

Pushes that **land in relay’s bare repos** MUST use the **git daemon URL** — **`git://{node}:{port}/{repo}.git`** (default port **9418**, **`relay-git-proxy`**). Do not use **SSH (`git@…`)** for that path; SSH is for **server administration**, not relay’s published git service. **Normative human-facing form:** **`git://relaygateway.net/{repo}.git`** — implement with **DNS / L4 routing** to the right node (**[`relay-master/README.md`](../../relay-master/README.md)**). See **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** (seed repos). First **`git-receive-pack`** creates a missing bare when **`RELAY_GIT_ENSURE_BARE_ENABLED`** is on and catalog **`publish_origin`** allows this node; **`POST /api/git/ensure-bare/:repo`** remains for pre-create. See **[`README.md`](../README.md)** (Git daemon).

---

## 1. Addressing and DNS (current decision)

- **Everything runs on Linode.** Public IPs are set **manually** for now.
- **No requirement** to automate DNS or Linode APIs in the first pass; operators point A/AAAA (and later wildcards) at the correct instance IPs when ready.

### 1.1 Wildcard DNS (single label before the node)

Relay traffic uses **one** DNS label before the node FQDN, e.g. **`{something}.atlanta1.relaygateway.net`**. A single wildcard is enough:

- **`*.atlanta1.relaygateway.net`** → node IP (zone `relaygateway.net`, record name `*.atlanta1`).

How **`{something}`** is interpreted is defined in **§8** (repo only → default branch; repo + optional delimiter + branch slug → that branch).

**Optional alternative:** **per-repo** wildcard `*.somerepo.atlanta1.relaygateway.net` if you prefer two labels (`main.somerepo.atlanta1...`) without folding; not the default scheme.

**Apex marketing domains** (`paradigmthreat.com`, marketing `relaygateway.net` hosts, etc.) stay **explicit** A/AAAA (or apex + `www`) as needed; wildcards are mainly for **node + repo** naming under your relay zone.

---

## 2. Reverse proxy and TLS

- **TLS terminates on each Linode** (e.g. **Caddy** or **nginx**).
- Routing (by `Host`):
  - Domains registered as **static / marketing sites** → **static document root** on disk (Caddy **`file_server`**, nginx **`root`**). Content is **only** what was **uploaded or rsync’d** after an **off-box** build (e.g. Next **`out/`** from `next export` / `output: 'export'`, Vite **`dist/`**, plain **`public/`** trees).
  - **Dynamic APIs** (Next SSR, Route Handlers, serverless, etc.) **do not** run on the relay VM in this model; keep them on **Vercel**, another host, or redesign behind a separate service — **not** colocated with relay static hosting.
  - **Node apex** (e.g. `atlanta1.relaygateway.net`) → **relay-server** HTTP: **default repo** on the default branch (static file serving from git). *Planned:* if no suitable index exists, fall back to a **pre-built static** tree (same directory layout as any other static site), **not** a live Node process.
  - **`{label}.{node}`** → **relay-server**: parse **`label`** per **§8** — either **repo only** (**default branch**) or **`repo{DELIM}branchSlug`** (non-default branch).

- **relay-server** stays a **separate systemd service** from the reverse proxy. **Reloading** Caddy/nginx for a new static tree **must not** restart **`relay-server`**.

---

## 3. Multiple static sites on one Linode

**Yes — many static sites can share one VM** without one Node process per product.

| Approach | Notes |
|----------|--------|
| **One directory per site** | e.g. `/var/www/paradigm-threat-site/current/` → symlink to versioned release dir after promote. |
| **Reverse proxy** | Caddy/nginx maps **`Host`** → filesystem **`root`**. No backend port unless you intentionally proxy to **relay-server** or another service. |
| **Optional `systemd`** | Only for **reload** hooks or **oneshot** deploy scripts — **not** for `next start`. |

Separate products **should** use **separate roots** and **separate `Host`** blocks. **Next.js** is supported **only** as **static export** artifacts placed in those roots.

---

## 4. Builds off the relay; deploy is promote-only

- **GitLab** remains the **mirror / file browser** for source repos. **HTML/JS/CSS builds** for static sites run **outside** the relay Linode (CI, laptop, or other builder).
- **Relay VM responsibilities:** accept **artifacts** (tarball, `rsync`, object store pull, etc.), extract into a **staging** directory, verify, then **atomically** point **`current`** at the new tree. **No `npm ci`**, **`npm install`**, or **`npm run build`** on the relay for these sites.
- **No artifact registry** is required for v1; **scp/rsync** from CI with a deploy key is enough.

### Singleton deploy queue (optional)

If many sites update at once, **serialize promote operations** on a given host (disk I/O, brief nginx reload) — a **lightweight** queue (shell + **`flock`**, or small helper binary). **Not** the same as queuing **compilation**; there is **no** compile step on-box.

### Promote only after a successful off-box build

1. Builder produces a **complete static tree** (e.g. `out/` or `dist/`).
2. If the **build fails upstream**, **do not** upload / do not promote.
3. After upload to **staging**, optional checks (hash list, `index.html` exists), then **atomic** switch: `ln -sfn` / versioned dirs / `rsync --delete` to live root per your playbook.
4. **Invariant:** **no partial live state** visible to visitors.

---

## 5. Install / update script vs relay-server (site mapping)

- **`install.sh` / `update` flow** (see **[`INSTALL_FEATURES.md`](./INSTALL_FEATURES.md)**, **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)**) may **prompt** for hosted **static site** domains and write installer-managed config (e.g. under `/opt/relay/state/`, root-owned) mapping **`Host` → filesystem root** (Caddy/nginx fragment), **not** `systemd` + Node.
- **relay-server** must **not** change static-site mappings; it may invoke a **fixed** helper (e.g. `relay-deploy-static`) after a **successful** git update on a **watched** branch only if that helper **only** triggers an **off-box** fetch or **pre-staged** artifact promote — **never** `npm run build` on the relay.
- Optional **post-receive / webhook** path: notify external CI, or pull a **published artifact URL**; failures leave live unchanged.

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

**Until `RELAY_DEFAULT_REPO` is implemented in relay-server** (see **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**):

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

1. **Trigger mechanism** for static deploy (GitLab **push webhook**, **relay post-receive**, **polling**, or **manual** `rsync`) — any is fine as long as promote rules in §4 hold.
2. **Artifact format:** raw directory, `.tar.zst`, or OCI — v1 can standardize on **tarball + atomic symlink**.
3. **`QUERY` body schema** for **`branch`** / **`all`** (exact JSON shape).

---

## Implementation status

| Item | Status |
|------|--------|
| GitLab CI **`package:linode-tgz`** + tests/smoke | **Done** (`.gitlab-ci.yml`) |
| **`relay-master`** catalog (**`.gitmodules`** + **`relayRootCommit`**) + **`sync-relay-catalog.sh`** | **Done** (workspace **[`relay-master`](../../relay-master)**); **`relay-server`** still consumes emitted YAML |
| **`POST /api/git/ensure-bare/:repo`** + normative **`git://`** sync docs | **Done** |
| First **`git push`** creates bare when allowed (`relay-git-proxy`; same as ensure-bare + `publish_origin`) | **Done** |
| **`scripts/relay-linode-redeploy-seed.sh`** + checklist in **`DEPLOY_LINODE.md`** | **Done** |
| Caddy/nginx + **static `root` / `file_server`** + relay split | Planned |
| **No** Next/Node **HTTP** or **`npm run build`** on relay for marketing sites | **Normative** (this doc) |
| Optional singleton **deploy** queue (unpack/promote only) | Planned |
| Installer prompts + state file for **static** site `Host` → root | Planned |
| relay-triggered static promote helper (no mapping edits) | Planned |
| 50 MB default-branch working-tree cap (non-authorized only) | Planned |
| `RELAY_DEFAULT_REPO` in code | Partial / pending — see **`RELAY_TRUST_AND_BOOTSTRAP`** |
| Host label `repo` or `repo{DELIM}branchSlug` (§8) | Planned |
| **No `X-Relay-Branch`**; removal per §11 | Planned |
| Push-time slug uniqueness (**§9**) | Planned |
| Node apex fallback: **static tree** only (no live Node) | Noted |

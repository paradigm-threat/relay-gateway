# Relay trust, catalog (`relay-master`), and bootstrap

**See also:** [RELAY_AUTH_PROTOCOL.md](./RELAY_AUTH_PROTOCOL.md) (signed commits, ingest vs peer, HTTP parity, fast-forward policy).

## Normative model: **`relay-master`** is the allowlist

**[`relay-master`](../../relay-master)** is the **master repository** for a Relay mesh: it lists **which repositories** may be mirrored, **where to clone them from**, and the **trusted root commit** for each entry. That same repository is the **default HTTP site** for the node apex when operators set **`RELAY_DEFAULT_REPO=relay-master`** (repo name without `.git`).

### Whitelist = `relay-catalog.json`

Each entry under **`repos`** in **`relay-master/relay-catalog.json`** defines one catalog entry:

| Field | Meaning |
|--------|---------|
| **`url`** | **Authoritative clone URL** for relays (HTTPS, `git://`, etc.). Used by **`sync-relay-catalog.sh`** for bare mirrors; **`relay-master`** does not nest git checkouts for these repos. |
| **`relayRootCommit`** | 40-character object id of the trusted root. After clone/fetch, that commit **must be an ancestor** of the configured branch tip—**same check** **`relay-gateway` applies to `anchor_commit`** after pull. |
| **`branch`** | Branch to verify (default **`main`**). |

Relay policy is **`url` + `relayRootCommit` + `branch`** (no git submodules in **`relay-master`**).

### Public forge URL vs private body (copyright / ops)

For any repository listed in **`relay-master`**, the **`url`** in **`relay-catalog.json`** is what relay nodes **clone or fetch** from the forge. Treat that URL as **public-facing policy**: it should point at a Git history whose **first commit is intentionally minimal** (typically **README-only**), so operators and **`sync-relay-catalog.sh`** can use:

```bash
git clone --depth 1 <repository_URL>
```

**Why:** **`--depth 1`** keeps forge bandwidth and disk small, avoids publishing large or restricted trees on GitLab/GitHub, and still gives **`relay-gateway`** a valid **`relayRootCommit`** (the trusted root) that remains an ancestor of the default branch tip. **Full history and payloads stay private**; **control of what the mesh actually serves** stays with operators who **`git push`** into the relay bare mirror (**`git://<node>:9418/<repo>.git`**, etc.) after the catalog sync has created the bare repo from the shallow public tip.

**Rules of thumb:**

- **`relayRootCommit`** = **`git rev-list --max-parents=0 <branch>`** on the **forge** clone (the minimal root you allowlisted).
- If you add **new commits on the forge** (e.g. README edits), ensure **`git clone --depth 1`** still reaches **`relayRootCommit`** (with the default **`RELAY_CATALOG_CLONE_DEPTH=1`**, that means the root must be the **tip** or you must increase depth — see **`relay-master/scripts/sync-relay-catalog.sh`**).
- **Never** point the catalog **`url`** at a private dump you refuse to host publicly; use a **stub repo** + push the real tree to relay only.

**Authentication on nodes:** **`sync-relay-catalog.sh`** runs **`git clone` / `git fetch`** as the **`relay`** (or equivalent) UNIX user. If the forge project is **private**, that user needs **non-interactive** read access (e.g. **Public** or **Internal** visibility for a README-only stub, a **read-only deploy token** in **`git credential`**, or an **SSH `url`** in **`relay-catalog.json`** plus a deploy key). Otherwise **`fatal: could not read Username for 'https://gitlab.com'`** is expected.

**Helper:** **`relay-master/scripts/init-minimal-public-catalog-repo.sh`** creates a fresh repo whose only commit is **`README.md`** (print **`relayRootCommit=`** for **`relay-catalog.json`**). **`relay-master/scripts/publish-catalog-stub-to-gitlab.sh`** (optional **`GITLAB_TOKEN`**) creates the GitLab project and pushes that single commit.

### First commit should read as an intentional empty / stub repo

When you add a **new** catalog repo, make the **first commit on the forge** (and what **`sync-relay-catalog.sh`** shallow-clones) **obviously** a placeholder: e.g. **`README.md`** whose title or body states that the repo is **empty**, **stub**, or **content is pushed to Relay only**. That way:

- Browsing **`https://{repo}.{node}/`** after a shallow sync shows a clear **“not synced / stub”** story instead of looking like a broken site.
- Operators can tell **forge-only** state from **mesh lag** or **policy failure** at a glance.
- **`POST /api/config` → `startup_catalog_sync`** (written by **`relay-startup-catalog-sync.sh`**) still reports forge vs peer fetch health; the visible default branch content should match the ops story above.

### How operators add a repo

1. **Merge** a change to **`relay-master`** that adds or updates a **`repos.<name>`** object (**`url`**, **`relayRootCommit`**, **`branch`**). Obtain the root with `git rev-list --max-parents=0 <branch>` on a trusted clone; if history has multiple roots, pick the **canonical** root your policy requires and record it in the PR. For **stub remotes**, the root is usually the **only** commit on the forge branch.
2. On each node, **update the `relay-master` checkout** (pull, deploy artifact, etc.).
3. Run **`./scripts/sync-relay-catalog.sh`** from that checkout. It **clones or fetches** bare mirrors under **`RELAY_REPO_PATH/<name>.git`** (default **shallow** from the forge) and **rejects** catalogs whose upstream no longer contains **`relayRootCommit`** in the branch ancestry (blocks unrelated history).

### `relay-gateway` enforcement (catalog path **or** YAML)

Set **exactly one** of:

| Mode | Env | Behavior |
|------|-----|----------|
| **Preferred** | **`RELAY_MASTER_CATALOG_PATH`** | Server reads **`relay-catalog.json`** from that **worktree** at startup: synthetic **`relay-master`** row from checkout **`HEAD`**, plus each catalog repo’s **`relayRootCommit`** and **`branch`**. |
| **Legacy / export** | **`RELAY_AUTHORIZED_REPOS_PATH`** | Server reads YAML (e.g. from **`sync-relay-catalog.sh --write-authorized-repos`**). |

**Never set both** — **`relay-gateway` exits** at startup if both are present.

**Pin the catalog checkout (recommended):** **`RELAY_MASTER_TRUST_ROOT`** = 40 hex chars, the **trusted root** of your real **`relay-master`** (usually `git rev-list --max-parents=0 main` on a known-good clone). On startup the server checks that this commit is an **ancestor** of **`HEAD`** at **`RELAY_MASTER_CATALOG_PATH`**, so a wrong directory or forked repo fails fast.

**After `git pull` on the catalog checkout:** **`relay-gateway` does not hot-reload** policy—**restart** the service (or rely on a future SIGHUP) whenever **`relay-catalog.json`** or **`HEAD`** changes.

**Greenfield nodes:** set **`RELAY_MASTER_CATALOG_GIT_URL`** in **`relay.env`** to your public **`relay-master`** HTTPS URL. **`install.sh`** creates **`/opt/relay/src`** owned by **`relay`**; on first **`relay-gateway`** start, **`relay-startup-catalog-sync.sh`** clones the catalog there (unless **`RELAY_MASTER_CATALOG_PATH`** already holds a valid worktree), then runs **`sync-relay-catalog.sh`**. You still set **`RELAY_MASTER_TRUST_ROOT`** and mesh secrets in the same **`relay.env`** — no separate SSH step to clone the catalog. The startup script uses a **non-trivial default clone depth** and may **deepen** a shallow catalog so **`RELAY_MASTER_TRUST_ROOT`** remains in history; see **`scripts/relay-startup-catalog-sync.sh`** and **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** § “Greenfield checklist”.

**Sync bare mirrors** (still use the script; independent of how policy is loaded):

```bash
cd /path/to/relay-master
git pull origin main   # or your default branch; avoids missing-upstream after fresh clone
RELAY_REPO_PATH=/opt/relay/data ./scripts/sync-relay-catalog.sh
# Optional if you still use YAML instead of RELAY_MASTER_CATALOG_PATH:
# ./scripts/sync-relay-catalog.sh --write-authorized-repos relay-atlanta1 /opt/relay/authorized-repos.yaml
sudo systemctl restart relay-gateway
```

Run as the UNIX user that owns **`RELAY_REPO_PATH`** (often **`sudo -u relay bash -lc '…'`**).

**Minimum production launch:**

| # | Requirement | How |
|---|-------------|-----|
| **1** | **Unique server name** | **`RELAY_SERVER_ID`**. |
| **2** | **Allowlist + hashes** | **`RELAY_MASTER_CATALOG_PATH`** *or* **`RELAY_AUTHORIZED_REPOS_PATH`** (YAML with **`relay_server_id`** matching (1)). |

Without (1)+(2), **`relay-gateway` starts with no allowlist** (dev only).

**Hand-written YAML** for emergencies only; do not contradict **`relay-master`**.

### What the hash means

- **`anchor_commit`** (whether hand-written or emitted from **`relayRootCommit`**) must be an **ancestor** of **`refs/heads/<branch>`** after each successful **`POST /git-pull`**. Otherwise the pull **fails** (HTTP **403**) and **`main`** is rolled back when applicable.
- Pinning the **repository root** in the catalog: choose **`relayRootCommit`** so unrelated top-level histories (different root not in ancestry) cannot satisfy the check.

### User-facing clone URLs (`git://relaygateway.net/<repo>.git`)

**Git has no HTTP-style redirect for `git://`.** Publishing **`git://relaygateway.net/<repo-name>.git`** is a **routing/DNS/L4** concern (anycast, load balancer, or “this hostname currently points at the canonical node”). See **`relay-master/README.md`** for detail.

### Security on **`relay-master`**

- **Today:** No required signed commits; protect merge access like any ops repo.
- **Planned:** Require **signed commits** (or equivalent) so the catalog allowlist cannot change without trusted keys.

### Anti-squat (names on the node)

For every catalog repo name, policy (**YAML or in-memory from **`relay-catalog.json`**) should record **`anchor_commit` / `relayRootCommit` + `branch` before** `RELAY_REPO_PATH/<name>.git` is trusted, so unrelated history cannot occupy the name first via push/pull. **`relay-gateway` enforces this on `POST /git-pull`** today; **`git daemon` `receive-pack`** should get the same rule in **`pre-receive`** (planned). Restrict **9418** to trusted peers until then if names are public.

### Example generated file

See **[`examples/authorized-repos.yaml`](./examples/authorized-repos.yaml)**.

### Runtime env

**Catalog mode (no YAML):**

```bash
export RELAY_SERVER_ID=relay-atlanta3
export RELAY_MASTER_CATALOG_PATH=/opt/relay/src/relay-master
export RELAY_MASTER_TRUST_ROOT=14f88ca11ce88f580c1a3cab1dfa5493eace34e2   # example: rev-list --max-parents=0 main on YOUR canonical relay-master
export RELAY_DEFAULT_REPO=relay-master
```

**YAML mode:**

```bash
export RELAY_SERVER_ID=relay-atlanta1
export RELAY_AUTHORIZED_REPOS_PATH=/opt/relay/authorized-repos.yaml
export RELAY_DEFAULT_REPO=relay-master
```

`systemd` **`EnvironmentFile=`** should include these as needed.

---

## Updating a catalog node (e.g. **atlanta3**)

Requires **SSH** as a **sudo-capable user** (e.g. **`ari@atlanta3.relaygateway.net`**). **Do not use `root@`** on hardened nodes (**`PermitRootLogin no`**). Agents should use **`ssh -o BatchMode=yes user@host`** and **`sudo`** on the server—see **[`CURSOR_AGENTIC_SSH.md`](./CURSOR_AGENTIC_SSH.md)** §5.

Replace **`ari`** and paths below if your install layout differs.

```bash
ssh -o BatchMode=yes ari@atlanta3.relaygateway.net
# Run sync as the UNIX user that owns RELAY_REPO_PATH (often "relay"):
# Prefer `git pull origin main` (or your branch) if `main` has no upstream yet.
sudo -u relay bash -lc 'set -e; cd /opt/relay/src/relay-master && git pull origin main && \
  RELAY_REPO_PATH=/opt/relay/data ./scripts/sync-relay-catalog.sh'
sudo systemctl restart relay-gateway
```

**No `origin` on the catalog checkout** (e.g. tree copied by hand, or incomplete repair): **`git pull`** fails with “could not read from remote repository” or “no tracking information”. Fix once:

```bash
sudo -u relay bash -lc 'set -e; cd /opt/relay/src/relay-master
git remote add origin https://gitlab.com/clevertree/relay-master.git   # same as RELAY_MASTER_CATALOG_GIT_URL
git fetch origin
git checkout -B main origin/main    # or: git branch --set-upstream-to=origin/main main && git pull --ff-only
'
```

Then run **`sync-relay-catalog.sh`** as above.

If the node still uses **`RELAY_AUTHORIZED_REPOS_PATH`**, also run **`--write-authorized-repos …`** before **`restart`**. With **`RELAY_MASTER_CATALOG_PATH`**, **`restart`** is enough after **`git pull`** (policy read from the worktree at startup).

After you **push** an updated **`relay-master`** to the forge, run **`git pull` + `sync-relay-catalog.sh`** on the server so new catalog repos become bare mirrors. **`relay-gateway` does not auto-pull** the catalog checkout; use this manual step, **`cron`**, or a future **`post-receive`** on **`relay-master.git`** until implemented.

**Verify a new repo** (example: **`snesology-library`** on **`main`**): after **`sync-relay-catalog.sh`**, the bare may contain only the **public stub** (e.g. README). **`GET /index.json`** (or other app files) succeeds only after a trusted **`git push`** has landed the real tree on the node. With **`Host: snesology-library.atlanta3.relaygateway.net`** (or your wildcard equivalent):

`curl -fsS -H 'Host: snesology-library.atlanta3.relaygateway.net' 'http://SERVER_IP/index.json'`

---

## Testing **`relay-master`** and adding a catalog repo

1. **Local:** on a trusted clone, record the catalog **root** for **`RELAY_MASTER_TRUST_ROOT`**:  
   `git -C relay-master rev-list --max-parents=0 main` (use your default branch if not **`main`**).
2. **Node:** ensure **`RELAY_MASTER_CATALOG_PATH`** points at a **full worktree** (not only a bare mirror), **`RELAY_SERVER_ID`** is set, and **`sync-relay-catalog.sh`** has populated **`RELAY_REPO_PATH/*.git`**.
3. **Start check:** `relay-gateway` must start; if **`RELAY_MASTER_TRUST_ROOT`** is wrong for that checkout, startup **errors** immediately.
4. **`POST /api/config`:** **`authorized_repos`** lists **`relay-master`** plus each repo name from **`relay-catalog.json`**.
5. **Add a repo:** merge an updated **`relay-catalog.json`** (**`url`**, **`relayRootCommit`**, **`branch`**), push, on the node **`git pull`** + **`sync-relay-catalog.sh`**, **`systemctl restart relay-gateway`**, confirm **`authorized_repos`** includes the new name and **`Host: {name}.{node}`** serves a file you expect.

---

## Canonical hosting (no GitHub for presets)

Preset libraries are **canonical on Relay**. List them in **`relay-master`**, run **`sync-relay-catalog.sh`**, use **`git push git://…:9418/…`** only for **ad hoc** seeds if needed; routine adds go through the catalog. See **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)**.

---

## Planned: mesh networking, events, and pull-based sync

**[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)** describes the roadmap: **master peer network** (**`RELAY_MASTER_PEER_LIST`**) as the **secure-zone backbone** for **HTTP mesh control** (only **masters** hold **system-level** mesh credentials; other nodes are **outside** that zone for control-plane auth), while **Git** data plane remains **open to any allowed client** on masters (e.g. **non-master `git push`**) like any Git host. Also: **subscriptions**, **IRC-style** listener/client modes, optional **WebRTC**, **event-notify + `git fetch`** vs **Auto-Push**, **validation** (anchors / signatures), and **internal** anti–over-ping (**SHA-first**, optional **`via` / hops** — **no** Slack dependency). Optional **Slack**: **[`SLACK_MESH_NOTIFY.md`](./SLACK_MESH_NOTIFY.md)**.

---

## API

**`POST /api/config`** returns **`relay_server_id`** and **`authorized_repos`** (names only, not hashes).

---

## Bootstrap script

**`scripts/relay-bootstrap.sh`** — manifest must include **`relay_server_id`** matching **`RELAY_SERVER_ID`**. Each **`bareRepos[]`** entry may include **`anchorCommit`** and **`branch`**; same ancestor check runs after clone/fetch. Prefer aligning manifests with **`relay-master`** over time.

---

## Extended trust (optional)

- **`git.relayTrust`** / **`branchRules`** / signed commits — see **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)** and **[`examples/preset-repo.relay.yaml`](./examples/preset-repo.relay.yaml)**.

---

## Planned: branch ref names vs HTTP host slugs

**Not implemented yet** — **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §9: **`pre-receive`** rejects branch updates that produce **empty** or **duplicate** canonical slugs so **Host**-based branch routing stays unambiguous while allowing most branch strings.

---

## Planned: working-tree size cap (non-authorized repos)

**Not implemented yet** — tracked with **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** §6.

- Repos **listed** in **`authorized-repos.yaml`**: keep **full** anchor and pull rules; **no** 50 MB working-tree cap (subject to disk/ops limits).
- Repos **not** in that allowlist: cap total **default-branch working tree** size (files that would be served), **ignoring `.git` object history**, at **50 MB** until policy changes.

---

## Planned: push-time bare provisioning

Stock **`git daemon`** cannot create missing bare paths on first connect. See **[`README.md`](../README.md)** (Git daemon). A future gateway may create **`RELAY_REPO_PATH/<name>.git`** when policy allows, still tied to catalog / YAML rules.

---

## Implementation status

| Piece | Status |
|-------|--------|
| **`relay-master`** catalog repo (**`relay-catalog.json`** + **`relayRootCommit`**) | **In tree** — **[`relay-master`](../../relay-master)** |
| **`scripts/sync-relay-catalog.sh`** (clone/fetch + root check + YAML emit) | **In tree** |
| **`RELAY_SERVER_ID` + allowlist** | **Enforced** at startup (**`RELAY_MASTER_CATALOG_PATH`** or **`RELAY_AUTHORIZED_REPOS_PATH`**) |
| **`RELAY_MASTER_TRUST_ROOT`** | **Optional**; when set, verifies catalog checkout lineage at startup |
| **Pull allowlist + anchor validation** | **Enforced** on **`POST /git-pull`** |
| **Bootstrap anchor check** | **Enforced** in **`relay-bootstrap.sh`** (manifest) |
| **relay-gateway reads `relay-catalog.json`** | **Implemented** via **`RELAY_MASTER_CATALOG_PATH`** |
| **`allowedKeys` fingerprint match on push** | Planned (hook handler) |
| **Anchor enforcement on `git push`** | Planned — **`pre-receive`** |
| **Signed commits on `relay-master`** | Planned (process + optional hooks) |
| **50 MB default-branch working-tree cap (non-authorized only)** | Planned — **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** |
| **Mesh event bus (IRC-style links, notify → pull)** | Planned — **[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)** |

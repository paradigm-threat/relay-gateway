# Catalog rescue (relay-master tip apply)

When a repo loses its sole **`adminKeys`** signer, or operators need to replace a bare mirror’s trusted root, **Relay does not** use GitLab or email recovery. The supported path is a **network-admin signed push** to **`relay-master`** that updates **`relay-catalog.json`**, then **automatic catalog apply** on each master.

**GitLab / forge:** detached from mesh and rescue automation. Forge backup remains **manual ops** outside Relay.

## What catalog apply does

On **`relay-master.git`** tip change (validated **`post-receive`** or **`POST /hooks/peer`** peer-fetch), [`src/git/catalog_apply.rs`](../src/git/catalog_apply.rs):

1. Diff **`relay-catalog.json`** at **`old..new`** tip.
2. **Removed slug** → delete **`$RELAY_REPO_PATH/<slug>.git`**.
3. **Added or changed entry** → remove old bare, **`git clone --bare`** from catalog **`url`**, optional shallow depth (**`RELAY_CATALOG_CLONE_DEPTH`**, default `1`), **`git fetch --deepen`** if **`relayRootCommit`** missing (**`RELAY_CATALOG_AUTO_DEEPEN`**, default `256`), set branch head to **`relayRootCommit`**, warm SQLite index when the repo has an index hook.
4. Slack audit via **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`** (best-effort).
5. Mesh **next-hop** propagates the **`relay-master`** tip; each hop runs the same apply.

Idempotent: re-notify with the same **`new_sha`** skips fetch; catalog apply compares entry equality before recloning.

## Operator runbook

### Prerequisites

- Active **network admin** SSH key listed in **`relay-master`** (or target repo) **`adminKeys`**.
- Catalog entry records a **bootstrap URL** peers can clone (often **`git://<peer>:9418/<repo>.git`**, not forge).
- Fleet on current gateway build with catalog apply wired (**`relay-hook-handler`** + **`relay-gateway`**).

### Steps

1. **Prepare catalog change** in a **`relay-master`** worktree: edit **`relay-catalog.json`** (or run **`scripts/build-relay-catalog-json.sh`** after **`catalog/repos/<slug>.json`**), set **`url`**, **`relayRootCommit`**, **`branch`** for the affected slug.
2. **Sign and push** to **`relay-master`** `main` (or configured publish branch) on one master.
3. **Verify receiving master:** journal for `catalog apply`; check **`$RELAY_REPO_PATH/<slug>.git`** exists and **`refs/heads/<branch>`** matches **`relayRootCommit`**.
4. **Mesh propagation:** confirm next-hop **`POST /hooks/peer`** chain (default **`notifyMode: next`**) updates other masters; use **`relay-fleet.sh drift`** on **`relay-master`** tips.
5. **Index warm:** optional **`POST https://<host>/api/index/warm/<slug>`** if QUERY lags; catalog apply already warms when index hooks exist.
6. **Smoke:** **`relay-fleet.sh health`** or **`validate-relay-query.mjs`** on the repo.

## Deploy order (mesh + catalog apply)

1. **Gateway binaries** — `./scripts/pre-deploy-check.sh` then `./scripts/relay-fleet.sh --build deploy-all`.
2. **`relay-master` config** — signed push of **`.relay.yaml`** with **`git.origins`** + **`adminKeys`** (separate from gateway tarball; mesh ring must match **`RELAY_MASTER_PEER_LIST`** on each master).
3. **Smoke** — push to **`relay-master`** or a content repo; confirm next-hop peer notify and (if catalog changed) bare mirror apply on receiving master.

### Lost sole admin (no forge backdoor)

1. Owner contacts network admin out-of-band.
2. Admin uses a **cold / backup `adminKeys`** line (recommended on every repo that sets **`adminKeys`**) **or** signs from an existing admin key still held by ops.
3. Admin updates catalog trust (**`relay_root_commit`**, **`signing_keys`**) via signed **`relay-master`** PR/push — not by weakening **`branchRules`** on the content repo.
4. Catalog apply reclones content mirrors from peer URLs in the catalog; editors re-enroll keys in catalog / **`.relay.yaml`** as needed.

There is **no** email recovery and **no** automated GitLab pull in this path.

## Related

- **[`RELAY_CATALOG.md`](./RELAY_CATALOG.md)** — catalog record schema.
- **[`RELAY_FLEET_CONSOLE.md`](./RELAY_FLEET_CONSOLE.md)** — mesh origins, **`health`**, **`drift`**, deploy.
- **[`RELAY_BRANCH_POLICY_OPERATOR.md`](./RELAY_BRANCH_POLICY_OPERATOR.md)** — **`adminKeys`** vs **`allowedKeys`**.
- **[`OPERATOR_BARE_REPO_RESET.md`](./OPERATOR_BARE_REPO_RESET.md)** — manual **`reclone-repo`** when catalog apply is not involved.

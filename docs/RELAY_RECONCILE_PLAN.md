# relay-master as Helm (reconcile plan)

**Status:** **shipped** — **`scripts/relay-reconcile.sh`**, **`relay-reconcile.service`**, **`relay-reconcile.timer`**, **`/opt/relay/etc/reconcile-allowlist`**, manifest **`relay-master/deploy/relay-gateway.json`**.

Implements the “chart + values” model: **relay-master** declares desired **relay-gateway** artifact version and optional **reconcile scripts**; each master node **pulls** default branch, verifies **signatures** and **allowlists**, then runs **`install.sh update`** under **root**. Rollback = move the manifest to an older **signed** commit on the default branch and re-run reconcile.

## Decisions (agreed)

| Topic | Choice |
|-------|--------|
| Trigger | **Pull** (timer or wake after boot) + **default branch** only |
| Trust | **Signed merges** only (same key material as **`git.branchRules`** / [RELAY_BRANCH_POLICY.md](./RELAY_BRANCH_POLICY.md)) |
| Script eligibility | Path must match **server allowlist** and **SHA-256** of file at that commit (allowlist + hash on disk and/or in **signed** catalog material) |
| `relay.env` | **Root-writable only** after install (`chmod 600` `root:root`); vars reach **`relay`** via systemd `EnvironmentFile` (see `install.sh`) |
| Naming | Production HTTP binary is **`relay-gateway`** (GitLab package / tarball); legacy **`relay-server`** name is retired |

## Operator setup

1. **Build / host** `relay-linode-deploy.tgz` and record **SHA-256** (e.g. `sha256sum`).
2. In **relay-master**, edit **`deploy/relay-gateway.json`**: set **`git_tag`**, **`artifact_url`**, **`artifact_sha256`** (lowercase hex). Commit (signed) and push **`main`**.
3. On each master **`relay.env`**: **`RELAY_RECONCILE_ENABLED=1`**. Optionally **`RELAY_RECONCILE_ENABLE_TIMER=1`** then run **`install.sh update`** (or **`repair`**) so **`relay-reconcile.timer`** is enabled.
4. **Canary / fleet:** If **`canary_hostname`** is set to one node’s **`RELAY_PUBLIC_HOSTNAME`**, other nodes wait until **`deploy/canary-complete.json`** exists in the repo with the same **`artifact_sha256`** (copy from **`deploy/canary-complete.example.json`** after canary succeeds). Commit and push.

**Manual run:** `sudo /opt/relay/bin/relay-reconcile.sh`

## Manifest schema (`deploy/relay-gateway.json`)

| Field | Meaning |
|-------|---------|
| **`git_tag`** | Tag on **relay-gateway** repo (informational). |
| **`artifact_url`** | HTTPS URL to **exact** tarball (generic package). |
| **`artifact_sha256`** | Lowercase hex SHA-256 of tarball. Empty = reconcile skips binary install (no-op). |
| **`reconcile_script`** | Optional path under relay-master (e.g. `scripts/deploy/foo.sh`). Must appear in **`/opt/relay/etc/reconcile-allowlist`** with blob SHA-256. |
| **`canary_hostname`** | If set, only this **`RELAY_PUBLIC_HOSTNAME`** runs until **`deploy/canary-complete.json`** matches **`artifact_sha256`**. |
| **`skip_binary_download`** | If **`true`**, run **`reconcile_script`** only (requires script); no tarball. |

## Artifact identity (recommendation)

Pin **three** fields:

1. **`git_tag`** — human meaning.
2. **`artifact_url`** — **immutable** GitLab generic package URL for that tag.
3. **`artifact_sha256`** — checksum before unpack.

Rollback: set the triple to a previous release; reconcile again.

## `.relay.yaml` / branch policy (point 5)

Per [RELAY_BRANCH_POLICY.md](./RELAY_BRANCH_POLICY.md): missing **`.relay.yaml`**, missing **`git.branchRules`**, or uncovered branch → push **rejected**. Protected **`main`**: signed commits. **HTTP PUT** follows the same rules.

## Coordinated multi-master upgrade (point 6)

**Goal:** Avoid fan-out of a bad manifest before one node validates.

**Implemented:** **`canary_hostname`** + **`deploy/canary-complete.json`** (same **`artifact_sha256`**). See **Operator setup** above.

**Alternatives** (manual): promote GitLab tag after canary; or add **`approved_for_fleet`** in a later revision.

## Failure and rollback (recommendation)

| Outcome | Action |
|---------|--------|
| Download/hash mismatch | **Abort**; no **`state/last_reconcile.json`** update on failure path. |
| `install.sh` fails | **Abort**; previous binaries remain. |
| Install OK, service unhealthy | Optional: reinstall previous manifest commit; see **`state/last_reconcile.json`**. |

## Allowlist governance

- **On disk:** **`/opt/relay/etc/reconcile-allowlist`** (root, `chmod 600`), seeded by **`install.sh`**.
- **Format:** `path<TAB>sha256_hex` (path = relay-master-relative).
- **Changes:** via **`install.sh`** or admin edit as **root**; not writable by **`relay`** via **`relay.env`**.

## Implementation (done)

| Piece | Location |
|-------|----------|
| Reconcile script | **`relay-gateway/scripts/relay-reconcile.sh`** → **`/opt/relay/bin/relay-reconcile.sh`** |
| Systemd | **`relay-reconcile.service`** (oneshot), **`relay-reconcile.timer`** (~15 min + boot delay) |
| State | **`/opt/relay/state/last_reconcile.json`** |
| Manifest | **`relay-master/deploy/relay-gateway.json`** |
| Packaged in tarball | **`pack-linode-deploy.sh`** includes **`relay-reconcile.sh`** |

## Outstanding / later

- Multi-branch manifests; automatic rollback; mesh “approved” signal beyond **`canary-complete.json`**; CI that publishes generic packages and updates **`artifact_sha256`** automatically.

## Related

- [RELAY_BRANCH_POLICY.md](./RELAY_BRANCH_POLICY.md) — `.relay.yaml` and signing  
- [DEPLOY_LINODE.md](./DEPLOY_LINODE.md) — tarball install and `relay.env` permissions  
- [DEPLOY_SELF_HOSTED.md](./DEPLOY_SELF_HOSTED.md) — `RELAY_DEPLOY_TGZ_URL`

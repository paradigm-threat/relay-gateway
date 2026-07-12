# Relay branch policy (`.relay.yaml`) — operator guide

Fleet **`relay-hook-handler`** runs **`pre-receive`** checks from [`src/git/branch_policy.rs`](../src/git/branch_policy.rs): every commit in the Git range **`old_oid..new_oid`** must have a valid root **`.relay.yaml`** with **`git.branchRules`** covering the branch (and signing rules when configured).

## Why GitLab accepts but fleet rejects

- **GitLab** does not run relay’s hook; pushes can land without per-commit relay policy.
- **Catalog sync** on masters uses **`git fetch`** from the forge into bare repos — **fetch does not invoke `pre-receive`** on the relay git daemon.
- **`git push`** to **`git://<master>:9418/<repo>.git`** does invoke **`pre-receive`**. If the bare **`main`** tip is **behind** GitLab’s, your push range can include **many** commits; **each** must pass policy. A single legacy commit without policy blocks the whole push.

See also [RELAY_FLEET_CONSOLE.md](./RELAY_FLEET_CONSOLE.md) (**`drift`**, **`sync-relay-catalog.sh`**).

## Workflow norms (prevention)

1. Treat **GitLab as canonical** for catalog repos; keep masters updated via **`sync-relay-catalog.sh`** so **`catalog_forge_sync_ok`** is healthy and **`relay-fleet.sh drift`** does not show forged vs bare mismatches.
2. Avoid large **direct** pushes to fleet when mirrors lag — prefer **forge-first**, then let masters **fetch**.
3. From repo creation, commit a valid **`.relay.yaml`** with **`git.branchRules`** so history never contains policy gaps.

## Local validation (same logic as the server)

From **`relay-gateway/`** after **`cargo build --release --bin relay-branch-policy-check`**:

```bash
# Full ancestry of main (after filter-repo / history repair — strongest guarantee)
./target/release/relay-branch-policy-check walk /path/to/repo -b main

# Linear main only (merge-heavy repos)
./target/release/relay-branch-policy-check walk /path/to/repo -b main --first-parent

# Exact window the server checks when updating a branch (not a brand-new ref)
./target/release/relay-branch-policy-check range /path/to/bare/repo.git <OLD_SHA> <NEW_SHA> main
```

Signing: if **`branchRules`** require SSH-signed commits, **`git verify-commit`** must succeed locally (same as on masters). When **`RELAY_MASTER_CATALOG_PATH`** is set and the repo has **`signing_keys_source: catalog`** in [`catalog/repos/<repo>.json`](../../relay-master/catalog/repos/), keys come from the catalog record (verify = active + disabled; authorize = active only). Repo **`allowedKeys`** still apply during migration (`signing_keys_source: repo`). See [`RELAY_CATALOG.md`](./RELAY_CATALOG.md).

## Admin vs editor keys (`adminKeys`)

| Key field | Who | What it gates |
|-----------|-----|----------------|
| **`allowedKeys`** | Editors | Normal commits and content changes on protected branches. |
| **`adminKeys`** | Network / repo admins | **Privileged** **`.relay.yaml`** diffs: **`git.origins`**, **`notifyMode`**, **`autoPush`**, **`branchRules`** / key lists, **`server.hooks`**. Relay enforces a **floor** — repos cannot shrink this set. |

Privileged changes require a signature from **`adminKeys`** on the **parent** tip’s branch rule (see [`src/git/privileged_policy.rs`](../src/git/privileged_policy.rs)).

**Cold admin key:** store a backup SSH public line in **`adminKeys`** (offline or separate machine) on every repo that enables admins. If the sole online admin key is lost, recovery is **[catalog rescue](./RELAY_CATALOG_RESCUE.md)** only — not GitLab, not email reset.

Example:

```yaml
git:
  branchRules:
    default:
      requireSigned: true
      allowUnsigned: false
      allowedKeys:
        - "ssh-ed25519 AAAA... editor"
      adminKeys:
        - "ssh-ed25519 AAAA... primary-admin"
        - "ssh-ed25519 AAAA... cold-backup-admin"
```

**Catalog PRs (`relay-master`):** require forge approval; commits signed with **active** keys; run **`scripts/check-catalog-pr-signing-keys.sh`** before merge.

## Unblocking options

| Approach | Notes |
|----------|--------|
| **Align mirrors** | Bring bare **`main`** to forge **`main`** via sync; small **`old..new`** pushes may skip ancient SHAs in the window — does **not** fix bad commits still in range. |
| **Rewrite history** | **`git filter-repo`** (or similar) so **every** commit on **`main`** carries valid policy; **`walk`** passes; force-push forge; refresh masters (**fetch** or **`reclone-repo`** per [OPERATOR_BARE_REPO_RESET.md](./OPERATOR_BARE_REPO_RESET.md)). |
| **Hook / policy change** | Relay maintainers only — e.g. Policy tweaks or OID allowlists; trade-offs vs strict governance. |

## GitLab CI (optional)

Run **`relay-branch-policy-check walk`** (or **`range`** on MR merge bases) in CI using a job image that contains the built binary or **`cargo run --bin relay-branch-policy-check`** against a checkout.

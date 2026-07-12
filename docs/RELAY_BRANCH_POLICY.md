# Relay branch policy (`.relay.yaml`)

`relay-hook-handler` `pre-receive` and HTTP PUT/DELETE enforce **`git.branchRules`** from **each commit’s** `.relay.yaml` when updating `refs/heads/*`.

**Policy I:** each push verifies only commits in **`old..new`** (newly introduced), not full ancestry. See [RELAY_AUTH_PROTOCOL.md](./RELAY_AUTH_PROTOCOL.md) and [RELAY_POLICY_MIGRATION.md](./RELAY_POLICY_MIGRATION.md).

**New ref (`old` all zeros):** the range is **`merge-base(refs/heads/main or master, new)..new`** so the first push of e.g. **`develop`** does not replay ancient history that predates `.relay.yaml`. Orphan histories (no merge-base with `main`/`master`) still use the full `rev-list` of `new`.

## Rules

- **Missing `.relay.yaml`** at a commit in the push range → reject.
- **No `git.branchRules`** in that file → reject.
- **Branch resolution**: exact `branches[].name` match, then **`name: "*"`**, then optional `default`. If nothing matches → reject (branch not covered).
- **`*` rule**: typically `requireSigned: true`, `allowUnsigned: false`, and **`allowedKeys`** (SSH public key lines).
- **`develop`** (or similar): `allowUnsigned: true` for unsigned commits on that branch only.
- **Governance**: if `.relay.yaml` changes, the commit must still verify with the **parent’s** allowlist when the parent branch rule required signatures (so new keys are added only by already-trusted signers).
- **Privileged sections** (`git.origins`, `notifyMode`, `autoPush`, `branchRules` / keys, `server.hooks`): when the diff touches these, the commit must be signed by a key in the parent’s **`adminKeys`** (not merely **`allowedKeys`**). Repos cannot disable this floor. See **[`RELAY_BRANCH_POLICY_OPERATOR.md`](./RELAY_BRANCH_POLICY_OPERATOR.md)** and **[`RELAY_CATALOG_RESCUE.md`](./RELAY_CATALOG_RESCUE.md)**.

## Verification

Uses `git verify-commit` with a temporary `gpg.ssh.allowedSignersFile` built from `allowedKeys`.

## HTTP PUT

Same branch rules: unsigned server commits are **rejected** on branches that require signatures; only branches with `allowUnsigned: true` accept PUT.

## Peer fetch

`fetch_from_peer_url` runs the same policy over `before..after` (not the forge-ingest exception). The fetch refspec does **not** use a leading `+`, so updates are **fast-forward only**.

## Bare repos

`POST /api/git/ensure-bare/:repo` sets **`receive.denyNonFastForwards=true`** so `git push` to relay cannot rewrite branch history without fast-forward.

## Merge note

Pushing a **merge** to `main` may include many commits in `old..new`; each must satisfy **`main`’s** rule from that commit’s tree. Prefer **squash merge** or **sign every commit** on feature branches when merging into protected branches.

## Related: server reconcile (relay-master → masters)

Automated **install / reconcile** from **relay-master** (default branch, signed) is specified in **[RELAY_RECONCILE_PLAN.md](./RELAY_RECONCILE_PLAN.md)**. It relies on the same signing expectations as **`git.branchRules`** above.


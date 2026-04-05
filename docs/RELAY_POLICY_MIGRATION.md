# Migrating repositories to signed commits

Relay enforces **Policy I** (see [RELAY_BRANCH_POLICY.md](./RELAY_BRANCH_POLICY.md)): on each push, every **new** commit in `old..new` must satisfy that commit’s snapshot of `.relay.yaml` for the destination branch. Merging pre-existing unsigned history is allowed when those commits are not in the introduced range (the **merge commit** must still be signed if the branch requires it).

## Archive + signed merge (typical)

1. **Archive** the current tip: create `archive/unsigned-main` (or similar) pointing at today’s `main`.
2. Add `.relay.yaml` with `git.branchRules`: `name: "*"` (signed + `allowedKeys`) and `name: develop` with `allowUnsigned: true` if you use a develop line for unsigned work.
3. **First commit on the new secured line** on `main`: either an orphan reset coordinated with the team, or a **signed merge** from `archive/unsigned-main` into `main` using `git merge -S` so the merge commit satisfies `main`’s rule.
4. Remove temporary relaxed rules once migration is complete.

## Existing bare repos on relay servers

`POST /api/git/ensure-bare/:repo` sets **`receive.denyNonFastForwards=true`** on create and on repeat calls. Repos created before that change should be updated once:

```bash
git -C /path/to/repo.git config receive.denyNonFastForwards true
```

## Operator note

**`git push --force`** to relay’s `git://` service is incompatible with append-only verification; use the forge for history repair, then sync via catalog ingest or a signed merge — not force-push to relay.

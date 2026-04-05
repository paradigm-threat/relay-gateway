# Relay auth and trust (normative summary)

This document ties together signing policy, ingest paths, and HTTP writes. The authoritative implementation is **`relay-hook-handler`** (`pre-receive`), **`handlers/write`** (PUT/DELETE), **`peer_hook`** (peer fetch), and catalog sync (forge URL) as documented in [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md).

## Per-commit policy

For ref updates to `refs/heads/*`, policy is read from **`.relay.yaml` at each commit `c`** in `git rev-list old..new` (oldest first). Missing file or uncovered branch → **reject**. Branch rule resolution: **exact name** → **`name: "*"`** → optional **`default`** → else reject.

## Policy I vs II

- **Policy I (implemented):** Verify every commit **newly introduced** by the push (`old..new`). Unsigned objects already reachable from `old` are not re-checked.
- **Policy II (not default):** Require every commit reachable from the tip to be signed — incompatible with merging legacy unsigned history without rewrite.

## Signing technology

SSH commit signing (`gpg.format=ssh`, `user.signingkey`, server-side temp `gpg.ssh.allowedSignersFile` from `allowedKeys`) is the primary path. OpenPGP may be added later.

## HTTP PUT / DELETE

Same branch rules as `git push`: server-created unsigned commits are allowed only when the resolved rule allows unsigned commits (e.g. `develop` with `allowUnsigned: true`).

## Ingest

- **Forge / catalog `url` fetch:** May introduce commits without per-commit relay signing checks; trust is TLS + forge ACL + `relayRootCommit` / anchor validation where configured.
- **Peer `git://` fetch:** Same signing policy as push (see `fetch_from_peer_url`); refspec is **fast-forward only** (no `+` force fetch).

## Fast-forward only

Bare repos should set **`receive.denyNonFastForwards=true`** (applied by `ensure-bare`). Peer **auto-push** uses **`git push` without `--force`**. Resolve divergence with merges on the forge or signed merges — not force-push to relay.

## Key governance

Changes to allowed key lines in `.relay.yaml` must be **signed by a key already on the parent’s allowlist** when the parent rule required signatures (see `branch_policy` governance check).

## Local parity

Use [scripts/verify-push-range.sh](../scripts/verify-push-range.sh) as a quick client check; for full allowlist parity, mirror server `allowedKeys` materialization.

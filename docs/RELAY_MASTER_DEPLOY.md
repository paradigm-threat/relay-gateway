# Deploying relay-master after a history reset

After resetting **`relay-master`** to a single signed commit (or any fast-forward that changes `HEAD`):

1. **Build and install** the new `relay-gateway` binary and `relay-hook-handler` on **each** relay node (see `DEPLOY_LINODE.md`).
2. **On each node**, update the **`relay-master`** checkout or bare mirror:
   - `cd $RELAY_MASTER_CATALOG_PATH` (or `RELAY_REPO_PATH/relay-master.git` as appropriate)
   - `git fetch origin`
   - `git reset --hard origin/main` (or `git pull --ff-only` if using a worktree)
3. Run **`scripts/sync-relay-catalog.sh`** so bare mirrors and optional `authorized-repos.yaml` match the catalog.
4. **Restart** `relay-gateway` (or full stack) so hooks pick up the new binary.

## Force push

Operators must **`git push --force origin main`** from the repo that was reset (coordinate with others). Forge branch protection may need temporary override.

## Signing locally

**Recommended layout:** private key in **`~/.ssh/`**, public **`*.pub`** beside it, public line also in **`.relay.yaml`** `allowedKeys`.

```bash
# One-time: keypair (or move an existing private key into ~/.ssh)
#   ~/.ssh/relay-catalog-signing-ed25519
#   ~/.ssh/relay-catalog-signing-ed25519.pub   (ssh-keygen -y -f ... > .pub)
chmod 600 ~/.ssh/relay-catalog-signing-ed25519

cd /path/to/relay-master
git config gpg.format ssh
git config user.signingkey "$HOME/.ssh/relay-catalog-signing-ed25519.pub"
git config commit.gpgsign true
git config merge.gpgsign true

ssh-add ~/.ssh/relay-catalog-signing-ed25519   # each login/session (or use ssh-agent/keychain)
```

After that, ordinary **`git commit`** and **`git merge`** sign automatically; use **`git commit --no-gpg-sign`** only when you intentionally need an unsigned commit (e.g. **`develop`** if policy allows).

Optional local verify: set **`gpg.ssh.allowedSignersFile`** to a file containing the same line as **`allowedKeys`**, then **`git verify-commit HEAD`**.

Keep **private** keys out of the repo (see **`relay-master/.gitignore`** for the old in-tree name).


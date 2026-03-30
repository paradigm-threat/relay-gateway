# Relay trust, authorized repos, and bootstrap

## Minimum requirements to launch a Relay server

Every production Relay node **MUST** have:

| # | Requirement | How |
|---|-------------|-----|
| **1** | **Unique server name** across the mesh | Set **`RELAY_SERVER_ID`** (e.g. `relay-atlanta1`). Same value must appear in **`authorized-repos.yaml`** as **`relay_server_id`**. |
| **2** | **Authorized repos + identifying hash** | YAML at **`RELAY_AUTHORIZED_REPOS_PATH`** listing each allowed bare repo name and **`anchor_commit`** (full 40-char Git object id). Every **`POST /git-pull`** (and bootstrap clone) must result in a branch tip that **contains that commit in its history**. |

Without (1)+(2), **`relay-server` will refuse to start** if **`RELAY_AUTHORIZED_REPOS_PATH`** is set.

### What the hash means

- **`anchor_commit`** is a **trust anchor**: a commit you vouch for (e.g. release tag, audited baseline).
- After any **pull**, the server checks: `anchor_commit` **is an ancestor** of `refs/heads/<branch>`. If someone points `origin` at a fork or rewrote history without your anchor, the pull **fails** and **`refs/heads/main`** is rolled back (HTTP **403**).
- When you **advance** the trusted line, update **`anchor_commit`** in the YAML (signed ops process) to a new commit that is a **descendant** of the old anchor.

### Whitelist slots: store the hash **before** the bare repo exists (anti-squat)

For every **whitelisted** repo name, **`authorized-repos.yaml` MUST list `anchor_commit` + `branch` even when** `RELAY_REPO_PATH/<name>.git` **does not exist yet**.

**Why:** Without a pre-declared anchor, an attacker could **`git push`** to `git://node:9418/<whitelisted-name>.git` first and publish **unrelated** history under **your** official name. After that, your real mirror is no longer uniquely tied to the name. The stored hash defines **which Git history** is allowed to occupy that slot: the configured branch tip must **contain `anchor_commit` in its ancestry** (same rule as pull validation).

**Operations:**

1. Choose a trusted tip on the real repo (e.g. `main`): `git rev-parse refs/heads/main`.
2. Put that full 40-character id in **`anchor_commit`** for that repo name **on every node** before or as soon as the name is exposed (DNS / routing).
3. **First push** or **pull** that creates or updates that bare repo must satisfy the anchor check.

**Implementation note:** **`relay-server` enforces the anchor today on `POST /git-pull`** after fetch. **Git daemon `receive-pack`** pushes should be guarded the same way in **`relay-hook-handler` / `pre-receive`** (planned) so squatting cannot succeed via push before your clone exists. Until that ships, restrict **9418** at the firewall to trusted peers if whitelisted names are public.

### Example file

See **[`examples/authorized-repos.yaml`](./examples/authorized-repos.yaml)**.

```yaml
relay_server_id: relay-atlanta1
repos:
  snesology-presets:
    anchor_commit: "abc123…"   # 40 hex chars
    branch: main
```

### Runtime env

```bash
export RELAY_SERVER_ID=relay-atlanta1
export RELAY_AUTHORIZED_REPOS_PATH=/opt/relay/authorized-repos.yaml
```

`systemd` **`EnvironmentFile=`** should include both.

---

## Canonical hosting (no GitHub for presets)

Preset libraries are **canonical on Relay**. Seed with **`git push git://…:9418/…`**, then other nodes **bootstrap** from the seed. See **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)**.

---

## API

**`GET /api/config`** returns **`relay_server_id`** and **`authorized_repos`** (names only, not hashes).

---

## Bootstrap script

**`scripts/relay-bootstrap.sh`** — manifest must include **`relay_server_id`** matching **`RELAY_SERVER_ID`**. Each **`bareRepos[]`** entry may include **`anchorCommit`** and **`branch`**; same ancestor check runs after clone/fetch.

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
- **Anchor checks** remain **standard** on all relay nodes for allowlisted repos; per-repo behavior otherwise still comes from **`.relay.yaml`** in Git, **except** for config files **written only by `install.sh` / `update`** (paths to be documented when implemented).

---

## Implementation status

| Piece | Status |
|-------|--------|
| **`RELAY_SERVER_ID` + authorized-repos file** | **Enforced** at startup |
| **Pull allowlist + anchor validation** | **Enforced** on **`POST /git-pull`** |
| **Bootstrap anchor check** | **Enforced** in **`relay-bootstrap.sh`** (manifest) |
| **`allowedKeys` fingerprint match on push** | Planned (hook handler) |
| **Anchor enforcement on `git push` (whitelisted names; anti-squat)** | Planned — **`pre-receive`** using same ancestry rule as pull; see **Whitelist slots** above |
| **50 MB default-branch working-tree cap (non-authorized only)** | Planned — see **[`DEPLOY_WEB_AND_CI.md`](./DEPLOY_WEB_AND_CI.md)** |

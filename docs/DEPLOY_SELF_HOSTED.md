# Self-hosted packaging and deploy (no cloud CI)

Relay is **source-first**: you build the deploy tarball **where you trust** (laptop, **GitLab CI** job `package:linode-tgz` in **`.gitlab-ci.yml`**, or another runner you control), then install on the node without GitHub Actions.

**Related:** **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** (runtime install), **[`scripts/pack-linode-deploy.sh`](../scripts/pack-linode-deploy.sh)** (build pack), **[`scripts/relay-curl.sh`](../scripts/relay-curl.sh)** (one-liner bootstrap), **`scripts/gitlab-ci-lint.sh`** (optional CI YAML lint via API).

---

## Recommended flows

### A — One-liner from GitLab (binaries required)

Host **`relay-linode-deploy.tgz`** on **any HTTPS URL you control** (download from a **GitLab pipeline artifact**, generic package, internal nginx, object storage). Then:

```bash
curl -fsSL "https://gitlab.com/clevertree/relay-gateway/-/raw/main/scripts/relay-curl.sh" | \
  sudo env RELAY_DEPLOY_TGZ_URL="https://YOUR_HOST/relay-linode-deploy.tgz" bash -s -- install
```

Set **`RELAY_RAW_BASE`** if you mirror the repo (see **`relay-curl.sh`** header).

### B — Clone repo on server + install from vendored binaries

On a **build machine** (same arch as the server, e.g. `x86_64` → `linux-amd64`):

```bash
./scripts/pack-linode-deploy.sh
mkdir -p dist/linux-amd64
cp target/release/relay-server target/release/relay-hook-handler dist/linux-amd64/
# Copy the whole relay-server tree (or rsync) to the server, including dist/linux-amd64/
```

On the **server** (as root, from repo root):

```bash
sudo ./scripts/install-from-checkout.sh install
```

This uses **`dist/linux-amd64/`** or **`dist/linux-arm64/`** only — **no** `cargo` on the server.

### C — Clone on server and compile there (fallback)

If you accept Rust on the node:

```bash
./scripts/pack-linode-deploy.sh   # produces target/relay-linode-deploy.tgz
sudo tar -xzf target/relay-linode-deploy.tgz -C /root/relay-dist
sudo RELAY_BIN_SOURCE=/root/relay-dist ./scripts/relay-install.sh install
```

(`pack-linode-deploy.sh` runs `cargo build --release`.)

---

## Vendored binaries **in git** — pros and cons

| Pros | Cons |
|------|------|
| Single **`git clone`** gives a runnable install path with no separate artifact host. | **Repo bloat** (tens of MB per arch); clones and CI slower. |
| Works **air-gapped** if the clone is already on disk. | **Platform-specific**: need separate blobs per **arch** (amd64, arm64) and sometimes **glibc** version. |
| No dependency on GitLab “releases” UI. | **Security / review**: opaque blobs; supply-chain and diff noise unless you use **Git LFS** + policies. |
| | **History**: every upgrade rewrites large objects unless you use **LFS** or a **subtree** only on release branches. |

**Practical recommendation:** keep **source** on `main`; build with **`./scripts/pack-linode-deploy.sh`**; publish **`relay-linode-deploy.tgz`** via **GitLab generic package**, **internal HTTP**, or **scp**. Only **vendor `dist/` in git** for small teams or air-gap forks, ideally on a **`release/binaries`** branch or LFS.

---

## Build the tarball locally (canonical)

```bash
./scripts/pack-linode-deploy.sh
# -> target/relay-linode-deploy.tgz
```

Copy to the server and:

```bash
tar xzf relay-linode-deploy.tgz && sudo ./install.sh install
```

(Or use **`relay-curl.sh`** with **`RELAY_DEPLOY_TGZ_URL`**.)

---

## `relay-curl.sh` without GitHub

Defaults use **GitLab raw** (`RELAY_RAW_BASE`). Override if you self-host:

```bash
export RELAY_RAW_BASE="https://git.example.com/org/relay-server/-/raw/main"
```

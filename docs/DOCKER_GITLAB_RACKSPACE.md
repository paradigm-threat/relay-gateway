# Docker image — GitLab CI and Rackspace Spot

## Image

| Item | Value |
|------|--------|
| Dockerfile | `relay-gateway/Dockerfile.production` |
| Registry | `registry.gitlab.com/clevertree/relay-server/relay-gateway` |
| Tags | `main`, `$CI_COMMIT_SHORT_SHA`, `$CI_COMMIT_TAG` |

Build locally from monorepo root (`~/dev` layout: `relay/`, `relay-runtime/`, `md2ast/`):

```bash
cd relay-gateway
./scripts/docker-build-production.sh
```

Standalone `relay-gateway` checkout:

```bash
./scripts/ci-clone-path-deps.sh
RELAY_IMAGE=relay-gateway:local ./scripts/docker-build-production.sh
```

## GitLab CI

Pipeline (`.gitlab-ci.yml` at repo root):

1. **test:verify** — clone path deps, `cargo test --locked`, smoke `POST /api/config`
2. **package:linode-tgz** — Linode tarball artifact on `main` / tags
3. **docker:prepare-context** — stage `relay-gateway/` + `relay-runtime/` + `md2ast/` (artifact)
4. **docker:publish** — Kaniko push to Container Registry on `main` / tags

### One-time GitLab project setup

| Step | Where | Action |
|------|--------|--------|
| Container Registry | **Deploy → Container Registry** | Enable (default on GitLab.com) |
| CI/CD permissions | **Settings → CI/CD → Token Access** | Allow `relay-runtime` and `md2ast` projects to be cloned via `CI_JOB_TOKEN` (same group), **or** make those repos public |
| Protected `main` | **Settings → Repository** | Optional: only maintainers merge to `main` so `:main` image tag stays trusted |
| Runner | **Settings → CI/CD → Runners** | Shared GitLab.com runners are enough |

GitLab injects `CI_REGISTRY_USER`, `CI_REGISTRY_PASSWORD`, and `CI_JOB_TOKEN` automatically — no manual registry secrets required for Kaniko push.

Optional overrides:

| Variable | Purpose |
|----------|---------|
| `RELAY_PATH_DEPS_REF` | Branch/tag for `relay-runtime` + `md2ast` clones (default `main`) |
| `RELAY_RUNTIME_GIT_URL` | Override runtime clone URL |
| `MD2AST_GIT_URL` | Override md2ast clone URL |

### Fix a failed pipeline

| Symptom | Fix |
|---------|-----|
| `Cargo.lock needs to be updated` | Run `cargo update` locally in `relay-gateway`, commit `Cargo.lock` |
| `could not read Username` cloning deps | Enable **CI job token** access to sibling repos in the same group |
| Kaniko `git: not found` | Use current `.gitlab-ci.yml` (`docker:prepare-context` job) |
| Image pull 404 on Rackspace | Image must be `registry.gitlab.com/clevertree/relay-server/relay-gateway:main` (note `/relay-gateway` suffix) |

## Rackspace Spot (Kubernetes)

Manifests: `terraform/rackspace-spot/k8s/`.

**Full deploy** (terraform + DaemonSet + atlanta3 DNS when spot nodes get an IP):

```bash
./terraform/rackspace-spot/scripts/rackspace-deploy.sh
```

Roll image only (kubeconfig must exist):

```bash
SKIP_TERRAFORM=1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
```

When spot bids win, the dfw1 relay pod upserts **`atlanta3.relaygateway.net`** (via `RELAY_EXTRA_DNS_FQDNS` after the next image publish) and the deploy script calls `vercel-dns-upsert.sh` as a belt-and-suspenders step. Set `VERCEL_API_TOKEN` + `VERCEL_TEAM_ID` or log in with the Vercel CLI.

Legacy one-liner (registry pull + apply only):

```bash
export REGISTRY=registry.gitlab.com/clevertree/relay-server
export RELAY_IMAGE_TAG=main
./terraform/rackspace-spot/scripts/deploy-from-registry.sh
```

DaemonSet image (after deploy):

`registry.gitlab.com/clevertree/relay-server/relay-gateway:main`

### IPFS on Rackspace nodes

Set per-node env (DaemonSet or `relay.env`):

| Env | Example |
|-----|---------|
| `RELAY_IPFS_GATEWAY` | `https://ipfs.io` or your Kubo gateway |
| `RELAY_IPFS_GATEWAY_ALLOWLIST` | `ipfs.io,your-gateway.example` |
| `RELAY_IPFS_MAX_FETCH_BYTES` | `104857600` |
| `RELAY_IPFS_CACHE_ROOT` | `/srv/relay/ipfs-cache` |

For a **sidecar Kubo** on the same pod/host, use `RELAY_IPFS_GATEWAY_ALLOW_PRIVATE=1` and `http://127.0.0.1:8080` only if the gateway is not reachable from other tenants.

## docker-compose (VM)

`docker-compose.yml` pulls from GitLab Registry (not GHCR). Authenticate:

```bash
docker login registry.gitlab.com
docker compose pull && docker compose up -d
```

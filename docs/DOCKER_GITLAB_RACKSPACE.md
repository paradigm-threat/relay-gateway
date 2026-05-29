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

Pipeline (`.gitlab-ci.yml`):

1. **test:verify** — `cargo test`, smoke `POST /api/config`
2. **package:linode-tgz** — Linode tarball artifact on `main` / tags
3. **docker:build** — Kaniko build on `main` / tags
4. **docker:publish** — push `latest` + SHA (+ tag name on release)

Required CI/CD variables (GitLab project **relay-server**):

| Variable | Purpose |
|----------|---------|
| `CI_REGISTRY_USER` | GitLab provides |
| `CI_REGISTRY_PASSWORD` | GitLab provides |

Enable **Container Registry** on the project.

## Rackspace Spot (Kubernetes)

Manifests: `terraform/rackspace-spot/k8s/`.

```bash
export REGISTRY=registry.gitlab.com/clevertree/relay-server
export RELAY_IMAGE_TAG=main
./terraform/rackspace-spot/scripts/build-push-deploy.sh
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

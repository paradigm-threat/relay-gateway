# GitLab CI setup (relay-server / relay-gateway)

## Pipeline jobs

| Job | Needs |
|-----|--------|
| `test:verify` | crates.io `relay-runtime` + `md2ast` (no private clone) |
| `package:linode-tgz` | same |
| `docker:prepare-context` | **git clone** of `relay-runtime` + `md2jsx` |
| `docker:publish` | Kaniko push to `registry.gitlab.com/clevertree/relay-gateway` |

## One-time: enable Docker builds (pick one)

### Option A — CI job token (recommended)

On **`clevertree/relay-runtime`**:

- **Public** (recommended if already on crates.io), **or**
- **Private** with job token: **Settings → CI/CD → Job token permissions** → add `clevertree/relay-gateway`, **or**
- Deploy token via `RELAY_RUNTIME_GIT_URL` (see Option B below)

### Option B — Deploy token (works across groups)

On **`clevertree/relay-runtime`**:

1. **Settings → Repository → Deploy tokens** → name `relay-server-ci`, scope **read_repository**
2. Copy token

On **`clevertree/relay-gateway`**:

1. **Settings → CI/CD → Variables**
2. Add **`RELAY_RUNTIME_GIT_URL`** (masked):

   `https://gitlab+deploy-token-<id>:<token>@gitlab.com/clevertree/relay-runtime.git`

## Container registry

Image: `registry.gitlab.com/clevertree/relay-gateway:main`

GitLab injects registry credentials into Kaniko automatically.

## Validate locally

```bash
cd relay-gateway
./scripts/ci-ensure-deps.sh          # path deps (dev layout)
CI=true ./scripts/ci-ensure-deps.sh  # registry mode (same as test job)
cargo test
```

Docker:

```bash
RELAY_CI_PATH_DEPS=1 ./scripts/ci-ensure-deps.sh
./scripts/docker-build-production.sh
```

## Rackspace roll

After `docker:publish` succeeds (pipeline [#2562267413](https://gitlab.com/clevertree/relay-gateway/-/pipelines/2562267413) on `73da48d`):

```bash
docker pull registry.gitlab.com/clevertree/relay-gateway:main
export KUBECONFIG=terraform/rackspace-spot/kubeconfig   # refresh via terraform if stale
./terraform/rackspace-spot/scripts/deploy-from-registry.sh
```

If kubeconfig API host does not resolve, refresh the Rackspace cloudspace (`terraform apply` in `terraform/rackspace-spot`).

`build-push-deploy.sh` builds locally; prefer `deploy-from-registry.sh` when CI already published the image.

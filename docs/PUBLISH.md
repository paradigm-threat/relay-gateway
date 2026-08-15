# Publishing `relay-gateway`

## crates.io (library + binaries)

The package is **`relay-gateway`** (`relay_server` lib). Path dependencies **`relay-runtime`** and **`md2ast`** must be published or replaced with crates.io versions before the gateway crate can publish.

### Publish order (crates.io)

Publish dependencies first (each from its crate directory):

```bash
cd md2ast && cargo publish --dry-run && cargo publish
cd relay/relay-gateway/themed-styler && cargo publish --dry-run && cargo publish
cd relay-runtime && cargo publish --dry-run && cargo publish
cd relay/relay-gateway && cargo publish --dry-run && cargo publish
```

Current versions: **md2ast 1.0.2**, **themed-styler 1.2.10**, **relay-runtime 0.1.1**, **relay-gateway 0.3.12**.

Path deps in `Cargo.toml` include matching `version =` keys so `cargo publish` strips paths and resolves from crates.io.

### Checklist

1. Bump versions in the chain above; tag **`v0.3.12`** (gateway) on GitLab.
2. Run **`cargo publish --dry-run`** in publish order.
3. GitLab job **`publish:crate`** (manual, on version tags) dry-runs gateway only.
4. When ready: `cargo login` then publish in dependency order.

### CI/CD token

Store `CARGO_REGISTRY_TOKEN` in GitLab CI variables for automated publish (optional future job).

## Container image

GitLab Container Registry:

`registry.gitlab.com/clevertree/relay-gateway:<tag>`

Built by **`docker:publish`** on `main` and release tags. See **[`DOCKER_GITLAB_RACKSPACE.md`](./DOCKER_GITLAB_RACKSPACE.md)**.

## Linode tarball

Artifact **`package:linode-tgz`** on `main` / tags — same as `scripts/pack-linode-deploy.sh`.

# Optional prebuilt binaries

Place release binaries here for **`scripts/install-from-checkout.sh`**:

- **`linux-amd64/relay-server`**
- **`linux-amd64/relay-hook-handler`**
- **`linux-arm64/…`** (same names on aarch64 servers)

Populate from your build machine:

```bash
./scripts/pack-linode-deploy.sh
mkdir -p dist/linux-amd64
cp target/release/relay-server target/release/relay-hook-handler dist/linux-amd64/
```

The **`dist/`** tree is **gitignored** by default so blobs are not committed accidentally; remove the ignore rule (or use **Git LFS**) if you intentionally vendor binaries. See **[`docs/DEPLOY_SELF_HOSTED.md`](../docs/DEPLOY_SELF_HOSTED.md)**.

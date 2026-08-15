# Agent instructions (relay-gateway crate)

## Production masters

**First:** **`relay-ops`** (CLI + UI) — **[`docs/RELAY_OPS.md`](docs/RELAY_OPS.md)**. Wrapper: **`./scripts/relay-fleet.sh`**. Topology: GitLab URL → **`ops/fleet.json`**.

Then **[`.cursor/rules/relay-production-ops.mdc`](.cursor/rules/relay-production-ops.mdc)** (SSH, tarball, **`POST /api/config`**, **`relay.env`**). **Reconcile:** **[`docs/RELAY_RECONCILE_PLAN.md`](docs/RELAY_RECONCILE_PLAN.md)**.

## Code

Run tests: **`cargo test`**.

**Android / web runtime testing:** in-tree APK + wasm-test live in **`relay-runtime`** — [`../relay-runtime/docs/ANDROID_WEB_TEST.md`](../relay-runtime/docs/ANDROID_WEB_TEST.md). Product app is **`relay-client-android`** (Rust JNI `.so`). Desktop native UI is skipped; desktop is web.

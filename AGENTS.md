# Agent instructions (relay-gateway crate)

## Production masters

**First:** from this directory, **`./scripts/relay-fleet.sh`** — see **[`docs/RELAY_FLEET_CONSOLE.md`](docs/RELAY_FLEET_CONSOLE.md)**. Local **`relay.env`** (gitignored) should set **`RELAY_OPERATOR_FLEET`** or **`RELAY_MASTER_PEER_LIST`**.

Then **[`.cursor/rules/relay-production-ops.mdc`](.cursor/rules/relay-production-ops.mdc)** (SSH, tarball, **`POST /api/config`**, **`relay.env`**). **Reconcile:** **[`docs/RELAY_RECONCILE_PLAN.md`](docs/RELAY_RECONCILE_PLAN.md)**.

## Code

Run tests: **`cargo test`**.

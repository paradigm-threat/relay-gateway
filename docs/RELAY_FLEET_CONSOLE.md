# Relay fleet console

**Canonical:** [`RELAY_OPS.md`](./RELAY_OPS.md) — `relay-ops` CLI + local UI. Wrapper: `./scripts/relay-fleet.sh`. Topology: [`ops/fleet.json`](../ops/fleet.json).

This filename is kept so old links still work. Do not add new operator procedure here.

**`--build` / stale tarball:** put `--build` **before** `update` / `deploy-all`. If `target/relay-linode-deploy.tgz` already exists and you skip `--build`, the fleet uploads that file as-is. See [`RELAY_OPS.md`](./RELAY_OPS.md) § Deploy pitfalls.

Provision a new master: [`RELAY_INFRA_OPS.md`](./RELAY_INFRA_OPS.md). Mesh notify YAML: [`RELAY_MESH_OPS.md`](./RELAY_MESH_OPS.md).

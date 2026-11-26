# Relay ops

Generic fleet CLI + local UI. GitLab URL → `ops/fleet.json` (or `relay.env.example`) → probes and admin commands. See **[docs/RELAY_OPS.md](../docs/RELAY_OPS.md)**.

- Topology: [`fleet.json`](./fleet.json)
- Credit / scale-down: [`CREDIT_BURN.md`](./CREDIT_BURN.md), [`credit_burn.json`](./credit_burn.json)
- Pre-destroy check: `../scripts/relay-scale-down-check.sh` → `scale-down-last-check.log`

```bash
cargo run --bin relay-ops -- status --gitlab https://gitlab.com/clevertree/relay-gateway
cargo run --bin relay-ops -- serve --gitlab https://gitlab.com/clevertree/relay-gateway
./scripts/relay-fleet.sh hosts
./scripts/relay-scale-down-check.sh
```

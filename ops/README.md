# Relay ops

Generic fleet CLI + local UI. GitLab URL → `ops/fleet.json` (or `relay.env.example`) → probes and admin commands. See **[docs/RELAY_OPS.md](docs/RELAY_OPS.md)**.

```bash
cargo run --bin relay-ops -- status --gitlab https://gitlab.com/clevertree/relay-gateway
cargo run --bin relay-ops -- serve --gitlab https://gitlab.com/clevertree/relay-gateway
./scripts/relay-fleet.sh hosts
```

# Relay brain — removed

The local fleet **brain** (scenario YAML auto-remediation on `:7781`) was **disabled and stripped** in the Linode fleet ops redesign.

- Scenarios under `ops/scenarios/` are gone (see that directory’s README).
- `relay-ops brain serve` / `relay-brain` exit with “brain disabled”.
- Operators use the thin Fleet UI (`relay-ops serve` on `:7780`): Refresh, Synchronize, Warm indexes, Point DNS, Repair/update.

Index freshness is hardened on the gateway (SearchStore warm on accepted tips). See [`RELAY_OPS.md`](./RELAY_OPS.md) and [`ops/CREDIT_BURN.md`](../ops/CREDIT_BURN.md).

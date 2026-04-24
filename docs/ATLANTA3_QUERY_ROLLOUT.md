# Staged QUERY / CleverScript rollout on atlanta3

Use a **single production node** (e.g. **atlanta3**) to validate CleverScript QUERY and hook behavior before a wider fleet rollout.

## Preconditions

- SSH access per **`relay-gateway/.cursor/rules/relay-production-ops.mdc`** (canonical production ops for relay nodes).
- A test bare repo on the host under the gateway’s **`RELAY_REPO_PATH`** with:
  - `.relay.yaml` containing **`server.hooks.query.path: hooks/server/query.cs`**
  - committed **`hooks/server/query.cs`** implementing the contract in [QUERY_CLEVERSCRIPT.md](./QUERY_CLEVERSCRIPT.md)
- Optional: keep **`server.hooks.index`** and **`index.json`** workflows on clients until QUERY is verified end-to-end (do not delete generated artifacts until validation passes).

## Deploy gateway build

1. Build **`relay-gateway`** from this branch locally: `cargo build --release -p relay-gateway`.
2. Package and install on **atlanta3** only: from **`relay-gateway/`**, **`./scripts/relay-fleet.sh --build update --only=atlanta3.relaygateway.net`** (or your usual **`pack-linode-deploy.sh` + `install.sh update`** flow; see production ops rule and **`docs/RELAY_FLEET_CONSOLE.md`**).
3. Restart **`relay-gateway`** on atlanta3 only: `sudo systemctl restart relay-gateway` (exact unit name per host).

## Verify

1. **`QUERY`** against the test repo with `Accept: application/json` and a body that includes **`branch`** and collection semantics your script expects.
2. Confirm logs: no CleverScript parse errors; rows match expectations.
3. Compare latency and CPU with the previous redb-only path on a clone workload (optional).

## Roll forward / back

- **Forward:** After atlanta3 is stable, promote the same binary to other nodes.
- **Back:** Redeploy the previous **`relay-gateway`** tarball; `.relay.yaml` without **`query.path`** ending in **`.cs`** keeps legacy redb QUERY.

## Notes

- Do not remove **paradigm-threat** **`index.json`** / **`generate-index.py`** until nav/search parity is confirmed against QUERY in production-like conditions (separate content migration).

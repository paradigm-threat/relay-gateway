# Rackspace Spot — operations

## Stacks (relaygateway.net)

| Stack | FQDN | Region | Terraform path |
|-------|------|--------|----------------|
| **dfw1** | `dfw1.relaygateway.net` | `us-central-dfw-1` | `terraform/rackspace-spot/` (root) |
| **lon1** | `lon1.relaygateway.net` | `uk-lon-1` | `terraform/rackspace-spot/stacks/lon1/` |
| **ord1** | `ord1.relaygateway.net` | `us-central-ord-1` | `terraform/rackspace-spot/stacks/ord1/` |

Mesh peers: `atlanta2;dfw1;jp-osa;lon1;ord1` (all `*.relaygateway.net`).

**Retired:** `node-dfw1` / `node-dfw2` on `relaynet.online`, second dfw spot pool, `atlanta3` naming on spot (use **dfw1**).

## Cloudspace Status: Error — `relay-node` metadata conflict

Rackspace may show **Healthy** workers but **Cloudspace Status: Error** with:

> Custom Metadata conflicts found in node pools … Label `relay-node` (value: `dfw1`) conflicts with existing node label (value: `dfw1`)

**Cause:** More than one **Server pool** in the same cloudspace each defines Spot custom metadata `relay-node=dfw1` (common after preemption, a retired `dfw2` pool, or re-applying Terraform while an old pool still exists). Do **not** also `kubectl label node … relay-node=dfw1` — that doubled the problem; deploy script no longer does.

**Fix:**

1. In Spot UI → **relay-network-cheapest** → **Server Pools** (or Resources): keep **one** pool with `desired_server_count=1` and delete **orphan** pools (extra bids). Terraform only manages one `spot_spotnodepool` per stack.
2. Pool labels should be only `component=relay-server` and `managed-by=terraform` (no `relay-node` on the pool).
3. Re-run `SKIP_TERRAFORM=1 RELAY_SPOT_STACK=dfw1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh` to roll the DaemonSet (`nodeSelector: component=relay-server`).

Until the extra pool is removed, you may run **two workers** with separate disks; DNS should target the node that actually runs `relay-daemon`.

**Repo mirror DNS:** `vercel-dns-upsert.sh` alone is not enough for `{repo}.dfw1.relaygateway.net`. After apex `dfw1.relaygateway.net` → node IP, `rackspace-deploy.sh` also creates **`*.dfw1` A** on the **`relaygateway.net`** zone (same pattern as `*.atlanta2`). Use **Vercel CLI auth** (`~/.local/share/com.vercel.cli/auth.json`) — stale tokens in K8s secrets return 403.

## Deploy

```bash
# All stacks (terraform + k8s + DNS upsert)
./terraform/rackspace-spot/scripts/rackspace-deploy.sh

# One stack
RELAY_SPOT_STACK=dfw1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
SKIP_TERRAFORM=1 RELAY_SPOT_STACK=lon1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
```

## Paradigm Threat (PT) on the mirror list

**Already in `relay-master`** — no catalog edit required to “add” PT:

| Bare slug | Catalog record | Forge (shallow clone) |
|-----------|----------------|------------------------|
| `paradigm-threat-files` | `catalog/repos/paradigm-threat-files.json` | `paradigm-threat-files-thin` (private) |
| `paradigm-threat-timeline` | `catalog/repos/paradigm-threat-timeline.json` | `paradigm-threat-timeline-thin` (private) |
| `paradigm-threat-site` | `catalog/repos/paradigm-threat-site.json` | public stub on GitLab |

**Still required before PT content is live on spot:**

1. **GitLab read-only deploy token** with access to the `*-thin` repos — set on the deploy host before `rackspace-deploy.sh`:
   ```bash
   export GITLAB_DEPLOY_USER='gitlab+deploy-token-NNN'
   export GITLAB_DEPLOY_TOKEN='…'
   ```
   These land in the `relay-credentials` secret (sourced from `relay.env` if present).

2. **Full bare trees** — thin forge repos are stubs; real content is pushed to relay git (`git://atlanta3.relaygateway.net:9418/…`). After first boot, either:
   - `git push` from your workstation to the spot node, or
   - let **`relay-startup-catalog-sync.sh`** peer-fetch from **atlanta2** / **jp-osa** (needs mesh reachable and those masters already holding mirrors).

3. **Trust / signing** — catalog records use `signing_keys_source: repo`; pushes must match repo `.relay.yaml` policy (same as Linode masters).

4. **HTTPS for `atlanta3.relaygateway.net`** — spot DaemonSet currently serves **HTTP :8080** (`RELAY_SSL_MODE=none`). Production TLS is still via **Caddy + Let’s Encrypt** on Linode (`install-caddy-tls.sh`) or a future spot TLS job. DNS to the spot IP does not by itself issue certs.

## What happens when the spot server goes down?

| Event | What happens | Automatic? |
|--------|----------------|------------|
| **Pod crash / restart** | Same node, **hostPath** keeps `/opt/relay/data` and catalog worktree. Boot script re-runs sync + DNS upsert. | Yes (DaemonSet restart) |
| **Spot preemption** (bid lost) | VM replaced; **new IP**; **empty disk** unless you add persistent volumes. Terraform/nodepool places a new bid. | Partially |
| **New spot node wins bid** | `relay-boot.sh` runs: clone `relay-master`, `sync-relay-catalog.sh`, **Vercel upsert** for **`{stack}.relaygateway.net`**, start git daemon + relay. | Yes (on pod start) |
| **DNS** | `vercel-dns-upsert.sh` updates A records to the node’s current public IP. | Yes (boot script + deploy script) |
| **SSL / HTTPS** | Not automatic on spot today (`RELAY_SSL_MODE=none`). Certbot path needs `/var/www/certbot` or switch to Caddy sidecar. | No — manual / follow-up |
| **Bare mirror data** | After preemption, re-sync from **forge** (thin stubs) + **peer fetch** from other masters or re-**push** full trees. | Peer fetch automatic if `RELAY_MASTER_PEER_LIST` + mesh secrets set and peers up |

**Recommended after preemption:** run `SKIP_TERRAFORM=1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh` once the new node is Ready (belt-and-suspenders DNS + rollout).

## Secrets

`relay-credentials` (Kubernetes):

- `VERCEL_API_TOKEN`, `VERCEL_TEAM_ID` — DNS
- `RELAY_MASTER_MESH_SECRET`, `RELAY_PEER_SECRET` — from `relay.env` when deploy script runs
- `GITLAB_DEPLOY_USER`, `GITLAB_DEPLOY_TOKEN` — optional; required for private thin repos

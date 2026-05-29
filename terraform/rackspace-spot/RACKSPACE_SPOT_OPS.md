# Rackspace Spot — operations

## Deploy

```bash
./terraform/rackspace-spot/scripts/rackspace-deploy.sh
```

Roll manifests only (cluster already up):

```bash
SKIP_TERRAFORM=1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh
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
| **New spot node wins bid** | `relay-boot.sh` runs: clone `relay-master`, `sync-relay-catalog.sh`, **Vercel upsert** for `node-dfw1.relaynet.online` + **`atlanta3.relaygateway.net`**, start git daemon + relay. | Yes (on pod start) |
| **DNS** | `vercel-dns-upsert.sh` updates A records to the node’s current public IP. | Yes (boot script + deploy script) |
| **SSL / HTTPS** | Not automatic on spot today (`RELAY_SSL_MODE=none`). Certbot path needs `/var/www/certbot` or switch to Caddy sidecar. | No — manual / follow-up |
| **Bare mirror data** | After preemption, re-sync from **forge** (thin stubs) + **peer fetch** from other masters or re-**push** full trees. | Peer fetch automatic if `RELAY_MASTER_PEER_LIST` + mesh secrets set and peers up |

**Recommended after preemption:** run `SKIP_TERRAFORM=1 ./terraform/rackspace-spot/scripts/rackspace-deploy.sh` once the new node is Ready (belt-and-suspenders DNS + rollout).

## Secrets

`relay-credentials` (Kubernetes):

- `VERCEL_API_TOKEN`, `VERCEL_TEAM_ID` — DNS
- `RELAY_MASTER_MESH_SECRET`, `RELAY_PEER_SECRET` — from `relay.env` when deploy script runs
- `GITLAB_DEPLOY_USER`, `GITLAB_DEPLOY_TOKEN` — optional; required for private thin repos

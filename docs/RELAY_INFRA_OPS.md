# Relay master infrastructure operations

**Terraform** provisions VMs; **`relay-master-deploy.sh`** installs Relay; **`relay-fleet.sh`** updates existing masters.

## Layout

| Path | Role |
|------|------|
| [`terraform/linode-relay/`](../terraform/linode-relay/) | Linode **`linode/linode`** — atlanta2 (import, do not reprovision) |
| [`terraform/rackspace-spot-vm/`](../terraform/rackspace-spot-vm/) | Rackspace Spot **VM** (iad1 / ord1 / hkg1); see [`PROVIDER_SPIKE.md`](../terraform/rackspace-spot-vm/PROVIDER_SPIKE.md) |
| [`terraform/shared-dns/`](../terraform/shared-dns/) | Vercel DNS helpers |
| [`scripts/relay-master-deploy.sh`](../scripts/relay-master-deploy.sh) | First-time / rebuild install |
| [`scripts/relay-master-validate.sh`](../scripts/relay-master-validate.sh) | HTTPS + API + mirror smoke tests |
| [`scripts/relay-fleet.sh`](../scripts/relay-fleet.sh) | Ongoing **`--build update`**, repair, features-set |

Archived Kubernetes Spot path: [`docs/archive/rackspace-spot-k8s/`](archive/rackspace-spot-k8s/).

## First-time master

### Linode (atlanta2)

1. Import existing VM: [`terraform/linode-relay/IMPORT.md`](../terraform/linode-relay/IMPORT.md)
2. **`terraform plan`** — must not replace instance
3. Updates only: **`./scripts/relay-fleet.sh --build update --only=atlanta2.relaygateway.net`**

Do **not** run full **`relay-master-deploy.sh install`** on atlanta2 without explicit approval (risk to `/opt/relay/data`).

### Rackspace Spot ord1 (VM, Chicago)

1. Create **VM Cloudspace** **`relay-network-ord`** in **`us-central-ord-1`** (Spot UI; MCP `createVMPool` is currently broken)
2. **`export RXTSPOT_TOKEN=...`** or **`rackspace_spot_token`** in tfvars (never commit)
3. ```bash
   cd relay-gateway
   ./scripts/relay-master-deploy.sh --target spot-ord1 --install-only --build
   ./scripts/relay-master-validate.sh --fqdn ord1.relaygateway.net
   ```

Publisher for catalog repos is **`iad1.relaygateway.net`** (`publish_origin`). After Spot preemption on a replica: reclaim with **`--target spot-ord1`** / **`spot-iad1`** / **`spot-hkg1`**.

### Retired: Spot Dallas / K8s interim

Former Dallas publisher / cloudspace **`relay-network`** is retired (replaced by **ord1**). Historical notes: [`docs/archive/rackspace-spot-k8s/`](archive/rackspace-spot-k8s/) and interim TLS under `terraform/` (do not apply).

## Ongoing operations

| Task | Command |
|------|---------|
| Fleet status | `./scripts/relay-fleet.sh status` |
| Binary deploy | `./scripts/relay-fleet.sh --build deploy-all` |
| Piper off/on | `./scripts/relay-fleet.sh features-set --only=… --piper=1` |
| Piper HTTPS `/voices` `/tts` | After enabling Piper, Caddy must proxy those paths to **:5590** — `install-caddy-tls.sh` (in tarball). Refresh fleet: `RELAY_SSH_USER=ubuntu ./scripts/relay-fleet-refresh-caddy-piper.sh` |

Spot VMs SSH as **`ubuntu`** (not `ari`). Production fleet: **iad1** (publisher), **ord1**, **hkg1**.

See **[`RELAY_OPS.md`](./RELAY_OPS.md)**.

## Secrets

- **`LINODE_TOKEN`**, **`RXTSPOT_TOKEN`**, **`terraform.tfvars`**, **`kubeconfig`**: gitignored — **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**
- If Rackspace token was ever committed: **rotate** in Spot UI → API Access

## DNS

- **Canonical strategy:** [`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md).
- Node identity: **`{node}.relaygateway.net`** A record, updated after Spot provisioning/replacement.
- Repo vhosts: **`*.{node}.relaygateway.net`** A record for **`{repo}.{node}.relaygateway.net`**.
- Stable network ingress: **`fleet.relaygateway.net`**, whose A/AAAA set is reconciled from the configured node FQDNs.
- Parked apex domains: provider **ALIAS/flattening → `fleet.relaygateway.net`**; parked subdomains: **CNAME → `fleet.relaygateway.net`**.
- Single-node canaries may CNAME directly to a node (for example `relay.clevertree.net → iad1.relaygateway.net`).
- Do not put all node addresses behind `fleet.relaygateway.net` until coordinated multi-node ACME/certificate distribution and renewal are ready.
- Existing node/wildcard helpers: **`terraform/shared-dns/scripts/`**. Fleet ingress: **`./scripts/relay-fleet.sh fleet-dns`** (also from **`relay-master-deploy.sh`** after node DNS upsert).

## Manual follow-up

- **paradigm-threat-site** Vercel env for iad1/ord1/hkg1 mirrors/TTS after validate
- Thin IPFS catalog cutover (out of scope)

# Relay master infrastructure operations

**Terraform** provisions VMs; **`relay-master-deploy.sh`** installs Relay; **`relay-fleet.sh`** updates existing masters.

## Layout

| Path | Role |
|------|------|
| [`terraform/linode-relay/`](../terraform/linode-relay/) | Linode **`linode/linode`** — atlanta2 (import, do not reprovision) |
| [`terraform/rackspace-spot-vm/`](../terraform/rackspace-spot-vm/) | Rackspace Spot **VM** (dfw1); see [`PROVIDER_SPIKE.md`](../terraform/rackspace-spot-vm/PROVIDER_SPIKE.md) |
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

### Rackspace Spot dfw1 (VM)

1. Create **VM Cloudspace** in Spot UI (or set **`public_ip_override`** in **`terraform.tfvars`**)
2. **`export RXTSPOT_TOKEN=...`** or **`rackspace_spot_token`** in tfvars (never commit)
3. ```bash
   cd relay-gateway
   ./scripts/relay-master-deploy.sh --target spot-dfw1 --build
   ./scripts/relay-master-validate.sh --fqdn dfw1.relaygateway.net
   ```

If the old K8s cloudspace was deleted, **:8080 / :443** on dfw1 will fail until this install completes. Use **`public_ip_override`** in **`terraform/rackspace-spot-vm/terraform.tfvars`** when the VM is created in the Spot UI.

### Retired: Spot K8s interim (dfw1)

**`relay-network-cheapest`** was deleted via **`spot-dfw1-migrate-to-vm.sh delete-k8s`**. Production dfw1 is the **VM Cloudspace** **`relay-network`** at **`174.143.201.10`** (Ubuntu 24.04). Do not point DNS at old worker IPs (**.57** / **.109**). Archived manifests: [`docs/archive/rackspace-spot-k8s/`](archive/rackspace-spot-k8s/). Interim Caddy DaemonSet (if any remains): [`terraform/k8s-interim-dfw1/`](../../terraform/k8s-interim-dfw1/) — not used on bare VM path.

After Spot preemption: re-run **`relay-master-deploy.sh --target spot-dfw1`** (terraform + install).

## Ongoing operations

| Task | Command |
|------|---------|
| Fleet status | `./scripts/relay-fleet.sh status` |
| Binary deploy | `./scripts/relay-fleet.sh --build deploy-all` |
| Piper off/on | `./scripts/relay-fleet.sh features-set --only=… --piper=1` |
| Piper HTTPS `/voices` `/tts` | After enabling Piper, Caddy must proxy those paths to **:5590** — `install-caddy-tls.sh` (in tarball). Refresh fleet: `RELAY_SSH_USER=ubuntu ./scripts/relay-fleet-refresh-caddy-piper.sh` |

Spot VMs SSH as **`ubuntu`** (not `ari`). Production fleet: **dfw1**, **lon1**, **hkg1**.

See **[`RELAY_FLEET_CONSOLE.md`](./RELAY_FLEET_CONSOLE.md)**.

## Secrets

- **`LINODE_TOKEN`**, **`RXTSPOT_TOKEN`**, **`terraform.tfvars`**, **`kubeconfig`**: gitignored — **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**
- If Rackspace token was ever committed: **rotate** in Spot UI → API Access

## DNS

- Apex: **`{node}.relaygateway.net`**
- Wildcard on **`relaygateway.net`**: **`*.{node}`** for **`{repo}.{node}.relaygateway.net`**
- Scripts: **`terraform/shared-dns/scripts/`**

## Manual follow-up

- **paradigm-threat-site** Vercel env for dfw1 mirrors/TTS after validate
- Thin IPFS catalog cutover (out of scope)

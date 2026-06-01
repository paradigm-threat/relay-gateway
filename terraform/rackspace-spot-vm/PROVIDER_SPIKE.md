# Provider spike: Rackspace Spot VM (May 2026)

## Finding

| Tool | VM Cloudspace support |
|------|------------------------|
| **`rackerlabs/spot` Terraform** (v0.1.4) | **K8s only** (`spot_cloudspace`, `spot_spotnodepool`) |
| **`spotctl` CLI** | **K8s only** (cloudspaces / nodepools) |
| **Spot public API** (`/apis/ngpc.rxt.io/...`) | K8s cloudspaces documented; **VM** paths not in published provider |
| **Spot UI** | **VM Cloudspace** — create pool, bid, SSH key, Ubuntu 24.04 |

Rackspace has stated Terraform/spotctl/API support for VM Cloudspaces is **planned** ([discussion #238](https://github.com/rackerlabs/spot/discussions/238)).

## Relay approach

1. **`scripts/spot-vm-provision.sh`** — writes **`vm-state.json`** with `public_ip` (from **`public_ip_override`**, DNS, or future API).
2. **`terraform apply`** — triggers provisioner; **`data.external`** reads state for outputs.
3. **`relay-master-deploy.sh --target spot-dfw1`** — same Linode path after IP is known: tarball, **`install.sh`**, Vercel DNS, Caddy, Piper.

Until API lands: create **VM Cloudspace** in Spot UI (1× **`gp.vs1.medium-dfw`**, bid **0.005**), set **`public_ip_override`** in **`terraform.tfvars`**, then:

```bash
./scripts/relay-master-deploy.sh --target spot-dfw1 --install-only
```

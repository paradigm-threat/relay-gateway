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
3. **`relay-master-deploy.sh --target spot-iad1`** (or **`spot-sjc1`** / **`spot-hkg1`**) — same Linode path after IP is known: tarball, **`install.sh`**, node/wildcard Vercel DNS, Caddy, Piper.
4. **Network ingress:** reconcile **`fleet.relaygateway.net`** from the stable node FQDNs after replacement. Parked domains point to the fleet name, not directly to Spot IPs. See **[`RELAY_NETWORK_DNS.md`](../../docs/RELAY_NETWORK_DNS.md)**.

**OS:** use Spot VM image **`ubuntu2404`** (Ubuntu 24.04). K8s worker nodes on **`relay-network-cheapest`** are **Ubuntu 20.04** — the Linode-style **`relay-linode-deploy.tgz`** binary requires **glibc ≥ 2.32** and will not start there.

Until API lands: create **VM Cloudspace** in Spot UI (e.g. 1× **`gp.vs1.medium-iad`**, bid **0.01**, **`ubuntu2404`**), set **`public_ip_override`** in per-node tfvars, then:

**Per-target tfvars:** **`iad1.tfvars`**, **`sjc1.tfvars`**, **`hkg1.tfvars`** (see `*.tfvars.example`). Retired **`dfw1`** / **`lon1`** examples remain historical only.

```bash
./scripts/relay-master-deploy.sh --target spot-iad1 --install-only
```

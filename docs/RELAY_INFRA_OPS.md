# Relay master infrastructure operations

**Terraform** provisions Linode VMs; **`relay-master-deploy.sh`** installs Relay; **`relay-ops` / fleet** updates existing masters.

## Layout

| Path | Role |
|------|------|
| [`terraform/linode-relay/`](../terraform/linode-relay/) | Linode publisher + mesh VMs (`pub1` / `mesh1` / `mesh2`) |
| [`terraform/shared-dns/`](../terraform/shared-dns/) | Vercel DNS helpers |
| [`scripts/relay-master-deploy.sh`](../scripts/relay-master-deploy.sh) | First-time / rebuild install |
| [`scripts/relay-master-validate.sh`](../scripts/relay-master-validate.sh) | HTTPS + API + mirror smoke tests |
| [`docs/MAIL_INBOUND.md`](MAIL_INBOUND.md) | Optional OpenSMTPD receive-only catcher + **relay-ops Inbox** (MX on **pub1**) |
| [`docs/INSTALL_FEATURES.md`](INSTALL_FEATURES.md) | Install-time features incl. **`RELAY_FEAT_INBOUND_MAIL`** |
| [`docs/MESH_IRC.md`](MESH_IRC.md) | Ergo IRC hub on **pub1**; mesh edge proxies |

**Live fleet:** `pub1.relaygateway.net` (publisher) + `mesh1.relaygateway.net` + `mesh2.relaygateway.net` — see [`ops/fleet.json`](../ops/fleet.json).

**Fully retired (do not redeploy):**

- Rackspace Spot **Kubernetes** (including `myproblem-intake`) — deleted 2026-11-26
- Rackspace Spot **VMs** (`iad1` / `ord1` / `hkg1` / `dfw1`) and `terraform/rackspace-spot-vm/`
- Legacy Linode names `atlanta1` / `atlanta2` / `atlanta3` / `jp-osa` — **jp-osa** Vercel A/AAAA removed 2026-11-27
- Local Spot leftovers wiped 2026-11-27: `~/.spot_config`, Spot kubeconfigs / discovery cache, Cursor `rackspace-spot` MCP
- Spot API tokens return **401** (platform wind-down; hard stop **2026-12-08 17:00 CT**)
- Archived Spot K8s notes: [`docs/archive/rackspace-spot-k8s/`](archive/rackspace-spot-k8s/)

**Note:** `iad1` / `ord1` / `hkg1` hostnames may still **resolve** via the Vercel zone catch-all `ALIAS *` → Vercel parking IPs. That is **not** Spot. Do not recreate Spot A records. Optional later: tighten the catch-all if you want those names to fail DNS.

## First-time master (Linode)

```bash
cd relay-gateway
./scripts/relay-master-deploy.sh --target linode-pub1 --build
./scripts/relay-master-validate.sh --fqdn pub1.relaygateway.net
```

Catalog `publish_origin` is **`pub1.relaygateway.net`**. Mesh nodes are edge-only for IRC (`CHAT_PRIMARY_FQDN=pub1.relaygateway.net`).

## Ongoing operations

| Task | Command |
|------|---------|
| Fleet status | `relay-ops` / local UI on `:7780` |
| Binary deploy | `./scripts/relay-master-deploy.sh` / pack + install |
| Piper HTTPS `/voices` `/tts` | `install-caddy-tls.sh` (in tarball) |

See **[`RELAY_OPS.md`](./RELAY_OPS.md)** and **[`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md)**.

## Secrets

- **`LINODE_TOKEN`**, **`terraform.tfvars`**: gitignored — **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)**
- **`RXTSPOT_TOKEN` / Spot kubeconfigs**: retired — rotate/delete any leftover Spot API tokens in the Spot UI

## DNS

- Node identity: **`{node}.relaygateway.net`** A → Linode IP
- Stable ingress: **`fleet.relaygateway.net`** A → **pub1 only** (Nanodes are not multi-A behind fleet)
- Parked apex: ALIAS/flattening → `fleet.relaygateway.net`
- Helpers: **`terraform/shared-dns/scripts/`**

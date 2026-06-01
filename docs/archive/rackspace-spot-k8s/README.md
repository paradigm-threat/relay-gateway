# Archived: Rackspace Spot Kubernetes path

Retired **May 2026** in favor of **bare VM** masters:

- **Linode:** [`terraform/linode-relay/`](../../terraform/linode-relay/)
- **Rackspace dfw1:** [`terraform/rackspace-spot-vm/`](../../terraform/rackspace-spot-vm/)
- **Deploy:** [`scripts/relay-master-deploy.sh`](../../scripts/relay-master-deploy.sh)
- **Ops:** [`docs/RELAY_INFRA_OPS.md`](../../docs/RELAY_INFRA_OPS.md)

This directory preserves the old **DaemonSet / kubeconfig** manifests and **`rackspace-deploy.sh`** for reference only. Do not apply these to production.

**Security:** If `terraform.tfvars` or `kubeconfig` were ever committed, **rotate** the Rackspace Spot token and treat kube credentials as compromised.

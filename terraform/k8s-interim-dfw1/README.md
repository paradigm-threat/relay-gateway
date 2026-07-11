# Interim: Caddy TLS on dfw1 Spot K8s worker

**Retired (May 2026):** dfw1 runs on bare Spot VM **`174.143.201.10`** (`relay-network` cloudspace). This DaemonSet was for interim K8s **`relay-network-cheapest`** only; do not apply unless reviving that path.

```bash
export KUBECONFIG=~/.kube/relay-network-cheapest.yaml   # spotctl cloudspaces get-config --name relay-network-cheapest
kubectl apply -f terraform/k8s-interim-dfw1/caddy-daemonset.yaml
```

**DNS:** apex **`dfw1.relaygateway.net`** and **`*.dfw1`** must point at **.57** (not the second worker .109) so ACME and traffic hit Caddy.

**Retire:** delete DaemonSet when **`relay-master-deploy.sh --target spot-dfw1`** runs on a Spot **VM** with SSH.

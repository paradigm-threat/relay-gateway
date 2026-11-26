# Import existing atlanta2 (HISTORICAL — host decom)

**Do not use.** Live masters are **iad1 / ord1 / hkg1**. This TF path is kept only for archival import notes.

**Goal:** Terraform state reflects the **live** Linode without replacing the instance or wiping `/opt/relay/data`.

## 1. Discover Linode ID

```bash
linode-cli linodes list --label atlanta2-relay
# or match by IP: dig +short atlanta2.relaygateway.net
```

## 2. Align variables with live VM

Before import, set **`region`**, **`instance_type`**, and **`label`** in **`terraform.tfvars`** to match the dashboard. Wrong **`instance_type`** can make **`terraform plan`** show a destructive replace.

## 3. Import

```bash
cd relay-gateway/terraform/linode-relay
export LINODE_TOKEN=...   # or terraform.tfvars
terraform init
terraform import linode_instance.atlanta2 <LINODE_ID>
```

Optional: import firewall if one already exists, or let Terraform create a new firewall and attach it on a maintenance window.

## 4. Validate (no apply without review)

```bash
terraform plan
```

Expect **no** `forces replacement` on **`linode_instance.atlanta2`**. If plan wants to recreate, fix **`instance_type`** / **`image`** / **`region`** first.

## 5. Runtime deploy

Do **not** run **`relay-master-deploy.sh --target linode-atlanta2`** **`install`** on production without explicit approval. Use:

```bash
./scripts/relay-fleet.sh status --only=atlanta2.relaygateway.net
```

For binary-only updates: **`./scripts/relay-fleet.sh --build update --only=atlanta2.relaygateway.net`**.

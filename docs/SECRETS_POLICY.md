# Relay secrets and deployment policy

Normative expectations for **master peer** nodes, **`relay.env`**, **mesh secrets**, **Slack/webhooks**, and the **shared deploy SSH key**. **`relay-server`** reads secrets from the environment (typically **`EnvironmentFile=/opt/relay/relay.env`**).

**Sanitized template (no real secrets):** **[`../relay.env.example`](../relay.env.example)**  
**Full variable reference:** **[`docs/examples/relay.env`](./examples/relay.env)** (comments and optional keys)

---

## 1. Manual `relay.env` on install (master peers)

- **Do not** bake production secrets into the git repo. Each server gets **`relay.env`** copied or edited **manually** (or via your private config management) **on first install** and whenever you change non-secret node identity.
- **`RELAY_MASTER_PEER_LIST`** must list the **complete** set of **master** mesh FQDNs (semicolon-separated, quoted in shell). Every master peer should use the **same** list so mesh behavior and **`/api/config` → `peers`** stay consistent.
- Set **`RELAY_PUBLIC_HOSTNAME`** and **`RELAY_SERVER_ID`** **per node** to match that host’s DNS and ops naming.
- **Catalog trust:** use **`RELAY_MASTER_CATALOG_PATH`** + **`RELAY_MASTER_TRUST_ROOT`** (first commit of the catalog checkout) on all peers that share the same policy; do **not** use **`RELAY_AUTHORIZED_REPOS_PATH`** for the catalog model (**[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)**).

After writing **`/opt/relay/relay.env`**, ensure ownership is usable by systemd (installer sets **`relay:relay`** on a fresh **`touch`**; if you merge files by hand, run **`chown relay:relay /opt/relay/relay.env`** and **`chmod 600`** as appropriate).

---

## 2. Rotating secrets (mesh + optional webhooks)

- **Rotate together:** When you change **`RELAY_MASTER_MESH_SECRET`**, **`RELAY_PEER_SECRET`**, **`RELAY_MESH_EVENT_SECRET`**, or **Slack / webhook** URLs, update **`relay.env`** on **all** masters (and any edges that share those values) in a **single** maintenance window so half-upgraded meshes do not reject each other’s hooks.
- **Order of operations:** generate new values → update every host’s **`relay.env`** → **`sudo systemctl restart relay-server`** (and any hook/notify consumers) on each node → retire old values once traffic is clean.
- **Webhooks:** Incoming Slack URLs and similar tokens are as sensitive as mesh secrets; include them in the same rotation checklist when they live in **`relay.env`**.

---

## 3. Shared deploy SSH key (validation on install)

- **One** operator/deploy **Ed25519** (or agreed) key pair is **standard across all** Relay servers for automation and agents (e.g. **`~/.ssh/id_ed25519_relay_gateway`** locally, **`ari`**’s **`authorized_keys`** on the node). Avoid per-server deploy keys unless you have a separate inventory story.
- **`install.sh`** can **enforce** that the deploy user’s **`authorized_keys`** contains an expected **SHA256** fingerprint (see below). Use this on **first boot** or **repair** so a rebuilt VM does not go to production without the correct key.

### Enabling install-time check

Set before **`./install.sh install`** or **`repair`** (non-interactive friendly):

| Variable | Meaning |
|----------|--------|
| **`RELAY_VALIDATE_DEPLOY_SSH_KEY=1`** | Run the check (any other non-empty value like **`yes`** also works). |
| **`RELAY_DEPLOY_SSH_USER`** | UNIX user whose **`~/.ssh/authorized_keys`** is inspected (default **`ari`**). |
| **`RELAY_DEPLOY_SSH_KEY_FINGERPRINT`** | Substring that must appear in **`ssh-keygen -lf`** output for that file (e.g. **`SHA256:uOAz78YRoCa6JeexYTEIeU0Qg6gbdpVkQ6NEhFmN/ME`** from **`ssh-keygen -lf -E sha256 ~/.ssh/id_ed25519_relay_gateway.pub`**). |

If validation is enabled and the fingerprint is missing from **`authorized_keys`**, **install aborts** with a clear error. Implemented in **`scripts/relay-install.sh`** (shipped as **`install.sh`** **`install`** / **`repair`**).

**Docs for operators/agents:** **[`CURSOR_AGENTIC_SSH.md`](./CURSOR_AGENTIC_SSH.md)** (non-root SSH, **`relay-linode-redeploy-seed.sh`** defaults).

---

## Terraform and Spot tokens

- Never commit **`terraform.tfvars`**, **`secrets.auto.tfvars`**, **`kubeconfig`**, **`terraform.tfstate`**, or **`rackspace_token`**.
- If a Rackspace Spot or Linode token was committed to git, **rotate** it in the provider console and treat old credentials as compromised (git history rewrite is optional; rotation is required).

## Related

- **Infra provision:** **[`RELAY_INFRA_OPS.md`](./RELAY_INFRA_OPS.md)**
- **Deploy flow:** **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** (including § Secrets / OpenBao–style futures).
- **Mesh roles and secrets:** **[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)**.
- **Slack:** **[`SLACK_MESH_NOTIFY.md`](./SLACK_MESH_NOTIFY.md)**.

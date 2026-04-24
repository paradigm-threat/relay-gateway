# Interactive / agentic SSH with Cursor

**Auto-login (keys) is enough for SSH.** To make the **AI agent** work *on the server* (edit files, run shell there), the workspace must be **opened on the remote machine** via Cursor’s Remote SSH—not just a local terminal running `ssh`.

## 1. Use Cursor’s Remote SSH (not Microsoft’s)

1. Extensions → install **`Anysphere Remote SSH`** (`anysphere.remote-ssh`).
2. If you still have **`Remote - SSH` (Microsoft)** enabled, **disable or uninstall** it to avoid conflicts.
3. `Ctrl+Shift+P` → **Remote-SSH: Connect to Host…** → pick a host from `~/.ssh/config` (add `Host` / `HostName` / `User` / `IdentityFile` entries first).

After the window reloads, the status bar shows **SSH: hostname**. Terminals, file tree, and **Agent** actions target that remote machine.

## 2. SSH config tips

| Goal | Setting |
|------|--------|
| Fewer idle drops | `ServerAliveInterval 60`, `ServerAliveCountMax 3` (added to your `~/.ssh/config` **Host \*** if missing) |
| Faster repeat connects | `ControlMaster` / `ControlPath` / `ControlPersist` — **can conflict** with multiple Cursor windows on the *same* host; remove if unstable |

## 3. If the agent still feels “local only”

- Confirm the window title / status bar shows the **remote** host.
- Open a folder on the server (e.g. `/opt/relay` or your deploy dir), not a local clone, when you want deploy edits there.
- For one-off commands without full remote workspace: use the **integrated terminal** after connecting via Remote-SSH, or paste output back into chat.

## 4. Cursor Server install failures on Linux

If Remote-SSH hangs on “Installing Cursor Server”, check disk space on the server, `~/.cursor-server` permissions, and [Cursor forum / GitHub issues](https://github.com/cursor/cursor/issues) for lock-file / proxy fixes.

---

## 5. Non-root SSH (normative for agents and hardened nodes)

Relay nodes should be reached as a **normal UNIX user** with **`sudo`** (e.g. **`ari`**, **`deploy`**, **`ubuntu`**)—**not** **`root`**, especially after **`PermitRootLogin no`** and key-only auth.

**Linode / stock images:** Some images ship **password-only `root`** until you add a key user. A vendor username (e.g. **`protricity`**) may exist but often has **no** deploy key in **`authorized_keys`**—verify with **`ssh -o BatchMode=yes -o ConnectTimeout=15 user@ip 'echo ok'`** before assuming agent access. Prefer **`root` once** (or the image’s documented bootstrap user) to create **`ari`**, install **`~/.ssh/authorized_keys`** for the shared **`id_ed25519_relay_gateway`** key, then **`passwd -l root`** and use **`ari@`** for all further ops.

### `~/.ssh/config` (operators and automation)

```sshconfig
Host atlanta3
  HostName atlanta3.relaygateway.net
  User ari
  IdentityFile ~/.ssh/id_ed25519
```

Use the same **`User`** for **Cursor Remote-SSH** (§1) so the agent’s terminal and file tree match **`ssh atlanta3`**.

### Commands from a local agent or CI (non-interactive)

Prefer explicit user and batch mode so failures are immediate (no password prompt):

```bash
ssh -o BatchMode=yes -o ConnectTimeout=15 ari@atlanta3.relaygateway.net 'hostname'
```

Catalog / **`relay-server`** maintenance still uses **`sudo`** on the server (e.g. **`sudo systemctl restart relay-server`**, **`sudo -u relay …`** for **`/opt/relay/data`**). Do **not** assume **`ssh root@…`** works on hardened hosts. Prefer **`./scripts/relay-fleet.sh`** for non-interactive fleet SSH — see **[`RELAY_FLEET_CONSOLE.md`](./RELAY_FLEET_CONSOLE.md)**. **`scripts/relay-linode-redeploy-seed.sh`** defaults to **`RELAY_SSH_USER=ari`** and **`RELAY_SSH_IDENTITY=~/.ssh/id_ed25519_relay_gateway`** (override with env if needed).

**Shared deploy key (policy):** use the **same** automation key on **all** Relay servers (e.g. **`id_ed25519_relay_gateway`**) so redeploy and agent SSH stay predictable. **`install.sh`** / **`relay-install.sh`** can verify **`authorized_keys`** contains that key’s fingerprint when **`RELAY_VALIDATE_DEPLOY_SSH_KEY=1`** — see **[`SECRETS_POLICY.md`](./SECRETS_POLICY.md)** §3.

### Docs that use `root@` for scp/install

**[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** keeps **`root@IP`** examples for **first boot** on images that still allow root. On hardened nodes, **scp to `~/`** and run **`sudo ./install.sh …`** over **`ssh user@IP`**, or use **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** (catalog update) with **`ssh user@node`**.

See also **§ “Updating a catalog node”** in **`RELAY_TRUST_AND_BOOTSTRAP.md`** for **`sudo -u relay`** + **`sync-relay-catalog.sh`**.

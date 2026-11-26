# Install-time server features (`features.json`)

Optional Relay capabilities are chosen when you run **`install.sh`** (or **`reconfigure-features`**), stored under **`${RELAY_INSTALL_ROOT:-/opt/relay}/state/features.json`**, and exposed to clients as **`GET`/`POST /api/config` → `installed_features.manifest`** plus a short **`installed_features.summary`**.

Peers and client apps can **`POST /api/config`** on each node to discover what is **enabled** and which **subfeatures** (voices, language pairs, npm packages, etc.) are present on that server.

- **Manifest** — full structure written by **`install.sh`** (ports, paths, flags, inventories).
- **Summary** — compact view assembled by **`relay-gateway`** (counts, language lists, package names).

Schemas use **`schema_version`** (currently **2**). Older nodes may still serve **`1`** until they run **`reconfigure-features`** or a fresh **`install`**.

---

## Commands (installer)

| Command | Purpose |
|---------|--------|
| `sudo ./install.sh install` | First install; interactive prompts unless `RELAY_INSTALL_NONINTERACTIVE=1`. |
| `sudo ./install.sh reconfigure-features` | Add/remove optional features; reinstalls Piper / Argos venv / npm per answers. |
| `sudo ./install.sh refresh-features` | **Rescan inventories only** (Piper `*.onnx` voices, Argos installed packs) into `features.json` — use after you add Piper models or `argospm install …` without changing enabled flags. |
| `sudo ./install.sh repair` | Reapply binaries, systemd, and features **from existing** `features.json`. |
| `sudo ./install.sh update` | Replace **`relay-gateway`** / **`relay-hook-handler`** from the tarball directory; refreshes inventories. |

See also **[DEPLOY_LINODE.md](./DEPLOY_LINODE.md)** for DNS, tarball download, and non-interactive examples.

---

## Core (always on)

Recorded under **`features.core`** for discoverability:

| Subfeature | Meaning |
|------------|--------|
| `relay_server` | HTTP API (`RELAY_HTTP_PORT`, default **8080**). |
| `relay_git_daemon` | `git daemon` receive-pack (**`RELAY_GIT_PORT`**, default **9418**). |
| `ports` | `{ "http", "git_daemon" }` |

These are not toggled off by the installer today; they define the baseline node.

---

## Kubo IPFS (`features.ipfs_kubo`)

Default **off** for **`install`**, **`update`**, and **`repair`**. Set **`RELAY_INSTALL_IPFS=1`** when expanding the mesh needs seed pins again. Downloads **Kubo v0.32.1** into **`/opt/relay/lib/kubo/`**, repo at **`/opt/relay/ipfs`**, systemd **`relay-ipfs.service`**. When left at the default, install stops/disables any prior Kubo units and records `features.ipfs_kubo.enabled=false`.

| Field | Meaning |
|-------|---------|
| **`api`** | `http://127.0.0.1:5001` |
| **`gateway`** | `http://127.0.0.1:8081` (not 8080) |
| **`swarm_port`** | **4001** (public TCP+UDP) |
| **`service`** | `relay-ipfs.service` |

**`GET /api/status`** reports **`self.ipfs`** / **`ipfs_ok`**. Pin timer: **`relay-ipfs-pin-catalog.timer`**. That is **IPFS seed** (Kubo pins), not git **`ensure-bare`** seed. See **[`RELAY_IPFS.md`](./RELAY_IPFS.md)**.

---

## Optional: Piper TTS (`features.piper_tts`)

Local speech synthesis via **[Piper](https://github.com/rhasspy/piper/)** and **`relay-tts-piper`** (Python HTTP wrapper).

### Subfeatures (inventory)

| Field | Description |
|-------|-------------|
| **`voices`** | List of objects: `id`, `language` (e.g. `en_US`), `voice`, `quality`, `model_file` — derived from **`*.onnx`** files under **`models_dir`**. |
| **`languages`** | Union of language codes inferred from voice ids (BCP‑47 style where models use `ll_CC`). |
| **`default_model`** | Initial model path used by the systemd unit (installer default: **en_US-lessac-medium**). |
| **`http_port`** | Piper HTTP port (default **5590**). |
| **`health_path`**, **`tts_path`** | HTTP paths implemented by `piper-tts-http.py` (`/health`, `/tts`). |

After adding or removing **`.onnx`** models under **`lib/piper/models`**, run **`sudo ./install.sh refresh-features`** so **`voices`** / **`languages`** stay accurate.

### Non-interactive env

| Variable | Values |
|----------|--------|
| **`RELAY_FEAT_PIPER`** | `1` / `0` — **Linode fleet default is `1`** (`relay-ops features-set`, `relay-master-deploy`). Caddy exposes **`/voices`** + **`/tts`** to `relay-tts-piper`. Pass `--piper 0` only to disable. |

---

## Optional: npm extensions (`features.npm_extensions`)

Installs packages with npm under **`node_extensions/`** for hook tooling (e.g. **`songwalker-js`**).

### Subfeatures

| Field | Description |
|-------|-------------|
| **`packages`** | JSON array of package names installed on this server. |

### Non-interactive env

| Variable | Values |
|----------|--------|
| **`RELAY_FEAT_NPM_PKGS`** | Space-separated list; if set, feature is treated as enabled. |

---

## Optional: offline text translation (`features.text_translation`)

**Goal:** relay users can translate to/from many languages **on the node** (no cloud API) by standardizing on **[Argos Translate](https://www.argosopentech.com/)** in a dedicated venv.

Implementation today:

- **Backend:** `argos_translate_local`
- **Paths:** `venv_dir` / `venv_python` under **`lib/argos-venv`**
- **Packages:** Open-source **translation models** installed with **`argospm`** (ships in the venv after `pip install argostranslate`)

Relay does **not** ship every language pair by default (that would be very large). You choose packs at install time or later via **`argospm install PACKAGE`**, then refresh the manifest.

### Subfeatures (inventory)

| Field | Description |
|-------|-------------|
| **`install_argos_packages`** | Package ids passed to **`argospm install`** at last **`reconfigure-features`** (e.g. **`translate-en_es`**). |
| **`language_pairs`** | Directed pairs `{ "from", "to", "package" }` detected from Argos after packages are installed. |
| **`from_languages`** | Sorted distinct source codes. |
| **`to_languages`** | Sorted distinct target codes. |

If Argos is enabled but the probe fails (venv missing, import error), **`probe_error`** may appear until **`repair`** / **`reconfigure-features`** fixes the install.

### Operations

1. Enable during **`install`** or **`reconfigure-features`**, optionally listing **`translate-xx_yy`** style ids.
2. To add more pairs later (as **`relay`** or root):

   ```bash
   sudo -u relay /opt/relay/lib/argos-venv/bin/argospm install translate-en_de
   sudo ./install.sh refresh-features
   ```

3. Other nodes learn what is available by reading **`installed_features`** from this host’s **`POST /api/config`** response.

### Non-interactive env

| Variable | Values |
|----------|--------|
| **`RELAY_FEAT_TRANSLATION`** | `1` / `0` |
| **`RELAY_FEAT_TRANSLATION_PKGS`** | Space-separated **`argospm`** package names (optional). |

---

## Federating capability metadata

There is no background mesh sync yet: each peer should call **`POST /api/config`** on the node (with the correct per-repo **`Host`** header if you also need repo context) and merge **`installed_features.summary`** (or the full manifest) into its own client or routing layer. The roadmap for **event streams, subscriptions, pull-based propagation**, and **internal** mesh anti–over-ping (**SHA-first**; optional **`via` / hops**; independent of Slack) is **[`RELAY_MESH_NETWORKING_PLAN.md`](./RELAY_MESH_NETWORKING_PLAN.md)**.

Recommended client behaviour:

- Prefer nodes that advertise **`libretranslate_api`**, **`text_translation`**, or **`piper_tts`** as needed.
- When **`voice_count`** or **`language_pair_count`** is zero, treat the feature as “enabled but not provisioned” and fall back to another peer or a local-only path.

---

## Environment quick reference

| Variable | Feature |
|----------|---------|
| `RELAY_FEAT_PIPER=1` | Piper TTS |
| `RELAY_FEAT_NPM_PKGS='pkg …'` | npm extensions |
| `RELAY_FEAT_TRANSLATION=1` | Argos offline translation |
| `RELAY_FEAT_TRANSLATION_PKGS='translate-en_es …'` | Argos packs to install during that run |
| `RELAY_FEAT_LIBRETRANSLATE=1` | LibreTranslate HTTP API (**`libretranslate.service`**, venv under **`RELAY_LIBRETRANSLATE_ROOT`**) |
| `RELAY_LIBRETRANSLATE_LOAD_ONLY` | Comma-separated language codes for LibreTranslate (default **`en,ru`**) |
| `RELAY_LIBRETRANSLATE_PORT` | LibreTranslate listen port (default **5588**) |
| `RELAY_LIBRETRANSLATE_ROOT` | Install directory (default **`/opt/libretranslate`**) |
| `RELAY_LIBRETRANSLATE_CACHE_TTL` | Translation result cache TTL in seconds for identical **`POST /translate`** requests (default **`2592000`** ≈ 30 days; upstream LibreTranslate is 7 days without the installer patch) |
| `RELAY_LIBRETRANSLATE_SHARED_STORAGE` | Optional **`LT_SHARED_STORAGE`** for LibreTranslate (default in-app is **`memory://`**). Use e.g. **`redis://127.0.0.1:6379/0`** if you need the translation cache to survive process restarts (**`repair`** / **`update`** re-applies the site-packages patch after **`pip install`**) |
| `RELAY_FEAT_INBOUND_MAIL=1` | Receive-only OpenSMTPD catch-all → Maildir (**no outbound**); see **[MAIL_INBOUND.md](./MAIL_INBOUND.md)** |
| `RELAY_MAIL_DOMAINS` | Space/comma-separated domains for the catch-all (required when inbound mail is on) |
| `RELAY_MAIL_USER` | Local UNIX mailbox user (default **`relaymail`**) |
| `RELAY_MAIL_HOSTNAME` | SMTP banner hostname (default **`mail.<first-domain>`**) |
| `RELAY_MAIL_MAILDIR` | Override Maildir path (default **`/home/<user>/Maildir`**) |

Combine with **`RELAY_INSTALL_NONINTERACTIVE=1`** for cloud-init / CI (see **DEPLOY_LINODE.md**). **`repair`** / **`update`** restart LibreTranslate when it is enabled in **`features.json`**; **`ufw`** opens **`RELAY_LIBRETRANSLATE_PORT`** when the feature is on. When **`inbound_mail`** is enabled, **`ufw`** also opens **`25/tcp`** and **`repair`** / **`reconfigure-features`** re-apply **`/etc/smtpd.conf`**.

---

## Optional: LibreTranslate HTTP API (installer or manual)

**[LibreTranslate](https://github.com/LibreTranslate/LibreTranslate)** provides a **segment HTTP API** (many **`POST /translate`** calls suit long books). Enable it during **`install`** or **`reconfigure-features`** (TTY prompt), or set **`RELAY_FEAT_LIBRETRANSLATE=1`** non-interactively. The installer records **`features.libretranslate_api`** in **`features.json`**, installs a venv, writes **`libretranslate.service`**, and opens the firewall port.

**Translation cache:** the unit runs LibreTranslate with **`--translation-cache all`** so repeated identical segment requests are served from LibreTranslate’s internal cache. Upstream hardcodes a **7-day** expiry; **`relay-install.sh`** patches **`site-packages/libretranslate/cache.py`** so **`LT_TRANSLATION_CACHE_TTL`** (set in the unit from **`RELAY_LIBRETRANSLATE_CACHE_TTL`**, default ~30 days) controls TTL. Storage is LibreTranslate’s **`LT_SHARED_STORAGE`** (**`memory://`** unless you set **`RELAY_LIBRETRANSLATE_SHARED_STORAGE`**); in-memory entries are lost on service restart unless you point shared storage at **Redis**.

### Calling it by domain (recommended)

LibreTranslate listens on **`0.0.0.0:`** + **`RELAY_LIBRETRANSLATE_PORT`** (default **5588**). Clients should use the **same node FQDN** as Relay’s **`RELAY_PUBLIC_HOSTNAME`**:

- **Base URL:** **`http://{RELAY_PUBLIC_HOSTNAME}:{port}`**
- **Example:** **`http://dallas1.relaygateway.net:5588`**

As long as the **A record** for that name points at the server (same as Relay HTTP), **`curl`**, scripts, and apps can use the hostname directly; DNS resolves to the IP and the connection is identical to using the address literally.

### Typical endpoints

| Method | Path | Purpose |
|--------|------|--------|
| `GET` | `/languages` | Supported codes and targets |
| `POST` | `/translate` | JSON body: `q`, `source`, `target`, `format` (`text` or `html`) |

Example:

```bash
curl -sS -X POST "http://dallas1.relaygateway.net:5588/translate" \
  -H "Content-Type: application/json" \
  -d '{"q":"Hello","source":"en","target":"ru","format":"text"}'
```

### TLS

By default this is **HTTP on port 5588**. For HTTPS, terminate TLS in **Caddy/nginx** on **443** and proxy to **`127.0.0.1:5588`**, or use another front door you already run on the node.

---

## Optional: inbound mail catcher (`features.inbound_mail`)

**Goal:** every Relay deployment can optionally accept **legacy SMTP** for OTPs, registrar mail, and future Maildir **triggers** — without ever becoming a sending MTA.

- **Always available in ops:** **relay-ops** UI **Inbox** tab (SSH-reads the Maildir configured in **`ops/fleet.json`** / `RELAY_OPS_MAIL_*`).
- **Optional on a node:** OpenSMTPD receive-only catch-all installed by **`install.sh`** / **`reconfigure-features`**.

Relay does **not** send mail. SPF for receiving domains should be **`v=spf1 -all`**. Point **MX at exactly one** host; do not multi-MX the mesh.

### Subfeatures

| Field | Description |
|-------|-------------|
| **`domains`** | Catch-all domains (`@domain` → local user). |
| **`local_user`** | UNIX account owning the Maildir. |
| **`maildir`** | Absolute Maildir path. |
| **`smtp_hostname`** | Banner / `listen … hostname`. |
| **`listen_port`** | Always **25**. |
| **`outbound`** | Always **`false`**. |
| **`service`** | **`opensmtpd`**. |

### Operations

1. Enable during **`install`** or **`reconfigure-features`** (prompt or **`RELAY_FEAT_INBOUND_MAIL=1`** + **`RELAY_MAIL_DOMAINS`**).
2. Wire DNS **A** + **MX** (+ SPF) yourself — installer never touches DNS for mail.
3. Read with **relay-ops → Inbox** or SSH. Full walkthrough: **[MAIL_INBOUND.md](./MAIL_INBOUND.md)**.

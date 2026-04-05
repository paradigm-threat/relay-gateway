# Relay mesh networking and realtime events (plan)

**Status:** design / roadmap — the full **unified** realtime subsystem is not shipped yet. Pieces exist today (**`RELAY_MASTER_PEER_LIST`** → **`POST /api/config` → `peers`**, optional per-repo **Auto-Push** in **`.relay.yaml`** (not required for the target topology), **`RELAY_SYNC_IN_PROGRESS`** loop guard in **`relay-hook-handler`**, and **HTTP fan-out** for **pull / anchor validation failures** — see **`POST /hooks/mesh-event`** below). This document folds those into a single direction: **event-first control plane**, **Git as data plane**, and **mesh-local** control traffic.

**Product decision (settled):**

- **Mesh network = internal to Relay** — relay-to-relay notify, fetch, validation, and structured events stay on **our** HTTP (and later optional WebSocket/SSE) paths between nodes. We do **not** run an **IRC server** (or similar) for **chat** inside the product.
- **Human-visible “chat” = optional outbound adapters** — when operators want **#relay-ops**, **Slack**, **Discord**, **Matrix**, etc., the node acts as a **client** (or webhook poster) to **their** existing service. No requirement to self-host chat infra for Relay to function.
- **Install / update path** — enabling or disabling those adapters, and writing the right **`relay.env`** keys (or sidecar config), should live in **`relay-install.sh`** (packaged as **`install.sh`**) **`install`** / **`update`** flows so defaults stay off until the operator opts in.
- **Slack / chat is not authoritative** — correctness, sync, and **anti–over-ping** on the mesh **must not** depend on Slack (or any external chat). Chat adapters are **observability**; optional **Slack-side** dedup (e.g. **`conversations.history`**) only reduces duplicate **human** notifications. **Internal** mesh discipline uses **Git-addressed dedup** and the envelope rules in **§ Mesh anti–over-ping strategy** below.
- **Master vs edge (terminology)** — a **master** is a designated backbone node (**`RELAY_MASTER_PEER_LIST`**). An **edge** is any other **non-master** Relay server. **Data plane (Git)** is separate: **masters still accept pushes and updates from edges and other clients** the same way **any Git server** can — subject to normal **`.relay.yaml`**, **hooks**, **anchors**, and catalog policy — without requiring the pusher to be a master.
- **Master peer network (secure zone)** — **only masters** hold **system-level** credentials for **full** mesh **control-plane** features (inter-master **`POST /hooks/peer`**, **`POST /hooks/mesh-event`** fan-out, and other privileged **HTTP** mesh operations). **Edges** are outside that zone for those secrets and **do not send mesh control events to masters** (**one-way** master → edge for realtime server events for now). **Future:** limited **non-auth** or edge-originated commands **may** be enabled later; not part of the current target.
- **Transport between masters** — inter-master control traffic should use **TLS** with **mutual TLS (mTLS)** — see **§ Transport: HTTP vs TLS vs mTLS**.

**Related:** **[`RELAY_TRUST_AND_BOOTSTRAP.md`](./RELAY_TRUST_AND_BOOTSTRAP.md)** (anchors, catalog), **[`LAUNCH_AND_TEST_PLAN.md`](./LAUNCH_AND_TEST_PLAN.md)** (peer webhook sketch), **[`SERVER_HOOKS.md`](./SERVER_HOOKS.md)** (Auto-Push), **[`INSTALL_FEATURES.md`](./INSTALL_FEATURES.md)** (peer discovery via config), **[`SLACK_MESH_NOTIFY.md`](./SLACK_MESH_NOTIFY.md)** (optional Slack; non-normative for mesh).

---

## Goals

1. **Bootstrap peer awareness from env** — each node ships with a **default list** of **master** relays. **`relay-install.sh`** (tarball **`install.sh`**) seeds **`RELAY_MASTER_PEER_LIST`** from **`DEFAULT_RELAY_MASTER_PEER_LIST`** (currently **`atlanta2.relaygateway.net;atlanta3.relaygateway.net`**) unless **`RELAY_MASTER_PEER_LIST`** is set; this list defines the **master peer network** (secure-zone backbone). **Edge** nodes use masters for **discovery**, **realtime events**, and **`git fetch` / `git push`** as clients; they do **not** hold **master-only** mesh credentials or **emit** mesh control traffic **to** masters (**§ Master peer network and secure zone**). Future work may add explicit **`RELAY_NODE_ROLE`** and separate lists. **Optional outbound chat** keys are separate and **off** unless configured via the same install/update flow.
2. **Subscriptions** — a node (or any client) can **subscribe** to **another** relay’s **event stream** without necessarily mirroring every repo.
3. **Validation, not blind trust** — each node **verifies** incoming work using existing ideas: **`relayRootCommit` / `anchor_commit`**, optional **signed commits**, **`pre-receive`** / hook policy. “Blockchain-like” here means **hash-linked Git history + explicit policy**, not mandatory BFT consensus unless we add that later.
4. **Avoid push storms** — prefer **small, idempotent event messages** and **receiver-driven `git fetch`** over **blind push-to-all** for large meshes (see **Anti–over-ping** below).

---

## Control plane vs data plane

| Plane | Carries | Purpose |
|--------|---------|---------|
| **Control (events)** | Short messages: e.g. **`{ event_type, repo, ref, new_sha, origin, hops_remaining, via, seq }`** (+ optional HMAC/signature) | Tell subscribers **“there is a new tip”** so they can **decide** whether to pull or forward. |
| **Data (Git)** | Packfiles, **`git fetch` / `git push`** | Actual object transfer; **must** still pass **anchor / signature** rules. |

Separating the two avoids treating “mesh chatter” and “multi-GB sync” as the same mechanism.

---

## Pull-oriented sync enabled by events (summary plan)

1. **Publisher** (node where a commit landed) emits **one event per logical tip change** (debounced if many commits arrive in a burst — coalesce to **final SHA**).
2. **Subscribers** dedupe by **`sha`**: if the bare already has **`new_sha`**, **do nothing**.
3. Otherwise they schedule **`git fetch`** (from the publisher or a canonical URL) with **jittered backoff** so N nodes don’t hit the same upstream at once.
4. **Auto-Push** in **`.relay.yaml`** is **optional** legacy fast-path sync; the **target** topology does **not** require it — **masters** push realtime **events** to **edges**, and edges **`git fetch`** from the relevant master when their local bare is behind (**§ Relationship to today’s Auto-Push**).

This matches the **`POST /hooks/peer`** direction in **[`LAUNCH_AND_TEST_PLAN.md`](./LAUNCH_AND_TEST_PLAN.md)** (notify → **trigger fetch**), generalized beyond HTTP POST.

---

## Mesh anti–over-ping strategy (internal — does not use Slack)

Relays coordinate with **small authenticated control messages**. **Git** remains the source of truth for object transfer; the mesh only hints **“consider fetching.”** This section is **normative** for future envelope work; today’s **`POST /hooks/mesh-event`** payload is a subset (e.g. **`pull_validation_failed`**).

### Principles

1. **SHA-first / content-addressed dedup (primary)** — Every tip-style event carries **`repo`** + **`ref`** (or branch) + **`new_sha`**. Receivers **must** treat “already at **`new_sha`**” as **no fetch** and **no re-broadcast** of the same update (same as today’s **`new_sha`** idempotency on **`POST /hooks/peer`**). This does not require remembering “who pinged”; it follows Git identity.
2. **Cheap to ignore** — Parsing and SHA comparison should be O(1) per message; no dependency on external services.
3. **Optional loop and blast-radius limits** — For fan-out beyond a static peer list (gossip / multi-path), add **hop** and/or **visited** metadata (below). **Do not** rely on unauthenticated “visited” lists alone for security; pair with **shared secret / HMAC** (existing **`RELAY_PEER_SECRET`** / **`RELAY_MESH_EVENT_*`**) or stronger signing later.
4. **Slack is out of band** — Human-channel dedup (e.g. Slack history) is **not** part of mesh correctness; see settled product decision above.

### Optional envelope fields (when forwarding / gossip)

| Field | Role |
|--------|------|
| **`origin_fqdn`** (or **`origin`**) | Who first emitted the logical tip change (for logs and seq scoping). |
| **`hops_remaining`** | Decrement on forward; stop at **0** to cap blast radius (safety net even if **`via`** is wrong). |
| **`via`** (bounded list, e.g. ≤ 8 FQDNs) | Nodes already forwarded through; forward only to peers **not** in **`via`**; append self when sending. Mitigates immediate cycles; cap length to control size. |
| **`seq` / `event_id`** | Monotonic or content-derived id per **`(origin, repo, ref)`** stream to drop stale duplicates. |

### Other discipline (unchanged)

- **Tip-level events only** (not per object).
- **Coalescing / debounce** for bursty commits (emit **final** SHA per window).
- **Random jitter** before fetch; **exponential backoff** on failure.
- **Topology policy:** hub-and-spoke, **tree**, or **gossip** with **max hops** / **fan-out k** instead of blind **N²** mesh.
- **HMAC or signed envelopes** on the wire so random clients cannot drive **`git fetch`** storms.

### Design options (pick per deployment)

| Mechanism | Use when |
|-----------|----------|
| **SHA-only** | Static peer list, star topology, or each node only notifies its configured peers once per event. |
| **`via` + cap** | Gossip or redundant paths; need explicit “already notified these relays” without central registry. |
| **`hops_remaining`** | Always recommended as a cheap backstop alongside **`via`**. |
| **Debounced emit** | High churn repos; coalesce many commits into one notify with final SHA. |
| **Tree / hub roles** | Large meshes; only certain nodes forward to certain children (**`RELAY_MASTER_PEER_LIST`** tiers — future). |

**Appending “servers already pinged”** is exactly the **`via`** (or **`relayed_by`**) list pattern. Prefer **bounded** lists and **authenticated** messages so a malicious peer cannot strip names to amplify load elsewhere.

---

## Target event envelope (normative direction for new mesh types)

Future tip-notify and forwarded events should share a small JSON shape (extends today’s **`MeshEventPayload`** ideas):

| Field | Required for tip notify | Notes |
|--------|-------------------------|--------|
| **`event_type`** | Yes | e.g. **`git_tip`**, **`pull_validation_failed`**. |
| **`repo`**, **`branch`** or **`ref`**, **`new_sha`** | Yes for tip | **`new_sha`** = 40 hex; omit or sentinel for non-tip events. |
| **`origin_fqdn`** | Recommended | Issuer of the logical event. |
| **`hops_remaining`** | Recommended when forwarding | Integer; receiver may forward only if **> 0** after decrement. |
| **`via`** | Optional | Array of normalized FQDN strings; max length enforced in implementation. |
| **`seq`** or **`event_id`** | Optional | Ordering / idempotency across retries. |
| Auth | Yes | Header secret today; document HMAC body later. |

Receivers apply in order: **auth OK** → **SHA already local?** skip fetch and usually skip forward → **self in `via`?** skip forward → **decrement hops / append self** → forward to selected peers.

---

## Internal mesh vs external “chat” (channel metaphor)

**IRC-style *organization*** (topics, many subscribers, fan-out) is still a useful **mental model** for **routing keys** inside our payloads (e.g. **`RELAY_MESH_EVENT_CHANNEL`**, future **`channel`** in JSON). That does **not** mean we speak the **IRC wire protocol** on the mesh.

### A — Internal mesh (relay ↔ relay)

Traffic between relays uses **Relay-owned** mechanisms:

- **Direction (target):** **masters** originate **control-plane** mesh HTTP to **other masters** and **out** to **edges** (events, notify). **Edges do not** call masters’ privileged mesh ingest (**`POST /hooks/mesh-event`** / peer-hook–class paths) to “report up”; **one-way** for now. **Future:** optional **non-auth** or limited edge→master commands **may** be added.
- **Today:** **`POST /hooks/peer`**, **`POST /hooks/mesh-event`**, **`POST /git-pull`**, **`git://`** data plane, shared secrets (**`RELAY_PEER_SECRET`**, **`RELAY_MESH_EVENT_*`**). Anti–over-ping follows **§ Mesh anti–over-ping strategy** ( **`new_sha`** idempotency today; **`via` / hops** when we add forwarded tip events). **Inter-master** paths should move to **HTTPS + mTLS** when wired (**§ Transport**).
- **Future (optional):** **WebSocket** or **SSE** for edge **subscription** to master event streams — still **not** IRC.

**Inbound listener** for *other relays* (Mode 2 / master hub below) remains in scope for **mesh event subscribers only**, not for arbitrary public chat.

### B — External notify (optional, install-script–gated)

For **humans** in **Slack / IRC / Discord / Matrix / …**:

- Relay runs **outbound** logic only: **webhook POST**, **IRC client** to **their** network, **Slack/Discord bot APIs**, etc.
- **Credentials and toggles** live in env (or a small sidecar config) and are **documented and optionally prompted** in **`relay-install.sh`** **`install`** / **`update`** so production nodes stay minimal by default.
- **`RELAY_MESH_EVENT_CHANNEL`** (and similar) remain **hints** for those adapters (e.g. target **#relay-ops**), not a promise that Relay hosts that channel.

### Mode 1 — Mesh: find a peer and **monitor** events

On startup (or periodically), the node may **discover** peers via **`RELAY_MASTER_PEER_LIST`** (and/or DNS SRV / well-known URL — TBD), then attach using **Relay’s** protocol (HTTP today; WebSocket/SSE later).

### Mode 2 — Mesh: **inbound** subscriber socket (relays / tooling only)

If policy says **“be a hub,”** the node may accept **inbound** connections for **mesh event subscribers** (other relays, authorized automation). Same **Relay** framing as above — **not** a public IRC service.

### Mode 3 — **Master** relays

**Designated masters** form the **master peer network** (**§ Master peer network and secure zone**): they hold **system-level** mesh credentials, sync/notify **with each other** (**TLS + mTLS** target), and fan **events** to **edges**. They **still** accept **Git** from **edges** and other clients per normal policy. **Master ≠ chat server.**

### Env configuration (conceptual)

**Mesh (internal):** **`RELAY_MASTER_PEER_LIST`**, **`RELAY_MESH_EVENT_*`**, **`RELAY_PEER_SECRET`**, etc.

**External chat (optional, names illustrative — finalized when adapters ship):**

- **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`**, **`RELAY_NOTIFY_DISCORD_WEBHOOK_URL`**, …
- **`RELAY_NOTIFY_IRC_SERVER`**, **`RELAY_NOTIFY_IRC_NICK`**, **`RELAY_NOTIFY_IRC_PASSWORD`**, **`RELAY_NOTIFY_IRC_CHANNEL`**, … (TLS/SASL TBD)

**`relay-install.sh`** should offer **non-interactive defaults (off)** and **interactive** prompts only when the operator opts into notifications.

---

## Master peer network and secure zone (normative for control plane)

The **master peer network** — the set of nodes identified as **masters** (bootstrap: **`RELAY_MASTER_PEER_LIST`**, plus future explicit **role** / **`RELAY_SERVER_ID`** policy) — is **required** for this design’s **trusted mesh control plane**. **Only masters** are issued **system-level** credentials for **full** mesh **HTTP** features (inter-master notify, mesh event fan-out, and any future **privileged** control APIs in this family). **All other nodes** are treated as **potentially outside the secure zone**: they do **not** receive **master-only** secrets and must not be able to impersonate the **inter-master** control plane.

### Control plane vs Git data plane (masters still accept edge Git clients)

| Plane | Who participates | Edge behavior |
|--------|------------------|---------------|
| **Control (HTTP mesh)** | **Masters** hold **full** auth; inter-master traffic uses **master-tier** secrets over **TLS + mTLS** (target). **Masters → edges:** realtime **events** (subscription / notify). **Edges → masters:** **no** privileged mesh ingest for now (**one-way**); future **non-auth** commands TBD. | Edges **do not** receive **master-only** secrets. They **subscribe** to master-originated events and **pull** Git when notified — they **do not** POST mesh control events **up** to masters. |
| **Data (Git)** | **Any client** the server already allows (same as **any Git host**). | **Edges** (developers, CI, other hosts) may **`git push`**, **`git fetch`**, etc., to a **master** per **daemon access**, **catalog / allowlist**, **`.relay.yaml`**, **pre-receive**, and **anchors** — **independent** of master vs edge role. |

So: **“outside the secure zone”** applies to **mesh control credentials**, not to **Git’s ability to accept pushes** from authorized clients.

### Roles (conceptual)

| Role | Control plane | Data plane (Git) |
|------|---------------|------------------|
| **Master** | Inter-master sync/notify (**TLS + mTLS**); fan-out **events** to **edges**; **master-tier** secrets (set at **install**). | Serves repos; accepts **push/fetch** from edges and other clients per policy. |
| **Edge (non-master)** | Receive **master-originated** events in realtime; **no** mesh control POST **to** masters (for now). Optional future: limited inbound routes without mesh secrets. | **`git fetch`** / **`git push`** to masters (or others) per normal Git rules. |

**`RELAY_MASTER_PEER_LIST`** today seeds the **master** FQDN set in **`install.sh`**; runtime **role** (“this running node is a master”) should align with ops (same list + **`RELAY_PUBLIC_HOSTNAME`**, or an explicit **`RELAY_NODE_ROLE=master`** when we add it).

### Restricting **HTTP** “pings” (not Git)

**Ignore or 403** on a **master** for **`POST /hooks/peer`** / privileged mesh ingest when the caller is **not** **master-authenticated** — so **edges cannot drive** master mesh **inject** loops. Combined with **one-way** event flow, edges **do not** need to POST validation failures (or similar) **to** masters; masters **originate** authoritative notify. This does **not** prevent **`git push`** from an edge; it parallels **SSH/git** being open to many clients while **privileged mesh HTTP** stays master-only.

*Today’s code* still uses a single **`RELAY_PEER_SECRET`** on many hosts; **master-only** distribution of **`RELAY_MASTER_MESH_SECRET`** (and rejecting **edge-originated** privileged mesh **ingest** on masters) is the target hardening.

### Master-tier secret / auth — effect

| Goal | Effect |
|------|--------|
| **Third-party / edge meddling on mesh HTTP** | **Strong reduction** if **master** secrets exist **only on masters**, **edges** do not originate privileged mesh calls **to** masters, and **TLS + mTLS** protects **inter-master** wire traffic. |
| **Honest duplication among masters** | Still use **SHA-first** + optional **`via` / hops** (**§ Mesh anti–over-ping strategy**). |
| **Git pushes from random clients** | Still governed by **Git policy**, not by master mesh membership. |

### Suggested env shape (illustrative — not all shipped)

| Variable | Purpose |
|----------|---------|
| **`RELAY_MASTER_MESH_SECRET`** (or **`RELAY_INTER_MASTER_SECRET`**) | Shared **only** on **master** nodes for **inter-master** **`POST /hooks/peer`** / **`POST /hooks/mesh-event`**. |
| **`RELAY_PEER_SECRET`** | **Target:** **absent** on **edges** for mesh **inject**, or **non-privileged** scope only (**never** accepted on masters for privileged peer hook). Today one secret is often shared; migrate toward **master-only** for inject on masters. |
| **`RELAY_MESH_EVENT_SECRET`** | Optional override for mesh events; on masters may **require** parity with **master-tier** secret when **`RELAY_MASTER_ONLY_MESH_INGEST=1`**. |
| **`RELAY_MASTER_ONLY_PEER_HOOK=1`** (conceptual) | On a **master**, accept **`POST /hooks/peer`** for **privileged** paths only with **master-tier** auth + caller in master list. |

**Operational rule:** **master-tier** credentials stay **only** on machines you treat as inside the **secure zone**; **edge** nodes rely on **Git** and **non-privileged** HTTP only (plus **receiving** master-originated events).

### Transport: plain HTTP vs TLS vs mTLS (why it matters)

Relay mesh control messages today are often described as “**HTTP**” because the application uses **request/response** paths (**`POST /hooks/peer`**, **`POST /hooks/mesh-event`**, …). That says nothing about **encryption** or **identity** on the wire:

| Layer | What it gives you | Caveat |
|--------|-------------------|--------|
| **HTTP (cleartext)** | Simple to run on a LAN or loopback. | Any observer on the path can read **bodies and headers** — including **bearer secrets** if you send them in headers. Not acceptable for **inter-master** mesh over untrusted networks. |
| **HTTPS (TLS, server auth only)** | **Encrypts** the channel; the **client** verifies the **server** certificate. | The **server** does not automatically know **which client** connected unless the app adds **tokens**, **HMAC**, or similar — TLS alone is **one-way** identity at the transport layer. |
| **HTTPS + mTLS** | **Mutual** TLS: **both** ends present certificates; **each side** verifies the peer before application logic runs. | Fits **machine-to-machine** **inter-master** links: pair with **small master list** / CA and reduce reliance on long-lived bearer secrets **leaking off the TLS-protected path**. |

**Target:** **inter-master** control-plane traffic uses **TLS** with **mTLS**. **Master → edge** delivery should also use **TLS** where the event channel crosses an untrusted network (exact topology TBD: reverse proxy, dedicated listener, or future WebSocket/SSE over HTTPS). **`git://`** data plane remains a **separate** concern (**unauthenticated** by default); do not confuse it with mesh **HTTPS** hardening.

### Inter-master secrets (install-time; no rolling codes for now)

**One shared secret** (or cert-bound identity via mTLS) established **at install** is **enough** for the current plan — **no** time-based rolling codes (**TOTP-style**) for masters **for now**. **Future:** optional **scheduled rotation** or host **secret manager** integration without changing the “single logical credential per deploy generation” model.

**Ops note:** use **OpenBao / Vault** (self-hosted on Akamai Cloud) or **another** secret store + **Vault Agent** / sync to **`relay.env`** for mesh / install secrets and rotation — see **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** § **Secrets (mesh / future)** (no separate Linode-managed secrets product as of 2026).

---

## Questions and answers

### A. Does IRC-style organization enable **pull** instead of **push-to-all**?

**Yes**, if **events are notifications, not the Git payload.** A subscriber that receives **`{ repo, ref, new_sha }`** can:

- **Ignore** if it does not host that repo or does not care about that ref.
- **Defer** pull until load is low.
- **Fetch** from the **publisher** or from a **canonical forge URL** (catalog **`url`**) depending on policy.

That is exactly the **decouple “who knows” from “who transfers objects”** pattern. The **target** topology does **not** require **Auto-Push**: **edges** receive **realtime events from masters** and **`git fetch`** from the same (or catalog) upstream when local repos are behind. **Per-repo Auto-Push** (**`originList`**) remains **optional** shipped behavior for operators who want **git-push** hot-standby sync without waiting for pull.

### B. Does the same design enable **realtime IRC-style chat**?

**Human chat is out of scope for the mesh transport.** Mesh events stay **operations-focused** (tips, validation failures, policy).

For **operator visibility**, we **forward selected events** to **existing** chat systems as **optional outbound clients** (see **External notify** above). That avoids running a **chat server** inside Relay while still allowing **#relay-ops**-style workflows where the team already lives (Slack, IRC company net, etc.).

If we ever add **in-product** chat, it should remain a **separate** feature with its own policy — not mixed into the **mesh** control plane.

### C. Does the mesh depend on Slack (or chat) for dedup or sync?

**No.** **Slack** (and similar) are **optional sinks** for operators. **Internal** anti–over-ping and **deduplication** use **SHA-first** rules and, when needed, **`via`** / **`hops_remaining`** on the mesh envelope — see **§ Mesh anti–over-ping strategy**. Chat-side dedup is **human convenience only**.

### D. Are masters the only nodes with full mesh auth? Can edges still push Git to masters?

**Yes** to both, by separating planes: **only masters** hold **master-tier** credentials for **inter-master** **HTTP** and for **originating** mesh events **to** **edges**. **Edges** are **outside** that zone and **do not** send mesh control events **to** masters (**one-way** for now).

**Masters still accept Git from edges** when **Git policy** allows: **`git push`** / **`git fetch`** are **data plane** (**hooks, anchors, catalog**), not mesh membership. **Privileged mesh HTTP** on masters is **master-authenticated** only. See **§ Master peer network and secure zone**.

---

## Relationship to today’s Auto-Push

**[`relay-hook-handler`](../src/bin/relay-hook-handler.rs)** **Auto-Push** pushes branch tips to **`originList`** after **`post-receive`**, with **`RELAY_SYNC_IN_PROGRESS`** to reduce loops. For the **target** **master → edge** model, **Auto-Push is not necessary**: **edges** get **tip / validation / policy** signals from **masters** in realtime and run **`git fetch`** (or **`git-pull`**) against the master (or catalog **`url`**) when they need objects — **no** **`git push`** storm to every **`originList`** peer.

**Auto-Push** stays **available** as an **optional** per-repo **`originList`** shortcut for deployments that still want **direct git pushes** to listed remotes (e.g. one-hop mirrors); it is **not** on the critical path for **event + pull** propagation.

---

## Implementation status (tracking)

| Piece | Status |
|-------|--------|
| **`RELAY_MASTER_PEER_LIST` → `/api/config` → `peers`** | **Shipped** |
| **Per-repo Auto-Push (`originList`)** | **Shipped** (hooks) |
| **`POST /hooks/peer` notify → `git fetch` from `git://peer:port/repo.git`** | **Shipped** — header **`X-Relay-Peer-Secret`** = **`RELAY_PEER_SECRET`**; when **`RELAY_MASTER_PEER_LIST`** is non-empty, **`peer`** in the JSON body must match a list entry. Optional **`new_sha`**: skip fetch if the branch tip already equals that commit. |
| **Mesh HTTP events (`pull_validation_failed`)** | **Shipped (minimal)** — on validation failure after ingest, the node **POSTs** to each peer’s **`POST /hooks/mesh-event`** (excluding self). **Target:** only **masters** originate such fan-out **to** **edges** / other masters; **edges** do **not** POST **up** to masters (**§ Master peer network**). **Today** any node with the secret may fan out — see **§ Plan tensions**. **Auth:** **`x-relay-mesh-secret`** vs **`RELAY_MESH_EVENT_SECRET`** / **`RELAY_PEER_SECRET`**; **targets:** **`RELAY_MESH_EVENT_URLS`** or **`RELAY_MASTER_PEER_LIST`**-derived URLs + **`RELAY_MESH_EVENT_SCHEME`**. Optional **`RELAY_MESH_EVENT_CHANNEL`** is a routing hint for adapters. |
| **Tip-change mesh notify + envelope (`via`, `hops_remaining`, `origin`)** | **Not shipped** — strategy and target fields documented in **§ Mesh anti–over-ping strategy** and **§ Target event envelope**. |
| **Unified internal event protocol (WebSocket/SSE between relays)** | **Not shipped** |
| **Optional outbound notify (Slack Incoming Webhook + ops events)** | **Shipped (minimal)** — **`RELAY_NOTIFY_SLACK_WEBHOOK_URL`** drives **human** notifications (validation failures, **`commit_update`**, startup, hook errors, etc.); optional Slack **history** dedup for **`commit_update`** only (**`RELAY_SLACK_BOT_TOKEN`**, **`RELAY_SLACK_DEDUP_CHANNEL_ID`**). **Not** used by the mesh control plane (see **`docs/SLACK_MESH_NOTIFY.md`**). **`relay-install.sh`** prompts are **not** added yet (env-only). |
| **Optional outbound notify (IRC client, Discord, …)** | **Not shipped** |
| **WebRTC signaling + DataChannel for events** | **Not shipped** (low priority vs mesh HTTP/WebSocket) |
| **Master-only inbound listener flag** (mesh subscribers) | **Not shipped** |
| **Master-tier mesh auth** (`RELAY_MASTER_MESH_SECRET`, master vs **edge** role, **edge** does not publish mesh HTTP; strict master ingest when master secret set) | **Shipped (minimal)** — **`src/mesh_role.rs`**, **`mesh_events`**, **`peer_hook`**; **mTLS** still external |
| **Signed / HMAC event envelopes** | **Not shipped** |

---

## Next steps (engineering, when prioritized)

1. Specify **stable internal event JSON schema** aligned with **§ Target event envelope** (tip notify: **`repo` / `ref` / `new_sha` / `origin` / `hops_remaining` / optional `via`**); tighten **auth** (HMAC body vs header-only secret vs mTLS).
2. Implement **receiver-side** rules: **SHA-first skip**, then **`via`** / **hop** checks before any forward (Slack not involved).
3. Implement **one** internal transport upgrade (**WebSocket** on existing HTTP port is the default candidate) before WebRTC.
4. Add **tip-change mesh notify** from **masters** to **edges** / masters: e.g. **`post-receive`** emits **`git_tip`**-style event per **§ Mesh anti–over-ping strategy** (edges **pull**; **Auto-Push** optional).
5. **Outbound adapters:** extend **`relay-install.sh`** prompts for Slack/webhooks when stable; keep adapters **strictly optional**.
6. Prototype **tree** topology (master → regionals → edges) to validate **fan-out** and **no N²** behavior.
7. Implement **master-tier** auth (**`RELAY_MASTER_MESH_SECRET`**, **`RELAY_MASTER_ONLY_PEER_HOOK`**) on **`POST /hooks/peer`** / **`POST /hooks/mesh-event`** on **masters** per **§ Master peer network and secure zone** (Git **push/fetch** behavior unchanged).

---

## Plan tensions: target model vs shipped code (read this before auditing)

These are **not** logical bugs in the *target* design, but **gaps** between **§ Master peer network and secure zone** and **what runs today**:

| Tension | Today | Target |
|---------|--------|--------|
| **Mesh HTTP secrets** | One **`RELAY_PEER_SECRET`** (and optional **`RELAY_MESH_EVENT_SECRET`**) is typically deployed on **every** node that runs **`relay-server`**. | **Master-only** **`RELAY_MASTER_MESH_SECRET`** for **privileged inbound** on masters; **edges** do not receive it. |
| **Who may call `POST /hooks/peer` on a master?** | Anyone with the shared secret (and **`peer`** in **`RELAY_MASTER_PEER_LIST`** when that list is non-empty). | **Master-tier** auth only; **edges** do **not** drive privileged inject (**implementation TBD**). |
| **Outbound mesh from edges** | Any node with the shared secret can **fan out** **`pull_validation_failed`** (and similar) today. | **Settled (target):** **only masters** originate mesh HTTP events **to** **edges** and other masters; **edges do not** POST mesh control **to** masters. **Future:** optional **non-auth** edge→master commands **not** in scope yet. |
| **`RELAY_MASTER_PEER_LIST` name** | Used both as **install default list of master FQDNs** and as **`peer`** **hostname allowlist** in **`POST /hooks/peer`** (the Git peer to fetch from — **not** HTTP client identity). | May need **`RELAY_MASTER_REGISTRY`** (or role flags) **vs** **`RELAY_PEER_ALLOWLIST`** to avoid overloading one variable. |
| **Edge still runs `relay-server`** | Edge nodes can expose **full HTTP API** per env today. | **Mesh control-plane privilege** is **master-only**; **edge** = **non-master** Relay. Clarify per-endpoint **tier** (public / edge / master). |
| **Slack / outbound chat** | Webhook URL on **any** node can post to Slack. | **Observability only**; not mesh authority — but ops should know **edges can spam** the channel if compromised. |

---

## Open decisions (to answer before or during adapter work)

1. **First adapter** — Slack webhook (simplest ops), **IRC** (TLS + SASL story), Discord, Matrix, or multiple with a **pluggable** interface?
2. **Where code runs** — **in-process** in **`relay-server`** vs **small sidecar** (easier dependency isolation for IRC/Slack SDKs, separate restart).
3. **Event filter** — only **`pull_validation_failed`** and similar **errors**, vs **all** ingested **`mesh-event`** types, vs **configurable allowlist** per destination.
4. **Secrets** — env-only (**`relay.env`**) today; **OpenBao / Vault** (Marketplace or LKE) or external secret stores + agent/sync for Linode-hosted nodes — see **[`DEPLOY_LINODE.md`](./DEPLOY_LINODE.md)** § **Secrets (mesh / future)**. **Never** log tokens.
5. **Payload hygiene** — redact paths, SHAs length limits, rate limits to avoid chat spam during flapping meshes.
6. **Multi-tenant** — one global notify config per node vs per-repo / per-**`relay_server_id`** routing (if ever needed).
7. **Max `via` length and hop default** — policy constants vs env (e.g. **`RELAY_MESH_EVENT_MAX_HOPS`**, **`RELAY_MESH_EVENT_MAX_VIA`**).
8. **Master vs edge** — explicit **`RELAY_NODE_ROLE`** (or equivalent) vs inferring from **`RELAY_MASTER_PEER_LIST`** + **`RELAY_PUBLIC_HOSTNAME`**; whether **edges** ever get **limited** HTTP routes beyond **subscribe + git-pull** vs **full** server with **tiered** auth per route.
9. **Edge → master mesh traffic** — **Settled:** **edges do not** emit mesh control **to** masters (**one-way** master → edge for now). **Future:** optional **non-auth** commands TBD.
10. **Split env: master registry vs peer allowlist** — keep one **`RELAY_MASTER_PEER_LIST`** or separate **“who is a master”** from **“which hostnames may appear in `peer` body”**?
11. **Edge `relay-server` surface** — subset of routes vs full server with **tiered** auth per route?
12. **Anonymous `git://`** — data plane remains **unauthenticated** by default; **settled** for mesh: **TLS + mTLS** between **masters**; **master → edge** over **TLS** when crossing untrusted paths (**§ Transport**). Shared **install-time** secret (no rolling codes for now — **§ Inter-master secrets**).

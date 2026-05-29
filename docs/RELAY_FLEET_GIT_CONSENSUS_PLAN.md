# Relay fleet Git consensus and auto-reconciliation (plan)

**Status:** design / roadmap — **not implemented**. Supplements [RELAY_AUTH_PROTOCOL.md](./RELAY_AUTH_PROTOCOL.md) (push signing, fast-forward policy) and [RELAY_FLEET_CONSOLE.md](./RELAY_FLEET_CONSOLE.md) (operator tooling).

**Repository note:** This file lives in the **relay-gateway** repository. The server is the Rust library **`relay_server`** (Cargo package name `relay-gateway`). The legacy binary name **relay-server** is retired (see [RELAY_RECONCILE_PLAN.md](./RELAY_RECONCILE_PLAN.md) naming row).

## Problem

Bare mirrors on multiple masters (`atlanta2`, `atlanta3`, `jp-osa`, …) can hold **different OIDs** for the same `refs/heads/<branch>` on the same repo. That causes:

- Non-fast-forward pushes and operator merges when tips diverge.
- **Race conditions** when auto-push and peer fetch interleave.
- Uncertainty for clients: a commit can be accepted on one node but never become the **de facto** network tip.

Goal: **reduce manual repair** (`reclone-repo`, ad hoc resets) while keeping behavior **predictable** and **observable**.

## Design: priority authority + follower alignment

### Authority list

Introduce an explicit **ordered** list of fleet hostnames (highest authority first), e.g. in `relay.env` / `POST /api/config`:

- **`RELAY_GIT_AUTHORITY_ORDER`** — example: `atlanta3.relaygateway.net;atlanta2.relaygateway.net;jp-osa.relaygateway.net`
- Index **0** is the **canonical** relay for “what should the bare `main` tip be?” among peers when they disagree.
- Each node resolves its own rank via **`RELAY_PUBLIC_HOSTNAME`** / **`RELAY_NODE_FQDN`** against that list.

### When two tips disagree

For a given bare repo `R` and branch `B`, let `tip(node)` be `refs/heads/B` on that node’s `R.git`.

For a **lower-ranked** node **L** comparing to a **higher-ranked** node **H**:

| Relationship | Follower action (proposal) |
|--------------|----------------------------|
| `tip(H)` is ancestor of `tip(L)` (H behind L) | **Alert** — authority is behind follower; do **not** auto-reset H from L without a separate “promotion” policy. Optionally **freeze** pushes on L until ops resolves. |
| `tip(L)` is ancestor of `tip(H)` (L behind H) | **Fast-forward** fetch from H (compatible with today’s non-force peer fetch in many cases). |
| **Neither** is ancestor of the other (true divergence) | **L** runs a **controlled** reconcile: fetch from H, then **move `refs/heads/B` to `tip(H)`** (equivalent to reset-to-authority for that ref). **Commits reachable only from `tip(L)` are dropped from that branch on L.** |

This matches the product ask: **Atlanta2 follows Atlanta3** when they detect they are out of sync, including hard cases where a plain fast-forward is impossible.

### Safety and “foolproof” expectations

True **single global order** without a separate truth source requires trusting the priority host’s tip. That is **not** Byzantine-fault tolerant:

- If the canonical host is **stale** or **wrong**, followers can **discard** newer good work.
- Mitigations to specify in a later revision:
  - Require `tip(H)` to pass the same **signing / `.relay.yaml`** rules as a push before L resets to it.
  - Optional **forge guardrail:** only allow auto-reset on L if `tip(H)` is an ancestor of **forge `main`** (or passes catalog anchor rules), so relays never “vote” ahead of GitLab unless explicitly configured.
  - **Quorum / two-of-N** agreement before destructive reset (more complex).
  - Safer default for some repos: **alert + push freeze** instead of auto-reset until an operator confirms.

The plan should default to **maximum alerting** and conservative behavior where configuration is ambiguous.

### Implementation touchpoints (for future work)

- **Peer fetch today:** [peer_hook.rs](../src/peer_hook.rs) `fetch_from_peer_url` uses a **non-force** refspec — reconciliation needs either a **trusted internal** force-fetch path or `git fetch` + `git update-ref` under the same peer-secret / allowlist gates.
- **Auto-push today:** [relay-hook-handler.rs](../src/bin/relay-hook-handler.rs) `handle_auto_push` uses `git push` **without** `--force` — it will not heal divergence alone.
- **Alerts:** extend existing [notify.rs](../src/notify.rs) / mesh-event patterns ([SLACK_MESH_NOTIFY.md](./SLACK_MESH_NOTIFY.md)) with a dedicated event type, e.g. `fleet_git_reconcile`, including: `repo`, `branch`, `follower_host`, `authority_host`, `old_oid`, `new_oid`, `reason` (`divergence` | `fast_forward` | `behind_authority` | `authority_behind_follower`).

### Operator UX

- **`relay-fleet.sh`** (or new subcommand): **`git-tips REPO BRANCH`** — print each fleet member’s OID for that ref (parallel HTTP or SSH + `git rev-parse`), similar spirit to **`drift`** but for bare mirrors.
- Document reconcile loop placement: **systemd timer** on each master vs central orchestrator — either works if authority order is shared config.

## UI: confirmation model (blockchain-like)

When the product UI performs a **commit** or **HTTP write** that should persist across the fleet:

1. Treat **authority list index 0** as the **confirmation target** for “this change is what the network considers canonical on relays.”
2. After success on the node the client used, **poll** that canonical host until:
   - the expected **commit OID** appears on `refs/heads/<branch>` (or on a small HTTP surface such as an API returning `{ repo, branch, oid }`), or
   - a **timeout** elapses.
3. If the OID **never** appears on the canonical host, show an explicit **degraded / uncertain** state: the change might exist only on a partition, auto-reconcile might not have run, or forge/catalog path might be broken — **the user cannot assume durability**.
4. Optional: show **weak confirmations** — count of fleet members whose `tip` OID **equals** the canonical OID (display-only; not a substitute for signing policy).

This is analogous to **Bitcoin confirmations**: more corroboration increases confidence; absence on the “main” view means survival is unknown.

## Relation to other plans

- **[RELAY_RECONCILE_PLAN.md](./RELAY_RECONCILE_PLAN.md)** — reconciles **relay-gateway binary** + manifest from relay-master; **orthogonal** to bare-repo **content** consensus between masters.
- **Amend-only / tip-replace push policy** (separate design) — can coexist but does not replace fleet alignment; one node’s amended tip still must reach or override followers via the mechanisms above.

## Open questions

- Per-repo authority order vs one global list?
- Default on divergence: **auto-reset** vs **freeze + alert only** for `main`?
- Interaction with **catalog ingest** and **forge** when both are sources of truth for the same repo name.

## Related

- [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md) — anchors, catalog, ingest trust.
- [OPERATOR_BARE_REPO_RESET.md](./OPERATOR_BARE_REPO_RESET.md) — today’s manual reclones and peer fetch.
- [RELAY_AUTH_PROTOCOL.md](./RELAY_AUTH_PROTOCOL.md) — fast-forward and signing norms.

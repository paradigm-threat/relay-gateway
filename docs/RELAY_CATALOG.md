# Unified Relay catalog (`relay-master/catalog/`)

Normative design for per-repo policy in **one** catalog tree. Implementation: [`src/catalog_policy.rs`](../src/catalog_policy.rs), [`src/authorized_repos.rs`](../src/authorized_repos.rs), [`src/git/branch_policy.rs`](../src/git/branch_policy.rs).

## Layout

```
relay-master/
  catalog/
    index.json
    policy-profiles.json
    repos/
      <repo-name>.json
  relay-catalog.json          # generated legacy aggregate (build-relay-catalog-json.sh)
```

| Profile (`tier`) | Rules |
|------------------|--------|
| **`primary`** | Lax — binaries OK, `publish_branch` usually `main`, signing may stay in repo `.relay.yaml` during migration (`signing_keys_source: repo`) |
| **`hosted`** | Strict — text-only Git, `publish_branch` still **`main` unless the record overrides it**, catalog-authoritative `signing_keys` |

## Per-repo record (`relay.catalog-repo.v1`)

| Field | Purpose |
|-------|---------|
| `repo` | Bare mirror slug (`paradigm-threat-files`) |
| `description` | Short public blurb for directory UIs and `/api/status` / `/api/config`. Empty/omitted = none. Independent of whether the bare is live on a node. |
| `tier` | `primary` \| `hosted` |
| `forge_url` | Upstream clone URL |
| `relay_root_commit` | Trusted root (40 hex) |
| `publish_branch` | Branch served on Relay HTTPS / anchor checks (this fleet: **`main`**) |
| `publish_origin` | Optional FQDN of the one Relay that should accept client `git push` (this fleet: **`iad1.relaygateway.net`**). Hostname, not a git branch. Gateways parse it; the receive-pack fence is not implemented yet. |
| `signing_keys` | Append-only identity list (`active` / `disabled`) |
| `signing_keys_source` | `repo` \| `catalog` \| `hybrid` |
| `domains` | Optional globally unique parked apex/subdomain hosts served by this repo |
| `clientHook` | Tip-owned client GET hook path (e.g. `hooks/client/get.jsx`). Required association for hosting repos unless `defaultClientHook` / fleet default applies |
| `defaultClientHook` | Catalog-assigned client GET hook when the tip has no `clientHook` (stub may later grow auto-index) |
| `ssrCache` | Optional pre-rendered HTML fragment cache for parked Hosts (see below) |
| `branch_features` | Optional `comments` / `dev` branches |
| `limits` | Per-repo overrides; inherit from `policy-profiles.json` when empty. For thin catalogs: `thin_ipfs_assets`, optional `fleet_pin_ipfs` (**`false`** = catalog-only, no fleet Kubo pin / movie disk cache — Flashback `fan-edits`), `max_ipfs_bytes` |

Fleet `catalog/index.json` may set **`defaultClientHook`** (same path shape). Load-time validation fails if any **hosting** repo (`domains[]` non-empty) resolves neither tip `clientHook`, per-repo `defaultClientHook`, nor the fleet default.

## Client hook obligation (hosting repos)

**Every hosting Relay repo must declare how the browser views its data:** an in-repo client hook (`clientHook`) **or** a catalog-assigned default (`defaultClientHook` / fleet `defaultClientHook`). Tip `.relay.yaml` `client.hooks.get.path` still wins at serve time when present; the catalog association is the policy SoT and must not be omitted for parked Hosts.

| Field | Who owns the hook file | Typical use |
|-------|------------------------|-------------|
| `clientHook` | This content tip | Park sites with a real UI (`hooks/client/get.jsx`) |
| `defaultClientHook` | Tip copy of the stub / future shared default | Hosting domain without a custom shell yet |
| `catalog/index.json` → `defaultClientHook` | Fleet fallback | Soft default so new hosting rows inherit until overridden |

**Non-SSR HTML:** when `ssrCache` is off (or a cache body misses), document navigations still get the **standard embed shell** (`relay-embed-config` + `embed.js`) pointed at the resolved client hook. SSR cache is optional acceleration, not the only way to get a shell. Do **not** run live full-app SSR on Nanodes — shell + client hook is the answer.

Stub for copy-into-tip: [`relay-template/hooks/client/default-get.jsx`](../../relay-template/hooks/client/default-get.jsx).

## Pre-rendered SSR cache (`ssrCache`)

When `ssrCache.enabled` is true, browsers on this repo’s `domains[]` get **composed static HTML** from a mesh-synced bare (default `relay-ssr-cache`): one shared shell + per-path body fragments. Cache miss → **standard CSR shell** (same embed bootstrap as non-SSR). **No live request-path SSR** (`ssrClientHookFromGetContext` is refused for these repos). Bake runs only on the **publisher / fat** node.

```json
"ssrCache": {
  "enabled": true,
  "repo": "relay-ssr-cache",
  "prefix": "sites/paradigmthreat.net/",
  "shellPath": "sites/paradigmthreat.net/_shell.html",
  "hookAssetsRepo": "relay-web-assets",
  "hookAssetsBranch": "release",
  "requirePreReceiveQa": true
}
```

| Field | Purpose |
|-------|---------|
| `enabled` | Opt into fragment compose on the serve path |
| `repo` | Cache bare slug (default `relay-ssr-cache`) |
| `prefix` | Site root inside the cache tip |
| `shellPath` | Shared `_shell.html` (must contain `<!--RELAY_SSR_BODY-->` or a `</main>`/`</body>` insert point) |
| `hookAssetsRepo` / `hookAssetsBranch` | Tip used for full re-bake invalidation when embed/WASM changes |
| `requirePreReceiveQa` | Publisher `pre-receive` runs CleverScript SSR + HTML QA on changed pages (default **true** when omitted). Set `false` to skip the push gate. |

Layout under the cache tip: `_shell.html`, `_meta.json` (`contentTip`, `hookAssetsTip`), `*.body.html` paths. See [`RELAY_POLICY_AND_PERFORMANCE.md`](./RELAY_POLICY_AND_PERFORMANCE.md), `scripts/relay-ssr-bake.py`, and `relay-ssr-bake` (CleverScript). Validation: enabled requires non-empty `prefix` + `shellPath` (no `..`).

Optional tip file **`.relay/ssr-qa.json`** adds site-specific require/forbid markers (examples: [`examples/ssr-qa.markers.json`](./examples/ssr-qa.markers.json), [`examples/ssr-qa.paradigmthreat.json`](./examples/ssr-qa.paradigmthreat.json)). Hard forbids always apply in Rust (`ssr_html_quality`).

## IRC channels (`irc-channels.json`)

Master inventory for Ergo product/ops channels (`relay.irc-channels.v1`). Pointed from `catalog/index.json` → `irc_channels`.

| Concern | Location |
|---------|----------|
| Channel exists on rebuild | `catalog/irc-channels.json` |
| Per-repo default room | `catalog/repos/<repo>.json` → `chat.default_channel` |
| Safe tip theme pack | `chat.theme_path` (e.g. `hooks/client/chat-theme/`) |
| Lobby playlist | `chat.lobby_playlist_path` |

Chat install runs `ops/chat/scripts/ensure-irc-channels.sh` and seeds chat-edge from this file. **Do not** mint product channels only on the server. Chat logs remain ephemeral on disk.

## Parked domains

`domains[]` assigns exact HTTP Host names to a repository. Values are normalized to lowercase without a trailing dot, validated as DNS hostnames, and must be globally unique across the catalog.

```json
{
  "repo": "clevertree-site",
  "domains": [
    "clevertree.net",
    "www.clevertree.net",
    "relay.clevertree.net"
  ]
}
```

The gateway materializes the signed assignment for HTTP routing and Caddy's on-demand TLS allowlist. DNS is a separate layer: the Relay network strategy uses **`fleet.relaygateway.net`** as the stable ingress target, apex ALIAS/flattening, and subdomain CNAME records. See **[`RELAY_NETWORK_DNS.md`](./RELAY_NETWORK_DNS.md)**.

## Publish branch

| Tier | Default | Notes |
|------|---------|--------|
| `primary` | `main` | Per-repo override OK |
| `hosted` | `main` | Same default. Do not force a second branch mesh-wide |

Existing hosted records may still set `publish_branch` to another name (for example `relay-web-assets` embed HTTP). New records default to `main`.

Example record (field is inert until the publish-once fence lands):

```json
{
  "schema": "relay.catalog-repo.v1",
  "repo": "paradigm-threat-files",
  "tier": "primary",
  "publish_branch": "main",
  "publish_origin": "iad1.relaygateway.net"
}
```

See also [`examples/catalog-repo.v1.json`](./examples/catalog-repo.v1.json).

Authors push:

```text
git push git://iad1.relaygateway.net:9418/paradigm-threat-files.git main
```

## Identity keys

| Concern | Location |
|---------|----------|
| Who may sign pushes / registry | `catalog/repos/<repo>.json` → `signing_keys` |
| Branch flags (`requireSigned`, `allowUnsigned`) | User repo `.relay.yaml` |
| Hosted enforcement | Gateway ignores repo `allowedKeys` when `signing_keys_source: catalog` |

## Signing key retirement

- **Default:** disable (`status: disabled`, `disabled_at`), do not delete `public_key` lines.
- **Verify:** all keys (`active` + `disabled`) for `git verify-commit`.
- **Authorize:** `active` only for new work; disabled keys allowed only if committer date `< disabled_at`.
- **History reset:** after bare rewrite + new `relay_root_commit`, catalog PR may remove keys — CI warns; requires human approval and signed commits (active keys only). See `scripts/check-catalog-pr-signing-keys.sh`.

## Migration (M0–M3)

1. **M0:** `catalog/repos/*.json` with `signing_keys_source: repo`; run `scripts/build-relay-catalog-json.sh`.
2. **M1:** Gateway dual-read; `relay-branch-policy-check` parity.
3. **M2:** User repos drop `allowedKeys` from `.relay.yaml`; catalog → `signing_keys_source: catalog`.
4. **M3:** Fleet uses catalog keys only.

## Per-commit enforcement

Set **`RELAY_MASTER_CATALOG_PATH`** to a `relay-master` checkout. Pre-receive loads `catalog/repos/<slug>.json` and merges catalog signing with `.relay.yaml` branch rules.

**Tip apply (rescue):** when **`relay-master`** `main` moves, gateway **catalog apply** reclones/removes bare mirrors under **`RELAY_REPO_PATH`** per **`relay-catalog.json`**. Operator runbook: **[`RELAY_CATALOG_RESCUE.md`](./RELAY_CATALOG_RESCUE.md)**.

## Branch features

### `comments`

- Branch `comments`; markdown under `.relay/comments/by-path/<encoded-path>/`.
- Client fetches / writes via `X-Relay-Branch: comments` (see `DEPLOY_WEB_AND_CI.md`; Host-based branch selection is the longer-term plan).
- Catalog default: signed commits + anonymous session keys (future registry).
- **Flashback MVP:** `allow_unsigned_commits: true` so browsers can `PUT` board posts without session keys; see fan-edits `docs/BOARD.md`.

Path encoding: [`catalog_policy::encode_comment_target_path`](../src/catalog_policy.rs).

### `dev`

- Unsigned commits allowed on `dev`.
- Merge into `publish_branch` must be signed by catalog **active** key (merge commit detection in `branch_policy.rs`).
- Public edit / PR UI is **not** required for Flashback board MVP.

## Hosted assets manifest

When media is referenced outside Git, require **`assets.manifest.json`** at repo root (hosted tier):

```json
{
  "schema": "relay.assets-manifest.v1",
  "assets": [
    {
      "id": "hero",
      "path": "content/hero.webp",
      "ipfs": "bafy…",
      "magnet": "magnet:?xt=urn:btih:…",
      "webrtc": { "room": "repo:example:hero" }
    }
  ]
}
```

No binaries in Git for `hosted`; pointers only.

## Operator scripts

| Script | Role |
|--------|------|
| `relay-master/scripts/build-relay-catalog-json.sh` | Regenerate `relay-catalog.json` from `catalog/repos/` |
| `relay-master/scripts/sync-relay-catalog.sh` | Bare mirrors + optional `authorized-repos.yaml` |
| `relay-master/scripts/check-catalog-pr-signing-keys.sh` | Warn on key removal without `relay_root_commit` change |
| `relay-master/scripts/check-catalog-client-hooks.sh` | Fail if a hosting repo lacks `clientHook` / `defaultClientHook` / fleet default |

## Related docs

- [RELAY_NETWORK_DNS.md](./RELAY_NETWORK_DNS.md) — parked-domain DNS, fleet ingress, Spot IP changes, TLS gate
- [PARADIGM_PARK_LOCAL.md](./PARADIGM_PARK_LOCAL.md) — local park SSR / tip hardening
- [RELAY_BRANCH_POLICY_OPERATOR.md](./RELAY_BRANCH_POLICY_OPERATOR.md) — `.relay.yaml` checks
- [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md) — anchors and trust root

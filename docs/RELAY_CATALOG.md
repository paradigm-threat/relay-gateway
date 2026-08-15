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
| `tier` | `primary` \| `hosted` |
| `forge_url` | Upstream clone URL |
| `relay_root_commit` | Trusted root (40 hex) |
| `publish_branch` | Branch served on Relay HTTPS / anchor checks (this fleet: **`main`**) |
| `publish_origin` | Optional FQDN of the one Relay that should accept client `git push` (this fleet: **`dfw1.relaygateway.net`**). Hostname, not a git branch. Gateways parse it; the receive-pack fence is not implemented yet. |
| `signing_keys` | Append-only identity list (`active` / `disabled`) |
| `signing_keys_source` | `repo` \| `catalog` \| `hybrid` |
| `domains` | Optional globally unique parked apex/subdomain hosts served by this repo |
| `branch_features` | Optional `comments` / `dev` branches |
| `limits` | Per-repo overrides; inherit from `policy-profiles.json` when empty |

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
  "publish_origin": "dfw1.relaygateway.net"
}
```

See also [`examples/catalog-repo.v1.json`](./examples/catalog-repo.v1.json).

Authors push:

```text
git push git://dfw1.relaygateway.net:9418/paradigm-threat-files.git main
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
- Client fetches via `X-Relay-Branch: comments` (see `DEPLOY_WEB_AND_CI.md`).
- Signed commits only; anonymous session keys (future registry) — planned.

Path encoding: [`catalog_policy::encode_comment_target_path`](../src/catalog_policy.rs).

### `dev`

- Unsigned commits allowed on `dev`.
- Merge into `publish_branch` must be signed by catalog **active** key (merge commit detection in `branch_policy.rs`).

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

## Related docs

- [RELAY_NETWORK_DNS.md](./RELAY_NETWORK_DNS.md) — parked-domain DNS, fleet ingress, Spot IP changes, TLS gate
- [PARADIGM_PARK_LOCAL.md](./PARADIGM_PARK_LOCAL.md) — local park SSR / tip hardening
- [RELAY_BRANCH_POLICY_OPERATOR.md](./RELAY_BRANCH_POLICY_OPERATOR.md) — `.relay.yaml` checks
- [RELAY_TRUST_AND_BOOTSTRAP.md](./RELAY_TRUST_AND_BOOTSTRAP.md) — anchors and trust root

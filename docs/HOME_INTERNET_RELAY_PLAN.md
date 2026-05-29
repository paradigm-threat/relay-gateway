# Home Internet Relay Plan

Status: draft plan, based on the May 2026 transcript.

![Home Internet Relay Plan](./HOME_INTERNET_RELAY_PLAN.png)

## Goal

Make Relay easy enough that a user can publish a website from a home PC, Mac, NAS, or later a plug-in appliance by downloading one Relay binary, choosing a Git repo, keeping a private key, and leaving the device online.

The browser should load the website from normal Relay servers over normal HTTPS. Relay servers host a thin, strictly limited repo that contains the page framework, client hooks, manifests, and fallback content. The home machine can optionally provide live or larger content when it is online, but the site shell must still load when the home machine is offline.

This extends the existing role of `relay-master`: today it is the catalog of allowed repos and trusted roots; this plan adds a controlled custom-site hosting lane plus optional home-live endpoint metadata.

## Product Shape

Advertise the feature as:

> Publish a website from your home computer. Relay hosts the lightweight site shell, and your PC, Mac, or NAS can add live content when it is online.

The user-facing setup target is:

1. Download and run the Relay server binary.
2. Choose or create the repo to host.
3. Generate or import the private signing key.
4. Relay validates the repo against strict hosting limits.
5. Relay mirrors the approved thin repo to Relay servers.
6. Optional: the home Relay server advertises live content sources for hooks to use when available.

If the private key is lost, the user can no longer update the website identity, publish repo updates, or advertise live home endpoints. Email/key recovery can be added later, but the initial model should treat the key as the site owner's root authority.

## Competitive Landscape

The target promise is unusual because it combines four constraints:

1. Free hosting.
2. No credit card.
3. Managed from, and optionally enhanced by, a home PC, Mac, NAS, or appliance.
4. Full HTML/code allowed, with no forced site editor.

Most competitors satisfy only two or three of those constraints.

### Closest Competitors

Cloudflare Tunnel is the strongest practical competitor. It can expose a local server over HTTPS, is free for many use cases, and can scale from development previews to production-like setups. Its weakness for Relay's positioning is that Cloudflare becomes the required middle layer, and stable production use generally assumes a Cloudflare account and domain setup.

Localhost tunnel services such as `localhost.run`, LocalExpose, Wormhole, nowpublic, and ngrok-style products are also close. They let a user run any local HTML/server stack and expose it online. Their weaknesses are usually unstable free URLs, bandwidth or request limits, account requirements, paid custom domains, or product positioning around temporary developer previews rather than durable personal websites.

Tailscale Funnel is relevant for technical users already using Tailscale. It is less natural as a public website hosting story for nontechnical users.

### Static Hosting Competitors

Static hosts usually satisfy free, no-credit-card, and full HTML/code, but they do not serve from the user's home machine:

- GitHub Pages.
- Netlify Drop.
- Vercel.
- Cloudflare Pages.
- Neocities.
- Codeberg Pages.
- HTMLPub / FreeKit-style paste or upload hosts.

These are likely the biggest SEO competitors for phrases like "free HTML hosting no credit card." They are easier than Relay for ordinary static hosting, so Relay should not compete only on "free static hosting." Relay's differentiator is Git-native publishing with signed identity, a constrained free hosting lane, and optional home-live extensions controlled by the repo's hooks.

### DIY Home Hosting Competitors

Traditional home hosting can be assembled from:

- DuckDNS, No-IP, or another dynamic DNS service.
- Router port forwarding.
- Caddy, nginx, Apache, or a NAS web server.
- YunoHost, CasaOS, Umbrel, or other self-hosting dashboards.
- NAS vendor packages.

These are conceptually similar, but they are not a one-step user experience. The user must understand DNS, ports, TLS, firewalls, process supervision, router configuration, updates, and backups.

### Positioning Gap

The refined Relay claim should be precise:

> Free Git-based website hosting with full HTML control. Relay hosts the lightweight site shell, while your home PC or NAS can power live content when it is online.

The technical caveat is important: normal browser users should not be required to connect directly to a home server. Relay servers must host enough of the repo for the page framework and fallback experience to load reliably over normal HTTPS.

## Important Browser Constraint

The transcript asks for the Rust binary for relay-runtime to fetch data through a non-SSL port, bypass browser security, and provide its own validation layer.

That is possible for a native Relay runtime, but not for a normal browser static client:

- Browser WASM cannot open arbitrary TCP sockets.
- Browser `fetch` cannot ignore mixed-content rules from an HTTPS Vercel page to a plain `http://ip:port` endpoint.
- Browser `fetch` cannot replace the browser's TLS certificate validation with a custom private/public key handshake.
- A static web app cannot speak `git://` directly from the browser.

Therefore there are three product tracks:

1. Browser hosting track: Relay servers host the thin repo and serve the site shell over normal HTTPS.
2. Hook extension track: repo hooks may optionally use browser-safe services such as WebRTC or IPFS gateways to load live or large content.
3. Native Relay runtime track: a Rust desktop/mobile/runtime client can connect to nonstandard ports and enforce Relay's own pinned-key transport.

This plan makes the browser hosting track the practical MVP. Pure browser-to-home-server fetching is no longer the main plan.

## Recommended Architecture

### Control Plane

`relay-master` remains the canonical catalog. **One tree** — no separate `custom-sites-catalog.json`:

```
relay-master/catalog/
  policy-profiles.json       # tier defaults: primary (lax) vs hosted (strict)
  repos/<repo-name>.json     # one record per registered repo
```

| `tier` | Use |
|--------|-----|
| **`primary`** | Mesh / clevertree repos — binaries OK, `publish_branch` usually `main` |
| **`hosted`** | Home thin hosting — text-only Git, default `publish_branch` **`release`**, `assets.manifest.json` for external media |

Optional **`home-relay-registry`** branch: `registry/repos/<repo>/<server-id>.json` for live home endpoints (WebRTC/IPFS hooks). Not mixed with catalog repo policy.

Normative catalog spec: **[`RELAY_CATALOG.md`](RELAY_CATALOG.md)** (signing keys, migration M0–M3, `comments`/`dev` branches, key retirement).

### Hosted repo record (example)

```json
{
  "schema": "relay.catalog-repo.v1",
  "repo": "example-user-site",
  "tier": "hosted",
  "forge_url": "https://gitlab.com/user/example-user-site.git",
  "relay_root_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "publish_branch": "release",
  "signing_keys_source": "catalog",
  "signing_keys": [{ "public_key": "ssh-ed25519 AAAA...", "status": "active" }],
  "limits": { "max_repo_bytes": 10485760, "allow_binary_files": false }
}
```

Hosted repos are intentionally thin: HTML, markdown, hooks, small SVGs, **`assets.manifest.json`** pointers (IPFS/magnet/WebRTC), and fallback content — not arbitrary binaries or large media in Git.

### Live Endpoint Record

Each home server may advertise live services that a GetHook can choose to use. These records do not determine whether the page shell loads; they only enable enhanced content when the home machine is online.

```json
{
  "schema": "relay.home-endpoint.v1",
  "repo": "example-site",
  "branch": "main",
  "server_id": "home-nobiserv",
  "owner_key_id": "ssh-ed25519 SHA256:...",
  "server_public_key": "ssh-ed25519 AAAA...",
  "observed_public_ip": "203.0.113.10",
  "fqdn": "optional.example.com",
  "services": {
    "http": {
      "protocol": "https",
      "port": 443,
      "path_prefix": "/"
    },
    "git": {
      "protocol": "git",
      "port": 9418,
      "path": "example-site.git"
    },
    "webrtc": {
      "protocol": "webrtc",
      "signaling_room": "repo:example-site:home-nobiserv"
    },
    "ipfs": {
      "protocol": "ipfs",
      "cid": "bafy..."
    }
  },
  "repo_heads": {
    "refs/heads/main": "0123456789abcdef0123456789abcdef01234567"
  },
  "relay_root_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "issued_at": "2026-05-19T14:00:00Z",
  "expires_at": "2026-05-19T14:15:00Z",
  "nonce": "base64-random",
  "signature": "base64-signature-over-canonical-json"
}
```

The registry should prefer canonical JSON signing or another deterministic envelope so both the master ingest process and hooks can verify identical bytes. A stale or missing live endpoint must never prevent the hosted site shell from loading.

### Registry Publishing

The site owner publishes the thin repo by signed Git updates. Relay accepts or mirrors only commits that pass strict policy.

The home Relay server separately and optionally:

1. Detects its public IP.
2. Confirms which optional live services are reachable.
3. Reads the current repo heads.
4. Signs an endpoint record with its private key.
5. Commits the record to the registry branch.
6. Pushes to the master Relay repo.

The master side:

1. Accepts only signed commits from allowed endpoint keys.
2. Validates the record schema.
3. Validates the repo is present in `relay-catalog.json` or the approved custom-site catalog.
4. Validates the advertised `relay_root_commit` and `repo_heads` match the hosted repo policy.
5. Enforces custom-site limits before mirroring content to public Relay servers.
6. Optionally probes live endpoints before publishing them as available.
7. Publishes the effective registry to Vercel Edge Config, Vercel KV, Redis, Postgres, or a signed static JSON artifact.

Best first implementation: keep the Git registry as the audit log, then have a small ingest job publish a compact `repos/<repo>.json` for the static client.

### Browser Client Loading

The browser path is intentionally ordinary:

1. User opens the site from a Relay HTTPS endpoint.
2. Relay serves the approved thin repo from its own mirror.
3. The page framework, fallback content, hooks, manifests, and small assets load from Relay servers.
4. The GetHook reads the live endpoint registry and decides whether to enhance the page.
5. If the home machine is online, the hook may connect through an approved browser-safe mechanism such as WebRTC signaling or IPFS.
6. If the home machine is offline or unavailable, the hook displays the hosted fallback.

The browser should not need to connect directly to a home IP or non-SSL port for the page to load.

## Security Model

### Key Roles

- Site owner key: proves authority to update the repo and endpoint records.
- Server key: proves this machine is the same server identity across IP changes.
- Master catalog signing key: proves catalog and registry policy updates.
- Repo commit keys: governed by `.relay.yaml` and existing Relay branch policy.

The private site/server key is the critical object. It stays on the home network and is never sent to Vercel or Relay masters.

### What Clients Validate

Clients should validate:

1. The repo is authorized by `relay-master`.
2. The repo's `relayRootCommit` is an ancestor of the served head.
3. The custom-site record allows the served head.
4. The repo satisfies file count, file size, total size, extension, and binary restrictions.
5. Optional live endpoint records are signed by an allowed key for that repo.
6. Optional live endpoint records have not expired.
7. Optional live content matches the advertised manifest, CID, or signed metadata before it is trusted.

For browser clients, the hosted shell and fallback content come from Relay HTTPS. Content validation adds defense in depth and prevents a home endpoint from silently replacing the hosted repo framework.

If a GetHook uses WebRTC, the page may establish an encrypted peer connection, but Relay still must bind the session to the advertised server key. The home server should sign a challenge that includes the repo name, server id, browser nonce, session expiry, advertised content manifest, and WebRTC DTLS fingerprint. The hook verifies that signature against the registry before displaying live content.

For native clients, use a pinned-key transport:

- Either TLS via `rustls` with a self-signed cert pinned to the advertised server key.
- Or a Noise-style protocol where the server key signs the handshake and all content is authenticated.

Do not call this "custom SSL" in implementation docs unless we are actually using TLS. Prefer "Relay pinned-key transport" or "Relay native secure transport."

## Network Modes

### Mode A: Relay-Hosted Thin Repo

This is the main browser product.

Relay servers host the approved custom repo directly. The repo must stay small and text-oriented so the free hosting lane remains cheap, reviewable, and safe. The site framework, hooks, fallback HTML/markdown, manifests, and small assets are always available through Relay HTTPS.

Pros:

- Works in every browser.
- Keeps SEO and initial page load on ordinary HTTP/HTTPS infrastructure.
- Avoids fragile browser-to-home networking.
- Lets the site load even when the home PC or NAS is offline.
- Keeps free hosting costs bounded by strict repo limits.

Cons:

- Relay infrastructure carries the thin repo bandwidth.
- Requires strict anti-abuse policy for custom repos.
- Large media and binary-heavy sites must use live home content, IPFS, paid hosting, or external storage.

### Mode B: GetHook Live Home Content

The hosted GetHook may attempt to load live or larger content from the user's home machine when it is online. WebRTC is allowed here as an optional feature for the hook, not as the transport for the whole repo. The hook owns the UX: loading state, live mode, stale mode, and offline fallback.

Pros:

- Keeps the public site available while still making a home machine useful.
- Fits IRC-like presence and "owner is online" experiences.
- Lets the home server provide dynamic content, previews, chat, large local media indexes, or private optional streams.

Cons:

- WebRTC gives a pipe, not a file protocol; Relay must implement fetch semantics, streaming, retries, range reads, caching, and error handling.
- Some networks require TURN, and TURN relays bandwidth through a server.
- Live content is less reliable than hosted fallback content.
- The hook must treat home content as optional and failure-prone.

### Mode C: GetHook IPFS Content

The GetHook may also fetch content by IPFS CID. This can be useful for larger static files, immutable bundles, and content-addressed verification.

In a standard browser, IPFS usually means one of:

- Fetching through an HTTPS IPFS gateway.
- Using a browser-supported libp2p transport if available.
- Loading content already cached or pinned by an extension/native helper.

IPFS is allowed as a hook feature, not as a replacement for hosting the repo shell. The hosted repo should contain the CID manifest, verification logic, and fallback.

### Mode D: Native Relay Runtime

Native clients can still use nonstandard ports, pinned TLS, `relay+tcp`, or other direct transports. This is useful for power users and appliances, but it is not required for the standard browser product.

## Git Head Validation

For known repos, gateway loads **`catalog/repos/<name>.json`** (or legacy **`relay-catalog.json`** until cutover):

- `forge_url` / `url`
- `relay_root_commit` / `relayRootCommit`
- `publish_branch` / `branch`

Hosted registration: signed PR adding `catalog/repos/<repo>.json` with `tier: hosted` before bare mirror and live registry entries are trusted.

Endpoint-level validation (future): advertised heads vs catalog `approved_head`, signed manifests, content object checks.

## Registry Projection Options

Recommended order:

1. Vercel Edge Config for live endpoint lists. Fast reads, simple client lookup.
2. Vercel KV or Postgres if we need richer querying, health history, or owner accounts.
3. Signed static JSON generated from the registry branch if we want minimum moving parts.

The Git registry should remain the source of truth and audit log. Vercel or database storage is a read-optimized projection for clients and hooks.

## Thin Repo Hosting Rules (`hosted` tier)

Defaults live in **`relay-master/catalog/policy-profiles.json`**. Per-repo overrides in **`catalog/repos/<name>.json` → `limits`**.

- No arbitrary binaries in Git; media via **`assets.manifest.json`** (`ipfs`, `magnet`, `webrtc` entries — see [`RELAY_CATALOG.md`](RELAY_CATALOG.md)).
- Caps: ~10 MB repo, 256 KB/file, 512 files; extension allowlist in profile.
- `.relay.yaml` branch rules in user repo; **signing keys in catalog** for hosted.
- Client GetHook + offline fallback required before public mirror.

SVG: serve with safe headers or sanitize when inlined.

## GetHook Live Content

The GetHook is responsible for optional live content. It must assume the home server may be offline and render a useful fallback from the hosted repo.

Allowed hook live sources:

- WebRTC session to the user's home Relay server.
- IPFS content by CID, usually through an HTTPS gateway in standard browsers.
- Normal HTTPS APIs or assets explicitly declared in the repo manifest.
- Native Relay transport when running outside a standard browser.

Disallowed as a requirement for first paint:

- Browser direct fetch to `http://home-ip:port`.
- Browser direct `git://`.
- Browser direct raw TCP.
- Any live source whose absence makes the page framework fail to load.

The hook UX should distinguish:

- `hosted`: showing fallback or cached content from Relay-hosted repo files.
- `connecting`: trying to reach live home/IPFS content.
- `live`: connected to home/IPFS content and displaying fresh data.
- `offline`: home source unavailable; fallback remains usable.
- `stale`: cached live content exists but could not be refreshed.

If the hook uses WebRTC, it may create DataChannels for live content, presence, or chat. That protocol is scoped to the hook feature. It is not the site-wide fetch implementation and it does not replace Relay-hosted repo files.

## User Hosting Self-Test

The product should include a built-in "Can I host from here?" test before asking users to publish a site. The test should now focus on two questions:

1. Can this repo be hosted safely on Relay's thin free lane?
2. Which optional live-content modes can the user's home machine support?

Self-test steps:

1. Scan the repo for file count, total bytes, max file size, file extensions, binary files, and disallowed paths.
2. Verify `.relay.yaml`, owner key, signed commits, trusted root, current branch head, and hook availability.
3. Verify the repo has a usable fallback when live content is unavailable.
4. Check outbound HTTPS to registry/signaling from the home Relay binary.
5. Check optional WebRTC support for hook-level live content.
6. Run a STUN test and classify whether WebRTC is likely direct or TURN-only.
7. Test optional IPFS configuration: declared CIDs, gateway reachability, and content hash match.
8. If the user also wants direct HTTPS mode, test dynamic DNS, DNS-01/HTTP-01 ACME readiness, and selected port reachability.
9. Publish a short signed health record into the registry with the successful optional live modes and expiry.

The user-facing output should be plain:

- `Relay-hosted ready`: repo passes thin-hosting limits and can be served from Relay.
- `Relay-hosted with live WebRTC`: repo is hosted, and hooks can optionally connect to the home server.
- `Relay-hosted with IPFS`: repo is hosted, and hooks can optionally fetch declared IPFS content.
- `Relay-hosted fallback only`: repo is hosted, but no live source is currently reachable.
- `Not ready`: show the exact missing item, such as repo too large, binary files present, unsigned commits, missing fallback, or invalid owner key.

This self-test is also the best onboarding mechanic. It tells a user whether their site qualifies for free thin hosting and which optional home-live features will work.

## Implementation Phases

### Phase 0: Design Constraints

- Write this plan.
- Decide product terms: "thin hosted repo", "home-live content", "Relay endpoint registry", and "Relay native secure transport."
- Make Relay-hosted thin repos the browser default.
- Treat WebRTC and IPFS as optional GetHook features, not as the primary fetch path.

### Phase 1: Thin Hosted Repo MVP

- Add custom-site records and hosting limits.
- Add repo scanner for file count, total bytes, max file size, extensions, binaries, signed commits, trusted root, and fallback presence.
- Mirror approved thin repos to Relay servers.
- Serve the site shell, fallback content, hooks, and manifests over normal Relay HTTPS.
- Reject custom repos that exceed limits or include disallowed binary content.

### Phase 2: Dynamic Endpoint Registry And Self-Test

- Add registry schema and validation script/hook.
- Add a `home-relay-registry` branch policy for optional live endpoints.
- Add TTL expiry and health fields.
- Add master ingest to publish compact per-repo endpoint lists.
- Add home-hosting self-test output and signed health records.
- Report `Relay-hosted ready`, `Relay-hosted with live WebRTC`, `Relay-hosted with IPFS`, `Relay-hosted fallback only`, or `Not ready`.

### Phase 3: GetHook Live Content

- Generate/import server key.
- Configure repo name, branch, and private key location.
- Publish signed optional live endpoint records.
- Add hook APIs/helpers for reading live endpoint state.
- Add hook-level fallback/loading/live/stale UX conventions.
- Add IPFS CID manifest support for hooks.

### Phase 4: Hook-Level WebRTC

- Add signaling rooms keyed by repo/server id for optional live content.
- Add home Relay outbound signaling client.
- Add hook-scoped WebRTC helper APIs, not global `Host.fetch` replacement.
- Add DataChannel message patterns for live data, chat, presence, or optional larger content.
- Add presence and repo event messages to support IRC-like UX.
- Add direct/STUN vs TURN detection and user-visible labels.

### Phase 5: Git and Content Verification

- Add signed `/api/repo-heads`.
- Add signed content manifest support.
- Add client-side verification before rendering.
- Reject endpoints whose served head does not match the registry.
- Enforce repo limits during push, catalog ingest, and mirror sync.
- Bind optional WebRTC session signatures to repo, server key, DTLS fingerprint, nonce, expiry, and advertised live manifest.

### Phase 6: Nonstandard Port Native Transport

- Add `relay+tcp` or pinned `rustls` transport to the Rust runtime.
- Add server handshake proving possession of the advertised key.
- Authenticate all content chunks.
- Support Git object transfer or Relay content APIs over this transport.
- Keep this outside the browser-only path unless browser capabilities change.

### Phase 7: One-Step Installer

- Package PC/Mac binaries.
- Add repo qualification self-test.
- Add optional live endpoint self-test.
- Add simple "choose repo" wizard.
- Add key backup warnings.
- Add NAS/appliance profiles.

## One-Step Setup Feasibility

PC/Mac: high feasibility.

A one-step setup can realistically generate a key, choose a repo, run the thin-hosting self-test, publish the approved repo to Relay servers, and start an optional home-live service. Router configuration is no longer required for the page shell to work; it only matters for optional direct HTTPS/native modes. WebRTC/IPFS live features should be treated as enhancements with hosted fallbacks.

NAS: moderate feasibility.

The first NAS target, NobiServ, is a 32-bit ARMv7 QNAP with 1 GB RAM, no package manager, no compiler, no Docker, and no Git installed. It can run a static ARMv7 binary if we build one elsewhere. Under the refined plan, NobiServ does not need to serve the full website to browsers; it only needs to publish signed repo updates and optionally provide home-live content. If we require local Git operations on the NAS, we either need to ship Git alongside Relay, install Git through Entware if available, or keep Git operations on a companion PC.

Plug-in Wi-Fi/USB appliance: high long-term feasibility if we control the OS image.

Use an ARM64 Linux board, ship Relay, Git, Caddy, and the setup UI preinstalled, and make the private key export/backup explicit.

## NobiServ Test Host Findings

Observed from the local network:

- Current workstation: `10.0.0.87/24`
- LAN: `10.0.0.0/24`
- Gateway/router: `10.0.0.1`
- NobiServ: `10.0.0.109`, hostname `NobiServ`
- NobiServ OS: Linux 4.2.8, `armv7l`
- NobiServ RAM: 1 GB
- NobiServ storage: `/share/CACHEDEV1_DATA`, about 10.6 TB with about 3.2 TB available
- NobiServ tools present: `curl`, `wget`, `netstat`
- NobiServ tools missing from PATH: `docker`, `rustc`, `cargo`, `git`
- Candidate open ports from this workstation: `22`, `443`, `8080`
- Candidate Relay git port `9418` is not open yet

NobiServ is useful as a constrained home-server test, but not as a Docker host. Treat it as a "static binary plus minimal init script" target.

## NobiServ Build and Deploy Next Steps

1. Decide whether NobiServ is a publisher, a live-content provider, or both.
2. For publishing, prefer having the user's PC perform Git pushes to Relay servers first; avoid requiring full Git tooling on the NAS for the MVP.
3. For live content, cross-compile a small ARMv7 home-live Relay agent rather than the full gateway if possible.
4. If full Relay/Git support is required, provide a Git binary and `git daemon` for ARMv7, likely by cross-building or installing Entware.
5. Cross-compile required Relay binaries for ARMv7 hard-float Linux.
6. Prefer a mostly static build. Validate `git2` with vendored OpenSSL on `armv7-unknown-linux-musleabihf` or `armv7-unknown-linux-gnueabihf`.
7. Copy binaries to a persistent location such as `/share/CACHEDEV1_DATA/relay/bin`.
8. Store live content and cache data under `/share/CACHEDEV1_DATA/relay/data`.
9. Store secrets under a root-only or owner-only path, not in the repo checkout.
10. Create a BusyBox-compatible start script.
11. Add a QNAP autorun entry only after manual startup is proven stable.
12. Confirm from the workstation that the hosted site shell works from Relay servers and that optional NobiServ live content reports online/offline correctly.

NobiServ should not run Docker for this plan unless QNAP Container Station is confirmed separately and supports this ARMv7 model. The current evidence says Docker is not available.

## Rackspace Docker Next Steps

Rackspace is the easier cloud deployment track because `relay-gateway` already has Docker and Rackspace Spot/Kubernetes artifacts.

1. Build the existing Docker image from `relay-gateway/Dockerfile`.
2. Push it to GHCR or another registry.
3. Provision Rackspace Spot Kubernetes from `relay-gateway/terraform/rackspace-spot`.
4. Save the generated kubeconfig.
5. Apply the existing DaemonSet or replace it with a more production-like Deployment plus Service/Ingress.
6. Provide `RELAY_SERVER_ID`, `RELAY_REPO_PATH`, `RELAY_MASTER_CATALOG_GIT_URL`, `RELAY_MASTER_TRUST_ROOT`, and peer list env vars.
7. Mount persistent storage for `/srv/relay/data`.
8. Expose HTTP and Git ports. For production, terminate TLS through an ingress or Caddy rather than relying on host networking.
9. Verify `POST /api/config`, repo serving, and git daemon reachability.
10. Enable the custom-site thin hosting policy only after catalog, trust root, and repo-limit enforcement are correct.

The Rackspace path is best for validating Relay-hosted thin repos and the registry projection quickly. NobiServ is best for validating the constrained home-live story.

## Best Route Forward

Build in this order:

1. Relay-hosted thin repo MVP: strict custom-site policy, signed commits, small text-only repos, and normal HTTPS serving.
2. User self-test: classify repo eligibility and optional live modes before publishing.
3. GetHook fallback contract: every custom site must render when home-live content is offline.
4. Optional live content: WebRTC/IPFS helpers that hooks can use without controlling first paint.
5. Registry hardening: signed records, TTLs, repo head validation, master ingest projection, and size-limit enforcement.
6. NobiServ constrained deployment: static ARMv7 publisher/live-content agent strategy.
7. Native secure transport: pinned-key Rust runtime for nonstandard non-HTTPS ports.
8. Appliance/one-step product polish.

This avoids blocking the whole feature on WebRTC reliability and browser-impossible custom HTTP transport work. The standard browser product is normal Relay HTTPS hosting for thin repos. WebRTC and IPFS are hook-level enhancements for live or large content, and native Relay transport remains the best path for full custom networking outside the browser sandbox.

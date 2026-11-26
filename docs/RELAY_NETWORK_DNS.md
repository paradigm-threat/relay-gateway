# Relay network DNS and parked domains

This is the canonical DNS strategy for a Relay network whose masters may be replaced or receive new public IP addresses.

## Goals

- Operators and catalogs refer to stable hostnames, not provider-assigned IP addresses.
- A parked site points at one stable Relay network name instead of copying the fleet's IPs into every customer zone.
- Rackspace Spot replacement changes one node record, then one fleet record set; it does not require editing every parked domain.
- Unknown HTTP `Host` values do not fall through to the default repository.

## DNS hierarchy

Use three layers:

1. **Node names (canonical Linode fleet):** `pub1.relaygateway.net` (publisher), `mesh1.relaygateway.net`, `mesh2.relaygateway.net`.
   Each name has the current A record for exactly one node (+ matching `*.pub1` / `*.mesh1` / `*.mesh2` wildcards). `relay-master-deploy.sh` / `point-dns` updates those after provisioning.
   Historical Rackspace Spot names (`iad1` / `ord1` / `hkg1`) had their Spot **A** records removed 2026-11-26 — **do not** recreate them for Linode.
2. **Network ingress:** `fleet.relaygateway.net`.
   A/AAAA = **publisher only** (`fleetDns: true` / `RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net`). Mesh nodes peer-sync git but must not appear in the fleet A set. Optional aliases: `irc.relaygateway.net` / `chat.relaygateway.net` → pub1.
3. **Parked domains:** customer or project names point to the network ingress:
   - zone apex: `ALIAS fleet.relaygateway.net` when the authoritative DNS provider supports apex ALIAS/flattening;
   - subdomain such as `www`: `CNAME fleet.relaygateway.net`;
   - single-node canary such as `relay.clevertree.net`: `CNAME pub1.relaygateway.net`.

Vercel DNS supports `ALIAS` records. Do not assume that multiple ALIAS records at one owner provide portable multi-target behavior. Relay therefore treats `fleet.relaygateway.net` as an ordinary, explicitly reconciled A/AAAA set. Observer auto-reconcile must also filter to `fleetDns` / non-mesh nodes (not every healthy mesh IP).

## Record model

Example network zone:

```text
pub1.relaygateway.net   A      172.234.148.70
mesh1.relaygateway.net  A      172.237.134.22
mesh2.relaygateway.net  A      172.236.146.220
*.pub1.relaygateway.net A      172.234.148.70
fleet.relaygateway.net  A      172.234.148.70   # publisher only
irc.relaygateway.net    A      172.234.148.70   # optional
chat.relaygateway.net   A      172.234.148.70   # optional
```

Example parked zone:

```text
clevertree.net      ALIAS  fleet.relaygateway.net
www.clevertree.net  CNAME  fleet.relaygateway.net
```

During a one-node canary or before multi-node TLS is ready:

```text
relay.clevertree.net  CNAME  pub1.relaygateway.net
```

Keep TTL at 60 seconds during rollout and recovery. Raise it only after replacement, certificate renewal, and rollback paths are proven.

## Catalog routing

DNS only sends traffic to Relay. The signed `relay-master` catalog decides which repository answers a host:

```json
{
  "schema": "relay.catalog-repo.v1",
  "repo": "clevertree-site",
  "domains": [
    "clevertree.net",
    "www.clevertree.net",
    "relay.clevertree.net"
  ]
}
```

Domains are normalized, globally unique across catalog records, and materialized by `relay-gateway` into:

- `/opt/relay/state/parked-domains.json` for HTTP Host-to-repository routing;
- `/opt/relay/state/parked-domains.txt` for Caddy on-demand TLS authorization.

The catalog is authoritative. Hand-written state files are permitted only as a temporary canary or recovery measure and must be removed after the signed catalog tip is active.

## Linode cutover (fat publisher + Nanode mesh)

Target after Spot retirement:

| Layer | Target |
|--------|--------|
| Node A | `pub1…`, `mesh1…` Linode FQDNs via `point-dns` / install Vercel DNS |
| **`fleet.relaygateway.net` multi-A** | **Only fat publisher IP(s)** — never Nanodes |
| Parked apex ALIAS / www CNAME | Stay on `fleet…` (no per-domain A churn) |
| `publish_origin` / git push | Fat node FQDN only — **never** `fleet…` |

**`ops/fleet.json`:** set publisher nodes `"role": "publisher"` / `"fleetDns": true`; Nanodes `"role": "mesh"` / `"fleetDns": false`. `relay-ops fleet-dns` and `relay-master-deploy.sh` reconcile **only** fleetDns participants (`RELAY_FLEET_DNS_PEERS` = fat FQDNs).

Cutover checklist:

1. Provision fat Linode (~8 GB) + Nanode mesh; install; sync catalog + **`relay-ssr-cache`**.
2. `point-dns` each new node FQDN; seed park-tls on fat from current publisher.
3. Set `publish_origin` / catalog push fence to fat FQDN; enable publisher bake queue (`RELAY_NODE_ROLE=publisher`).
4. Nanodes: `RELAY_NODE_ROLE=mesh` (bake queue refuses); they only `git fetch` including `relay-ssr-cache`.
5. `fleet-dns` reconcile **fat IP only**; confirm `dig fleet.relaygateway.net` has no Spot/Nanode IPs.
6. Parked ALIAS/CNAME unchanged (still → `fleet`).
7. Drain Spot: remove from peer list + fleet A; decommission after mesh green.
8. Ops gate: `ops/fleet.json` `health.sslDomains` + SSL checks against fat.

**Breaks if:** Nanodes remain in fleet reconcile → round-robin HTML/TLS to 1 GiB boxes; or authors push git to `fleet…`.

## Spot replacement flow (legacy)

When a Rackspace Spot master is replaced:

1. Obtain the replacement VM's public IP (currently from the Spot UI / `public_ip_override` until the VM API is available).
2. Run `relay-master-deploy.sh --target spot-<node> --build`.
3. The deploy updates `<node>.relaygateway.net` and `*.<node>.relaygateway.net`.
4. The deploy reconciles **`fleet.relaygateway.net`** from **`RELAY_FLEET_DNS_PEERS`** (fat/publisher only — not the full mesh peer list).
5. Validate the node by its FQDN.
6. Verify that the old IP is absent from the fleet A/AAAA set (`dig +short fleet.relaygateway.net A`).
7. Verify parked-domain HTTP and HTTPS against the replacement node.

Manual fleet reconcile (without a full redeploy):

```bash
cd relay-gateway
export RELAY_FLEET_DNS_PEERS=pub1.relaygateway.net   # publisher only
./scripts/relay-fleet.sh fleet-dns --dry-run
./scripts/relay-fleet.sh fleet-dns
```

The node-name update and fleet-record reconcile scripts exist. Spot VM discovery is retired with Spot.
## Opt-in park TLS plugin

Parked multi-node HTTPS is **not** a global gateway default. A content repo opts in:

```yaml
server:
  tls:
    plugin: park
```

With the plugin enabled:

- `caddy-ask` only allowlists that repo's catalog `domains[]` for on-demand / park issuance;
- the gateway `@relay/tls/park` worker issues Let's Encrypt certs via HTTP-01;
- challenge tokens and cert/key PEMs fan out to peers over `POST /hooks/park-tls` (mesh secret);
- Caddy loads file TLS from `/opt/relay/state/park-tls/Caddyfile.parked`.

**Generation (do not hand-edit on nodes):** `relay-gateway` writes `Caddyfile.parked` via `park_tls::write_parked_caddyfile` (`src/park_tls/caddy.rs`) whenever the park worker runs, fan-out receives PEMs, or the TLS allowlist is refreshed. `/etc/caddy/Caddyfile` from `scripts/install-caddy-tls.sh` always `import`s that file **before** the catch-all on-demand TLS block so multi-A apex names (e.g. `flashbackrepository.org` on `fleet.relaygateway.net`) never stall on per-node ACME.

**Fleet deploy seed:** `scripts/relay-master-deploy.sh` (and reclaim paths that call it) tar-copies `state/park-tls/certs/` + `Caddyfile.parked` from `--peer` / `RELAY_PEER_CLONE_SOURCE` (default `pub1.relaygateway.net`) onto new or reclaimed nodes, then reloads Caddy. Set `RELAY_PARK_TLS_ISSUER=1` on the publisher so alphabetical peer election cannot leave issuance on a mesh node. Ops should list every park-plugin apex in [`ops/fleet.json`](../ops/fleet.json) `health.sslDomains` so `./scripts/relay-fleet.sh ssl` runs per-node SNI checks (see `scripts/relay-fleet-ssl-check.sh`).

Repos without the plugin still get Host→repo HTTP routing from `domains[]`, but do not join shared ACME.

## TLS gate for multi-node ingress

Do not add multiple addresses to `fleet.relaygateway.net` merely because HTTP routing works.

With multiple A records, an ACME HTTP-01 validation request may reach a different node from the node that started the order. Relay must first provide one of:

- shared HTTP-01 challenge state plus certificate/key fan-out; or
- centrally issued certificates distributed securely to every ingress node.

Renewal must use the same coordinated path. A certificate on only one node is not multi-node readiness.

A single-node CNAME/ALIAS to `pub1.relaygateway.net` is safe with the existing per-node Caddy on-demand TLS path.

## Parked-domain rollout

1. Add every apex and alias host to one catalog record's `domains[]`.
2. Validate catalog-wide uniqueness.
3. Publish the site's served branch.
4. Signed-push the catalog and wait for mesh propagation.
5. Deploy/restart the gateway and `caddy-ask` on every intended ingress node.
6. Confirm the generated parked-domain map and ask allowlist on every node.
7. Canary with a subdomain CNAME to one node.
8. Verify pre-rendered HTML (`X-Relay-Ssr-Cache: hit|miss`), client hydration, assets, contact links, and unknown-Host rejection.
9. Complete the multi-node TLS gate.
10. Populate/reconcile `fleet.relaygateway.net`.
11. Point the apex ALIAS and `www` CNAME at `fleet.relaygateway.net`.
12. Smoke each fleet IP independently with the parked `Host`, then test normal DNS.

**Thin IPFS catalogs** (Flashback Repository / `fan-edits`): movie pages are `/slug` with no tip blob; the gateway must treat `index.json` slugs as virtual shell paths (`resolve.rs`). Until that binary is on every ingress node, HTTPS may succeed while `/slug` returns **404** — deploy a gateway build that includes the slug logic, then re-run the per-IP curl smoke above.

## Verification

```bash
dig +short pub1.relaygateway.net A
dig +short mesh1.relaygateway.net A
dig +short mesh2.relaygateway.net A
dig +short fleet.relaygateway.net A
dig +short clevertree.net A
dig +short www.clevertree.net CNAME

# Operator laptop: fleet SSL report (nodes + parked Hosts).
cd relay-gateway
./scripts/relay-fleet.sh ssl
# Or: ./scripts/relay-fleet-ssl-check.sh

# Force a parked Host to one node before changing public DNS.
curl --resolve clevertree.net:443:<node-ip> https://clevertree.net/
curl -sI --resolve flashbackrepository.org:443:<node-ip> https://flashbackrepository.org/van-helsing-deanna-d

# Caddy ask: parked host allowed, random host denied.
curl -o /dev/null -w '%{http_code}\n' \
  'http://127.0.0.1:3999/allow?domain=clevertree.net'
curl -o /dev/null -w '%{http_code}\n' \
  'http://127.0.0.1:3999/allow?domain=unclaimed.example'
```

Expected ask results are `200` and `403`. Also verify the certificate SAN, issuer, expiry, and the same content on every fleet address.

**Deploy soft gate:** `./scripts/relay-fleet.sh update` / `deploy-all` runs the SSL check **before** and **after** the host loop. Failures print `FAIL`/`WARN` and continue by default; set **`RELAY_FLEET_SSL_GATE=1`** to abort. Content repos that need park Host HTTPS must set **`server.tls.plugin: park`** in `.relay.yaml` or ask stays `403` and browsers see `ERR_SSL_PROTOCOL_ERROR`.

## Rollback

1. Restore the parked domain's previous apex and `www` records.
2. Remove unhealthy addresses from `fleet.relaygateway.net`.
3. Leave catalog `domains[]` in place while DNS caches expire; removing it early can turn delayed requests into unknown-Host failures.
4. After the old TTL window, remove or change the catalog assignment with a signed catalog push.

## Implementation status

- Done: catalog `domains[]`, uniqueness checks, Host routing, unknown-Host rejection, generated Caddy allowlist, node canary path, and **`vercel-dns-fleet-reconcile.sh`** / **`relay-fleet.sh fleet-dns`** (wired into `relay-master-deploy.sh`).
- Pending: coordinated multi-node ACME/certificate distribution and renewal testing for multi-A `fleet` (today fleet A = pub1 only).


# Relay network DNS and parked domains

This is the canonical DNS strategy for a Relay network whose masters may be replaced or receive new public IP addresses.

## Goals

- Operators and catalogs refer to stable hostnames, not provider-assigned IP addresses.
- A parked site points at one stable Relay network name instead of copying the fleet's IPs into every customer zone.
- Rackspace Spot replacement changes one node record, then one fleet record set; it does not require editing every parked domain.
- Unknown HTTP `Host` values do not fall through to the default repository.

## DNS hierarchy

Use three layers:

1. **Node names:** `iad1.relaygateway.net`, `sjc1.relaygateway.net`, and `hkg1.relaygateway.net`.
   Each name has the current A record for exactly one master. `relay-master-deploy.sh` updates the node A record after provisioning or replacing that node.
2. **Network ingress:** `fleet.relaygateway.net`.
   Its A/AAAA set is the materialized set of healthy fleet-node addresses. A fleet DNS reconciler resolves the configured node names and replaces stale addresses. Node names remain the source; do not copy provider IPs into configuration or documentation.
3. **Parked domains:** customer or project names point to the network ingress:
   - zone apex: `ALIAS fleet.relaygateway.net` when the authoritative DNS provider supports apex ALIAS/flattening;
   - subdomain such as `www`: `CNAME fleet.relaygateway.net`;
   - single-node canary such as `relay.clevertree.net`: `CNAME iad1.relaygateway.net`.

Vercel DNS supports `ALIAS` records. Do not assume that multiple ALIAS records at one owner provide portable multi-target behavior. Relay therefore treats `fleet.relaygateway.net` as an ordinary, explicitly reconciled A/AAAA set.

## Record model

Example network zone:

```text
iad1.relaygateway.net   A      <current iad1 IPv4>
sjc1.relaygateway.net   A      <current sjc1 IPv4>
hkg1.relaygateway.net   A      <current hkg1 IPv4>
fleet.relaygateway.net  A      <resolved iad1 IPv4>
fleet.relaygateway.net  A      <resolved sjc1 IPv4>
fleet.relaygateway.net  A      <resolved hkg1 IPv4>
```

Example parked zone:

```text
clevertree.net      ALIAS  fleet.relaygateway.net
www.clevertree.net  CNAME  fleet.relaygateway.net
```

During a one-node canary or before multi-node TLS is ready:

```text
relay.clevertree.net  CNAME  iad1.relaygateway.net
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

## Spot replacement flow

When a Rackspace Spot master is replaced:

1. Obtain the replacement VM's public IP (currently from the Spot UI / `public_ip_override` until the VM API is available).
2. Run `relay-master-deploy.sh --target spot-<node> --build`.
3. The deploy updates `<node>.relaygateway.net` and `*.<node>.relaygateway.net`.
4. The deploy reconciles **`fleet.relaygateway.net`** from the configured peer FQDNs (`vercel-dns-fleet-reconcile.sh`).
5. Validate the node by its FQDN.
6. Verify that the old IP is absent from the fleet A/AAAA set (`dig +short fleet.relaygateway.net A`).
7. Verify parked-domain HTTP and HTTPS against the replacement node.

Manual fleet reconcile (without a full redeploy):

```bash
cd relay-gateway
./scripts/relay-fleet.sh fleet-dns --dry-run
./scripts/relay-fleet.sh fleet-dns
```

The node-name update and fleet-record reconcile scripts exist. Automatic Spot VM discovery (without UI/`public_ip_override`) is still pending.
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

Repos without the plugin still get Host→repo HTTP routing from `domains[]`, but do not join shared ACME.

## TLS gate for multi-node ingress

Do not add multiple addresses to `fleet.relaygateway.net` merely because HTTP routing works.

With multiple A records, an ACME HTTP-01 validation request may reach a different node from the node that started the order. Relay must first provide one of:

- shared HTTP-01 challenge state plus certificate/key fan-out; or
- centrally issued certificates distributed securely to every ingress node.

Renewal must use the same coordinated path. A certificate on only one node is not multi-node readiness.

A single-node CNAME/ALIAS to `iad1.relaygateway.net` is safe with the existing per-node Caddy on-demand TLS path.

## Parked-domain rollout

1. Add every apex and alias host to one catalog record's `domains[]`.
2. Validate catalog-wide uniqueness.
3. Publish the site's served branch.
4. Signed-push the catalog and wait for mesh propagation.
5. Deploy/restart the gateway and `caddy-ask` on every intended ingress node.
6. Confirm the generated parked-domain map and ask allowlist on every node.
7. Canary with a subdomain CNAME to one node.
8. Verify SSR, hydration, no-JS CSS, assets, contact links, and unknown-Host rejection.
9. Complete the multi-node TLS gate.
10. Populate/reconcile `fleet.relaygateway.net`.
11. Point the apex ALIAS and `www` CNAME at `fleet.relaygateway.net`.
12. Smoke each fleet IP independently with the parked `Host`, then test normal DNS.

## Verification

```bash
dig +short iad1.relaygateway.net A
dig +short sjc1.relaygateway.net A
dig +short fleet.relaygateway.net A
dig +short clevertree.net A
dig +short www.clevertree.net CNAME

# Operator laptop: fleet SSL report (nodes + parked Hosts).
cd relay-gateway
./scripts/relay-fleet.sh ssl
# Or: ./scripts/relay-fleet-ssl-check.sh

# Force a parked Host to one node before changing public DNS.
curl --resolve clevertree.net:443:<node-ip> https://clevertree.net/

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
- Pending: committed/released gateway changes, signed catalog publication, full-fleet deployment, coordinated multi-node ACME/certificate distribution, renewal testing, and automatic Spot VM IP discovery without UI override.


# Thin repo migration (paradigm-threat-files, paradigm-threat-timeline)

**Not in scope:** `paradigm-threat-site` (Next.js).

## Steps

1. **Inventory + pin** (long-running; run per repo):

   ```bash
   cd relay-gateway/scripts
   ./relay-ipfs-migrate-inventory /path/to/paradigm-threat-files --branch main
   ```

2. **Rewrite history** with `git-filter-repo` using callback from:

   ```bash
   python3 relay-ipfs-rewrite-filter.py --csv ipfs-migration.csv -o /tmp/blob-callback.py
   cd /path/to/repo && git filter-repo --force --blob-callback "$(cat /tmp/blob-callback.py)"
   ```

3. **Push** to new GitLab project; archive legacy repo.

4. **Catalog** — update `relay-master/catalog/repos/paradigm-threat-*.json`:
   - `forge_url` → new remote
   - `relay_root_commit` → first signed commit on thin `main`
   - `limits.allow_binary_files: false` (already set in catalog template)

5. **Fleet** — `relay-fleet.sh reclone-repo paradigm-threat-files` (and timeline).

6. **Deploy** gateway with IPFS resolver ([`RELAY_IPFS.md`](RELAY_IPFS.md)).

## Day-to-day

See [`RELAY_IPFS.md`](RELAY_IPFS.md) — `relay-ipfs-ingest`, pre-commit hook, `relay-ipfs-cache-gc`.

# CleverScript QUERY hooks

When `.relay.yaml` sets **`server.hooks.query.path`** to a file ending in **`.cs`**, the relay-gateway runs that source with **relay-runtime** (CleverScript) for each QUERY request instead of reading the embedded **redb** index only.

## Configuration

```yaml
server:
  hooks:
    query:
      path: hooks/server/query.cs
    # Optional: branch allowlist for QUERY (overrides index-only allowlist when set)
    # branches: [ "main" ]
```

If **`server.hooks.query.branches`** is omitted, QUERY branch policy falls back to **`server.hooks.index.branches`** (same as before).

## Script contract

- **Default export** must be a function. The first parameter receives a **context** object (JSON): `branch`, `collection`, `query` (from the QUERY body), `repo` (bare repo name).
- **Return value:** either **`{ rows: [ ... ] }`** or a **top-level array**. Each row is serialized to JSON for CSV/XML/JSON responses.
- **Git file reads:** use **`import { readFile } from '@relay/git'`** (and optionally **`readFileBase64`**) to read paths from the current branch in the bare repo. The module **`@relay/image`** is **not** available in QUERY hooks (unknown specifier). There is **no** global **`Relay`** (`Relay` is **`null`**).

**Migration:** replace **`Relay.git.readFile`** with the import above.

## Redb path

If **`server.hooks.query.path`** is unset or does **not** end with `.cs`, QUERY uses the **index hook → `index.db.json` → redb** path (after Rust sync of `data/**/meta.yaml` when `server.hooks.index` is configured).

## Related

- [ATLANTA3_QUERY_ROLLOUT.md](./ATLANTA3_QUERY_ROLLOUT.md) — staged deployment on one production node.

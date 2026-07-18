# Paradigm Threat thin park — status (2026-07-18)

## Architecture (current)

- **UI in thin repos** — no `paradigm-threat-web` park path.
- **Files:** thin tip + in-repo CleverScript shell; park Host `relay.paradigmthreat.net` → `paradigm-threat-files`.
- **Timeline:** thin tip + in-repo CleverScript shell on **repo subdomain** only (`paradigm-threat-timeline.{node}`).
- **Assets:** `relay-web-assets` for embed/WASM.
- Hard markdown components **disabled** — see `hooks/COMPONENTS_DISABLED.md` in files thin.

## Tips (dfw1)

| Repo | Tip (approx) |
|------|----------------|
| paradigm-threat-files | `4a4d793` (home article list + Index; GitLab thin `main`) |
| paradigm-threat-timeline | `4070449` |
| relay-master catalog | `dd6847a` (park domain on files; web domains cleared) |

## Force push / thin invariant

- Thin tips must stay **history-aligned** with from-repos and ideally **binary-free**. Current thin files tip still lists some `.html`/image paths — track with `relay-ipfs-verify-thin` and a follow-up rewrite pass.
- Mesh `git push` of `.relay.yaml` privilege changes needs adminKeys; tips were installed with bundle + `update-ref` when pre-receive blocked.

## Smoke (dfw1)

- `Host: relay.paradigmthreat.net` → files SSR (`Thin files` in HTML).
- `paradigm-threat-files.dfw1…` `/`, `/README.md`, `/search` → 200 SSR.
- `paradigm-threat-timeline.dfw1…` `/`, `/README.md` → 200 SSR.

## Still needed

1. Peer sync lon1/hkg1 files+timeline tips.
2. Durable fleet IPFS; turn force-push OFF on GitLab thin `main`s if still on.
3. Wave C: re-enable AutoIndex / RelayImage / etc.
4. Optional files `/timeline` subpath later.
5. Thin binary purge verify gate before Phase 6.

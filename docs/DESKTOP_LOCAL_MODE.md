# Desktop local mode

**Not the Android tester path.** Testers: [`../relay-runtime/docs/ANDROID_WEB_TEST.md`](../relay-runtime/docs/ANDROID_WEB_TEST.md). This file is only for the **desktop crate** (`relay-client-desktop --repo`).

Desktop **`--repo PATH`** embeds **`relay_server` in-process** (no loopback subprocess, no git mirror).

## Client modes

| Mode | Entry | Server |
|------|-------|--------|
| **Web** | Browser + WASM | Remote fleet gateway only |
| **Desktop local** | `relay-client-desktop --repo PATH` | In-process `relay_server` |
| **Desktop remote** | `--remote URL` | Remote HTTP (same as web) |

## GET resolution (`ServeContext::LocalWorktree`)

Per request, in [`git/resolve.rs`](../src/git/resolve.rs):

1. **Live working tree** when `X-Relay-Branch` matches the checkout’s current branch — includes uncommitted and **gitignored** files ([`git/worktree.rs`](../src/git/worktree.rs)).
2. **Git tree at branch HEAD** when the worktree miss or a different branch is requested.
3. **`server.hooks.get`** CleverScript when both miss (unchanged).

Server hooks do not implement worktree policy; hooks see accessible bytes or a miss.

## Fleet mode unchanged

`ServeContext::BareFleet` (default) skips the worktree layer — bare repos under `RELAY_REPO_PATH` only.

## Desktop env

Embedded sessions set:

- `RELAY_DESKTOP_MODE=1`
- `RELAY_PUT_REQUIRES_PRECOMMIT=1`

Host header: `{repo}.localhost`.

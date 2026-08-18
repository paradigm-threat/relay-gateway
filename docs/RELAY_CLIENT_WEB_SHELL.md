# Relay client web shell (WASM CSR + optional SSR hydrate)

The **relay-client-web** operator SPA (`relaygateway.net`) and **parked sites** (`relay.paradigmthreat.net`, repo vhosts) share one boot path: **`renderHook`** in `relay-runtime-web.js` (vendored into `relay-client-web` as `src/vendor/relay-runtime-web.js`).

## Host modes

| Mode | Where | HTML SSR | `renderHook` |
|------|-------|----------|--------------|
| **Parked / embed** | Gateway document `#root` | Yes — article + shell in HTML | `mode: auto`, hydrate when `data-relay-ssr=1` |
| **Relay client repo tab** | iframe `#relay-hook-mount` | No — WASM CSR only | `mode: csr`, `hydrate: false` |
| **Preset / Test tab** | iframe or mount | Optional | `mode: auto` |

Repo tabs use **`ThemedHookRenderer`** → `mountRelayHookIntoElement` with **`initialPath`** from the tab URL. They do **not** navigate an iframe to the repo origin (no duplicate HTML stack).

## Boot phases

`renderHook` publishes status on the mount element:

- Dataset: `data-relay-boot-phase`, `data-relay-boot-message`, …
- Event: `relay-boot-status` on the mount node
- Manifest (for CleverScript `@hooks`): `relayBootPhase`, `relayBootMessage`, `relayHostMode` (`ssr` \| `csr`), `relaySsrArticlePresent`

Phases: `wasm` → `manifest` → `modules` → `theme` → `render` → `live` → `error`.

API: `setBootStatus(container, { phase, message, detail?, progress? })`, `getBootStatus(container)`.

## Hook-owned loading (no React Suspense)

Sites wrap UI in **`@relay/ui/RelayBootShell`** (CleverScript). The overlay renders on the first hook pass (fetch-free brand + spinner) and hides when `isClientLive()` is true.

CSR `renderHook` also walks static local `from '…'` imports after the `hooks.json` prefetch (same closure as gateway SSR), so helpers like `propOr.jsx` do not need to be listed in the manifest. HTML fallback bodies are skipped; the walk caps at 256 modules.

**`StyleContext`** (`@clevertree/style`) accepts **`fullScreen={false}`** when an outer boot shell owns the viewport.

Example (paradigm-threat-files):

```jsx
import BootShell from 'hooks/client/components/BootShell.jsx'
import StyleContext from '@clevertree/style'

export default function Root() {
  return (
    <BootShell>
      <StyleContext src="hooks/client/theme.yaml" fullScreen={false}>
        {/* routes */}
      </StyleContext>
    </BootShell>
  )
}
```

## CSR vs SSR in repo hooks

Hooks read **`manifest.relayHostMode`** from `@hooks`:

- **`csr`** — relay web client; no SSR assumptions; catalog/article components return `null` until `isClientLive()` (boot shell covers loading).
- **`ssr`** — parked server; P0 article may be pre-rendered; hydrate reuses `[data-pt-article]` markup.

See `paradigm-threat-files/hooks/CLEVERSCRIPT_TIP.md` and `hooks/client/hostMode.jsx`.

## Build / verify

```bash
# relay-runtime (host modules + WASM)
cd relay-gateway && cargo test -p relay-runtime

# relay-client-web
cd relay-client-web
npm run build:wasm   # after relay-runtime changes
npm run verify:local
npm run test:e2e     # optional; needs live master
```

After editing `relay-runtime/js/relay-runtime-web.js`, sync the vendored copy:

```bash
cp relay-runtime/js/relay-runtime-web.js relay-client-web/src/vendor/relay-runtime-web.js
```

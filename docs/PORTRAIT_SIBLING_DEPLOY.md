# Portrait sibling art-direction — deploy checklist

**Feature:** tip-gated `-portrait` `<picture>` sources (PopImage + embed markdown HTML).  
**Do not** invent portrait URLs client-side without the tip index.

## Commits (local → push GitLab only until go-live)

| Repo | Expected tip theme |
|------|--------------------|
| `relay-runtime` | `@relay/path` portrait invent + index lookup + gallery hash/DOM id; `@relay/util` TTS sentence chunking |
| `relay-gateway` (`clevertree/relay-gateway`) | tip portrait index, float-404 HTTP fix, SSR inject |
| `paradigm-threat-files` | thin PopImage / galleryStore / TOC / TTS dock (host helpers, no tip invent/probe) |

## Order

1. **Push `relay-runtime` `main` to GitLab** (CI clones it next to the gateway checkout).
2. **Push `relay-gateway` `main` to GitLab** (`origin` / `gitlab` → `clevertree/relay-gateway`).
3. **Trigger gateway CI on demand** (auto-run is off for branch pushes):

```bash
cd ~/dev/relay/relay-gateway
glab ci run -b main -R clevertree/relay-gateway
# or: GitLab UI → Build → Pipelines → Run pipeline
```

`test:verify` / `package:linode-tgz` clone public `relay-runtime` + `md2ast` (and `relay-client-web` for pack) into the path-dep layout Cargo expects.4. **Push `paradigm-threat-files` to GitLab `main` only** when ready for forge SoT (content tip).  
   **Go live (Relay remotes)** is a separate session: `git push dfw1/hkg1/lon1 HEAD:main` after gateway nodes are upgraded.

5. **Gateway node deploy** (Linode / fleet): use CI `package:linode-tgz` artifact or existing pack script after `test:verify` is green. Restart gateways so SSR cache / new binary pick up inject + HTTP 404 fix.

## Verify after gateway + files tip are live

- Mobile Depression article: hero `currentSrc` is landscape PNG (no `-portrait` file).
- Mobile scorecard embed: `currentSrc` is `…-portrait.png` when sibling exists.
- Missing media URL returns **HTTP 404** (not 200 JSON).
- View-source / SSR HTML contains `id="relay-portrait-siblings"` and `__RELAY_PORTRAIT_SIBLINGS__`.

## Unit coverage (gateway)

- `git::portrait::*` — invent, repo-rel, set membership, tip walk fixture, `if_present`, JSON map
- `git::markdown_html::*` — no invent without index; picture only when in set
- `git::get_hook_http::*` — int + float `404` → HTTP 404; fractional / OOR stay 200
- `git::embed_html::*` — inject before `embed.js`; script-breakout escape

*Last updated: 2026-08-08*

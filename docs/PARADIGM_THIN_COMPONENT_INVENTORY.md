# Paradigm Threat → CleverScript component inventory (Phase 0)

Source: `/home/ari/dev/pt/paradigm-threat-site`.  
**UI hosts:** thin `paradigm-threat-files` + thin `paradigm-threat-timeline` (in-repo hooks).  
**Retired park shell:** `paradigm-threat-web` (no catalog domains).

## Priority map

| Area | Items | Priority |
|------|-------|----------|
| Chrome | SiteChrome, Nav, ThemeToggle, Layout, DirNav | **P0 — shipped (files)** |
| Content | ArticleView, plain MarkdownRenderer, same-repo paths | **P0 — shipped (files)** |
| Search | SearchPage, QUERY | **P1 — shipped (files)** |
| Timeline shell | TimelineHome, EventView on timeline subdomain | **P2 — shipped (timeline)** |
| Media / overrides | RelayImage, AutoIndex, ChangeLog, EmbedFile | **P1 — disabled** (`hooks/COMPONENTS_DISABLED.md`) |
| Timeline viz | VisTimeline, maps, Three | **Host-defer** |
| Defer | TTS, Chat, SiteAi, submit, files `/timeline` subpath | **P3** |

## Existing CleverScript

| Repo | Client | Status |
|------|--------|--------|
| paradigm-threat-files (thin) | Full shell: get + ArticleView + DirNav + Search | Park canary |
| paradigm-threat-timeline (thin) | TimelineHome + EventView | Repo subdomain |
| paradigm-threat-web | Retired from park | Scrap only |

## Routes

**Files:** `/`, catch-all article, `/search`.  
**Timeline:** `/`, event md paths on timeline vhost.  
**Deferred:** files `/timeline`, `/chat`, `/reply`, `/submit`, `/mcp`, TTS.

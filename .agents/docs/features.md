# swiss Feature-Point Master Inventory

> The single consolidated ledger of every feature point in the product, one line each.
> Sources: the twelve domain documents in this directory (all re-verified against the tree at
> `4eaba56`, 2026-09-22) plus the specs in the repo's `docs/`. Where this file and a domain
> document disagree, the domain document (and behind it, the code) wins — this is the rollup,
> not a second source of truth.
>
> Status vocabulary: **Shipped** (code + tests + docs agree) · **Shipped, open polish**
> (works, a tracked fix-plan item remains — `fix-plan #N`) · **Shipped, test gap** (works and
> is live-verified, a planned test in `tests/*.md` is still open) · **Owner-side** (cannot
> move from a dev session).

## MCP (plugin `mcp`, ~30 endpoints — detail: [mcp.md](mcp.md))

| # | Feature point | Surface | Code home | Tests | Status |
| --- | --- | --- | --- | --- | --- |
| M1 | Every MCP client hangs under `/mcp/{name}`; old root POST/DELETE answers a moved-hint 404 (hard cutover, never an alias) | HTTP | src/app.rs:528-593 | tests/app.rs (16) | Shipped |
| M2 | Ten adapters: echo / proc / mysql / mariadb / pg / redis / rest / http / zai-vision / figma(expands to http+oauth) | adapters | crates/swiss-mcp/src/adapters/mod.rs:311-358 | swiss-mcp inline 332 | Shipped |
| M3 | proc MCPs are lazy by default: wake on first request, reap after idle (the biggest memory feature) | lifecycle | adapters/proc.rs | registry.rs inline | Shipped |
| M4 | http/rest deliberately have no health ping — registry reports unknown (metered endpoints) | registry | crates/swiss-mcp/src/registry.rs | inline | Shipped |
| M5 | OAuth (docs/24): single-flight authorize + poll, refresh-on-401 with one retry, NEEDS_AUTH refusal, sealed mcp-oauth.json | POST /api/mcps/{name}/authorize | src/adminapi.rs:1648-1801, oauth.rs | adminapi | Shipped |
| M6 | Definition revisions (docs/28): replace/delete/restore, cap five oldest-evicted, parked snapshots, rename carries | /api/mcps/{name}/revisions* | adminapi.rs:1400-1639 | adminapi | Shipped |
| M7 | Connection test: TESTABLE_TYPES = mysql/mariadb/redis/pg/http/rest, 5 s cap, driver error verbatim | POST /api/mcpdefs/test | adminapi.rs:1120-1251 | adminapi (111) incl. mariadb gate | Shipped |
| M8 | Definition import (namespace rule: static segment would shadow {name}) | POST /api/mcpdefs/import | adminapi.rs:1069-1109 | adminapi | Shipped |
| M9 | Tool call proxying (RawValue envelope, never a DOM), tool toggle, tool-history backfill | POST /api/mcps/{name}/call | swiss-mcp | root tests | Shipped; tool/resource toggle routes have zero route-level tests (tests/mcp.md row 12, planned) |
| M10 | Traffic page: full stored args+reply search, transactional paging | GET /api/calls | run-history.ts | panel vitest | Shipped |

## Data (plugin `data`, 20 `/api/db/*` routes — detail: [data.md](data.md))

| # | Feature point | Surface | Code home | Tests | Status |
| --- | --- | --- | --- | --- | --- |
| D1 | Connection list from adapter browser hooks (which MCPs are browsable) | GET /api/db | dbbrowser_api.rs:181 | dbbrowser_api 46 | Shipped |
| D2 | Lazy table list (bounded page + counted total, name filter) | GET /{name}/tables | dbbrowser_api.rs | inline | Shipped |
| D3 | Database axis (docs/43 M3): primary/current/rest, read-only reasons for pg/redis | GET /{name}/databases | dbbrowser_api.rs:335 | wiring | Shipped |
| D4 | Row grid: columns/rows/total/editability, server-side filters (JSON columns fixed via CAST(? AS JSON)) | GET /{name}/data | swiss-host dbbrowser.rs | swiss-it L1 | Shipped |
| D5 | Export json/csv/sql (streaming dump, filter-bearing, caps + x-export-* headers) | GET /{name}/export | dbbrowser_api.rs | inline + swiss-it | Shipped |
| D6 | Structure tabs: columns/indexes/FK/DDL | GET /{name}/schema | dbbrowser_api.rs | inline | Shipped |
| D7 | DDL sheets with live statement preview (W4.6) | POST /{name}/ddl(-preview) | dbbrowser_api.rs | inline | Shipped |
| D8 | SQL console: single-statement rule (reads and writes alike), server-side completion, elapsedMs | POST /{name}/query, /completion | mysql/pg_browser.rs | swiss-it L1/L2 | Shipped |
| D9 | Redis key browser: SCAN paging, MATCH pattern, type filter, per-key view + TTL edit + rename/delete | GET /{name}/keys, /key | redis_browser.rs | swiss-it redis 19 | Shipped |
| D10 | Redis structured edits: buffered hash/zset/list/set edits → command preview → one-pipeline commit (409 on moved rows) | POST /{name}/redis-pipeline, /edits | dbbrowser_api.rs | swiss-it | Shipped |
| D11 | Redis stream view (docs/45): newest-first 100-row window, entry-id cursor paging, Load earlier | GET /{name}/stream | dbbrowser_api.rs:945 + data-stream.ts | unit + swiss-it + vitest 833 + live | Shipped |
| D12 | Stream Follow: 1/2/5 s XREVRANGE into a 500-row ring, hidden tab = zero fetch, gap bar, jump voids in-flight ticks (S3) | same | data-stream.ts:371-391 | vitest jump-race case + live walk | Shipped (meta entries count does not refresh mid-follow — cosmetic, reopen updates) |
| D13 | Stream consumer groups: read-only XINFO GROUPS fold (name/consumers/pending/lag/last-delivered) | GET /{name}/stream/groups | dbbrowser_api.rs:966 | vitest + live | Shipped |
| D14 | Activity monitor + kill (live sessions on the leased connection) | GET /{name}/activity, POST /activity-kill | dbbrowser_api.rs | swiss-it | Shipped |
| D15 | Object tabs (docs/42/43): card tabs cap 12, sidebar tree, scoped toolbar, status strip | panel | data-tabs.ts / data-tree.ts | panel vitest | Shipped |

## Tunnels (plugin `tunnels`, 16 route paths / 18 entries — detail: [tunnels.md](tunnels.md))

| # | Feature point | Surface | Code home | Tests | Status |
| --- | --- | --- | --- | --- |
| T1 | SSH connections CRUD + groups; rows carry proxy trio (password as MASK sentinel), jump id, keyPath — absent-when-unset, frozen prefix | /api/tunnels* | tunnel/manager.rs rows():1614-1699 | rows()-read-surface test + live | Shipped |
| T2 | Edit sheet prefills from the row (keyPath round trip; empty falls back to the default path) | panel sheet | tunnel-sheets.ts:85,133,224 | vitest + live prefill walk | Shipped |
| T3 | HTTP/SOCKS5 proxy dialing (portless normalized, userinfo refused, whole-field refs) | tunnel dial | ssh.rs + proxy.rs | inline | Shipped |
| T4 | Jump chains with save-time validation (unknown/self/cycle refused; proxy+jump mutually exclusive) | CRUD validation | manager.rs | inline family | Shipped |
| T5 | Forwarding rules, port occupancy ledger, failed-start rows, debug port endpoint (no panel consumer) | /api/tunnels/port/* | api.rs | inline | Shipped (debug endpoint documented as such) |
| T6 | Auto-reconnect, per-connection runtime state (idle → starts on first request) | runtime | manager.rs | shell family | Shipped |

## Jobs (plugin `jobs` — detail: [jobs.md](jobs.md))

| # | Feature point | Surface | Code home | Tests |
| --- | --- | --- | --- | --- |
| J1 | Config-driven v2 schema with migration; can target any registered capability | managed jobs config | swiss-jobs def.rs/migrate.rs | inline 90 + root groups_e2e |
| J2 | Calendar scheduling (cron fields, timeout policy, misfire skip/run-once) | scheduler | schedule.rs | calendar-walk tests |
| J3 | Execution fully handed to the shared RunCoordinator (producer "jobs"); runlog with byte budget | runs | swiss-host runs.rs | wedged/deadline tests |
| J4 | Jobs page: definitions, history, live output; history reads the runlog | panel | views/jobs.ts | panel vitest |

## Terminal (plugin `terminal` — detail: [terminal.md](terminal.md))

| # | Feature point | Surface | Code home | Tests |
| --- | --- | --- | --- | --- |
| E1 | Remote SSH terminal over tunnel targets; local PTY shell off by default | WS /terminal | swiss-terminal + root axum | terminal_ws 15 |
| E2 | Single-use tickets, burned within 10 s | POST ticket | tickets.rs (6 tests) | inline |
| E3 | Windows paste/copy keys; per-terminal Ctrl+Shift+F find (addon-search 0.16.0) | panel | views/terminal.ts | panel vitest |
| E4 | Output-only recordings (8 MB cap; never committed) | recordings | recording.rs (5) | inline |
| E5 | Four timers (idle reap, activity deadline…) with injected clocks | session | session_tests 27 | inline |

## Remote (plugin `remote`, docs/34+41 — detail: [remote.md](remote.md))

| # | Feature point | Surface | Code home | Tests |
| --- | --- | --- | --- | --- |
| R1 | Sealed target table (remote.json): endpoints/provider, CRUD with strict parser, endpoint-when-serving | /api/remote/targets* | swiss-remote | inline 68 |
| R2 | exec/sync/pull/cat/write actions: UTF-8 character-boundary windows, LANG=C.UTF-8 default | actions.rs:1092+ | swiss-remote | swiss-it gateway 7 |
| R3 | Remote Runs page: live cursor output, actor everywhere, 7-day audit window protected from budget eviction | GET /api/runs/{id}/output | services/api.rs:218-258 + runs.rs | root tests + panel vitest |
| R4 | Five MCP tools under /mcp/remote (owner remote-mcp); CLI `swiss remote`/`swiss run` (-- passthrough, resolve, audit) | MCP + CLI | adapters/remote.rs:215-219 | cli inline |
| R5 | Budgets: 30 d / 500 MiB / 5000 runs / 16 MiB output / 64 KiB tail | history.rs | swiss-remote | inline |

## Process (page-less capability plugin — detail: process.md)

| # | Feature point | Surface | Code home | Tests |
| --- | --- | --- | --- | --- |
| P1 | process.exec + legacy-command through the shared supervisor; registers last, starts first | actions | swiss-host process | plugin_host 28 |
| P2 | Process history + capabilities page region; Disable withdraws capabilities, keeps history | plugins page | plugins/mod.rs | plugin_host |

## Host mechanisms (crate `swiss-host` + root — detail: [host.md](host.md))

| # | Feature point | Surface | Code home | Tests | Status |
| --- | --- | --- | --- | --- | --- |
| H1 | Plugin host state machine: enabled/running/visible distinct; disable really releases (routes/tasks/children) | /api/plugins | swiss-host plugin host | plugin_host 28 | Shipped |
| H2 | Loopback-only boundary: non-loopback Host/Origin refused 403; bearer only on MCP endpoints | every request | app.rs guard | app.rs tests | Shipped (peer-address route test planned: tests/host N-series) |
| H3 | Sealed state: AES-256-GCM + HKDF per-file keys, DPAPI master.key, format frozen (docs/05) | state files | swiss-core secure/ | seal_bench + fixtures | Shipped |
| H4 | Secret vault (docs/19) + ${...} envelope rules (docs/25): refs only on disk, expansion at adapter build, sentinel masks | /api/secrets | vault + config.rs | the_family_* scope | Shipped |
| H5 | Unified groups family (docs/20): mcps/conns/rules/jobs/secrets/tokens scopes, rename/members/order | /api/groups/{scope}* | adminapi.rs:798-917 | groups_e2e | Shipped |
| H6 | Run accounting: shared actions/runs pool, actor taxonomy (cli/mcp/panel/jobs/api), caps + 429 | /api/actions, /api/runs* | runs.rs | root tests | Shipped |
| H7 | Config: strict load, env precedence, autostart, memory footprint endpoint, shutdown | /api/autostart, /api/memory... | config.rs + adminapi | env_precedence etc. | Shipped |
| H8 | CLI: start/stop/status/logs/token/creds/autostart/update/export/import/remote/run/skill | CLI | src/cli* | cli inline | Shipped |

## Panel (crate `swiss-panel` — detail: [panel.md](panel.md), [style-design.md](style-design.md))

| # | Feature point | Surface | Code home | Tests | Status |
| --- | --- | --- | --- | --- | --- |
| L1 | 66 own ES modules, 13 pages (10 plugin + 3 host), rail + context-bar shell (docs/39) | panel | panel/src | npm run check (833) | Shipped |
| L2 | i18n en/zh (docs/38): symbolic keys, en static floor, zh lazy, 文/A flip, two machine gates | panel | i18n.ts + locales | i18n-complete/ratchet | Shipped |
| L3 | Sheets / empty states / tiered confirms / JSON tree with repaint-surviving opens | panel | add-sheet.ts, util.ts | panel vitest | Shipped |
| L4 | Light/dark themes, five-step type scale, CSS-variable design system | panel | base.css | — | Shipped |
| L5 | Per-plugin last-page memory; focus mode | panel | last-page.ts | vitest | Shipped |
| L6 | V2 Unicode-glyph cleanup coverage (⚙/⚿/✕/↩/↺/↑↓/📁/📄 remain, en+zh) | panel | data-grid/filters/browsers | — | Shipped, open polish (fix-plan #14) |
| L7 | V5 red-button-into-menu coverage (Tokens Revoke, Secrets Delete now live in the danger menus) | panel | tokens.ts:346-349, secrets.ts:280-281 | — | Shipped (fix-plan #15 closed) |

## Open items rollup (the single remaining-tasks view)

- **Panel polish** (fix-plan #12 #14 #16 #17): stale header comments, glyph leftovers, three renames
  + three interactions, two generations of localStorage key names.
- **Small code/comment items** (fix-plan #1 #2 #3 #4/#23 #5 #7 #8 #9 #10 #11): stale comments, docs/17
  status line, docs/13 snapshot, docs/09 state machine, traffic.rs OnceLock, resolve_def dead chain.
- **Planned tests still open** (tests/*.md matrices): the P0 rollup (tool/resource toggle routes,
  lazy-wake + failed-start shapes, /api/db 403/413, peer-address, registration rejections, /api/shutdown,
  tunnels wire contract) plus N/G/mcp/terminal planned names and swiss-remote sync.rs coverage.
- **Cosmetic watch**: stream meta entries count during Follow; suspected (never reproduced) adminapi flake.
- **Owner-side**: repository creation, CI first run, first tag (docs/40); deploy to 19999 via
  scripts/deploy.ps1; cutting the Unreleased changelog into a version.

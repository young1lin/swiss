# swiss Project Functionality and Style Overview (multi-agent audit)

> Delivery method: 6 parallel audit agents (A MCP / B Data / C Tunnels+Jobs / D Terminal+Process / E host core / F panel & style) + a main coordinating agent consolidating the draft, 11 documents in total (remote.md joined later when the remote plugin shipped — agent slot R).
> Audit target: the current workspace source code, a read-only audit; where source and this document conflict, the source wins. Every functionality point carries a source-code path (down to line numbers) as evidence in its sub-document.
> Re-audited through commit `6b84f26` (2026-09-12) by three fresh agents: groups become ordinary (default renameable/deletable, first-slot sink, `groupsV2` marker — mcp.md/host.md/tests), Data table-list sorting + the `/api/db` picker following the sidebar visual order (data.md/tests), and panel group drag handles / menu moves / Data list sorting (panel.md/style-design.md).
> Post-docs/20 refresh (2026-09-13, G1–G8): ONE group family `/api/groups/{scope}` (mcps/conns/rules/jobs/secrets/tokens; ADR-015) replaced every per-scope grouping route — the retired `PUT /api/order`, `PUT /api/groups`, `POST /api/groups/{name}/rename`, `PUT /api/mcps/{name}/group`, the `/api/tunnels/groups/*` set and `PUT /api/tunnels/order` are gone. Jobs/Secrets/Tokens gained groups (jobs ride the config row; secrets/tokens refuse the order verb); `/api/db` rows carry `group` and the Data dropdown folds optgroups; the panel renders every list through `js/groups.js` (fold state under `swiss.groups.<scope>`). mcp/tunnels/panel/host/data/jobs updated in place.
> Refresh against the current tree (post docs/24–45): MCP clients moved under the `/mcp/` prefix (docs/24); Tunnels gained the SSH proxy/jump fields (docs/27) and the connection rows now carry `keyPath`; MCP def revisions landed (docs/28, `/api/mcpdefs/*` + the `/api/mcps/{name}/revisions` family); the **swiss-remote** plugin shipped (docs/34 + docs/41 — target table, exec/sync/pull, `swiss remote`/`swiss run`, five MCP tools under `/mcp/remote`, run output + audit); the Data console became single-statement-writable and gained `/api/db` streams (docs/22, docs/45); the Gateway group is three host-owned pages (plugins/secrets/system); all counts below recounted against the code.

## Project One-Liner

**swiss** — the developer's pocket multitool: one very-low-memory Rust process (a single static exe) that hangs every
small tool AI programming needs (MCP gateway, database browsing, SSH tunnels, scheduled jobs, web terminal, process
management) behind one loopback port, `127.0.0.1:19999`, and one admin panel. It is the port and plugin-shaped
refactor of the Node project node-original: the same on-disk formats (sealed state files, gateway.config.json,
tunnels.json); for the workload where the Node build measured 113.8 MB RSS, the Rust side runs at
22.4 MB (8.6 MB private bytes).

## Fast Facts

| Fact | Value |
| --- | --- |
| Listening | 127.0.0.1:19999 (loopback is the security boundary, not a default; a non-loopback host is refused at load time) |
| Artifact | a single self-contained swiss.exe (current_thread Tokio runtime, opt-level z + fat LTO) |
| Auth | **The panel and /api/* are entirely unauthenticated**: the loopback guard (any non-local Host/Origin gets 403) is the entire security boundary ("The gate: there is none"); the Bearer token belongs to MCP endpoints only (the tokenEnv-named variable wins; SWISS_TOKEN / the legacy name SWISS_TOKEN both honored) |
| Credentials | config stores only ${ENV_VAR} or secret:// vault references, expanded only at adapter build time; the panel round-trips sentinel masks; vault values go in but never out |
| State files | AES-256-GCM + HKDF-derived per-file keys; master.key through DPAPI (each build opens the other's files; format frozen) |
| Panel | crates/swiss-panel/panel/src/*.ts with the emit committed under src/admin_assets/js (ADR-024); gate: npm run check in crates/swiss-panel/panel/; a SHA-1 version stamp drives /api/info self-reload |
| Build | cargo build --release; tests/clippy must go --workspace; deploys run through scripts/deploy.ps1 |

## Architecture (crate dependency edges are the architecture)

```
swiss-core  ←  swiss-host  ←  { swiss-mcp, swiss-data, swiss-tunnels, swiss-jobs, swiss-terminal, swiss-remote, swiss-panel }  ←  swiss (assembly + CLI)
```

- **swiss-core**: platform primitives (paths, logging, sealed envelopes, process tree/job objects/DPAPI/PTY), knows nothing about "gateways"; the only layer in the project allowed unsafe.
- **swiss-host**: the mechanism every subsystem shares — the plugin host (descriptor/state machine/hot-plug), the Action/Run services, the process supervisor, the connection catalog, the shell capability bits, the secret vault, the config store, the loopback security boundary. Contains no business logic.
- **The seven subsystem crates** do not depend on each other (the former data→mcp edge was removed along with the connection catalog); the root `src/` is assembly only: `src/builtin.rs` + `src/plugins/` are the composition table — "add a plugin = one factory + one register line", the host gains no match arm.
- Registration order is the reverse of startup order: Process registers last, so it starts first (capabilities ahead of consumers); Tunnels ahead of MCP, which may ride on tunnels.
- swiss-terminal deliberately ships zero axum and zero SSH: HTTP/WS live in the root crate (the axum layer only covers routes that already exist at mount time), SSH goes through the tunnels ShellRegistry capability bit.
- swiss-remote links no SSH client either (docs/34): it holds the agent-facing vocabulary (targets, exec, sync, pull) and leases every network touch through the host's RemoteTransportRegistry, which the tunnels plugin serves — the same capability-seat shape as the shell.

## Plugin Inventory

| Plugin id | Name | One-liner | Contributed pages (order) | Main code | Sub-document |
| --- | --- | --- | --- | --- | --- |
| mcp | MCP gateway | every MCP server hangs under /mcp/{name} (docs/24 — the prefix is the MCP plugin's domain): echo / proc (lazy start + idle reaping) / http / rest (deliberately no ping) / direct / proxy adapters + in-process mysql/pg/redis drivers + the builtin `remote` MCP (five tools, docs/34 R7); call log, traffic ring, token management, def revisions (docs/28) | Servers(10) Traffic(20) Token(30) | crates/swiss-mcp | [mcp.md](mcp.md) |
| tunnels | Tunnels | SSH tunnels: 16 /api/tunnels* route paths (18 method+path entries), forwarding rules, auto-reconnect, HTTP/SOCKS5 proxy dialing and jump references (docs/27), the 409+dependents destructive-action funnel, routing connections for MCP; also the host's shell AND remote-transport provider | SSH Connections(30) Port Forwards(35) | crates/swiss-tunnels | [tunnels.md](tunnels.md) |
| data | Data | database browsing/queries (20 /api/db/* routes, docs/22 + docs/43 + docs/45: databases catalog, streams, activity, completion, redis pipeline, DDL preview), leasing MCP's shared connection pools from the host connection catalog with request-scoped leases; zero config, zero disk writes, zero background tasks | Data(40) | crates/swiss-data | [data.md](data.md) |
| jobs | Jobs | config-driven scheduled jobs (v2 schema), execution fully handed to the shared RunCoordinator (producer="jobs"), can target any registered Action | Jobs(50) | crates/swiss-jobs | [jobs.md](jobs.md) |
| terminal | Terminal | web terminal: remote SSH + local PTY, xterm.js; tickets are single-use and burn within 10 s; recordings capture output only; the local shell is off by default | Terminal(70) | crates/swiss-terminal + src/plugins/terminal*.rs | [terminal.md](terminal.md) |
| remote | Remote | agent-friendly remote execution (docs/34): a sealed target table (remote.json), exec/sync/pull/cat/write actions through the shared RunCoordinator, the /api/remote routes, the `swiss remote`/`swiss run` CLI, five MCP tools under /mcp/remote, and a durable run record with UTF-8 discipline and a seven-day audit window (docs/41) | Targets(75) Runs(76) | crates/swiss-remote + src/plugins/remote.rs + src/remote_cli.rs | [remote.md](remote.md) |
| process | Process | a page-less capability plugin: process.exec / legacy-command actions through the shared process supervisor; registers last, so starts first | —(no pages) | swiss-host services + src/builtin.rs | [process.md](process.md) |
| (host) | Host & CLI | the plugin-host state machine, loopback boundary, config/vault, CLI, daemon — the mechanism all plugins share | Plugins(1000), Secrets(1001), System(1002) (host-owned pages synthesized by the panel, page-registry.ts:37-41) | swiss-host / swiss-core / root src/ | [host.md](host.md) |
| panel | Panel | the embedded admin panel itself: 66 own ES modules (52 in panel/src + 14 view modules), rail + context-bar navigation, thirteen pages (ten plugin-contributed plus the host-owned trio) | —(it is the panel) | crates/swiss-panel | [panel.md](panel.md) |

> Navigation detail: level 1 = plugin groups (the grouping pure function lives in page-core.ts, group order = the smallest
> page order in the group); a multi-page plugin lays its pages as underline tabs in the context bar — MCP (Servers | Traffic
> | Token), Tunnels (SSH Connections | Port Forwards), Remote (Targets | Runs); single-page plugins draw only the title. The
> Token page belongs to the MCP group, but the /api/tokens route is owned by the host — credentials do not stop working when
> a plugin is disabled. The docs/13 in-text snapshot is stale (it still says mcp contributes two pages and level 1 has seven
> tiles); /api/plugins, builtin.rs and src/plugins/ are authoritative.

## Unified Patterns (ten of them, isomorphic across the whole project)

1. **Composition-table philosophy**: adding a capability = data declarations (descriptor/pages/routes/requires) + one register line; zero match arms in the host; route/page id conflicts are loudly rejected at registration.
2. **Routes stay mounted; dispatch consults live state**: one cheap is_active question per request; disabling really releases — route slots out first → instance.stop() → scope.shutdown (withdraw→drain→close, cancel→grace→forced teardown).
3. **Loopback, then the admin session**: /api/* and the panel shell need the CLI key or a session cookie set by a one-time `/?token=` link (docs/48, src/session.rs); the bearer belongs to MCP endpoints only, and it is checked before the body is read.
4. **Credentials are references only**: ${ENV_VAR} / secret:// hit the disk; the expansion moment is globally unique (adapter build time); sentinel masks round-trip; resolved values never enter error messages.
5. **Errors are values, not panics**: the {error} envelope, absent-not-null, dot-path error pointers, 409 + structured confirmation; a single point of failure is isolated into one log line; no unwrap on config/network/db/fs.
6. **Memory discipline is a constant, not a config**: a 64KB catch-up buffer, 8MB recordings, a 32-entry history ring, proc lazy by default + 10 min reaping, RawValue pass-through on forwarding paths, direct Win32 calls with zero powershell, exports capped at 100k rows.
7. **Strong cancel semantics**: biased select, first wins; when cancel returns, the child has already been reaped and the readers joined.
8. **The panel is the spec**: panel TypeScript in crates/swiss-panel/panel/src with the emit committed (ADR-024); a panel change ships with its vitest case in crates/swiss-panel/panel/test/; where docs and code conflict, code wins, but discovered debt gets fixed in the docs.
9. **Honesty principle**: every wrapper comment states item by item what disable really does and does not do; a missing capability gets a 503 naming who is missing; memory numbers are honest (childrenPending, not a confident 0).
10. **Test culture**: a behavior change gets its failing test first; a ported module ports its vitest cases too; clocks are injected, never slept on; a skipped guard test must be called out.

## Known Exceptions/Inconsistencies (summary; details and line numbers in each sub-document)

- **Documentation debt**: the docs/17 and 18 status lines still say "to do" although the V1–V7 visual refresh has fully landed; the docs/13 snapshot is stale; the WaitingDependency state designed in docs/09 was never implemented (a requires/requiresMet data projection stands in).
- **Stale comments**: mem.rs says /api/mem which is actually /api/memory; plugins/mod.rs points at src/host, which no longer exists; the pg_browser.rs module header contradicts the implementation; the panel's data-view.js header says "default 500" but it is 50; the util.js view-list comment lags behind.
- **Transitional states / dual philosophies**: traffic.rs is still a process-level OnceLock (CallLog is already instantiated); credential resolution has lenient and strict resolve_def coexisting; process.legacy-command's lenient refs vs process.exec's strict (Node compatibility preserved); tunnels replicates JS Number() leniency vs jobs' native-Rust strictness — ported vs native, two philosophies, each declared in comments.
- **Deliberate asymmetries**: /api/tokens/{id}/secret can be read back in plaintext (tokens and the vault have different threat models); CLI open uses cmd /c start (the platform default opener); local terminal sessions are uncapped (the user's call, deviation on record); a recording failure only warns and does not block the session.
- **Panel leftovers**: the V2 Unicode-glyph ban is not fully covered (⚙/✕/↺/↑↓ remain); the V5 red-button-into-menu rule misses Tokens Revoke / Secrets Delete; three renames with three interactions; two generations of localStorage key names coexist (swiss.* and swiss_*).
- **Re-audit deltas (through 6b84f26)**: stale comments contradicting the ordinary-group model on the panel side (util.js:13, base.css:334-337 — fixable only via the Node repo) and on the Rust side (adminapi.rs still says members "return to the default group"; recorded as host.md inconsistency item 7); the grouped lists diverge at the edges — only the MCP sidebar got drag grips + menu moves, and the sidebar's delete confirm names the first remaining group and refuses the last delete while Tunnels still names the literal default with no panel-side guard (tunnels.js:92-97).

## Style and Conventions Entry Point

- **[style-design.md](style-design.md)** — the overall style design (the key deliverable the user named): design principles →
  the panel visual design system (the full base.css CSS-variable table, light/dark themes, a five-step type scale) →
  the layout skeleton and two-level navigation → 18 component patterns → interaction patterns (the polling contract /
  fetch channels / tiered confirmation / revision races) → copy and naming → API shape conventions →
  unified backend architecture style → **the self-check checklist for onboarding a new plugin/new page (10 items)** + the
  known-exceptions list. In one sentence: swiss's style is one set of principles projected isomorphically onto Rust,
  HTTP, and CSS.

## Fixes and Tests (the second batch of deliverables)

The second batch's 7 agents deliver two executable things:

- **[fix-plan.md](fix-plan.md)** — the fix plan: the audit's 24 exceptions, each re-verified against source and triaged into
  **17 fixes + 10 won't-fixes** (27 rows in the overview table). Batch 1, eight zero-risk items (stale comments + docs
  status lines); batch 2, three code fixes (traffic.rs OnceLock instantiation following CallLog's S2 pattern, closing out
  the resolve_def dead code — the audit took it for a migration, verification found make_adapter already uses the strict
  version, plus the terminal.rs comment rewrite); batch 3, six panel-side items executed in this repo per ADR-024
  (the Node-checkout precondition was retired with it). All 10 won't-fix groups carry their
  provenance, to prevent mistaken fixes (deliberate asymmetries, compatibility shapes, user-decided items).
- **[tests/README.md](tests/README.md)** — the integration-test master plan: the shape of the whole repo's ~940 tests, the
  standard test-shape code (build_app → oneshot → asserts, the Host header must be patched to loopback), a shared-helpers
  quick reference, four environment iron rules, the three established patterns for what cannot be oneshot'd (WS / real
  rmcp / http proxying), global-state mutual exclusion, and the panel-test boundary (behavior lives in the Node repo's
  vitest; this repo tests only API shapes + asset guards).
- **tests/{mcp, data, tunnels-jobs, terminal-process, host}.md** — one row per functionality point: existing tests →
  gaps → new test names (English sentence-style) → Arrange/Act/Assert (ready to copy and start work) → special
  handling (clock injection / FakeConn / self-skip / temp ports). Roughly 130 functionality-point rows and ~90 planned
  new tests in total, ordered by security & wire contracts (P0) / main paths (P1) / edge cases (P2).

  The most important cross-domain test gaps (the P0 rollup across the matrices): the two MCP tool/resource toggle routes
  have zero coverage; the lazy-wake and failed-start dual-track error shapes have no route-level verification; /api/db's
  own loopback 403 and 413 have never been pinned; peer-address validation has never been tested at the route layer
  (by injecting ConnectInfo); PluginHost's three registration-time rejections have zero tests; the real /api/shutdown
  route has zero coverage; Tunnels' HTTP wire contract (CRUD / credential masks / 409 dependents) is almost entirely
  missing.

## Document Index for This Directory

| Document | Coverage | Audit agent |
| --- | --- | --- |
| [features.md](features.md) | **the consolidated feature-point master inventory**: one line per point across all nine domains (surface / code / tests / status) + the open-items rollup | rollup |
| [mcp.md](mcp.md) | MCP plugin: 27 functionality points, ~30 endpoints, 8 adapter classes | A |
| [data.md](data.md) | Data plugin: 20 /api/db/* routes, the lease contract, the docs/45 stream view | B |
| [tunnels.md](tunnels.md) / [jobs.md](jobs.md) | Tunnels (16 route paths; proxy/jump per docs/27) / Jobs (v2 schema, the producer contract) | C |
| [terminal.md](terminal.md) / [process.md](process.md) | Terminal (tickets / four timers / recording) / Process (supervisor / RunCoordinator) | D |
| [remote.md](remote.md) | Remote plugin (docs/34 + docs/41): targets, exec/sync/pull actions, CLI, run record, five MCP tools | R |
| [host.md](host.md) | host mechanisms, core primitives, CLI, security boundary, secret vault | E |
| [panel.md](panel.md) / [style-design.md](style-design.md) | the panel's module inventory and page walkthrough / the overall style-design spec | F |
| [fix-plan.md](fix-plan.md) | fix plan: 24 exceptions triaged (17 fixes + 10 won't-fixes, three batches) | FIX |
| [tests/README.md](tests/README.md) | integration-test master plan: patterns / helpers / iron rules / boundaries | G |
| [tests/mcp.md](tests/mcp.md) | MCP test matrix: 27 rows, 22 new tests (11 copyable code blocks) | H1 |
| [tests/data.md](tests/data.md) | Data test matrix: 21 rows, 33 gap tests (G1–G33) | H2 |
| [tests/tunnels-jobs.md](tests/tunnels-jobs.md) | Tunnels (23 rows) + Jobs (25 rows) matrices, 15+4 new tests | H3 |
| [tests/terminal-process.md](terminal-process.md) | Terminal (17 rows) + Process (13 rows) matrices | H4 |
| [tests/host.md](tests/host.md) | host test matrix: 30 rows, 15 new tests (N1–N15) | H5 |

The original spec documents (design intent and decision records) live in the repo's `docs/` directory: 01 memory
budget, 02 architecture, 03 dependency map, 04 porting checklist, 05 wire-format compatibility, 06 roadmap,
07 decisions, 08 testing, 09 plugin architecture, 10–11 Jobs, 12 remaining work, 13 panel navigation, 14–15 terminal,
16 operational hardening, 17–18 panel canvas and visual refresh, 19 secret vault — and since this audit was written,
among others: 20 groups, 22 data parity, 24 the /mcp/ prefix, 25 the ${...} envelope, 27 SSH proxy/jump, 28 def
revisions, 34 agent remote execution, 38 panel i18n, 39 shell refresh, 42–43 Data tabs/catalog, 45 Redis streams.

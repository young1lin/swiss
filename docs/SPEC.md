# swiss — Specification

This is the one specification of swiss. It describes the system **as it is now**, organised
by area: one numbered chapter per area under [`spec/`](spec/), and this page, which says how
to read and amend them and maps every anchor to its chapter. There are no per-change spec
files, no hand-off prompts and no separate decision log: a change to behaviour amends the
section it touches, in the same commit as the code.

## §about — How to read and amend this document

**Chapters.** Each area is one file, `docs/spec/NNNN-area.md`. Its title carries the area's
anchor (`# §host — The host`) and each section carries its own (`## §host.vault — The secret
vault`). The number is the chapter's place in the reading order and, once given, its identity:
a new area takes the next free number, numbers are never reused or reshuffled, and an area
that is renamed keeps its number. A new facet of an area is a new section in that area's
chapter, never a new chapter.

**Crates.** The chapters follow the code's modules. Every crate of the workspace is specified by
exactly one chapter, the one whose row in the chapter table names it, and a chapter's sections
follow the parts of its crate (the MCP plugin's adapter families are §mcp.proc, §mcp.http,
§mcp.rest, §mcp.db, §mcp.zai and §mcp.figma). The root `swiss` crate is composition and is specified with the host (§host.cli,
§host.daemon, §host.session); the dev-only `swiss-it` harness with the testing chapter
(§testing.it). The chapters that name no crate cover what cuts across all of them: the product,
the architecture, the security model, releases and the decisions. A new crate gets its chapter,
or its place in an existing chapter's row, in the same commit.

**Parts.** No file under `spec/` holds more than 1000 lines. A chapter that outgrows one file
becomes a directory, `docs/spec/NNNN-area/`: its `README.md` carries the title, the area's
introduction and the list of its parts, and numbered part files (`01-record.md`,
`02-reading.md`, …), each titled by what it covers, hold the sections. Splitting moves
sections between files; it never renames an anchor.

**Anchors.** Code comments, tests and skills cite a rule as `SPEC §host.vault`; the part before
the first dot names the chapter, so `§mcp.proc` lives in `spec/0005-mcp.md` and
`§memory.init` in one of the parts of `spec/0012-memory/` (the map says which). A decision is
cited by its number, `ADR-018`, and lives in §decisions. An anchor is a contract: renaming one
means rewriting every citation in the same commit (`git grep 'SPEC §'`).

**The map.** The anchor map below lists every chapter and every section in file order, each a
link to its heading. `tests/spec_layout.rs` holds this page and the chapters to each other:
the numbering of chapters and parts, each title and section anchor against its file name, the
size of each file, the chapter table, each parts list and the map against the headings, the
table's crates against the workspace's manifests, and every `§` citation in the tree against
an anchor that exists. A failing run prints the lines it expected.

**What wins.** The code wins over this document when they disagree — a disagreement is a bug
in whichever side is wrong, and fixing it means amending one of them, never leaving both.
Within the document, a later amendment replaces the text it contradicts; the old wording is
deleted, not struck through. History lives in git.

**Normative words.** *Must*, *never* and *always* are rules a test or a gate enforces; a rule
without an enforcing test is a gap to close, not a suggestion. Numbers in *records* (memory
readings, sizes, timings) are measurements with a date, never gates, unless the text says
gate.

**Amending.** Use the `/swiss-spec` skill: interview → amend the relevant section in place →
acceptance tests → delivery order. A new section gets its line in the map in the same commit; a
new area gets a chapter file, a row in the chapter table (naming its crate, if it has one) and
its block in the map; a new part gets its line in its chapter's parts list. A decision that is
hard to reverse, surprising without its context and a real trade-off gets an `ADR-NNN` entry
in §decisions.

**Language.** English, like every code comment. The panel's user-facing copy is localised
(§panel.i18n); this document is not.

## Chapters

| No. | Chapter | Crate | Area |
|---|---|---|---|
| 0001 | [§product](spec/0001-product.md) | — | What swiss is, the four properties, the memory record |
| 0002 | [§arch](spec/0002-arch.md) | — | Crates and edges, runtime model, dependency policy, Rust rules, declared exceptions |
| 0003 | [§core](spec/0003-core.md) | `swiss-core` | The data directory, the sealed envelope, the master key |
| 0004 | [§host](spec/0004-host/README.md) | `swiss-host`, `swiss-plugins`, `swiss` | Plugin host, lifecycle, routes, wire rules, security boundary, session, config, vault, groups, CLI, daemon |
| 0005 | [§mcp](spec/0005-mcp.md) | `swiss-mcp` | The MCP plugin: endpoint, protocol eras, registry, adapters, OAuth, revisions, call log |
| 0006 | [§data](spec/0006-data/README.md) | `swiss-data` | The Data plugin: connections, browsers, SQL console, Redis, tabs |
| 0007 | [§tunnels](spec/0007-tunnels.md) | `swiss-tunnels` | SSH connections, forward rules, proxy and jump hosts |
| 0008 | [§jobs](spec/0008-jobs.md) | `swiss-jobs` | Config-driven scheduled jobs |
| 0009 | [§terminal](spec/0009-terminal.md) | `swiss-terminal` | Local and remote shells over WebSocket |
| 0010 | [§remote](spec/0010-remote.md) | `swiss-remote` | Remote execution: targets, exec/sync/pull, runs |
| 0011 | [§process](spec/0011-process.md) | `swiss-process` | The process plugin: the supervisor, managed apps, the children view, the Process page |
| 0012 | [§memory](spec/0012-memory/README.md) | `swiss-memory` | Long-term project memory: records, projects, one MCP endpoint, the CLI |
| 0013 | [§llm](spec/0013-llm.md) | `swiss-llm` | The model hub: named providers, the embedding model, agents and their OTLP telemetry |
| 0014 | [§panel](spec/0014-panel/README.md) | `swiss-panel` | The admin panel: navigation, design system, UI library, TypeScript, i18n |
| 0015 | [§security](spec/0015-security.md) | — | The security model in one place |
| 0016 | [§testing](spec/0016-testing.md) | `swiss-it` | The gates, the suites, the integration harness, live verification |
| 0017 | [§release](spec/0017-release.md) | — | CI, release artefacts, open-source hygiene |
| 0018 | [§decisions](spec/0018-decisions.md) | — | ADR-001 … the decision log |
| 0019 | [§bus](spec/0019-bus.md) | `swiss-bus` | The event bus: one WebSocket per page, protobuf frames, generated codecs |

## Anchor map

### 0001 · [§product — What swiss is](spec/0001-product.md)

- [§product.properties — The four properties](spec/0001-product.md#productproperties--the-four-properties)
- [§product.memory — The memory record](spec/0001-product.md#productmemory--the-memory-record)
- [§product.nongoals — What this does not buy](spec/0001-product.md#productnongoals--what-this-does-not-buy)
- [§product.tactics — Where the memory goes, and the levers](spec/0001-product.md#producttactics--where-the-memory-goes-and-the-levers)

### 0002 · [§arch — Architecture](spec/0002-arch.md)

- [§arch.crates — Crates and edges](spec/0002-arch.md#archcrates--crates-and-edges)
- [§arch.runtime — Runtime model](spec/0002-arch.md#archruntime--runtime-model)
- [§arch.deps — Dependency policy](spec/0002-arch.md#archdeps--dependency-policy)
- [§arch.rules — Rust rules](spec/0002-arch.md#archrules--rust-rules)
- [§arch.exceptions — Declared exceptions](spec/0002-arch.md#archexceptions--declared-exceptions)

### 0003 · [§core — The core: where state lives and how it is sealed](spec/0003-core.md)

- [§core.home — The data directory](spec/0003-core.md#corehome--the-data-directory)
- [§core.sealed — The sealed envelope (frozen)](spec/0003-core.md#coresealed--the-sealed-envelope-frozen)
- [§core.masterkey — The master key](spec/0003-core.md#coremasterkey--the-master-key)

### 0004 · [§host — The host](spec/0004-host/README.md)

- [§host.plugins — The plugin contract](spec/0004-host/01-plugins.md#hostplugins--the-plugin-contract)
- [§host.lifecycle — States, start, stop, config changes](spec/0004-host/01-plugins.md#hostlifecycle--states-start-stop-config-changes)
- [§host.routes — Route trees and host-owned routes](spec/0004-host/01-plugins.md#hostroutes--route-trees-and-host-owned-routes)
- [§host.wire — HTTP wire rules](spec/0004-host/01-plugins.md#hostwire--http-wire-rules)
- [§host.boundary — The loopback boundary](spec/0004-host/02-access.md#hostboundary--the-loopback-boundary)
- [§host.session — The admin session](spec/0004-host/02-access.md#hostsession--the-admin-session)
- [§host.tokens — MCP client tokens](spec/0004-host/02-access.md#hosttokens--mcp-client-tokens)
- [§host.config — Config and the config store](spec/0004-host/02-access.md#hostconfig--config-and-the-config-store)
- [§host.refs — Credentials and the resolver](spec/0004-host/02-access.md#hostrefs--credentials-and-the-resolver)
- [§host.vault — The secret vault](spec/0004-host/02-access.md#hostvault--the-secret-vault)
- [§host.groups — Groups](spec/0004-host/03-shared.md#hostgroups--groups)
- [§host.actions — Actions and runs](spec/0004-host/03-shared.md#hostactions--actions-and-runs)
- [§host.history — Module-owned log storage (mechanics)](spec/0004-host/03-shared.md#hosthistory--module-owned-log-storage-mechanics)
- [§host.seats — Capability seats](spec/0004-host/03-shared.md#hostseats--capability-seats)
- [§host.memory — The memory reading](spec/0004-host/03-shared.md#hostmemory--the-memory-reading)
- [§host.cli — The command line](spec/0004-host/04-command-line.md#hostcli--the-command-line)
- [§host.setup — Configuring from the command line](spec/0004-host/04-command-line.md#hostsetup--configuring-from-the-command-line)
- [§host.gui — The panel in its own window](spec/0004-host/05-running.md#hostgui--the-panel-in-its-own-window)
- [§host.daemon — Daemon, pid file, logs, build stamp](spec/0004-host/05-running.md#hostdaemon--daemon-pid-file-logs-build-stamp)
- [§host.ops — Production, the test instance, deploys](spec/0004-host/05-running.md#hostops--production-the-test-instance-deploys)

### 0005 · [§mcp — The MCP plugin](spec/0005-mcp.md)

- [§mcp.endpoint — The client endpoint (ADR-018)](spec/0005-mcp.md#mcpendpoint--the-client-endpoint-adr-018)
- [§mcp.protocol — MCP protocol eras](spec/0005-mcp.md#mcpprotocol--mcp-protocol-eras)
- [§mcp.registry — Registry and lifecycle](spec/0005-mcp.md#mcpregistry--registry-and-lifecycle)
- [§mcp.defs — Definitions](spec/0005-mcp.md#mcpdefs--definitions)
- [§mcp.adapters — The adapter families](spec/0005-mcp.md#mcpadapters--the-adapter-families)
- [§mcp.proc — stdio children](spec/0005-mcp.md#mcpproc--stdio-children)
- [§mcp.http — Remote HTTP MCPs](spec/0005-mcp.md#mcphttp--remote-http-mcps)
- [§mcp.rest — REST declared in config](spec/0005-mcp.md#mcprest--rest-declared-in-config)
- [§mcp.db — The database engines](spec/0005-mcp.md#mcpdb--the-database-engines)
- [§mcp.zai — zai-vision (ADR-022)](spec/0005-mcp.md#mcpzai--zai-vision-adr-022)
- [§mcp.figma — The figma type (ADR-021)](spec/0005-mcp.md#mcpfigma--the-figma-type-adr-021)
- [§mcp.oauth — OAuth for remote MCPs (ADR-020)](spec/0005-mcp.md#mcpoauth--oauth-for-remote-mcps-adr-020)
- [§mcp.revisions — Revisions and disable (ADR-023)](spec/0005-mcp.md#mcprevisions--revisions-and-disable-adr-023)
- [§mcp.admin — The admin API](spec/0005-mcp.md#mcpadmin--the-admin-api)
- [§mcp.import — Import and the connection test](spec/0005-mcp.md#mcpimport--import-and-the-connection-test)
- [§mcp.logs — The call log and traffic on disk](spec/0005-mcp.md#mcplogs--the-call-log-and-traffic-on-disk)
- [§mcp.calls — The call log and the Logs tab](spec/0005-mcp.md#mcpcalls--the-call-log-and-the-logs-tab)
- [§mcp.traffic — Traffic (the ring and the archive)](spec/0005-mcp.md#mcptraffic--traffic-the-ring-and-the-archive)
- [§mcp.catalog — The connection catalog and the tunnel link](spec/0005-mcp.md#mcpcatalog--the-connection-catalog-and-the-tunnel-link)
- [§mcp.panel — The Servers, Traffic and Token pages](spec/0005-mcp.md#mcppanel--the-servers-traffic-and-token-pages)

### 0006 · [§data — The Data plugin](spec/0006-data/README.md)

- [§data.api — The routes](spec/0006-data/01-browse.md#dataapi--the-routes)
- [§data.browse — Tables and rows](spec/0006-data/01-browse.md#databrowse--tables-and-rows)
- [§data.databases — The database axis (ADR-027)](spec/0006-data/01-browse.md#datadatabases--the-database-axis-adr-027)
- [§data.grid — The grid, form and value views](spec/0006-data/01-browse.md#datagrid--the-grid-form-and-value-views)
- [§data.edits — Buffered edits](spec/0006-data/01-browse.md#dataedits--buffered-edits)
- [§data.sqlite — The built-in memory store](spec/0006-data/01-browse.md#datasqlite--the-built-in-memory-store)
- [§data.console — The SQL console](spec/0006-data/02-console.md#dataconsole--the-sql-console)
- [§data.history — What a console ran, per database](spec/0006-data/02-console.md#datahistory--what-a-console-ran-per-database)
- [§data.agent — The agent beside the console](spec/0006-data/02-console.md#dataagent--the-agent-beside-the-console)
- [§data.analysis — The analysis ledger](spec/0006-data/02-console.md#dataanalysis--the-analysis-ledger)
- [§data.completion — Server-side completion (ADR-017)](spec/0006-data/03-workspace.md#datacompletion--server-side-completion-adr-017)
- [§data.activity — Live sessions](spec/0006-data/03-workspace.md#dataactivity--live-sessions)
- [§data.export — Export and import](spec/0006-data/03-workspace.md#dataexport--export-and-import)
- [§data.ddl — Structural operations](spec/0006-data/03-workspace.md#dataddl--structural-operations)
- [§data.redis — Keys and values](spec/0006-data/04-redis.md#dataredis--keys-and-values)
- [§data.redis-console — The Redis console](spec/0006-data/04-redis.md#dataredis-console--the-redis-console)
- [§data.streams — Redis streams](spec/0006-data/04-redis.md#datastreams--redis-streams)
- [§data.pubsub — Redis pub/sub](spec/0006-data/04-redis.md#datapubsub--redis-pubsub)
- [§data.mongo — MongoDB: the routes and the sidebar](spec/0006-data/05-mongo.md#datamongo--mongodb-the-routes-and-the-sidebar)
- [§data.mongo-edits — Writes: one document, many, and transfers](spec/0006-data/05-mongo.md#datamongo-edits--writes-one-document-many-and-transfers)
- [§data.mongo-pipeline — Aggregation and explain](spec/0006-data/05-mongo.md#datamongo-pipeline--aggregation-and-explain)
- [§data.mongo-schema — Schema, indexes, validation](spec/0006-data/05-mongo.md#datamongo-schema--schema-indexes-validation)
- [§data.mongo-panel — The documents workspace](spec/0006-data/05-mongo.md#datamongo-panel--the-documents-workspace)
- [§data.tabs — Object tabs, the tree and the bars (ADR-026)](spec/0006-data/06-panel.md#datatabs--object-tabs-the-tree-and-the-bars-adr-026)
- [§data.sessions — The session cache](spec/0006-data/06-panel.md#datasessions--the-session-cache)

### 0007 · [§tunnels — The Tunnels plugin](spec/0007-tunnels.md)

- [§tunnels.api — The routes](spec/0007-tunnels.md#tunnelsapi--the-routes)
- [§tunnels.store — Connections, rules and tunnels.json](spec/0007-tunnels.md#tunnelsstore--connections-rules-and-tunnelsjson)
- [§tunnels.ssh — The SSH client](spec/0007-tunnels.md#tunnelsssh--the-ssh-client)
- [§tunnels.forward — Forward rules](spec/0007-tunnels.md#tunnelsforward--forward-rules)
- [§tunnels.selfheal — A wedged session](spec/0007-tunnels.md#tunnelsselfheal--a-wedged-session)
- [§tunnels.proxy — Proxy dialling](spec/0007-tunnels.md#tunnelsproxy--proxy-dialling)
- [§tunnels.jump — Jump hosts](spec/0007-tunnels.md#tunnelsjump--jump-hosts)
- [§tunnels.mcp — MCPs that ride a tunnel](spec/0007-tunnels.md#tunnelsmcp--mcps-that-ride-a-tunnel)
- [§tunnels.providers — Shells and remote execution](spec/0007-tunnels.md#tunnelsproviders--shells-and-remote-execution)
- [§tunnels.panel — The pages](spec/0007-tunnels.md#tunnelspanel--the-pages)

### 0008 · [§jobs — The Jobs plugin](spec/0008-jobs.md)

- [§jobs.config — The config row](spec/0008-jobs.md#jobsconfig--the-config-row)
- [§jobs.triggers — Triggers and scheduling](spec/0008-jobs.md#jobstriggers--triggers-and-scheduling)
- [§jobs.api — The routes](spec/0008-jobs.md#jobsapi--the-routes)
- [§jobs.runlog — Run records, state and history](spec/0008-jobs.md#jobsrunlog--run-records-state-and-history)
- [§jobs.apply — Lifecycle](spec/0008-jobs.md#jobsapply--lifecycle)
- [§jobs.migrate — The v1 migration and acceptance](spec/0008-jobs.md#jobsmigrate--the-v1-migration-and-acceptance)
- [§jobs.panel — The Jobs page](spec/0008-jobs.md#jobspanel--the-jobs-page)

### 0009 · [§terminal — Terminal](spec/0009-terminal.md)

- [§terminal.api — Routes, tickets and the wire](spec/0009-terminal.md#terminalapi--routes-tickets-and-the-wire)
- [§terminal.sessions — The session machine](spec/0009-terminal.md#terminalsessions--the-session-machine)
- [§terminal.config — Config](spec/0009-terminal.md#terminalconfig--config)
- [§terminal.recording — Recording](spec/0009-terminal.md#terminalrecording--recording)
- [§terminal.local — Local shells](spec/0009-terminal.md#terminallocal--local-shells)
- [§terminal.remote — Remote shells](spec/0009-terminal.md#terminalremote--remote-shells)
- [§terminal.panel — The terminal page](spec/0009-terminal.md#terminalpanel--the-terminal-page)
- [§terminal.verify — Acceptance and live verification](spec/0009-terminal.md#terminalverify--acceptance-and-live-verification)
- [§terminal.budget — Budget](spec/0009-terminal.md#terminalbudget--budget)
- [§terminal.roadmap — Not built yet](spec/0009-terminal.md#terminalroadmap--not-built-yet)

### 0010 · [§remote — Remote execution](spec/0010-remote.md)

- [§remote.targets — Targets and the routes](spec/0010-remote.md#remotetargets--targets-and-the-routes)
- [§remote.actions — The five actions](spec/0010-remote.md#remoteactions--the-five-actions)
- [§remote.security — What a target can and cannot do](spec/0010-remote.md#remotesecurity--what-a-target-can-and-cannot-do)
- [§remote.transport — The transport seat](spec/0010-remote.md#remotetransport--the-transport-seat)
- [§remote.utf8 — UTF-8 end to end](spec/0010-remote.md#remoteutf8--utf-8-end-to-end)
- [§remote.history — The run log and the audit](spec/0010-remote.md#remotehistory--the-run-log-and-the-audit)
- [§remote.cli — `swiss remote` and `swiss run`](spec/0010-remote.md#remotecli--swiss-remote-and-swiss-run)
- [§remote.project — Project binding, notes and the skill](spec/0010-remote.md#remoteproject--project-binding-notes-and-the-skill)
- [§remote.mcp — `/mcp/remote`](spec/0010-remote.md#remotemcp--mcpremote)
- [§remote.panel — The Targets and Runs pages](spec/0010-remote.md#remotepanel--the-targets-and-runs-pages)

### 0011 · [§process — Processes and the supervisor](spec/0011-process.md)

- [§process.actions — The two actions and their rules](spec/0011-process.md#processactions--the-two-actions-and-their-rules)
- [§process.runs — The coordinator's invariants](spec/0011-process.md#processruns--the-coordinators-invariants)
- [§process.supervisor — Spawn, capture, teardown](spec/0011-process.md#processsupervisor--spawn-capture-teardown)
- [§process.tree — Process trees on each platform](spec/0011-process.md#processtree--process-trees-on-each-platform)
- [§process.apps — Managed apps](spec/0011-process.md#processapps--managed-apps)
- [§process.detached — A program that outlives the gateway](spec/0011-process.md#processdetached--a-program-that-outlives-the-gateway)
- [§process.children — Every child that dies with the gateway](spec/0011-process.md#processchildren--every-child-that-dies-with-the-gateway)
- [§process.panel — The Process page](spec/0011-process.md#processpanel--the-process-page)
- [§process.cli — `swiss process`](spec/0011-process.md#processcli--swiss-process)
- [§process.verify — Acceptance and live verification](spec/0011-process.md#processverify--acceptance-and-live-verification)
- [§process.budget — Budget](spec/0011-process.md#processbudget--budget)
- [§process.roadmap — Not built yet](spec/0011-process.md#processroadmap--not-built-yet)

### 0012 · [§memory — Project memory for code agents](spec/0012-memory/README.md)

- [§memory.model — Notes, lifecycle, revisions](spec/0012-memory/01-record.md#memorymodel--notes-lifecycle-revisions)
- [§memory.scope — Projects, roots, resolution](spec/0012-memory/01-record.md#memoryscope--projects-roots-resolution)
- [§memory.capture — The write path](spec/0012-memory/01-record.md#memorycapture--the-write-path)
- [§memory.domains — The project map](spec/0012-memory/01-record.md#memorydomains--the-project-map)
- [§memory.freshness — Freshness and the review inbox](spec/0012-memory/01-record.md#memoryfreshness--freshness-and-the-review-inbox)
- [§memory.store — Storage and durability](spec/0012-memory/01-record.md#memorystore--storage-and-durability)
- [§memory.brief — The primary read path](spec/0012-memory/02-reading.md#memorybrief--the-primary-read-path)
- [§memory.search — Search](spec/0012-memory/02-reading.md#memorysearch--search)
- [§memory.embedding — Semantic search](spec/0012-memory/02-reading.md#memoryembedding--semantic-search)
- [§memory.links — The memory graph](spec/0012-memory/02-reading.md#memorylinks--the-memory-graph)
- [§memory.related — Association search](spec/0012-memory/02-reading.md#memoryrelated--association-search)
- [§memory.graph — Graph reads](spec/0012-memory/02-reading.md#memorygraph--graph-reads)
- [§memory.rag — asking a question](spec/0012-memory/02-reading.md#memoryrag--asking-a-question)
- [§memory.code — The code index](spec/0012-memory/03-sources.md#memorycode--the-code-index)
- [§memory.docs — The documents a project keeps](spec/0012-memory/03-sources.md#memorydocs--the-documents-a-project-keeps)
- [§memory.preview — What a note points at, read in place](spec/0012-memory/03-sources.md#memorypreview--what-a-note-points-at-read-in-place)
- [§memory.map — The first map, drawn from the code](spec/0012-memory/04-init.md#memorymap--the-first-map-drawn-from-the-code)
- [§memory.init — Full knowledge initialization](spec/0012-memory/04-init.md#memoryinit--full-knowledge-initialization)
- [§memory.sync — The memory follows the code](spec/0012-memory/05-sync.md#memorysync--the-memory-follows-the-code)
- [§memory.settings — Which agents a run asks](spec/0012-memory/05-sync.md#memorysettings--which-agents-a-run-asks)
- [§memory.cli — `swiss memory` offline](spec/0012-memory/06-surfaces.md#memorycli--swiss-memory-offline)
- [§memory.mcp — `/mcp/memory`](spec/0012-memory/06-surfaces.md#memorymcp--mcpmemory)
- [§memory.api — `/api/knowledge`](spec/0012-memory/06-surfaces.md#memoryapi--apiknowledge)
- [§memory.panel — The Memory page](spec/0012-memory/06-surfaces.md#memorypanel--the-memory-page)
- [§memory.security — Logs and trust](spec/0012-memory/06-surfaces.md#memorysecurity--logs-and-trust)
- [§memory.evaluation — Verification](spec/0012-memory/06-surfaces.md#memoryevaluation--verification)

### 0013 · [§llm — The model hub: providers, embedding, agents](spec/0013-llm.md)

- [§llm.config — The row](spec/0013-llm.md#llmconfig--the-row)
- [§llm.providers — Named endpoints](spec/0013-llm.md#llmproviders--named-endpoints)
- [§llm.embedding — The embedding model](spec/0013-llm.md#llmembedding--the-embedding-model)
- [§llm.agents — Named agents](spec/0013-llm.md#llmagents--named-agents)
- [§llm.telemetry — Reporting agent runs](spec/0013-llm.md#llmtelemetry--reporting-agent-runs)
- [§llm.history — The call and conversation record](spec/0013-llm.md#llmhistory--the-call-and-conversation-record)
- [§llm.api — `/api/llm`](spec/0013-llm.md#llmapi--apillm)
- [§llm.panel — The Providers, Embedding, Agents and History pages](spec/0013-llm.md#llmpanel--the-providers-embedding-agents-and-history-pages)
- [§llm.roadmap — Not built yet](spec/0013-llm.md#llmroadmap--not-built-yet)

### 0014 · [§panel — The admin panel](spec/0014-panel/README.md)

- [§panel.nav — Navigation, the shell and page layouts](spec/0014-panel/01-shell.md#panelnav--navigation-the-shell-and-page-layouts)
- [§panel.design — The design language](spec/0014-panel/01-shell.md#paneldesign--the-design-language)
- [§panel.code — Code blocks and their colour schemes](spec/0014-panel/01-shell.md#panelcode--code-blocks-and-their-colour-schemes)
- [§panel.ui — The UI library](spec/0014-panel/01-shell.md#panelui--the-ui-library)
- [§panel.pages — Per-page look rules](spec/0014-panel/02-pages.md#panelpages--per-page-look-rules)
- [§panel.groups — The grouped list](spec/0014-panel/02-pages.md#panelgroups--the-grouped-list)
- [§panel.settings — System, Plugins, Secrets](spec/0014-panel/02-pages.md#panelsettings--system-plugins-secrets)
- [§panel.i18n — Languages](spec/0014-panel/03-build.md#paneli18n--languages)
- [§panel.toolchain — TypeScript, the build and the emit](spec/0014-panel/03-build.md#paneltoolchain--typescript-the-build-and-the-emit)
- [§panel.lint — The lint ratchet](spec/0014-panel/03-build.md#panellint--the-lint-ratchet)
- [§panel.vendor — Vendored browser libraries](spec/0014-panel/03-build.md#panelvendor--vendored-browser-libraries)

### 0015 · [§security — The security model in one place](spec/0015-security.md)

- [§security.threats — What is in scope and what is not](spec/0015-security.md#securitythreats--what-is-in-scope-and-what-is-not)
- [§security.reporting — Disclosure](spec/0015-security.md#securityreporting--disclosure)
- [§security.rules — Rules for every change](spec/0015-security.md#securityrules--rules-for-every-change)

### 0016 · [§testing — Gates and suites](spec/0016-testing.md)

- [§testing.layers — Where a test goes](spec/0016-testing.md#testinglayers--where-a-test-goes)
- [§testing.rules — Environment, time and shared state](spec/0016-testing.md#testingrules--environment-time-and-shared-state)
- [§testing.it — The integration harness (gate 2)](spec/0016-testing.md#testingit--the-integration-harness-gate-2)
- [§testing.memory — The memory guard](spec/0016-testing.md#testingmemory--the-memory-guard)
- [§testing.live — Live verification](spec/0016-testing.md#testinglive--live-verification)
- [§testing.gaps — Known gaps](spec/0016-testing.md#testinggaps--known-gaps)

### 0017 · [§release — CI, releases and repository hygiene](spec/0017-release.md)

- [§release.ci — The workflow](spec/0017-release.md#releaseci--the-workflow)
- [§release.hygiene — What the repository may contain](spec/0017-release.md#releasehygiene--what-the-repository-may-contain)

### 0018 · [§decisions — Decision log](spec/0018-decisions.md)

- [ADR-001 — No third-party adapter module door](spec/0018-decisions.md#adr-001--no-third-party-adapter-module-door)
- [ADR-002 — One crate, not a workspace](spec/0018-decisions.md#adr-002--one-crate-not-a-workspace)
- [ADR-003 — `current_thread` runtime, `Arc`/`RwLock` state](spec/0018-decisions.md#adr-003--current_thread-runtime-arcrwlock-state)
- [ADR-004 — `mongo` as an opt-in compile feature](spec/0018-decisions.md#adr-004--mongo-as-an-opt-in-compile-feature)
- [ADR-005 — The traffic ring is disk-backed](spec/0018-decisions.md#adr-005--the-traffic-ring-is-disk-backed)
- [ADR-006 — How it ships](spec/0018-decisions.md#adr-006--how-it-ships)
- [ADR-007 — No `tracing`, no `regex`](spec/0018-decisions.md#adr-007--no-tracing-no-regex)
- [ADR-008 — Job Objects replace tree-kill](spec/0018-decisions.md#adr-008--job-objects-replace-tree-kill)
- [ADR-009 — The panel is copied, never forked](spec/0018-decisions.md#adr-009--the-panel-is-copied-never-forked)
- [ADR-010 — Workspace crates, still one binary](spec/0018-decisions.md#adr-010--workspace-crates-still-one-binary)
- [ADR-011 — The terminal: WebSocket, hand-written ConPTY, no tunnels edge](spec/0018-decisions.md#adr-011--the-terminal-websocket-hand-written-conpty-no-tunnels-edge)
- [ADR-012 — MongoDB support is deleted, not feature-gated](spec/0018-decisions.md#adr-012--mongodb-support-is-deleted-not-feature-gated)
- [ADR-013 — The RustCrypto duplicate stack](spec/0018-decisions.md#adr-013--the-rustcrypto-duplicate-stack)
- [ADR-014 — The secret vault is a sealed file of its own](spec/0018-decisions.md#adr-014--the-secret-vault-is-a-sealed-file-of-its-own)
- [ADR-015 — Groups are host mechanism](spec/0018-decisions.md#adr-015--groups-are-host-mechanism)
- [ADR-016 — The Node reference build is retired; the panel is edited here](spec/0018-decisions.md#adr-016--the-node-reference-build-is-retired-the-panel-is-edited-here)
- [ADR-017 — SQL completion is computed server-side, on the leased connection](spec/0018-decisions.md#adr-017--sql-completion-is-computed-server-side-on-the-leased-connection)
- [ADR-018 — Path-space partition: `/mcp/*` is the MCP plugin's domain](spec/0018-decisions.md#adr-018--path-space-partition-mcp-is-the-mcp-plugins-domain)
- [ADR-019 — Vault references wear the `${...}` envelope](spec/0018-decisions.md#adr-019--vault-references-wear-the--envelope)
- [ADR-020 — HTTP MCP OAuth: impersonate the allowlisted client, own the token](spec/0018-decisions.md#adr-020--http-mcp-oauth-impersonate-the-allowlisted-client-own-the-token)
- [ADR-021 — The figma adapter type](spec/0018-decisions.md#adr-021--the-figma-adapter-type)
- [ADR-022 — The zai-vision type: ported native](spec/0018-decisions.md#adr-022--the-zai-vision-type-ported-native)
- [ADR-023 — The name is the service, the def is a revision](spec/0018-decisions.md#adr-023--the-name-is-the-service-the-def-is-a-revision)
- [ADR-024 — The panel is authored in TypeScript, erased to the same JS](spec/0018-decisions.md#adr-024--the-panel-is-authored-in-typescript-erased-to-the-same-js)
- [ADR-025 — The panel's gate is the check suite, not byte-equality](spec/0018-decisions.md#adr-025--the-panels-gate-is-the-check-suite-not-byte-equality)
- [ADR-026 — Data's resource navigation is object tabs](spec/0018-decisions.md#adr-026--datas-resource-navigation-is-object-tabs)
- [ADR-027 — Data's many databases: primary writable, the rest read-only, no per-database pools](spec/0018-decisions.md#adr-027--datas-many-databases-primary-writable-the-rest-read-only-no-per-database-pools)
- [ADR-028 — The integration harness: real engines in a dev-only crate](spec/0018-decisions.md#adr-028--the-integration-harness-real-engines-in-a-dev-only-crate)
- [ADR-029 — The panel's UI library lives in-tree](spec/0018-decisions.md#adr-029--the-panels-ui-library-lives-in-tree)
- [ADR-030 — MongoDB returns as an engine: the official driver, no DNS resolver, TLS per target](spec/0018-decisions.md#adr-030--mongodb-returns-as-an-engine-the-official-driver-no-dns-resolver-tls-per-target)
- [ADR-031 — Memory storage is SQLite+FTS5 through the sqlx the tree already ships](spec/0018-decisions.md#adr-031--memory-storage-is-sqlitefts5-through-the-sqlx-the-tree-already-ships)
- [ADR-032 — Memory notes are atomic claims with evidence; who wrote one is not who checked it](spec/0018-decisions.md#adr-032--memory-notes-are-atomic-claims-with-evidence-who-wrote-one-is-not-who-checked-it)
- [ADR-033 — Module-owned history: MCP's calls and traffic move into `logs/mcp.db`](spec/0018-decisions.md#adr-033--module-owned-history-mcps-calls-and-traffic-move-into-logsmcpdb)
- [ADR-034 — The module log-storage plan lands: query surface, quotas, and the panel truth](spec/0018-decisions.md#adr-034--the-module-log-storage-plan-lands-query-surface-quotas-and-the-panel-truth)
- [ADR-035 — Terminal recordings join the module-history store; .cast becomes an export](spec/0018-decisions.md#adr-035--terminal-recordings-join-the-module-history-store-cast-becomes-an-export)
- [ADR-036 — Managed apps outlive the gateway, and nothing restarts them](spec/0018-decisions.md#adr-036--managed-apps-outlive-the-gateway-and-nothing-restarts-them)
- [ADR-037 — Memory's agents are chosen by layer and pooled](spec/0018-decisions.md#adr-037--memorys-agents-are-chosen-by-layer-and-pooled)
- [ADR-038 — The window shell uses the system webview](spec/0018-decisions.md#adr-038--the-window-shell-uses-the-system-webview)
- [ADR-039 — The event bus: one WebSocket, protobuf frames, generated codecs](spec/0018-decisions.md#adr-039--the-event-bus-one-websocket-protobuf-frames-generated-codecs)

### 0019 · [§bus — The event bus](spec/0019-bus.md)

- [§bus.wire — Frames](spec/0019-bus.md#buswire--frames)
- [§bus.registry — Modules, subs and messages](spec/0019-bus.md#busregistry--modules-subs-and-messages)
- [§bus.link — Connecting, staying connected, leaving](spec/0019-bus.md#buslink--connecting-staying-connected-leaving)
- [§bus.topics — Subscribing, events and resync](spec/0019-bus.md#bustopics--subscribing-events-and-resync)
- [§bus.requests — Requests and handlers](spec/0019-bus.md#busrequests--requests-and-handlers)
- [§bus.editing — One person edits](spec/0019-bus.md#busediting--one-person-edits)
- [§bus.codegen — The generated code](spec/0019-bus.md#buscodegen--the-generated-code)
- [§bus.security — Who may connect](spec/0019-bus.md#bussecurity--who-may-connect)

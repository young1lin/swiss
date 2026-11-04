# §decisions — Decision log

Chapter 0018 of the [swiss specification](../SPEC.md).

Each entry is an architecture decision that is hard to reverse, surprising without its
context, and a real trade-off. Entries keep their `ADR-NNN` numbers forever (code cites them by
number); a new decision takes the next number. An entry states the decision in force, what it
cost, and what was rejected. A superseded entry keeps one paragraph saying what replaced it.

## ADR-001 — No third-party adapter module door

**Accepted.** The Node build let a config entry name a module to load at runtime
(`"adapter": "./my-adapter.mjs"`). Rust links statically; the options were a Node sidecar (re-imports
the memory the port removed), WASM plugins (a `wasmtime` store costs more than the whole budget),
or native `dylib` plugins (no stable ABI, in a process holding database credentials). The door is
dropped: `make_adapter` (`crates/swiss-mcp/src/adapters/mod.rs`) matches the built-in type names and
nothing else, and an unknown `type` fails at build time listing them. An external adapter becomes a
`proc` or `http` MCP. What is lost: a third-party adapter cannot appear in the Data view.

## ADR-002 — One crate, not a workspace

**Superseded by ADR-010.** What forced the revisit was not compile time but that "MCP is a plugin,
not the trunk" was a claim nothing checked.

## ADR-003 — `current_thread` runtime, `Arc`/`RwLock` state

**Accepted.** A local gateway serving one machine has no use for work stealing, and each worker
thread costs a stack plus allocator caches. The trap: `current_thread` does not permit `Rc` —
`axum::serve` spawns connections through `tokio::spawn`, which requires `Send` futures whatever the
flavour. State is therefore `Arc<RwLock<…>>`, uncontended on one thread. Do not attempt a `LocalSet`
plus a hand-rolled accept loop to recover `Rc`.

## ADR-004 — `mongo` as an opt-in compile feature

**Superseded by ADR-012** (the feature and the adapter were deleted).

## ADR-005 — The traffic ring is disk-backed

**Accepted in part.** `traffic.rs` writes a JSONL tail beside the call log and restores from it at
boot, so a restart no longer blanks the view. Reads are still served from the in-memory ring of
`KEEP = 500` entries (~1 MB resident) rather than paged off the file; the file format already
permits finishing it, which stays a deferred optimisation.

## ADR-006 — How it ships

**Accepted — GitHub Releases.** CI builds five targets and attaches one archive per target (the
binary, the licence files and the skills) plus `SHA256SUMS` to a GitHub Release on a `v*` tag
(§release.ci). There is no npm package and no npm publish step; the per-platform
`optionalDependencies` package remains possible release plumbing on top of the same artefacts.
Open: `-C target-feature=+crt-static` is not applied — the Windows binary links the CRT
dynamically. Turn it on and verify with `dumpbin /dependents` before a release goes to a machine
this repository never built on.

## ADR-007 — No `tracing`, no `regex`

**Accepted.** The logger is a hand-rolled JSON-line writer of about thirty lines. `regex` would be
imported for three patterns — `${ENV_VAR}` expansion, the secret-name wordlist, the loopback host
check — each a short hand-written scanner, and the regex engine's code segment is resident memory in
a process budgeted in single-digit megabytes. Recorded because both crates are reflexive to reach
for. (This is not a licence to hand-roll a real grammar: shell quoting, JSON, YAML and the like use
mature crates — §arch.deps.)

## ADR-008 — Job Objects replace tree-kill

**Accepted.** On Windows each `proc` child is assigned to a Job Object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the OS reaps the whole subtree when the gateway's handle
closes — crash, End Task and debugger detach included. The PID ledger stays: the job object covers
*this* gateway dying; the ledger covers a child that outlived a previous generation. The Node
build's command-line sweep for known MCP packages is gone.

## ADR-009 — The panel is copied, never forked

**Superseded by ADR-016.** While the Node build was the reference, `admin_assets/` was a
byte-for-byte copy guarded by a tree-equality test.

## ADR-010 — Workspace crates, still one binary

**Accepted. Supersedes ADR-002.** The source is a cargo workspace (§arch.crates); the product is one
static `swiss` binary. The split buys nothing at runtime, and saying otherwise is forbidden. What it
buys is that the architecture stops being a claim: `swiss-data` has no `swiss-mcp` in its manifest,
so a change that reintroduces that edge does not compile. When a change appears to need such an
edge, the host contract is missing something (the connection catalog exists for exactly that). The
cost: manifests to keep in step, and gate commands that need `--workspace` or they silently check
the root package alone. Measured: +1.1% exe size for the crate-boundary codegen, idle footprint
flat.

## ADR-011 — The terminal: WebSocket, hand-written ConPTY, no tunnels edge

**Accepted.**

- **WebSocket, not SSE.** A terminal is a bidirectional byte stream; over SSE every PTY byte would
  need base64 or JSON wrapping on the hot path, and keystrokes would need a second channel. axum's
  `ws` feature adds `tokio-tungstenite` + `tungstenite` and nothing else. Binary frames carry raw
  PTY bytes both ways; the only JSON is the resize message and the server's exit/error/stalled
  notices. Measured +662 KB exe against a +800 KB budget (~440 KB of it the vendored xterm tree,
  read-only and never paged in).
- **Hand-written ConPTY FFI, not `portable-pty`.** ~300 lines against the `windows` crate already
  linked. `open_pty` returns a cheap cloneable `PtyHandle` plus exactly one `PtyPump` on a
  `spawn_blocking` thread — that thread is why local sessions have a lower cap than remote ones.
  Measured +233 KB working set per attached idle local session (budget 1.0 MB).
- **`swiss-terminal` links no SSH and has no edge to `swiss-tunnels`.** Remote shells arrive through
  the `ssh-shell` capability seat in `swiss-host`: tunnels registers a provider on start and
  withdraws it on stop, and the terminal dials through the tunnel manager's own refcount. The
  plugin's `requires` stays empty so local shells work on a machine with no tunnels; when tunnels
  is absent, `/api/terminal/targets` says so by name. Marginal remote session cost measured at
  ~251 KB (a record, not a gate).

## ADR-012 — MongoDB support is deleted, not feature-gated

**Superseded by ADR-030.** Accepted at the time; it superseded ADR-004. No user, and the heaviest driver in the set (an estimated 3–5 MB).
Deleted whole: the cargo feature, the `mongodb` dependency, the adapter and its resources, the
`MongoBrowser` trait and flavour, its `/api/db` routes and its admin form column. A `mongo`-typed
MCP fails like any other unknown type. The build is plain `cargo build --release`, and the gates run
one feature combination.

## ADR-013 — The RustCrypto duplicate stack

**Accepted.** Every direct crypto dependency of ours sits on the newest generation (aes-gcm 0.11,
sha2 0.11, hkdf 0.13; rand 0.10, whose OS RNG is getrandom's `SysRng` — a CSPRNG error is a broken
machine and is expected away). The sealed envelope is a byte-level contract (§core.sealed) and
the committed fixture proves the primitives did not move. The duplicates that remain are pinned
by others and named rather than fought: the older RustCrypto/rand generation by sqlx, base64 by
axum, older `windows` by russh/rmcp, and proc-macro-only pairs that cost nothing in the binary.
Verify any change with `cargo tree -d -e normal,build` and `cargo tree -i`.

## ADR-014 — The secret vault is a sealed file of its own

**Accepted; its reference-syntax clause is superseded by ADR-019.** Spec: §host.vault.
The env store doubles as every child's environment, so a key stored there is readable by every
proc MCP, job and shell, and a missing `${ENV}` resolves to empty — the wrong failure for a
credential. The vault is `secrets.json`, sealed like every state file:

- names are lowercase kebab `[a-z][a-z0-9-]{0,63}` — a different character class from
  `${UPPER_SNAKE}`;
- references resolve at every use point through one resolver shared with env refs; a missing
  name is a hard refusal naming the surface and the reference (never a value);
- write-only: no API reads a value back;
- nothing merges the vault into a child environment — the only exit is substitution at a use point;
- mutations are rev-checked (stale rev → 409 with both numbers, missing rev → 400);
- `export` carries the vault and `import` merges it;
- host-owned, not a plugin: every plugin may depend on it.

## ADR-015 — Groups are host mechanism

**Accepted.** Spec: §host.groups. `swiss-host` owns one `Groups` type with the rules, a
`GroupScope` registry each scope registers into (a six-method trait), and one route family,
`/api/groups/{scope}`. The panel renders every grouped list through one component. Rejected:
per-plugin grouping (six diverging copies), and unifying only the panel (the server sides already
differed). The per-scope group routes that predate the family are retired. Secrets and tokens refuse
the family's `order` verb (they list by name and creation order).

## ADR-016 — The Node reference build is retired; the panel is edited here

**Accepted. Supersedes ADR-009.** `crates/swiss-panel/` is the panel's source of truth. What
survives: the panel is served straight from embedded assets with no bundler; the panel's code is
the spec for the admin API (every `/api/*` shape change ships on both sides in one commit); the
panel's test suite lives in this repository; the sealed-envelope format stays frozen with its
committed fixture.

## ADR-017 — SQL completion is computed server-side, on the leased connection

**Accepted.** Spec: §data.completion. Completion is a per-keystroke service, not a dataset: the
rejected alternative walks the whole catalog into the browser before the first keystroke. One
route, one lease: `POST /api/db/{name}/completion {sql, caret}`. Candidates fold most-specific
first; the cache lives with the connection, loads lazily, serves for ten minutes, drops on DDL, and
is capped (a wide schema degrades to keywords + tables instead of growing without bound). Redis is
excluded.

## ADR-018 — Path-space partition: `/mcp/*` is the MCP plugin's domain

**Accepted.** Spec: §mcp.endpoint. `/mcp/<name>` is the only MCP endpoint shape. Hard cutover, no
alias: a root single-segment POST answers 404 with a "moved to /mcp/<name>" hint when the name is a
registered MCP. There are no host-reserved words inside `/mcp/`; the root belongs to host chrome
(`/`, `/admin`, `/health`, `/api/*`) and to whatever a future plugin claims. Rejected: a permanent
or deprecation-window root alias, and a growing reserved-name list. The partition deletes a rule
instead of growing one.

## ADR-019 — Vault references wear the `${...}` envelope

**Accepted. Supersedes ADR-014's reference-syntax clause.** Spec: §host.refs. A bare scheme
(`secret://name`) has no token boundary inside URLs, headers and command lines — the scanner
claimed `secret://aaa` inside `https://test.com/secret://aaa/test`. One envelope, two families:
`${UPPER_SNAKE}` stays lenient, `${secret://kebab}` hard-fails; outside `${...}` there are no
references. A malformed name inside the envelope refuses rather than guessing. Legacy whole-value
bare refs are rewritten in memory at each loader with one boot note per file; mixed strings are not
auto-migrated.

## ADR-020 — HTTP MCP OAuth: impersonate the allowlisted client, own the token

**Accepted.** Spec: §mcp.oauth.

- Providers whose dynamic client registration admits only allowlisted `client_name`s are answered
  with a default client name per provider; `oauthClientName` overrides it. Nothing else lies.
- Endpoints are discovered, never hard-coded (RFC 9728 → RFC 8414).
- Grants live in their own sealed file, `mcp-oauth.json`, keyed by MCP name — not the vault;
  export/import carries them.
- The def surface is `auth: "oauth"` plus the optional `oauthClientName`; a hand-written
  Authorization header alongside is refused.
- `POST /api/mcps/{name}/authorize` single-flights a flow per name. The 401 path refreshes once,
  retries once, then clears the grant and shows "needs authorization" instead of looping.
- The single-flight dedups on the refused token, not on freshness.
- No new dependency; the loopback callback is one ephemeral listener per flow.

Rejected: a general provider UI, the device flow, token reveal, auto-reauthorize.

## ADR-021 — The figma adapter type

**Accepted.** `type: "figma"` is sugar over http + OAuth: its def is a name and an optional
description and proxy, and `make_adapter` expands it to the full http def. The stored def
refuses `url`, `auth`, `oauthClientName` and `headers`. One predicate, `is_oauth(def)`, answers
"is this an OAuth MCP". The row reports the def type, not the adapter kind. The form has no
Test button. Rejected: a panel preset (a saved URL the operator never chose) and a generic
provider registry (one provider does not justify a table — the second one reopens this ADR).

*Amended 2026-10-21:* `proxy` moved from refused to accepted. The type decides what Figma
requires (endpoint, credential mode, client name); the route to Figma is the operator's
network, and in a region that cannot reach Figma directly the type was unusable without
rewriting it as a hand-built http def. The authorize flow, which had ignored every def's
proxy, now rides it too. Rejected with it: a central proxy settings page that each place
picks from. Every outbound place already names its own proxy (http, rest, zai-vision and
figma MCPs, LLM providers, SSH connections) with dialects that differ (CONNECT only; socks5;
credentials in fields), one `${secret://...}` reference already lets many places share one
value, and the system proxy is the machine-wide switch; a page of named proxies would be a
second way to say what each field says.

## ADR-022 — The zai-vision type: ported native

**Accepted.** The upstream vision MCP is eight tools, each a fixed system prompt plus one
multimodal chat-completions call, so it is an `Engine` compiled into the binary
(`adapters/zai.rs`) instead of a Node child. Parity is the contract: tool names and prompts are
byte-identical, the prompts extracted verbatim by `scripts/extract-zai-prompts.js`; descriptions
and schemas match upstream except where the gateway is not the client's child (§mcp.zai:
absolute local paths, video formats, strict schemas), and retries skip non-429 4xx. The key is
always a `${...}` reference (a literal is refused). No ping and no Test button: a metered endpoint
is never probed. The def names only `mode` or `baseUrl`, and optional `model`, `proxy`,
`timeoutMs`.

## ADR-023 — The name is the service, the def is a revision

**Accepted.** Spec: §mcp.revisions. Two live same-named defs would contend for name-keyed state
(OAuth grants, call log, groups, tunnel links), so a replaced def is parked as an inert revision
(managed.json `revisions`, at most five per name). A revision is never registered, started or
resolved. Replace builds first, so a bad def changes nothing; a def that builds but will not start
is a 200 with `restartError`. Restore takes before it parks. A def swap is not a start. Rename
carries revisions; delete clears them. Rejected: a disabled-shadow registry.

## ADR-024 — The panel is authored in TypeScript, erased to the same JS

**Accepted.** Spec: §panel.toolchain. Source is `crates/swiss-panel/panel/src/*.ts`; ts-blank-space
blanks type-only syntax line by line, so each emitted `.js` line keeps its `.ts` line number. The
emit is committed under `admin_assets/js`: the repo builds with cargo alone, node is a dev-only
dependency. Rejected: a bundler (a build step and a second artefact), in-browser erasure, staying on
hand-written JS.

## ADR-025 — The panel's gate is the check suite, not byte-equality

**Accepted.** The early rule that emitted JS stay byte-identical to the pre-port JS forced bad
TypeScript and is revoked. The gate is `npm run check` (typecheck ×2 + lint + emit freshness +
vitest), per-file test coverage for rewritten modules, and the live walk on 19998. The machine gate
is the eslint ratchet (§panel.lint): no-var, prefer-const, no-non-null-assertion (shrinking
whitelist), consistent-type-imports, and a zero-allowlist ban on any `.innerHTML =` write.

## ADR-026 — Data's resource navigation is object tabs

**Accepted, amended.** Spec: §data.tabs. The Data page holds several objects as a tab strip — L3
resource navigation made visible, not a new layer — with a third tab shape (card + glyph + ×).
Costs: a tab cap (eight when accepted, twelve today — §data.tabs), a background tab's rows are
dropped and re-fetched on activation, a dirty tab is never evicted, and the navigation guard asks
about every tab. Opening an object when every tab is dirty is refused. Amendment: the strip's `+` force-opens a new SQL tab even then (going
over the cap beats refusing an explicit open). Rejected: one object with a split toolbar, a bottom
dock, a command palette.

## ADR-027 — Data's many databases: primary writable, the rest read-only, no per-database pools

**Accepted.** Spec: §data.databases. MySQL browses same-instance databases by qualifying names
(free); Postgres lists other databases but disables them (a connection is bound to one database);
Redis reads its keyspace from INFO. Secondary databases are read-only, period: the configured
database is the connection's write boundary. Rejected: one database per connection, and a pool per
database. A proxy that refuses `CLIENT INFO` falls back to db0 rather than failing the catalog.

## ADR-028 — The integration harness: real engines in a dev-only crate

**Accepted.** Spec: §testing.it. `crates/swiss-it` (feature `it`, default off) runs real MySQL,
Postgres and Redis through testcontainers (or `SWISS_IT_*_URL`), restores per-test databases from
the repo's seeds, and drives L1 browser tests, L2 adapter tests through a real rmcp client on a
real listener, and an L3 proc group over the repo's own stdio server. Rejected: sqlx-layer mocks
(the exact-string BIGINT, completion casing and udt_name gaps were invisible to them) and a
developer-installed local engine (a missing engine would have to skip — a silent green). Costs: the
dev graph grows testcontainers/bollard (never shipped; the CI duplicate check gates
`-e normal,build`), gate 2 is mandatory on DB-path diffs, and the seeds are a maintained surface.
A machine without Docker fails gate 2 with setup copy, never skips.

## ADR-029 — The panel's UI library lives in-tree

**Accepted.** Spec: §panel.ui. `panel/src/ui/*.ts` components plus one `ui.css` that owns their
classes, with ratchet gates in `npm run check`. `ui/` may import only `h.ts`, `i18n.ts` and itself;
`views.css` may not style a class `ui.css` owns; font weights are four tokens; the hidden gallery
page `/admin/ui.html` renders every component, and design mockups are gallery scenes built from
the real components. Rejected: rules-only (the drift it produced was measured), a separate package
(needs a bundler and breaks "cargo builds without node"), a second embed crate, a third-party
component library.

## ADR-030 — MongoDB returns as an engine: the official driver, no DNS resolver, TLS per target

**Accepted. Supersedes ADR-012.** Spec: §mcp.db, §data.mongo. A user asked for it, and the
weight ADR-012 estimated (3–5 MB) measures at **+2,295,112 bytes** (13,995,880 → 16,290,992;
Linux x86-64, the release profile, stripped) — +16%, for an engine with six tools, resources and
a documents workspace on the Data page. The driver is `mongodb` 3.9 with default features off:
the wire protocol, SCRAM, server discovery and sessions are exactly the grammar §arch.deps forbids
hand-rolling. Costs, each chosen:

- **No `dns-resolver`.** hickory is a whole resolver stack for one URL scheme: +443,560 bytes
  more (16,734,552) for `mongodb+srv://`. A seed list names the same hosts; an SRV URL fails in
  the driver's own words, and the form's hint says to list the hosts.
- **TLS per target.** The driver has no native-tls backend, so it takes the rustls the target
  already links — aws-lc-rs beside reqwest's off Windows, ring beside russh's on Windows. On
  Windows that is rustls next to schannel, the one declared second TLS layer (§arch.exceptions).
- **One shared client per connection** (pool 4, idle 60 s, built lazily, §product.tactics), so
  sessions, cursors and transactions never span calls: the guard refuses them, and the browser
  opens its own transaction only inside one edit batch.
- **Every value crosses as Extended JSON** — canonical on the browser seam, relaxed in tool
  replies — so the panel prints and reads the shell's syntax without one lossy conversion.

Rejected: a feature flag (ADR-004's two builds and two gate matrices, which ADR-012 ended), a
hand-rolled wire client, `openssl`, and the standalone-only gate (transactions exist only on a
replica set, so the integration engine is one).

## ADR-031 — Memory storage is SQLite+FTS5 through the sqlx the tree already ships

**Accepted.** Spec: §memory. The native Memory plugin's store is one SQLite database at
`<SWISS_HOME>/memory/store.sqlite3` (WAL, `foreign_keys=ON`, 5 s busy timeout), reached through
the sqlx 0.9 the DB adapters already link, with the `sqlite` feature added: the marginal shipped
graph is `sqlx-sqlite` + `libsqlite3-sys` (bundled C, FTS5 verified present). The Phase 0 A/B
spike (a standalone harness, since deleted, seeded with 10,000 records over 10 projects in
Chinese and English plus ~28,000 revisions, scored by 112 hand-labelled queries) measured, on
Windows 11 with a WSL Ubuntu leg for correctness and concurrency:

| Criterion (the spike's weights) | SQLite+FTS5 | redb 4.3.0 |
|---|---|---|
| Atomic CAS revise, crash-free reopen, export | pass | pass |
| Recall@5 / absent / leak / exclusion violations | 1.0000 / 0 / 0 / 0 | 1.0000 / 0 / 0 / 0 (after its token index moved to AND semantics) |
| Two independent processes on one home (mandatory) | **pass** — both writers keep every row, cross-process CAS succeeds | **fail** — second open: "Database already open. Cannot acquire lock", Windows and Linux alike |
| Search p50/p95 (release; substring tier always on) | 31.7 / 292 ms | 6.6 / 8.1 ms |
| Create / revise / source insert p50 (release) | 0.32 / 0.31 / 0.13 ms | 2.95 / 1.76 / 0.94 ms |
| Open + count, 10k rows | 9 ms | 19 ms |
| Binary delta over a no-engine baseline | +1,725,952 B | +1,005,056 B |
| Idle working set, DB open (median of 3) | ~22.5 MB | ~31 MB |

redb fails a non-negotiable criterion — the CLI must work on the same home while the gateway
holds the store open (§memory.cli) — and its search quality needed a hand-maintained token index
standing in for FTS, so the decision rule selects SQLite on correctness and concurrency alone.
SQLite's p95 search tail is the always-on LIKE substring scan; the shipped path runs the
substring tier only when the lexical tiers return nothing (§memory.search), so the tail does not
ship. One production rule the spike paid for: **mutating transactions run inside
`BEGIN IMMEDIATE`** — sqlx pools open `BEGIN` (deferred), and a deferred read-then-upgrade
returns `SQLITE_BUSY` on a stale snapshot that no busy timeout can retry; the WSL leg caught it
live. If the dependency gate ever objects to `sqlx-sqlite` specifics, `rusqlite` (bundled,
FTS5) is the named fallback with a blocking-IO thread; rejected outright: shipping both engines,
a lock-manager sidecar to fake redb's multiprocess story, and any per-project database file.

## ADR-032 — Memory notes are atomic claims with evidence; who wrote one is not who checked it

**Accepted; landed in seven phases, the sixth in part.** Spec: §memory.model, §memory.capture, §memory.embedding,
§memory.evaluation. An audit of the first large bootstrap — DeepSeek Harness at commit
`21638c5631`, 185 leaf domains, 1,995 notes, 4,305 links, written by a headless agent through the
CLI — and of the code behind it found four faults, each with a measured cost:

- **Formation.** The bootstrap prompt asked for 6 to 12 notes per domain, 25 to 80 lines each, in
  five fixed sections (What happens / Why / Code path / Example / When you change this) with a
  3-to-10-hop code path. All 1,995 notes follow the template; the mean is 3,220 characters (p10
  2,554, p90 3,999) and none is under 1,500; 1,965 say always, never, only, every, exactly or
  all. A 57-note sample checked against the code held 714 claims (12.5 a note): 94.8% correct,
  4.3% partly, 0.8% wrong — yet only 29 of the 57 notes (51%) were wholly correct. The errors are
  the global claims (a count, `every`, `only` with an exception), the wrong file for a quote or
  symbol, an inverted direction, and a test cited for behaviour it never calls: the code path
  and the example retell code an agent can read, and every retold line is a claim that can rot.
- **Provenance.** The CLI writes `origin=user, actor=cli`, and the pipeline then confirmed every
  note: all 1,995 read `user / cli / confirmed`. One flag stood for both "a person accepted it"
  and "the source backs it", and nothing recorded what a note was derived from.
- **Embedding.** The fingerprint (`provider:model:dims`) leaves out the endpoint, so with
  `dimensions` unset two endpoints serving different widths under one model name share one
  index; a width mismatch made `cosine` return 0, so a stale index ranked nothing without a
  word; the status read the width of one row.
- **Evaluation.** Search recall@5 and MRR only: no brief, no wrong-domain, stale or unsupported
  measure, and no record of the embedding setup a number was taken under.

The fix keeps the design — one binary, one gateway, one `/mcp/memory`, the project resolved per
request, SQLite+FTS5 (ADR-031), CLI, MCP and panel over one `MemoryService` — and lands in
phases, each its own commit with its tests first:

1. **Formation** (landed: §memory.capture). "Code is ground truth; memory is not code documentation." A note is one
   central claim, a few facts, why (and why not), and its evidence — typically 300 to 900
   characters; zero notes is a valid answer. Decisions, constraints, pitfalls, rationale,
   non-obvious behaviour, business invariants, compatibility rules and procedures qualify;
   what a reader derives mechanically from the code does not. Global words and counts only when
   mechanically checked, else scoped ("observed", "in the inspected scope"). The rules live in
   swiss-memory and both the sync prompt and the bootstrap read them.
2. **Provenance and evidence** (landed: §memory.model, §memory.capture, §memory.store). The producer (human, agent, bootstrap, import, sync) is apart
   from the transport (cli, mcp, panel, internal); `user_confirmed` is apart from
   `source_verified`. `memory_evidence` binds a source (human, session, code, commit, test, doc,
   bootstrap, import — path, symbol, commit, session, run, source and excerpt hashes; never a
   line number) to one revision of a note as `derived_from`, `supports` or `verified_by`. The
   migration keeps every row and derives what the old columns prove, nothing more.
3. **Embedding** (landed: §memory.embedding). The fingerprint names the endpoint (conservative: a moved endpoint
   re-embeds); one fingerprint holds one width; a mismatch marks the index stale, leaves the
   incompatible vectors out and re-indexes — never a silent 0.
4. **Evaluation** (landed: §memory.evaluation, §memory.search). Lexical, hybrid, `brief(task)` and `brief(task + paths)` modes; recall@1/3/5,
   MRR@5 and the wrong-domain, unsupported, stale and fully-correct context rates; the
   embedding setup recorded with every report; path, domain and symbol affinity in ranking so
   an exact identifier beats a semantic neighbour.
5. **Code index** (landed: §memory.code). A derived, rebuildable map of files, symbols, definitions, imports and
   references, keyed by file hash, deletable without touching a note, used at formation to
   hold for review a claim naming a symbol or file that is not there. Its own
   `code.sqlite3`, never the graph. Definitions are a keyword scan, not a parser: measured on a
   121 KB base binary, `regex` adds 587 KB, and tree-sitter with the TypeScript, TSX, Rust and
   Python grammars adds 4.47 MB - a third of the 13.6 MB gateway - for definitions the scan
   finds on the lines that declare them. A note is held, not refused: a name can be right and
   the index behind (or cut), and a person decides.
6. **The same bootstrap again** (landed in part). Same commit, the first run's map and hubs
   (copied, so the notes are the only variable), the rules of `swiss memory rules --bootstrap`,
   the same model through the same runner, saved with `--producer bootstrap` and code evidence
   and never confirmed - over 48 of the 176 leaves, in 11 of the 26 areas (28 leaves one to a run,
   20 four of one area to a run). Old and new notes of those 48 leaves, side by side, and 24 of
   each checked against the code by the audit's prompt in one run:
   - notes 553 → 198 (11.5 → 4.1 a leaf, one leaf none); mean 3,338 → 728 characters (p90
     4,081 → 902); 1.85 M → 144 k characters in all; sectioned notes 553 → 0; evidence rows 0 → 696;
     8 held for review by the code check.
   - claims a note 12.2 → 7.4; correct 94.5% → 92.7% (4 of the new unverifiable); wrong or
     partly a claim 5.5% → 5.1%; wholly correct notes 12 of 24 (50%) → 18 of 24 (75%), Fisher
     p = 0.14 at this size. The old sample repeats the audit (12.5 a note, 94.8%, 51%). The gain
     is fewer claims a note, not fewer errors a claim; what the new notes still get wrong is the
     global claim (`exactly two`, `every other`, one said to be checked by grep) - the rule is
     stated and not yet kept.
   - a leaf took 527 s and 43.5k output tokens before (its hub written too), 412 s and 23.7k
     one to a run, 187 s and 11.4k four to a run - which wrote 2.6 notes a leaf against 5.3.
   - one vector a note (4 KB at 1,024 dimensions): 2.3 MB → 0.8 MB for these leaves. The code
     index of the checkout: 13,933 files listed, 19.7 MB, built in 24 s, refreshed in 0.7 s.

   Not measured: MRR and recall@5 over a whole new corpus (128 leaves were not run), so not
   whether the shorter corpus still answers what the longer one did; a whole corpus's store
   size and bootstrap time.
7. **The rules kept by the save** (landed: §memory.capture, §memory.code). Phase 6's wrong
   claims were rules stated and not kept, so the save keeps them; a model writes the notes,
   fixes what the check finds and is never asked to vouch, and a person directs and never
   reviews note by note.
   - **Quotes.** Evidence carries the line or two a fact rests on, copied letter for letter;
     the save reads the file and holds a note whose quote is not there (`excerpt_mismatch`) or
     whose file is not (`missing_evidence`).
   - **Claims over a set.** A count, a closed set, every other and most are re-counted against
     the code index when the note names the set and what its members hold, held when the count
     differs (`count_mismatch`) and held when nothing names what to count and nothing scopes
     it (`unscoped_quantifier`). The lint's `global` hint, which flagged every always and never,
     becomes `quantifier`, about claims over a set only.
   - **Fewer claims.** One central claim and at most three facts behind it: every claim is one
     more thing to get wrong.
   - **The loop closed.** An edit is checked like a save and releases a note it fixes; a
     verified mark by a model does not release a note the check holds.

Rejected: a vector or graph database (Qdrant, LanceDB, Neo4j), GraphRAG, a Python or Node
service, and a per-project endpoint — each buys less than the one-file store it would replace
costs.
## ADR-033 — Module-owned history: MCP's calls and traffic move into `logs/mcp.db`

**Accepted. Supersedes the storage half of ADR-005 (the traffic ring's file authority); the
ring itself stays as the live window.** Spec: §host.history, §mcp.logs, §mcp.calls,
§mcp.traffic. The MCP plugin opens one SQLite database for its whole module — call index rows,
sealed body chunks, every traffic row and the client dimension — through the host's
module-history store (engine gate, single-connection pool, transactional schema, bounded
writer queue, generations). What decided it:

- **The ring lied by omission.** Five hundred rows, then silence: a restart trimmed history
  the panel still claimed to show, and a busy day rolled a slow client's evidence off the
  end. The archive has no row ceiling; the age sweep is the only retention.
- **Two files per concern was two formats to keep honest.** The preview/flags/sequence wire
  shapes survive byte-for-byte; a clear resets the wire sequence (it always did) while
  internal row identity never reuses a number.
- **The legacy tree is imported, never destroyed.** First open folds `logs/calls/*.jsonl`,
  the body files and `logs/traffic.jsonl` in idempotently (natural-key `INSERT OR IGNORE`,
  import state in the database's meta table), counts torn tails, and leaves every source file
  in place: retiring the backup is the operator's explicit call after the cutover is verified.
- **Disable means zero.** Construction opens nothing; the database opens on the plugin's
  start, detaches and closes bounded on its stop, and a failed open degrades to the jsonl
  fallback instead of failing the module.

Costs, each chosen: one SQLite file more per home (WAL mode, one connection); traffic reads
cross a bounded queue barrier instead of a lock; and byte budgets become physical accounting
over the whole module, which the retention phase owns.


## ADR-034 — The module log-storage plan lands: query surface, quotas, and the panel truth

**Accepted. Completes the delivery ADR-033 started (plan `docs/plans/module-log-storage.md`
D0-D7).** Spec: §host.history, §remote.history, §mcp.calls, §panel deep-search clause. What
the landing added and why:

- **One engine, two databases, zero cross-module edges.** MCP and Remote each own their
  SQLite module store through the same host engine (gate, generations, sealed-body
  encryption, bounded writer queue); neither subsystem crate knows the other exists, and
  the cross-module query is a host-owned seat the plugins register adapters into — never
  a dependency between peers (§host.plugins).
- **Literal search with honest cursors.** The scan engine folds needle and decoded
  haystack after decrypt, carries chunk-tail state in the master-key-signed cursor, and
  anchors pages at the chunk that filled them. An exhausted budget with zero hits is an
  INCOMPLETE page with a Continue — the panel may never paint it as "no matches", and
  only a complete zero-hit scan may (Q3, P1).
- **Bodies are served bounded, never guessed.** `/api/history/{module}/records/{id}/body`
  hands out slices with a frontier; a pruned body answers `gone`; a stopped module is
  named unavailable. The panel's deep searches in Calls and Remote Runs ride exactly this
  surface — Traffic stays on its server-side row filter because upstream capture is
  character-clipped and a deep panel would promise text the store never kept (plan §9).
- **Physical quotas are per module.** Declared shares, sidecars and in-flight work
  counted, protected metadata with honest overrun, age sweeps as the only retention
  lever (§host.history quotas).
- **Disable isolation is proven, not asserted.** L6 pins both directions: disabling MCP
  keeps Remote's store live and the jobs history writing; disabling Remote keeps MCP's
  searchable in full — skipped-by-name, never an error, never a rescan.

Costs, each chosen: the cross-module fan-out is one HTTP round trip per page (bounded
work per module); sealed-body search decrypts per chunk and pays the fold (measured in
the working set: +1.6 MB over the cold-boot record with the stores open over recorded
bodies — the 2026-10-09 §product.memory row); and the panel carries three honest states
instead of one empty string.


## ADR-035 — Terminal recordings join the module-history store; .cast becomes an export

**Accepted. Extends ADR-033/034 to the third log-shaped owner.** Spec: §terminal.recording,
§host.history, §core.home. One hundred six `.cast` files and 13 MB sat in
`~/.swiss/terminal/` with no retention at all — the exact pile ADR-033 retired for MCP. What
decided the shape:

- **The event lines are the stored bytes.** A part's plaintext is the verbatim asciicast
  `[elapsed, "o", text]` lines, so the export route reconstructs a byte-faithful `.cast`
  (header + lines) and `asciinema play` keeps working; timing never round-trips through a
  second format. The recorder becomes a line-buffering writer: at most 64 KiB in flight, one
  sealed part per flush, the session's own 8 MB cap unchanged.
- **ADR-033's import contract applies verbatim.** First open folds every legacy `.cast`
  idempotently (natural key = session id, `INSERT OR IGNORE`; import state in meta; torn
  tails counted, never fatal), and the source files stay in place — retirement is the
  operator's explicit call.
- **The identity surface is the host's, not a new one.** Sessions carry an integer row id so
  the generic body route (`/api/history/{module}/records/{id}/body`) and the deep search
  fan-out serve terminal recordings with zero new host SQL; the `.cast` reconstruction rides
  the same reader. The terminal module registers on start and withdraws on stop like its
  peers (§host.history lifecycle).
- **Quota and retention are declared, not invented.** Share 100,000,000 bytes — the split
  still sums to the 600,000,000 budget — with a 90-day sweep (a recording's value decays
  faster than a call log's 180); the 8 MB per-session cap bounds the pathological case either
  way. An over-share denies NEW recordings honestly; a live session keeps running, recording
  degrades to the same stopped-with-marker state the cap already used.
- **Disable means zero, again.** Construction opens nothing; the database opens on the
  plugin's start and closes bounded on its stop; a failed open costs the recording, never the
  terminal (the existing degrade-to-warn rule).

Costs, each chosen: one SQLite file more per home; recording writes cross the bounded queue
(batched 64 KiB parts, so the queue sees parts, not PTY reads); and the panel gains no new
surface yet — the recordings browser stays roadmap (§terminal.roadmap) until the export
route proves itself live.
## ADR-036 — Managed apps outlive the gateway, and nothing restarts them

**Accepted.** Spec: §process.apps, §process.detached. The process plugin starts and stops
outside programs the operator declares (cc-otel first). They are spawned **detached** — no
kill-on-close job, no PID ledger entry, no reap, a capture file instead of a pipe — and
identified on demand from the listener table (port owner + image) or from a record of pid +
creation time. Nothing watches them: status is read when someone asks, and a crash leaves the
app stopped until the operator starts it again.

Costs, each chosen: "killing swiss leaves no child anywhere" (§process.tree) stops being
universal — apps are the declared exception; a Stop without the app's own stop command is a
hard kill; a crashed app stays down; a self-daemonizing launcher without declared ports cannot
be tracked at all.

Rejected:

- **Apps as runs** through the supervisor and the run pool. The job object kills a launcher's
  daemon the moment the launch succeeds, and resident apps would hold pool slots.
- **Windows services** (NSSM, `sc.exe`). A service account loses the user's home and PATH that
  these programs resolve against.
- **Trusting pid files.** Both failures were observed live on 2026-08-18: a stale file naming
  a dead process, and a serving process with no file.
- **Command strings.** `start`/`stop` are typed argv (§process.actions).
- **The `svc` watchdog** (desired state per app, a periodic ensure loop, crash-loop backoff,
  a home-wide lease so two daemons never race). It costs more than a crashed local tool does
  today. §process.roadmap records how it is added when that changes.

## ADR-037 — Memory's agents are chosen by layer and pooled

**Accepted.** Spec: §memory.settings. A memory run used to ask the one agent the sync block
named, and only when it named one. Now the workers and the lead are each decided by the first
layer that says anything — the run's own request, the project's settings, the sync block, and
(for the workers alone) the agent added last — and several workers form a pool that routes
every question to the member with the most room, fails a question over once to another member,
and can be named task by task by a lead.

Surprising enough to record: **an absent `sync.agent` changed meaning** from "off" to
"inherit", so an untouched block that silently asked nobody now asks the newest agent — the
nudge excepted (it asks only an agent someone named, and declines with `no-agent` rather than
spend a surprise model run), and the lead excepted (never a fallback, so a lead is never a
surprise). A lead that could delegate to one worker now sees a roster when there are several.

Costs, each chosen: one SQLite table more (`memory_project_settings`, machine bookkeeping,
never exported); the block editor must offer inherit / off / names (the panel fix is its own
change); a pool's fail-over doubles a failed question's worst-case cost, bounded by one retry.

Rejected:

- **A single `agents` list field on the block alone.** It cannot express "this project asks
  its own, that one inherits", and a project switch would edit the machine-wide block.
- **Round-robin or hash routing in the pool.** Both ignore that members differ in
  `concurrency`; most-room keeps every member at its own pace without a scheduler.
- **The lead as a pool, or a worker leading itself.** A lead that delegates into itself has
  neither a second judgement nor a deadlock-free tool loop; the lead stays one named agent.
- **Falling back to the newest agent for nudges and leads.** A session's end would silently
  spend whatever agent was added last - exactly the surprise the nudge's opt-in exists to
  prevent.

## ADR-038 — The window shell uses the system webview

**Accepted.** Spec: §host.gui. The panel needs a window of its own on Windows and macOS without giving
up the one-static-exe release. `swiss gui` adds tao (loop, window), wry (the WebView2 surface)
and tray-icon — three small crates, no bundler, no second engine. The system WebView2 on
Windows and WKWebView on macOS render the same embedded panel. The owner's macOS `.app`
request, 2026-10-30, extends the existing shell rather than adding a desktop framework or
browser engine. Linux stays headless. macOS packaging uses an ad-hoc signature for local
previews; trusted distribution still requires Developer ID signing and Apple notarization.

Surprising enough to record: **the first cut's single-instance mutex was desktop-wide**, so a
second `swiss gui` under another `SWISS_HOME` exited silently while the first window — hidden
in the tray, because closing hides — kept showing the old gateway's panel, reading exactly
like a refused config switch (2026-10-30, the owner's report). The claim is now named by port
and home: one window per gateway, two homes two windows, and the losing launcher for the same
gateway raises the standing window through a named event instead of doing nothing. The brand
mark was a plain red tile for that one day — drawn from `assets/logo.svg`'s own geometry now,
an SVG rasterizer in the build being the step still deferred.

Rejected:

- **Electron, with or without a Rust sidecar.** The gateway already IS the sidecar; an
  Electron shell buys a second Chromium and an update channel to ship a webview the OS
  provides.
- **GPUI.** Cap's migration case is a video-pipeline UI; this panel is text and tables, and
  GPUI means rewriting every view off the web platform for one target.
- **Slint.** A DSL and a license triple (Royalty-free / GPL / Commercial) against a panel
  that already exists in HTML.
- **Electrobun 2.x.** Its default Windows webview is WebView2 — the same engine, a new
  toolchain (Hutch) and a days-old 2.x; only its CEF mode differs, at hundreds of MB.
- **An installer, MSIX or auto-update.** The release archive stays the channel (§host.ops):
  `update` checks, the exe swaps by hand.

## ADR-039 — The event bus: one WebSocket, protobuf frames, generated codecs

**Accepted.** Spec: §bus. Pages kept other windows' changes out of sight until a reload or a
focus, and every live view polled on its own timer. One socket per page now carries requests,
events and relays; module numbers are chapter numbers; the codecs are generated from one
`.proto` set; writes go over the bus from the first delivery because nothing is released yet.
Single-user editing is the product's shape, so concurrent edits are refused by revision, not
merged; a merge, if ever, is three-way.

Rejected:

- **SSE for events plus HTTP for writes.** Two transports, and no page-to-page path for relays.
- **JSON frames.** No schema, no generated types, and every new message hand-checked twice.
- **A hand-written codec.** A grammar written here (§arch.deps); the owner chose generation.
- **protoc, or prost-build in a build script.** A system binary, or the generator in every
  build's graph; protox outside the workspace needs neither.
- **ts-proto, protobuf.js.** ts-proto emits `enum` (not erasable) and needs protoc or buf
  anyway; protobuf.js's static output is a CommonJS-era runtime.
- **OT or CRDT (Yjs, Automerge).** They solve two people typing at once, which this product
  does not have.
- **The terminal's socket.** A per-session byte pump with its own tickets; unrelated.

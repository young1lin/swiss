# What the plugins share

Part 03 of [§host](README.md), chapter 0004 of the [swiss specification](../../SPEC.md).

## §host.groups — Groups

One model (`swiss_host::groups`, ADR-015), seven scopes: `mcps`, `tokens`, `conns`, `rules`,
`jobs`, `secrets`, `targets`. Each owner registers its scope in the `GroupScopes` table; the
host routes by the scope string with no match arm.

**Invariants** (unit-tested once, in `groups.rs`):

1. The name list is never empty; replacing it with `[]` is 400 "at least one group must
   remain".
2. Names are trimmed, non-empty, ≤ 64 characters, unique case-insensitively; members store
   the canonical casing.
3. A member with no explicit entry, or whose group is gone, renders in the **first group** —
   the slot, not the name `default`, is the sink.
4. Replacing the list deletes omitted groups (`default` included); their members fall to the
   new first group.
5. Rename keeps the slot and carries its members; a collision is 400.
6. Reordering never moves a member: if the old first group survives but is demoted, members
   that rendered there only by default are first pinned to it (in the new list's spelling).
7. Order is not part of `Groups`: each scope keeps its own flat order (the MCP `order`, the
   tunnel arrays, the job array, the vault's `order`, the target list; tokens by creation
   time), sliced by group. Moving a member between groups never rewrites the ordering.

**API** (identical shapes for every scope; reads ride each scope's own list, which carries
top-level `groups` and a per-row `group`):

| Method | Path | Body | 200 |
|---|---|---|---|
| PUT | `/api/groups/{scope}` | `{groups: [names]}` | `{groups}` |
| POST | `/api/groups/{scope}/rename` | `{from, to}` | `{groups, moved}` |
| PUT | `/api/groups/{scope}/members/{id}` | `{group: name \| null}` | `{group}` (canonical) |
| PUT | `/api/groups/{scope}/order` | `{order: [ids]}` | `{order}` — `tokens` refuses (400) |

Unknown scope or member → 404; invariant violations → 400. The panel component is
§panel.groups.

**On disk.** Every grouped list carries its model as **additive** keys beside the data (ADR-015).
A file that predates groups names none of them and reads as the single `default` group with
every member unassigned, so old sealed fixtures keep passing unchanged.

| File | Keys |
|---|---|
| `managed.json` | `groups` + `mcpGroups`; `tokenGroups` + `tokenMembers` (keyed by token id) |
| `tunnels.json` | `connGroups` + `ruleGroups`; `tunnelGroupsV2: true` marks the list as literal (a deleted `default` stays deleted) |
| `secrets.json` | `groups` + `secretGroups` + the row order (§host.vault) — a group label is a folder name, never a credential |
| `gateway.config.json` | the jobs row's `groups` + each definition's sparse `group` (absent renders in the first group) |

## §host.actions — Actions and runs

**Actions** are named capabilities (`GET /api/actions` lists them with their schemas).
Duplicate names are refused; disabling the owner unregisters them, and new submissions get
"unknown action". `process.exec` (strict env refs) and `process.legacy-command` (lenient, the
v1 job shape) come from the process plugin.

**Runs** — `RunCoordinator`, the one execution plane for panel, CLI, jobs, MCP and remote:

- `POST /api/runs` answers **202 + `runId`** at once (the request does not own the task);
  unknown action 400; a full pool **429** naming the bound. Defaults: 2 concurrent, 32 queued,
  8 remote runs per target. Timeout default 10 min, max 24 h.
- `GET /api/runs`, `GET /api/runs/{id}`, `POST /api/runs/{id}/cancel`. Cancel is per owner;
  the terminal state is first-wins; a deadline takes the same cancel path as a manual stop; a
  cancel awaits the reaped child and the joined pipes. A panicked action is one failed run.
- `GET /api/runs/{id}/output?after=&max=` → `{runId, state, cursor, nextCursor, output,
  truncated, terminal}`: the live cursor slice a follower polls. The live buffer keeps
  256 KiB; a finished run keeps its last 64 KiB; a read returns at most 128 KiB; a cursor older
  than the window reads the oldest kept bytes with `truncated: true`. The 32 most recent
  finished runs stay in memory; durable history belongs to the producing plugin (§remote.history,
  §jobs.runlog).
- **`actor`** — who asked: `cli:<user>@<host>` (self-declared over loopback), `mcp:<token
  label>`, `panel`, `jobs`, or `api` when omitted. The audit trail's who column.

**The process supervisor** (`services/process.rs`) runs every child: bounded capture that
truncates while reading (16 KiB default — never `read_to_end`), pipes joined without hanging
(5 s drain grace), and a subtree kill (a Windows kill-on-close job object; a POSIX process
group). Children spawn with `CREATE_NO_WINDOW`.
`run_streamed` can tee pipe chunks into a caller's bounded `RunOutputSink` before exit,
without changing retained-tail limits. When a process resolves credential references, it
publishes only the masked final output so a credential split across chunks cannot leak; stdout
and stderr are both masked, since a failing tool echoes its arguments on stderr.

## §host.history — Module-owned log storage (mechanics)

The storage seam every log-owning module runs on. The host provides machinery and
contracts; the owner (MCP for calls and traffic, Remote for its runs, Process for its
outcomes) provides tables, budgets and interpretation. Nothing here dispatches on a
module identity.

- **Engine gate** (§arch.deps): an owner opens a database in WAL mode only after the
  linked SQLite passes the WAL-reset fix gate (>= 3.51.3 or a reviewed backport); an
  engine this build cannot vouch for never sees so much as an empty file created.
- **One store per module** (`history::store`): construction performs no I/O. `open`
  runs the engine gate, connects a pool of one writer, migrates the owner's versioned
  schema steps (DDL and stamp commit in one transaction; an unknown newer version
  refuses with the store untouched), and bumps a generation. Writes are admitted
  synchronously against a byte-bounded queue and stamped with the generation;
  `flush_barrier` waits for everything admitted, polled or not. An admitted write that
  fails warns in the gateway log with its module and reason (`a history write failed`),
  besides failing the barrier that covers it: most writes are queued and never awaited,
  and a row that never lands must leave a findable reason. `close` is bounded:
  withdraw, drain, release the pool under one deadline — on the deadline the owner is
  FAILED with its unresolved count, never “closed”, and stale-generation queue entries
  are dropped and accounted, never executed into a newer generation. A disabled or
  failed owner's queries and writes refuse without opening or re-enabling anything.
- **Body and binding primitives** (`history::body`, `history::seal`): bodies chunk at
  64 KiB with a raw/lz4 codec choice per chunk (raw wins when compression would
  expand); reads cap at 128 KiB per chunk (a slice request on the body route clamps
at 256 KiB). Sealed chunks carry an authenticated binding of module,
  resource, event, part, generation and range through the frozen envelope (§core.sealed) —
  a swapped or reordered chunk is rejected on open, with no plaintext or
  empty-content fallback; the refused row serves as gone. The wired owners pin the
  binding's resource empty and its generation at 0 (the row does not store the worker
  epoch; event, part and range pin the identity), and sealing is keyed by the same
  master-key sources every sealed state file uses: a machine with no key source at all
  runs the documented degraded mode — plain coded blocks, one error logged per store —
  because refusing to record history over a missing key is a trade nothing else in the
  home makes either. The legacy/degraded fallback bounds what the sealing IS: it is
  confidentiality at rest and swap detection between genuine envelopes, not
  authenticity against whoever can write the database - a writer with that access can
  always inject plain rows, and reads answer them as plain.
- **Acknowledged completion** (`RunHistorySink::finish` → `HistoryAck`): a run's
  terminal view is published only after each sink's finish future acknowledges (bounded
  at 5 s). `Recorded` claims durable visibility; `Unavailable` publishes
  `meta.historyAck = "unavailable"` plus the reason on the view — a record that could
  not persist is a visible status, never a failed run and never false success.
- **Run-id allocation is cross-process** (`runs.seq`): every allocation and sink-floor
  raise claims a short-lived lock file, re-reads the committed value under the claim
  and commits through tmp+rename. A crashed claimant is stolen after 5 s with its
  payload as the floor — ids may be skipped, never reused. The allocator stays a host
  file; it never moves into a module database.

Schema steps take the SQLite write lock before re-reading the version stamp on that same
connection. Concurrent opens skip a step a peer already committed; they never repeat its DDL,
borrow a second connection while holding the only one, or lower a newer stamp. Each step's DDL
and stamp remain one atomic transaction, including the initial meta table.

Module-owned tables, budgets, queries and panel surfaces are the owners' sections
(§mcp history, §remote.history, §llm.history, §process.apps, §data.history), delivered per the module-log-storage plan.
**The physical budget (plan `docs/plans/module-log-storage.md` D4).** The enrolled module
histories (MCP, Remote, Terminal, LLM, Process and Data) share one declared normal budget of 1,560,000,000
bytes, split by policy into owner shares (default MCP 200,000,000, Remote 200,000,000, Terminal
100,000,000, LLM 1,000,000,000, Process 10,000,000, Data 50,000,000 — the split still sums to the declared budget;
the LLM share's first 100,000,000 were carved out of Remote's, which held 300,000,000 until its own
record existed, per ADR-035's precedent of the Terminal share; the Process share grew the declared
budget by its 10,000,000: a fifth store joined the home, and shaving the others to keep the total
would have been arithmetic pretending to be policy; and the LLM share grew to 600,000,000, the
budget with it, when one memory recheck — whose prompts inline the code they ask about — filled
100,000,000 within a day and the size trim kept cutting history still being read; the Data console
history joined with its own 50,000,000, the budget growing by it as the Process seat's did; and the
LLM share grew again to 1,000,000,000, the budget with it, when the store held 393 MB of its
600,000,000 seat on 2026-10-27 and the operator chose a larger seat over an earlier trim). A share answers to
its OWNER: adding targets, MCP servers or databases multiplies nothing. It is a soft budget —
Remote's seven-day protected window outranks it, and an over-share admission from a protected
owner is denied with the reason `protected-window-overrun` (a steady state, never a promise
that maintenance is coming), while a plain over-share denies with `over-owner-share`. The
store denies past the share but never deletes on its own; an owner that wants to keep
recording reads its share from the store and trims itself under it (the llm owner does,
`§llm.history`).

Usage is measured from the FILESYSTEM, never from logical row counts: the database file, its
live `-wal`/`-shm`/`-journal` sidecars, leftover maintenance scratch, and the reservation
ledger's in-flight bytes. Deleted rows are not reclaimed credit until the bounded maintenance
pass reclaims them: a passive checkpoint (which reports busy instead of waiting, so WAL growth
is never hidden) plus a VACUUM when the freelist exceeds a quarter of the file, checkpointed
again so the smaller file actually lands. Admission runs at the store's submit: the declared
share answers the projected bytes (measured usage plus what this process admitted but has not
yet landed), and a denial names its reason on the wire. At completion the store replaces the
estimate with freshly measured physical usage before releasing its local reservation; compressed
payloads, duplicate writes and failed payloads never accumulate their logical estimates as disk
usage. Maintenance also refreshes growth caused by a peer sharing the file. Reservations live in the owner's own
database (`quota_reservation`): the token's row exists while its write is between execution
and completion (the submit queue itself costs nothing), so every process with the file open
spends the same headroom (a retried token replaces its row rather than adding one -
a cross-process collision, the case that rule guarded, can no longer mint identical
tokens since tokens carry a per-boot instance id); a
crashed process's stale tokens reconcile in the next quiescent maintenance pass. That
reconciliation is a quiescence claim, not a liveness oracle: it checks the local queue has
drained, and a live peer's rows still in flight are indistinguishable from stale ones, so
they are collateral the budget absorbs - the owner's own admission never over-admits (its
counter is in-process), but other processes' next usage refresh stops seeing those bytes
while the write flies. The budget is soft by design. Every enrolled history registers its
maintenance pass (checkpoint, conditional VACUUM, ledger reconciliation, age sweep) on the
host's maintenance seat at start and withdraw it at stop, and the server's hourly retention
timer — the same one that ages the jsonl call logs and job run logs — runs every registered
hook: a stopped module holds no hook, and its closed database is never opened by the timer.

**The cross-module query surface (plan `docs/plans/module-log-storage.md` D5).** A module
whose history database is OPEN registers a query adapter on a host seat at start and withdraws
it at stop. `POST /api/history/query` fans out to the registered owners (or a requested
subset), merges their pages into one answer with per-module resume tokens, reports a stopped
module as `skipped` BY NAME (its database is never auto-opened by a query), and refuses a
forged or changed token for its owner alone — the rest of the page still answers. A
continuation page describes every stream: an owner absent from the cursors map has finished and
is not rescanned from the beginning. `GET /api/history/usage` reports each enrolled owner's
declared share and MEASURED physical bytes; stopped owners are absent because a report does not
open databases. Registering an owner also enrolls that owner's `db_file` on the module-database
seat the Data catalog reads (§data.sqlite); unregister withdraws every file of that owner.
A stopped module's file is not opened by the browser.

**The body endpoint (plan `docs/plans/module-log-storage.md` D5).** `GET
/api/history/{module}/records/{id}/body?after=&max=` serves one stored record's plaintext
after a byte cursor, bounded to `max` (default 64 KiB, capped at 256 KiB) with the next byte
frontier when more remains. A pruned body — or one that never crossed the preview line — says
`gone`, never a silently short text. A stopped module is an explicit unavailable, and a module
without record-body semantics says so; the host route never learns a table name. Legacy `q`
on the calls endpoint does not masquerade as full-body search (see §mcp.calls): the complete
literal surface is the query route above.

**The literal scan engine (plan `docs/plans/module-log-storage.md` D5).** History search
over stored bodies is LITERAL: ASCII case folds, and everything else — non-ASCII text,
quotes, paths, spaces — matches byte for byte. No FTS grammar, ranking or stemming: a hit
is a folded substring, a one-byte needle is legal, and the query bounds (1..256 bytes) are
validated explicitly. Pages are snapshot-ordered by (run, offset): a page's cursor anchors
at the chunk that filled it, new rows with later keys extend the stream without renumbering
returned pages, and a clear bumps the generation, which EXPIRES every cursor explicitly. A
match spanning a chunk boundary is found exactly once via a carry — the previous chunk's
tail, reconstructed on resume, never persisted; a byte gap or a run boundary resets the
carry so two halves never join into a hit that exists in neither. One call scans a bounded
batch; exhausting the budget returns progress (incomplete + cursor) even with zero hits,
never a false "no matches". Resume tokens are signed with the host master key and bind
owner, query digest and generation: a forged, cross-owner or changed-query cursor is
refused by name before any body is read, and no plaintext ever rides in a token.

## §host.seats — Capability seats

The catalog also holds an optional SQL history observer contract (dbbrowser/execution.rs).
Data installs the durable recorder while serving and clears it before closing. Agent and analysis
operations observe a request-scoped future; only an adapter mark immediately before user SQL
submission creates a history attempt. Validation/setup refusals produce none. A scope records
once on success/failure, or unknown completion on cancellation after submission. Internal
transaction/control SQL is excluded. This adds no dependency between peer subsystems.

A seat decouples a provider plugin from a consumer plugin with no crate edge between them.
The seat is held by `RuntimeServices`; the provider registers on start and on stop goes
**withdraw → drain → close**; the consumer sees a possibly empty seat, never a missing module.

| Seat | Provider → consumer | Notes |
|---|---|---|
| Connection catalog (`services/catalog.rs`) | mcp → data | Data takes a request-scoped lease per `/api/db` call; drain 5 s. A connection key includes config identity and credential context, never just host:port. Disabling Data releases only its own leases |
| Shell (`services/shell.rs`) | tunnels → terminal | A session is a lease (terminals never hand back voluntarily, drain 3 s); `PtySession` is a transport-neutral duplex, so terminal links no SSH client (ADR-011) |
| Remote transport (`services/remote.rs`) | tunnels → remote | exec, SFTP and streaming in 64 KiB chunks; §remote |

The composition also feeds Data's connection picker the sidebar's visual order: registry
names sorted by the flat manual order (unranked last, by name), sliced by group.

## §host.memory — The memory reading

`GET /api/memory[?tree=1]` → `gatewayMb` (working set), `heapUsedMb`/`heapTotalMb` (private
bytes; the Node-era field names), `childrenMb`, `processCount`, `childrenPending`, `at`;
`swiss status` prints the same. The process-tree walk (Toolhelp32, a direct call) is cached
20 s, deduplicated in flight, and skipped when the children registry is empty. A call without
`tree=1` answers `childrenPending: true` rather than a confident 0. Every panel read passes
`tree=1`. Numbers are honest or absent. The records are in §product.memory.

- **The roots** are the children registry's pids (§process.children): runs, `proc` MCPs and
  local terminal shells. They used to be the `proc` MCPs alone, so `childrenMb` now also
  counts a running job and an open shell.
- **Managed apps are never roots** and are never registered. A 300 MB Node app the operator
  runs is not the gateway's memory.
- **One walk, two readers.** The walk keeps a subtotal per root. `/api/memory` sums the
  subtotals, and the Process page's children rows read them from the same cache.

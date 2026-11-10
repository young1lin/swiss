# Live pages on the bus: brief, draft SPEC amendments and delivery plan

This is the implementation brief for taking the panel off its 6 s tick. It is not a second
canonical specification. The current-behaviour specification remains [SPEC](../SPEC.md). The
amendment texts in §5 land **with the commit that makes each paragraph true**, never ahead of it
(SPEC §about, the swiss-spec skill). The research this brief rests on is
[bus-poll-handoff.md](bus-poll-handoff.md); read it first, it lists every timer and the facts
that are easy to get wrong. The bus itself is SPEC §bus (chapter 0019) and the accepted brief
[event-bus.md](event-bus.md).

Status: draft, 2026-11-10, written for the owner's approval. No code accompanies it. Every
decision in §2 is open to the owner's veto; where this brief and the code disagree, re-read the
code and say so in the commit, the code wins.

## 1. The goal

The owner's words (commit `521fc953`):

> 我说的是全局六秒的那个不需要了，只需要 WS 和刷新页面加载。其他的缺口不是实时的，也需要一起带上，保证实时

Read as:

- `setInterval(poll, 6000)` in `crates/swiss-panel/panel/src/main.ts` goes, and with it
  `PageModule.poll` and `pollPage`.
- A page reads on mount, after every WELCOME, on the `r` key, and when a notice of a topic it
  shows arrives. Everything that swiss itself changes reaches every open page that shows it
  within about a second, including a change made in another window, by the CLI, by an agent
  or by a background task.
- The gaps that were never on the tick but are in-process state (the Jobs Run now loop, the
  MCP log's stderr, the memory chip) are closed in the same effort.

Out of scope, on purpose (D9): following one open item (a Remote run's output, a process log,
an LLM dialogue, a memory sync or review sheet, a Redis stream, one Data analysis execution),
and pages that sample a system swiss does not own (Data Activity, the Pub/Sub listing, the
Process inventory). Those keep their own page timers, which already stop when the page or the
tab goes away.

## 2. Decisions, with reasons

**D1. The tick is deleted; a page may export `clock()` instead.** `PageModule.poll` is
removed from the interface. A page whose tick today keeps times relative to now fresh on
screen (Remote Targets' last-run column, the Jobs last/next line, and any other page that
exports `poll` today and paints through `relTime` or a sibling time helper; find them by
search, not from this list) exports `clock(): void`, which repaints those cells from data the
page already holds. A page that does not repaint relative times today does not gain a clock. The shell calls the active page's `clock` every
6 s while the document is visible (`CLOCK_MS = 6000`, the cadence the eye is used to). A
`clock` never reads the network; a test per page proves it (§6). Reason: the owner objected to
the backend reads, not to a relative time that ticks; a page that stops repainting "2 min ago"
would be a regression nobody asked for.

**D2. Notices plus a coalesced re-read of the existing GET routes.** Every new topic is a
`Notice` (empty body) except HOST.MEMORY (D5). The page re-reads the route it already reads,
where masking and shaping already happen. No HTTP route is deleted, and no write moves onto
the bus. Reason: this is the shape the Remote work proved (`521fc953`, `8a3c5dd6`), and the
owner accepted it; `event-bus.md` §1 decision 3 is about writes, and nothing here is a write.

**D3. One coalescer.** `coalescedRead` moves from `panel/src/views/remote-live.ts` to
`panel/src/bus/live.ts`, constants unchanged (`SETTLE_MS = 150`, `CAP_MS = 1000`), its comment
rewritten to name every user. The Remote pages import it from the new place. Next to it,
`busOnWhileVisible(topic, id, schema, fn, onVisible)`: subscribes while
`document.visibilityState === "visible"`, unsubscribes on hide, re-subscribes on show and
then calls `onVisible` (the page's re-read). Every other topic stays subscribed while hidden
and lets the coalescer hold its read, as Remote does today.

**D4. The shell owns three topics; pages do not read them again.** The shell (main.ts and
page-registry.ts) subscribes once per page load to HOST.PLUGINS (the nav's plugin inventory,
`reloadPluginInventory`), HOST.TOKENS (`loadTokens`) and HOST.MEMORY (the chip). The Plugins
and Tokens pages paint from the shell's read through a callback (`onInventory`,
`onTokens`), never a second GET for the same notice. `/api/info` is read once at boot and again
after every WELCOME (its facts are static per process; a new process is always a new WELCOME).
Reason: these three are what the tick reads on every page; the nav needs the plugin inventory
on every page, and SPEC §remote.panel records "a plugin enabled in another window is not on the
bus" as a known gap.

**D5. Gauges are sampled by the gateway, and only while someone looks.** The hub gains a
demand query (§3.1). HOST.MEMORY carries the reading itself (`MemoryReading`, §4), sampled
every 6 s while at least one link subscribes, at once when demand rises, never while nobody
subscribes. The chip holds the topic with `busOnWhileVisible`, so a hidden or closed window
costs the gateway nothing, which keeps today's "a hidden tab does not poll". On subscribe the
chip still does one `GET /api/memory?tree=1` (subscribe, then read), and clicking it still
reads once. A sample whose rounded figures (as the chip prints them) equal the last published
reading is not published. Reason: one sample serves every open window; the reading carries no
secret, so it can travel whole (SPEC §bus.topics "an event that can carry the record does").

**D6. MCP health is held by subscription, and the loop is broken by visibility.** While any
link subscribes MCP.SERVERS, a holder task calls `Registry::watch()` once per probe interval
(15 s), so the 30 s window never lapses under a subscriber. Pages hold MCP.SERVERS with
`busOnWhileVisible`. When no visible page subscribes, the holder stops, the window lapses after
two periods, `forget_health` turns the rows Unknown and publishes one notice, which reaches no
subscriber, so no read can re-arm the probe. The flap of bus-poll-handoff.md "Facts" cannot
happen: the lapse needs zero demand, and with zero demand nobody re-reads. The probe still
never starts at boot. `http` and `rest` adapters still have no ping.

**D7. High-rate owners publish through a throttle.** `swiss_bus::Notifier` (§3.2) sends at most
one notice per topic per 250 ms: the first poke at once, the last poke of a burst at the end of
the gap. It is used by MCP.CALLS (calls and stderr), MCP.TRAFFIC, LLM.HISTORY and
LLM.TELEMETRY. Low-rate owners (a definition saved, a plugin state, a group edit) publish
directly. Reason: the client coalescer already merges a burst into one read, but a chatty MCP's
stderr or a busy traffic log could otherwise fill a slow link's 256-frame queue and end it with
BYE `SLOW`; a notice carries nothing, so a dropped duplicate loses nothing, and the trailing
publish guarantees the last change is announced.

**D8. A notice leaves only once the change is visible through the GET.** For writes that go
through a history store queue (`HistoryStore::submit`: the MCP call log, MCP traffic, the LLM
history), the owner facade wraps the job so the notice is poked from inside the job's future
after the statements returned `Ok`, never at admission. For the jsonl fallback of the MCP call
log, after the append is flushed. For JSON-file owners, after the file write returned `Ok`,
while the owner still holds its write lock where it has one. A write that changed nothing
publishes nothing.

**D9. What stays a timer.** Listed in §1. In particular: Remote's open output stays the 1.5 s
follow (`event-bus.md` §7's "live output on HOST.RUNS" is superseded by `521fc953`; do not
implement it). Process keeps its mount-owned 500 ms / 2 s / 5 s sampler (SPEC §process.panel);
only its redundant tick `poll` goes. Data Activity, Pub/Sub listing, Stream Follow and the
analysis execution timers are untouched; `event-bus.md` §7's DATA.ACTIVITY, DATA.ANALYSIS,
DATA.REDIS, PROCESS.APPS and PROCESS.LOGS rows are not built.

**D10. Tunnels' traffic counters get no notice.** `/api/tunnels` rule rows carry `sockets`,
`bytesIn`, `bytesOut` and `channelFailures`, which move with every byte. The panel paints none
of them today (search `panel/src` outside `types/`). TUNNELS.TABLE announces state, reason,
definitions and groups only; the counters ride along whenever the page re-reads. A later page
that paints them needs a demand-held sampler topic, not a notice per byte.

**D11. Jobs: two topics.** The Jobs page subscribes HOST.RUNS with key `""` and drops every
`RunChanged` whose `owner` is not `"jobs"` before the coalescer, plus a new JOBS.LIST notice.
JOBS.LIST is needed because the HOST.RUNS terminal event fires before `persist_last_ok`
settles `lastRunAt` / `lastOk` into `jobs-state.json`: a re-read on the run event alone can
paint the old last-run. JOBS.LIST fires after that settle, after a definitions write
(`apply_config` and the revision-checked store), after `forget`, and after a jobs group edit.
Run now no longer loops `GET /api/runs/{id}`: `runJob` holds its own HOST.RUNS subscription
(independent of the page, so leaving the page never cancels the wait, as today), waits for the
terminal `RunChanged` of its run id, computes `ms = ended_at_ms - started_at_ms`, and does one
`GET /api/runs/{id}` only when the terminal state is not succeeded (for `error`, which the event
may not carry, SPEC §host.actions). An event that arrives before the POST answers is kept in a
small map of recent terminal events by run id. After a WELCOME while waiting, one GET settles
what a disconnect might have hidden. The 30-minute UI bound stays, as a timeout, not a poll.

**D12. Terminal: the session list joins the bus, the PTY bytes do not.** TERMINAL.SESSIONS
announces a session opened, closed or relabelled, and recording turned on or off. `bytesOut`
and `attached` are not painted by the page today (`paintTabs` uses neither), so they get no
notice; the stale comment on `views/terminal.ts` `poll` goes with the poll. SPEC §bus.registry's
"9 (terminal) keeps its own socket" is narrowed to the PTY socket.

**D13. A group edit publishes its owner's topic.** `/api/groups/{scope}` writes go through
`GroupScope` implementations registered in `GroupScopes` (`swiss-host/src/groups.rs`). Each
scope publishes what its owner publishes for a route write, the way `TargetGroups` already does
for REMOTE.TARGETS: `mcps` → MCP.SERVERS, `tokens` → HOST.TOKENS, `conns` and `rules` →
TUNNELS.TABLE, `jobs` → JOBS.LIST, `secrets` → HOST.VAULT, `targets` → REMOTE.TARGETS
(already done).

**D14. Hooks, not dependencies.** Owner crates that do not already depend on `swiss-bus`
take a change hook (`set_notice(Arc<dyn Fn() + Send + Sync>)`, or `Arc<dyn Fn(&str)>` for a
keyed topic) the way `swiss-remote` stores and `TunnelManager::set_remote_seat_changed` do. The
composition, which holds `services.bus`, builds the closure (a direct publish or a
`Notifier::poke`). `swiss-core` (the vault) gets a process-wide hook slot for the same reason:
it cannot depend on the bus.

**D15. One topic per commit, the tick last.** Each commit adds one topic end to end (proto, both
codecs, golden, owner publish, page subscription, the page's `poll` export removed, tests, SPEC
slice). The global tick keeps calling the pages that still export `poll` until the last commit
deletes it. Order in §7.

## 3. New bus mechanisms (the first commit)

### 3.1 Demand

```rust
impl Bus {
    /// True while at least one welcomed link subscribes to (module, sub) under any key.
    /// Updated on SUBSCRIBE, UNSUBSCRIBE and link close; a receiver created while demand
    /// already holds starts true.
    pub fn demand(&self, module: u32, sub: u32) -> tokio::sync::watch::Receiver<bool>;
}
```

Kept in the hub next to `links` (`crates/swiss-bus/src/hub.rs`): a per-(module, sub) count of
subscribing links, recomputed under the same lock as `subscribe` / `unsubscribe` / `close`,
and a `watch::Sender<bool>` per (module, sub) created on first `demand()` call. A link that is
not yet welcomed does not count (publish already ignores it).

A sampler is a task: wait until `demand` is true; sample at once; then sample every period
while `demand` stays true; when it turns false, stop sampling and wait again. It does no work
while nobody subscribes, which is the idle-memory rule of SPEC §host.memory and §mcp.registry.

### 3.2 Notifier

```rust
/// Publishes `Notice`s for one (module, sub, id): at most one per key per `gap`. The first
/// poke publishes at once; pokes inside the gap arm one trailing publish at its end. Never
/// blocks and never waits on a link; safe to call from any thread (it holds a runtime
/// handle captured at construction).
pub struct Notifier { /* bus, module, sub, id, gap, per-key last-sent + armed */ }
impl Notifier {
    pub fn new(bus: Arc<Bus>, module: u32, sub: u32, id: u32, gap: Duration) -> Arc<Self>;
    pub fn poke(&self, key: &str);
}
```

`gap` is 250 ms everywhere it is used here. Per-key state is dropped once a key has been quiet
for a whole gap with nothing armed, so keys (MCP names) do not accumulate.

### 3.3 Panel

- `panel/src/bus/live.ts`: `coalescedRead` (moved), `busOnWhileVisible` (D3).
- `PageModule`: `poll?` removed in the last commit; `clock?: () => void` added in the first.
  Until the last commit the shell calls both: `poll` on the tick for pages that still export
  it, `clock` on the same tick.

## 4. Registry rows

New module numbers are chapter numbers (SPEC §bus.registry): MCP 5, TUNNELS 7, JOBS 8,
TERMINAL 9, LLM 13. New `.proto` files `mcp.proto`, `tunnels.proto`, `jobs.proto`,
`terminal.proto`, `llm.proto` beside the existing ones; both generators read every `.proto` in
`crates/swiss-bus/proto/swiss/bus/v1/`. Enum value names are unique across the package, which is
why ids carry their sub's name (`HistoryId` is already REMOTE.HISTORY's).

| Module | Sub | Id | Kind | Body | Key | Published when |
|---|---|---|---|---|---|---|
| 4 HOST | 3 PLUGINS | 1 PLUGINS_CHANGED | EVENT | `Notice` | plugin id | a plugin's state moved (`set_state` with a different state), its last error changed, or its config was saved |
| | 4 TOKENS | 1 TOKENS_CHANGED | EVENT | `Notice` | — | a token created, rotated, removed; a `tokens` group edit |
| | 5 VAULT | 1 VAULT_CHANGED | EVENT | `Notice` | — | a secret put or deleted, vault groups set, an import merged, a `secrets` group edit |
| | 6 MEMORY | 1 MEMORY_READING | EVENT | `MemoryReading` | — | a sample while demand holds (D5) |
| 5 MCP | 1 SERVERS | 1 SERVERS_CHANGED | EVENT | `Notice` | — | what `GET /api/mcps` answers moved: register, start, stop, restart, update, rename, delete, an idle reap, a probe round, `forget_health` that changed a row, an order or `mcps` group edit |
| | 2 CALLS | 1 CALLS_CHANGED | EVENT | `Notice` | MCP name | a call row committed; that MCP's log cleared (rows removed); its stderr ring grew (throttled) |
| | 3 TRAFFIC | 1 TRAFFIC_CHANGED | EVENT | `Notice` | — | a traffic row committed (throttled); the traffic log cleared |
| 7 TUNNELS | 1 TABLE | 1 TUNNELS_TABLE_CHANGED | EVENT | `Notice` | — | a rule's or connection's state or reason moved; a rule or connection created, edited, deleted; start-all / stop-all; a port freed; a `conns` or `rules` group edit |
| 8 JOBS | 1 LIST | 1 JOBS_LIST_CHANGED | EVENT | `Notice` | — | definitions written; `lastRunAt` / `lastOk` settled; a job forgotten; a `jobs` group edit |
| 9 TERMINAL | 1 SESSIONS | 1 SESSIONS_CHANGED | EVENT | `Notice` | — | a session opened, closed, relabelled; recording turned on or off |
| 13 LLM | 1 CONFIG | 1 LLM_CONFIG_CHANGED | EVENT | `Notice` | — | `LlmState` applied a row, started or stopped (its `generation` bump) |
| | 2 TELEMETRY | 1 TELEMETRY_CHANGED | EVENT | `Notice` | — | the reporter's status moved: an export finished or failed, a span dropped or rejected (throttled) |
| | 3 HISTORY | 1 LLM_HISTORY_CHANGED | EVENT | `Notice` | — | an LLM history write committed (calls, runs, rounds, close, reopen); a sweep or trim that removed rows (throttled) |

```proto
// HOST.MEMORY (SPEC §bus.registry, SPEC §host.memory). Module 4, sub 6.
// The reading itself: one sample serves every open page. Sampled only while some link
// subscribes (SPEC §bus.topics "Demand").
message MemoryReading {
  double gateway_mb = 1;         // the route's gatewayMb (working set)
  double private_mb = 2;         // the route's heapUsedMb / heapTotalMb (private bytes)
  optional double children_mb = 3; // absent while childrenPending
  uint32 process_count = 4;
  bool children_pending = 5;
  uint64 at_ms = 6 [jstype = JS_NUMBER];
}
```

Check the field list against `crates/swiss-host/src/mem.rs` `get_memory_info` and the panel's
`renderMemory` before committing it; the route's JSON is the authority. Each new topic gets a
golden frame in `crates/swiss-bus/testdata/golden.json` (one `Notice` frame per new topic and
one `MemoryReading`), as REMOTE.TARGETS did.

## 5. Draft SPEC amendments

Each lands with the commit that makes it true (§7). Text in quotes is a draft; fit it to the
paragraph it joins.

### 5.1 Chapter 0019 (§bus)

- §bus.registry: add the rows of §4. Replace "1–3 have no runtime; 9 (terminal) keeps its own
  socket" with "1–3 have no runtime; the terminal's PTY bytes keep their own socket
  (§terminal.api), its session list is on the bus (TERMINAL.SESSIONS)."
- §bus.topics, after **Notices**, two new bullets:
  - "**Demand.** The hub knows, for each (module, sub), whether any welcomed link subscribes to
    it under any key. An owner whose state is a sample rather than a commit (the memory
    reading, MCP health) samples only while that demand holds and stops when the last
    subscriber leaves or its link ends. A page holds such a topic only while it is visible, so
    a hidden or closed window costs the gateway nothing."
  - "**Throttled notices.** An owner whose writes can come many per second (the MCP call log
    and stderr, traffic, the LLM history and telemetry) publishes through a notifier that sends
    at most one notice per topic and key per 250 ms: the first at once, the burst's last at the
    end of the gap. A notice carries nothing, so a dropped duplicate loses nothing, and the
    trailing one announces the last change."
- §bus.link, the new-build notice: the sentence "This replaces the stamp check in the 6 s
  `/api/info` poll" becomes "`/api/info` is read once at boot and after each WELCOME; nothing
  reads it on a timer."

### 5.2 Chapter 0014 (§panel), README

- **The rendering contract**: "never by a poll" → "never by a notice's re-read"; "survives every
  tick" → "survives every re-read".
- **Polling** is replaced by:
  "**No tick.** The panel has no network timer of its own. A page reads on mount, after every
  WELCOME (§bus.topics), on `r`, and when a notice of a topic it shows arrives, coalesced; every
  table swiss changes reaches an open page that way (§bus.registry). What still runs on a timer
  is named where it lives: following one open item (a Remote run's output, a process log, an
  LLM dialogue, a sync or review sheet, a Redis stream, a Data analysis execution) and sampling
  a system swiss does not own (Data Activity, the Pub/Sub listing, the Process inventory). A
  page's `clock()` repaints times relative to now from what the page holds, every 6 s while
  the document is visible; it never reads the network. The shell holds three topics for every
  page: HOST.PLUGINS (the nav), HOST.TOKENS (the token list) and HOST.MEMORY (the chip, held only
  while the document is visible). The memory chip is a reading, not a reload button: clicking
  it reads once. The one explicit view refresh is `r` (`refreshNow`)."
- `PageModule`: `poll?` replaced by `clock?: () => void; // relative times only, no network`.

### 5.3 Chapter 0004 (§host)

- §host.plugins: "Every state a plugin enters that differs from the last, a changed last
  error, and a saved config publish HOST.PLUGINS keyed by the plugin id. The panel's nav
  re-reads `GET /api/plugins` on it, so a plugin enabled, disabled or failed in one window or
  by the CLI shows in every open window."
- §host.tokens: "Create, rotate, remove and a `tokens` group edit publish HOST.TOKENS."
- §host.vault: "A secret put or deleted, the vault groups set, an import merged and a `secrets`
  group edit publish HOST.VAULT once the file is written. A change made to the vault file by
  another process is not announced; the next read shows it."
- §host.memory: append "The panel reads it through HOST.MEMORY: while any page holds the topic
  the gateway samples every 6 s (at once when the first subscriber arrives) and publishes the
  reading when its printed figures moved; with no subscriber it samples nothing."
- §host.groups: "A write to a scope publishes its owner's topic, as the owner's own route
  would (the table in §bus.registry)."
- §host.memory's 20 s tree-walk cache is unchanged; say nothing new about it.

### 5.4 Chapter 0005 (§mcp)

- §mcp.registry, the health-probe row: after "calls `watch()`" add "; so does a page holding
  MCP.SERVERS: while any link subscribes, the gateway calls `watch()` once a period, so the
  window never lapses under it. Pages hold MCP.SERVERS only while visible, so the probe parks
  when no window shows health, exactly as before. A probe round publishes one MCP.SERVERS
  notice when it changed what the list answers (a state, a latency, a reason); the lapse to
  Unknown publishes one too, and it can only happen with no subscriber, so no notice can re-arm
  the probe."
- §mcp.calls, **History pages hold still**: "Page 0 polls every 6 s" → "Page 0 is live: it
  re-reads on MCP.CALLS for its MCP (a committed call, a Clear, stderr that grew) and repaints
  only when its signature changed; a page > 0 ignores the notice, and returning to page 0
  fetches at once." **Pager semantics**: "an unchanged poll" → "an unchanged re-read".
- §mcp.traffic: after the sentence about the 6 s poll's cost, add "The list is live on
  MCP.TRAFFIC: one throttled notice for any number of committed rows, then one re-read; rows
  still carry no body."
- §mcp.panel **Polling** → "**Live.** Servers, Traffic and the Tunnels pages hold MCP.SERVERS
  while visible and re-read `GET /api/mcps` on its notices, patching the sidebar and the detail
  header only — never a form mid-entry (`paneHasFocus`)." And "the next poll" in the drag line →
  "the next re-read".

### 5.5 Chapter 0007 (§tunnels)

- §tunnels.providers: after the seat sentence, "Separately, every rule state transition, every
  rule or connection created, edited or deleted, start-all, stop-all, a freed port and a
  `conns` or `rules` group edit publish TUNNELS.TABLE. The traffic counters on a rule row move
  without a notice; they are fresh whenever the table is read."
- §tunnels.panel: "a poll patches dots, reasons, buttons and counts in place" → "a TUNNELS.TABLE
  or MCP.SERVERS notice re-reads and patches dots, reasons, buttons and counts in place".
- Chapter 0002's table row "`state.tun.keys` is cached for the page's life | … unlike the
  polled data" → "unlike the live data".

### 5.6 Chapter 0008 (§jobs)

- §jobs.panel: the patch sentence gains "The page re-reads on JOBS.LIST and on HOST.RUNS events
  whose owner is `jobs`, coalesced."
- **Run now** → "submits `{"async":true}` and waits for its run's terminal HOST.RUNS event, at
  most 30 minutes (a UI bound; the job's own timeout is the real one); the duration comes from
  the event, and a run that did not succeed costs one `GET /api/runs/{id}` for its error. Then
  it points at History. Closing the page never cancels a job, and never ends the wait."
- §jobs.runlog (or wherever `jobs-state.json` settling is described): "JOBS.LIST is published
  after `lastRunAt` / `lastOk` are settled, which is after the run's terminal HOST.RUNS event."
- Chapter 0002 table: "Run now polls for at most 30 minutes" → "Run now waits at most 30
  minutes".

### 5.7 Chapter 0009 (§terminal)

- §terminal.sessions: "Opening, closing or relabelling a session and turning its recording on
  or off publish TERMINAL.SESSIONS. `bytesOut` and `attached` move without a notice."
- §terminal.panel: the session tabs and the count chip re-read the list on that notice; the
  PTY socket is unchanged.

### 5.8 Chapter 0010 (§remote)

- §remote.panel **Live without a poll**: "Targets' `poll()` reads nothing: on the tick it
  rewrites…" → "Targets' `clock()` rewrites…". The last sentence "A plugin enabled or disabled
  in another window is not on the bus; the pages learn it from their next read" → "A plugin
  enabled or disabled elsewhere reaches the nav through HOST.PLUGINS (§host.plugins)."
- The comment on `LIVE_EVERY_MS` in `views/remote-runs.ts` ("the 6 s poll moves the list") is
  corrected in the first commit.

### 5.9 Chapter 0011 (§process)

- §process.panel: "Shell and local polls share one in-flight request per inventory" → "The
  page's own sampler keeps one in-flight request per inventory" (the shell no longer polls).

### 5.10 Chapter 0013 (§llm)

- §llm.panel: "Providers, Agents and Embedding re-read `/api/llm/status` on LLM.CONFIG; Agents
  also on LLM.TELEMETRY (the export counters) and HOST.VAULT (`secretsInForce`). Embedding never
  replaces a form under the operator's hands: with unsaved changes only the in-force card
  repaints. History re-reads its calls and runs lists on LLM.HISTORY and patches them; an open
  run's dialogue still follows every 3 s while it is live."
- §llm.history: "Every committed write, and a sweep or trim that removed rows, publishes
  LLM.HISTORY, throttled."
- §llm.telemetry: "The reporter publishes LLM.TELEMETRY when its status moves, throttled."

### 5.11 ADR-040 (chapter 0018)

Add after ADR-039:

"**ADR-040 — The panel has no tick; gauges are sampled on demand.** Context: after the bus
(ADR-039) every list still re-read itself every 6 s, and a change made elsewhere showed up to
6 s late or, for pages off the tick, never. Decision: every table swiss changes announces a
notice on the bus, the page re-reads its route coalesced, and the 6 s timer is deleted.
State that is a sample rather than a commit (the memory reading, MCP health) is sampled by the
gateway only while a visible page subscribes (hub demand), so idle costs nothing. Rejected:
pushing every record over the bus (the routes already mask and shape, and several tables are
large); keeping the tick as a safety net (it hides a missing notice instead of failing a test);
a sampler that runs for the life of the process (the idle-memory rule). Consequence: a writer
that forgets to publish is a bug a test must catch, because nothing re-reads by itself any
more."

## 6. Acceptance tests

Every behaviour change ships with a test that fails before it. Cite the SPEC anchor in a
comment above each test (`// SPEC §<anchor>: …`). Rust notice tests follow the existing
counting-hook shape (`counting_notice()` in `crates/swiss-remote/src/history.rs`) or the
subscribed-link shape (`crates/swiss-host/src/services/runs.rs` and `services/remote.rs`
test helpers). Panel tests use the existing fake bus (`test/console-bus-fixture.ts`,
`test/remote-live.test.ts`, `test/admin-remote-view.test.ts`).

Bus core (`swiss-bus`):

- `demand_rises_on_the_first_subscriber_and_falls_on_the_last`: two links on one (module, sub)
  under different keys; unsubscribing one keeps it true; the second's close turns it false.
- `demand_ignores_a_link_that_is_not_welcomed`.
- `a_demand_receiver_made_while_demand_holds_starts_true`.
- `notifier_publishes_the_first_poke_at_once_and_one_trailing_for_a_burst` (paused tokio time:
  10 pokes inside 250 ms → exactly 2 frames, the second at the gap's end).
- `notifier_keys_are_independent` and `notifier_forgets_a_quiet_key`.

Panel core:

- `coalescedRead` tests move with the function, unchanged.
- `busOnWhileVisible`: subscribes when visible; a hide sends UNSUBSCRIBE; a show re-subscribes
  and calls `onVisible` once; unmount while hidden sends nothing more.

Per topic (each listed test fails before its commit):

- HOST.PLUGINS: enable, disable and a failed start each publish once with the plugin's id; a
  `set_state` to the same state publishes nothing; a config put publishes once. Panel: a
  PLUGINS notice reloads the inventory once and repaints the nav; with the Plugins page mounted
  the page repaints from that same read (the api mock sees one `GET /api/plugins`, not two).
- HOST.TOKENS: create, rotate, remove, a tokens group edit each publish once; a refused write
  publishes nothing. Panel: a notice re-reads the token list once; the Tokens page exports no
  `poll`.
- HOST.VAULT: put, delete, vault groups set, import merge each publish once; a no-op import
  publishes nothing. Panel: the Secrets page re-reads on the notice; LLM Agents re-reads its
  status on the notice.
- JOBS.LIST + HOST.RUNS: a definitions write publishes; `persist_last_ok` publishes after the
  state file holds the new `lastOk` (assert by reading the state inside the hook);
  `forget` publishes. Panel: a `RunChanged` with owner `remote` causes no read; one with owner
  `jobs` causes one coalesced read; Run now with fake timers makes zero `GET /api/runs/{id}`
  for a succeeded run and exactly one for a failed one; a terminal event that arrives before
  the POST answer is honoured; leaving the page does not end the wait.
- MCP.SERVERS: register, start, stop, delete, a probe round that changed latency, and
  `forget_health` each publish; a probe round that changed nothing publishes nothing. Holder:
  with demand the probe never lapses across 5 periods (paused time) and rows never go Unknown;
  without demand it lapses after 2 periods and publishes once; nothing starts the probe at boot
  (the existing boot test stays green). Panel: Servers, Traffic and Tunnels subscribe
  MCP.SERVERS with `busOnWhileVisible`; a hide sends UNSUBSCRIBE.
- MCP.CALLS: the notice is published only after the row is readable through `read_calls`
  (database path and jsonl path, both asserted from inside the hook); Clear publishes when rows
  were removed and only then; stderr growth publishes throttled, keyed by name. Panel: page 0
  re-reads on its MCP's notice; a page > 0 does not; another MCP's notice does not.
- MCP.TRAFFIC: committed traffic publishes throttled; Clear publishes. Panel: one notice burst
  → one read of the list.
- TUNNELS.TABLE: every `data.state =` site in `manager/lifecycle.rs` and `manager/recovery.rs`
  publishes through the hook; rule create, edit, delete, start-all, stop-all, port free, a
  `rules` and a `conns` group edit publish; a connection state change publishes both
  HOST.REMOTE_SEAT (unchanged) and TUNNELS.TABLE. Panel: Tunnels and Forwards re-read
  `/api/tunnels` and `/api/mcps` once per burst; neither exports `poll`.
- LLM.CONFIG / TELEMETRY / HISTORY: `apply` and `stop` publish CONFIG; an export result
  publishes TELEMETRY throttled; a committed history write publishes HISTORY only after it is
  readable through `read_calls` / `read_runs`. Panel: History patches its lists on the notice
  without stealing the search box's focus (extend `test/admin-llm-history.test.ts`); Embedding
  with unsaved changes repaints only the in-force card on a CONFIG notice.
- TERMINAL.SESSIONS: open, close, relabel, recording on/off publish; output bytes do not.
  Panel: the tabs and the count chip re-read on the notice.
- HOST.MEMORY: no sample with no subscriber (assert the sampler's counter stays 0 across 30 s of
  paused time); the first subscriber gets a reading at once; an unchanged rounded reading is
  not republished; the last unsubscribe stops sampling. Panel: the chip subscribes while
  visible, does one GET on subscribe, repaints from READING events, unsubscribes on hide.
- Tick removal (last commit): every module in the page registry exports no `poll` (a test that
  imports each page descriptor's module); every page that exports `clock` has a test that mounts
  it with the api mock, clears the mock's log, runs `clock()` and asserts zero requests; if the
  shell can be booted under vitest (see `test/i18n-shell-zh.test.ts`), boot it, let the initial
  reads settle, advance fake timers 60 s with a visible document and assert no request.

Live, after the owner deploys (never by the implementer): on 19998 only, two windows. Enable
and disable a plugin in one; the other's nav follows within a second. Start and stop a tunnel
rule; both windows follow. Run a job; the Jobs page and the Run now toast follow without
`/api/runs/{id}` polling (check the network log). Hide both windows for a minute; the gateway
log shows no MCP probe and no memory sampling; show one; health and the chip return within a
second.

## 7. Delivery order

One commit per item. Each commit: the owner code and its publish, the proto and both generated
codecs (`cargo run -j 2 --manifest-path crates/swiss-bus/protogen/Cargo.toml`, then
`npm run proto` in `crates/swiss-panel/panel`), the golden frames, the panel change with its
page's `poll` export removed, the tests of §6 for that item, the SPEC slice of §5 for that
item, and the panel emit (`npm run build`, the generated `src/admin_assets/js` committed).

1. **Bus core.** Demand (§3.1), `Notifier` (§3.2), `bus/live.ts` with the moved `coalescedRead`
   and `busOnWhileVisible`, `PageModule.clock` (Remote Targets' `poll` becomes `clock`), the
   `LIVE_EVERY_MS` comment fixed. SPEC: §bus.topics Demand and Throttled notices; §remote.panel
   `clock()`; ADR-040.
2. **HOST.PLUGINS**, the shell's nav subscription, the Plugins page off the tick.
3. **HOST.TOKENS**, `/api/info` once at boot and after WELCOME, the Tokens page off the tick.
4. **HOST.VAULT**, the Secrets page off the tick.
5. **JOBS.LIST** and the Jobs page on HOST.RUNS; Run now without the loop.
6. **MCP.SERVERS** and the health holder; Servers, Traffic and Tunnels hold it while visible
   (Servers off the tick; Traffic and Tunnels still poll their own tables until 7 and 8).
7. **TUNNELS.TABLE**; Tunnels and Forwards off the tick.
8. **MCP.CALLS** (calls, Clear, stderr) and **MCP.TRAFFIC**; Traffic off the tick.
9. **LLM.CONFIG**, **LLM.TELEMETRY**, **LLM.HISTORY**; the four LLM pages off the tick.
10. **TERMINAL.SESSIONS**; Terminal off the tick.
11. **HOST.MEMORY** and its sampler; the chip off the tick.
12. **The tick deleted.** Process's `poll` export removed; `setInterval(poll, 6000)`,
    `pollPage` and `PageModule.poll` deleted; the shell's visibility handler keeps only the
    clock and the bus's own reconnect; SPEC §panel **No tick**, §bus.link, §process.panel.

Gates for each commit, run serially, one heavy job at a time, never two at once:

- Rust: only the tests the commit touches, `cargo test -j 2 -p <crate> <filter> --
  --test-threads=1`; clippy on the touched crates with `-D warnings`, `-j 2`. Say in the commit
  report which tests ran (this machine does not run the full workspace suite).
- `tests/spec_layout.rs` when a SPEC anchor or chapter file changed.
- Panel: `npm run check` in `crates/swiss-panel/panel` (proto:check, typecheck, lint, build
  check, tests; vitest workers are capped), then `npm run build` and commit the emit.

## 8. Traps

- `cargo fmt` is never run; the tree is hand-formatted.
- Never hand-edit `crates/swiss-panel/src/admin_assets/js`; edit `panel/src` and build.
- A proto number is never reused or renumbered; a retired message leaves a `reserved` line.
- `http` and `rest` MCP adapters get no ping. The probe never starts at boot.
- A notice before the commit makes the page re-read and miss the row (D8).
- MCP Logs page > 0 must not move under a notice (SPEC §mcp.calls).
- Traffic rows carry no body; do not put one in a notice.
- `HOST.REMOTE_SEAT` is the remote transport seat, not the tunnels table; keep it as it is.
- Remote's open output stays a 1.5 s follow; do not move it onto HOST.RUNS.
- The terminal PTY socket is not the bus.
- `RunChanged` carries no error text and never will (SPEC §host.actions).

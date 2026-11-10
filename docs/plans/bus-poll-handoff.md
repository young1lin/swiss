# Handoff: which panel polls can move onto the bus

Status: exploration notes, 2026-11-10, checked against the code a second time the same day
(the corrections are folded in below). Not a specification and not an accepted plan.
The current-behaviour specification remains [SPEC](../SPEC.md). The accepted bus brief is
[event-bus.md](event-bus.md). The brief that turns this file into SPEC amendments is
[bus-live-pages.md](bus-live-pages.md). Nothing in this file has been implemented. A later model
should re-judge the classification against the code it reads; where this file and the code
disagree, the code wins.

The owner asked for a document another model can use to decide how to replace the panel's
remaining timed backend reads with bus messages. This file is that document. It records what
was read, what the last Remote work already settled, every timed read that was found, one
proposed split, and the facts that are easy to get wrong.

## What the owner has already said

Quoted from the commit that took the Remote pages off the tick (`521fc953`, the owner's
words):

> 我说的是全局六秒的那个不需要了，只需要 WS 和刷新页面加载。其他的缺口不是实时的，也需要一起带上，保证实时

Read that as the goal, not as a claim that every timer in the tree is the same kind of
gap. Two readings are both compatible with the sentence:

- Delete the global 6 s tick, and make every page that today learns about a change from
  that tick learn it from the bus instead. A page still loads on navigation.
- Also make live the gaps that were never on that tick (an open log, a follow, a
  per-second sample of another process).

The exploration's recommendation is the first reading, plus the gaps where swiss itself
already commits the change. Byte pumps and samples of systems swiss does not own stay
pulls. That recommendation is not a decision.

## How a poll becomes a bus message here

Phase 1 of the bus is shipped. One WebSocket per page, `GET /api/bus`, protobuf frames,
generated codecs. Module numbers are SPEC chapter numbers. SPEC §bus.

What a later page should copy is the Remote work, not a new shape:

- **Subscribe, then read.** The page subscribes before its first HTTP read, and re-reads
  after every WELCOME (`busOn` / `busOnResync` in `crates/swiss-panel/panel/src/bus/bus.ts`).
  The client refcounts a topic, so two views subscribe once.
- **A notice is empty.** `Notice` in `bus.proto` has no fields. It says that something
  under its topic changed, never what. The page re-reads the owning HTTP route, where
  masking already happens. SPEC §bus.topics, SPEC §remote.security.
- **Coalesce the re-read.** 150 ms after the last notice, never more than 1 s after the
  first, one read in flight and at most one queued, held while the document is hidden.
  The implementation both Remote pages share is `coalescedRead` in
  `crates/swiss-panel/panel/src/views/remote-live.ts` (`SETTLE_MS = 150`, `CAP_MS = 1000`).
  Its comment says it is the Remote pages' helper. A second page family should reuse this
  function or move it, not invent a second coalescer with different numbers.
- **Publish only when the state moved, while the write is still held.** A housekeeping
  pass that changed nothing publishes nothing, so a read cannot feed itself. Events on
  one topic leave in commit order. Publishing never waits on a slow link.
- **An event that can carry the record does.** `HOST.RUNS` carries `RunChanged`: run id,
  state, action, owner, actor, lane, queued/started/ended times, exit code. It does not carry
  input, label, error text, or output. A page that needs those re-reads the route. `revision`
  is 1 queued, 2 running, 3 terminal. The run view's `ms` is `ended_at - started_at`
  (`runs.rs`, the terminal view), so a duration can be computed from the event; only the
  error text needs the GET.

`coalescedRead` is the pattern the Remote pages proved. DATA.CONSOLES is the other shipped
shape: the record itself travels on the bus (`ConsoleSaved` and friends), and the writes
are bus requests. SPEC §bus.registry. Poll replacement so far has been notices plus the
existing GET, not "delete the HTTP route". `event-bus.md` §1, decision 3 ("one step all
the way"), said writes go over the bus and the HTTP routes they replace are deleted. The
Remote commits replaced reads, not writes, and did not delete
`GET /api/remote/runs` or `GET /api/remote/targets`. Follow the shipped Remote pattern
unless a judgment says a particular route should become a bus request.

## What is already on the bus

Registry as generated today (`crates/swiss-bus/proto/swiss/bus/v1/`):

| Module | Sub | Id | Body | Key |
|---|---|---|---|---|
| 0 BUS | LINK | HELLO, PING, BYE, BUILD | link messages | — |
| 0 BUS | TOPIC | SUBSCRIBE, UNSUBSCRIBE | `Topics` | — |
| 4 HOST | 1 RUNS | CHANGED | `RunChanged` | action namespace, the type up to the first dot |
| 4 HOST | 2 REMOTE_SEAT | CHANGED | `Notice` | `""` |
| 6 DATA | 1 CONSOLES | LIST, PUT, REMOVE, CLEAR_CLOSED, CHANGED, REMOVED, CLEARED | console records | connection |
| 10 REMOTE | 1 TARGETS | CHANGED | `Notice` | `""` |
| 10 REMOTE | 2 HISTORY | CHANGED | `Notice` | `""` |
| 14 PANEL | 1 PREFS | CHANGED | `Prefs` relay | — |

`event-bus.md` §7 also planned, and the code did **not** do:

- live output of an open Remote run on `HOST.RUNS` (the open row still polls);
- `CANCEL` as a bus request (`POST /api/runs/{id}/cancel` remains);
- `HOST.MEMORY`, `HOST.PLUGINS`, `MCP.SERVERS`, `DATA.ACTIVITY`, `DATA.ANALYSIS`,
  `DATA.REDIS`, `PROCESS.APPS`, `PROCESS.LOGS`, `JOBS.RUNS`.

`HOST.RUNS` is published by `RunCoordinator::announce` in
`crates/swiss-host/src/services/runs.rs` for every state a run enters, under the
bookkeeping lock. Jobs submit through that coordinator, so a job run already produces
`HOST.RUNS`. The Jobs page does not subscribe. The key is `action.split('.').next()`.
The legacy job action is `process.legacy-command` (`JOBS_ACTION` in
`crates/swiss-jobs/src/jobs/mod.rs`), so those runs use the key `process`. A job whose
stored action type is something else uses that type's first segment. Subscribing the
Jobs page only to `process` misses those jobs. Subscribing to key `""` hears every
namespace, including `remote`. But every job run, scheduled or Run now, is submitted with
`owner: "jobs"` (`JOBS_OWNER`, same file, both `gated_submit` sites), and `RunChanged`
carries `owner`. A page on key `""` can drop every event whose owner is not `"jobs"` before
it reaches the coalescer, so a remote run costs the Jobs page nothing.

`HOST.REMOTE_SEAT` is fired from `TunnelManager::remote_seat_changed`. Call sites found:
connection state (`crates/swiss-tunnels/src/tunnel/manager/pool.rs`, the `on_state` hook),
connection create (`tunnel/api.rs`, `POST /api/tunnels/connections`), connection edit
(`manager/edits.rs`, `apply_connection_update`) and connection delete (`manager/edits.rs`,
`delete_connection`). No rule operation calls it: not `start_rule` / `stop_rule` in
`manager/lifecycle.rs`, not `apply_rule_update`, not `delete_rule`. The Remote Targets page
listens. The Tunnels and Forwards pages do not, and still poll the whole table. A Tunnels
page that only subscribed to `HOST.REMOTE_SEAT` would miss every rule change.

## The fourteen commits this exploration started from

The fourteen commits under the two workspace-split commits of 2026-11-10 (`1f127657`,
`e3da39c2`, panel layout only), that is `git log HEAD~2 -14`, oldest first. Two threads,
interleaved. Neither changed the global tick except to stop the Remote pages from using it.

Remote, off the 6 s tick:

| Commit | What it settled |
|---|---|
| `42e9e41a` | A run already in the record is left out of `active`, so one GET cannot list it twice during the history-ack window. |
| `f063eda0` | `HOST.RUNS` / `RunChanged`. |
| `ecb43301` | `HOST.REMOTE_SEAT`. |
| `d280dc77` | `REMOTE.TARGETS` on a successful `remote.json` write. `REMOTE.HISTORY` when the record moves for a reason a run event does not carry: a row filed, Clear, retention that removed something, the open-at-start import. A pass that changed nothing is silent. |
| `521fc953` | Runs and Targets subscribe, then re-read, coalesced. Targets' `poll()` only rewrites relative times. Runs exports no `poll`. An open live row still follows output every 1.5 s. The global tick stays for every other page. |
| `90a74c85` | Live-verify text: the ninth concurrent remote exec is HTTP 429, not a queued row. |
| `619c2c8d` | Vitest pins subscribe-before-read, one coalesced re-read per extra WELCOME, unsubscribe on unmount. |
| `992b33e2` | Clear publishes when the database removed rows even if the spill ledger was empty. |
| `ce5e1396` | A targets notice repaints the Runs filter. A run event does not re-read `/api/remote/targets`. A burst of terminals goes through the coalescer. `history()` attaches its notice once. Proto names straightened; `Notice` lives in `bus.proto`. |
| `2697e147` | Codegen digest restamped. The previous stamp had included a stray empty proto. |
| `8a3c5dd6` | Targets re-reads the newest page on a run or record event and keeps a per-target answer until that answer can be wrong. A vanished Runs filter target falls back to All inside the same read. |

Tunnels self-heal, not the bus: `9f640376` (a channel open waits at most 10 s),
`7f2a5964` (three unanswered opens judge the session wedged), `31abc63d` (one liveness
probe before that judgment; a late answer closes its channel).

SPEC that moved with the Remote thread: §bus.registry, §bus.topics, §host.actions,
§host.seats, §remote.targets, §remote.history, §remote.panel ("Live without a poll"),
§remote.verify, and the tunnels self-heal section. `event-bus.md` §7 was not edited to
match what actually shipped. Treat §7 as the original wishlist, and §remote.panel as
what the owner accepted for output.

## The global tick, and who else has a timer

`crates/swiss-panel/panel/src/main.ts`, `setInterval(poll, 6000)`, skipped while the
document is hidden, and run again when it becomes visible.

```text
function poll() {
  loadMemory(true);   // GET /api/memory?tree=1     polling.ts
  loadInfo();         // GET /api/info, then the token list
  pollPage();         // the active page's poll(), if it has one
}
```

`loadInfo`'s comment says the panel stamp arrives on WELCOME, and this poll stays until
the token list has its own phase. `page-registry.ts` `pollPage` calls `active.module.poll`
for the page on screen only, and drops a tick that arrives while the previous one is
still running.

So a hidden page does not poll. A visible page that is not the active page does not get
`pollPage` either. The only reads every visible tab makes every 6 s are memory and info
(plus tokens inside info). Plugin enablement in another window is explicitly not on the
bus: SPEC §remote.panel says those pages learn it from their next read.

### Pages whose `poll()` still hits HTTP

All of these are every 6 s, only while that page is active and visible.

| Page | `poll` | What it reads |
|---|---|---|
| Servers | `views/mcps.ts` | `GET /api/mcps`. If the open detail is the Logs tab, page 0, and no page switch is in flight: `GET /api/mcps/{name}/calls`. Page > 0 does not poll (SPEC §mcp.calls). |
| Traffic | `views/traffic.ts` | `/api/mcps` and the traffic list. |
| Tunnels, Forwards | `views/tunnels.ts`, `views/tunnel-forwards.ts` → `tunnels.ts` `pollTunnelsPage` | `/api/mcps` and the tunnels table (`loadTunnels(true)`). |
| Jobs | `views/jobs.ts` | `loadJobs(true)`. |
| Plugins | `views/plugins.ts` | plugin inventory, then patch. |
| Terminal | `views/terminal.ts` | the session list (bytes out, attached dot). The PTY bytes are a different socket. |
| Secrets | `views/secrets.ts` | the secret list, then patch. |
| Tokens | `views/tokens.ts` | the token list, then patch. |
| LLM History | `views/llm-history.ts` | calls list and runs list. Patches; does not rebuild, so a search box keeps focus. |
| LLM providers, agents, embedding | the three view files | each re-reads its status. Embedding leaves the form alone when the operator is editing. |
| Process | `views/process.ts` | `GET /api/process`, skipped when the document is hidden. This is a second cadence on top of the page's own timer below. |
| Remote Targets | `views/remote.ts` `poll` | `patchCols()` only. No HTTP. Rewrites relative times from data the page holds. |

Runs has no `poll`. Memory's pages (`views/memory*.ts`) have no `poll`. The Data grid has
no 6 s `poll`.

### Timers that are not the 6 s tick

| Where | Interval | What it hits |
|---|---|---|
| `views/process.ts` mount | 500 ms tick. `load()` every tick while a row is busy, else every 4th tick (2 s). `loadChildren()` every 10th tick (5 s). | `GET /api/process` and the children route. SPEC §process.panel states 2 s / 500 ms / 5 s. The status is computed on each GET from ports and pids (`crates/swiss-process/src/status.rs`); it is not a transition the supervisor pushes. |
| `views/process/logs.ts` | 1 s while Follow is on | log tail with `after`. |
| `jobs.ts` `runJob` (the top-level `panel/src/jobs.ts`, not `views/jobs.ts`) | 1 s, at most 1800 times | `GET /api/runs/{id}` until the state is terminal, then one `loadJobs`. The toast uses `ms` and `error`. `RunChanged` carries no `error`; `ms` is `ended_at_ms - started_at_ms`, both of which it carries. |
| `detail.ts` `loadCalls` via the Servers poll | 6 s, page 0 only | calls. The same response also carries proc stderr (`j.stderr`). |
| `data-activity.ts` | 5 s while the Activity tab is in front | activity list. |
| `data-pubsub.ts` | 5 s while the Pub/Sub tab is in front | the listing. The feed itself is a separate socket-style subscription (`dbPubsubOpen`), not this poll. |
| `data-stream.ts` | 1 s, 2 s, or 5 s while Follow is on | Redis stream read. Groups fold every 5th tick. |
| `data-analysis.ts` | 1 s during an execute, and again while accepted executions are still running | the analysis task. |
| `views/llm-history.ts` `readRun` | 3 s while the open run's `live` flag is set | one run's dialogue. |
| `views/memory-sync.ts`, `views/memory-auto.ts` | 3 s (`FOLLOW_MS`) while a queued or running sync/review is on an open sheet | that sheet's overview. |
| `views/remote-runs.ts` `armLive` | 1.5 s (`LIVE_EVERY_MS`) while a live row is expanded | that run's output cursor. A terminal result calls the coalescer, not `refresh()` directly. The comment on the constant still says "the 6 s poll moves the list"; the list has since moved to the bus. |

These repaint from data already on the page and were not counted as backend polls:
Targets' `poll`, the Redis TTL tick (`data-browsers.ts`, 1 s), the LLM stream "waiting"
clock (`llm-stream.ts`, 1 s). Debounced search, toasts, and focus timers are not polls.

## Proposed split

This is the exploring model's recommendation. Re-judge it. "可以" means swiss already
commits the change, or already publishes it, so a notice (or an existing `RunChanged`)
plus a coalesced re-read removes that timer. "一般" means it can be done, and it is what
removes the global tick or serves a second window, but the acting page often already
refreshed from the write response, or the gateway must sample while someone is
subscribed. "没必要" means moving it onto the bus keeps a timer, or the timer never
called the backend.

| Place | Now | Split | Why |
|---|---|---|---|
| Jobs run state, Run now | 6 s list; 1 s `GET /api/runs/{id}` | 可以 | `HOST.RUNS` is already published. The page does not listen. Filter on `owner == "jobs"`. On terminal, one GET still has to fetch `error`; the duration comes from the event's times. |
| Jobs definitions | inside that 6 s list | 可以 | Publish when the jobs document is written. Relative "next run" can be repainted locally, the way Targets repaints "last run". |
| Tunnels, Forwards | 6 s whole table | 可以 | Connection state already notifies the seat. Rule start/stop does not. One tunnels notice has to cover both, or the pages will still poll. |
| Plugins | 6 s inventory | 可以 | Enable, disable, and a failed start are in-process. Today only the Plugins page polls this, so another window's nav waits until something else re-reads. SPEC §remote.panel records that gap. |
| MCP list and health dots | 6 s `/api/mcps` from Servers, Traffic, and Tunnels | 可以 | Keep the probe lazy. `Registry::watch` runs the 15 s health probe only while health is being read, for two more periods, then health returns to Unknown (`crates/swiss-mcp/src/registry.rs`, SPEC §mcp.registry). A subscription must call `watch` the way a GET does, and must not start the probe at boot. `http` and `rest` adapters have no ping and must not gain one. `GET /api/mcps` itself calls `watch_fresh`, so a health notice that triggers that GET re-arms the probe: see the loop under "Facts". |
| MCP Logs page 0, and Clear | 6 s `/calls` while page 0 is showing | 可以 | `CallLog::record_call` (`crates/swiss-mcp/src/calls.rs`) is the entry point. With the module database attached (`src/builtin.rs`, the normal case: `logs/mcp.db` is the authority, SPEC §mcp.logs) it hands the row to `McpHistory::record_call` (`crates/swiss-mcp/src/history.rs`), one queued transaction through `store.submit`; the jsonl queue is the fallback when no database is attached. Publish when that write commits, on either path, and when the log is cleared. Key by MCP name. The reply body stays on `GET …/calls/{seq}`. Page > 0 stays still, which the spec already requires. |
| Traffic list | 6 s | 可以 | `TrafficLog::record_traffic` (`crates/swiss-mcp/src/traffic.rs`, called from `src/app.rs`) already runs for every `/mcp/*` request and persists through `McpHistory::record_traffic` when the database is attached. Publish when that write commits. A notice, then one coalesced re-read of the list. Do not push a frame per request for the page to paint. Rows already omit bodies for this reason (SPEC §mcp.traffic: 200 rows times a 6 s poll was megabytes a minute). |
| LLM History lists | 6 s calls and runs | 可以 | A call or run that appears from elsewhere. Same shape as Logs. |
| Secrets | 6 s | 一般 | The page that saved already reloads. The bus is the other window and the CLI. |
| Tokens | 6 s, and again inside `loadInfo` | 一般 | Same as secrets. This is also the read that `loadInfo`'s comment is waiting to retire. |
| LLM providers, agents, embedding | 6 s each | 一般 | A successful save already refreshes the acting page. |
| Process apps and children | 2 s, 500 ms while busy, children 5 s, plus the 6 s `poll` | 一般 | Nothing in-process commits "this pid appeared". Each GET walks ports and pids. The honest bus shape is: while a page is subscribed, the plugin samples once on today's cadence and publishes a notice; several windows share that sample; nobody subscribed, no sample. |
| Memory chip | every visible tab, 6 s, `/api/memory` | 一般 | A gauge. One sample, one notice, every open page. This is the read that lets `setInterval(poll, 6000)` go away after the per-page polls are gone. Doing it alone leaves the tick in place. |
| proc stderr on the Logs tab | carried by the `/calls` poll | 一般 | A ring buffer filled by the child pipe (`adapters/proc.rs` `pump_stderr`), not a recorded call. A `record_call` notice does not fire when stderr grows between calls. Publish when the ring grows, to a page subscribed to that MCP's logs. |
| Data analysis execution | 1 s while an execution is running | 一般 | The task is in-process. Publish when its status changes. |
| Terminal session list | 6 s | 一般 | Open, close, and attach are events the gateway has. `bytesOut` is a counter; it can ride the same notice. |
| Open Remote run output | 1.5 s | 没必要 | Deliberately left as a follow in `521fc953`. The list is already live. `event-bus.md` §7 still says the opposite; the spec and the code are the later decision. |
| Terminal PTY bytes | the terminal WebSocket | 没必要 | SPEC §terminal.api and `event-bus.md`: that socket is not the bus. |
| Process log Follow | 1 s | 没必要 | A tail. |
| Open LLM dialogue | 3 s while `live` | 没必要 | The body is a stream. The list can move; the open body stays a follow. |
| Memory sync / review follow | 3 s while a run is queued or running | 没必要 | Following one in-progress run on an open sheet. |
| Data Activity | 5 s | 没必要 | The page is asking the database. A bus topic would still need that query. `event-bus.md` §7 lists `DATA.ACTIVITY` anyway. |
| Data Pub/Sub listing | 5 s | 没必要 | The page is asking Redis. The live feed is already not this poll. |
| Data Stream Follow | 1 / 2 / 5 s | 没必要 | Same: a follow of Redis. |
| Targets `poll`, Redis TTL tick, LLM wait clock | 1 s local | 没必要 | No backend read. |

MCP Logs was missed in the first pass of this exploration and then added. It is a recorded
row, the same shape as Remote history, not a byte stream. The sentence "do not push one
frame per request" constrains the body, not the topic: the notice is empty, and the page
re-reads page 0.

## Facts that are easy to get wrong

- The global tick is not "every page, every 6 s". Memory and info are. `poll()` is the
  active page only.
- Process is polled twice while its page is showing: its own 500 ms timer and the 6 s
  `poll`.
- Health probes exist only because something called `watch()`. Stopping the `/api/mcps`
  poll without a subscriber calling `watch()` parks the probe and shows every Started
  server Unknown. Starting the probe at boot was a bug that kept a pool per database for
  the life of the process. SPEC §mcp.registry.
- The health probe can feed itself. `GET /api/mcps` calls `watch_fresh`. When nobody
  watches for two intervals (30 s), the probe loop's `forget_health` turns every row
  Unknown. If that change published a notice and the notice made the page re-read
  `GET /api/mcps`, the re-read would re-arm the probe, the probe would turn the rows Up
  again, that would publish, and so on. One subscribed page would flap Unknown/Up every
  ~30 s and the probe would never park. Whatever holds the probe for a subscriber must
  hold it continuously, so the lapse cannot happen under a visible subscriber.
- `http` and `rest` MCP adapters report Unknown on purpose. Do not add a ping.
- `RunChanged` has no error string. It has no `ms` field either, but the run view's `ms`
  is `ended_at_ms - started_at_ms`, and the event carries both. The Jobs toast reads only
  `error` from `GET /api/runs/{id}` once this is used.
- Job action keys are not all `process`. Only `process.legacy-command` is. The field on
  a definition is `action.type_`. The owner of every job run is `"jobs"`, and the event
  carries it.
- `HOST.REMOTE_SEAT` is the remote transport seat (endpoint dots, tunnels presence on
  the Targets page). It is not a "tunnels table changed" event. It fires on connection
  create, edit, delete and state; no rule operation fires it.
- A refused run never becomes a run and does not publish. Remote's ninth concurrent exec
  is 429 and must not appear on the page.
- Clear of remote history publishes when either store removed rows, and publishes nothing
  when both were empty.
- Call-log append is deliberately not awaited by the tool call. With the module database
  attached, the row is one queued transaction (`McpHistory::record_call`, `store.submit`)
  and `read_calls` flushes the store first; without it, the jsonl queue does the same. A
  notice published before the write commits would make the page re-read and miss the row.
  The Remote terminal event waits for the history ack for the same reason. Match that:
  publish once the row is visible through the GET, from inside the committed write, on
  both paths. Traffic is the same (`McpHistory::record_traffic`).
- Logs page 0 and an offset page are different. A notice must not refresh an offset page
  out from under the reader. SPEC §mcp.calls.
- Proc stderr and the call list share one HTTP response today and are different events.
- Traffic list rows have no body. Do not "improve" the notice by embedding the body.
- The terminal session list and the terminal byte stream are different. Only the list
  is a candidate.
- `coalescedRead` lives under `views/` and its comment names the Remote pages. Reusing
  it from Jobs or MCP is a small move, not a new mechanism.
- Panel source is `crates/swiss-panel/panel/src`. The files under
  `src/admin_assets/js` are the committed emit. Edit the TypeScript and run the panel
  build. Do not hand-edit the emit.
- A new message is a `.proto` change, both codecs regenerated in the same commit, the
  golden frame, and the SPEC registry row. Numbers are never reused.
- This tree is hand-formatted. Do not run `cargo fmt`.

## What the next model should decide

These are the forks. The recommendation above is one way through them.

1. **Scope of the first batch.** The "可以" rows are independent topics (jobs, tunnels,
   plugins, MCP, traffic, LLM history). One topic per commit matches how Remote landed.
   Doing all of them before deleting the tick matches the owner's sentence. Deleting the
   tick before Tokens, Secrets, the LLM config pages, Process, and the memory chip is
   not possible: those `poll` functions and `loadMemory` / `loadInfo` are what the tick
   calls.
2. **Whether "一般" is in scope.** If the goal is "no 6 s tick", yes. If the goal is
   "a change made elsewhere shows up at once", Secrets, Tokens, and the LLM config pages
   are the weak ones: the window that wrote already updated itself.
3. **Jobs and `HOST.RUNS`.** Listen to the existing event, and also add a jobs-document
   notice for definitions. Subscribe on key `""` and drop events whose `owner` is not
   `"jobs"` before the coalescer. Per-namespace keys taken from the loaded definitions are
   not needed. The Run now loop can stop; the duration comes from the event's times, and
   one terminal GET for `error` remains, because SPEC §host.actions forbids free text on
   the event.
4. **Tunnels topic versus reusing `HOST.REMOTE_SEAT`.** Reuse is not enough for rule
   start/stop. A new sub on module 7 (tunnels' chapter) fits the numbering rule better
   than overloading the host seat, whose comment says it is the remote seat.
5. **MCP: one topic or several.** List/health, call log, traffic, and stderr change for
   different reasons. One "MCP changed" notice makes every open Logs page re-read on a
   health flap, and every sidebar re-read on a call. Keying the call log by MCP name
   avoids that. Health can stay one topic with key `""`.
6. **Who calls `watch()`.** The bus subscriber count is the natural "someone is
   reading health". A page that is subscribed and hidden should be defined: Remote holds
   the re-read but stays subscribed. Keeping `watch()` alive for a hidden subscriber
   continues the 15 s probe. Dropping it returns health to Unknown and makes the next
   focus wait on `watch_fresh`. Pick one and write it into §mcp.registry. Whichever is
   picked must also break the loop under "Facts": the lapse to Unknown must not happen
   under a visible subscriber, or must not cause a re-read that re-arms the probe.
7. **Process and memory samplers.** A sampler that runs only while the subscriber count
   is non-zero preserves the idle-memory rule. A sampler that runs for the life of the
   process does not. Process today already samples only while its page is mounted; moving
   the sample into the plugin is worthwhile when two windows would otherwise both walk
   the port table. One window does not, by itself, justify the topic.
8. **Data Activity, Pub/Sub, Stream, and `event-bus.md` §7.** The brief lists them.
   This exploration says no, because the gateway would still poll Postgres or Redis.
   The exception already half-built is the pub/sub feed, which is not the 5 s listing
   poll. Analysis is the one Data timer aimed at in-process state.
9. **Do not reopen Remote output.** §7 says put the live tail on `HOST.RUNS`. The owner
   accepted a 1.5 s follow after that brief. Implementing §7's output row would undo
   `521fc953`.
10. **Shared coalescer.** Move `coalescedRead` to `panel/src/bus/` when a second module
    needs it, and fix the Remote-only comment. Keep the two constants.

## What was not done

No code was changed for this question. The gateway was not walked. Request rates were
not measured on a live panel. Publish sites were confirmed by search for the symbols
named above (`announce`, `remote_seat_changed`, `record_call`, `watch`, `JOBS_ACTION`),
not by reading every writer of every JSON file. The second pass re-read the registry, the
global tick, every `poll` and every timer listed here, `announce`, the job submit sites,
the seat call sites, `Registry::watch` / `start_timer`, the `/api/mcps` handler, and the
call-log and traffic writers; the corrections above came from that pass. Before implementing a row, re-read the
writer that would publish and the `poll` that would be deleted, and add the test that
fails if the notice is dropped. Remote's own rule was: a behaviour change ships with a
test that failed before it.

Panel work also needs the panel gate (`npm run check` in `crates/swiss-panel/panel`)
and, for anything the eye has to believe, the test instance on port 19998. Port 19999
is the owner's running panel and is not a verification target.

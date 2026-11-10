# §process — Processes and the supervisor

Chapter 0011 of the [swiss specification](../SPEC.md).

How the gateway starts, watches, captures and tears down the programs it runs, and the one
page where every one of them is seen and stopped. Four layers, all on top of the platform calls
in `swiss_core::platform`:

- The **process plugin** contributes two capability actions (§process.actions) and the
  Process page: the **managed apps** (§process.apps) and the **children view**
  (§process.children).
- The host's **run coordinator** (§host.actions) queues and deadlines the runs.
- The host's **supervisor** spawns and reaps every child that dies with the gateway: the jobs'
  one-shot commands and the `proc` MCPs' resident children alike (§process.supervisor).
- The host's **`spawn_detached`** starts the one kind of program that must outlive the
  gateway: a managed app (§process.detached, ADR-036).

| Where | What |
|---|---|
| `crates/swiss-host/src/services/actions.rs` | `process.exec`, `process.legacy-command` |
| `crates/swiss-host/src/services/process.rs` | The supervisor: bounded capture, spawn, teardown, the legacy tokenizer, output decoding |
| `crates/swiss-host/src/services/detached.rs` | `spawn_detached` and the PATH it builds (§process.detached) |
| `crates/swiss-host/src/services/children.rs` | `ChildRegistry`, the one list of children that die with the gateway (§process.children) |
| `crates/swiss-host/src/services/runs.rs` | `RunCoordinator` |
| `crates/swiss-host/src/proc_pids.rs` | The port-scoped PID ledger and boot settlement |
| `crates/swiss-core/src/platform/` | Job objects (`job.rs`), Toolhelp32 walks, working sets, the listener table and creation times (`windows.rs`), process groups and `setsid` (`unix.rs`), the `/proc` parsers (`procfs.rs`, un-`cfg`'d so any host tests them), the shared descendant walk (`mod.rs`) |
| `crates/swiss-process/src/` | Managed apps: `config.rs` (schema, validation), `status.rs` (the pure verdict), `facts.rs` (one request-local listener/owner snapshot), `control/` (start, stop, restart, start with swiss; the production seams in `seams.rs`), `history.rs`, `logs.rs` (capture file, reading, masking), `api.rs` (the `/api/process` routes) |
| `crates/swiss-plugins/src/process.rs` | `ProcessPlugin`: the factory, the two actions' registration, mounting the routes |
| `src/process_cli.rs` | `swiss process …` (§process.cli) |
| `crates/swiss-panel/panel/src/views/process.ts` | Inventory, row actions and mount-owned polling (§process.panel) |
| `crates/swiss-panel/panel/src/views/process/` | Shared types, session-owned editor/picker, the Add sheet's templates (`templates.ts`) and log follow/history |

**The plugin** — id `process`, label Process, version 0.2, config schema version 2,
`restart_on_config_change: false` (config applies in place, §process.apps), `requires: []`.
One page, `process` (order 55, layout `page`, §panel.nav), and one route prefix,
`/api/process`, in the extra tree (§host.routes), mounted the way the terminal plugin mounts
its routes (`crates/swiss-plugins/src/terminal_api.rs`: a state slot installed on start, withdrawn on
stop). While the plugin is not active every `/api/process` route answers the structured 503
that names it.

- **Registered last, so it starts first** (boot walks the registration list in reverse): a
  capability provider is online before anything can submit a run against it. The factory
  lives in `crates/swiss-plugins/src/process.rs`; its register line stays the last one in
  `src/builtin.rs`.
- **The capabilities are isolated from the apps.** Start registers both actions first, exactly
  as before; nothing in the apps half can fail them. An app entry that boot validation
  refuses is dropped with a warning (§process.apps), never the plugin.
- **Start registers both actions or neither.** If the second registration fails, the first is
  rolled back — no plugin reports active with half its capabilities.
- **Stop withdraws, then cancels.** Both actions leave the registry first (a new submission
  gets 400 "unknown action type" instead of silently doing nothing), then every run already
  executing one of them is cancelled and **awaited** — tree killed, readers joined — before
  stop returns. Runs of other capabilities are untouched, and producers keep running: a
  disabled process plugin leaves the Jobs scheduler ticking, and its process jobs report the
  missing capability (§jobs).
- **Stop leaves the apps running.** They belong to the operator. The plugin scope's tasks end:
  an exit watcher drops its handle without killing (the app's exit is no longer recorded), an
  action in flight is abandoned and recorded in the history as `abandoned: plugin stopped`,
  a pending start-with-swiss retry is cancelled. The tracked record stays on disk, so a
  started plugin reads the app as running again (§process.apps).

## §process.actions — The two actions and their rules

| Action | Input (strict; unknown keys 400) | References |
|---|---|---|
| `process.exec` | `{program, args?: [string], cwd?, env?: {NAME: string}}` — `program` required and non-empty | **Strict**: a missing `${VAR}` is an error naming the variable, never its value |
| `process.legacy-command` | `{command, cwd?, env?}` — the pre-v2 command string | **Lenient** for `${VAR}` (unset → empty, the historical meaning); `${secret://…}` stays strict |

The rules, which every future process-like capability inherits:

- **program + args, never a string to split.** Each argument is substituted on its own and
  stays **one** argument: a value containing spaces never becomes several. `program` resolves
  through `PATH`.
- **No shell by default.** A shell is an explicit `cmd /c …` or `sh -c …` in the command itself,
  never something the gateway adds. A config file is a code-execution boundary: nothing runs
  from viewing a page or from an import preview.
- **References resolve at execution time**, through the one resolver (§host.refs). Neither a
  resolved snapshot nor an error log carries a value back out.
- **Masking.** Every value a reference resolved to (8 bytes or more) — env or vault, strict or
  lenient — is registered and masked in the captured output. A program that re-encodes its own
  secret cannot be caught this way, which is why output access and file modes still matter
  (§security).
- **The legacy tokenizer is the compatibility contract.** `process.legacy-command` expands the
  references first, then splits with the old rules: single and double quotes group; on Windows
  a backslash is a path separator, not an escape, and only `\"` and `\\` escape inside double
  quotes. It is a port kept for its exact meaning (a maintained splitter would change what
  existing Windows commands mean) — not a model for new code; new capabilities take typed argv.
- **One deadline owner.** The actions hand the supervisor a "never" deadline
  (`COORDINATED_TIMEOUT_MS`); the coordinator owns deadline policy and cancels through the
  handle, so the tree kill and the reader joins all happen inside the supervisor's cancel path.
- Every action declares its input schema (the Jobs form builds its input editor from
  `GET /api/actions`) and whether it can be cancelled.

## §process.runs — The coordinator's invariants

The route contract is §host.actions. Underneath it, `RunCoordinator` keeps three invariants:

- **Bounded.** Concurrency and the queue both have caps (the Jobs plugin's
  `maxConcurrentRuns`/`maxQueuedRuns` reset the shared pool; defaults 2 and 32; remote runs
  have their own per-target lanes, §remote.actions). Over the cap is a visible capacity error
  naming it — never an invisible queue. `queueIfBusy` asks to wait in the queue instead of
  being refused when the pool is busy.
- **Owner-scoped.** Every run has an owner (`manual` for `/api/runs`, `jobs`, `remote`,
  `remote-mcp`). `cancel_owner` (stopping Jobs cancels only jobs' runs), `cancel_action_types`
  (stopping a provider), and `shutdown_all` (the gateway's own wind-down, before exit) are the
  only mass cancels. No ownerless detached task exists.
- **First-wins.** In the race between deadline, cancel and exit exactly one writes the
  terminal state; a late cancel is an idempotent no-op answering with the terminal view.

Also: a deadline never drops a task — it cancels, and the run reports `timedOut`. Only actions
whose `cancelable()` is true are accepted (one that cannot stop would hold a slot forever,
the very case the deadline exists to prevent). A panicking action is caught into one failed
run whose error says so; the slot is released and the pool is not poisoned.
`count_for_label` gives the jobs scheduler its overlap decision. The output buffer of a run
exists from the moment its id is committed, so a queued run can already be followed.

## §process.supervisor — Spawn, capture, teardown

`Supervisor::run` owns the whole life of one child:

- **Spawn.** stdin null, stdout and stderr piped, `kill_on_drop`. Windows adds
  `CREATE_NO_WINDOW` (a console-less daemon must not flash a console per child) and assigns the
  child to a kill-on-close job object at spawn (`KillOnCloseJob::assign`, ADR-008). Unix starts
  a private process group (`process_group(0)`), so one `kill(-pgid)` reaches any depth. A
  `ChildGuard` registers the root pid in the children registry (§process.children) with the
  owner, label and stop hook the caller passed, and unregisters on drop — a panic leaves no
  stale entry.
- **Capture is bounded while reading.** Two reader tasks feed a fixed-size tail buffer as bytes
  arrive (16 KiB per pipe by default); a chatty child can never balloon the gateway first and be
  trimmed later. Never `read_to_end`.
- **The endgame** is a biased select with cancel first: when a cancel and a natural exit land on
  the same tick, the cancel is the outcome the caller asked for. On timeout, cancel or a wait
  failure the tree is killed — Windows drops the job handle (the whole tree dies with it), then
  the root is killed and reaped within 3 s; Unix kills the group, then the root, and waits the
  same 3 s. No zombies.
- **Readers are joined, never detached.** After the child dies the readers wait for the pipes to
  close; a descendant that inherited a handle and keeps it open costs at most 5 s
  (`DRAIN_GRACE`), then the readers are aborted and reaped. Every byte already read is kept.
- **Output.** stdout, then `[stderr]` and stderr. Bytes that are not UTF-8 — typically a Chinese
  Windows console's GBK — are decoded as GBK (`encoding_rs`), else lossily. Resolved reference
  values are masked before the text leaves the supervisor.
- **`ok` means exactly** "exit 0 within the deadline and not cancelled"; every other ending
  carries its own flag (`timedOut`, `canceled`, `error`, `outputTruncated`).

A resident `proc` MCP child is not a run and does not enter the coordinator (§mcp.proc), but it
uses the same spawn flags, the same job object, the same PID ledger and the same children
registry: lifecycles separate, platform mechanics shared. A managed app uses none of them
(§process.detached).

## §process.tree — Process trees on each platform

- **Windows.** One kill-on-close job object per child tree: when the handle closes — clean
  exit, crash, End Task, a debugger detaching — the OS kills the tree. PID-reuse safe by
  construction. The walk (for the memory reading, the boot settlement and a managed app's
  hard kill) is a Toolhelp32 snapshot; working sets come from `GetProcessMemoryInfo`. No
  PowerShell anywhere (§arch.rules).
- **Unix.** A private process group per child; teardown is `kill(-pgid)`.
- **The shared walk** (`walk_descendants`) is one breadth-first pass with cycle protection and
  an "exclude my own pid" option, used by the memory reading (§host.memory), the orphan
  settlement and the hard kill (§process.apps).
- **Parent links are checked against creation times.** Toolhelp's parent pid outlives the
  parent: once a pid is reused, the new process "adopts" the dead one's orphans, and a walk
  from it would reach strangers. An edge counts only when the child was created at or after
  its parent. Times are read only for the pids the walk reaches, never for the whole snapshot.
  An edge whose times cannot be read counts for the memory reading (a number) and does not
  count for a kill (an act). The walk takes the mode as a parameter (`WalkMode::Measure`,
  `WalkMode::Kill`), and `tree_kill` and the boot settlement pass `Kill`.
- **A kill is checked, never assumed.** `tree_kill` reports the pids that were still alive
  after it ran (an elevated process refuses `PROCESS_TERMINATE` to a non-elevated gateway);
  each caller decides what to say about them.
- **The PID ledger** (`.proc-pids-<port>.json`, §core.home) covers what the job object cannot: a
  child that outlived a previous generation of the gateway. A successful `proc` spawn records
  its pid, a clean close removes it (temp + rename, so a kill mid-write never tears it). At
  boot, before any `proc` MCP starts, the ledger is read and cleared and every live pid that is
  a descendant of the recorded tree and not this instance's is tree-killed; if the snapshot
  fails, nothing is killed this round (a stray left for next time beats killing a child this
  instance just started). Port-scoped, so two instances never reap each other (§host.daemon).
- **The local terminal** assigns its shells through the same two functions (§terminal.local):
  killing `swiss` leaves no run, MCP, shell or agent-runner child anywhere. Managed apps are the one
  declared exception: they are the operator's programs and outlive the gateway on purpose
  (§process.detached, ADR-036).
- **FFI `unsafe` lives only here**, in `swiss_core::platform`, with a SAFETY comment per item
  stating handle ownership and lifetime; no other crate calls Win32 or libc directly. The only
  other `unsafe` is edition 2024's `env::set_var`/`remove_var` — at process start before any
  thread exists (with a SAFETY comment saying so) and in tests. The `setsid` hook a detached
  spawn needs is platform code for the same reason (§process.detached).

## §process.apps — Managed apps

An app is an outside program the operator declares, cc-otel first. The Process page and
`swiss process` do five things with an app: read its status; start, stop and restart it;
start it when a daemon boots; show its logs; and keep a history of what was done to it. This
is the job the standalone `svc` supervisor did, now inside the resident gateway. **Nothing
watches an app between those moments.** A crash leaves it stopped until someone starts it
(ADR-036; §process.roadmap is how a watchdog is added later).

**Config** is `plugins.process.config`, schema version 2. A stored version-1 config (`{}`)
reads as no apps; there is no migration code.

```json
{
  "apps": [
    {
      "name": "cc-otel",
      "start": { "program": "cc-otel", "args": ["start"] },
      "stop":  { "program": "cc-otel", "args": ["stop"] },
      "ports": [4317, 4318, 8899],
      "image": "cc-otel.exe",
      "ui": "http://127.0.0.1:8899/",
      "autostart": true
    },
    {
      "name": "docs-preview",
      "dir": "~/dev/site",
      "start": { "program": "npx", "args": ["serve", "-l", "5173"] },
      "ports": [5173],
      "image": "node.exe"
    }
  ]
}
```

| Field | Rule |
|---|---|
| `apps` | At most 64 entries. Their order is the page's order and the start-with-swiss order |
| `name` | Required, unique, `^[a-z0-9-]{1,32}$`. It is the key in routes, the CLI and the history, and it names the capture file |
| `start` | Required: `{program, args?}`, typed argv, never a string to split (§process.actions). `program` must not be empty; `args` holds at most 64 entries |
| `stop` | Optional, same shape. Without it, Stop is a hard kill, and the row says so (`stopIsKill`) |
| `dir` | Optional working directory; defaults to the user's home. A leading `~` expands, and the result must be absolute, or the save is refused. A directory missing at start fails that start, naming the directory |
| `ports` | Optional, at most 16, each 1–65535. A port is unique within its entry and across entries (one port belongs to one app), and is never the port this gateway listens on |
| `image` | Required when `ports` is set, and refused without `ports`. It is the executable basename expected to own the ports: no path separators, compared case-insensitively. On Windows a name without an extension also matches `<name>.exe`. `swiss` and `swiss.exe` are refused |
| `log` | Optional path of the app's own log. `~` expands, and the path must be absolute. `*` and `?` are allowed in the last component only, and the newest match by mtime is the one read |
| `ui` | Optional `http://` or `https://` URL; any other scheme is refused |
| `autostart` | Boolean, default `false`: start the app when a daemon boots (see "Start with swiss" below) |
| `env` | Optional map of at most 64 names (`^[A-Za-z_][A-Za-z0-9_]*$`). Each value is a literal or a `${ENV_VAR}` / `${secret://name}` reference (§host.refs) |

- **References in `args` and `env` resolve at each spawn, strictly.** A missing `${VAR}` or
  vault name fails that start, naming the reference and the field, never a value: the
  `process.exec` contract. No existing resolver is strict for both families, so a new
  function is added beside `resolve_collect` in `swiss_core::secure::refs`:
  `resolve_strict_collect`, strict for env and vault references alike, which also collects
  the values it produced. Nothing is stored resolved.
- **Validation layers** (§host.config):
  - PUT runs `validate_config`. One invalid entry refuses the whole PUT, and every error names
    its `apps[i].field`.
  - Boot runs `validate_config_for_start`. An invalid entry is dropped with a warning naming
    it, and the rest load.
  - `config_warnings` reports what is storable but not runnable now: a `program` that does
    not resolve on PATH, or a `dir` that does not exist.
- **Saving config never runs anything.** Only an explicit Start or a daemon boot starts an
  app (§security). Config applies in place (`apply_config`):
  - A new entry appears with whatever status it reads.
  - A removed entry leaves the page and its tracked record is dropped; its process, if any,
    keeps running.
  - An action already in flight finishes against the definition it started with.

**Status** is one verdict per app, from a pure function (`status::verdict`) over facts gathered
once per read. Every row of this table is a unit test with no machine access.

| Declared | Facts | Status |
|---|---|---|
| `ports` | Some declared port is owned by a process whose image does not match | `conflict`, naming that owner's pid and image (the first such port). It wins over `partial` |
| `ports` | Every declared port is owned by a matching image | `running`; `pid` is the owner of the first declared port |
| `ports` | Some ports are owned by a matching image; nothing listens on the rest | `partial`; `downPorts` lists the rest |
| `ports` | Nothing listens on any declared port | `stopped` |
| none | A tracked record whose pid is alive with the recorded creation time and image | `running` |
| none | A tracked record whose pid is gone, or alive with another creation time or image (the pid was reused) | `stopped`, and the record is dropped |
| none | No tracked record | `unknown`: "not started by this swiss; declare ports to see it from outside" |
| `ports` | The listener table cannot be read (macOS, or a failed read) | `unknown`, with the reason |

- **Facts.** One listener-table snapshot answers every app at once (`tcp_listeners()`: every
  TCP socket in LISTEN, IPv4 and IPv6, on any local address, as port → owning pid). Image and
  creation time are read once per distinct owner. Uptime is now minus the creation time.
- **Nothing is probed while nobody asks.** Facts are gathered for each `GET /api/process`,
  each CLI status and inside an action, and at no other time.
- **The tracked record.** `.process-apps-<port>.json` in the home (§core.home), sealed
  (`statefile::write_secure_json`) and port-scoped, so a test instance never reads
  production's. Its shape is a bare map,
  `{"<name>": {"pid": 0, "createdAt": 0, "image": ""}}`, with `createdAt` in Unix
  milliseconds - no envelope, because the only reader is the next gateway of the same port.
  - An entry is written when a start leaves a process alive (a foreground app). A start that
    resolved through a launcher (exit 0, the child bound) pins the port's OWNER, never the
    dead launcher - the record is what the exit watcher follows.
  - It is removed when the exit watcher sees that process end, when a Stop succeeds, and when
    a status read finds it stale.
  - The record is the exit watcher's identity gate: an exit line lands only for the pid the
    record holds, so a stop command's own exit and a dead launcher's never become the app's.
    An app with `ports` is identified by its ports; for such an app the record carries
    the pid the exit watcher holds.

**Actions.** Every action takes the app's lock. A second action on a busy app answers 409
`{error: "busy: starting"}`, naming the action already running. The routes answer **202** at
once; no HTTP request waits for a program. The row carries `busy` (the action's name) until
the outcome lands, then `lastAction: {action, ok, message, at, canForce?}`. The same outcome is
appended to the history. The panel and the CLI poll `GET /api/process` to see it.

**Start**:

1. `running` answers ok: "already running (pid N)". `conflict` and `partial` are refused,
   naming what holds which port. `stopped` and `unknown` go ahead.
2. The start prepares and spawns:
   1. Resolve the references and expand `dir`.
   2. Append a header line to the capture file: `--- <RFC 3339> start by <by> ---`.
   3. Call `spawn_detached` (§process.detached).

   A spawn error fails the start at once with its own sentence: the program is not found on
   PATH, `dir` is missing, an argument cannot be passed to a batch file, or "requires
   elevation (os error 740)".
3. Then the start polls every 250 ms for at most 30 s (`START_WINDOW`), and the first row
   below that comes true decides the outcome. When the listener table cannot be read, an app
   with `ports` is judged by the rows for an app without them.

   | What happens | Outcome |
   |---|---|
   | The status reaches `running` (an app with `ports`) | ok: "running (pid N)" |
   | The spawned process exits non-zero | failed: "exited with code N", plus the last 20 lines of the capture file, masked |
   | The spawned process exits 0, and the app has `ports` | Polling goes on. The ports bind → ok. The window ends first → failed: "launcher exited 0 but nothing listens on 4318" |
   | The spawned process exits 0, and the app has no `ports` | ok: "exited 0; declare ports to track what it started" |
   | An app without `ports` is still alive 2 s after the spawn (`SETTLE`) | ok: "running (pid N)" |
   | The window ends with the spawned process alive and a port still not bound | failed: "started (pid N) but not listening on 4318 after 30 s; still running, not killed" |

4. **A start never kills what it started**, not when the window ends and not on a failure. A
   foreground app that is slow to listen is a slow app, and the operator decides what to do.
   A spawned process still alive when the start resolves goes into the tracked record. An
   exit watcher (a task in the plugin's scope, holding the child) appends
   `{action: "exit", exitCode}` to the history when that process ends, and drops the record.

**Stop**:

1. `stopped` answers ok: "not running". `unknown` is refused: "nothing identified to stop".
   `conflict` is refused: "port 4318 is held by node.exe (pid N), not cc-otel — not ours to
   stop". `running` and `partial` go ahead.
2. **With a `stop` command, and no `force`:**
   1. Append the header line (`stop by <by>`) and spawn the stop command detached into the
      same capture file.
   2. Wait up to 30 s for the stop command to exit. A completed command is never killed.
      An overrun is killed only if its spawn creation time and captured image still match
      a fresh read; unreadable or reused identities stay alive with a reported warning.
      The app status phase goes on either way.
   3. Poll the status every 250 ms for up to 15 s. `stopped` → ok. Still running → failed with
      `canForce: true`: "still running after its stop command; force kills pid N". A listener
      table that cannot be read never counts as stopped: the poll runs to its end and fails
      naming the unreadable table, `canForce` still offered.
3. **Without a `stop` command, or with `force`, the stop is a hard kill:**
   1. Re-resolve the target immediately before acting, never from an earlier read. For an app
      with `ports`, the targets are every distinct owner of a declared port whose image
      matches. For an app without, the target is the tracked pid, if its creation time and
      image still match. A pid read ten seconds ago may belong to someone else now.
   2. Refuse a target that is the gateway itself: its own pid, or any process whose image is
      the gateway's own exe name. A 19998 instance must never be able to kill 19999.
   3. Kill. Windows: `tree_kill`, walk mode `Kill` (§process.tree). Unix: `SIGTERM` to the
      process group, up to 10 s, then `SIGKILL`.
   4. Probe again. A survivor → failed: "could not terminate pid N (access denied — is it
      elevated?)". Otherwise poll up to 5 s for the ports to free, then ok.
4. Every hard kill is in the history with its fresh target's pid and image. Target resolution
   yields a typed creation-time/image identity; the sole killer-call boundary checks it again
   immediately before termination. Unreadable facts and any foreign owner refuse the whole
   set. The platform operation remains PID-based, not an atomic handle-bound compare-and-kill;
   final-read-to-syscall PID reuse is a residual platform-contract limit.

**Restart** is Stop, then Start, under one lock and one history line. The stop runs **without**
force unless the request asked for it. A stop that leaves the app running ends the restart
there, with the stop's answer (`canForce` included). The panel offers Force as a separate,
confirmed step.

**Start with swiss** runs once per gateway process, after every plugin has started, and only
on a **daemon** (`BootInfo.is_daemon`, §host.daemon).
- A daemon is a `serve` that `swiss start` spawned. That includes the start at sign-in, since
  it runs `swiss start --no-open`.
- A foreground `start -f` and a bare `serve` (the test instance) never start an app on their
  own. Enabling the plugin later or applying config never starts one either.
- In config order, one at a time, it starts with `by: boot` each `autostart` app whose status
  is `stopped`, or `unknown` because nothing is tracked (an app without `ports`).
- `running`, `partial`, `conflict` and an unreadable listener table are recorded in the
  gateway's boot log alone - never as failed history lines on an app nobody started - and
  left alone.
- A failed start gets **one** retry 60 s later, because a start at sign-in races the network
  and mapped drives. After that it is left to the operator.

**History** is `logs/process.db` in the home, on the §host.history seam (one store per
module, same seat as mcp.db, remote.db, terminal.db and llm.db), one row per outcome:
`{ts, app, action: start|stop|restart|exit, by: panel|cli|boot|plugin, ok, message, pid?,
image?, exitCode?, forced?}`.
- `by` is attribution, not authorization. The CLI sends `{"by": "cli"}` in the action body;
  the panel sends nothing, which means `panel`. Boot writes `boot`, and the exit watcher
  writes `plugin`. Both callers passed the same gate.
- Retention is a row bound, not a rotation: the newest 5,000 rows survive, swept along the
  append path every 64 admits (the jsonl era's 1 MiB rotation kept one generation; a row
  bound says the same thing in the unit the panel actually pages).
- A read barrier first, newest last, optionally one app's — the jsonl era's synchronous
  visibility kept whole: a recorded outcome is visible to the very next read.
- The legacy `process-history.jsonl` tree (and its `.1` generation) imports once on first
  open, oldest first, `INSERT OR IGNORE` against the natural key, the fold stamped in `meta`
  — and the source files are left in place for the operator to retire (ADR-033's contract).
- Output is never copied into it; output stays in the capture file.
- A history write that is refused warns once in the gateway log and never fails the action:
  the history is a record, not a gate.

**Logs** come from two sources per app. Both are read only through the log route, which never
takes a path from the request.

- **The capture file** `logs/apps/<name>.log`, which exists after the first start. It holds the
  header lines, then the stdout and stderr of `start`, of `stop`, and of the app itself when
  the app does not redirect its own output (§process.detached).
  - Above 10 MiB it is copy-truncated: copied to `.1`, then cut to zero. The app's append-only
    handle keeps writing at the new end.
  - The check runs on the plugin's hourly tick and before each start.
  - A line written between the copy and the cut is lost. That is the price of never being in
    the write path.
- **The app's own log** (`log`). It is opened read-only with full sharing for each read and
  closed again, so the app's own rotation is never blocked.
- **Reading** returns at most the last 5,000 lines, taken from at most the last 1 MiB.
  - Text decodes as UTF-8, else GBK (`encoding_rs`), else lossily: the supervisor's rule.
  - Every value the app's references resolve to (8 bytes or more) is masked as `••••••••`.
    The references are resolved again at read time. One that no longer resolves cannot be
    masked: its value was a credential the vault no longer holds.

**Routes** live under the prefix `/api/process`. They sit behind the admin gates every `/api`
route has, and the wire is camelCase.

| Route | Answer |
|---|---|
| `GET /api/process` | `{apps: [{name, status, reason?, pid?, ports, downPorts, heldPorts?, conflictPid?, conflictImage?, uptimeSec?, busy?, lastAction?, autostart, ui?, stopIsKill, hasAppLog}], gateway: {pid, uptimeSec, memMb}}` - `heldPorts` is every port the row's pid actually holds, declared or not (one listener pass for the whole answer, never a probe per row) |
| `POST /api/process/apps/{name}/start` | Body `{by?}`. 202 `{accepted: true}`; 409 when busy; 404 for an unknown app |
| `POST /api/process/apps/{name}/stop`, `…/restart` | Body `{force?, by?}`. The same answers |
| `GET /api/process/apps/{name}/log?source=capture\|app&tail=500&after=<size>` | `{source, path, size, lines, truncated, reset}`. With `after`, only what was appended past that byte offset. A file now shorter than `after` was rotated, so the tail comes back with `reset: true`. 404 when the source does not exist |
| `GET /api/process/history?app=&limit=100` | `{entries}`, newest last. `limit` is at most 1,000. The read spans the current file and its `.1` generation |
| `GET /api/process/children` | §process.children |
| `POST /api/process/children/{pid}/stop` | §process.children |
| `GET /api/process/fs?dir=<path>` | `{dir, parent?, entries: [{name, dir, exe}], truncated}` - the Program picker's read-only listing: folders first then names, capped at 2,000, names and shapes only, never contents. `dir` absent starts at the user's home; a path that is not a readable directory is a 400 naming it |

Config is read and written through the host's `GET/PUT /api/plugins/process/config`
(§host.plugins). There is no app CRUD route of its own.

## §process.detached — A program that outlives the gateway

`spawn_detached(spec) -> Result<DetachedChild, SpawnError>` in `swiss-host` is the only way a
program is started to outlive the gateway (§security.rules).
- `spec` carries the resolved program, the args, the working directory, the env overlay and
  the capture file's path.
- `DetachedChild` carries the pid, the creation time, and the child handle: a
  `tokio::process::Child` without `kill_on_drop`, owned by the caller's exit watcher.

Runs and MCP children die with the gateway on purpose (ADR-008); an app must not. A child that
outlives its parent falls into traps, mostly on Windows, and each row below is the rule that
avoids one.

| Trap | What goes wrong | Rule |
|---|---|---|
| **Job objects** | A child inherits its parent's job. In the supervisor's kill-on-close job it dies when the gateway's handle closes, which happens on success too: a launcher's daemon would die the moment the launch worked. A gateway started from an IDE or agent terminal runs inside that tool's job, and every app would die with it | No `KillOnCloseJob`, no `kill_on_drop`, no PID-ledger entry, no reap. Spawn with `CREATE_BREAKAWAY_FROM_JOB`. When the job forbids breakaway (os error 5), spawn again without it and put "shares the gateway's job" in the start's message |
| **Pipes** | Nobody reads a pipe once the action is over, so the buffer fills and the app blocks on its next write. After a gateway restart the pipe is broken and Node dies of `EPIPE`. A grandchild holding the write end means EOF never comes | stdout and stderr both go to the capture file, opened append-only and handed to the child. stdin is null. **Never a pipe.** Nothing ever waits for a stream to close, only for a process to exit |
| **Inherited handles** | Rust spawns with handle inheritance on whenever stdio is set, so the child receives every inheritable handle the gateway holds. A daemon's own stdout and stderr *are* `gateway-<port>.log`: an app would hold the gateway log open for its whole life and break its rotation | The `serve` path calls `keep_std_handles_from_children()` at boot, before any spawn, as the detaching `start` already does (§host.daemon). A test proves an app never holds the gateway log open |
| **Consoles** | `DETACHED_PROCESS` leaves the app without a console, so its first console grandchild (`cmd`, or `node` under `npx.cmd`) opens a new, visible window. `CREATE_NO_WINDOW` is ignored in that combination. And a shared console is a shared fate: closing the terminal of a foreground `swiss start -f` sends `CTRL_CLOSE_EVENT` to everything attached to it | `CREATE_NO_WINDOW \| CREATE_NEW_PROCESS_GROUP \| CREATE_BREAKAWAY_FROM_JOB` (`0x0800_0000 \| 0x0000_0200 \| 0x0100_0000`), never `DETACHED_PROCESS`. Each app gets its own hidden console, and the gateway's Ctrl-C group never reaches it. The cost is one `conhost.exe` per app |
| **No SIGTERM** | On Windows a console-less process can be sent no signal, and `TerminateProcess` gives it no chance to flush | Graceful stop exists only as the app's own `stop` command. Without one, Stop is a hard kill, and the row says "stop = kill" before the click (§process.apps) |
| **PID reuse** | Windows recycles pids quickly. A pid stored yesterday, or read ten seconds ago, may now be someone else's | A pid is never trusted bare. While the exit watcher holds the child's handle, the pid is pinned. Otherwise identity is pid + creation time + image, re-resolved immediately before any kill |
| **Stale parent links** | Toolhelp's parent pid outlives the parent, so a tree walk from a reused pid reaches strangers | The creation-time check in the walk (§process.tree). It also fixes `tree_kill` and the boot settlement for every caller |
| **Silent kill failure** | An elevated process refuses `PROCESS_TERMINATE` to a non-elevated gateway, and `tree_kill` used to ignore that error | `tree_kill` reports survivors (§process.tree), and the stop names the pid that survived. The gateway never elevates: an app that needs elevation fails its start with os error 740, named as such |
| **A stale PATH** | A resident gateway's environment is a snapshot taken at boot. A tool installed after the gateway started (a new entry in the user's `Path`) stays invisible to every app it launches, for days | Each spawn rebuilds PATH: the user's `Path` (`HKCU\Environment`), then the machine's (`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`), both expanded, then the login extras of §host.daemon, deduplicated. When the registry cannot be read, the gateway's own PATH is used. The program resolves against that PATH with PATHEXT, never as an extensionless spelling (os error 193). The rest of the environment is the gateway's scrubbed one plus the entry's `env` |
| **Batch launchers** | `npm`, `npx` and `pnpm` are `.cmd` files. They run through `cmd.exe`, so the tree becomes `cmd.exe → node.exe`, and Rust refuses an argument it cannot quote safely for a batch file (BatBadBut) | A refused argument fails the start, naming it. Status reads the port owner (`node.exe`), never the root of the spawned tree, which is why `image` names the owner |
| **Log growth under a live writer** | Renaming a file the app is writing moves the writer with it, so rotation by rename never shortens anything | Copy-truncate on an append-only handle (§process.apps). Files the gateway only reads are opened for each read with full sharing, and never held |
| **Encoding** | A Chinese Windows writes GBK to a redirected stdout | The reader decodes as the supervisor does (§process.apps) |
| **Reserved ports** | Hyper-V and WinNAT reserve port ranges at boot. A port inside one fails to bind with access denied | Nothing to build: the start fails with the app's own bind error in the capture tail. Noted here so nobody hunts for a gateway bug |

**Unix:**

- **Detaching.** The child calls `setsid()` before exec: a new session, not just a process
  group, so a terminal hangup never reaches it. `pre_exec` is `unsafe`, so the hook is a
  function of `swiss_core::platform` (`detach_command`), like every other FFI item
  (§process.tree).
- **Output and stop.** stdout and stderr go to the capture file. A stop without a command
  signals the process group (§process.apps).
- **No zombies.** The exit watcher awaits the child, so a held app never becomes a zombie.
- **The Linux probe:**
  - The listener table is the LISTEN rows (state `0A`) of `/proc/net/tcp` and
    `/proc/net/tcp6`, joined to their owners by socket inode through one pass over
    `/proc/*/fd`.
  - The image is the basename of `/proc/<pid>/exe`, falling back to `/proc/<pid>/comm`.
    `comm` is truncated to 15 bytes, so in that fallback a longer image matches on its first
    15 bytes.
  - The creation time is field 22 of `/proc/<pid>/stat`, divided by the clock tick, plus
    `btime` from `/proc/stat`. Field 22 is counted after the last `)`, because `comm` may
    contain spaces and parentheses.
  - The parsers are pure functions over text, in `platform/procfs.rs`.
  - Without privileges, the owner of a socket held by another user's process cannot be read.
    Such a port reads as owned by pid 0 with an unknown image, which is a `conflict` for any
    app: "owned by another user".
- **macOS has no probe.** Status reads `unknown`. An explicit Start still works, and so does a
  Stop with a `stop` command.

**The platform surface** this adds to `swiss_core::platform`, with one re-export list per
platform as today:

- `tcp_listeners() -> io::Result<Vec<(u16, u32)>>`. On Windows this is the
  `GetExtendedTcpTable` call that `tcp_listener_pid` makes, done once for the whole table for
  `AF_INET` and `AF_INET6`. On Linux it is the `/proc` join above. On macOS it is an
  `Unsupported` error.
- `process_created_at(pid) -> Option<u64>`, in Unix milliseconds. On Windows it is
  `GetProcessTimes` through `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`; a pid that cannot
  be opened has no time.
- `process_name(pid)`, now on Linux as well.
- `machine_path_read()`, beside `user_path_read()`.
- `detach_command(&mut std::process::Command)`: the creation flags on Windows, the `setsid`
  hook on Unix.
- `tree_kill`, which now returns the pids that survived it.
- `walk_descendants`, which now takes the creation-time lookup and the `WalkMode`.

## §process.children — Every child that dies with the gateway

`ChildRegistry` (in `swiss-host`, held by `RuntimeServices`) is the one list of the gateway's
own children. It replaces the supervisor's private pid set.
- An entry is `{pid, createdAt, owner, kind: run|mcp|terminal|agent, label, stop}`.
- `register` returns a `ChildGuard` that removes the entry on drop, so a panic leaves no stale
  row.
- Managed apps are not children (they outlive the gateway) and are never registered.

| Kind | Registered by | Label | Stop hook |
|---|---|---|---|
| `run` | The supervisor, after the spawn | `<action type> · <run label>` | The coordinator's `cancel(run_id)`, so the run reaches `canceled` through its own path |
| `mcp` | The `proc` adapter, after the spawn, at the point where it records the pid for the ledger | The MCP's name | What `POST /api/mcps/{name}/stop` does: stop, persisted as disabled, so the MCP does not come back at the next boot (§mcp.admin) |
| `terminal` | The local terminal provider, after the PTY spawn (`PtyHandle::pid()`) | The session's title at open, else its shell | Close that session (`TerminalSessions::close`) |
| `agent` | The memory sync runner, after the spawn (§memory.sync) | The runner's program | Tree-kill the pid — the runner has no richer lifecycle; the row leaves when the child does |

- **The tag travels with the spawn.** `ActionContext` gains a field
  `child: ChildTag {owner, label, stop}`, which the coordinator builds for each run. Its hook
  holds a weak reference to the coordinator plus the run id. `process.exec` and
  `process.legacy-command` put the tag into the `ProcSpec`. A supervisor spawn without a tag
  (a direct caller) registers with owner `host`, the program as its label, and no hook.
- **Stop goes through the owner, never a raw kill.** Killing underneath an owner leaves its
  state lying: an MCP would still read as started, a run would never reach its terminal state,
  and a terminal tab would hang. A row without a hook has no Stop.
- **Memory per row** is the child's own working set, read in one platform call per row at
  read time (the same primitive the memory tree walk sums). A row without a reading shows
  none, never 0.
- **Remote runs** execute on another machine over an in-process SSH channel. They have no local
  child, so they appear only in the runs list (§remote).

| Route | Answer |
|---|---|
| `GET /api/process/children` | `{children: [{pid, owner, kind, label, uptimeSec, memMb?, stoppable}]}`, ordered by owner, then label |
| `POST /api/process/children/{pid}/stop` | Runs the entry's hook and awaits it for at most 15 s, then answers `{ok, message}`. 404 when no entry has that pid any more; 409 when the entry has no hook |

## §process.panel — The Process page

The page is `crates/swiss-panel/panel/src/views/process.ts`, emitted to
`admin_assets/js/views/process.js` (the page entry, §panel.nav). Its rail glyph is `activity`
(`GLYPHS` in `plugin-palette.ts`). It has three blocks, top to bottom.

**The gateway**: one quiet, labeled read-only header row with pid, elapsed uptime and memory.

**Managed apps**: one row per app. The section states that apps keep running when the gateway
stops, and saving a definition never starts it. Gateway children have the opposite lifetime.

- **Scan line.** Name; one neutral row verb, a direct Logs glyph and the overflow; a status badge (`running`, `partial` with the down ports, `stopped`,
  `conflict` with the stranger's pid and image, `unknown` with the reason); ports OCCUPANCY
  FIRST - every port the row's pid actually holds is a green chip (declared or not: the
  listener table's fact), then a declared port the running pid does not hold rides red after
  them; an app with no pid of ours holds nothing, so its declared ports show as quiet gray
  chips and a conflict's stay red (a stranger holds the promise); pid;
  elapsed uptime; an autostart mark; and a "stop = kill" mark when `stopIsKill`. Ports, labeled
  PID, elapsed uptime and policy marks share the library row's always-visible wrapping `meta`
  strip, not a fixed-width column per fact. Add app is the page's only accented primary.
- **Actions.** Start, Stop, Restart, Open UI (the `ui` URL, in a new tab with
  `rel="noopener"`), Logs, Edit and Delete.
- **Disabled buttons.** A button the status makes meaningless is disabled: Start on
  `running`, `conflict` and `partial`; Stop on `stopped`, `unknown` and `conflict`; every
  mutation button while the row is `busy`. A busy badge replaces the previous verdict with
  localized Starting / Stopping / Restarting words. Logs remains accessible.
- **The last action.** Current conflict / partial diagnostics take priority over a stale action
  result. A failed action still appears in red for an unknown app; its generic identity
  explanation remains in the status tooltip, never hides the failure. Otherwise `lastAction` shows under the row, and takes the error style
  when it is not ok. A plain running row's REASON echo stays quiet underneath - the badge and
  the pid column already said it, and "running (pid N)" there would say the pid twice (an ok
  `lastAction` line still shows). One stale shape is a reconciliation, not an alarm: a start
  that failed at the window's end on a row that is running again (the launcher pattern - the
  launcher exited 0, nothing listened before the window closed, and the child bound the port
  only after) shows a quiet "the last start failed; a matching process holds the port now"
  line, and the raw sentence stays in the history. (A child that binds inside the window is
  not this shape: the start itself answers ok.) A failed stop or restart on a running row
  stays red - it is current and actionable.
- **Force is always a confirm** naming the pid, image and port (the native `confirm(tr(…))`
  the panel uses elsewhere). It is offered only after a stop answered `canForce`.
- **The empty state** says what an app is and opens the add sheet.

**Gateway children**: the §process.children rows grouped by owner. Bands fold through real
buttons and retain their folds across polls. Rows show the localized kind, labeled pid,
elapsed uptime and memory only when known. Stop exists only with an owner hook; the confirm
names the label, and stopping an MCP disables it.

Sheets:

- **Logs.** A Capture tab, and an App tab when `hasAppLog`. Each shows the last 500 lines.
  The capture is a SEQUENCE OF EVENTS, never one stream: the supervisor writes a
  `--- <time> <action> by <who> ---` header before every action, and the pane renders each
  run as its own disclosure row - every start and stop stays a visible boundary however long
  the output between them; the newest run is open, older runs fold to their header, and a lone
  headless group (the app tab, or a capture whose head the tail cut) stays one plain block.
  Follow polls every 1 s with `after`. A `reset` answer clears both the buffer and rendered
  events; visible output remains bounded to the last 500 lines. Source controls, output
  and history have distinct stable roots, so output refreshes never replace the sheet. A follow tick is
  append-only - the newest run's text moves, the sheet is never rebuilt under the reader, and
  the pane stays pinned to the bottom only while the reader was already there. A tick that
  carries a new header (a start or stop happened while watching) repaints the log block alone:
  a new event row is born, the rare path by construction; a headerless tick APPENDS to the
  newest event's own pre - it never rewrites it with the whole buffer. That app's last 50 history entries
  show below as RUNS, not lines: a start opens a run, its stop and exit close it, and one row
  tells the whole story - who started it, how long it ran, how it ended (`exit N`, stopped by
  whom). A stop pairs with its run even when the exit line landed first (a stop-command app
  dies before the supervisor writes its line), and a FAILED start is its own red run - the
  failure sentence is the outcome, never a green "still running"; when the pump's exit line
  races ahead of the start line, the failed run adopts it. The raw sentences (an exit's
  multi-line stderr included) fold into the row's detail; entries that pair with nothing ride
  in the run they followed.
- **Add / Edit:**
  - The fields of §process.apps. The common case is four answers - name, program, args,
    ports; image and working dir sit behind the Advanced fold, and when ports are set and
    image is left empty the program's own file name is the image. Program and args are
    separate inputs, args one per line: the stored form is argv, never a string to split; an
    empty line is an empty argument, and a value the line format cannot carry (an embedded
    line break) is refused at save, never silently rewritten. Ports are comma-separated.
    Program carries a **Browse** button: an inline picker panel
    (the house sheet is one slot, so the picker lives under the field, not over the form;
    Escape with the picker open closes the picker alone, and only with it shut cancels the
    form)
    fed by `GET /api/process/fs` - whose answer carries the platform's `sep` so a walked path
    joins with the server's word, never a hard-coded backslash, and whose executable test is
    the platform's own (extension on Windows, the executable bit on Unix). A folder walks in,
    an executable fills Program. A first save teaches the next step - the toast says Start on
    the row is what runs it. The shared form uses a zero-minimum grid track and the picker
    path input may shrink: a long directory path must never push Go or Up outside the dialog.
  - **Templates** (Add only; an existing app's launch is edited, not re-picked). A Template
    select over the form fills a known program's whole launch, so the reader picks HOW it
    starts instead of working out which executable ends up owning the port: name (only while
    the field is empty or still holds the last template's), program, args, ports and image.
    Autostart, dir, stop, env, log and UI stay as they are; None empties the filled fields
    again, the name only if a template put it there. One quiet line under the select says
    what the chosen template decided. The table is `views/process/templates.ts`, and two dsh
    launches ship: `dsh web` (opens the browser on each start) and `dsh web --no-open` (for
    Start with swiss - nothing pops up at boot, and Logs has the link with its token), both
    with `--port 3080`, ports `[3080]` and image `node`. dsh is an npm-installed Node CLI:
    `dsh` resolves to `dsh.cmd`, which runs `node.exe`, and the image the form derived from
    the program (`dsh`) read the running app as a conflict with itself - "port 3080 is held by
    pid N (node.exe)" after a start that timed out (the owner's finding, 2026-11-10). `node`
    matches `node.exe` on Windows and `node` on Unix; `--port` pins the port the row declares.
    No `ui`: dsh's link carries a per-start token the config cannot know.
  - `env` is `KEY=value` lines through `envToText`/`envToObj` (`fields.ts`, the proc MCP
    form's editor), kept exactly as typed, so a reference stays a reference.
  - Both start and stop args use one line per argv entry, preserving spaces within entries
    and empty entries alike - an Edit that never touched args never drops one.
    Existing names are read-only history/CLI identities. Add refuses duplicates.
  - Saving rereads the whole config, replaces the app in place (preserving order and
    unrelated fields), and PUTs its revision. The opened target's canonical baseline is
    checked before every PUT: target edits/removal refuse with a reopen instruction.
    A 409 retries once only when the target is unchanged; unrelated app updates survive.
    A 400 shows the backend sentence, and warnings stay under the form.
- **Delete** is a confirm, then the same PUT without the entry. The confirm says that a
  running process is left running.

**Polling.** `GET /api/process` every 2 s while the page is shown and every 500 ms while a row
is busy; children every 5 s. The page's own sampler keeps one in-flight request per inventory;
hidden documents do not poll. Each mount owns its abort signal and requests: late responses
cannot repaint another page or restart timers. Memory-only updates patch the gateway line,
app changes patch individual rows, and child-only responses paint immediately. Sheets own
independent sessions: late config, picker, save or log-source responses cannot change a
newer sheet. Missing captures are inline states, not repeated polling toasts; new event
boundaries refresh the app's history.

**Strings.** Every string goes through `tr()`/`trn()`, with `process.*` keys in `en.ts` and
`zh.ts` (§panel.i18n).

**The rail.** An operator who has already stored rail pins gets no new seat (`pinnedGroups`,
`plugin-palette.ts`); their list stays as stored, and for them Process waits in the palette
until starred. The default rail seats every served group up to the cap (§panel.nav), so
Process rides it without a pin — on the rail the moment it serves.

## §process.cli — `swiss process`

```
swiss process [status] [--json]
swiss process start|stop|restart <name> [--force]
swiss process logs <name> [--app] [-n N] [-f]
```

The command lives in `src/process_cli.rs`, and `cli::main` dispatches to it before the generic
parser, like `remote` and `run`.
- It speaks the admin API with the CLI key (`daemon::with_cli_key`).
- It targets the instance `swiss api` targets (`resolve_port()`: `SWISS_PORT`, else the
  config's port).
- There is no offline path: the gateway is the one control path.

**`status`** prints one line per app: name, status, pid, ports, uptime, and the reason or the
conflict. `--json` prints the `GET /api/process` body instead. Exit codes:
- 0 when every `autostart` app is `running`, and when no app has `autostart`.
- 1 otherwise.
- 2 when the gateway cannot be reached or the process plugin is off.

**`start`, `stop` and `restart`** POST with `{"by": "cli"}`, adding `force` only for stop and
restart. Then they poll `GET /api/process` every 250 ms, for at most 120 s, until the row is
no longer busy and its `lastAction.at` is not older than the request. They print the message.
Exit codes:
- 0 when the action succeeded.
- 1 when it failed. A `canForce` answer adds "run again with --force to kill pid N".
- 1 for an unknown app or a 409 busy.
- 1 after 120 s without an outcome, printing "still busy; see `swiss process status`".
- 2 when the gateway cannot be reached.

**`logs`** prints the last `-n` lines (default 200) of the capture file, or of the app's own
log with `--app`. `-f` follows the way `swiss logs -f` does, polling every 1 s with `after`.

## §process.verify — Acceptance and live verification

Acceptance (the numbering is stable; code cites it as "item N"):

1. The gates pass, gate 2 included (§testing). `cargo tree -d -e normal,build` shows no new
   duplicate, and no dependency is added.
2. The tests named below exist and pass.
3. On 19998 (below), cc-otel's shape works end to end. A launcher that exits 0 leaves an app
   whose ports read `running`, and a Stop without a stop command kills only the port's owner.
4. A start whose window ends leaves its process running, and the history says so.
5. Disabling the process plugin leaves a started app running. Enabling the plugin again reads
   the app as `running` from the tracked record.
6. Stopping the gateway leaves a started app running, and the gateway log can be renamed while
   that app runs.
7. Killing the gateway still leaves no run, MCP, shell or agent-runner child behind (ADR-008).
8. Status on a conflict names the stranger, and neither Start nor Stop touches it.
9. The Process page passes the panel proof of life (§testing.live) in English and in Chinese.
10. Every §process.budget row is measured and within budget.

**Tests.** Unit tests are inline in the named module; integration tests carry these names in
their file.

| Where | Tests |
|---|---|
| `crates/swiss-process/src/status.rs` | `every_port_owned_by_the_image_is_running`, `some_ports_up_is_partial_and_names_the_down_ones`, `no_port_listened_on_is_stopped`, `a_stranger_on_any_port_is_a_conflict_naming_pid_and_image`, `conflict_beats_partial`, `a_tracked_pid_with_its_creation_time_is_running`, `a_reused_pid_is_stopped_and_drops_the_record`, `nothing_tracked_without_ports_is_unknown_with_the_reason`, `an_unreadable_listener_table_is_unknown_with_the_reason`, `image_matching_ignores_case_and_a_missing_exe_suffix_on_windows` |
| `crates/swiss-process/src/config.rs` | `every_field_is_validated_and_named_by_index`, `the_gateways_own_port_and_the_swiss_image_are_refused`, `a_port_belongs_to_one_app`, `image_without_ports_is_refused`, `a_non_http_ui_is_refused`, `boot_drops_an_invalid_app_and_keeps_the_rest`, `a_version_1_empty_config_reads_as_no_apps` |
| `crates/swiss-process/src/control/tests.rs`, over an injected `Probe`, `Spawner` and `Killer` and a `start_paused` clock | `a_second_action_on_a_busy_app_is_409`, `start_refuses_conflict_and_partial`, `a_start_window_that_ends_never_kills_a_live_process`, `a_launcher_that_exits_0_with_nothing_listening_fails_naming_the_port`, `a_nonzero_exit_fails_with_the_masked_tail`, `a_missing_reference_fails_the_start_naming_it_and_never_a_value`, `a_hard_kill_targets_only_the_reresolved_owner`, `a_hard_kill_refuses_the_gateway_by_pid_and_by_image`, `a_kill_survivor_is_reported_not_swallowed`, `a_stop_command_that_leaves_the_app_running_offers_force`, `restart_ends_at_a_failed_stop`, `start_with_swiss_runs_only_on_a_daemon_and_retries_once`, `start_with_swiss_leaves_a_conflict_alone` |
| `crates/swiss-process/src/logs.rs`, `history.rs` | `copy_truncate_keeps_one_generation`, `a_file_shorter_than_after_resets_the_follow`, `gbk_decodes_and_resolved_values_are_masked`, `history_rotates_above_one_mib_and_reads_across_the_generation` |
| `crates/swiss-core/src/platform/mod.rs`, `procfs.rs` | `the_walk_drops_a_child_created_before_its_parent`, `an_edge_without_times_counts_for_measuring_not_for_killing`, `proc_net_tcp_listen_rows_parse_for_v4_and_v6`, `proc_stat_start_time_survives_a_comm_with_spaces_and_parens` |
| `crates/swiss-host/src/services/detached.rs`, `children.rs`; `crates/swiss-core/src/secure/refs.rs` | `the_spawn_path_puts_the_registry_path_first_and_dedups`, `a_dropped_guard_unregisters_even_on_panic`, `stop_goes_through_the_owners_hook`, `strict_resolution_refuses_a_missing_env_var_and_a_missing_secret` |
| `tests/process_apps.rs` (root; oneshot over fakes) | `the_inventory_carries_the_process_page_and_routes`, `start_answers_202_and_the_outcome_lands_in_the_row_and_the_history`, `every_process_route_is_a_structured_503_while_the_plugin_is_off`, `an_invalid_put_is_refused_whole_and_nothing_runs`, `the_log_route_reads_only_the_configured_files`, `stopping_a_run_child_cancels_it_through_the_coordinator`, `disabling_the_plugin_leaves_a_started_app_running` |
| `tests/plugin_host.rs` | `inventory_shape_is_exact_and_the_mcp_plugin_started_the_mcps`, updated for the new page and route |
| `crates/swiss-it/tests/it/detached.rs` (gate 2, real processes; the helper is `crates/swiss-it/src/bin/it-app.rs`) | `a_detached_child_survives_its_spawners_kill_on_close_job` (Windows), `a_detached_child_survives_its_spawners_session` (Unix), `a_megabyte_of_output_never_blocks_the_app`, `an_app_never_holds_the_gateway_log_open`, `a_launcher_child_is_found_as_the_owner_of_its_port`, `creation_time_is_stable_for_a_live_pid_and_absent_for_a_dead_one`, `tree_kill_ends_a_detached_tree_and_reports_no_survivors` |
| `crates/swiss-panel/panel/test/admin-process-view.test.ts` | Badges per status; buttons disabled per status and while busy; the force confirm; follow with `after` and `reset`; the add sheet writing argv; the empty state. `i18n-complete` and `i18n-ratchet` stay green |

**The `it-app` helper.** Each role is a plain Rust function, never a shell:
- `serve <port>` binds `127.0.0.1:<port>`, answers every connection with `HTTP/1.1 200 OK`,
  and prints one line a second.
- `launcher <port>` spawns `serve <port>` through `detach_command` and exits 0.
- `flood` writes 1 MiB to stdout, then sleeps 60 s.

Every helper a test starts is killed by a drop guard. A listener is held for as long as a test
asserts on its port.

**Live verification** runs on 19998 (§testing.live), never on 19999.

1. After `/health` answers, `scripts/test-instance.ps1 -Start` replaces the copied apps
   through the API, so the operator's real apps never appear on the test instance. Their Stop
   would reach the real programs. The script runs this with `SWISS_HOME` and
   `SWISS_PORT=19998` set as for the serve, so the CLI key and the port are the test
   instance's:

   ```
   swiss api PUT /api/plugins/process/config '{"config":{"apps":[ … ]}}'
   ```

2. The demo apps:

   | Name | Shape | Shows |
   |---|---|---|
   | `demo-ping` | `ping -n 3600 127.0.0.1` (POSIX: `sleep 3600`), no ports | A tracked foreground app; Stop is a kill |
   | `demo-listen` | `it-app serve 18431`, ports `[18431]`, image `it-app.exe`, `ui` its URL | `running` by port; Open UI; Stop kills the owner |
   | `demo-launcher` | `it-app launcher 18432`, ports `[18432]`, image `it-app.exe` | cc-otel's shape: the launcher exits 0 and the port reads `running` |
   | `demo-conflict` | ports `[135]` (the RPC endpoint mapper, owned by `svchost.exe` on every Windows), image `ping.exe`; Windows only | `conflict` naming svchost; Start and Stop are refused |
   | `demo-missing` | program `swiss-demo-missing` | A start that fails, naming the program |

   `it-app` is the gate-2 build's (`target/debug/it-app.exe`). When it has not been built, the
   two `it-app` rows are skipped with a line saying so.
3. The walk:
   1. Start, stop and restart each app, from the page and from `swiss process`.
   2. Force after a refused stop.
   3. Logs with follow, and the history.
   4. Open a terminal tab, then stop its child row from the page; the tab closes.
   5. `-Stop` the instance. `demo-listen` must still answer, and the test home's
      `gateway-19998.log` must be renamable.
   6. Start the instance again. The apps read as running; stop them from the page.

## §process.budget — Budget

| Item | Budget | Measured (2026-10-09, release exe, the 19996 instance) |
|---|---|---|
| RSS, the plugin active with zero apps | +≤ 0.3 MB | +24 KB (working set, 7-sample medians, plugin toggled off/on) |
| Each app the gateway holds (one exit-watcher task and one process handle) | +≤ 64 KB | +36 KB (one running `ping`: watcher + handle) |
| One `GET /api/process` over 10 apps (one listener snapshot) | ≤ 20 ms on Windows | median 9.9 ms, p90 14.1 ms, max 17.5 ms (warm, loopback session) |
| Polling while the page is closed | none | none by construction - no timer outlives unmount (the view's vitest) |

Measure per §testing.memory. The numbers go into this table, as a Measured column, in the
commit that measures them.

## §process.roadmap — Not built yet

**The watchdog.** The owner decided on 2026-10-08 to leave it out of the first version: a
crash shows on the page and in the history, and the operator restarts. When it is built, it
comes in this order, each step usable alone:

1. **`restartOnExit`** (per app, default `false`) for an app whose process the gateway holds:
   an app without `ports`, or a foreground app with them. The exit watcher already observes
   the exit as an event, so no polling is needed.
   - **Which exits count.** A Stop sets a "stopping" mark before it acts, so the watcher
     tells an exit that was asked for from a crash. Only a crash restarts.
   - **Backoff.** 1 s, 2 s, 4 s … capped at 5 min, and reset after 10 min of running.
   - **`failing`.** Five consecutive failed restarts put the app in a new `failing` status: no
     more restarts, and the row says why. An operator Start clears it.
2. **Desired state, for apps the gateway cannot hold** (a launcher's daemon, an app that was
   running before this gateway started):
   - A persisted `desired: up|down` per app. `autostart` is its value at boot; a Start sets
     `up`; a Stop sets `down`.
   - A loop every 30 s probes only the apps whose desired state is `up`, and runs Start when
     one reads `stopped`, never on `conflict` or `partial`. It uses the same backoff and the
     same `failing` status.
   - The loop is the first polling this plugin does while nobody asks, so it gets a
     §process.budget row.
3. **One loop per home.** A lease file, `.process-watchdog.lock` in the home, held under an
   exclusive OS file lock by a daemon. A `serve` (the test instance) never takes it. A second
   daemon on another port finds it held, says "watchdog held by pid N", and runs no loop.
   Without the lease, two instances would race to start the same app.
4. **Re-adoption after a gateway restart.** At plugin start, every tracked record whose pid,
   creation time and image still match is re-opened, so its exit is an event again. Windows:
   `OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION)`. Linux 5.3 or later: a
   pidfd. Elsewhere: the step-2 loop.
5. **The surface.** A `failing` badge, a Reset action, and the time of the next backoff
   attempt, on the page and in `swiss process status`.

**Other work, not planned yet:**
- A macOS probe through `libproc`, `proc_pidinfo` and `proc_pidfdinfo`, when a Mac user needs
  status there.
- Ordering apps by dependency, health URLs beyond ports, resource limits, and running apps as
  Windows services (rejected in ADR-036).

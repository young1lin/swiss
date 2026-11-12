# 05 — Process: `restartOnExit` (watchdog step 1, with step 5's surface for it)

Implementation brief, not SPEC. The SPEC amendments in §8 land in the commit that makes each
paragraph true. Shared rules: [00-handoff.md](00-handoff.md). Independent of 02–04; needs 01 for
its strings.

## 1. Goal

A managed app that dies on its own comes back, with backoff, and gives up loudly when it keeps
dying. This is step 1 of the watchdog §process.roadmap already designed (owner's decision
2026-10-08: not in v1; now asked for). It also covers the parts of step 5 (failing badge, Reset,
next attempt time) that apply to step 1. Steps 2–4 (desired state loop, lease, re-adoption) are
**not** in this brief.

## 2. Facts (verified 2026-11-12)

- `crates/swiss-process/src/control/mod.rs` (1906 lines — see §6 on splitting):
  - `AppControl` holds `suppress: Mutex<HashSet<u32>>` (the pids a stop or restart is
    killing), `record` (the tracked process per app), `last`, and `busy`.
  - `handle_exit(&ExitLine{app, pid, code, at})` (line 1385):
    1. returns early if the pid is in `suppress` (an exit that was asked for);
    2. returns early if the pid is not the record's (a stop command or a dead launcher);
    3. otherwise appends an `exit` history line, updates `last`, and `drop_record`s.
  - `handle_exit` is called from the plugin's pump (`crates/swiss-plugins/src/process.rs:296-316`,
    a `tokio::select!` with `stop_pump.notified()`) and directly by the tests.
  - `start(name, by)` (line 599) is the operator's Start. `run_start(app, by)` is the engine.
  - The busy mark refuses a second action with `Busy`.
- `start_with_swiss` shows the house pattern for a timer (line ~1515): a `tokio::time::sleep` in a
  `select!` with the plugin's stop signal, and `start_paused` tests advancing it.
- `AppConfig {name, start, stop, dir, ports, image, log, ui, autostart, env}` (`config.rs:60`).
  `set_apps(apps)` is how a config change reaches the control.
- `Row` (`control/mod.rs:197`) is the `/api/process` row, camelCase, `skip_serializing_if` on the
  optional fields.
- **Which processes the watcher actually follows** (§process.apps; `seams.rs:83` waits on the
  spawned child):
  - A foreground app, with or without ports: the spawned child lives on, and its exit is an
    event. **Watched.**
  - The dsh templates (`views/process/templates.ts:52-89`): `dsh` resolves to `dsh.cmd`, which
    runs and waits on `node.exe`, so the spawned shim stays alive. They should be **watched** —
    prove it with a test, do not assume it.
  - A launcher-resolved start (the launcher exits 0, and the port's owner is pinned in the
    record): the owner is **not** a child, and no exit event arrives. **Not watched.**
  - A record re-loaded from the previous gateway (`load_tracked`): no child handle, so **not
    watched** until this gateway starts it (re-adoption is roadmap step 4).
- **The Process page is not on the bus.** It runs its own mount-owned sampler
  (`views/process.ts:399`: a 500 ms tick, a `load()` every 2 s, or every tick while a row is busy;
  SPEC §process.panel). The restart state reaches the page through that sampler. **No bus work
  in this brief.**

## 3. Behaviour

### 3.1 Config

- `restartOnExit: bool`, default `false`, in the app's JSON next to `autostart`.
- It is parsed and validated in `config.rs` and round-trips through the editor form
  (`views/process/form.ts`) as one checkbox: "Restart when it exits on its own".
- Templates keep it off.
- `swiss process` config output, if any prints the fields, shows it.

### 3.2 What counts as a crash

Any exit that reaches the end of `handle_exit`'s two gates counts, whatever the code. An exit
with code 0 that was not asked for still counts: a server that returns is down. The two gates
already discard a Stop's or Restart's own kill, a stop command's exit and a dead launcher's exit.
Hook in **after** those gates, where the exit line is written.

### 3.3 The restart loop

Per app, the control keeps `RestartState {attempt: u32, pending: Option<(next_at_ms, JoinHandle)>, failing: bool, last_code: Option<i32>, started_at_ms: Option<u64>}`.

- **On a counted exit**, when `restartOnExit` is on:
  1. If the run that just ended lasted ≥ 10 min (from the start that made it, by the injected
     clock), reset `attempt` to 0.
  2. `attempt += 1`.
  3. If `attempt > 5`, set `failing`, append a history line (action `failing`, by
     `restart-on-exit`, message "exited 5 times in a row; not restarting"), and stop.
  4. Otherwise schedule a start after `min(1 s · 2^(attempt−1), 5 min)`: 1, 2, 4, 8 and 16 s for
     the five attempts. The cap matters only if the limit grows later.
- **When the timer fires**, call the normal start path with `by = "restart-on-exit"`, so history,
  busy, the record and capture all behave as an operator's Start.
  - If the app is busy at that moment (an operator acted), skip this attempt and clear `pending`.
    The operator's action wins.
  - A start that fails (spawn error, port never bound and the window failed) **counts as a
    crash**: go back to "on a counted exit" with that outcome's code (or none).
- **A Stop or Restart by the operator** cancels any pending timer. Stop also resets `attempt` and
  clears `failing`.
- **A Start by the operator** clears `failing` and `attempt`, then starts as usual.
- **Reset** (step 5's action), `POST /api/process/apps/{name}/reset` (beside `start`/`stop`/`restart`
  in `crates/swiss-process/src/api.rs:81-83`): clears `failing` and `attempt`, cancels any pending timer, and starts nothing. It
  answers 200 with the row.
- **A config change** (`set_apps`): for each app whose config changed or that left the config,
  cancel its timer and reset its state. Apps whose config did not change keep theirs.
- **The plugin stopping or being disabled** cancels every timer (the `stop_pump` signal, or abort
  the handles in the plugin's stop path). No start may land after the plugin's routes answer 503.
  This is the same rule `start_with_swiss` follows.
- **No polling.** The exit is an event, and the only waits are the backoff timers.

### 3.4 What the row says

`Row` gains `restart: Option<RestartView>`, present only when `restartOnExit` is on:

```json
"restart": { "watched": true, "attempt": 2, "nextAt": 1763000000000, "failing": false, "lastCode": 3 }
```

- `watched: false` (with `reason: "launcher"` or `"adopted"`) when the record is not a held child
  (§2). The page then shows "not watched: <reason sentence>" instead of promising a restart.
- `nextAt` is present only while a restart is pending. It is Unix milliseconds.
- When `failing` is true and the app reads `stopped`, `status` becomes `"failing"`. It is a new
  status value, so the panel's `statusWord`, its dot state and the CLI all learn it.

The panel (`views/process.ts`) shows:

- "Restarting in 4 s (attempt 2 of 5)" while pending, counting down from `nextAt` on the page's
  existing tick (no new timer);
- a `failing` status with an err tone;
- a Reset item in the row menu, enabled only when failing or pending;
- "not watched" as a muted note.

All keys go in `en/ops.ts` and `zh/ops.ts`.

`swiss process status` prints the same facts: `failing (exited 5 times in a row)`,
`restarting in 4 s (attempt 2/5)`, `not watched (launcher)`.

## 4. Tests

Rust, in the process crate (`cargo test -j 2 -p swiss-process -- --test-threads=1 restart`),
with `#[tokio::test(start_paused = true)]` and the injected clock kept in step with tokio's
paused time:

1. `exit_of_a_held_app_restarts_after_one_second`.
2. `backoff_doubles_and_failing_after_five` (1, 2, 4, 8, 16 s; the 6th exit sets failing and
   writes the `failing` history line).
3. `a_run_of_ten_minutes_resets_the_count`.
4. `stop_cancels_a_pending_restart` and `operator_start_clears_failing`.
5. `reset_clears_without_starting`.
6. `a_stops_own_kill_never_restarts` (the suppress path) and `a_launchers_exit_never_restarts`
   (the record gate).
7. `a_failed_scheduled_start_counts_as_a_crash`.
8. `busy_app_skips_the_attempt`.
9. `config_change_cancels_and_resets` (the changed app only).
10. `plugin_stop_cancels_every_timer` (no start after stop: assert on the fake spawner's calls).
11. **Eligibility**, through the fake spawner/probe:
    - a foreground app without ports is watched;
    - a foreground app with ports is watched;
    - the dsh template's shape (shim alive, a port owned by a grandchild) is watched;
    - a launcher-resolved start reports `watched:false, reason:"launcher"`;
    - an adopted record reports `"adopted"`.
12. The `Row` JSON: `restart` is absent when the setting is off; the `failing` status
    serializes.

Vitest (`--maxWorkers=2`, touched files): the form checkbox round-trip
(`process-form.test.ts`); the row renders pending, failing and not watched; the Reset menu item
POSTs once (`admin-process-view.test.ts`).

## 5. Live proof (19998, scratch apps only)

1. On the 19998 test instance, add an app `crashy` with `restartOnExit: true` whose start runs a
   command that exits with code 3 after about 3 s. Use whatever interpreter the machine has, for
   example `node -e "setTimeout(()=>process.exit(3),3000)"`, with no ports.
2. Start it and watch the page through five restarts into `failing`. Screenshots: pending with
   its countdown, and failing. Paste the history lines.
3. Reset, then Start: it runs again.
4. Then add a long-lived app (`node -e "setInterval(()=>{},1e9)"`), start it, kill its pid from a
   terminal (`taskkill /PID <pid> /F`), and see one restart after 1 s.
5. Do **not** use the owner's dsh or any app from production's config on 19998. The snapshot
   home may carry them, and starting one would fight production for its ports. Leave them
   stopped.

## 6. File size

`control/mod.rs` is 1906 lines, and `control/tests.rs` 1620. Put the restart state machine in a
new `control/restart.rs` and its tests in `control/restart_tests.rs` (or a `tests/` submodule,
following how `tests.rs` is wired). Do not grow `mod.rs` by more than the hook and the wiring.

## 7. Out of scope

Roadmap steps 2–4, namely:

- the desired-state loop for launcher apps;
- the lease file;
- re-adoption after a gateway restart.

Restarting an app that is `partial` or `conflict` is also out of scope, as are health URLs.

## 8. SPEC and docs in the same commit

- §process.apps: the `restartOnExit` field in the config table; what counts as a crash; the
  backoff; `failing`; Reset; `watched` and its two reasons; and the `restart` row object.
- §process.roadmap: step 1 and the step-5 parts it delivered leave the list. Steps 2–4 stay, and
  step 5's remainder stays for them.
- ADR-036: an amendment line ("2026-11: restartOnExit for held apps; nothing else restarts
  them") — the ADR's title says "nothing restarts them", so its text must stop being false.
- §process.panel: the row's restart line and Reset. §process.cli: the status words.
- §process.verify: the live walk above as an acceptance row.
- CHANGELOG `[Unreleased]`: Added.

## 9. Delivery

`feat(process): restartOnExit restarts a held app that exits on its own, with backoff and a
failing stop`, then `feat(panel): the Process row shows restarts, failing and Reset` (with the
emit). Two commits.

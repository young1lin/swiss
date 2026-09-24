# 06 — Roadmap

Six phases. Each has one exit criterion, and none of them is "the code compiles". The ordering is
deliberate: cheapest and most informative first, so the project can be abandoned early and cheaply
if the numbers do not appear.

## Current implementation status

> 2026-09-22 amendment (`062928a`…`929af06`, merged at `f3b6899`): verification is now TWO gates.
> Gate 1 is `cargo test --workspace`, below. Gate 2 is `cargo test -p swiss-it --features it`
> (docs/44) — the real MySQL/PostgreSQL/Redis integration suite, which needs a Docker endpoint
> (`DOCKER_HOST`) or one of the three `SWISS_IT_*_URL` escapes and treats their absence as a
> failure, never a skip. It runs in CI on ubuntu (`929af06`) and is mandatory when a diff touches
> the DB adapters/browsers or swiss-it itself.

The Rust implementation now contains all 70 planned backend modules and wires every adapter
family through the factory. The MongoDB adapter was deleted outright (ADR-012), as was the
HTTP Tools touchstone plugin once it had served its proof; neither is feature-gated. Gate 1
is green and has grown since it was last quoted here: 941 workspace tests at `54da925`
(2026-09-11), 1,406 at `13ec65c` (2026-09-22, counted with `cargo test --workspace -- --list`),
with strict Clippy and the release build passing alongside. Exact counts rot; only a green
`--workspace` run means the suite passed.

Since then the build has become the toolbox docs/09 and docs/10 describe, and both documents are
now fully implemented (`2937034`). A plugin host owns the subsystems; Actions, Runs and the process
supervisor are shared services rather than each subsystem's own; Jobs are defined in configuration
with occurrence keys, misfire, DST and retry semantics; Data reaches its connections through a
catalog contract instead of reaching into MCP; and the source is nine shipping member crates
whose dependency edges the compiler enforces, still linking into one `swiss.exe` — plus the
dev-only `swiss-it` harness member (docs/44), which ships nothing. `docs/11` and `docs/12` record
the stages and their acceptance criteria.

**One acceptance task remains, and it is not code.** docs/01's rows are all filled now — the
realistic adapter workload measured 22.4 MB against a fresh Node run's 113.8 MB on the same
data directory and traffic (2026-09-11, `3b4934f`). Phase 6's calendar week is underway de
facto: since the W2 measurement the Rust build is the only gateway on the real port (Node
stopped — the two cannot hold the tunnel listeners simultaneously anyway), and it carries
daily traffic: all seven MCPs, the two SSH tunnel connections, remote terminal sessions
against both hosts, and the vim/htop/CJK acceptance legs. The week accrues by itself; the
swap-back path (shared data dir, Node still installable) is exactly the property Phase 6
exists to preserve.

## Phase 0 — The spike that can kill the project

**Do this before writing anything else. Budget: one sitting.**

Roughly 100 lines: an axum server on 19998, one `rmcp` `StreamableHttpService` with
`legacy_session_mode(false)`, serving a single hard-coded `echo` tool behind a bearer check.

Then prove all four:

1. A real MCP client (Claude Code, pointed at `http://127.0.0.1:19998/echo`) completes
   `initialize`, lists the tool, and calls it.
2. The modern `server/discover` probe is answered natively, not with method-not-found.
3. A second, older client that opens with the 2025-era `initialize` is also served.
4. `cargo build --release` with the docs/03 profile, and the resulting `.exe` RSS is read from Task
   Manager. **Write the number into docs/01.**

Separately, and just as blocking: **a unit test that opens a real sealed envelope.** Seal a fixture
with the Node build under `SWISS_MASTER_KEY`, decrypt it in Rust, assert the payload matches.
Then confirm by hand that a Rust binary can DPAPI-unseal the live `~/.swiss/gateway.config.json`.

**If (1)–(3) fail**, the port means hand-implementing the MCP protocol on both the server and the
client side, which is a different project with a different budget — stop and re-decide.
**If the envelope test fails**, the port is not a drop-in replacement and ADR-006 needs revisiting
before any more code is written.

## Phase 1 — Skeleton

Boot on the real data dir; serve the panel and `echo`. ~3,300 lines. See docs/04 for the file list.

Order within the phase matters:

1. `secure/` first — nothing can be read from disk until envelopes open.
2. `config` + `managed` — the state model.
3. `local_only` + `auth` + `token` — the boundary, ported test-first.
4. axum app + the embedded panel + the golden-response harness (docs/05 §3).
5. `registry` + `echo` + the `/api` subset the panel needs to render.

**Exit:** the binary boots on `~/.swiss`, the panel loads and every view renders (empty is
fine), `echo` answers a real client, and `/api/memory` gives the first Rust number for docs/01.

This is the moment the project's premise is confirmed or refuted. If the skeleton is already at
40 MB, stop and find out why before porting 12,000 more lines.

## Phase 2 — The direct adapters

mysql, pg, redis, and the whole Data view. ~5,900 lines — the largest phase by volume and the
smallest by risk. Take them in order (`sql` → `mysql` → `pg` → `redis`), because the first one
establishes the shared `tool_server` / `resources` / result-rendering shape and the rest follow it.

`calls.rs` and `traffic.rs` belong here rather than Phase 1: they are only meaningfully testable
once something real is being called.

**Exit:** all four DB MCPs serve a real client; the Data view browses, filters, edits, runs SQL and
exports CSV identically to the Node build. Second RSS comparison, now on a realistic workload — this
is the number that justifies the whole project.

## Phase 3 — `proc`

Small (~630 lines) and the most dangerous. Child process lifetime, orphan prevention, lazy wake,
idle reap.

The one deliberate improvement: **a Windows Job Object with `KILL_ON_JOB_CLOSE`** replaces the
tree-kill sweep. When the gateway dies for any reason — crash, Task Manager End Task, a debugger
detaching — the OS reaps the whole child tree. Keep the PID ledger anyway: the job object covers the
gateway dying, the ledger covers a child that outlived a previous *generation* of the gateway.

**Exit:** a `uvx`-launched MCP wakes on first request, is reaped after idling, and leaves nothing
behind when the gateway is hard-killed. Verify the last one with Task Manager, not with a test.

## Phase 4 — Tunnels and the remaining adapters

~3,400 lines. russh is the unknown; the `http`/`rest` adapters are routine.

Port the tunnel tests first (`tunnel-manager`, `tunnel-forward`, `tunnel-store`, `tunnel-ssh` —
1,657 lines of vitest between them). The invariant they encode is the one users actually feel: a
local port is bound only while its tunnel can carry traffic, so a dead tunnel gives an honest
`ECONNREFUSED` instead of accepting connections that go nowhere.

**Exit:** feature parity. Every panel view works against the Rust binary, including Tunnels.

## Phase 5 — `swiss` and shipping

~1,200 lines. The CLI is mechanical; the daemon is not — detached spawn, pid file, graceful-then-
force stop, and the Windows "no deliverable SIGTERM" problem the Node build solves by asking the
running gateway to shut itself down over HTTP. Port that approach; it is the reason children do not
get orphaned on stop.

**Exit:** `swiss start` / `stop` / `status` behave identically, and the `.exe` runs on a machine with
no Node installed. Build with `-C target-feature=+crt-static` so there is no MSVC redistributable to
chase — see ADR-006.

## Phase 6 — Cutover

Not code. Run both builds side by side for a week on the same data dir, Rust on the real port and
Node on a spare. Then swap. Keep the Node build installable for a release or two — the data
directory is shared, so falling back costs nothing, and that is exactly the property worth
preserving until confidence is earned.

## Sizing

| Phase | Backend lines | Risk | Notes |
| --- | --- | --- | --- |
| 0 — Spike | ~100 | — | Go / no-go |
| 1 — Skeleton | ~3,300 | Med | Confirms the premise |
| 2 — Direct adapters | ~5,900 | Low | Bulk typing, low surprise |
| 3 — proc | ~630 | **High** | Platform traps |
| 4 — Tunnels + rest | ~3,400 | **High** | russh is the unknown |
| 5 — CLI + shipping | ~1,200 | Med | Windows daemon semantics |

Phase 2 is 38% of the lines and perhaps 10% of the difficulty. Phases 3 and 4 invert that. Plan
attention accordingly, and do not let Phase 2's easy progress set the pace expectation for Phase 3.

## When to stop

Honest kill criteria, decided now rather than in the middle:

- **Phase 0 fails on protocol.** Stop. Hand-writing MCP on both sides is not this project.
- **Phase 4 stalls on russh.** Ship without tunnels behind a feature flag and keep the Node build
  for tunnel users. The tunnel view is the most self-contained subsystem here — it is the natural
  thing to cut, and cutting it does not block the memory win.

# 34 - Agent-Friendly Remote Execution (SSH Gateway)

> 2026-09-21: docs/41 amends this contract in three places - every remote command runs
> under a UTF-8 locale and every byte window ends on a character boundary (§1.1 there);
> every run carries an `actor` and the record keeps seven days whatever the budgets say
> (§1.2); `GET /api/remote/runs` takes `since` / `until` / `actor` and `swiss run audit`
> prints the window. Where this file and docs/41 disagree, docs/41 wins.

Phase R1-R5, implemented 2026-10. The goal: let an AI agent (or a human at a terminal) run
commands, upload source trees, and pull artifacts back on the machines the Tunnels plugin
already reaches - through the SAME run accounting everything else uses, with output an agent
can actually read.

## What ships (R1-R5)

- **`swiss-remote`** (new peer crate, deps: `swiss-core` + `swiss-host` only): the target
  table, the three actions, the `/api/remote` routes, the `.swiss/remote.json` project
  binding, and the sync/pull tree walkers. It never imports `swiss-tunnels` - it talks to
  the host `RemoteTransportRegistry` seat, and only the tunnels plugin sits in it.
- **Host transport contract** (`swiss-host/src/services/remote.rs`): `RemoteTransportProvider`
  (list / exec / stat / open_read / create / mkdir_p), `RemoteRead`/`RemoteWrite` streaming
  file halves, `RemoteError` (Unknown/Unavailable/Withdrawing/Unsupported/Canceled/Failed),
  and the one-provider registry with begin_withdraw/clear lifecycle.
- **Tunnels implements it** (`TunnelRemote` over `TunnelManager`): exec via one SSH "exec"
  channel, files via SFTP sessions, op-scoped connection leases so a transfer never steals
  a tunnel port forward mid-use. All argv crossing into a shell goes through ONE function,
  `quote_posix` in the tunnels crate, tested against the nasty strings.
- **Live run output** (host): `RunOutputSink` streams action output into the run buffer
  (256 KiB live window, 64 KiB finished tail) and `GET /api/runs/{id}/output?after=&max=`
  reads it with a monotonic cursor - the panel AND the CLI AND an agent poll the same URL.
- **The `remote` plugin** (root `src/plugins/remote.rs`): the #remote targets page, routes
  `/api/remote`, deliberately NO capability requirement - the target table stays editable
  while tunnels is off, and exec says honestly what is missing.
- **CLI** (`swiss remote ...`, `swiss run ...` in `src/remote_cli.rs`): endpoints/targets/
  target add|set|remove, resolve, exec/sync/pull, run status/logs/cancel. Everything after
  a bare `--` is ARGV for the far side, untouched. The exec command streams live output and
  exits with the REMOTE exit code.

## The target model

A target row (sealed in `remote.json`, strict JSON, credential fields refused loudly):

```json
{
  "id": "build",
  "endpoint": "conn-1",          // a Tunnels connection id - nothing else
  "workspaceRoot": "/data/ws/proj", // absolute POSIX, no . or .. segments
  "shell": "posix",
  "capabilities": ["exec", "sync"]
}
```

The endpoint names a connection the TRANSPORT serves; the row never carries a host, user,
password, key or passphrase - those live in tunnels.json sealed storage and never cross
this boundary. While a provider is serving, target CRUD refuses an unknown endpoint with
the known list; while none is, any non-empty id is accepted so config works with tunnels
disabled.

## Actions (a run, not a job system)

| action | input | notes |
|---|---|---|
| `remote.exec` | `{target, argv[], env?, cwd?, timeoutMs?}` | argv is an ARRAY. cwd is workspace-relative. Streams stdout+stderr live; outcome carries exitCode, canceled flag, target/endpoint meta. |
| `remote.sync` | `{target, source?, to?, exclude[], verbose?}` | Upload, NEVER deletes. A directory source walks the tree (skips `.git/ .swiss/ target/ node_modules/` plus caller excludes, uploads only changed sizes); a FILE source uploads just that file, renamed by `to` when given. One summary line. |
| `remote.pull` | `{target, remote, to?, verbose?}` | A workspace-relative FILE streams to a local path; a workspace-relative DIRECTORY recurses (list_dir over the transport, depth/count capped) into a local tree. |

Every action runs through the shared RunCoordinator: Run ID is the job ID, cancel is the
run cancel chain (`POST /api/runs/{id}/cancel` reaches the SSH channel close through the
transport lease), and the deadline is the run timeout. A cancel is an OUTCOME (canceled:
true, ok: false), not an error - the run row says "canceled", which is the truth.

## Security model

- **workspaceRoot is a guardrail, not a sandbox, and it anchors - it does not cage**:
  RELATIVE paths (cwd, sync, pull) resolve under it and a `..` that climbs is refused;
  an ABSOLUTE path is what the caller typed in full and is used as-is, the same trust an
  ssh command line gets. To work on /home/dev/app through a target rooted at
  /tmp/swiss, pass `--cwd /home/dev/app` (or pull `/home/dev/app/...`) - no
  per-directory target needed. The exec still runs as the SSH login user; the row
  documents intent, the machine enforces reality.
- **Privilege is the machine's business, and the surface is an SSH superset**: sudo passes
  through like any argv[0]. There is no PTY, so an interactive password prompt cannot be
  answered - passwordless sudo (NOPASSWD or `sudo -n`) is what works, exactly as over
  `ssh host "sudo ..."`.
- **No agent forwarding, no SSHFS, no interactive shells**: one exec channel per run, one
  SFTP session per file operation, bounded (3 s) drain on cancel.
- **POSIX quoting happens once**, inside the tunnels crate, through `quote_posix`: a safe
  set passes bare for readable command strings, everything else is single-quoted with the
  `'\''` splice - tested with embedded quotes, `$`, backticks, newlines, unicode.
- **Credentials never cross**: the consumer sees endpoint ids; tunnels resolves them.

## Project binding (`.swiss/remote.json`)

Plain JSON (no secrets - a repository carries it), discovered by walking up from the cwd,
first file wins, schemaVersion 1. The state home's own `remote.json` (the sealed target
table, which for `~/.swiss` sits at exactly this path for `~`) is not a binding: the walk
recognises the sealed envelope, skips it and continues above it (2026-09-20).

```json
{
  "schemaVersion": 1,
  "project": "demo",
  "defaultTarget": "dev",
  "actions": { "build": { "target": "dev", "workspace": "build/arm", "timeoutMs": 7200000 } },
  "sync": { "exclude": ["vendor/"] }
}
```

Name resolution order: `--target` > a target id > a project action > the binding default.
Deadline order: `--timeout` > action > target row > 2 h, capped at 24 h (the submit route
ceiling).

## CLI examples

```
swiss remote endpoints
swiss remote target add build --endpoint conn-1 --root /data/ws/proj --caps exec,sync
swiss remote exec build -- make -j8            # streams, exits with make exit code
swiss remote exec build --timeout 30m -- ./test.sh --filter "weird \"quoted\" name"
swiss remote exec --target build --detach -- make check   # 202 + run id
swiss remote sync build --source . --exclude vendor/
swiss remote push build app.exe            # one file, to the workspace root
swiss remote pull build out/app.bin --to artifacts/app.bin
swiss remote pull build out/dists          # a directory recurses into artifacts/out/dists
swiss run logs 17 -f; swiss run cancel 17
```

## Explicitly not this phase

- **The panel page (R6) shipped after the first cut**: #remote lists targets with an Add/Edit sheet over the same /api/remote routes (crates/swiss-panel/src/admin_assets/js/views/remote.js).
- **R7: the MCP adapter shipped**: five thin tools over /mcp/remote (swiss-mcp's
  adapters/remote.rs), dispatching these actions by name through the same run
  coordinator - a model can drive a target with no shell.
- **R8: the targets group family shipped** (see below).
- No second SSH client, no separate daemon, no sync delete, no shell pseudo-terminal, no
  multi-crate dependency edges between peers (the host seat is the only path).

## R8 - the targets group family (docs/20's seventh scope)

The target list is a group scope exactly like conns and rules: one scope word
(`targets`), the family's four routes (`PUT /api/groups/targets`,
`.../members/{id}`, `.../rename`, `.../order`), registered at the composition point
(swiss's server.rs) over the plugin's ONE system - so grouping outlives plugin
start/stop the way the tunnel scopes outlive a stopped tunnels plugin. The scope
implementation lives in `crates/swiss-remote/src/groups.rs`; the store carries the
names-plus-row-membership model tunnels.json uses, sealed in the same
`remote.json`:

```json
{ "groups": ["default", "prod"], "targets": [ { "id": "build", "group": "prod", ... } ] }
```

A pre-R8 file (a bare row array) still opens: no groups, one undivided list - exactly
the page it always was. The write door canonicalizes a row's group against the live
names (unknown group is a named error), a demoted first group pins its unpinned rows
to the name, and the scope's order is array order in the file.

The panel renders the list through the shared groups component (page density, drag
both ways): a row drag reorders the flat list, a drop into another group moves the
row, and the Add/Edit sheet carries the family's Group select - one POST, the row is
born into its group. The names ride with the rows in the one `GET /api/remote/targets`
response, so the page paints from a single read.

## R9 - the run log (2026-09-18)

The coordinator's finished ring is 32 views in memory and gone at restart; the build an
agent kicked off at 3 a.m. must still be readable in the morning, in the panel. So every
`remote.*` run now leaves a durable record beside the target table:

```
~/.swiss/logs/remote/
  runs.jsonl          one line per FINISHED run: the /api/runs row as it was (runId, state,
                      exitCode, ms, timedOut/canceled, error, meta.target/endpoint) plus
                      input (argv, cwd, target, sync/pull paths; env as envKeys ONLY - the
                      values may be secrets), outputBytes, and outputCapped + tail when the
                      output file hit its cap
  out/<runId>.txt     the whole stdout+stderr stream, teed as it flowed (head up to 16 MiB;
                      past that the record keeps the last 64 KiB as `tail`)
```

Budgets, oldest-first: **30 days**, **500 MiB** on disk (index plus output files), 5000
runs. Checked in O(1) on every finish and every page read from a tracked ledger; the full
pass (two streaming walks, a tmp+rename rewrite) runs only when a budget is over.

The seam is the host's (`RunHistorySink` in `swiss-host/src/services/runs.rs`): a sink
sees a run START — and hands back a tee that receives every output append — then its
terminal view once, before the view enters the finished ring. The remote plugin registers
`swiss-remote/src/history.rs` at start and removes it at stop; the host never learns a
directory. `RunView` gained `meta` (the action's outcome extras) so a run row can say what
it ran against.

Routes, under the plugin's `/api/remote`:

| route | answers |
|---|---|
| `GET /api/remote/runs?before=&limit=&target=` | `runs` (recorded, newest first, paged by run id), `active` (queued/running remote runs from the coordinator, with their in-flight `input`), `nextBefore`, `limits`, `usage` |
| `GET /api/remote/runs/{id}` | one record (404 when unknown or evicted) |
| `GET /api/remote/runs/{id}/output?after=&max=` | the recorded stream from a byte cursor, the live route's shape plus `total` |
| `DELETE /api/remote/runs` | forget everything |

The panel: **Remote / Runs**, the plugin's second page (the context bar switches Targets
and Runs — sibling pages, never page-local tabs). A Traffic-shaped card: live runs first
(output followed on a 1.5 s timer while the row is open, Cancel in the body head), the
record under them; a row opens to its output (128 KiB a read, Load more), the capped tail,
the error; a target filter and Clear. `views/remote-runs.js`; vitest
`admin-remote-runs-view.test.ts`.

## Where the tests live

- `swiss-host`: registry/contract fakes + run output buffer cursor semantics (runs.rs).
- `swiss-tunnels`: `quote_posix` cases, fake-transport exec/stat/lease tests, and two REAL
  russh server tests (exec round-trip, cancel of a never-finishing command).
- `swiss-remote`: target validation/store, project binding discovery, all three actions
  (streaming, tail bounds, cancel outcome, sudo passes through, unknown target, honest
  no-transport error, sync skip/upload, pull round-trip), `/api/remote` route policy, and
  the submit-to-output E2E chain through the REAL host run routes. The run log (R9):
  `history.rs` unit tests (tee + record, env keys only, cap + tail, the three budgets,
  paging + filter, orphan sweep, clear) and the `recorded` chain test (a real run through
  the real routes leaves a readable record). The group contract:
  store family semantics (pin/rename/reorder/legacy shape), the scope unit test, and the
  root integration test `the_family_serves_the_targets_scope` (tests/adminapi.rs).
- Root: plugin lifecycle (start registers/stop withdraws and unregisters), CLI parsing
  (the `--` contract, durations, env pairs).

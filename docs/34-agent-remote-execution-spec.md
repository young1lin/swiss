# 34 - Agent-Friendly Remote Execution (SSH Gateway)

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
first file wins, schemaVersion 1:

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
- **R7: the MCP adapter** - the actions already appear in `GET /api/actions` with schemas;
  an MCP tool wrapper is additive and NOT built now.
- No second SSH client, no separate daemon, no sync delete, no shell pseudo-terminal, no
  multi-crate dependency edges between peers (the host seat is the only path).

## Where the tests live

- `swiss-host`: registry/contract fakes + run output buffer cursor semantics (runs.rs).
- `swiss-tunnels`: `quote_posix` cases, fake-transport exec/stat/lease tests, and two REAL
  russh server tests (exec round-trip, cancel of a never-finishing command).
- `swiss-remote`: target validation/store, project binding discovery, all three actions
  (streaming, tail bounds, cancel outcome, sudo passes through, unknown target, honest
  no-transport error, sync skip/upload, pull round-trip), `/api/remote` route policy, and
  the submit-to-output E2E chain through the REAL host run routes.
- Root: plugin lifecycle (start registers/stop withdraws and unregisters), CLI parsing
  (the `--` contract, durations, env pairs).

# 34 - Agent-Friendly Remote Execution (SSH Gateway)

> 2026-09-21: docs/41 amends this contract in three places - every remote command runs
> under a UTF-8 locale and every byte window ends on a character boundary (§1.1 there);
> every run carries an `actor` and the record keeps seven days whatever the budgets say
> (§1.2); `GET /api/remote/runs` takes `since` / `until` / `actor` and `swiss run audit`
> prints the window. Where this file and docs/41 disagree, docs/41 wins.

> 2026-09-22 amendment (`fdea8e4`): the MCP `remote_exec` tool's `env` is an OBJECT of
> name to string and reaches `remote.exec` in exactly that shape. The adapter used to
> serialize env into `K=V` strings, which the action's object-only door refused - every
> env-bearing MCP exec failed validation before the wire. The adapter validates the shape
> (object, string values) and passes it through verbatim; identifier validation and
> deterministic ordering stay in the action. Catch-up in the same pass: the actions table
> below gains the `remote.cat` / `remote.write` rows that shipped 2026-09-17 (`c00e771`)
> without one.

Phase R1-R5, implemented 2026-10. The goal: let an AI agent (or a human at a terminal) run
commands, upload source trees, and pull artifacts back on the machines the Tunnels plugin
already reaches - through the SAME run accounting everything else uses, with output an agent
can actually read.

## What ships (R1-R5)

- **`swiss-remote`** (new peer crate, deps: `swiss-core` + `swiss-host` only): the target
  table, the five actions (exec, sync, pull, cat, write), the `/api/remote` routes, the
  `.swiss/remote.json` project binding, and the sync/pull tree walkers. It never
  imports `swiss-tunnels` - it talks to the host `RemoteTransportRegistry` seat, and
  only the tunnels plugin sits in it.
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
  target add|set|remove, resolve, exec/sync/push/pull/cat/write, run status/logs/cancel/audit.
  Everything after a bare `--` is ARGV for the far side, untouched. For exec the same cut
  happens at the first command word - the second positional after the subcommand, or the
  first when `--target` already named the target - so `exec t ls -a` needs no `--`, local
  flags end there, and a word before a bare `--` is no longer dropped (`exec t make -- -k`
  sends `["make","-k"]`). Flag-shaped tokens BEFORE the command word are still local
  errors. The exec command streams live output and exits with the REMOTE exit code.

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
disabled. The CLI's `--endpoint` accepts either that stable id or one exact, unique endpoint
display name - the `label` `GET /api/remote/endpoints` serves, shown as the NAME column of
the `endpoints` listing - resolving the latter to the id before it writes the row (an exact
id always beats a colliding label). Duplicate names are refused listing both ids and require
an id; display-name convenience never becomes persisted identity, and while no transport is
serving there is no inventory to resolve against, so the selector is stored verbatim. The
human-readable `endpoints` listing puts names before ids, and `targets` shows endpoint labels
instead of UUIDs, falling back to the id on an empty label or unknown id; `--json` keeps the
canonical ids.

## Actions (a run, not a job system)

| action | input | notes |
|---|---|---|
| `remote.exec` | `{target, argv[], env?, cwd?, timeoutMs?}` | argv is an ARRAY; env is an OBJECT of name to string - the only shape the action accepts, and the shape the MCP tool passes through verbatim (`fdea8e4` above). cwd is workspace-relative. Streams stdout+stderr live; outcome carries exitCode, canceled flag, target/endpoint meta. |
| `remote.sync` | `{target, source?, to?, exclude[], verbose?}` | Upload, NEVER deletes. A directory source walks the tree (skips `.git/ .swiss/ target/ node_modules/` plus caller excludes, uploads only changed sizes); a FILE source uploads just that file, renamed by `to` when given. One summary line. |
| `remote.pull` | `{target, remote, to?, verbose?}` | A workspace-relative FILE streams to a local path; a workspace-relative DIRECTORY recurses (list_dir over the transport, depth/count capped) into a local tree. |
| `remote.cat` | `{target, remote}` | The model-facing read primitive (`c00e771`): one workspace-relative FILE as the run output, buffered whole and decoded once - larger than 128 KiB is refused with "use remote.pull". Same path rule as pull; needs the `files` (or `sync`) capability. |
| `remote.write` | `{target, remote, content}` | The mirror write primitive: a content string becomes the remote file (create/overwrite, chunked upload). Same path rule; needs the `files` (or `sync`) capability. |

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

## Per-target notes (`~/.swiss/remote-notes/`)

The shipped skill's memory layer: free-form, agent-written, one `<alias>.md` per target under
the state home - never the skill directory, which `swiss skill install` replaces wholesale on
every upgrade. Machine-checkable facts stay in project bindings (above); notes carry only what
prose can (command lines, log and service locations, ports, the project-to-path map) and never
credentials. The skill teaches read-before-discovery, dated entries, and cheap re-verification
(`resolve`, `exec <t> -- pwd`) over trust: the gateway's own answer outranks the note.

## CLI examples

```
swiss remote endpoints
swiss remote target add build --endpoint "Build server" --root /data/ws/proj --caps exec,sync
swiss remote exec build -- make -j8            # streams, exits with make exit code
swiss remote exec build --timeout 30m -- ./test.sh --filter "weird \"quoted\" name"
swiss remote exec --target build --detach -- make check   # 202 + run id
swiss remote sync build --source . --exclude vendor/
swiss remote push build app.exe            # one file, to the workspace root
swiss remote pull build out/app.bin --to artifacts/app.bin
swiss remote pull build out/dists          # a directory recurses into artifacts/out/dists
swiss remote cat build logs/build.log      # one remote file to stdout (128 KiB limit)
swiss remote write build notes.md < notes.md   # stdin becomes the whole remote file
swiss run logs 17 -f; swiss run cancel 17
```

## Explicitly not this phase

- **The panel page (R6) shipped after the first cut**: #remote lists targets with an Add/Edit sheet over the same /api/remote routes (crates/swiss-panel/src/admin_assets/js/views/remote.js).
- **R7: the MCP adapter shipped**: five thin tools over /mcp/remote (swiss-mcp's
  adapters/remote.rs) - `remote_exec`/`remote_sync`/`remote_pull`/`remote_cat`/
  `remote_write`, one per action - dispatching by name through the same run coordinator,
  so a model can drive a target with no shell. `remote_exec` takes `target, argv[],
  cwd?, env?, timeoutMs?`; env is an OBJECT of name to string, the action's own shape,
  passed through verbatim (`fdea8e4`) - the adapter checks the shape, the action checks
  identifiers and sorts; unknown arguments are refused at the tool door.
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

## R10 - vault references in exec (2026-09-28)

The owner's ask: a remote command should be able to say `${secret://name}` (or
`${secret://name:default}`, docs/19's 2026-09-28 grammar) and have it replaced by the stored
value on the way out, while every page keeps showing the reference. Before this, `remote.exec`
expanded nothing: a reference reached the far side as its own literal text (the market-feed demo
authenticated with the 31-byte string `${secret://test-redis-password}` and got WRONGPASS).

- **Where**: `resolve_exec_refs` in `crates/swiss-remote/src/actions.rs` resolves argv words,
  env VALUES and cwd with `refs::resolve_secrets_collect` - the vault family only. `${UPPER}`
  stays text: the gateway's own environment is not the far side's, and argv is single-quoted
  by `quote_posix`, so `sh -c` is the way to let the remote shell expand a variable.
- **When**: inside the action, after the coordinator recorded the run. The run list, the audit
  line (`envKeys` only, as ever), the panel's Runs page and `swiss run status` keep the input
  as typed; the resolved values exist only in the request that leaves for the SSH channel.
- **Refused before anything is sent**: a reference to a name the vault lacks (and no default)
  fails the run before the SSH channel opens, naming the field
  (`env.REDISCLI_AUTH references secret://x which is not in the vault`). `validate_input`
  checks the SYNTAX only (`refs::check_secret_refs`, which never reads the vault): it also runs
  when jobs.json is saved and at boot, where a failing definition is dropped, and a job that
  names a secret stored later must survive both.
- **Masked output**: the resolved values (8 bytes or longer - a shorter one would mask common
  words) are replaced with `••••••••` before a byte reaches the live buffer, the record or the
  outcome. `swiss_host::mask::StreamMask` holds back a chunk tail that could be the start of a
  value, so a secret split across two SSH reads is still caught; the stream's end releases
  what was held. A default is the operator's own literal and is not masked.
- **Prefer env over argv**: an argv word is visible to every user of the remote machine in
  `ps`; `--env NAME='${secret://x}'` is not. Quote the reference with single quotes in both
  PowerShell and bash - in double quotes each shell reads `${...}` as its own variable.

Tests (`actions.rs`): `a_vault_reference_reaches_the_far_side_resolved_and_comes_back_masked`,
`a_missing_vault_reference_is_refused_before_anything_runs`,
`a_malformed_reference_is_refused_by_validation_naming_its_field`,
`the_live_buffer_is_masked_too`; `refs.rs`: `a_syntax_check_never_consults_the_vault`;
`mask.rs`: three `StreamMask` cases (a split secret, the held-back tail, short values skipped).

## R11 - remote runs in parallel (2026-09-28)

The owner's question: can several terminals drive one server at once through a reused SSH
connection? The connection was never the limit - every exec opens its own session channel
on the target's one connection (`ssh.rs` `exec`), and SSH multiplexes them. The limit was the
run coordinator: one pool for every run, `maxConcurrentRuns` = 2, and a submission past it is
refused. Measured on the test server: two `sleep 4` in parallel took 5.0 s in all, the third
got `429 run capacity is full (2/2 running)`. A detached run holds its slot until it ends,
so one long feed left a single slot for everything else.

That pool protects THIS machine (a local process costs its CPU); a remote run costs a channel
there and a buffer here. So the coordinator now has lanes (`RunLane`, `services/action.rs`):
`Action::lane(input)` defaults to Local - the shared pool, unchanged - and all five remote
actions answer `Remote(<target>)`. Each target's lane holds
`DEFAULT_MAX_REMOTE_RUNS_PER_TARGET` = 8 runs: OpenSSH's `MaxSessions` allows 10 channels
per connection by default, which leaves room for a terminal tab and a file transfer beside
them. The ninth is refused naming the target (`remote target test already has 8/8 runs
going`); another target has its own 8. A queued run (a job's queue-one) waits for its own
lane only, so a remote run waiting on a busy target never holds back a local one.
`GET /api/runs` reports the bound as `capacity.maxRemoteRunsPerTarget`.

Tests: `runs.rs` `remote_runs_take_their_targets_lane_not_the_local_pool`,
`a_queued_run_waits_for_its_own_lane_only`; `actions.rs`
`every_remote_action_runs_in_its_targets_lane`.

## R12 - what a write wrote, kept sealed (2026-09-28)

The owner, on a `write stream-demo.py` row that showed only `Output 34 B`: "I don't know what
was written, only a result." That morning the record had stopped keeping a write's body
(e5bc766) because `runs.jsonl` held it in clear - writing a `.env` put its password in the
audit log for 30 days. Asked to choose, the owner picked: keep the body, encrypted.

- `history.rs` seals a remote.write's `content` into `out/<runId>.content` with
  `write_secure_json` - the AES-256-GCM envelope under the machine key that every state file
  uses - capped at `CONTENT_MAX_BYTES` (256 KiB, cut on a character boundary, the line says
  `contentTruncated`). Kept whatever the outcome: a failed write's body is what failed to
  land. The index line keeps `input.contentBytes` and gains `contentStored` /
  `contentFileBytes`; the body never enters `runs.jsonl`.
- The sealed file is an evictable file like the output: it counts in the byte budget, goes
  with its record (age, count, Clear), and under byte pressure inside the seven-day window it
  goes with the output file, the line then saying `contentEvicted`.
- `GET /api/remote/runs/{id}/content` unseals it for the signed-in panel (404 when the record
  kept none). The Runs page shows a "Content written" block with the size and a Show button;
  nothing is fetched until it is pressed, and the unsealed text lives in the page's memory
  only while the page is on screen (Hide, Clear and leaving the page drop it).

- 2026-09-28, the same day: the owner, on a `write seed.sql` row from the night before —
  "this still isn't solved". Its body was never missing. Records written before e5bc766 carry
  it in CLEAR under `input.content` (the very thing e5bc766 stopped), and the panel shows a
  sealed body only, so those rows looked empty while the file sat in `runs.jsonl`. Opening the
  log now moves them: `seal_legacy_content` walks the index once, seals each clear body beside
  its record and rewrites the line the way `record` writes one today (tmp + rename, as evict
  does). A line whose seal fails — no machine key yet — is left exactly as it is, so the next
  open retries rather than dropping what it could not keep; a torn line goes, as in evict.
  Nothing to move is one streaming walk and no rewrite, which is every start after the first.
  The open-time orphan sweep now takes `out/<id>.content` as well as `out/<id>.txt`.
- The window between e5bc766 and R12 keeps its size and nothing else — those bodies are gone.
  Such a row used to draw no content block at all, which reads exactly like one whose Show has
  not been pressed; it now says the write was recorded before content was kept.

Tests: `history.rs` `a_remote_write_keeps_its_content_sealed_beside_the_record`,
`a_long_write_keeps_its_head_cut_on_a_character_boundary`,
`the_sealed_body_counts_against_the_budget_and_leaves_with_its_record`; `lib.rs`
`a_finished_remote_run_is_readable_from_the_record` (a write through the real routes, then
`/content`); vitest `admin-remote-runs-view.test.ts` (Show / Hide, the evicted note). The
migration: `a_legacy_records_clear_body_is_sealed_when_the_log_opens`,
`the_seal_at_open_leaves_a_record_it_has_already_moved_alone`,
`orphaned_output_files_are_removed_at_open_and_clear_forgets_everything` (both file kinds),
and vitest "a write recorded while no body was kept says that, rather than drawing nothing".

## R13 - the whole command, copyable (2026-09-28)

The owner, on a Runs row: "this command can only be looked at - not copied, and I can't see
what is actually in it." The row's line joins the argv with plain spaces and is cut with an
ellipsis to fit, so a long `sh -c '…'` was never readable whole.

- An open row's body now starts with a Command block, above the output: an exec's argv quoted
  word by word the way `quote_posix` (swiss-tunnels `tunnel/remote.rs`) sent it - the same
  safe set, the same `'\''` splice - so a pasted copy runs as the record says; a file action
  reads as its kind and path (`write C:/Users/me/seed.sql`). Its Copy puts that line on the
  clipboard. A reference stays as typed (`${secret://name}`), as everywhere in the record.
- The live output's in-place update finds its block by a `data-rlive` mark: it took the
  body's first `pre`, which is now the Command.

Tests: vitest `admin-remote-runs-view.test.ts` (the quoted line and its Copy, a write's
Command), `remote-runs-live-tail.test.ts` (a pull writes the live block and leaves the
Command alone).

## R14 - running a target from the page (2026-09-28)

The owner, on the Remote page: "it should let me run write and exec straight from the web, and
Runs should show what is running so I can cancel it." The second half already held - an open run
streams its tail and carries Cancel - so this is the door, and nothing new on the server.

- A target's `⋯` offers `Run a command…` when it holds `exec` and `Write a file…` when it holds
  `files`, and neither otherwise: a door whose only answer is the gateway's refusal is not a
  door. Each sheet submits `POST /api/runs` with `actor: "panel"` - the same route the CLI
  knocks on - and then goes to `#remote-runs`, where the run is watched, cancelled and recorded
  like every other.
- A typed command is sent as `sh -c <line>`, not split here. A shell parser written in the panel
  would be a second opinion about quoting, and the operator's pipes, quotes and redirects would
  mean whatever it decided; the target's own shell is the one that matters, and the Runs page's
  Command block (R13) shows exactly what was sent.
- The write sheet takes a path (relative to the workspace root, as the CLI takes it) and a body.
  The body is sealed beside the record (R12), so the row can show it back.

Tests: vitest `admin-remote-view.test.ts` - the capability gating, the submitted exec (argv,
optional cwd, timeout, actor), the refusal of an empty line, and the submitted write.

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
  (the `--` and first-command-word contract, endpoint name resolution, durations,
  env pairs).

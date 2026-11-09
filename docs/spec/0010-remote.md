# §remote — Remote execution

Chapter 0010 of the [swiss specification](../SPEC.md).

The agent-facing way to run a build or a test on a machine the gateway reaches over SSH,
without the agent holding a credential or an interactive shell. A **target** is an alias the
agent types; the gateway resolves it to a tunnels connection, runs the command there and keeps
the record. A remote action **is a run** through the host's coordinator (§host.actions) — the
same queue, cancel, cursor and history as every other run — not a second job system.

| Where | What |
|---|---|
| `crates/swiss-remote` | `target` (the table), `actions` (the five actions), `sync` (file movement), `project` (the binding), `history` (the run log), `groups` (the targets scope), `api` (`/api/remote`) |
| `crates/swiss-plugins/src/remote.rs` | The descriptor, start/stop, the builtin MCP mount |
| `src/remote_cli.rs` | `swiss remote …`, `swiss run …` |
| `crates/swiss-mcp/src/adapters/remote.rs` | `/mcp/remote` (§remote.mcp) |
| `crates/swiss-tunnels/src/tunnel/remote.rs` | `TunnelRemote`, the transport provider, and `quote_posix` |
| Panel `views/remote.ts`, `views/remote-runs.ts` | The Targets and Runs pages |
| `src/skill_assets/remote/SKILL.md` | The `swiss-remote` skill (§remote.project) |

`swiss-remote` depends on `swiss-core` and `swiss-host` only — never on `swiss-tunnels`. The
endpoint is an id resolved through the host's remote transport seat (§host.seats), so the SSH
client, the credentials and the quoting stay in tunnels (§tunnels.providers).

**Descriptor.** id `remote`, label Remote, version 0.1, config schema version 1 with an empty
schema (`additionalProperties: false`: nothing is configurable, and a typo still fails at
save). Two pages switched in the context bar, both `sidebar: false`: `remote` (Targets, order
75) and `remote-runs` (Runs, order 76). Routes `/api/remote`. `restart_on_config_change:
false`. **`requires` is empty on purpose**: the target table stays editable while Tunnels is
off, and a run says honestly what is missing when it runs.

**Lifecycle.** The factory opens the sealed `remote.json` once, so the groups scope and the
routes see the same table across restarts. Start registers the five actions into the shared
registry, attaches the run-history sink (§remote.history), installs the system into the
routes' slot, and mounts the builtin `remote` MCP at `/mcp/remote`. If a user MCP already owns
that name, theirs wins, a warning is logged and the rest of the plugin is unaffected. Stop
takes all of it back; a run already accepted still gets its record.

**Out of scope.** Interactive shells (§terminal), a PTY, agent forwarding, SSHFS, remote
deletes, non-POSIX targets (a `cmd.exe` would be mis-quoted, so `shell` is `posix` only), and
scheduling (§jobs can submit a remote action).

## §remote.targets — Targets and the routes

A target row, camelCase on the wire:

| Field | Rule |
|---|---|
| `id` | A slug: 1–64 of `a-z 0-9 -`, starting with a letter or digit, no trailing or doubled dash. Immutable — an update cannot rename |
| `label` | Human label; defaults to the id; not empty |
| `endpoint` | The endpoint id — for the tunnels transport, a tunnels connection id |
| `workspaceRoot` | An absolute POSIX path: starts with `/`, no `\`, NUL, empty, `.` or `..` segment (so no trailing slash) |
| `shell` | `posix` — the only family the quoting is proven for |
| `capabilities` | A subset of `exec`, `files`, `sync`, no duplicates; `exec` is required |
| `defaultTimeoutMs` | Optional, a positive integer |
| `group` | Optional; the targets scope of §host.groups (`/api/groups/targets`). Absent means the first group; an unknown group is a named 400 |

- **No credentials, ever.** A row names no host, user, password or key; those live in
  `tunnels.json` and never cross the seat, so a row can be printed, committed or read aloud.
  The parser refuses, by name, the denylist `host`, `port`, `username`, `user`, `password`,
  `passphrase`, `key`, `keyPath`, `privateKey`, `identityFile`; every other unknown field is
  refused too.
- **Validated at every entry** — store load, API write, action resolve — so nothing downstream
  defends against a half-formed row.
- **Endpoint policy.** While a transport provider is serving, a row may only name an endpoint
  the provider lists (a typo fails at save, not at exec). With no provider serving, any
  non-empty id is accepted.
- **Store.** The sealed `~/.swiss/remote.json` (§core.sealed), written through on every
  mutation. A pre-groups file (a bare array) still opens.
- **Every write is announced** (REMOTE.TARGETS, a notice, §bus.topics). Each write of
  `remote.json` — a route, the CLI through the same routes, a group operation of the targets
  scope — publishes once the file is written, while the table's lock is still held. A refused
  write publishes nothing.

| Route | Answers |
|---|---|
| `GET /api/remote/endpoints` | `{presence, provider, endpoints}` — the provider's inventory: ids, labels and live state; no hosts, users or credentials. `presence` lets a client say "start Tunnels" instead of guessing |
| `GET/POST /api/remote/targets` | The rows with the group names; add (a duplicate id is refused) |
| `GET/POST/DELETE /api/remote/targets/{id}` | Read, update, delete → `{removed}` |
| `GET /api/remote/runs` | §remote.history |
| `DELETE /api/remote/runs` | Clear the record |
| `GET /api/remote/runs/{id}`, `/output?after=&max=`, `/content` | One record, its stored output, the sealed write content |

The routes mount once at boot inside the loopback guard and the plugin boundary; the system
behind them exists only while the plugin serves. The boundary answers the structured 503 for a
disabled plugin, and the handlers check the slot anyway (a mounted router never assumes who
mounted it).

## §remote.actions — The five actions

| Action | Input (strict; unknown fields refused) | Needs |
|---|---|---|
| `remote.exec` | `{target, argv[], env?: {NAME: value}, cwd?, stdin?, timeoutMs?}` | `exec` |
| `remote.sync` | `{target, source, exclude?[], verbose?, to?, delta?, base?}` — upload a local tree, or one file (`to` names it remotely) | `sync` |
| `remote.pull` | `{target, remote, to?, verbose?}` — download one file or a directory tree | `sync` |
| `remote.cat` | `{target, remote}` — one file's text, at most 128 KiB ("use remote.pull instead") | `files` or `sync` |
| `remote.write` | `{target, remote, content}` — create or overwrite one file with the whole content | `files` or `sync` |

- **The shape every action shares.** Validate strictly, resolve the target under the store
  lock, do the work lock-free against the transport, stream output into the run's live buffer
  as it arrives. The outcome carries a bounded 64 KiB tail plus counters, never the whole log.
- **Lanes.** Every remote action runs in its target's lane (`RunLane::Remote(target)`), outside
  the local pool: at most 8 runs per target by default (`capacity.maxRemoteRunsPerTarget`;
  OpenSSH's `MaxSessions` defaults to 10, which leaves room for a terminal and a forward). One
  more is refused with 429: "remote target dev already has 8/8 runs going; retry later".
- **Owners.** Runs submitted through `/api/runs` are the host's; the plugin's own are `remote`;
  MCP-initiated ones `remote-mcp` — so one population's cancels and shutdown sweeps never reach
  another's.
- **Paths.** `workspaceRoot` anchors, it does not cage. A relative `cwd` or `remote` resolves
  under it; an absolute one is used as typed; a `..` segment is refused in either form. The
  command itself can `cd` anywhere.
- **Stdin.** An exec's `stdin` is a string of at most 1 MiB (`STDIN_MAX_BYTES`), sent to the
  command's standard input exactly as given — never quoted, trimmed or re-ended — and then
  closed. Without it stdin is closed at once, so a command that reads it (`cat`, `psql` without
  `-c`, `read`) sees EOF instead of waiting out its deadline. This is how text with quotes in it
  travels: SQL for `psql -X`, a script for `sh -s`. No shell between the caller and the command
  parses stdin, so nothing has to be escaped for one. Bigger input goes up with `remote.sync` and
  is redirected from the file.
- **Sync** is upload-only and **never deletes** — a sync that removes remote files is a footgun
  no agent should hold. It walks the local tree, skips the default excludes (`.git/`,
  `.swiss/`, `target/`, `node_modules/`, by path prefix) plus the binding's and the input's,
  and uploads a file that is missing, whose remote size differs, or whose whole-second mtime
  differs — rsync's quick check: equality only, never newer or older, so disagreeing clocks
  cannot loop; the local time is floored to seconds because SFTP's is, and a stat that reports
  no mtime falls back to the size-only rule. An upload stamps the remote file with the local
  mtime — whole files through the transport's set-mtime (SFTP SETSTAT; a provider without one
  refuses once per run and the mtime stays the upload time), a delta by the script's utime
  before the rename. A same-size file whose mtime differs is content-checked before it is
  sent: files uploaded before this rule carry the upload time as their mtime, and the first
  sync after it must not re-upload a whole workspace. One `sha256sum` exec covers a batch of
  up to 200 paths (argv, never a shell string; the escaped-name output is parsed by name, and
  a path the command cannot read reads as changed); a file whose digests match keeps its bytes
  and has only its mtime fixed, counting as up to date, and a target without `sha256sum`
  sends the batch whole with one note. Caps: 20,000 files, depth
  32. Pull caps: 2,000 files, depth 32. Both report compactly — a summary line
  (`sync: N scanned, N uploaded (N bytes), N up to date`, with `; N by delta, N bytes sent`
  inside the parentheses when a delta went), one line per file only when `verbose`. The
  outcome's meta carries `delta` (files sent as a delta) and `sent` (content bytes that crossed
  the wire).
- **Delta.** A changed file of 1 MiB or more (`DELTA_MIN_BYTES`) whose previous version is on
  the target goes up as a delta — rsync's algorithm, no rsync on either side. The target
  computes block signatures (adler32 weak, sha256 cut to 16 bytes strong; the block is
  √len rounded up to a power of two, within 4–128 KiB) with a python3 script sent as stdin.
  The gateway rolls the weak checksum over the local file and uploads a patch of copy runs and
  literal bytes to a hidden `.<name>.<hex>.swiss-delta` beside the destination. The script
  rebuilds into a temporary in the same directory, checks the whole file's sha256, stamps the
  local mtime, and renames it over the destination. The previous version is the destination
  itself; for a single file
  whose destination does not exist yet it is `base` when given, else the newest sibling with the
  same extension (any case), not a dot file, within ×2 of the size — rsync's `--fuzzy`. A target
  without python3 (exit 127) turns delta off for the rest of the run with one note; any other
  delta failure sends that file whole with a note naming the reason, and the patch is removed.
  `delta: false` runs nothing remote; `base` with `delta: false`, or `base` on a tree, is
  refused.
- **Whole-file writes are not atomic**: a remote file is streamed in 64 KiB chunks over one SFTP
  session and overwritten in place. A delta write is: the rename lands the whole file or nothing.
- **Cancel is an outcome**, not an error: `canceled: true`, `ok: false`. The channel is closed
  from this side, a bounded drain (3 s) collects what the far side flushes, and the run
  resolves as canceled — no future-dropping pretence.

## §remote.security — What a target can and cannot do

- **The login user's rights, exactly.** A run executes as the SSH login user of the
  connection; `sudo` works only non-interactively (`NOPASSWD`, `sudo -n`). The gateway adds no
  sandbox and claims none: `workspaceRoot` is a guardrail against a mistyped path, not a jail.
- **One channel per run.** One exec channel per run, one SFTP session per file operation. No
  PTY, no agent forwarding, no port forwarding, no SSHFS.
- **Credentials never cross.** Remote sees endpoint ids; tunnels keeps hosts, users and keys
  (§tunnels.store). Auth and host-key failures point back at the Tunnels page.
- **One quoting step.** `quote_posix` (in tunnels' `remote.rs`) is the only argv → string step
  in the tree: a conservative safe word (ASCII alphanumerics and `_ . : = / @ % + , -`) passes
  bare so `ps` stays readable; everything else is single-quoted with an embedded `'` spliced as
  `'\''`; an empty argument is `''`. The command string is `export NAME='v'; … cd 'cwd' && exec
  'argv0' 'argv1' …` — env values are quoted like arguments, so `$(…)` or `;` in a value stays
  data. No `join(" ")` shortcut exists anywhere. Stdin never passes through it: it is data on
  the channel, not part of the command.
- **Vault references in exec.** argv, env values, cwd and stdin may carry `${secret://NAME}`
  references (§host.vault); a `${UPPER}` stays literal text. `validate_input` checks syntax
  only. Resolution happens after the run is recorded — the record keeps the reference as typed —
  and a missing secret fails before any channel opens, naming the field. Every resolved value
  of 8 bytes or more is masked (`••••••••`) before a byte reaches the live buffer, the record or
  the outcome, and so is every other value the vault stores (a `.env` the command prints);
  `remote.cat` masks every stored value the same way. A `:-default` is not masked. Prefer env over argv — an env value shows in the far
  side's `ps` only until the login shell execs, an argv word for the command's whole life — and
  single-quote a reference in a shell so the local shell leaves it alone. `sudo`'s env_reset
  drops an env value unless the command names it (`sudo -n --preserve-env=NAME …`); the skill
  puts `sudo` on the one command that needs root, never on an interpreter (`sudo -n python3 -`
  runs a whole stdin script as root).
- **No credential typed into a command line** (2026-10-22). `remote.exec` refuses an argv or a
  cwd holding a credential as text, before anything is sent: `swiss_core::secure::literal`
  reads the argv as the one command line it makes and finds a value in six slots — an
  assignment whose name's last word is a credential word (`REDISCLI_AUTH=`, `PGPASSWORD=`,
  `spring.redis.password=`, `?token=`; `DB_PASSWORD_FILE` and `PWD` are not), a long option
  (`--password v`, `--api-key=v`, `-Dx.password=v`), an `Authorization:` or credential-named
  hyphenated header, a JSON member (`"password": "v"`), a URL's `:v@` and a client's own flag
  (`redis-cli -a v`, `sshpass -p v`, `mysql -pv`). A reference (`${secret://…}`, `$VAR`,
  `$(…)`), a placeholder, a path, the next flag, a mask or a flag word is no literal. The error
  names the field and the slot, never the value, and points at the vault and env. It is in
  `parse`, so `validate_input` refuses it too (a jobs.json definition with one is refused at
  save and dropped at boot). env values and stdin are not scanned: neither stays in `ps` nor
  lands in a record. Defence in depth, not a guarantee — a value under an innocent name passes;
  the records mask what the detector finds anyway (§remote.history).
- **Recorded, not trusted.** Every run is recorded with who asked (§remote.history); the CLI's
  actor is self-declared, the MCP's is the authenticated token.

## §remote.transport — The transport seat

The host defines the seat (`services/remote.rs`, §host.seats); tunnels provides it
(`TunnelRemote`, §tunnels.providers).

- **Operations:** `list` endpoints, `exec` (stdout and stderr streamed as they arrive, never
  read to the end; returns the exit code), and the SFTP family — `stat`, list, streaming read
  and streaming write in 64 KiB chunks (`REMOTE_CHUNK_BYTES`).
- **Backpressure.** Exec output crosses a bounded queue (`EXEC_EVENT_QUEUE`, 64 events); a
  command that floods slows the transport down instead of being buffered in memory.
- **Stdin rides alongside the output.** The request's `stdin` is fed while the output loop keeps
  draining, never ahead of it: a command that answers its input (`cat`, `sh -s`) would otherwise
  fill the window the feed waits on. russh cuts the text to the window; EOF follows the last
  byte, or comes at once when there is no stdin. A send refused because the command already
  exited is not an error — the read side reports how it ended.
- **Leases.** Every operation takes an operation-scoped connection lease on the shared SSH
  client (§tunnels.ssh); the lease travels with the exec future or the SFTP reader, so the
  connection reference lives exactly as long as the work.
- **Withdraw first.** On tunnels stop the transport withdraws before anything else, with no
  drain: a remote exec is a run with its own deadline and cancel, so new calls are refused and
  in-flight ones end through their cancel path.
- **Errors** map onto a transport-neutral `RemoteError`; cancellation keeps its own kind so the
  outcome can be an honest "canceled".
- **The inventory is announced.** The provider calls the seat's `changed()` whenever `list`
  would answer differently, and the seat publishes HOST.REMOTE_SEAT (§host.seats), so the
  Targets page learns an endpoint's state and the provider's presence without asking.

## §remote.utf8 — UTF-8 end to end

- **Windows land on character boundaries.** Wherever a byte window becomes text — the live
  buffer (§host.actions), the history window, the tail ring, `cat` — the start skips
  continuation bytes and the end backs off to the last whole character
  (`swiss_core::utf8::window`); a character split across two reads arrives whole in the next
  one, never as U+FFFD. Only genuinely invalid bytes decode lossily. A window never stalls: when
  `max` cannot hold one character it returns lossily and the cursor still advances. `cat`
  collects the whole file, then decodes once.
- **Remote commands run under a UTF-8 locale.** `remote.exec` exports `LANG=C.UTF-8` and
  `LC_ALL=C.UTF-8` after the login shell starts and before `exec`, so the shell never warns
  about a missing locale; a program without that locale falls back to C and still prints the
  argv's raw UTF-8 bytes. The caller's own `LANG` wins; any caller `LC_*` means the caller is
  managing categories, and no `LC_ALL` is added.
- **The local side is documented, not coded.** Windows PowerShell 5 needs
  `[Console]::OutputEncoding = [Text.Encoding]::UTF8` before reading the output (pwsh 7.4+
  already does); `swiss remote write` passes stdin through byte for byte, so the file must
  already be UTF-8. `swiss remote exec --stdin-file` reads its file itself: a UTF-8 byte-order
  mark is dropped, a UTF-16 file with one (Windows PowerShell 5's `>`) is decoded, anything else
  that is not UTF-8 is refused. The CLI usage and the skill say so.

## §remote.history — The run log and the audit

The coordinator forgets a finished run after 32 more; the build an agent started at 3 a.m. must
still be readable in the morning. The remote plugin registers a `RunHistorySink` at start and
removes it at stop.

- **Module-owned database (plan `docs/plans/module-log-storage.md` D3).** While the plugin is
  started, the module owns `~/.swiss/logs/remote.db` — one SQLite database for the whole
  module (every target, one pool), opened at start (never at composition; a constructed or
  disabled plugin opens no history path) and closed bounded at stop. Durability is FULL: this
  is the audit store. The jsonl tree stays the write-through spill and the crash-recovery
  source — its budgets keep running, and the importer never deletes it.
- **Archive semantics on top of the spill.** Rows survive spill trimming (the db holds the
  archive); reads (page, get, output, content) go to the db when attached and to the legacy
  tree otherwise, so a failed open degrades to yesterday's behavior without a failed start.
  Output is chunked (64 KiB) with byte-cursor reads: a window that would end inside a
  multi-byte character stops before it, `after` is a byte offset in that run's stream (never
  a row offset), `total` is the stream's true length, and a missing middle chunk stops the
  read at the hole — head and tail never join. `finish` acknowledges `Recorded` only after
  the db commit.
- **Cutover import.** The first open after the jsonl tree exists folds it (oldest first, torn
  tail counted in `meta`, never fatal) and marks the marker `legacy_import`; re-runs and
  reopens are idempotent (INSERT OR IGNORE on run id). A content file whose envelope does not
  open under the host's master key stays an error on read and stays on disk — no plaintext
  copy is ever made (the sealed envelope text is stored verbatim).
- **Ongoing crash recovery.** Every open also files spill-only records left between spill
  append and database commit. Clear and retention atomically advance a recovery cutoff that
  never moves backwards: a later, older retention cutoff cannot resurrect cleared or expired
  rows, while newer spill-only runs remain recoverable.
- **Identity on clear.** Clearing the history wipes rows, outputs and content, never the wire
  numbering (ids come from the coordinator) nor internal row identity: the next recorded run
  gets a fresh internal id, so no number is ever reused.
- **Stop order.** Stop withdraws the module, unregisters its actions, removes the sink,
  cancels runs still executing the module's actions BY ACTION IDENTITY (whoever submitted
  them; unrelated runs survive), detaches, then closes the database bounded.

- **Layout.** `~/.swiss/logs/remote/runs.jsonl` holds one line per **finished** run: the run
  view exactly as `/api/runs` showed it plus what was asked — target, argv, cwd, **env keys
  only** (`envKeys`; values may be secrets), a write's content and an exec's stdin as their
  sizes only (`contentBytes`, `stdinBytes`; the index never holds either text). Every other
  text of the input, the label and the error keep their words with a credential literal's value
  masked (`••••••••`, §remote.security) — a refused run is recorded too. Lines and rows filed
  before 2026-10-22 kept argv as typed (a redis password in a `bash -c` word on target test,
  2026-10-21): every open masks the spill's lines (one streaming walk, a tmp + rename rewrite
  only when one holds a literal), and the database's rows are masked once (meta `literal_mask`,
  a version bumped when the detector learns a slot) with `secure_delete` on so the old text is
  overwritten. Output is the program's and is not masked. The full output is teed as it flows
  into `out/<runId>.txt`. Private modes (0700/0600).
- **Output budget.** A file keeps its head up to 16 MiB; past that the line keeps the last
  64 KiB as `tail`, so a capped build log still shows both ends — the error is usually at the
  end.
- **Retention.** Three budgets, oldest first: 30 days, 500 MiB on disk (index plus output
  files), 5000 runs. Checked in O(1) on every finish and every page read from three tracked
  numbers; the full pass (two streaming walks and a tmp + rename rewrite, never the whole file
  in memory) runs only when one is over. Orphaned output files are swept at open.
- **The seven-day promise.** A line younger than 7 days (`AUDIT_WINDOW_MS`) is never dropped by
  a budget. Under byte pressure the window's output files go first, oldest first, and the line
  says so (`outputEvicted: true`; its `tail` stays). Under count pressure the window simply runs
  over. A pass that could free nothing records `hold_until_ms` (when the oldest protected line
  leaves the window) and later reads skip the rescan until then; a new output file over the
  byte budget lifts the hold.
- **What a write wrote, sealed.** A `remote.write`'s content is kept as
  `out/<runId>.content` through the sealed store (a written `.env` must not sit on disk in
  clear), up to 256 KiB cut at a character boundary. The line carries `contentStored`,
  `contentFileBytes`, `contentTruncated`, and `contentEvicted` once the budget took it; an
  older plaintext content file is sealed in place at open. The panel's Show keeps the text in
  memory only.
- **An exec's stdin rides the same slot.** The owner's finding (2026-10-10, and again 2026-10-15):
  a record whose input says only `sh -s` or `bash -s` answers nothing about WHAT ran. An exec's
  stdin is sealed beside the record exactly as a write's content — they share the
  `out/<runId>.content` slot (a write run has no stdin, an exec run no written body), the line
  carries `stdinStored`, `stdinFileBytes`, `stdinTruncated` and `stdinEvicted`, and the row
  line names the stdin's size so a script-carrying row never looks like bare argv. Rows
  recorded before 2026-10-10 keep the size only and say the text was not kept.
- **Run ids survive restarts.** The coordinator persists its next id in `~/.swiss/runs.seq`
  (tmp + rename on every allocation), and the sink's `last_run_id` is the floor — without it a
  restart reused numbers and the log carried two runs under one id.
- **The query.** `GET /api/remote/runs?before=&limit=&target=&since=&until=&actor=` → `{runs,
  active, limits {maxAgeMs, maxTotalBytes, maxRuns, maxOutputBytes, auditWindowMs}, usage,
  nextBefore}`. Newest first, 20 per page by default (at most 100), paged by the older-than
  cursor; `active` are the remote runs the coordinator still holds, less any run the page's
  `runs` already carries — the record commits a moment before the coordinator retires the run,
  and a read in that moment listed it twice — so a page paints from one read. `since` is
  inclusive and `until` exclusive, both on `endedAt`, as epoch ms or ISO-8601; a malformed time
  is a 400, never a silently widened window. Reading walks the index from its end in blocks, so
  a page costs a page.
- **Changes are announced** (REMOTE.HISTORY, a notice, §bus.topics). The record publishes when
  what the query answers moves for a reason a run's own events do not carry (§host.actions): a
  record filed — a late one included, after a sink's acknowledgment bound broke — a Clear that
  removed something from either the spill or the database (they keep their own retention, so an
  empty spill ledger does not mean an empty record), a retention pass (the spill's budget pass,
  the database's hourly sweep) that removed a line or evicted a file, and the open at start,
  whose import and mask pass may rewrite rows. A pass
  that removed nothing publishes nothing: every page read runs the O(1) budget check, and a
  notice that caused a read that caused a notice would never stop.
- **Output reads.** `/output?after=&max=` reads the stored file (128 KiB at most per read) with
  the same cursor contract as the live one; `/content` answers the sealed body - a write's
  content or an exec's stdin, whichever the run kept - or 404 when none was.
- **Actor** — §host.actions: `cli:<user>@<host>`, `mcp:<token label>`, `panel`, `jobs`, or `api`
  (blank or over 128 bytes also falls back). The MCP actor comes from the call source captured
  when the server was built, not a task-local read inside the call (rmcp drives `call_tool` from
  its own task, where the task-local is empty).

## §remote.cli — `swiss remote` and `swiss run`

```
swiss remote endpoints | targets | resolve [name]
swiss remote target add <id> --endpoint <id|unique-label> --root <path> [--caps exec,sync,files]
swiss remote target set <id> [--endpoint …] [--root …] [--caps …] [--label …] | target remove <id>
swiss remote exec [name] [--cwd DIR] [--env NAME=VALUE]… [--stdin-file PATH|-] [--timeout 2h] [--detach] [--] ARGV…
swiss remote sync [name] [--source PATH] [--exclude PATTERN]… [--no-delta] [--verbose]
swiss remote push [name] <file> [--to NAME] [--base REMOTE] [--no-delta] | cat [name] <path> | write [name] <path> | pull [name] <path> [--to LOCAL]
swiss run status <id> | logs <id> [-f] | cancel <id>
swiss run audit [--since 7d|36h|90m|30s|ISO] [--until ISO] [--target t] [--actor a] [--json] [--export DIR]
```

- **The hard rule.** Everything after a bare `--` is argv for the far side, passed through
  untouched. For `exec`, argv also begins at the first command word: `exec dev ls -a` equals
  `exec dev -- ls -a`.
- **Stdin, not quoting.** `--stdin-file PATH` sends a file's text as the command's stdin
  (§remote.actions); `-` reads this process's own stdin. The CLI reads the file itself, so no
  local shell's quoting, redirection (pwsh has no `<`) or pipeline line endings (pwsh adds CRLF
  to text it pipes into a program) touch it. It is a local flag, so it goes before the command
  word. When the command is a shell reading its script from stdin (`sh -s`, a bare `bash`) and
  the text has CRLF line ends, the CLI prints a note — sh keeps each `\r` — and sends the text
  unchanged.
- **A script cut apart is refused.** pwsh ends a quoted argument at its closing quote, so the
  POSIX `'\''` idiom splits `bash -c '… '\''x'\'' …'` into a script that stops early plus stray
  words, and bash fails with "unexpected EOF while looking for matching". Before submitting, the
  CLI finds the first shell in argv (`sh`, `bash`, `dash`, `ash`, `zsh`, `ksh`, by basename;
  a `sudo …` in front is fine), reads its options as sh does, and refuses a `-c` script that has
  stray words after it and either has unbalanced quotes (`shlex`) or stray words starting or
  ending with the idiom's leftover `\`. The refusal prints the argv as received and the two
  ways out: `''` for a quote inside a pwsh `'…'`, or `--stdin-file job.sh -- sh -s`. A whole
  script with real `$0`/`$1` words after it passes.
- **Name resolution.** `--target` wins; otherwise the first word or the binding's
  `defaultTarget` names either a target id or a project action (§remote.project). No name at all
  is an error naming the three ways to give one.
- **Endpoints by label.** `--endpoint` takes a connection id, or one exact display label;
  labels are not identity, so an ambiguous label is refused listing the candidates as `label
  (id)`.
- **Deadline chain.** `--timeout` > project action > target `defaultTimeoutMs` > 2 h, capped at
  24 h.
- **Exit codes.** `exec` streams the live output through the cursor API until the run is
  terminal, then exits with the **remote** exit code (clamped to 1–255 for a failure):
  `swiss remote exec dev -- false` exits 1. A timeout, a cancel or a transport error prints why
  and exits 1. `--detach` prints the run id (and the `swiss run logs <id> -f` to follow it)
  and returns.
- **A fast run prints whole.** Any read that comes back truncated — a run that finished
  before the first poll and was compacted to its last 64 KiB, or a command that wrote past the
  256 KiB live window between two polls — has its gap printed from the stored record, which
  gets every byte at the live cursor's offsets (up to its cap; it is closed before the run
  turns terminal), and the follow resumes where the record ended. Only what the record does
  not hold either (past its cap, no record) is skipped, with a note on stderr. A read that
  found output is followed by the next at once; only an empty one waits. Without this a `cat`
  or a quick build silently lost its head, and a 1 MiB `cat` or `seq 1 140000` a stretch of
  the middle (found live 2026-10-06).
- **Audit.** One line per run — time to the second, actor, target, action, argv quoted by the
  POSIX rules (an exec fed stdin adds `< [2.0K stdin]`), result, duration, bytes (a `*` when
  the output was evicted), `#id`. The last
  7 days by default; `--export DIR` writes `runs.jsonl`, `out/<id>.txt` for every output still
  on disk, and `out/<id>.content` - the sealed body of every run that kept one (an exec's stdin
  script, a write's content). A handover without the script answers only what the argv already
  said - the owner's finding, twice over.

## §remote.project — Project binding, notes and the skill

**`.swiss/remote.json`** is a repository's binding: plain JSON (a repository carries it; it
holds only target ids and paths), strict, `schemaVersion: 1`. Found by walking up from the
working directory like git finds `.git` (at most 32 levels); the first binding wins, and a
sealed file on the way (the user's own `~/.swiss/remote.json`) is skipped, never misread.

```json
{
  "schemaVersion": 1,
  "project": "shop",
  "defaultTarget": "dev",
  "actions": { "build": { "target": "dev", "workspace": "build", "timeoutMs": 7200000 } },
  "sync": { "exclude": ["dist/"] }
}
```

An action names a target (required), a `workspace` (the default cwd) and a `timeoutMs`; a flag
given on the command line overrides either. `sync.exclude` adds to the default excludes.

**Per-target notes** — `~/.swiss/remote-notes/<alias>.md` (under `SWISS_HOME` when set) — are
the skill's memory of a machine: toolchain paths, quirks, the command that builds. They never
hold a credential. The CLI owns them end to end: `swiss remote notes` lists every target
that has one (newest first, with size, age and a first-line preview), `swiss remote notes
<id>` prints the note verbatim, and `--write`/`--append <id>` replace or extend it — body on
stdin or `--file`, LF-normalized, capped at 64 KiB, an empty body refused so a broken pipe
cannot wipe a memory. The verbs are flags, never words, so a target named `write` stays
readable as `swiss remote notes write`. No gateway is involved: the CLI resolves the home
through the same data_path the gateway uses, so no caller guesses where the notes live (an
agent's glob miss once read as "no notes" and the memory went unused).

**The skills.** `src/skill_assets/SKILL.md` (`swiss`: what the gateway is, its boundaries, the
panel and `swiss --help`) and `src/skill_assets/remote/SKILL.md` (`swiss-remote`: the command
reference, the MCP tools, the UTF-8 and audit contracts, the notes). `swiss skill install`
writes the ones the user chooses, `swiss-memory` (§memory.mcp The skill) among them (§host.cli).
`disable-model-invocation: true` is the owner's choice; keep it.

## §remote.mcp — `/mcp/remote`

The builtin `remote` MCP (def type `remote`): five tools — `remote_exec`, `remote_sync`,
`remote_pull`, `remote_cat`, `remote_write` — so a model can drive a target with no shell.

- **Thin and name-based.** swiss-mcp never links swiss-remote. A tool call becomes a run of the
  matching `remote.*` action through the coordinator (owner `remote-mcp`, actor `mcp:<token
  label>`, or `panel` from the panel's MCP console); the result is the outcome text plus a
  trailing exit/status line. An unregistered action (Remote disabled) is an in-band tool error
  naming it, never a protocol fault.
- **Descriptions carry the routing rules.** `target` is an alias from the gateway's table, never
  a host; relative paths resolve under `workspaceRoot`, absolute ones pass as typed; the paths
  sync and pull name are **gateway-side** paths, which a model on another machine must not
  mistake for its own. The alias list is read live, so a target added on the page appears on the
  next `tools/list`.
- `env` is an object passed through verbatim; unknown arguments are refused; a missing
  `timeoutMs` gets the same 2 h the CLI gives. `remote_exec` does not take `stdin` — the owner's
  call (2026-10-06): the field is the action's and the CLI's.

## §remote.panel — The Targets and Runs pages

An empty list explains that Tunnels provides SSH endpoints while Remote targets name workspace
and capabilities. A Tunnels link remains available even with existing endpoints; without any,
the endpoint field explains setup. The default group and Add remain. No link runs commands or
copies configuration.

- **Targets** (`#remote`): library rows (§panel.ui) — the alias; a sub-line saying where
  it runs (endpoint, the workspace root in mono, the capabilities); when it last ran, read from
  the run record's own route; one ⋯ menu. The CLI and the page write the same rows through the
  same routes; the page exists so a target never needs a terminal. The list renders through the
  groups component (§panel.groups) at page density with drag both ways: a row drag reorders, a
  drop into another group moves the row, a group drags by its head. Add and edit happen in a
  sheet. A row's ⋯ menu offers **Run a
  command…** (with `exec`) and **Write a file…** (with `files`): `POST /api/runs` with actor
  `panel` and a 15-minute deadline. A typed command line is sent as `sh -c <line>` to the
  target's own shell rather than through a parser here, so pipes, quotes and redirects mean what
  the operator meant.
- **Runs** (`#remote-runs`): one read of `/api/remote/runs` paints the live runs first, then the
  record, newest first, 20 per page with Load more. An event list (§panel.ui): time with the
  date as the day heading; the kind and its command; who — target and actor, as a column only
  while they vary; a failed run as a red tag saying how; the duration. Identical consecutive
  runs fold into ×N — never a run fed stdin: the same argv may have read a different script.
  The row line names the stdin's size (`sh -s · stdin 2.0 KB`), and the open row unseals the
  script on Show - a row that said only `sh -s` answered nothing about WHAT ran (the owner's
  finding, twice). An open live row follows its output every 1.5 s through
  `/api/runs/{id}/output` — output is a stream, not state, and stays off the bus; a re-read
  never rebuilds an open row. Reads are 128 KiB.
- **The whole command, copyable.** An opened row shows the command exactly — argv quoted word
  by word by the same rule as `quote_posix`, so a pasted copy runs as the same argv — with a
  Copy button.
- **The actor shows the surface only** (`cli`, `mcp`, `panel`, …), never the machine's user or
  host name, and is never translated.
- **Search is the first thing in the head, always visible** (2026-10-12). It searches what the
  recorded runs printed, through `POST /api/history/query` with module `remote`. ASCII letters
  fold and everything else matches as typed. The output is searched, not the command: the
  index keeps no command text, and an `sh -s` script is sealed stdin. Typing searches after a
  300 ms pause, Enter searches at once, and Escape empties the box and brings the list back.
  The results replace the list as its own rows, newest first, with no ×N folding and no pager.
  Each row carries the line its first match is on, with the match marked, read through the
  record-body route. An open result shows the usual body plus its match count. The scan walks
  the record oldest first, so one round asks until it holds 50 more runs, the record ends, or
  32 batches have gone by; **Keep searching** then runs the next round from the cursor. An
  unfinished scan that found nothing says that the newer part of the record is not searched
  yet, never "no matches" (§host.history). A stopped plugin's record is named unavailable; a
  refused scan gives its reason and a **Search again**. The target filter narrows the results
  on the spot, and a re-read leaves both the results and the box alone. Runs still in flight are
  not searched, because their output is not in the record yet.
- Filter by target; Clear sits in the ⋯ menu (a destructive verb gets no standing button). The
  budget line states the limits and that the last 7 days are always traceable; an evicted row
  says its output was cleared and the record kept.
- **Live without a poll.** Neither page reads anything on the panel's 6 s tick (§panel). Each
  subscribes before its first read, then re-reads on every event of its topics, coalesced as a
  notice is (§bus.topics), and after every WELCOME. Runs listens to HOST.RUNS `remote`,
  REMOTE.HISTORY and REMOTE.TARGETS (the target filter's list); Targets listens to the same
  three and HOST.REMOTE_SEAT (the endpoint dots and the Tunnels presence line). A run from any
  surface — CLI, MCP, jobs, the panel — appears the moment it is queued or starts, changes
  state the moment it does, and moves into the record the moment it ends; a Clear, a target
  added from the CLI or an endpoint dropping shows in every open window at once. A history
  notice also drops what the Targets visit cached for targets the newest page missed. Targets'
  `poll()` reads nothing: on the tick it rewrites the last-run column's relative times from what
  the page already holds. A plugin enabled or disabled in another window is not on the bus; the
  pages learn it from their next read.

## §remote.verify — Live verification

The suites prove each publish point and each page's reaction. What only a running gateway and a
real browser can prove is walked on the test instance (§testing.live), never on production,
with Runs open in one window and Targets in another:

1. **No cadence.** Both pages idle for 30 s with every row closed: the network log shows no
   `/api/remote/*` request. The pages read on an event, a WELCOME or a user action, nothing else.
2. **A run from any surface.** Ten concurrent `swiss remote exec <target> --detach -- sleep 8`
   from a terminal: within a second eight rows read running, each moves into the record as it
   ends, no run is ever listed twice, and the target's last-run column on Targets follows. The
   ninth and tenth submissions are refused with 429 — the per-target lane holds eight and
   `remote exec` does not queue behind them — and nothing appears for them on either page: a
   refused run never existed. (`queued` is the jobs lane's overlap=queue-one state, not
   something `remote exec` produces.)
3. **A cancel.** `swiss run cancel <id>` on a running run collapses its row into the record
   at once, grouped with its like (SPEC §remote.panel's Runs grouping).
4. **Another window's writes.** Clear in one window empties the record in the other;
   `swiss remote target add` shows the target on Targets and in the Runs filter without a reload.
5. **The seat.** Disabling Tunnels changes the presence line on Targets at once and enabling it
   restores it; a run against a target whose connection cannot dial turns its endpoint dot red
   as the dial fails.
6. **Reconnect.** Restarting the test instance shows the reconnecting line; after the WELCOME
   both pages match a fresh load.
7. **A hidden tab.** Runs submitted while the tab is hidden cause no request until it is shown,
   then one.

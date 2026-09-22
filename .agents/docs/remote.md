# Remote Plugin: Agent Remote Execution (swiss-remote)

> Remote is the agent's way to reach another machine through the gateway: a small sealed table of **targets** (id + endpoint + workspace root + capabilities), five actions (exec / sync / pull / cat / write) that all run through the shared RunCoordinator, the `/api/remote` management surface, the `swiss remote` / `swiss run` CLI, and five MCP tools under `/mcp/remote`. The crate deliberately links no SSH client — every network touch leases through the host's RemoteTransportRegistry, which the Tunnels plugin serves (docs/34); every run is recorded with who/what/where/exit/output under a UTF-8 end-to-end discipline and a protected seven-day audit window (docs/41).

## Feature Overview

> Naming: the crate is `crates/swiss-remote` (workspace member, Cargo.toml:11); the plugin id is `remote` (src/plugins/remote.rs:46); the routes' owner prefix is `/api/remote` (route_owner), mounted once at boot inside the loopback guard and the plugin boundary (src/server.rs:356-359) and driven by a state slot the plugin lifecycle installs and withdraws (api.rs:22-36) — the same shape the terminal plugin pioneered.

| Functionality | Entry (endpoint/CLI/action) | Panel entry | Notes |
| --- | --- | --- | --- |
| Target table | `GET/POST /api/remote/targets`, `GET/POST/DELETE /api/remote/targets/{id}` | Targets page (`#remote`, order 75) | One row = {id, label, endpoint, workspaceRoot, shell, capabilities, defaultTimeoutMs?, group?} (camelCase on the wire, target.rs:55-79); the parser is STRICT — unknown fields are refused naming the path, and a credential denylist rejects rows that carry secret-shaped keys loudly (target.rs:38-41); a slug id (lowercase/digits/inner dashes), an absolute POSIX workspace root (a guardrail paths are checked against, not a sandbox), capabilities ⊆ {exec, sync, files} with exec always required; the group list rides with the rows so the page paints the docs/20 grouped list from one response (api.rs:349-351) |
| Endpoint validation policy | the shared write path (api.rs:367-408) | — | When a transport provider is SERVING, a target may only name an endpoint it lists (a typo fails at save, not at exec); when none serves, any non-empty id is accepted — the tunnels plugin being off must not lock the target table, and exec says honestly what is missing when it runs (api.rs:31-36) |
| Remote exec | action `remote.exec` | — (Runs pane rows) | Runs through the shared RunCoordinator (owner "remote"): argv is passed as an array (no shell string is ever built client-side), cwd/env/timeout ride the input; output streams into the run's live buffer as it arrives; the outcome keeps a bounded 64 KiB tail plus counters, never the whole log (actions.rs:44-48,350) |
| Remote sync / pull / cat / write | actions `remote.sync`, `remote.pull`, `remote.cat`, `remote.write` | — (Runs pane rows) | Tree upload with excludes (sync), file/folder download (pull), one file printed to stdout (cat), stdin written to a remote file (write); the files-direction actions gate on the target declaring files or sync (actions.rs:850,996); every path resolves under the workspace root via safe_join |
| Durable run record | `GET /api/remote/runs` (+ `/{id}`, `/{id}/output`, `DELETE`) | Runs page (`#remote-runs`, order 76) | One JSONL index line per finished run (target, argv, cwd, env KEYS, exit, duration, bytes) plus the full output in `out/<id>.txt` under `<home>/logs/remote`; budgets: 30 days age / 500 MiB total / 5000 runs / 16 MiB output per run / 64 KiB retained tail (history.rs:58-73), with records younger than AUDIT_WINDOW_MS = 7 days never dropped by a budget (docs/41 A2, history.rs:59-62) |
| Live runs | `GET /api/runs` family (host-owned) | the Runs pane's active section | Live runs are the host's runs, not a second job system — there is no RemoteJobManager; the Run ID IS the remote job id, cancellation is the coordinator's own cancel, live output is the run output endpoint (lib.rs:26-29) |
| The `remote` builtin MCP | `POST /mcp/remote` | — | Five thin tools — remote_exec, remote_sync, remote_pull, remote_cat, remote_write — dispatching the actions BY NAME through the shared run coordinator under run owner "remote-mcp" (docs/34 R7; crates/swiss-mcp/src/adapters/remote.rs) |
| CLI | `swiss remote …`, `swiss run …` | — | A thin, honest client of the HTTP surface: every mutation goes through /api/remote, every execution through /api/runs, live output streams back through the cursor API exactly like a panel page would (src/remote_cli.rs:17-23) |

## The /api/remote Routes

**Endpoint enumeration** (all six route paths, api.rs:90-104; every handler re-checks the state slot because a mounted router must never assume who mounted it):

| Method+Path | Request | Success response | Failure |
| --- | --- | --- | --- |
| GET `/api/remote/endpoints` | — | `{presence, provider, endpoints:[{id,label,state}]}` — the transport's live list plus its presence, so a client can say "start Tunnels" instead of guessing | 503 (plugin not running) |
| GET `/api/remote/targets` | — | `{targets:[…], groups:[name]}` | 503 |
| POST `/api/remote/targets` | one target row | the stored row | 400 (strict parse / endpoint not served / duplicate id); 503 |
| GET `/api/remote/targets/{id}` | — | the row | 404 (`target {id} does not exist`); 503 |
| POST `/api/remote/targets/{id}` | the full row (id immutable — a rename is refused) | the stored row | 400; 404; 503 |
| DELETE `/api/remote/targets/{id}` | — | `{removed: id}` | 404; 503 |
| GET `/api/remote/runs` | q: before/limit/target/since/until/actor | `{runs:[…], active:[…], limits:{maxAgeMs,maxTotalBytes,maxRuns,maxOutputBytes,auditWindowMs}, usage:{bytes,runs}, nextBefore?}` — one page of recorded runs newest-first PLUS the coordinator's queued/running remote.* runs not yet in the record, so the pane paints from one read | 400 (malformed since/until — an audit that silently widened its window would be worse than one that failed); 503 |
| GET `/api/remote/runs/{id}` | — | the recorded row plus `tail` when its output file was capped | 404; 503 |
| GET `/api/remote/runs/{id}/output` | q: after/max | `{runId,cursor,nextCursor,output,total,truncated:false,terminal:true}` — the same reader shape the live route uses, so the panel follows a finished run with the reader it follows a live one with | 404; 503 |
| DELETE `/api/remote/runs` | — | `{ok: true}` (forget the whole record — the pane's Clear) | 503 |

Live counterparts on the host plane: `POST /api/runs` (202 + runId), `GET /api/runs/{id}`, `GET /api/runs/{id}/output?after=&max=` → `{runId,state,cursor,nextCursor,output,truncated,terminal}`, `POST /api/runs/{id}/cancel` (services/api.rs:59-67,218-258). The CLI and the panel stream through exactly this cursor contract.

## The CLI (src/remote_cli.rs)

- **The hard rule (docs/34 SS26)**: everything after a bare `--` is ARGV for the far side, never parsed as a flag — `swiss remote exec build -- make -j8 -- -k` sends `["make","-j8","--","-k"]` untouched. For exec the same cut happens at the first command word: `exec t ls -a` needs no `--` (remote_cli.rs:25-28,77-107).
- **Read commands**: `endpoints`, `targets`, `resolve [name]` (what a name resolves to — target or project action).
- **Target CRUD**: `target add <id> --endpoint <id|unique-name> --root <path> [--caps exec,sync,files]`, `target set <id> […]`, `target remove <id>`; an endpoint given as a unique connection name is resolved to its id before the write (remote_cli.rs:543+).
- **Execution**: `exec [name] [--cwd DIR] [--env NAME=VALUE]… [--timeout 2h] [--detach] [--] ARGV…`, then `run status <id> | run logs <id> [-f] | run cancel <id>` — the run surface.
- **Files**: `sync [name] [--source PATH] [--exclude PATTERN]…`, `push [name] <file> [--to NAME]`, `cat [name] <path>`, `write [name] <path>` (stdin), `pull [name] <path> [--to LOCAL]`.
- **Audit**: `run audit [--since 7d|ISO] [--until ISO] [--target t] [--actor a] [--json] [--export DIR]` — who ran what, where, with what result; the last seven days by default.
- **Timeout precedence**: `--timeout` > project binding > target default > 2h, max 24h (remote_cli.rs:34-39).
- The CLI names itself as the actor: `cli:<os user>@<hostname>` (docs/41 A1).

## Configuration and Storage

- **remote.json** (sealed, `<home>/remote.json`): the target table. Opened ONCE at composition time (not per plugin start) so the targets group scope can register over it and outlive start/stop — the same shape as the tunnel scopes over tunnels.json (docs/34 R8; src/plugins/remote.rs:56-61). A restart installs the SAME Arc; stop only withdraws it from the routes' slot.
- **`.swiss/remote.json`** (per project, plain JSON — a repository carries it, and a binding holds no secrets, only target ids and paths): named actions, a default target, sync excludes, a timeout policy; discovered by walking up from the working directory, it turns `swiss remote exec build -- …` into `exec --target dev --timeout 2h -- …` without the agent retyping anything (project.rs:17-34).
- **The run record**: `<home>/logs/remote/runs.jsonl` (index) + `out/<id>.txt` (output files); the budgets and the protected seven-day window are described above. The record only ever holds finished runs; orphans from a crash mid-run are swept at open (history.rs:231).
- **Plugin wiring**: descriptor id "remote", two pages — remote(75, entry `/admin/js/views/remote.js`) and remote-runs(76, entry `/admin/js/views/remote-runs.js`) — routes `["/api/remote"]`, and deliberately NO `requires:["remote-transport"]`: the target table must stay writable while tunnels is off; the capability probe answers the "remote-transport" question separately so the inventory can say honestly whether remote exec is currently possible (src/plugins/remote.rs:123-149; src/server.rs:317-337). Boot registers the plugin after terminal (server.rs:301-316), so start order puts it first of the peers.

## UTF-8 and the Audit Contract (docs/41)

- **Byte windows land on character boundaries.** Every place a byte window becomes text — the live output window, the history file window, the outcome's tail ring, cat's chunk stream — skips continuation bytes at the start and retreats to the last complete character at the end; a character straddling a window boundary is read on the NEXT poll, not replaced by U+FFFD. Only genuinely invalid bytes go lossy, and the cursor still advances (docs/41 U1; swiss_core::utf8::window, used at actions.rs:92-94).
- **Remote commands run under a UTF-8 locale.** `remote.exec` exports `LANG=C.UTF-8` / `LC_ALL=C.UTF-8` after the login shell but before exec (so the shell itself never warns about a missing locale); a caller's explicit `LANG`/`LC_ALL`/`LC_*` wins word-for-word — nothing is overridden (docs/41 U2; the CLI states the same contract in its help text, remote_cli.rs:394-399).
- **Actor everywhere.** Every run records who asked: `cli:<user>@<host>` (self-declared over loopback), `panel`, `jobs`, `api` (default), `mcp:<token label>` for the MCP tools (runs.rs:264-267) — the audit trail's who column, surfaced by `run audit --actor` and `GET /api/remote/runs?actor=`.
- **Seven days, protected.** Budget eviction is oldest-first (with the run's output file), but records inside AUDIT_WINDOW_MS = 7 days survive every budget — "the last seven days are traceable" is a promise the code keeps, not a hope (history.rs:59-62,509-567; tests pin the byte-budget eviction honoring the window).

## Key Source Files

| File | Role |
| --- | --- |
| crates/swiss-remote/src/lib.rs | RemoteSystem: the sealed target table + the run history, one instance per process (lib.rs:52-79) |
| crates/swiss-remote/src/api.rs | The /api/remote tree and its state slot (api.rs:53-104) |
| crates/swiss-remote/src/actions.rs | The five actions (exec/sync/pull/cat/write) over the resolve-stream-report skeleton, owner "remote" |
| crates/swiss-remote/src/target.rs | RemoteTarget, the strict parser, slug/root validation, the credential denylist |
| crates/swiss-remote/src/project.rs | The .swiss/remote.json per-project binding (docs/34 §24) |
| crates/swiss-remote/src/sync.rs | Tree sync / pull with excludes |
| crates/swiss-remote/src/history.rs | The durable run record: budgets, the seven-day window, paging and output cursors |
| crates/swiss-remote/src/groups.rs | register_remote_scopes: the targets scope over the docs/20 group family (`/api/groups/targets`) |
| src/plugins/remote.rs | The plugin factory: descriptor, lifecycle, the /mcp/remote builtin mount (R7) |
| src/remote_cli.rs | `swiss remote` / `swiss run` (docs/34 SS26 + docs/41 A3) |
| crates/swiss-mcp/src/adapters/remote.rs | The five MCP tools dispatching the actions by name |
| Panel | panel/src/views/{remote.ts,remote-runs.ts} (targets page / runs page) |

## Relationship with Other Docs

| Doc | Relationship |
| --- | --- |
| docs/34 (agent remote execution spec) | The total contract this plugin implements (R1–R8, SS26 CLI, §17 output cursors) |
| docs/41 (UTF-8 + audit spec) | The UTF-8 discipline, the actor column, the seven-day window and the audit query surface |
| docs/09 (plugin architecture) | The public-contract plugin shape this plugin follows (like terminal before it) |
| docs/20 (groups) | The targets list as the family's seventh scope |
| [host.md](host.md) | The RunCoordinator, /api/runs, actor, and the remote-transport capability seat |
| [tunnels.md](tunnels.md) | The transport provider that serves the RemoteTransportRegistry |

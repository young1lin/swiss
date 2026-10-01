# swiss — Specification

This is the one specification of swiss. It describes the system **as it is now**, organised
by area. There are no numbered spec files, no hand-off prompts and no separate decision log:
a change to behaviour amends the section it touches here, in the same commit as the code.

## §about — How to read and amend this document

**Anchors.** Every section heading carries its anchor literally: `## §host — …`,
`### §host.vault — …`. Code comments, tests and skills cite a rule as `SPEC §host.vault`;
a decision is cited by its number, `ADR-018`, and lives in §decisions. An anchor is a
contract: renaming one means rewriting every citation in the same commit (`git grep 'SPEC §'`).

**What wins.** The code wins over this document when they disagree — a disagreement is a bug
in whichever side is wrong, and fixing it means amending one of them, never leaving both.
Within the document, a later amendment replaces the text it contradicts; the old wording is
deleted, not struck through. History lives in git.

**Normative words.** *Must*, *never* and *always* are rules a test or a gate enforces; a rule
without an enforcing test is a gap to close, not a suggestion. Numbers in *records* (memory
readings, sizes, timings) are measurements with a date, never gates, unless the text says
gate.

**Amending.** Use the `/swiss-spec` skill: interview → amend the relevant section in place →
acceptance tests → delivery order. A decision that is hard to reverse, surprising without its
context and a real trade-off gets an `ADR-NNN` entry in §decisions.

**Language.** English, like every code comment. The panel's user-facing copy is localised
(§panel.i18n); this document is not.

**Contents.**

| Anchor | Area |
|---|---|
| §product | What swiss is, the four properties, the memory record |
| §arch | Crates and edges, runtime model, dependency policy, Rust rules, declared exceptions |
| §formats | The data directory, the sealed envelope, logs, the wire rules |
| §host | Plugin host, lifecycle, routes, security boundary, session, config, vault, groups, CLI, daemon |
| §mcp | The MCP plugin: registry, adapters, endpoint, OAuth, revisions, call log |
| §data | The Data plugin: connections, browsers, SQL console, Redis, tabs |
| §tunnels | SSH connections, forward rules, proxy and jump hosts |
| §jobs | Config-driven scheduled jobs |
| §terminal | Local and remote shells over WebSocket |
| §remote | Remote execution: targets, exec/sync/pull, runs |
| §process | The process plugin and the supervisor |
| §panel | The admin panel: navigation, design system, UI library, TypeScript, i18n |
| §security | The security model in one place |
| §testing | The gates, the suites, the integration harness, live verification |
| §release | CI, release artefacts, open-source hygiene |
| §decisions | ADR-001 … the decision log |

## §product — What swiss is

swiss is a developer's pocket multitool: **one local process that holds every small tool a
developer reaches for while writing code** — MCP servers, database browsing, SSH tunnels,
scheduled jobs, terminals, remote execution, and whatever the next one turns out to be —
behind one loopback port (`127.0.0.1:19999` by default) and one panel. It ships as one
self-contained binary.

Memory is why it is Rust: the same program's practical floor on Node was 45–60 MB, and the
Node build it replaced is retired (ADR-016). This repository owns every layer, the panel
included. When a piece of behaviour looks odd, the module's comment
usually records the bug that was paid for once already.

### §product.properties — The four properties

A change that trades any of these away for convenience is the wrong change, however much
shorter it makes the diff.

- **Ruthlessly small.** Memory is the product, not an optimisation pass. Idle cost is the number
  that matters; a tool nobody is using right now should cost close to nothing.
- **Plugin-shaped.** Every capability is a plugin with its own descriptor, routes, pages and
  resources. The host keeps only cross-cutting mechanism — the security boundary, config,
  registration, lifecycle, run accounting. No business logic climbs back up into it (§host.plugins).
- **Hot-pluggable.** Enabled, running and visible are three different states, switchable at
  runtime. Disabling a plugin really releases what it held — tasks, connections, children,
  routes — and nothing else in the process notices.
- **Three ways to bring a tool in.** A stdio child process, an HTTP endpoint, or Rust compiled
  into the binary. The first two cost a process or a socket and are how foreign tools arrive;
  the third costs almost nothing and is how the tools worth keeping end up shipping.

MCP is the most important plugin, not the trunk everything hangs off. Data, Tunnels, Jobs,
Terminal, Remote and Process are its peers, and a new tool reaches the panel by contributing a
descriptor, an action and a page — never by editing a match arm in the host.

### §product.memory — The memory record

Memory numbers are **records, not gates**: no band is enforced, and a debug build costing more
is fine. What stays load-bearing is the direction — every megabyte of idle cost has to be
argued for, and a change that moves the number the wrong way is worth stopping for.

The instrument is the gateway's own `GET /api/memory` (`gatewayMb`, the process tree's
`childrenMb`, `processCount`). Measure a change by running it on 19998 against the same data
and traffic as a baseline, and add a row here; the `swiss-memory-record` skill does the
measuring.

| Date | Build | Workload | Working set | Note |
|---|---|---|---|---|
| 2026-09-07 | Node | mysql, pg, 2×redis, 2×http, echo; 2×proc asleep | 117.5 MB | the baseline |
| 2026-09-07 | Rust spike | echo only | 9.1 MB | axum + rmcp stateless, 1.05 MB exe |
| 2026-09-10 | Rust | echo + panel | 14.0 MB | whole gateway present, scratch home |
| 2026-09-10 | Rust + terminal | plugin loaded, zero sessions | 13.7 MB | flat; +233 KB per idle local session |
| 2026-09-11 | Rust | mysql, pg, 2×redis over its own tunnels | 21.8 MB | 40/40 calls ok, private 8.2 MB |
| 2026-09-11 | Rust | full workload, 60 calls, 9 tunnel rules | **22.4 MB** | private **8.6 MB**, 6 threads |
| 2026-09-11 | Node | same set, same data dir, same traffic | 113.8 MB | private 124.9 MB, 13 threads |
| 2026-09-13 | Rust | 100k-row pg table exported | peak 32.5 MB | `format=sql` streams; csv materialises (§data.export) |
| 2026-09-15 | Rust + MCP OAuth | full workload, a grant stored | 19.9 MB | the OAuth module costs nothing idle |
| 2026-09-15 | Rust + native zai-vision | full workload | 21.8 MB | the retired Node child measured 66.6 MB alive (ADR-022) |
| 2026-09-22 | Rust | real-state snapshot, nothing awake | 27.0 MB | cold boot, zero traffic; private 14.0 MB |

Private bytes are the honest "owned memory" figure; the working set carries paged-in image
sections. The regression guard inside the test suite is a **delta**, not a ceiling
(§testing.memory).

### §product.nongoals — What this does not buy

1. **`proc` MCPs are untouched.** An `npx`/`uvx` child is 50–150 MB of Node or Python, and
   nothing here changes that. The saving holds only while children are asleep, which is why a
   `proc` MCP is lazy by default (§mcp.proc).
2. **Nothing gets faster.** The gateway is idle almost all the time; latency is the databases'
   and children's. Do not sell this as a performance tool.

Two things come free and are real: no PowerShell spawns (the process-tree walk and DPAPI are
direct Win32 calls, so the memory view is always current), and no `npx` wrapper holding
`cmd.exe` + npx resident for the life of the process.

### §product.tactics — Where the memory goes, and the levers

In descending order of payoff:

1. **No `serde_json::Value` on a forwarding path.** The proxying adapters parse only the
   envelope (`&RawValue` for `params`/`id`) and pass the rest through untouched. A body at the
   2 MiB limit then costs one buffer instead of hundreds of thousands of allocations
   (§arch.rules).
2. **`current_thread` runtime** (ADR-003).
3. **Pools are the per-connection cost.** Database pools are built lazily and kept tiny
   (`min_connections(0)`, a small max, idle timeout); what must stay lazy is pool
   construction, not code loading — code that never executes is never paged in.
4. **The release profile.** `opt-level = "z"`, fat LTO, one codegen unit, `panic = "abort"`,
   stripped symbols. The text segment is resident memory; `panic = "abort"` is also why
   `.unwrap()` on fallible paths is banned.
5. **Allocator.** Windows' system allocator is fine. On glibc, measure `MALLOC_ARENA_MAX=2` or
   mimalloc before choosing.
6. **On-disk logs** (ADR-005, §formats.logs).
7. **Small strings in long-lived structures** (`Arc<str>`/`Box<str>`), last.

## §arch — Architecture

### §arch.crates — Crates and edges

A cargo workspace that ships one static binary (ADR-010). The dependency edges are the
architecture, and the compiler enforces them:

```
swiss-core  ←  swiss-host  ←  { swiss-mcp, swiss-data, swiss-tunnels, swiss-jobs,
                                swiss-terminal, swiss-remote, swiss-panel }  ←  swiss
```

| Crate | Owns |
|---|---|
| `swiss-core` | Knows nothing about gateways: paths, the JSON-line logger, atomic writes, the sealed store (`secure/`: envelope, key, statefile, envstore, secretstore, refs), UTF-8 windows, platform calls (`platform/`: process tree, job objects, DPAPI, private files, the PTY seam — ConPTY and openpty) |
| `swiss-host` | The mechanism every subsystem shares: the plugin host (`host/`), the runtime services (`services/`: actions, runs, the process supervisor, the connection catalog, the shell and remote capability seats), config and the config store, `managed.json`, tokens, auth, the loopback guard, masking, memory, groups, the DB-browser contract (`dbbrowser.rs`) |
| `swiss-mcp` | The MCP plugin: registry, adapters, the call log, traffic, paging, import, OAuth |
| `swiss-data` | The Data plugin's routes over the connection catalog (`dbbrowser_api.rs`) |
| `swiss-tunnels` | SSH connections, forward rules, proxy/jump dialing, the SSH capability providers |
| `swiss-jobs` | Config-driven jobs: definitions, schedule, clock, state, runner, run log |
| `swiss-terminal` | The terminal session machine: config, sessions, tickets, recording, local shells. No axum and no SSH — its routes live in the root crate's `plugins/` |
| `swiss-remote` | Remote execution: targets, actions, sync, project config, history, the `/api/remote` routes |
| `swiss-panel` | The admin panel: the TypeScript source (`panel/`), the committed emit (`src/admin_assets/`), the embedded asset server (`admin.rs`) |
| `swiss` (root `src/`) | Composition and nothing else: argv, the axum app, `/api/*` assembly, plugin descriptors, boot, the daemon and CLI, the admin session, autostart, the update check, skill install |
| `swiss-it` | The dev-only integration harness (§testing.it). A leaf nothing depends on; every dependency hides behind its `it` feature, so without it the crate compiles to empty targets |

**No subsystem crate depends on another.** Data reaches its connections through the connection
catalog (`swiss-host/src/services/catalog.rs`), not through MCP. If a change seems to need an edge between two subsystem crates, the host contract is
missing something — add it there instead. The same holds for terminal ↔ tunnels (ADR-011): SSH
shells arrive through a capability seat in the host.

`src/` is deliberately the smallest crate that could hold what is left: composition names every
other crate, so nothing else has to.

**`--workspace` is not optional.** Without it cargo selects the root package alone — a small
minority of the suite — and the member crates, where most tests live, are never built. The run
still reports ok. Every gate command carries `--workspace`; the same holds for clippy.

### §arch.runtime — Runtime model

`#[tokio::main(flavor = "current_thread")]` (ADR-003). `current_thread` does not permit `Rc`:
`axum::serve` spawns connections with `tokio::spawn`, which requires `Send` futures. Shared
state is `Arc<RwLock<…>>`, uncontended on one thread. Blocking work (the ConPTY pump, file walks
that can block) goes to `spawn_blocking`. Do not reach for `multi_thread` without a measured
reason in the commit message.

### §arch.deps — Dependency policy

- **Every dependency justifies its weight.** `default-features = false` first; add back what is
  needed. A crate that pulls a second TLS stack, a second async runtime or its own thread pool
  is a bug, not a dependency. Run the `swiss-dependency-review` skill on any manifest change.
- **One TLS stack per platform.** reqwest speaks schannel (`native-tls`) on Windows and rustls
  elsewhere; sqlx carries no TLS; russh uses `ring`. `cargo tree -d -e normal,build` gates the
  shipping graph (the CI step fails on a second tokio, TLS stack or hyper); dev edges — the
  integration harness's testcontainers among them — never ship and may carry their own copies.
- **The main crates and why:** `rmcp` 3.x (server + client, streamable HTTP server, child
  process transport; `macros` off — servers implement `ServerHandler` by hand), `axum` 0.8
  (http1, json, query, tokio, ws), `tokio` (`rt`, never `rt-multi-thread`), `sqlx` 0.9 (runtime
  `query()` only — never the `query!` macros, which need a live database at compile time),
  `redis` (redis-rs; `fred` is heavier and nothing needs it), `reqwest`, `russh` + `russh-sftp`,
  `serde_json` with `raw_value` and `preserve_order`, the RustCrypto AEAD/KDF crates (ADR-013),
  `rust-embed` for the panel, `windows` for Win32 FFI, `encoding_rs` for GBK console output,
  `shlex` for POSIX shell word splitting (the data console), `time` and `chrono` (default features off).
- **Deliberately not taken:** `tracing`, `regex` (ADR-007); `once_cell`/`lazy_static` (std has
  `OnceLock`/`LazyLock`); `tower-http` (assets are embedded and served by hand; no compression or
  CORS layer); `openssl`; `portable-pty` (ADR-011); a `tempfile` crate in tests (scratch homes are
  `temp_dir()` + random hex).
- **Mature libraries over hand-rolled grammars.** ADR-007's scanners are three trivial patterns.
  Anything with a real grammar — shell quoting, JSON, SQL literals the engine will parse,
  protocol framing — uses a maintained crate (or a vendored, pinned copy for the panel,
  §panel.vendor), never a parser written here.
- **Manifests and the lock move together.** A commit that edits a `Cargo.toml` carries the
  regenerated `Cargo.lock`; self-check with `cargo tree --locked --offline --workspace
  --depth 0`. Every CI command is `--locked`.
- **Build speed.** The workspace links with `rust-lld` (`.cargo/config.toml`; measured −28% on a
  cold `cargo test --workspace`). `[profile.dev]` keeps line tables only (full debuginfo across
  the link targets exhausts a small paging file, os error 1455). The release profile is
  deliberately the slowest thing here; incremental release and a thin-LTO fast lane were
  measured and rejected (the floor is dependency codegen at `opt-level = "z"`).

### §arch.rules — Rust rules

- **No `serde_json::Value` on a forwarding path.** `proc`, `http` and `rest` pass payloads through
  as `&RawValue`, parsing only the envelope fields they route on. `tests/memory.rs` pins it.
- **No subprocess where a syscall exists.** The process-tree walk (Toolhelp32), DPAPI, the
  memory reading and the job objects are direct Win32 calls. A new `Command::new("powershell")`
  needs a very good excuse.
- **No `unsafe`** outside the FFI at the platform boundary, and never to make a memory number
  look better.
- **No `.unwrap()`** on anything that touches config, the network, a database or the
  filesystem. One failing MCP must never take down the others; with `panic = "abort"` a panic
  kills the whole process.
- **Accepted but not executed is a lie.** A config field whose semantics this build does not
  implement is refused — a 400 naming what is missing — never parsed and silently ignored.
- **Comments are English**, including in doc samples, and cite this document as `SPEC §…`.
- **The tree is hand-formatted.** Match the surrounding style by hand and wrap comments near
  100 columns. Do not run `cargo fmt` or `rustfmt` — it reflows lines the change never touched;
  clippy and the tests are the gates, and CI has no fmt check.
- **LF everywhere.** `.gitattributes` enforces `* text=auto eol=lf`; binary types are marked
  binary.

### §arch.exceptions — Declared exceptions

Each line below departs from a rule in this document on purpose. An exception lives here and in
a comment at its site; anything that departs from a rule and is not listed is a bug.

| Exception | Why it stays | Specified in |
|---|---|---|
| A bearer token can be read back in plaintext | Two credential classes, two threat models; reading it is an explicit act | §security.threats |
| `swiss open` runs `cmd /c start` (Windows) / `xdg-open` | The platform opener has no syscall equivalent; `CREATE_NO_WINDOW` and null streams keep the cost at the floor | §host.cli |
| Local terminal sessions are uncapped | Owner decision 2026-09-12: a local PTY spends the operator's own machine; remote caps stay | §terminal.local |
| One blocking thread per local terminal session | ConPTY's anonymous pipes cannot be polled | §terminal.local |
| A recording that cannot be written warns once and the session goes on | Losing a terminal to a recording failure is the worse outcome | §terminal.recording |
| `process.legacy-command` resolves `${VAR}` leniently beside strict `process.exec` | The pre-v2 command strings keep their historical meaning; new capabilities take the strict contract | §process.actions |
| Tunnels coerce numbers like JavaScript's `Number()` | `tunnels.json` and the API stay byte-compatible with the Node build; Jobs is the strict native model | §tunnels, §formats |
| `new_id` calls `.expect()` on the OS random source | A CSPRNG failure means the machine is broken; the one declared exemption from "no unwrap" | §tunnels |
| `with_explain` exists in the server and in the panel | The panel's copy runs; the server's copy is the tested contract (prefix once, strip the terminator) | §data.console |
| `retention.maxHistoryBytes` is accepted and stored but not enforced | The cross-file sweep is not built; the field and its gap are declared, not silent (the one exception to "accepted but not executed is a lie"). Owner decision pending: build the sweep or mark the field advisory | §jobs.config |
| The Jobs view probes `/api/jobs` when there is no plugin inventory | Without it an older gateway loses the whole Jobs tab; inventory first, probe as fallback | §jobs.panel |
| Run now polls for at most 30 minutes | A UI bound; the job's own timeout is the real one | §jobs.panel |
| The sidebar mechanism renders the MCP list whatever page asks for it | Only the MCPs page opens a sidebar; a fix needs pages to declare their own sidebar content | §panel.nav |
| `/api/mcps/{name}/{kind}` is one wide route with a kind whitelist | A literal translation of the Node route shape | §mcp.admin |
| `state.tun.keys` is cached for the page's life | A low-frequency, static directory, unlike the polled data | §tunnels.panel |
| Stop-all's 409 reuses the Dependents shape | The same destructive-action funnel: "users to stop" instead of "users to delete" | §tunnels.api |
| A resident `proc` MCP child is not a run | Lifecycles stay separate; the platform mechanics are shared | §process.supervisor |
| The process plugin registers last | Boot starts the list in reverse, so the capability is online first | §process |
| The memory reading's heap field is private commit, `externalMb` 0 | An honest approximation of a JavaScript-shaped field | §host.memory |

## §formats — On-disk formats and wire rules

### §formats.home — The data directory

`~/.swiss`, or `$SWISS_HOME` when set (`swiss_core::paths::data_dir`, read fresh on every call
so a test's override is honoured). The binary can run from any directory; a fixed home is what
makes the token and the state global. The pre-rename home (`~/.mcp-gateway`) and its
`MCP_GATEWAY_*` variables are never read; a straggler directory is moved by hand (sealed files are path-independent). A leftover `tokenEnv:
"MCP_GATEWAY_TOKEN"` in an old config is inert data, not a fallback.

```
~/.swiss/
  gateway.config.json    sealed   server layout, port, host, tokenEnv, the plugins' config rows
  managed.json           sealed   user-added MCPs, overrides, toggles, enabled flags, tokens, groups
  tunnels.json           sealed   SSH connections + forward rules + groups
  env.json               sealed   the env store (overlaid onto every child's environment)
  secrets.json           sealed   the secret vault, {rev, secrets} (§host.vault)
  mcp-oauth.json         sealed   HTTP MCP OAuth grants (§mcp.oauth)
  remote.json            sealed   remote targets (§remote)
  jobs-state.json        sealed   job run state: last/next run, streaks (§jobs)
  session.json           sealed   the admin-session signing key + the CLI key (§host.session)
  master.key             opaque   the DPAPI-protected master key (Windows only)
  jobs.json                       legacy v1 job file, migrated into the jobs config row once
  runs.seq                        the run-id sequence, so ids never repeat across restarts
  gateway-<port>.pid              daemon pid file, port-scoped
  gateway-<port>.log              daemon log, port-scoped
  .proc-pids-<port>.json          ledger of spawned proc-MCP pids, port-scoped
  logs/
    traffic.jsonl                 the MCP traffic tail (§formats.logs)
    calls/<mcp>.jsonl             one line per tool call
    calls/bodies/<mcp>/<seq>.txt  the full reply, when it exceeded the preview
    jobs/<job>.jsonl              one line per job run (§jobs)
    remote/runs.jsonl             one line per finished remote run (§remote.history)
    remote/out/<runId>.txt        that run's output stream; `.content` is a sealed write body
  terminal/<sessionId>.cast       asciicast v2 recordings, output only (§terminal)
```

**Port-scoping is load-bearing.** Two gateways on different ports share one home (19999 and a
19998 test instance do, every day); a shared pid ledger once let one instance's reap kill the
other's live children. Keep every per-instance file keyed by port.

**Private permissions.** On unix the directory is created 0700 and every state file 0600, at
first run and on every write. On Windows the call is a no-op — the profile directory's
inherited ACL is the boundary, and the sealing is what protects the contents. A plaintext `.env` from an old install is
folded into `env.json` verify-then-delete: the sealed store is read back and must hold every
legacy key with its exact value before the plaintext file goes.

**Frozen names.** The state file names, the envelope's `"lmg"` marker, the `lmg-state-v1` HKDF
info and the `local-mcp-gateway/v1` DPAPI entropy predate the rename and are wire literals, not
the product name. A rename never touches them; a test pins each.

### §formats.sealed — The sealed envelope (frozen)

Every state file is AES-256-GCM under the machine-bound master key. The only way state reaches
disk is `swiss_core::secure::statefile`.

```json
{ "lmg": 1, "alg": "aes-256-gcm", "keySource": "dpapi",
  "salt": "<base64, 16 bytes>", "iv": "<base64, 12 bytes>", "tag": "<base64, 16 bytes>",
  "ct": "<base64>" }
```

| Step | Value |
|---|---|
| Per-file key | `HKDF-SHA256(ikm = master, salt = salt, info = "lmg-state-v1", len = 32)` — a fresh random salt per write |
| Cipher | AES-256-GCM, 12-byte random IV, no AAD |
| Plaintext | the payload as pretty JSON (2-space indent), UTF-8 |
| Encoding | every binary field base64 (standard alphabet) |
| File | the envelope as pretty JSON, written atomically (tmp + rename) |

- **`keySource` is a diagnostic, never trusted.** Opening tries every candidate key the machine
  can produce and lets the GCM tag decide — a wrong key, a tampered byte and a truncated file
  all fail the tag, which is how "not sealed on this machine" is detected.
- **Legacy plaintext is accepted on read.** A file that is not an envelope is read as JSON and
  re-sealed at once; with no key obtainable at all it is served read-only rather than failing
  the boot. That is how a hand-authored `gateway.config.json` works and how a user can edit
  config in an emergency (`swiss export` / `swiss import` is the sanctioned route).
- The HKDF argument order is the classic trap: `ikm` is the master key, `salt` the per-file
  salt. Getting it silently wrong means no existing file opens. `tests/envelope_compat.rs`
  opens `tests/fixtures/node-sealed.json`, sealed by the retired Node build under a fixed key
  — the test that catches an argument-order slip, a base64 mistake or a wrong info string.

### §formats.masterkey — The master key

Resolved at most once per process and cached (`swiss_core::secure::key`). Candidates,
strongest first; the machine-id source is always last, so a file whose stronger source
vanished still opens.

| Platform | Source |
|---|---|
| override | `SWISS_MASTER_KEY`, 64 hex chars — wins over everything; CI, containers, recovery and every test use it, so the suite never spawns an OS helper |
| Windows | DPAPI (CurrentUser): a random key generated once, protected with the entropy `local-mcp-gateway/v1`, stored as `master.key` (direct Win32 calls, no PowerShell). Opaque to any other machine or user — the anti-copy property |
| Linux | `/etc/machine-id` (or `/var/lib/dbus/machine-id`), hashed as `sha256("local-mcp-gateway/v1" + NUL + id)`. World-readable, so it binds to the machine, not the user |
| macOS | not implemented: no machine-id file and no Keychain source yet, so a save fails with "no master key available" unless `SWISS_MASTER_KEY` is set. The login Keychain is the tracked future work |

### §formats.groups — Group keys

Every grouped list carries its model as **additive** keys beside the data (ADR-015,
§host.groups). A file that predates groups names none of them and reads as the single
`default` group with every member unassigned, so old sealed fixtures keep passing unchanged.

| File | Keys |
|---|---|
| `managed.json` | `groups` + `mcpGroups`; `tokenGroups` + `tokenMembers` (keyed by token id) |
| `tunnels.json` | `connGroups` + `ruleGroups`; `tunnelGroupsV2: true` marks the list as literal (a deleted `default` stays deleted) |
| `secrets.json` | `groups` + `secretGroups` + the row order (§host.vault) — a group label is a folder name, never a credential |
| `gateway.config.json` | the jobs row's `groups` + each definition's sparse `group` (absent renders in the first group) |

### §formats.logs — Log files

Append-only JSONL, read by seeking from the **end**. Budget-trimmed by copying the newest bytes
to a temp file and renaming. A torn last line (a kill mid-append) is skipped, never fatal.

**`logs/calls/<mcp>.jsonl`** — one `CallEntry` per line: `seq, at, tool, via, client?, ok, ms,
args, output, chars, preview?, body?, bodyGone?`. The constants are part of the format:

| Constant | Value | Meaning |
|---|---|---|
| `ARGS_MAX` | 4 KB | arguments clipped inline (secret-looking keys masked) |
| `PREVIEW_MAX` | 2 KB | reply head held inline, so a page needs no extra reads |
| `BODY_MAX` | 1 MB | ceiling on a stored reply |
| `BODY_KEEP` | 50 | full replies retained per MCP |
| `MAX_BYTES` / `KEEP_BYTES` | 2 MB / 1 MB | index budget and what a trim keeps |

`bodyGone` stays **absent rather than false** while the payload is readable — the panel
distinguishes the two and a test pins it. Body pruning works by **listing the directory**,
never by arithmetic on `seq`: replies that fit the preview are never written, so the files are
sparse.

**`logs/traffic.jsonl`** — one `TrafficEntry` per line (token, client name/version, method,
redacted params ≤ 512 B, body and response ≤ 8 KB each). Reads are served from an in-memory
ring of the newest 500; the file is the recovery tail restored at boot, budget 2 MB / 1 MB
(ADR-005).

The job run log, the remote run history and the terminal recordings have their own budgets,
specified with their plugins (§jobs.runlog, §remote.history, §terminal.recording).

### §formats.wire — HTTP wire rules

Every `/api/*` response is a contract with the panel and with scripts that call it.

- **camelCase everywhere** (`#[serde(rename_all = "camelCase")]`).
- **Absent is not null.** The panel tests `undefined` in places (`bodyGone`, `childrenMb`,
  `measuredAt`): optional fields use `skip_serializing_if = "Option::is_none"`.
- **Numbers keep their shape.** `gatewayMb: 117.5` is a float with one decimal.
- **Error shapes differ by route on purpose.** MCP endpoints answer JSON-RPC
  (`{jsonrpc, error: {code: -32603, message}, id: null}`); admin routes answer
  `{error: "..."}`. Each refusal returns its route's own shape; do not unify them behind one
  axum error type.
- **Revisions.** A mutation of shared config names the `rev` it was planned against and fails
  with 409 on a mismatch, so two panels never silently overwrite each other (§host.config).

The golden capture of every `/api/*` response (asserting the shapes above wholesale) is still
open; today the shapes are pinned by the per-route tests.

### §formats.protocol — MCP protocol eras

One endpoint serves two protocol eras and must keep doing so:

- **2026-07-28** — the modern path, including the `server/discover` probe clients send before
  falling back to `initialize`. Stateless.
- **2025-era** — `initialize` and every later request from a client on the old handshake, over
  the stateless legacy fallback.

In rmcp this is `StreamableHttpService` with `legacy_session_mode(false)`. There is
deliberately **no GET handler**: on 2026-07-28 notifications ride the client's
`subscriptions/listen` POST stream, so a GET has nothing to open and falls through to 404.
`DELETE` answers 204 though there is no session, so a client that sends it closes cleanly.

## §host — The host

The host is what every plugin shares and nothing more: the security boundary, config and
credentials, registration, lifecycle and run accounting. `swiss-core` holds the platform
primitives, `swiss-host` the mechanism, the root crate the composition and the CLI.

**The host keeps cross-cutting mechanism only.** MCP tool selection, SQL browsing, SSH rule
logic, cron parsing and any plugin's cache or front-end state never live here. Replacing the
old big `AppContext` with a giant `Services` object would be the same mistake renamed.

### §host.plugins — The plugin contract

Every plugin contributes six things and owns one scope:

1. **Descriptor** — id, kind, label, version, config schema version, default-enabled,
   `requires` (capability names), `restart_on_config_change`.
2. **Config** — a typed schema with defaults, validation and migration; the schema is data the
   panel can render. The backend is the authority; the panel is not a validator.
3. **Services / actions** — capabilities registered by stable name, scoped to the plugin.
4. **Routes** — its own API prefixes, always inside the host's boundary (§host.routes).
5. **Pages** — zero or more page contributions (§panel.nav): id, label, order, path, entry,
   sidebar, layout. A page is not a backend module boundary.
6. **Lifecycle** — create / start / apply_config / stop, plus health, last error and resource
   counts.

`PluginScope` owns the resources: supervised task handles, the cancel signal, registration
tokens, connection leases, child-process ownership, caches. Many tasks per plugin are fine;
losing a task's owner is not. The descriptor is resident; the instance is not — a disabled
plugin keeps nothing expensive. Public contracts are typed traits and DTOs; `serde_json::Value`
appears only at the config and admin boundary.

**Registration.** `src/builtin.rs` is the one composition table (the built-ins, then terminal
and remote from `src/plugins/`). Adding a plugin is one factory plus one register line — no
match arm in the host, no edit under `crates/swiss-host/src/host/`. Duplicate ids, route
prefixes, action names and page ids are refused loudly at registration; a silent shadow route
is a boundary hole.

**Plugins that need plugins** are expressed as data, never as crate edges:

- `requires` names a capability; the host's capability probe answers met/unmet and the
  inventory carries `requires` + `requiresMet`. A route of a plugin whose requirement is unmet
  gets a refusal naming the dependency. There is no blocking `WaitingDependency` state.
- **Capability seats** (§host.seats) — a provider registers, a consumer leases. Seats live in
  `RuntimeServices` and outlive both plugin instances.
- Read-only bridges between subsystems (tunnels seeing the registry's MCP names) live in the
  composition crate behind read-only traits (`src/mcp_link.rs`).

**The built-ins and their relations:** `mcp` (registry, catalog provider), `tunnels` (shell
and remote-transport provider), `data` (pure consumer, `requires: ["connection-catalog"]`),
`jobs` (a producer on the shared run pool, owner `jobs`), `process` (the capability plugin:
`process.exec`, no page), `terminal` and `remote` (the public-contract plugins; remote
deliberately declares no `requires`, so its target table stays editable while tunnels is off).

**The acceptance test for the contract:** a new tool is added without touching host business
logic, the jobs scheduler or the panel shell. Disabling it makes jobs that reference its
action show dependency-unavailable while everything else keeps working. Loading third-party
code at runtime (DLL, WASM, a Node plugin host) is out of scope (ADR-001); a new backend
implementation needs a rebuild.

### §host.lifecycle — States, start, stop, config changes

States: `Disabled` / `Idle` / `Starting` / `Active` / `Stopping` / `Failed`
(`host/descriptor.rs`). Enabled, running and visible are three different things.

- **Single-flight.** One tokio mutex per plugin serializes lifecycle operations; a concurrent
  second start queues, re-reads the world and joins the first result.
- **Start** = `validate_config_for_start` → create → `scope.start`, under `START_TIMEOUT` 30 s.
  Timeout or failure shuts the scope down and records `Failed` + `lastError`. **One failing
  plugin is a status row, never a failed boot**; only the host's own security or config being
  unusable fails the boot.
- **Stop really releases.** Leave the route slot first (in-flight requests now see
  not-serving) → stop new triggers → `instance.stop()` under `STOP_TIMEOUT` 10 s → drain or
  cancel in-flight work → reap children and join readers → flush bounded logs → unregister
  contributions and release leases → `scope.shutdown()` (cancel, 5 s grace per task, abort) →
  drop the instance. Consumers stop before providers. Routes never capture an expensive
  instance behind an `enabled` bool: paths stay mounted and dispatch asks live state.
- **The honesty rule.** A plugin wrapper may stop short of a full unload (the MCP registry and
  the tunnel store stay constructed; they hold no sockets), but its comment says item by item
  what disable really does, and the inventory never claims more. Re-enabling replays the start
  decisions: a panel Stop and a lazy MCP stay as they were.
- **Config change** — the running instance has first claim: `apply_config` applies in place
  (jobs: an edit neither bounces the instance nor cancels runs); `Failed` keeps the instance
  running, the revision unmoved and answers `applied: false`; `NotApplicable` restarts when the
  descriptor says `restart_on_config_change` (tunnels, terminal) and otherwise only records the
  revision (mcp, data, jobs).

**Boot order** (`src/server.rs::run_gateway`; launcher noise is already scrubbed,
§host.daemon): log who started the gateway → PATH repair → first run → inject the env store
and the vault (two lookup paths; vault values never enter the env overlay) → load config →
port probe **before** orphan reaping → call log, registry, managed store → config and managed
MCPs registered, not started (the start decision is the mcp plugin's) → runtime services →
job system → tokens → traffic restore → the admin session (fatal if it cannot be sealed) →
group scopes → plugins registered (process last among the built-ins, so it starts first:
providers before consumers) → **bind** → `start_enabled()` spawned behind the open port in
reverse registration order → the hourly log sweep. Plugins start after the
bind so one slow plugin is never the whole gateway's outage; a request to a plugin still
starting gets the boundary's structured 503.

**Teardown:** `POST /api/shutdown` (Windows has no SIGTERM; a watch channel is the signal) or
ctrl-c → 3 s for in-flight requests → plugins stop in reverse registration order (tunnels
before the registry, so forwarded ports release while MCP connections drain) → runtime
services (unowned runs too) → registry close → flush the call log and traffic.

### §host.routes — Route trees and host-owned routes

Two trees — the main tree (panel, health, `/api/*` admin, `/api/db`, `/mcp/*`) and the extra
tree (tunnels, jobs, actions and runs, terminal, remote) — each carry the same layers, outermost
first: the **loopback guard** (§host.boundary) → the **admin session gate** (§host.session) →
the **plugin boundary** (a prefix owned by a plugin that is not serving → a structured 503
naming it; the longest owned prefix wins). Security answers before plugin state does. Both
trees also carry a 2 MiB body limit (413 `request body exceeds N bytes`). An axum layer covers
only the routes that exist when it is attached, so each tree gets its layers separately
(`src/app.rs::build_app`). Unknown paths and wrong methods both answer 404 `{error: "no route
for …"}`, never axum's bare 405; the old root spelling of a known MCP gets a moved-hint 404.

**Registration refuses conflicts, loudly.** A duplicate plugin id, a route prefix that overlaps
a host-owned route, and one that overlaps another plugin's prefix are all errors at
registration (`host/engine.rs`): a silently shadowed route would be a boundary hole.

**Host-owned routes** (a plugin may not claim them; `/api/plugins`, `/api/actions` and
`/api/runs` stay answerable while their providers are stopped):

| Route | Contract |
|---|---|
| `GET /api/plugins` | `{revision, plugins[], pages[]}`; each row `{id, kind, label, version, enabled, state, configRevision, pages[], lastError?, requires?, requiresMet?}`. Compiled (the descriptor), desired (the store row) and actual (`configRevision`) drifting apart is the visible gap of a restart or apply that has not happened |
| `POST /api/plugins/{id}/enable\|disable` | write the row, then reconcile; disable really stops the instance |
| `GET/PUT /api/plugins/{id}/config` | GET `{config, schema, revision}`; PUT validates (400, nothing persisted), CAS-writes (409), reconciles, answers `{revision, config, schema, warnings, applied[, error]}` |
| `/api/actions`, `/api/runs…` | §host.actions |
| `/api/tokens…` | §host.tokens — host-owned though the Token page sits in the MCP group, so disabling MCP takes the page off the air but leaves the credential API and CLI serving |
| `/api/secrets…` | §host.vault |
| `/api/groups/{scope}…` | §host.groups |
| `GET /api/info` | `tokenEnv`, `panelVersion`, `build` — the panel reloads itself when the build changes |
| `GET/PUT /api/autostart` | the one OS-level setting (§host.cli) |
| `GET /api/memory[?tree=1]` | §host.memory |
| `POST /api/shutdown` | answers first, then signals |
| `POST /api/session/ticket` | §host.session |
| `GET /health`, `/health/check` | unauthenticated; carries `build` (a short hash on loopback is acceptable) |

`GET /` and `GET /admin/*` serve the embedded panel with `no-store` (§panel). The MCP client
endpoints `/mcp/{name}` are the MCP plugin's domain (ADR-018, §mcp.endpoint).

### §host.boundary — The loopback boundary

swiss is a local tool. Three checks run on every route, before anything else:

1. **Bind.** The configured host must be a loopback address; a non-loopback host is refused
   when the config loads, not warned about.
2. **Peer.** The peer address must be loopback (an IPv4-mapped `::ffff:` prefix is stripped).
3. **Host and Origin.** Both headers, when present, must name loopback — the DNS-rebinding
   defence.

A refusal is `403 {error}`. Requests without `ConnectInfo` (tests driving the router with
`oneshot`) count as local; a real listener always uses `into_make_service_with_connect_info`.

**No framing.** Every answer — the panel shell, the lock page, assets, API answers and refusals —
carries `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY`. The panel
frames nothing of its own, and a page on another local port that framed the signed-in panel
could steer its clicks.

### §host.session — The admin session

The loopback checks cannot tell one local process from another, so `/api/*` and the panel
also need a credential. Two open the admin surface, **on top of** the loopback checks:

| Part | Rule |
|---|---|
| Login token | single-use, 120 s (`TICKET_TTL_MS`), at most 16 unredeemed. Minted by `swiss start` (which then opens the browser) and `swiss open`, via `POST /api/session/ticket` → `{token, url}` (itself protected) |
| Redemption | only `GET /?token=<t>`: valid → 303 to `/` with the cookie; invalid, used or expired → 401 lock page |
| Browser session | cookie `swiss_session_<port>` (the port is in the name: 19998 and 19999 share `127.0.0.1`), value `v1.<issued ms>.<random>.<HMAC-SHA256>`, `HttpOnly; SameSite=Strict; Path=/; Max-Age` 30 days |
| Cookie provenance | the cookie opens `/api/*` only for the panel's own pages: `Sec-Fetch-Site` must be `same-origin` or `none` when the browser sends it, else `Origin` must name the request's own `Host`, port included; a write with neither is refused (403). `SameSite=Strict` alone would let a page on any other port of `127.0.0.1` ride the cookie into blind POSTs. The CLI key needs no provenance: no page can send it |
| Signing key | 32 bytes in the sealed `session.json`, kept across restarts so a signed-in browser stays signed in |
| CLI key | `cliKey` in the same file, rotated on every daemon start, sent as `X-Swiss-Key` (never `Authorization`, which carries MCP bearers). Only the same OS user on the same machine can unseal it |
| Protected | every `/api/*` in both trees and the terminal WebSocket upgrade. `/` without a session serves the lock page; `/admin/*` and `/health` carry no data and stay open; `/mcp/*` keeps its bearer tokens |
| Fail closed | a request with `ConnectInfo` arriving while the session is not wired answers 401 |

The panel's `api()` answers a 401 with `location.assign("/")`; the lock page (English and
Chinese) says one thing: run `swiss open`. No username/password and no logout — deleting
`session.json` regenerates the key and ends every session.

### §host.tokens — MCP client tokens

`/mcp/*` is authenticated by bearer tokens, checked before the body is read.

- **Named tokens** — one per client, individually revocable: `GET/POST /api/tokens`,
  `GET /api/tokens/{id}/secret`, `POST /api/tokens/{id}/rotate`, `DELETE /api/tokens/{id}`.
  Comparison is timing-safe; `verify` returns the matching record so the call log and traffic
  attribute each request. A token's secret can be read back (loopback-only, behind the admin
  session): unlike the vault, a token is compared in plaintext anyway.
- **The config token** — the variable named by `tokenEnv` (overlay, then process env), then
  `SWISS_TOKEN`. The first boot seeds a token (`random_hex(24)`) when none exists.
- Token groups are the `tokens` scope (§host.groups); tokens have no manual order.

### §host.config — Config and the config store

`gateway.config.json` holds `port`, `host`, `tokenEnv`, `servers` (config-sourced MCPs),
root-level subsystem rows and versioned `plugins.<id>` rows `{kind?, disabled?, config?}`.

- **One grammar.** A missing row means enabled; only a literal `disabled: true` disables
  ("turn off jobs" = `{"jobs": {"disabled": true}}`). Unknown fields in a known schema are
  refused with their path.
- **The v1 → v2 projection does not migrate.** Old root rows keep working in place; a
  `plugins.<id>` row takes precedence (an empty one counts). Boot never rewrites the file; only
  an operator change does.
- **Revision** = the top 53 bits of the content's SHA-256 — fits a JS number, stable across
  restarts, and no metadata is written into the file. Every write is compare-and-set: a stale
  revision is 409 "configuration changed; reload before saving".
- **Validation layers.** PUT runs `validate_config` (rejected values are not persisted);
  boot runs `validate_config_for_start` (jobs drops bad definitions left by an older
  validator with a warning instead of failing the plugin); `warnings` report "storable but not
  runnable now" (an unregistered action).
- **Port.** `SWISS_PORT` > the config > 19999. `--port` is saved into the config (sealed,
  atomic) as the new default; illegal values are refused ("takes a number between 1 and
  65535").
- `managed.json` holds user-added MCPs, overrides, the enabled map, named tokens, the sidebar
  order, groups and parked revisions (§mcp.revisions).

### §host.refs — Credentials and the resolver

Credentials in config are references, never literals: `${ENV_VAR}` or `${secret://name}`.
Literal values exist only inside sealed files and are masked when echoed.

**One resolver, one pass** (`swiss_core::secure::refs::resolve`), applied at every use point
— there is no allowlist of boundaries, it is a contract every plugin inherits:

- `${UPPER_SNAKE}` → the env overlay, then the process env. **Missing → empty string** (the
  machine's current state may legitimately lack it). `process.exec` is the strict exception: a
  missing variable is a named error.
- `${secret://kebab-name}` → the vault. **Missing → hard failure** naming the use point and the
  reference, never a value (`references secret://stripe-key which is not in the vault`). An
  operator declared it; an empty credential would become a mysterious 401 on someone else's
  server.
- `${secret://name:default}` → the text after the first `:` stands in when the vault lacks the
  name.
- Outside `${…}` there are no references: a bare `secret://` is literal text, so
  `https://x/secret://aaa/y` passes byte-identical. Invalid names inside the envelope
  (`${secret://BadName}`) fail hard. An unclosed `${` is literal. A replaced value is never
  re-scanned. The scanner is hand-written over characters (ADR-007).
- Legacy whole-value `secret://name` strings are rewritten to `${secret://name}` at load
  (`migrate_legacy`, idempotent); the file picks up the new form on its next save. Mixed
  strings are not migrated.

**Use points** (each covered by a test): MCP http/rest headers, url and body templates; proc
args and env; tunnel passwords, key passphrases and rule fields; job commands and env; the
panel's MCP Test endpoint; MCP matching (a failed resolve counts as no match).

**Masking.** Config echoed to the panel replaces literal secrets with the sentinel `••••••••`
and restores them on PUT (`unmask_body`). References are not secrets and pass through as-is.
The vocabularies (URL-embedded passwords, header names, parameter keys) are in `mask.rs`.
The echo also drops the dead `readonly` key an old MCP definition may still store (stored state
keeps it until the next save).
Output captured from a run masks exactly the values its refs resolved to.

### §host.vault — The secret vault

A namespace-isolated store for credential values, referenced as `${secret://name}` (ADR-014,
ADR-019). It is **host mechanism, not a plugin** — everything depends on it, so it has no
disabled state; its page is a host asset.

- **Storage.** `secrets.json`, sealed: `{rev, secrets: {name → value}, groups, secretGroups,
  order}`. Names match `[a-z][a-z0-9-]{0,63}`. Loaded once at boot into memory; it never joins
  the env lookup path.
- **Child isolation.** Only the env overlay is merged into child environments. The vault has no
  merge path at all — a key saved for one HTTP MCP is not readable by every proc MCP, job and
  shell. A test pins it.
- **API — values go in, never come out.** `GET /api/secrets` → names with update times, `rev`,
  `groups`, `secretGroups`, `order` (stored as-is, stale names included). `PUT
  /api/secrets/{name}` `{value, rev}` creates or overwrites; `DELETE /api/secrets/{name}`
  `{rev}` (also prunes the name from `order`). A missing rev is 400, a stale one 409.
- **Order.** Rows are draggable within and across groups. Names in `order` sort by slot;
  unmentioned names follow in name order; an empty `order` is name order. Unknown names and
  duplicates are dropped on write; stale names are ignored on read. A value write never
  touches `order`.
- **Export.** Vault values leave `swiss export` as `******` and are typed in again after an
  import; import skips any value equal to the mask so it never overwrites a real secret.
- No zeroize (ADR-007): values must stay resident to be resolved; the attack surface is the
  file and the children, not the heap.

### §host.groups — Groups

One model (`swiss_host::groups`, ADR-015), seven scopes: `mcps`, `tokens`, `conns`, `rules`,
`jobs`, `secrets`, `targets`. Each owner registers its scope in the `GroupScopes` table; the
host routes by the scope string with no match arm.

**Invariants** (unit-tested once, in `groups.rs`):

1. The name list is never empty; replacing it with `[]` is 400 "at least one group must
   remain".
2. Names are trimmed, non-empty, ≤ 64 characters, unique case-insensitively; members store
   the canonical casing.
3. A member with no explicit entry, or whose group is gone, renders in the **first group** —
   the slot, not the name `default`, is the sink.
4. Replacing the list deletes omitted groups (`default` included); their members fall to the
   new first group.
5. Rename keeps the slot and carries its members; a collision is 400.
6. Reordering never moves a member: if the old first group survives but is demoted, members
   that rendered there only by default are first pinned to it (in the new list's spelling).
7. Order is not part of `Groups`: each scope keeps its own flat order (the MCP `order`, the
   tunnel arrays, the job array, the vault's `order`, the target list; tokens by creation
   time), sliced by group. Moving a member between groups never rewrites the ordering.

**API** (identical shapes for every scope; reads ride each scope's own list, which carries
top-level `groups` and a per-row `group`):

| Method | Path | Body | 200 |
|---|---|---|---|
| PUT | `/api/groups/{scope}` | `{groups: [names]}` | `{groups}` |
| POST | `/api/groups/{scope}/rename` | `{from, to}` | `{groups, moved}` |
| PUT | `/api/groups/{scope}/members/{id}` | `{group: name \| null}` | `{group}` (canonical) |
| PUT | `/api/groups/{scope}/order` | `{order: [ids]}` | `{order}` — `tokens` refuses (400) |

Unknown scope or member → 404; invariant violations → 400. The on-disk keys are in
§formats.groups; the panel component is §panel.groups.

### §host.actions — Actions and runs

**Actions** are named capabilities (`GET /api/actions` lists them with their schemas).
Duplicate names are refused; disabling the owner unregisters them, and new submissions get
"unknown action". `process.exec` (strict env refs) and `process.legacy-command` (lenient, the
v1 job shape) come from the process plugin.

**Runs** — `RunCoordinator`, the one execution plane for panel, CLI, jobs, MCP and remote:

- `POST /api/runs` answers **202 + `runId`** at once (the request does not own the task);
  unknown action 400; a full pool **429** naming the bound. Defaults: 2 concurrent, 32 queued,
  8 remote runs per target. Timeout default 10 min, max 24 h.
- `GET /api/runs`, `GET /api/runs/{id}`, `POST /api/runs/{id}/cancel`. Cancel is per owner;
  the terminal state is first-wins; a deadline takes the same cancel path as a manual stop; a
  cancel awaits the reaped child and the joined pipes. A panicked action is one failed run.
- `GET /api/runs/{id}/output?after=&max=` → `{runId, state, cursor, nextCursor, output,
  truncated, terminal}`: the live cursor slice a follower polls. The live buffer keeps
  256 KiB; a finished run keeps its last 64 KiB; a read returns at most 128 KiB; a cursor older
  than the window reads the oldest kept bytes with `truncated: true`. The 32 most recent
  finished runs stay in memory; durable history belongs to the producing plugin (§remote.history,
  §jobs.runlog).
- **`actor`** — who asked: `cli:<user>@<host>` (self-declared over loopback), `mcp:<token
  label>`, `panel`, `jobs`, or `api` when omitted. The audit trail's who column.

**The process supervisor** (`services/process.rs`) runs every child: bounded capture that
truncates while reading (16 KiB default — never `read_to_end`), pipes joined without hanging
(5 s drain grace), and a subtree kill (a Windows kill-on-close job object; a POSIX process
group). Children spawn with `CREATE_NO_WINDOW`.

### §host.seats — Capability seats

A seat decouples a provider plugin from a consumer plugin with no crate edge between them.
The seat is held by `RuntimeServices`; the provider registers on start and on stop goes
**withdraw → drain → close**; the consumer sees a possibly empty seat, never a missing module.

| Seat | Provider → consumer | Notes |
|---|---|---|
| Connection catalog (`services/catalog.rs`) | mcp → data | Data takes a request-scoped lease per `/api/db` call; drain 5 s. A connection key includes config identity and credential context, never just host:port. Disabling Data releases only its own leases |
| Shell (`services/shell.rs`) | tunnels → terminal | A session is a lease (terminals never hand back voluntarily, drain 3 s); `PtySession` is a transport-neutral duplex, so terminal links no SSH client (ADR-011) |
| Remote transport (`services/remote.rs`) | tunnels → remote | exec, SFTP and streaming in 64 KiB chunks; §remote |

The composition also feeds Data's connection picker the sidebar's visual order: registry
names sorted by the flat manual order (unranked last, by name), sliced by group.

### §host.memory — The memory reading

`GET /api/memory[?tree=1]` → `gatewayMb` (working set), `heapUsedMb`/`heapTotalMb` (private
bytes; the Node-era field names), `childrenMb`, `processCount`, `childrenPending`, `at`;
`swiss status` prints the same. The process-tree walk (Toolhelp32,
a direct call) is cached 20 s, deduplicated in flight and skipped when no proc MCP runs; a
call without `tree=1` answers `childrenPending: true` rather than a confident 0. Every panel
read passes `tree=1`. Numbers are honest or absent. The records are in §product.memory.

### §host.cli — The command line

`swiss start | stop | restart | status | logs | token | creds | open | export | import <file> |
skill install | autostart [on|off] | update | serve | remote … | run … | api <METHOD> <path>
[json]`. Options: `-p/--port` (start: listen and save as default; others: which instance),
`-f` (start: foreground; logs: follow), `--no-open`, `-n/--lines` (default 200), `--force`,
`--json`, `-h`, `-v`. `swiss --help` is the reference text; `remote` and `run` are §remote.

- **`api`** — one admin call signed with the CLI key; what scripts use instead of curl.
- **`export` / `import`** — the one plaintext escape: a v1 bundle of config, managed, tunnels,
  OAuth, env and secrets (vault values masked, §host.vault). Import re-seals every file on this
  machine; env and secrets merge (imported values win, nothing deleted). The CLI warns that
  the rest of the bundle is plaintext.
- **`autostart`** — start at sign-in: a registry Run value on Windows, a LaunchAgent on macOS,
  a systemd user unit on Linux. The OS registration is the store (no revision to race);
  `GET/PUT /api/autostart` read and apply it.
- **`update`** — checks GitHub for a newer release; updating stays a manual exe swap.
- **`skill install`** — writes the embedded `swiss` and `swiss-remote` skills into each AI
  client's skills directory, staged beside and renamed.
- **`creds`** prints the panel URL and the gateway token; **`open`** mints a login link.

### §host.daemon — Daemon, pid file, logs, build stamp

- **Detach.** `swiss start` spawns `serve` with `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`
  and no job object, so the daemon survives the CLI. Only "my own child is still alive" counts
  as started — the loser of a racy double start cannot report success.
- **Attribution.** `start` hands its own pid and argv to the daemon in `SWISS_SPAWNER` (removed
  from the environment at boot); the first log line, `gateway booted`, records it with the
  parent process, so "who started this instance" is answerable after the fact.
- **Launcher noise.** The daemon's environment is the user's, not the launching shell's. One
  list (`swiss_core::env`, case-insensitive) is removed on both paths (`start` via
  `env_remove`, `serve` in-process before any thread): `NO_COLOR`, `CI`, `TF_BUILD`,
  `GITHUB_ACTIONS`, `CLAUDECODE` and `CLAUDE_CODE_*`, `DSH_*`, `WT_SESSION`/`WT_PROFILE_ID`,
  `TERM_PROGRAM*`. It is a blocklist: `PATH`, `HOME`, proxies and `SWISS_*` stay.
- **PATH repair** at boot adds `~/.local/bin`, `%APPDATA%\npm` and `~/.cargo/bin`. Command
  resolution follows `PATHEXT` and never tries extensionless spellings on Windows (os error
  193).
- **Pid file** `gateway-<port>.pid`: five fields, parsed strictly (a partial record is
  rejected, a torn file means not running).
- **Stop.** `POST /api/shutdown` → wait up to 10 s for the port to close → tree-kill. A live pid
  that is not serving is refused a blind kill without `--force`.
- **Status** reports the running build (`/health`) against the binary on disk (`--version` of
  the pid file's entry) and says "restart to pick it up" when they differ; `--json` carries
  `build` and `diskBuild`.
- **Build stamp.** `build.rs` sets `SWISS_GIT_HASH` (`<short>` or `<short>-dirty`) and
  `SWISS_BUILD_TIME` (RFC 3339 UTC), both `unknown` without git — the build never fails for it.
  `swiss --version` prints `swiss <version> (<hash>, <time>)`.
- **Logs.** JSON lines `{ts, level, msg, extra?}` on stdout, redirected by the daemon into
  `gateway-<port>.log`, rotated at 10 MB keeping one generation. `swiss logs -f` follows by
  polling (Windows change notifications are unreliable for appends).
- **First run** creates the private home, seeds the echo config and ensures a token exists.
- **Orphan reaping.** The kill-on-close job holds only while the gateway lives; after a hard
  kill the next boot tree-kills the pids in `.proc-pids-<port>.json` — after probing the port
  (a live instance's ledger is left alone) and skipping any live pid that is not a descendant.

### §host.ops — Production, the test instance, deploys

- **19999 is production.** It runs from `bin\swiss.exe`, a copy of the build output that git
  ignores — never from `target\`. The linker and the daemon never share a file, so a build
  happens while the old daemon serves.
- **Deploy** only with `scripts/deploy.ps1`: gates (unit tests, the integration suite,
  clippy; `-SkipGates` skips all) → release build → stop → copy into `bin\` (retried while the
  old process releases its handle) → start → status → prove `/health`'s `build.hash` equals
  `bin\swiss.exe --version`, or fail with both values. The outage is stop + copy + start, and
  the script prints it.
- **19998 is the test instance** (`scripts/test-instance.ps1`): `-Start` copies the state files
  (not pid, log or terminal files) into `%LOCALAPPDATA%\swiss-test-home`, sets `SWISS_HOME` and
  `SWISS_PORT=19998` and serves; `-Stop` kills **only the process owning port 19998**, never by
  image name; `-Fresh` starts from an empty home. The copy works because DPAPI binds
  `master.key` to machine + user, not directory. Saves on 19998 touch only its own home.

## §mcp — The MCP plugin

The most important plugin: it hangs every MCP server — a stdio child, a remote HTTP MCP, a REST
API declared in config, an in-process database driver, a compiled-in tool set — on one path
segment under `/mcp/`, and owns the registry, the adapters, OAuth for remote MCPs, the call
log, the traffic ring and the Servers/Traffic/Token pages. Code: `crates/swiss-mcp` plus the
composition in `src/app.rs` (the endpoint), `src/adminapi.rs` (the admin routes),
`src/builtin.rs` (the descriptor) and `src/mcp_link.rs` (the tunnels glue).

**Descriptor.** Id `mcp`; pages `mcps` (order 10, the sidebar entry), `traffic` (20) and
`tokens` (30), the last two shown only as second-level bars inside the MCP group (§panel.nav);
routes `/api/mcps` and `/api/traffic`; `restart_on_config_change: false` — no MCP reads a
config row today, and restarting every managed MCP because one line moved is destruction
dressed up as applying config. The Token page belongs to the group but `/api/tokens` is
host-owned (§host.tokens); a test pins that split.

**Start and stop.** Start: the background timer → `start_hosted_mcps` (the start decisions:
panel-disabled stays stopped, lazy stays Idle, the rest start, one failure isolated) → register
the connection catalog **last**, so a failure leaves nothing listed-but-stopped. Stop: withdraw
the catalog (no new leases) → drain in-flight leases (an honest warning on timeout) →
`close_all` (pools closed, children tree-killed — what really costs memory) → clear the seat.
**No definition is unregistered**: order, logs and `managed.json` survive, and re-enabling
replays the start decisions (the honesty rule, §host.lifecycle). With the plugin disabled, the
client endpoints, `/api/mcps` and `/api/traffic` answer the host's structured 503.

### §mcp.endpoint — The client endpoint (ADR-018)

`/mcp/{name}` is the MCP plugin's domain; the root belongs to host chrome (`/`, `/admin`,
`/health`, `/api/*`) and to future plugins as `/<plugin-domain>/*`. Inside the domain there are
**no reserved words**: an MCP may be called `health`, `api`, `admin` or `mcp`. Everything that
prints an endpoint builds it as `origin + "/mcp/" + name` (the panel's `endpointUrl`, the
connect snippets).

`POST` and `DELETE` only; there is **no GET handler** (§formats.protocol) and a GET falls into
the same 404 as any unhandled method. A request runs, in order:

1. **Bearer first.** `verify_bearer` reads only the `Authorization` header against the named
   tokens (§host.tokens); a failure is 401 `{error: "Unauthorized"}` and the body is never
   read. The matched token rides down for attribution.
2. **Plugin gate.** `plugin_client_guard` — after auth, so a stranger still sees the plain
   401, and before any wake, so a lazy child is never spawned behind a disabled plugin.
3. **Lookup.** An unknown name is 503 JSON-RPC `-32603 "Unknown MCP path: <name>"`.
4. **Wake.** An Idle entry is started and the request **waits** for it: AI clients do not retry
   a 503 helpfully, so the request that wakes a child is the request it serves, bounded by the
   adapter's own start timeout. Concurrent wakes coalesce into one spawn. A failure is 503
   `MCP '<name>' failed to start: …`.
5. **Activity.** `note_activity` pushes a running lazy entry's idle deadline back.
6. **Disabled.** A stopped entry is a disable (§mcp.revisions): 503
   `MCP '<name>' is disabled — enable it from the panel`.
7. **Forward.** The body is read once (the 2 MiB limit), the JSON-RPC envelope feeds the traffic
   record, and the **original bytes** are forwarded. The response is read back under a 16 MB
   cap (the first 16 KB, `RESPONSE_CAPTURE`, are the traffic preview); a response that cannot be
   read back is an honest 502, never a 200 with an empty body. The token's label is scoped into
   a task-local so the call log can attribute the call.

`DELETE /mcp/{name}` is 204 after the bearer check — the stateless protocol has no session to
close, but a client that sends one closes cleanly.

**Service cache.** The rmcp `StreamableHttpService` is cached per name with the entry's
generation; a generation mismatch (a restart) cancels the old service before the new one is
installed. Rename and delete fire the registry's evictor, registered as a **`Weak`** callback
so the context → registry → evictor → context cycle never forms.

**The cutover is hard.** The old root spelling (`/{name}`) is not an alias. A `POST` or `DELETE`
to a single-segment root path naming a **registered** MCP gets a 404 whose body says
`… moved to /mcp/<name>, update the client URL`; an unknown name keeps the plain 404.

### §mcp.registry — Registry and lifecycle

`crates/swiss-mcp/src/registry.rs`. One entry per name: def, adapter, lifecycle, health, the
three page caches.

- **States.** `Starting / Started / Stopping / Stopped / Idle / Error`. The shown state is the
  health (up/down/unknown) while started, else the lifecycle. On registration a lazy entry lands
  in Idle, the rest in Stopped.
- **One op queue per entry.** A tokio mutex serialises every lifecycle operation; mutable state
  is snapshotted out and written back under short std locks, never held across an await. Two
  concurrent starts build once; a stop racing a start leaves no orphan. Tests pin both.
- **Generation fence.** Start and stop each bump the generation. A health probe records it and
  discards its result if it changed meanwhile — a late failure once marked an already-stopped
  entry down forever.
- **Tombstone.** Delete sets `deleted` first, then waits for the stop; a queued start wakes,
  finds the tombstone and refuses, so no pool or child is built for an entry already gone.
- **Where stop lands.** A lazy entry returns to **Idle** (the next request wakes it — a lazy
  entry has no true off; that is disable or delete); a non-lazy one to Stopped. Stop also clears
  status, latency, the last error and the page caches.
- **`update_def`.** In one queue slot: stop → carry the old adapter's tool and resource toggles
  to the new one (otherwise a connection edit would silently re-enable every disabled tool) →
  swap def and adapter → start if it was running.
- **Rename.** Moves the map key, fires the evictor, renames the call log (in-flight calls keep
  writing through a bounded `current_name` alias), and syncs the adapter's name cell. The admin
  route then re-keys OAuth credentials, the store and the tunnel links (§mcp.admin).

**Background timer** (`start_timer`):

| Task | Period | Rule |
|---|---|---|
| Health probe | 15 s | one task per Started entry, so one slow ping never holds up the rest. `http` and `rest` have no ping and report **Unknown**, never Down — a metered third-party endpoint must not spend a real request every 15 s. **Do not add a ping to them** |
| Idle reap | 1 s | stops lazy entries past their deadline through the op queue. `DEFAULT_IDLE_MS` is 600 000; `idleMs: 0` opts out. The `is_lazy` guard in `arm_idle` is load-bearing: the timer once armed for every started entry and quietly stopped http MCPs and DB pools ten minutes later |
| Page-cache sweep | each probe round | drops tools/resources/prompts page caches not paged through for 60 s |

`close_all` (shutdown) stops the timer and every entry concurrently; a graceful shutdown then
flushes the call log.

**Paging.** `GET /api/mcps/{name}/{kind}` (kind ∈ tools, resources, prompts) serves 50 per page
from an incremental server-side cache behind a base64url cursor; the fill is capped at 200
upstream pages and a cache lives 60 s. A server that once dumped thousands of resources does
not stay resident.

**Toggles.** A disabled tool is **absent** from `tools/list` — the broadcast list is the
contract. Resources have one master toggle per MCP. Both persist first
(`disabledTools`, `resourceToggles`), then change memory.

**Plugin-owned entries.** Another plugin may mount a builtin entry in the registry: the remote
plugin's `remote` type at `/mcp/remote` (§remote.mcp), mounted on its start and taken down on
its stop. A user MCP that already owns the name wins; the builtin stays unmounted.

### §mcp.defs — Definitions

A def is a JSON object with a `type`. Where it lives: `gateway.config.json` `servers` (config
MCPs) or `managed.json` `mcps` (panel-added, and `override: true` entries that replace a config
MCP's def so the config file keeps its committed shape). `mcpEnabled` holds the disabled flag of
config MCPs, which otherwise would live only until the next boot.

**One expansion point.** `${ENV}` and `${secret://…}` references (§host.refs) are never expanded
at load: the def held by the registry, the panel and every persistence layer is the reference
verbatim. `make_adapter` expands a **copy** through the strict `resolve_def_checked` — a vault
reference this machine cannot resolve refuses the build. The connection test expands the same
way. A test pins that an unset url reference is a start error while the stored def still reads
`${…}`.

**Masking.** On the way to the panel `mask_def` swaps credential fields for the `••••••••`
sentinel and the password inside a URL for `••••••••@`; on the way back `unmask_body` restores
an unchanged sentinel to the stored value. The browser never receives a credential, and editing
an unrelated field cannot break one.

**Keys per type** (the `build_typed_def` whitelist; anything else is refused):

| Type | Required | Other keys | Notes |
|---|---|---|---|
| `proc` | `command` | description, env, cwd, exposeResources, exposePrompts, timeoutMs | lazy by default (§mcp.proc); `expose*` default on, only an explicit false is stored |
| `http` | `url` | description, headers, exposeResources, exposePrompts, proxy, auth, oauthClientName | url is `http(s)://` or a reference; `auth: "oauth"` hands Authorization to §mcp.oauth; proxy must be an explicit `http(s)://` |
| `rest` | `baseUrl`, non-empty `tools` | description, headers, timeoutMs (30 s), proxy | `tools` is stored as authored (§mcp.rest) |
| `mysql`, `mariadb` | — | description, host (localhost), port, user, password, database, timezone, maxRows | `database` passes `assert_ident` (it is spliced into `SHOW CREATE TABLE`) |
| `pg` | **`url`** | description, maxRows | an empty url is dangerous — libpq would fall back to `PGHOST`/`PGDATABASE` — so it is the one required field |
| `redis` | — | description, host, port (6379), password, db, allowDestructive, allowEval | §mcp.db |
| `zai-vision` | `apiKey` | mode (`ZHIPU`/`ZAI`), model, baseUrl, description, timeoutMs, proxy | the key must be a reference; a literal is refused (§mcp.zai) |
| `figma` | — | description, exposeResources, exposePrompts | url, auth, oauthClientName, headers and proxy are refused: "decided by the figma type" (§mcp.figma) |
| `echo` | — | — | the demo |

`lazy` is accepted on every type: the panel's *Start automatically* checkbox writes its inverse.
Only `proc` is lazy by default.

**Names** match `NAME_RE`: an ASCII letter or digit, then `[A-Za-z0-9_-]`, 63 characters at most.

### §mcp.adapters — The adapter families

`make_adapter` dispatches on the def's `type`; nothing else in the tree matches on it.

| Type | Adapter | Reaches |
|---|---|---|
| `echo` | `echo.rs` | one echo tool, a hand-written `ServerHandler` (rmcp's `macros` feature stays off) |
| `proc` | `proc.rs` | a stdio child (§mcp.proc) |
| `http` | `http.rs` | a remote streamable-HTTP MCP (§mcp.http) |
| `figma` | `http.rs` | the same adapter from a fixed def (§mcp.figma) |
| `rest` | `direct.rs` + `rest.rs` | a plain HTTP API declared in config (§mcp.rest) |
| `mysql`, `mariadb`, `pg`, `redis` | `direct.rs` + the engine | an in-process driver (§mcp.db) |
| `zai-vision` | `direct.rs` + `zai.rs` | compiled-in vision tools (§mcp.zai) |
| `remote` | `remote.rs` | builtin only, mounted by the remote plugin (§remote.mcp) |

`direct.rs` is the shell for compiled-in engines: a lazily built, single-flight connection and
toggle seeding. `proxy.rs` is the transport-agnostic half every proxying adapter shares (the
`RemoteMcp` seam, paging aggregation, annotation stripping, one shared client per proxy).
Compiled-in results go to the model as **compact JSON**; display formatting belongs to the panel
(§mcp.calls) and never costs model context.

**Output budget.** Engine results render under `DEFAULT_LIMITS` — 1000 items and 256 KB. Past
the budget items are shed, then halved, and the result says it is no longer valid JSON.

**Calls.** A compiled-in engine (`tool_server.rs`) answers `tools/call` as the MCP spec asks. A
tool that ran and failed — a driver error, a refused command, a vendor 4xx — is an in-band result
with `isError: true`: the model reads the reason, and the call log records a failure. Only a tool
the list does not advertise (unknown, or disabled by the operator) is a protocol error, `-32602`;
naming a disabled tool never runs it. A schema that declares `additionalProperties: false` —
every built-in engine's — refuses an argument it does not name, in-band, listing the ones it
takes, so a typo such as `limt` is not silently a default. A `rest` tool's schema stays open:
its templates may read any argument.

**A new family** is a new adapter module plus one `make_adapter` arm plus its `build_typed_def`
row; there is no third-party module door (ADR-001). The `swiss-add-plugin` skill covers a whole
new plugin; a new MCP type is a smaller change inside this one.

### §mcp.proc — stdio children

- **Lazy by default** — the product's biggest memory lever (§product.nongoals): an npx/uvx
  child is 50–150 MB, so it lives only while used and is reaped after the idle period.
- **Timeouts.** `PROC_HANDSHAKE_TIMEOUT_MS` (60 s, room for a cold npx/uvx download) bounds
  `initialize`; on failure the subtree is killed before the error returns.
  `PROC_CALL_TIMEOUT_MS` (180 s — vision and reasoning calls outlive the SDK's 60 s) or the def's
  `timeoutMs` bounds each call.
- **One child, many requests.** All concurrent requests share the child (JSON-RPC ids keep them
  apart); each request gets a fresh proxy server over the same child client.
- **stderr** is kept in a 64 KB ring (`STDERR_MAX`) and shown under the Logs tab.
- **Teardown.** On Windows every child is in a Job Object with kill-on-close, so even a hard
  kill of the gateway takes the subtree along (ADR-008). Each pid also goes into the
  port-scoped ledger (`.proc-pids-<port>.json`); the next boot reaps strays before registering
  any MCP. The FFI lives in one `win` module — the file's only `unsafe`.
- **Command line.** Tokenised by the process service's shared tokenizer; `npx`/`uvx` resolve
  through the repaired PATH (§host.daemon) without a resident `cmd.exe` wrapper.

### §mcp.http — Remote HTTP MCPs

A remote MCP reached over **streamable HTTP only** (a legacy SSE endpoint is not supported —
this gateway serves no GET stream to test it against). rmcp ships no client transport in the
feature set in use, so the half the proxy needs is written over reqwest: `initialize` (offering
`2025-06-18`), notifications, and one JSON-RPC POST per request answered by JSON or a
single-shot SSE stream, with the session header and the negotiated version carried. The
listening GET stream is deliberately not ported: the proxy forwards client-initiated requests
only. The default per-request deadline is 60 s.

Header names are validated once at construction, so a malformed one is a start error. A
hand-written `Authorization` header together with `auth: "oauth"` is refused at construction.
`proxy` routes the outbound connection through an explicit HTTP(S) proxy, shared by the OAuth
half. There is no health ping (§mcp.registry).

### §mcp.rest — REST declared in config

Turns a plain HTTP API into MCP tools without writing an adapter — the companion to `http`,
which needs the far end to speak MCP already. The rule: **the request you write is the request
that gets sent.** A tool's `request` block (`method`, `path`, `query`, `body`) is the vendor's
own example copied out of their docs, with the values the model controls replaced by `{{arg}}`;
everything else stays literal. `input` declares the arguments (`type`, `required`, `default`,
`enum`, `description`) and compiles to the tool's schema; `pick` selects top-level keys of the
reply.

`{{arg}}`, **not** `${arg}`: `${VAR}` already means a reference resolved before the adapter
sees the def, and two identical-looking syntaxes resolving from different places is a trap. A
failed response quotes at most 600 bytes of its body back. No health ping.

### §mcp.db — The database engines

The drivers are in-process; the tool surfaces are deliberately tiny.

| Engine | Tools | Connection |
|---|---|---|
| mysql / mariadb | `mysql_query`, `mysql_list_tables` | sqlx pool, max 5, acquire 5 s; ping `SELECT 1 AS ok` |
| pg | `pg_query`, `pg_list_tables`, `pg_describe_table` | sqlx pool, max 4, idle 60 s (longer than the probe period, or every probe forks a backend) |
| redis | `redis_scan`, `redis_read`, `redis_command` | one multiplexed, auto-reconnecting `ConnectionManager`; a 10 s command timeout that also bounds the offline queue; a pipeline is one round trip |

- **Pools are built lazily** and kept small (`min_connections(0)`); the pool is the
  per-connection cost (§product.tactics).
- **A cut stream is an error.** mysql must see the EOF/OK terminator and pg must complete every
  statement group; otherwise the call reports the connection lost mid-query rather than passing
  partial rows off as a result. A test pins each.
- **pg** keeps the simple protocol for a multi-statement query: one summary group per statement.
- **No read-only mode.** `mysql_query` and `pg_query` run writes as well as reads (gate 2
  pins the write contract); the SQL helpers (`sql.rs`: one statement, the automatic row limit)
  shape the call and give clear errors — they are not a security boundary. A `readonly` key an
  old definition still stores is dead (§data).
- **Redis policy.** Ordinary writes run. Commands that damage the server itself (`SHUTDOWN`,
  `DEBUG`, `REPLICAOF`, `MIGRATE`, `SWAPDB` …) are always refused; `FLUSHALL`/`FLUSHDB` need
  `allowDestructive`; scripting (`EVAL`, `FCALL` and their variants — the one family no other
  rule can inspect) needs `allowEval`; container commands are decided per subcommand.
- **Tool schemas.** Numeric arguments are integers with their bounds declared: a query's
  `limit` 1–10 000, `mysql_list_tables`/`pg_list_tables` `limit` 1–1000 and `page` from 0,
  `redis_scan` `count` 1–10 000, `redis_read` `offset` from 0 and `limit` 1–1000. The engines
  still clamp, so a client that skips validation is bounded, not refused. Descriptions state this
  instance's own numbers: the query tools name the def's `maxRows` as their row cap, and
  `redis_command` lists exactly what this instance rejects given `allowEval` and
  `allowDestructive`. Every schema refuses undeclared arguments (§mcp.adapters).
- **Resources.** Each engine exposes its schema or keyspace as MCP resources (shard folding,
  bounded sampling, the protocol's narrow 2024-11-05 field set).
- **Browsers.** Each engine also carries the Data plugin's `DbBrowser` half (`*_browser.rs`),
  reached through the connection catalog (§mcp.catalog, §data).

### §mcp.zai — zai-vision (ADR-022)

The GLM vision tool set compiled in: eight tools (`ui_to_artifact`,
`extract_text_from_screenshot`, `diagnose_error_screenshot`, `understand_technical_diagram`,
`analyze_data_visualization`, `ui_diff_check`, `analyze_image`, `analyze_video`) over an
OpenAI-compatible chat-completions endpoint. `mode` picks the vendor endpoint (`ZHIPU` or
`ZAI`); `baseUrl` overrides it for a self-hosted shape. Retries skip every 4xx except 429. The
system prompts are ported verbatim from the upstream package (`zai_prompts.rs`, re-extracted by
`scripts/extract-zai-prompts.js`, attributed in `THIRD_PARTY_NOTICES.md`). `apiKey` must be a
reference — a literal key is refused by the type.

A media source is an http(s) URL, passed through, or a local file the gateway reads and sends
base64-encoded. The local path must be **absolute**: upstream ran as the client's own child, in
the client's project directory, while the gateway would resolve a relative path against its own
working directory and quietly read another file. A local image is `.jpg`/`.jpeg`/`.png` up to
5 MB; a local video is `.mp4`/`.mov`/`.m4v` up to 8 MB — upstream encoded any extension as video,
which would let any file the gateway can read ride to the vendor. Both are checked before the
file is opened. The schemas refuse undeclared arguments.

### §mcp.figma — The figma type (ADR-021)

`{"type": "figma"}` is sugar: at build time it expands to an `http` def with `auth: "oauth"`
aimed at `FIGMA_MCP_URL` (`https://mcp.figma.com/mcp`), and builds exactly the adapter a
hand-written def would get. The type decides the endpoint, the credential mode and the client
name, so the def is one field long and cannot be configured wrong; any key it decides is
refused. `is_oauth(def)` decides OAuth eligibility for both spellings.

### §mcp.oauth — OAuth for remote MCPs (ADR-020)

`crates/swiss-mcp/src/oauth.rs` plus the adapter's refresh half. Eligible: an `http` def with
`auth: "oauth"` (optional `oauthClientName`) or a `figma` def.

**Flow.**

1. **Discovery.** Protected-resource metadata (RFC 9728), then the authorization server's
   metadata (RFC 8414). A failed discovery is never cached.
2. **Registration.** Dynamic client registration (RFC 7591) **on every flow**, because the
   loopback callback port is random. Some providers allowlist `client_name` — Figma accepts
   only `"Claude Code"` and `"Codex"`.
3. **Authorization.** Authorization code + PKCE S256; `state` is 32 random bytes compared in
   constant time; `resource` per RFC 8707. The callback is a one-shot listener on
   `127.0.0.1:0` at `/callback`.
4. **Exchange.** Token endpoint with `client_secret_post`, a 20 s deadline.
5. **Store.** Credentials go into the sealed `mcp-oauth.json` keyed by MCP name —
   `client_id`, `client_secret`, `access_token`, `refresh_token`, `expires_at`, `scope`, `at`.
   A separate file from the vault because these are machine-rotated, not operator-authored.
   When the `client_id` changes, the new client is stored first, then the old tokens cleared
   (one atomic swap — never a new client beside old tokens). On approval the MCP starts, so the
   sidebar turns green without a second click.

**Provider defaults.** A figma host gets client name `"Claude Code"` and scope `mcp:connect`;
any other host gets `"swiss"` and the discovered scopes. `oauthClientName` always wins.

**In the adapter.** The bearer is added after the def's headers. An access token within 60 s of
expiry counts as expired. A 401 triggers one **single-flight** refresh shared by every
concurrent caller, then **one** retry; a second refusal, or an `invalid_grant`/`invalid_client`
from the token endpoint (which also clears the stored credentials), surfaces
`needs authorization` (`NEEDS_AUTH`) — the marker the panel keys the Authorize button on.

**API.** `POST /api/mcps/{name}/authorize` starts one flow per name (single-flight: a live flow
is handed back, a finished one replaced) and returns
`{flowId, status: "authorization_required", authorizationUrl}`; `GET` polls
`{status: starting | authorization_required | approved | error, error?, toolsCount?}`. A flow
waits at most 5 minutes for the redirect. The panel opens the URL and polls every 3 s; **the
gateway never spawns a browser.** List and detail rows carry `oauth: "authorized" |
"needs-auth" | null` plus `expiresAt`. Rename re-keys the credentials; delete clears them;
export/import carries the section (§host.cli).

**Secrets stay in the store.** Tokens, client secrets, codes, verifiers and state never reach a
log, an error, an API response or the call log. Error templates:
`figma: OAuth registration refused (HTTP 403) — the server only accepts specific client names;
try oauthClientName "Claude Code" or "Codex"` and
`figma: needs authorization — open the panel and click Authorize`.

**Not built:** device flow, `client_credentials`, a generic provider UI, a PAT-based REST
adapter, token reveal, automatic re-authorization, several accounts per MCP.

### §mcp.revisions — Revisions and disable (ADR-023)

**The name is the service; a def is one revision of it.** OAuth credentials, logs, group
membership and tunnel links hang on the name. A revision is a parked def — never registered,
never started, never resolved.

- **Storage.** `managed.json` `revisions: {name: [{def, at, note}]}`, at most **5** per name,
  the oldest evicted. For a config MCP, restore goes through the override mechanism. Rename
  carries the list; delete clears it; boot reads only `mcps`.
- **API.**
  - `GET /api/mcps/{name}/revisions` → `{revisions: [{index, at, note, type}]}`, oldest first.
  - `POST /api/mcps/{name}/replace` parks the **current** def and installs the new one. Every
    fallible step (`build_def`, `make_adapter`) runs before anything is written: a bad def is
    400 and changes nothing. A restart failure after the swap is 200 with `restartError`.
  - `POST …/revisions/{index}/restore` takes the target **first**, then parks the current def
    (parking first could evict the very revision being restored at the cap).
  - `DELETE …/revisions/{index}` → `{deleted: true}`.
  - A stopped MCP stays stopped through any def swap: editing is not starting.
- **Disable.** The panel says **Disable/Enable** and the state reads `disabled`; the API verbs
  stay `start`/`stop`, which persist `enabled`/`mcpEnabled`. A stop that survives a boot and
  refuses every client *is* a disable (the 503 in §mcp.endpoint says so).
- **Discoverability.** A right-click on a sidebar row opens its menu — Rename…, Disable/Enable
  (read live from the lifecycle), Delete. The row is a `<button>`, which cannot nest the ⋯
  button other plugins' rows use. Disable/Enable also sits beside Restart in the detail menu.

Not built: two live revisions under one name, a diff view, automatic rollback.

### §mcp.admin — The admin API

Every route answers `{error}` on failure (§formats.wire) and sits behind the admin session
(§host.session).

| Route | Behaviour |
|---|---|
| `GET /api/mcps` | status rows — name, source, type, tag, description, lifecycle, state, group, and optionally latencyMs, lastCheck, reason, startedAt, oauth, expiresAt (absent, never null) — in panel order, unranked by name; plus `groups`, the complete ordered group list |
| `POST /api/mcps` | name check → `build_def` → register + persist → optional start |
| `PUT /api/mcps/{name}` | `unmask_body` → `build_def` → `make_adapter` → persist (a config MCP as an override) → `update_def`. A stopped MCP stays stopped |
| `DELETE /api/mcps/{name}` | a config MCP is first removed from `gateway.config.json` (touching only that key; otherwise it resurrects at boot) → registry delete → store remove → tunnel links forgotten → OAuth cleared |
| `POST …/start`, `…/stop`, `…/restart` | the action, persisted as enabled/disabled |
| `POST …/rename` | registry rename → OAuth re-key → store rename → tunnel links follow |
| `GET …/details` | state, reason, stderr, the masked config, and the tunnels this MCP depends on (listed on the same screen when health fails) |
| `GET/DELETE …/calls`, `GET …/calls/{seq}` | §mcp.calls |
| `GET …/tool-history?tool=&q=` | a tool's latest 300 runs (`TOOL_HISTORY_MAX`), `q` matched against the full recorded arguments |
| `POST …/call`, `POST …/resource` | panel trial runs over a throwaway in-memory session (`introspect.rs`); errors come back as results; the gateway token never reaches the browser; counts as activity |
| `POST …/tools/{tool}`, `POST …/resources-toggle` | §mcp.registry toggles |
| `GET …/{kind}` | §mcp.registry paging, with `disabledTools`/`resourceEnabled` for the switches |
| `…/revisions…`, `…/replace` | §mcp.revisions |
| `POST/GET …/authorize` | §mcp.oauth |
| `POST /api/mcpdefs/import`, `POST /api/mcpdefs/test` | §mcp.import |
| `GET/DELETE /api/traffic`, `GET /api/traffic/{seq}` | §mcp.traffic |

`/api/mcps/{name}/{kind}` is one wide route with a kind whitelist, a literal translation of the
original route shape.

### §mcp.import — Import and the connection test

Both live under `/api/mcpdefs/`, not `/api/mcps/`: a static segment there would shadow the
`{name}` routes, and an MCP named `import` or `test` could no longer be edited or deleted.

- **Import** (`mcp_import.rs`) reads a client's config — Claude Code's `mcpServers`, Cursor's
  `servers`, or a bare name → entry map. `stdio` entries become `proc`, `url` entries `http`;
  a clashing name gets `-1`, `-2`; an entry pointing back at this gateway (a loopback host on
  the gateway's port) is skipped. Imported MCPs are added, not started.
- **Test** checks a def without saving it, capped at 5 s: a DB ping, the http handshake, or
  one plain GET for rest — never a tool, which might be billed. It expands references strictly.

### §mcp.calls — The call log and the Logs tab

**Storage** is specified in §formats.logs: an index line per call with a 2 KB inline preview,
the full reply in `bodies/` when it did not fit (the latest 50 kept per MCP), argument keys that
look secret masked by the shared `mask::is_secret_arg_key` vocabulary, a byte budget plus a
180-day age limit swept hourly. The log is an instance held by the app context. A failed write
warns at most every 30 s; it never fails the call.

**API.**

- `GET /api/mcps/{name}/calls?page=N&q=TEXT` — 20 rows a page (`CALLS_PAGE_SIZE`), newest
  first, `more` set when an older page exists. `q` is a case-insensitive match over the tool
  name, the **full** stored arguments and the stored reply — not the clipped row preview —
  applied before paging, so `more` describes the filtered set.
- `GET …/calls/{seq}` — one call's whole reply, or `bodyGone` when it was pruned.
- `DELETE …/calls` — clears the index **and** the stored replies.

**The Logs tab** (`logs.ts`, `detail.ts`, `run-history.ts`):

- **Search.** A debounced (300 ms) box riding `?q=`; Escape clears it; the live input node,
  focus, caret and IME composition survive every repaint. The empty states stay distinct: no
  calls yet, nothing matching `"X"`, past the end.
- **Paging is a transaction.** `callsPage` is the page on screen. A switch leaves the rows in
  place marked `aria-busy`, disables both directions and reads `Page N · Loading…`; a success
  commits page, rows and `more` at once; a failure keeps the committed page, its open rows and
  its scroll, and offers `Could not load calls.` + Retry for the same target. A request
  generation drops any response overtaken by a newer page, query or MCP. Only a first load
  with nothing to show uses the bare `Loading calls…`.
- **The pager is the scroll anchor.** After a committed switch the pane scrolls by the pager's
  displacement (`scrollTop += newTop − oldTop`), so the button stays under the pointer; a
  keyboard switch returns focus to the same direction (or the other one at an edge). No
  `scrollIntoView`.
- **History pages hold still.** Page 0 polls every 6 s (and repaints only when its signature
  changed); a page > 0 does not poll, and returning to page 0 fetches at once.
- **Clear** lives behind the toolbar's ⋯ (`More log actions`) as the last, danger item
  `Clear logs…`, behind a confirm naming both costs: *Clear all recorded tool calls for
  "<name>"? This removes the call history and stored full replies. The MCP configuration is not
  changed.* Cancel sends nothing.
- **Pager semantics.** `role="navigation"`, a polite live status that does not re-announce an
  unchanged poll; Newer always points at page 0.

**The JSON viewer** (`ui/json-view.ts`). A call's arguments and reply each render as **one
formatted code block**, the same for every MCP — no SQL- or redis-shaped special cases.

- A value is `JSON.stringify(v, null, 2)`; one whose compact form fits 80 characters with no
  line-breaking string prints on one line.
- **JSON inside a string is shown as JSON**, as many layers as the wire stacked, behind a
  `decoded` / `decoded ×N` marker (its words in `::before`, so a selection never picks them
  up); only `{`, `[` and `"` qualify, so `"50"` stays a string.
- **JSON followed by prose** keeps the JSON formatted and the text in a plain block below.
- **Everything else is shown as it arrived**: text, markdown, a preview cut mid-value, and every
  error (in red).
- A string's `\n` prints as a real line break; every other escape stays escaped.
- **Copy** (visible) writes valid indented JSON of the decoded structure; **Copy raw** (behind
  the block's ⋯) writes the stored text byte for byte. Both read the call row, not the DOM.
- No inner scroll box: past 200 lines (`JV_LINES`) a block ends in `Show all N lines`, a choice
  kept as state across repaints; the cap also bounds what a 1 MB body can build.
- **Opening a clipped row fetches its whole reply once**; a pruned reply is said in place.
- Keys, strings, numbers, literals and punctuation take five muted `--syn-*` tokens — the one
  sanctioned use of colour as information in a content block (§panel.design).

Logs, Traffic and run history all paint on the library's one event list (`ui/timeline.ts`,
§panel.ui).

### §mcp.traffic — The traffic ring

Every request on `/mcp/*` is recorded: token, client name and version (remembered per token
from `initialize`), method, redacted params (≤ 512 B), body and response (≤ 8 KB each). Reads
come from an in-memory ring of the newest **500**; `logs/traffic.jsonl` is the recovery tail,
restored before the listener binds, so a restart does not wipe it (ADR-005, §formats.logs).

- `GET /api/traffic?page&pageSize&mcp&client&method&actions` — 20 a page by default.
  `actions=1` keeps user actions (`tools/call`, `resources/read` and the like) so the handshake
  does not flood the view; `totalUnfiltered` gives the full count. **Rows carry no body** —
  200 rows × a 6 s poll was once megabytes a minute; a row's body comes from
  `GET /api/traffic/{seq}` when it is expanded.
- `DELETE /api/traffic[?client=]` clears everything or one client. Clearing the log does not
  forget the token's client identity.
- **Clients** (`traffic_clients`) fold the whole ring, not the page: label, tokens, MCPs
  visited, counts, last seen; labels upgrade mid-stream (`token X` → `Claude Code 1.x`).

### §mcp.catalog — The connection catalog and the tunnel link

- **Catalog.** `RegistryCatalog` registers each entry's browsable half as a capability in the
  host catalog (§host.seats). The Data plugin takes a **lease** per request; a non-browsable
  entry answers NotBrowsable naming its type (`MCP 'x' (fake) has no database to browse`). On
  stop the plugin withdraws, drains, then closes pools, so a disable never pulls the floor from
  under a Data page mid-request. Without the Data plugin, `/api/db` is an honest 503.
- **Picker order.** Data's connection picker ranks by the sidebar's **visual** order — the flat
  manual order sliced by group, unassigned names in the first group — read live per request,
  so a drag reorders the picker on the next poll.
- **Tunnels.** Interop runs only through `src/mcp_link.rs`: the registry implements the tunnels
  side's read-only `McpView` and `McpDisplay`, so neither crate depends on the other (§tunnels).
  MCP details, rename and delete consult `TunnelLinks`.

### §mcp.panel — The Servers, Traffic and Token pages

**Servers** (`views/mcps.ts` with `pane.ts`, `detail.ts`, `run-history.ts`, `logs.ts`,
`polling.ts`, `add-sheet.ts`, `sidebar.ts`):

- **Polling.** `GET /api/mcps` every 6 s patches the sidebar and the detail header only — never
  a form mid-entry (`paneHasFocus`). The top-bar chip reads `N MCPs · N on · N failing`.
- **Sidebar.** Groups in stored order (§host.groups, §panel.groups); a row renders under its
  group, or the first group; the group header's + opens the add form preset to that group; a
  right-click opens the row menu (§mcp.revisions). A type chip shows an icon where one is
  mapped (`TYPE_ICONS`, a whitelist; `typeTagHtml(tag)` renders it): brand marks from Simple
  Icons (CC0), the mysql dolphin from devicon (MIT), the mariadb sea lion, a "Z" glyph for
  zai-vision, and generic glyphs for http, rest, proc, npx and uvx (globe, plug, terminal,
  package). Anything else falls back to the text tag; the `aria-label` always carries the word.
- **Detail header.** Title, the `/mcp/{name}` mount path, status dot, latency, since-time; a
  stopped MCP reads `disabled`; an OAuth MCP adds its auth badge and an Authorize/Reauthorize
  button ahead of the primary. The primary is Disable/Enable (blue only for Enable). The ⋯
  menu: Connect a client (Claude Code command, Codex command, `.mcp.json` entry, endpoint URL —
  every URL under `/mcp/`, carrying the selected token), the group picker, Restart,
  Disable/Enable, Edit configuration…, Rename…, Delete (red, isolated at the bottom).
- **Tabs.** Tools, Resources, Prompts (with counts, paged, per-tool switches, the resources
  master switch), Run, Config, Logs (§mcp.calls).
- **Run.** Pick a tool, edit JSON arguments, run; a backfill list offers that tool's recent runs
  (searchable over the full arguments).
- **Config.** Fields by type; credentials show the sentinel and an unchanged sentinel keeps the
  stored value; Test connection for testable types; Replace… with the parked-revision list
  (Restore, Delete, each confirmed). **The pg form** splits the url into host, port, user,
  password, database and options fields while the def keeps the url string: `${…}` references
  are protected by placeholders, the mask sentinel passes through untouched, and a url that will
  not parse falls back to the raw field (`__pgRaw`). Save, Replace, Add and Test connection
  share one parse/serialise pair.
- **Add.** Name, Type, the per-type fields, *Start it now*, Import `.mcp.json`, Test
  connection. The same sheet serves the group forms.
- **Delete** says, for a config MCP, that `gateway.config.json` will be edited too.

**Traffic** (`views/traffic.ts`, `traffic.ts`): the Clients summary (click to filter) above the
Activity log (Actions filter on by default, paging, expand to fetch the body). A signature
check skips the repaint when nothing changed, so expansions and scroll survive the poll. Clear
clears the selected client, or everything.

**Token** (`views/tokens.ts`): the named tokens (id, label, created), create, rotate, revoke;
*Use* picks the token the copy snippets embed (remembered id → the default label → the first).

## §data — The Data plugin

A DBeaver-style database browser that **owns no connections**: every `/api/db` request rents
one from the MCP plugin through the host's connection catalog (§mcp.catalog). It browses
tables and rows, runs a SQL console, commits buffered edits in one transaction, imports and
exports, and browses Redis keys, values and streams. Code: `crates/swiss-data`
(`dbbrowser_api.rs`, every route), the browser model in `crates/swiss-host/src/dbbrowser.rs`
(traits, shapes, SQL builders, clamps), the three browsers in
`crates/swiss-mcp/src/adapters/{mysql,pg,redis}_browser.rs`, and the panel's `data-*.ts` and
`db-state.ts`.

**Descriptor.** Id `data`; page `data` (order 40, `workspace` layout — a full-bleed body under
the context bar); route `/api/db`; `requires: ["connection-catalog"]` — a capability, not a
plugin id, so the Plugins page shows the dependency unmet while MCP is off. Zero config and zero
disk; `start` and `stop` are no-ops. There are no background tasks and no eager resources: the
only things Data holds are request-scoped leases, which die with their requests.

**Leases.** Every handler first checks the catalog (serving / stopping / absent), then calls
`catalog.lease(name, "data")`; the lease counts +1 on grant and −1 on `Drop`, so there is no
"remember to return it" API to forget. The provider's stop is withdraw → drain (5 s,
`DRAIN_TIMEOUT_MS`, an honest warning naming the unreleased count on timeout) → close
(§mcp). Browsers are minted on demand and share the MCP tools' own `Arc<Lazy<Pool>>` — one pool
per connection serves tools, resources, the ping and the Data page; a second pool is never
built.

**Connection identity.** A connection is a registry entry and its key is the entry's name. Two
definitions sharing host and port but differing in credentials are two entries and never merge
(a test pins it). **There is no read-only flag**: Data is editable and queryable, and the
operator's databases are the operator's to change. An old definition may still store
`readonly`; nothing reads it and the echo drops it (§host.refs). The one read-only
surface is a table in a database other than the configured one (§data.databases). The browser
`label` names only the target (`db @ host:port`), never a credential.

**Boundary and errors.** `/api/db` sits inside the host's loopback guard, 2 MiB body limit and
plugin boundary (§host); it does no bearer auth — loopback is the gate. The error body is
`{error}`:

| Status | When | Text |
|---|---|---|
| 400 | a driver or guard refusal, a bad parameter | the driver's message **verbatim** — "Data too long for column 'x'" is exactly what a grid user needs |
| 404 | unknown name; not a database | `unknown MCP: <name>`; `MCP '<name>' (echo) has no database to browse`; `MCP '<name>' (mysql) is not a redis connection` |
| 409 | a buffered edit lost a race (§data.edits) | `{error, conflictColumns, row}` |
| 503 | no catalog provider, or it is stopping | names the missing party: `the mcp plugin provides database connections and is currently disabled` |

Query parameters keep the route-literal semantics: missing and empty both mean "not sent";
numbers are coerced like JavaScript's `Number()`.

**Logs.** `data view import` (info), `data view ddl` (**warn**), `data view commit` (info),
`data view redis commit` (info, with the command count), `data view activity kill` (**warn** —
the one route that interrupts someone else's work).

### §data.api — The routes

All under `/api/db`; `{name}` is the registry name.

| Route | Answer |
|---|---|
| `GET /` | `{connections:[{name,dialect,label,state,group}]}` — browsable entries only (`dialect != "none"`), in the sidebar's visual order (§panel.groups): groups in stored order, members in the manual order, uncovered names by name. The order is a closure read per request, so a sidebar drag reorders the picker on the next poll |
| `GET /{name}/tables` | `{tables:[{schema,name,type,approxRows,size}],total,page,limit,more}` (§data.browse) |
| `GET /{name}/databases` | the database catalog (§data.databases) |
| `GET /{name}/data` | `{schema,table,columns,rows,total,offset,limit,primaryKey,editable,editNote?,nextPage?}` |
| `GET /{name}/schema` | `{schema,table,columns,primaryKey,indexes,foreignKeys,ddl}` |
| `GET /{name}/export` | an attachment (§data.export) |
| `POST /{name}/import` | `{inserted}` (§data.export) |
| `POST /{name}/ddl`, `/ddl-preview` | `{ran}`; `{sql}` (§data.ddl) |
| `POST /{name}/query` | `{columns,rows,rowCount,limitApplied?,note?,elapsedMs}` (§data.console) |
| `POST /{name}/edits` | `{results:[{op,affected,row?}]}` — at most 1000 edits (`MAX_EDITS`) (§data.edits) |
| `POST /{name}/completion` | `{items:[{label,kind,detail}]}` (§data.completion) |
| `GET /{name}/activity`, `POST /{name}/activity-kill` | §data.activity |
| `GET /{name}/keys` | `{keys:[{key,type,ttl,binary?}],cursor,done,total}` (§data.redis) |
| `GET /{name}/key` | the `redis_read` shape; a stream key answers the newest window |
| `POST /{name}/command` | `{reply,elapsedMs}` (§data.redis-console) |
| `POST /{name}/redis-pipeline` | the replies of one pipelined round trip (§data.redis) |
| `GET /{name}/redis-commands` | the server's own command catalog (§data.redis-console) |
| `GET /{name}/stream`, `/stream/groups` | §data.streams |

### §data.browse — Tables and rows

**The table list.** `GET /tables` takes `grep`, `page`, `limit`, `sort`, `dir`. The browsers
clamp `limit` to 200 by default and **5000** at most; the panel fetches the whole catalog at
once (`DB_TREE_FETCH_LIMIT = 2000`, §data.tabs). `sort` is `name`, `rows` or `size`, `dir` is
`asc`/`desc`; missing means name ascending and an unknown key or direction is a **400 — a sort
is never silently dropped**. The sort is an `ORDER BY` inside the adapter's own list statement,
never in memory: MySQL orders by the statistics it already computes with the name as an
ascending tiebreaker; pg orders by `lower(relname)`, `approx_rows NULLS LAST` or
`pg_total_relation_size`, so never-analysed tables sit at the bottom. The MCP tools keep their
standing order — only the browser passes a sort.

**The grep grammar.** A comma is AND, `|` is OR, `*` is a wildcard, matching is
case-insensitive; each piece becomes a bound `LIKE` pattern, at most 32 per request
(`BROWSE_GREP_MAX`).

**Rows.** `GET /data` takes `table`, `schema`, `offset`, `limit`, `order`, `dir`, `filters`.
Page sizes are 10/20/50/100/200/500, default 50, cap 500. The page asks for `limit + 1` rows and
answers `nextPage` from the extra one. Sorting and filtering run on the server.

**Filters.** `filters` is a JSON array of `{column, op, value}`, at most 16 (`MAX_FILTERS`).
Thirteen operators (`BROWSE_FILTER_OPS`): `eq ne gt gte lt lte in notIn between like notLike
isNull isNotNull`; `in`/`notIn` take at most 100 values (`BROWSE_IN_MAX`). Values are always
bound parameters; a column not in the table or an unknown operator is refused while the
`WHERE` is built. The same `browse_where` builds the rows and the export, so an export carries
the grid's filters. A JSON comparison on MySQL binds as `CAST(? AS JSON)`.

**Numbers.** An integer a double cannot hold exactly (beyond 2^53) travels as a string, both
ways — a snowflake id is never rounded on its way to the grid or back into a filter or an edit.

**pg schemas.** pg takes a `schema` parameter on every table route; MySQL's `schema` is the
database (§data.databases).

### §data.databases — The database axis (ADR-027)

`GET /databases` answers
`{primary, current, databases:[{name,primary,browsable,system,tables?,reason?}]}`; the trait's
default is an empty catalog.

- **MySQL** lists every database on the instance. The `schema` parameter names the database;
  it is checked against that list **before any SQL is built**, then quoted with `quote_ident`,
  and applies to `/tables`, `/data`, `/schema` and `/export`. The write paths stay on the
  configured database: a secondary database answers `editable:false` with the editNote
  `Read-only: <db> is not this connection's configured database (<primary>).`
- **pg** browses only the connection's own database; the others are listed with a `reason`.
- **Redis** reads `INFO keyspace` and `CLIENT INFO`; the current db is always listed, even when
  empty, and a proxy that refuses `CLIENT INFO` falls back to db0.

System databases are flagged and sorted last. Switching database resets the tables and tabs,
behind the pending-edits confirm.

### §data.grid — The grid, form and value views

- **Headers** carry the type, a PK marker and the column comment; a click cycles
  asc → desc → none (server-side). `NULL` is drawn explicitly.
- **Widths and hidden columns** persist in localStorage under
  `swiss.dbGrid.<conn>_<schema.table>`; widths are clamped to 48–1200 px.
- **Keyboard.** Arrow navigation, TSV paste into a range, Ctrl+C copies rows. Right-click copies
  a cell, or a row as JSON/CSV/INSERT (dialect-aware quoting, buffered values win); checked rows
  copy as CSV/TSV/Markdown/JSON. The focus ring moves between the live cells without rebuilding
  the grid, and a cell mousedown keeps focus on the keyboard layer, so the click and double-click
  that follow still land on the cell they edit.
- **Cells** are type-aware (`dbCellView`): JSON longer than 100 characters folds, a URL is a
  link, numbers use tabular figures, booleans read `TRUE`/`FALSE`, binary is marked.
- **The value sheet** shows a value as a JSON tree, text, a link, or hex (the first 512 bytes).
- **Form view** shows one row as a form; **FK jump** opens the referenced table in a new tab
  with the filter preset.
- **Column stats** ("Value distribution…", "Numeric stats…") and **filter by this value** come
  from the cell menu; the stats run through `/query` with whitelisted identifiers.
- **Structure** is a tab with Columns / Indexes / Foreign Keys and the DDL (MySQL
  `SHOW CREATE TABLE`, pg assembled from the catalog); the panel re-lays CREATE TABLE out
  aligned.

### §data.edits — Buffered edits

Every change — a cell, a new row, a delete — goes into a client-side buffer first; the amber
bar counts it and says it is local only. "SQL" previews exactly the statements Commit will run;
"Commit (1 transaction)" POSTs `/edits` after a confirm; "Discard" is a pure client-side drop.

- **One transaction.** The server runs the batch on one connection and rolls the whole batch
  back on the first error. Each edit reads the stored row back **in the same transaction**, so
  the grid shows what the database kept (defaults, triggers, truncation).
- **Optimistic concurrency.** An update carries the row as it was read plus the diff; if the
  row moved underneath (affected = 0), the whole batch rolls back with a **409 naming the moved
  columns**, the cell turns red and the buffer is kept.
- **Rows without a primary key** are addressed by every column; a value over 64 bytes is
  compared by `MD5(col)`. MySQL adds `LIMIT 1`; pg rolls back if more than one row would be
  affected.
- **Read-only** connections and secondary MySQL databases refuse edits; the editNote says why in
  the server's words.

### §data.console — The SQL console

- **One statement per request** (`assert_single_statement`) is the console's one rule. Reads
  and writes alike go through: the console belongs to the operator's own machine, and no
  statement-shape classification refuses a write.
- **Automatic LIMIT.** A statement without one gets `with_row_limit`: the open table tab's
  page size, or 50; at most 10 000 (`MAX_ROW_LIMIT`), reported back as `limitApplied`/`note`.
  The reply carries `elapsedMs`.
- **Blocks and batches.** Ctrl+Enter runs the blank-line-separated block under the caret
  (`dbSubqueryAt`). A block is split on statement-level semicolons and sent **one statement per
  request**, each answer its own result tab labelled `SELECT · 42`; the first failure stops the
  batch and keeps the tabs that answered.
- **Explain.** Explain and Explain Analyze prefix each statement (idempotently); the driver's
  own error is passed through. A plan is not history.
- **History and favourites** live in localStorage (`swiss.dbSqlHistory`, 50, only whole
  successful runs; `swiss.dbFavorites`, 50). **Format** is whitespace only.
- **Templates.** Generate SELECT / INSERT / UPDATE / DELETE for the open table.
- A statement that touches the schema drops the completion cache.

### §data.completion — Server-side completion (ADR-017)

`POST /completion {sql, caret}` → `{items:[{label,kind,detail}]}`: keywords (a core list plus
pg and MySQL extras), table names, and the columns of the FROM-nearest table, for the word
ending at the caret. Columns come from a lazy per-connection cache (10 minutes, 64 KB budget)
that a schema-touching statement invalidates. The panel asks 150 ms after a keystroke that ends
a word and shows at most 8 items.

### §data.activity — Live sessions

`GET /activity` lists the server's sessions in one shape for both dialects (pg
`pg_stat_activity` with `pg_blocking_pids`, MySQL's processlist). `POST /activity-kill
{pid, mode}` cancels (`pg_cancel_backend` / `KILL QUERY`) or terminates (`pg_terminate_backend`
/ `KILL`); mode and pid are validated and the kill is logged at warn. The Activity tab polls
every 5 s while active.

### §data.export — Export and import

- **Export** (`GET /export`, `format=csv|json|sql`): CSV (RFC 4180), NDJSON, or a **streamed
  SQL dump** — CREATE TABLE, foreign-key checks off, multi-row INSERTs batched up to 1 MB
  (a single larger row ships alone), the dialect's foot — sent through `Body::from_stream` with
  the lease riding inside the stream. Every format stops at 100 000 rows (`EXPORT_ROW_CAP`),
  read in 5 000-row chunks; the headers `x-export-rows`, `x-export-capped` and
  `x-export-format` say what was saved. The export carries the grid's filters.
- **The panel's CSV** button exports the current page or result client-side (CSV + BOM).
- **Import** (`POST /import {table, schema, header, lines, mapping}`): at most 10 000 rows
  (`IMPORT_ROW_CAP`), one transaction. The wizard: paste or upload → map each column (by name
  by default, skippable) → preview three rows → confirm. Numbers bind as numbers only when
  `Number()` is lossless. **Upsert** uses MySQL `ON DUPLICATE KEY` or pg `ON CONFLICT (pk)`; a
  table without a primary key says so and falls back to plain inserts. On failure nothing on
  screen changes.

### §data.ddl — Structural operations

`POST /ddl {op, …}`: `rename`, `truncate`, `drop`, and the minimal create set `create_table`,
`add_column`, `create_index`. Rename validates `[A-Za-z0-9_$]{1,64}`; truncate and drop require
typing the table name. The New table / Add column / New index sheets show
`POST /ddl-preview`, built by **the same builder** the commit runs, so the preview is exactly
the executed SQL. Read-only connections refuse DDL. Not built: ALTER TYPE, constraints, views,
triggers.

### §data.redis — Keys and values

- **Keys.** `GET /keys` walks `SCAN` (`pattern`, `cursor`, `count` 1–1000 default 200, `type`
  from string/hash/list/set/zset/stream); TYPE and TTL ride a pipeline 200 at a time. Keys are
  de-duplicated on arrival (SCAN promises only "at least once").
- **The tree** groups keys by `:` (`redisNamespaceTree`) with single-child path compression.
  Each row shows **only the segment it adds at its level**; the full key sits in the right slot.
  A band's collapse state is keyed by its full path. Depth is capped at 32
  (`REDIS_NS_MAX_DEPTH`); deeper keys are rows of the deepest band. The collapse map has no
  prototype, so a key named `constructor` or `__proto__` behaves.
- **Names that the page would alter** (leading or trailing space, newline, zero-width, bidi
  controls, the empty name) are shown quoted and escaped like redis-cli (`dbKeyShown`) —
  display only; every action sends the key's own bytes.
- **Non-UTF-8 names.** SCAN keeps names as bytes; a non-UTF-8 name is printed the way
  redis-cli prints it and marked `binary:true`. The panel keeps it at the root, labels it,
  refuses to open it ("use redis-cli") and skips it for bulk delete and completion. There is no
  byte-addressed action channel.
- **Values.** `GET /key` reads by type (string, hash, list, set, zset; windows of 1000, hash
  and set capped at 1000 and marked `truncated`); a module type names its own commands; a
  stream key answers the newest window (§data.streams).
- **Structured edits** buffer typed changes (+Field / +Item / +Member, string SET, EXPIRE),
  preview them as commands, and commit them as **one pipelined round trip**
  (`POST /redis-pipeline`); every command passes the console guard first and the first refusal
  rejects the batch whole.
- **Key actions** go through the pipeline as argv, never as a command line: Delete (typed
  confirm), Rename (`RENAMENX`, a taken name is reported beside the input), Set TTL (0 is
  refused — use Delete; `EXPIRE` answering 0 says the key is gone). Values are submitted
  exactly, whitespace included.
- **TTL** is one readout at the head's top-right: read once with the value, then counted down
  by one shared 1 s ticker (`10s`, `1:31`, `2:05:00`); clicking it edits (Enter = EXPIRE,
  empty = PERSIST). The tree can sort by TTL but never prints it as a clock.
- **Bulk delete.** A band's ⋯ offers "Delete N keys" (`keysUnder`). `DEL` is one atomic
  command; past 1000 keys (`REDIS_DEL_CHUNK`) it splits, and past 1.5 MB of JSON per request
  it splits into several requests — the confirm says the guarantee then is one round trip, not
  one command, and a partial SCAN changes the confirm to "only the loaded keys". The toast
  reports what redis deleted, and the gap separately; a failure stops and reports what was
  done. Tabs on the deleted keys are cleared and the tree re-walked. No MULTI/EXEC: it is not a
  rollback and would promise nothing more.

### §data.redis-console — The Redis console

- **One command** per run through `POST /command`, split by `split_words` (shlex with a
  word-leading `#` kept as a character, not a comment); the guard is the MCP's redis policy
  (§mcp.db). Array replies over 1000 items are truncated and labelled.
- **The command catalog comes from the server.** `GET /redis-commands` asks `COMMAND DOCS`
  (words: summary, syntax, group, since, tokens) and `COMMAND INFO` (arity, first/last key,
  step, and redis 7 key specs) **separately** — redis 6 has no DOCS, and a pipelined failure
  would sink both — and merges them (`documented:false` when DOCS failed). Container commands
  keep their own row and flatten their subcommands (`XINFO STREAM`). The panel asks once per
  connection; nothing is cached server-side and the panel keeps no command table.
- **Completion** reads only the caret's line: word 0 completes command names (never on an
  empty prefix), word 1 of a container its subcommands, a key position (first/last/step, or the
  key specs for commands whose keys move) the keys the tree has walked, any other position the
  command's own tokens not yet typed; a value position gets nothing. A key that would not split
  back to itself is inserted quoted. Lines over 64 KB are not completed; the word-span search
  looks back at most 4096 characters.
- **The signature line** under the console shows the recognised command's
  `name syntax — summary`, else the fixed hint.
- **Templates** is its own button left of Run: 32 hand-written templates grouped string / key /
  hash / list / set / zset / stream / server.
- **One splitter.** The panel and the server split the same way; the shared corpus
  `tests/fixtures/console-split.json` is read by both test suites, with the three known
  divergences documented.

### §data.streams — Redis streams

- **Windows.** `GET /stream {key, before?, after?, count?}` reads with XREVRANGE only: newest
  `+ -`, older `(before -`, newer `+ (after` — exclusive bounds need Redis ≥ 6.2. The answer is
  `{key,type,ttl,length,firstId,lastId,entries:[{id,ts,fields}],columns,more}`, entries always
  newest first, `ts` ISO with milliseconds from the id, `columns` the union of fields in
  first-seen order, `more` = the page is full. `count` defaults to 100, max 1000; ≤ 0 or
  non-numeric, or `before` with `after`, is a 400; a non-stream key is a 400
  `X is a hash, not a stream`; a missing key is `type:"none"`.
- **Budget.** Exactly three commands per window — XREVRANGE, XLEN, XINFO STREAM — in one
  pipeline; a test pins it. `redis_read` on a stream keeps its oldest-first offset/limit.
- **The view** (`data-stream.ts`) is a read-only table (id | time | columns), no virtual
  scroll, at most 500 rows; Load earlier appends and drops from the top.
- **Follow** polls at 1, 2 or 5 s, only while the tab is active and the document visible. A
  view at the top rides the live edge; otherwise new rows pool behind an "↑ N new" pill, and
  hovering the table holds them too ("paused while reading"). A full catch-up page shows a gap
  bar with "jump to latest", issued on the same request token as the ticks. The rate is the
  delta of `length` and may be negative. A tick error stops Follow with the reason; a tick
  repaints only the table.
- **Filter.** `match` is parsed on the server (`parse_stream_filter`, shlex): `field=value`
  exact (case-insensitive value), `field~value` contains, a bare word matches the id, time or
  any value; terms AND; no OR, negation or regex; a bad filter is a 400 before any lease. A
  filtered read scans back in pages of 500 until `count` hits, 20 000 entries scanned, or the
  range ends, and adds `scanned`, `scannedFrom` (the Follow cursor) and `scannedTo` (the Load
  earlier cursor) — present only with a filter. The input survives repaints; debounce 400 ms,
  Enter applies at once; zero hits says "no scanned entries match".
- **Summary chips** (panel-only): "value N · r/s" for an auto-chosen field (the first column
  with 2–24 distinct values and distinct × 2 ≤ rows, ≥ 4 rows; sticky; the user's choice,
  including none, wins), at most 12 chips; a click sets the filter.
- **Consumer groups** are read-only (`/stream/groups`: group, consumers, pending, lag,
  last-delivered-id; lag is "—" on 6.2), fetched when the fold opens and every 5th Follow tick.
  No XACK/XCLAIM/XTRIM/XDEL — those move someone else's consumption.
- XREAD and XREADGROUP stay refused: one shared connection must never block.

### §data.tabs — Object tabs, the tree and the bars (ADR-026)

- **State.** Connection scope (`DbConnState`) is split from a tab list (`DbTab[]`, a
  discriminated union of `table`, `sql`, `key`, `activity`). One `dbOpenTab()` opens anything;
  opening an open object activates it; an FK jump opens a new tab and keeps the source.
- **Cap 12** (`DB_TAB_MAX`). Room is made by evicting the least recently used clean, inactive
  tab (the toast names it); when every tab is busy `dbOpenTab` refuses with a toast, but the
  strip's `+` always opens a SQL tab. Background tabs drop their rows and refetch at their
  offset and filters on activation, keeping edits. Closing a dirty tab confirms;
  `hasPendingChanges()`/`canLeave()` count every tab.
- **The strip** is a scroll area plus a pinned end (overflow menu, `+`); the active tab scrolls
  into view; a schema prefix equal to the current scope is dropped (the title is always full).
  The overflow menu lists every tab plus Close others / to the right / all. The wheel scrolls
  it, middle-click closes, right-click offers rename and close left/right. Card tabs are a third
  shape, distinct from underline tabs and pills (§panel.design).
- **The sidebar tree** has Tables / Views / Routines sections (collapsible groups, §panel.groups;
  `+` only on Tables); pg nests schema bands over them. Each section renders 200 rows
  (`DB_TREE_ROW_CAP`), "show N more" adds 500, "Show fewer" sits at the end; search results
  are uncapped. Connection and database pickers are drawers, one open at a time.
- **Toolbars** show only the active tab's controls, at most one non-icon button each (a test
  gates it): table = view segment + one primary + ⋯; sql = Run + ⋯; key = the type's primary;
  activity = Refresh. Refresh / CSV / Export / Import live in ⋯.
- **The status bar** — range, pager, page size, elapsed, editNote, connection — is separate
  from the commit bar.
- **Structure** folds its panes into four tabs: Data / Form / Structure / DDL.
- Not built: docking, drag-reorder, workspace persistence.

### §data.sessions — The session cache

- **Parking.** A session is the connection record, its tab strip and the active index, parked
  per connection name. Switching connection parks the current session and restores the target
  with no confirm; pending edits travel with their session. Leaving the page parks it and
  remembers the connection. `canLeave` asks about pending edits across **all** sessions.
- **Quiet re-validation.** A restored session paints at once, then re-reads the catalog and the
  front table page at their coordinates, repainting only if the answer changed; the front key's
  value too, unless it has pending edits. The Redis key list is re-walked quietly to the same
  depth on restore, on `r`, on switch-back and after each console command. Console results are
  never re-run.
- **Limits.** At most 8 parked sessions (`DB_PARKED_MAX`), evicting the least recently used
  clean one — a session with pending edits is never evicted; sessions of vanished connections
  are dropped. Parking stops the Activity poll.
- Not built: a (connection, database) key — switching database still resets; nothing survives
  a reload.

## §tunnels — The Tunnels plugin

SSH connections and the local port forwards that ride them: each rule forwards
`127.0.0.1:localPort` over SSH to `targetHost:targetPort`. Enabled rules stay up, reconnect
with backoff, and hold their local port **only while the tunnel can carry traffic**. A
connection can be dialled through an HTTP CONNECT or SOCKS5 proxy, or through another
connection as a jump host. The same connections are the terminal's remote hosts and the remote
plugin's build servers. Code: `crates/swiss-tunnels/src/tunnel/` (`types`, `store`, `ssh`,
`proxy`, `forward`, `manager`, `mcpmatch`, `port`, `import`, `shell`, `remote`, `api`), the
descriptor in `src/builtin.rs`, and the MCP glue in `src/mcp_link.rs`.

**Descriptor.** Id `tunnels`; pages `tunnels` ("SSH Connections", order 30) and
`tunnel-forwards` ("Port Forwards", 35), two sibling pages under one rail seat — `#tunnels`
keeps meaning SSH Connections; route `/api/tunnels`; no config rows; no `requires`.

**Start.** (1) On the first run (no `tunnels.json`) import a previous forward-port tool's
config from `%APPDATA%/forward-port/config.json`, every rule as `enabled:false` — the old tool
may still hold the ports; the import lives inside start, so a disabled plugin never touches the
file. (2) `manager.reopen()` lowers the close gate synchronously, so a Start click racing the
boot task is not refused. (3) A scoped task starts the enabled rules; each failure is a warning.
(4) Register the remote transport, then the shell provider, **last**; if either fails, nothing
is left registered.

**Stop.** The remote transport withdraws first (no drain — a remote exec is a run with its own
deadline and cancel, §remote); the shell provider withdraws and drains its sessions briefly,
naming how many were closed; `close_all` ends every forward and client (each `end()` capped at
1.5 s) and releases every port; the providers are cleared. **The enabled flags are untouched**,
so the next start restores exactly the set that was running.

**Ordering.** Tunnels starts before the MCP plugin and stops after it, so an MCP that rides a
tunnel finds it up (§host.lifecycle).

### §tunnels.api — The routes

The tree mounts inside the host's loopback guard and plugin boundary; the boundary is the gate.
Errors funnel through one function: a guard conflict is **409**
`{error, dependents, confirmRequired:true}`; a message starting with `unknown ` is 404;
anything else is 400. Request bodies coerce like JavaScript's `Number()`; `?force=1` and
`{force:true}` are both accepted.

| Route | Answer |
|---|---|
| `GET /api/tunnels` | `{connections, rules, ruleGroups, connGroups, mcps}` — the live rows plus groups and MCP names |
| `GET /keys` | `{keys:[{path,name}], defaultPath}` — `id_*`, `*.pem`, `*.key` under `~/.ssh` |
| `GET /browse?dir=` | `{dir, parent?, entries:[{name,path,dir}], error?}` — directories first; a read failure is the `error` field |
| `GET /suggest/{port}` | `{port, mcps:[name]}` (§tunnels.mcp) |
| `POST /connections`, `PUT /connections/{id}`, `DELETE /connections/{id}` | 201 `{connection}` (masked); `{connection}`; `{id, deleted:true}` — a connection still carrying rules or serving as a jump host is refused, naming them |
| `POST /connections/{id}/test` | `{ok, ms, banner?, error?, kind?, fingerprint?, hostKey?}` |
| `POST /connections/{id}/trust` | `{id, hostKey}` — accept the presented key, or clear it to relearn |
| `POST /rules`, `PUT /rules/{id}`, `DELETE /rules/{id}` | 201 `{rule}` (`start:true` starts it; a failed start does not undo the save); `enabled` is not accepted from the client |
| `POST /rules/{id}/start`, `/stop` | `{rule}`; a failed start is **a result, not a transport error**: 200 `{rule, ok:false, error}` |
| `POST /start-all`, `/stop-all` | `{results:[{id,name,ok,error?}]}` — started concurrently, grouped by connection |
| `GET /port/{port}` | `{port, free, owner|null}` — a real bind probe, the holder from `netstat -ano` + `tasklist` (about 4 MB and 40 ms, against PowerShell's 65 MB and 350 ms) |
| `POST /port/{port}/free` | `{port, killed:{pid,name}}` — `taskkill /F`; refuses pid 0 and the gateway itself; Windows only |

Grouping and order go through the host's one groups family (`/api/groups/{conns,rules}`,
§host.groups).

### §tunnels.store — Connections, rules and tunnels.json

`tunnels.json` is a sealed state file (§formats.sealed):
`{connections, rules, ruleGroups, connGroups}`. **Key order is the file format** —
serialisation is hand-written, camelCase, absent rather than null.

- **Connection:** `id, name, host, port, username, authType` (`key`|`password`), then the
  optional `keyPath, passphrase, password, hostKey, group`, then the proxy and jump fields
  **appended** in the order `proxy, proxyUsername, proxyPassword, jump`. An unset field keeps
  the old key set exactly; a test pins both halves.
- **Rule:** `id, name, connectionId, localPort, targetHost, targetPort, remark,
  autoReconnect, reconnectInterval, enabled, mcps`, then `group`.
- **Validation** (`valid_conn`, `valid_rule`): ports are integers 1–65535; the gateway's own
  port is refused as a local port; ids are unique; local ports do not collide;
  `reconnectInterval` NaN or 0 becomes 10, with a floor of 1.
- **Proxy and jump validation** (English messages):
  - `proxy` is `http://` or `socks5://` `host[:port]`; a portless URL is saved normalised
    (http → 80, socks5 → 1080). `socks5h://` and `https://` get their own messages (swiss always
    lets the proxy resolve names; TLS to the proxy is not built). userinfo, path and query are
    refused: `proxy URL must not carry credentials; use the proxy username and password fields`.
  - `jump` must name another existing connection; itself is refused, and a cycle is refused
    naming the chain: `jump cycle detected: A -> B -> A`.
  - A connection may have a proxy **or** a jump, not both: `put the proxy on the jump
    connection if it needs one`.
- **Tolerant load.** A corrupt file is a warning and empty tables. A row with an empty name or
  host or a bad local port is dropped, but **a rule with an unknown connection is kept** and
  shown as an error — one bad hand edit never eats the other rows.
- **Groups.** `default` is an ordinary, explicitly listed name (a pre-groups file has it
  materialised); rename migrates members; deleting a group moves its members to the **first**
  group; order is array order (§host.groups).
- **Host keys.** An edit that keeps host and port re-adopts the stored key, so editing never
  silently drops trust; `trust` and the clear path are explicit.

**Credentials.** `password`, `passphrase`, `proxyUsername` and `proxyPassword` hold a literal
or a whole-field reference (`${ENV}` or `${secret://name}`, §host.vault); the reference is what
lands on disk, and it resolves **at connect time**, strictly — a missing reference refuses the
connection before any socket opens, naming the reference. The API masks secrets outbound and
restores the sentinel inbound (`mask_conn`/`unmask_conn`), so editing an unrelated field never
destroys a stored secret; `proxyPassword` rides the rows as the sentinel unless it is a
reference, which is not a secret and passes as itself. The rows also carry `keyPath`, so a save
never rewrites a custom path to the default.

### §tunnels.ssh — The SSH client

- **One client per connection, reference-counted** (`acquire`/`release`): 14 rules on one host
  dial once; forwards, terminal sessions and remote execs share it; `end` happens when the last
  user leaves. Dialling is single-flight.
- **Budget.** TCP + key exchange + auth (and any proxy handshake or jump chain) fit in one 15 s
  budget (`READY_TIMEOUT`). Keepalive 15 s × 3 detects a NAT-dropped link in about 45 s; a
  watcher per client reports the loss with a generation, so a stale report is ignored.
- **Auth.** `key` (a private key file, `~` expanded, an optional passphrase) or `password`.
- **Host keys are TOFU.** The fingerprint (`SHA256:…`, OpenSSH format) is learned on the first
  connect; a changed key is refused with the expected and actual fingerprints, and the panel
  offers Trust. A Test on a connection without a stored key stores nothing.
- **Test** is a throwaway handshake (`{ok, ms, banner, …}`) through the same dial path.
- **Failure kinds** (`FailureKind`): `auth`, `hostkey`, `config` are **never retried** (retrying
  a bad password invites fail2ban); `network` and `port` are. auth and hostkey put the rule in
  `error`, not `reconnecting`.
- The PTY requests `TERM=xterm-256color` (§terminal).

### §tunnels.forward — Forward rules

- **Start** binds the local port, acquires the client and runs the accept loop (at most 200
  sockets per rule); each socket is pumped to a `direct-tcpip` channel with byte counts; TCP
  keepalive 30 s. A successful start persists `enabled:true`.
- **Stop** closes the listener and every socket and verifies the port can be bound again;
  `persist` decides whether `enabled` is cleared.
- **Per-rule serialisation.** Every operation on a rule holds that rule's async mutex for the
  whole operation; run state is snapshotted under a short lock, and **no lock is held across an
  await**.
- **Loss and recovery.** When a client dies, each affected rule **releases its port first, then
  reports** `reconnecting` with the reason; recovery runs through each rule's own queue; a user
  stop racing it wins (re-checked under the lock).
- **Backoff.** `interval × 2^(n−1)` with the exponent capped at 8, capped at 5 minutes,
  ±20 % jitter; a success resets the count. Retry timers hold weak references and are
  abortable.
- **Edits.** Editing a connection stops its running rules, swaps the client and restarts only
  those that were running; editing a connection used as a jump host reconnects its dependants.
- **A stale listener** of another rule in this gateway holding the port is torn down through
  that rule's own queue, and the start is retried once. A foreign holder is reported
  (`portOwner`), and the panel offers **Force free**.
- **Rows** carry `sockets`, `bytesIn`, `bytesOut`, `channelFailures`, `lastError`, and the dot
  state: connected/up green, connecting/starting/reconnecting yellow, error red, idle/stopped
  grey.

### §tunnels.proxy — Proxy dialling

`proxy.rs` dials the proxy itself and hands the stream to russh's `connect_stream`; no new
dependency.

- **Order.** Resolve the credential references first (a failure is `config`, no dial, the
  reference named); each resolved value is at most 255 bytes (RFC 1929). Then connect to the
  proxy with `TCP_NODELAY`.
- **HTTP CONNECT.** `CONNECT host:port HTTP/1.1`, `Host:`, and `Proxy-Authorization: Basic`
  when credentials are set; a non-2xx status is `proxy <h>:<p> refused CONNECT: HTTP <code>`.
  The response head is read under an 8 KB cap.
- **SOCKS5.** Offer no-auth, plus username/password when set; the RFC 1929 sub-negotiation must
  answer `[01 00]`. CONNECT sends the **target name unresolved** (ATYP 03 — socks5h
  semantics; an IP literal uses ATYP 01/04). A non-zero reply is `socks5 reply <rep>`.
- **Credentials reach the handshake only as components.** No code path builds a URL containing
  a password; the dialler is the only consumer and does not take URLs.
- **Every proxy failure is constructed as `network`** — never classified from its message — so
  it is retryable and auto-reconnect applies.

Not built: ssh-agent auth, `-D` dynamic forwarding, `ProxyCommand` and `~/.ssh/config`
(never: a subprocess and an unbounded grammar), TLS to the proxy, SOCKS4, proxy chains, per-hop
timeouts, and socks5 for the MCP http adapters.

### §tunnels.jump — Jump hosts

`jump` is another connection's id, and the transport is recursive — OpenSSH `-J`'s per-hop
model:

```
transport(conn) = conn.jump ? live(jump).open_channel(conn.host, conn.port)   // SSH over SSH
                            : conn.proxy ? proxy::dial(conn) : TcpStream
```

- Each hop is an ordinary connection with its own auth, TOFU, Test and row; the target's
  `hostKey` is the final server's. A proxy for a hop lives on that hop.
- **Holds.** Each connection holds its direct jump host; stopping the target returns every
  hop's reference count to zero (a test pins it on a three-hop chain). Test builds a private
  throwaway chain.
- **Failures.** A dead jump kills the target's channel; the target reports `network`, releases
  its ports, and reconnects by re-evaluating the whole chain. A hop's auth or hostkey failure
  keeps its kind and is prefixed `via <jump-name>: …`.
- Deleting a connection used as a jump host is refused, listing its dependants; cycles are
  refused at save time, so the runtime never sees one.

### §tunnels.mcp — MCPs that ride a tunnel

- **Suggest.** `GET /suggest/{port}` is pure definition analysis (`mcpmatch.rs`): mysql and
  redis read host/port (defaults 3306/6379), pg parses its connection string; references are
  resolved first — an unresolvable one does not match. proc and echo never match.
- **Links.** A rule's `mcps` names the MCPs it serves; rows carry `mcpRows`
  (`{name,state,known}`), and an MCP started before the tunnel's last reconnect is flagged
  `stalePool`. The MCP detail page lists its tunnels.
- **The guard.** Stopping or deleting a rule that a **started** MCP depends on, or stop-all, is
  409 `Dependents`; the panel lists them and resends with force.
- **Renames.** An MCP rename migrates the links and a delete clears them.
- **No dependency cycle.** swiss-tunnels defines two narrow traits (`McpView`, `McpDisplay`);
  the registry-backed implementations live in the composition layer (`src/mcp_link.rs`), so
  neither plugin crate depends on the other (§arch).

### §tunnels.providers — Shells and remote execution

The tunnels connections are the **one** host inventory. `TunnelShells` implements the host's
shell capability for the terminal (§terminal): PTY sessions take a reference on the shared
client, and credentials never leave `ssh.rs`. `TunnelRemote` implements the remote transport
for the remote plugin (§remote): exec requests over the same clients, POSIX-quoted here where
the SSH string is built, returning events and exit codes only.

### §tunnels.panel — The pages

- **Two pages over one scope switch** (`mountTunnelsPage("conns" | "rules")`), grouped cards
  with collapse state, drag to reorder and regroup through the groups family; a poll patches
  dots, reasons, buttons and counts in place and redraws only when the row set changes, never
  while dragging.
- **Rule rows**: an inline Start/Stop, ⋯ with Edit, Copy local port, Force free (only with a
  `portOwner`), Delete; a 409 lists the dependants and confirms before the forced resend.
- **Connection rows**: Test (a hostkey failure offers Trust, then retries); ⋯ Edit, Copy host,
  Delete. Badges: `proxy`, `via <name>`.
- **The connection sheet**: key auth shows the key path (prefilled from the row) with Browse…
  and the passphrase; password auth swaps in a password. An **Advanced** `<details>`, closed by
  default, holds Proxy (URL, username, password) and Via connection (a jump picker excluding
  itself); its summary carries `proxy` / `via <name>` chips when set, so closed never means
  hidden. The backend is the one judge of cycles; its 400 shows inline.
- **The rule sheet**: changing the local port asks `/suggest` and pre-checks the matching MCPs;
  `mcps` is sent only when the panel means it (omitted keeps, `[]` clears).

## §jobs — The Jobs plugin

A config-driven scheduler. Job definitions live **only** in the `plugins.jobs.config` row;
the scheduler is one producer of the host's shared run service (§host.actions) and runs any
registered action — by default the process plugin's `process.exec` or
`process.legacy-command` (§process). One tick a second scans a next-due table; occurrence
anchors, misfire, DST and retry state persist in `jobs-state.json`; every attempt is a line in
`logs/jobs/<id>.jsonl`. Code: `crates/swiss-jobs/src/jobs/` (`def`, `schedule`, `clock`,
`state`, `runlog`, `runner`, `migrate`, `groups`, `api`, `mod`), the descriptor and the
process plugin in `src/builtin.rs`.

**Descriptor.** Id `jobs`; page `jobs` ("Jobs", order 50); route `/api/jobs`; no `requires`;
`restart_on_config_change: false` — the row is **applied in place** (§jobs.apply). A restart
would cancel and await every in-flight run on every edit of any job.

**Three responsibilities, never mixed:**

| Part | Owns | Does not own |
|---|---|---|
| Definitions (the config row) | versions, validation, persistence, import | last run, PIDs, timers |
| Scheduler | trigger times, misfire, overlap and queueing, the run index | how a process starts, what an MCP is |
| Actions (§host.actions) | lookup, input validation, execution, cancel, result | cron and the job editor |

The chain is `trigger → occurrence → run coordinator → action registry → provider`. Stopping
Jobs never takes manual action runs away from other producers, and cannot cancel them.

**Out of scope:** DAGs and job dependencies, distributed scheduling, expression interpreters,
exactly-once external side effects, new cron dialects (seconds, `@daily`, `L`/`W`/`#`),
named time zones (`chrono-tz` is excluded), a second CRUD surface for definitions.

### §jobs.config — The config row

The single source of definitions is `gateway.config.json → plugins.jobs.config` (sealed,
revisioned by the config store, §host.config). The one authoritative parser is
`JobsConfig::parse` (`def.rs`): unknown fields are refused **naming the layer's legal fields**;
every error carries the **dotted path** of the field (`definitions.nightly.retry.maxAttempts`)
so the panel can point at it; numbers are whole and non-negative — a string, fraction or
negative is a type error, never a silent default (the JavaScript spelling `60.0` is tolerated).

Plugin level:

| Field | Default | Bounds |
|---|---|---|
| `schemaVersion` | 2 | only 2; anything else refuses the whole row |
| `maxConcurrentRuns` | 2 | 1–64; drives the run coordinator's capacity |
| `maxQueuedRuns` | 32 | 0–1024 |
| `retention.days` | 180 | 1–3650 |
| `retention.maxBytesPerJob` | 2 MiB | 64 KiB–64 MiB |
| `retention.maxHistoryBytes` | 64 MiB | 1 MiB–1 GiB (accepted and stored; the cross-file sweep is not built) |
| `definitions` | `{}` | at most 512; **the key is the job id** |
| `groups` | — | the group list (§host.groups); each definition carries a sparse `group` |

A job's id is its stable identity and its run-log file name: 1–64 characters of letters,
digits, `.`, `_`, `-`. Renaming a title never moves history; renaming a key is a delete plus a
create, and the history stays under the old key.

Definition level:

| Field | Default | Bounds and meaning |
|---|---|---|
| `title` | the id | ≤ 200 characters; display only |
| `labels` | `[]` | ≤ 16, each ≤ 64 |
| `disabled` | false | a disabled job keeps its history and can still Run now |
| `trigger` | required | §jobs.triggers |
| `action` | required | `{type, input, schemaVersion?}` |
| `timeoutMs` | 600 000 | 1000–86 400 000; **per attempt** |
| `overlap` | `skip` | `skip` \| `queue-one` |
| `misfire` | `skip` | `skip` \| `run-once` |
| `retry` | `{maxAttempts:1, delayMs:0, backoff:"fixed", retryOn:["failure"]}` | maxAttempts 1–10, delayMs 0–3 600 000, backoff `fixed`\|`exponential`, retryOn ⊆ {`failure`,`timeout`} |
| `output` | `{capture:"tail", maxBytes:16384}` | capture `tail`\|`none`; maxBytes 1024–1 048 576, enforced while reading |

- **Total deadline.** `maxAttempts × (timeoutMs + delayMs) ≤ 24 h` is checked at validation
  time, not discovered at run time.
- **Actions.** `type` is a capability id. It need not be registered to save — the provider
  may be disabled — but the PUT answers `warnings` and the job row reports
  `actionAvailable:false`; a registered type validates `input` against **its own schema**, so a
  saved job can never carry input its first run would reject. No shell semantics at the action
  layer: a shell is an explicit `cmd /c` or `sh -c`.
- **Credentials.** `${ENV}` and `${secret://…}` references in `action.input` are stored as
  references and resolved inside the capability at run time — `process.exec` strictly (a
  missing variable is a named error), `process.legacy-command` leniently (unset is empty, the
  old semantics). Resolved values join output masking and never reach the config, an error or
  the run index.
- **Two validation policies, one parser.** A PUT is strict. Boot is lenient: object
  placeholders left by an older validator are dropped with a warning, so one stale entry does
  not take the scheduler down; a non-object is still fatal.
- **Writes** go: the revision read → parse, validate, check dependencies → persist → publish
  the new desired revision → apply. A revision conflict is 409 (the later saver never silently
  overwrites the earlier); a failed persist is an error, never a warning behind a success; an
  apply failure shows desired against actual rather than pretending external effects roll back.
  The UI is a config editor, not a second database; the form and the JSON editor round-trip
  losslessly.

### §jobs.triggers — Triggers and scheduling

```json
{ "kind": "manual" }
{ "kind": "interval", "everyMs": 300000, "firstRun": "after-interval" }
{ "kind": "cron", "expression": "30 3 * * *", "timezone": "local" }
```

- **manual** never fires on its own. Every job, whatever its trigger, can Run now.
- **interval.** `everyMs` 1000–31 536 000 000 (1 s–365 d). `firstRun` `after-interval`
  (default: the anchor is boot, the first fire waits a full period) or `immediate`. The anchor
  is the persisted last run, never the moment a page opened.
- **cron.** Five fields, vixie semantics — when both day-of-month and day-of-week are
  restricted, either matches (`schedule.rs`, hand-written: the grammar is five small fields,
  ADR-007). Compiled **once** at parse time. `timezone` accepts only `"local"`; anything else
  is refused naming the rule.
- **The tick.** One task per plugin, once a second: sleep, scan the next-due table (`due ≤
  now`), spawn one occurrence task per due job. Never a resident task per job. The table is
  recomputed in full on apply or a clock change and per job after a claim; the tick never walks
  a calendar (a test counts `next_after` calls on a 1000-job table).
- **Occurrence keys** are the persistent identity of "this firing": `cron:<local wall minute>`
  (`cron:2026-09-09T03:30`, no offset) or `interval:<due ms>`; manual runs carry none and never
  move an anchor. Restart, sleep and clock jumps are judged against the persisted
  `lastOccurrenceKey`, not an in-process timer.
- **DST.** A spring-forward gap minute does not exist and is skipped; a fall-back repeated
  minute runs once (the second occurrence key equals the first). The clock is injected
  (`Clock`: `LocalClock` in production, a `FakeClock` with a synthetic DST rule in tests), so
  the assertions pass in any machine time zone, and **scheduling tests never sleep**.
- **Misfire.** After a restart or resume, the missed occurrences are counted from the anchor:
  `skip` records one `missed` summary (`missedCount`) and runs nothing; `run-once` runs the
  newest one and summarises the rest. Days of downtime never become a storm.
- **Claim first.** The anchor moves to the occurrence **before** anything else — whether it
  then runs, is skipped or is refused — or a full pool would retrigger the job every tick.
- **Overlap.** Runs are submitted with the label `job:<id>` (a bare id would let a manual run
  with the same label block the scheduler). `skip`: a running or queued run of the job records
  a `skipped` occurrence (`reason:"overlap"`). `queue-one`: admit **one** successor into the
  shared queue; a full queue is a visible `skipped` (`reason:"capacity"`).
- **Retry** belongs to one occurrence, not to the coordinator. Each attempt is its own submit
  (the global capacity applies per attempt) and records `attempt`/`attempts`. Only the
  outcomes in `retryOn` retry (`failure` = non-zero exit or spawn failure; `timeout`);
  **canceled never retries**, nor does a refusal (capacity, a missing capability).
  `exponential` doubles `delayMs` per attempt. The delay is raced against cancel, so disabling
  the plugin aborts a waiting occurrence at once.
- **Settle.** `lastOk` records only runs that happened — a refusal cannot claim an exit that
  never was; a settle after the definition was deleted is skipped.

### §jobs.api — The routes

The tree mounts inside the host's loopback guard and plugin boundary. Errors are typed: a write
error is 400 (invalid), 409 (not editable in v1) or 500 (persist); a run error is 404
(unknown), 409 (busy — the label is already running), 429 (capacity) or 503 (the capability is
not registered).

| Route | Answer |
|---|---|
| `GET /api/jobs` | `{jobs:[row]}` |
| `PUT /api/jobs/{name}` | the v1 body `{command, everySec?\|cron?, enabled?, timeoutMs?, cwd?, env?}` folds into the same config row, revision-checked; 409 for a definition v1 cannot spell (`use PUT /api/plugins/jobs/config`) |
| `DELETE /api/jobs/{name}` | `{deleted:name}`; a failed row write is 500 — the job would come back at the next boot, so no success is claimed |
| `POST /api/jobs/{name}/run` | synchronous by default: 200 `{run:record}`, bounded by the job's timeout; `{"async":true}` → 202 `{runId}` — one coordinator run either way, never a second execution path |
| `GET /api/jobs/{name}/runs?limit=&cursor=` | `{runs:[…newest first], nextBefore?}` — limit default 20, clamped 1–100; `before` is an alias of `cursor`; `nextBefore` is absent when exhausted |

**The row** keeps the v1 fields the panel has always read — `name` (= id), `command`,
`everySec` (only for a whole-second interval) or `cron`, `enabled` (= `!disabled`),
`timeoutMs`, `cwd`, `env`, `lastRunAt`, `lastOk`, `running`, `nextDueAt` (absent when
disabled) — and adds `id, title, labels, trigger, action, overlap, misfire, retry, output,
source:"config", actionAvailable, editableInV1, configRevision`. A `process.exec` job
projects a read-only display command and `editableInV1:false`.

**Editing a definition** is a plugin-config write — `GET /api/plugins/jobs/config`, change one
entry, PUT the **whole** object back with the revision just read (§host.config). The PUT
replaces the whole `config` object, which is why every editor must round-trip fields it does
not own.

### §jobs.runlog — Run records, state and history

- **The record** is built at one point (`ran_record`) for both the file line and the API
  reply: `{seq, at, trigger, ok, ms, outcome, occurrenceKey?, attempt, attempts, runId?, pid?,
  exitCode?, preview?, output, chars, timedOut?, canceled?, error?}`; `outcome` is `ran`,
  `refused`, `skipped` (with `reason`) or `missed` (with `missedCount`); a non-`ran` record has
  no exit code or output. `output.capture:"none"` drops `output`, `chars` and `preview`.
- **`logs/jobs/<id>.jsonl`**, one line per attempt, `seq` first (restored from the last line
  after a restart). Over `maxBytesPerJob` it is trimmed to half; older than `retention.days` it
  is swept hourly with the call log (§formats.logs). The page read walks **backwards from the
  tail in 8 KiB chunks**, so a page costs its own size, not the history's; torn lines are
  skipped; directory and files are private. The run log is an instance owned by the job
  system, not a process global.
- **`jobs-state.json`** (sealed, §formats.sealed) holds one line of **facts** per job:
  `lastOccurrenceKey, lastRunAt, lastOk, lastRunId, consecutiveFailures`. Missing or corrupt is
  an empty state with a warning; a field of the wrong type degrades to absent; **a write
  failure is a warning**, because the run already happened. Deleting a definition drops its
  line; its history stays.
- **Three files, three policies:** definitions (intent — a failed write is an error), facts (a
  failed write warns), history (outlives the job).

### §jobs.apply — Lifecycle

- **Create.** (1) Migrate first (§jobs.migrate); a failure leaves the plugin failed with the
  step named, and the scheduler never ticks over a half-migrated tree. (2) Read the row the
  store holds now (the migration may have merged into it). (3) Apply it.
- **Start** rebuilds the next-due table and spawns the single tick task.
- **Stop** aborts the tick, cancels occurrences waiting in a retry delay, and cancels **by
  owner** and awaits every jobs run — child tree reaped, pipe readers joined. Manual runs of
  other producers are untouched. Definitions, facts and history survive; enabling resumes.
- **Apply in place** (the host's third reconcile outcome, §host.lifecycle): swap the table,
  push the capacity to the coordinator (shrinking never kills in-flight runs), re-budget the run
  log, recompute next-due. Runs already started keep the definition snapshot they claimed;
  deleting a definition cancels its future, not its running attempt, and not its history. A
  failed apply leaves the plugin Active with the error and the revision unmoved, so desired and
  actual differ visibly.
- **Capability gone.** With the process plugin disabled its jobs still list and save; runs are
  refused 503 with a refusal record, and rows show `actionAvailable:false`.
- **Shutdown order:** jobs stops first (before data, tunnels and MCP), then the host reaps
  ownerless runs.

### §jobs.migrate — The v1 migration and acceptance

Runs once, at plugin create, before the scheduler:

1. No `jobs.json`, or one with `migratedAt` → done (idempotent).
2. Copy it to `jobs.json.v1.bak` (sealed, private; never overwritten).
3. Map each v1 row — `name` → key and title; `command`+`cwd`+`env` →
   `process.legacy-command` input (the old tokenizer and lenient references are the meaning of
   existing commands); `everySec` → an interval; `cron` → a local cron; `enabled:false` →
   `disabled:true` — and merge into the row. **On an id conflict the config wins**, logged.
4. Write the whole row with the current revision; one conflict retry, then fail.
5. Seed `jobs-state.json` with `lastRunAt`/`lastOk`.
6. Rewrite `jobs.json` as `{jobs:[], migratedAt, migratedTo, backup, count}` — the emptied
   array is what stops an old binary from running the table twice.

A crash between steps 4 and 6 reruns 3–6 safely (the merge keeps existing definitions); a test
interrupts between the two steps rather than calling the whole flow twice. The migration is not
transactional and does not claim to be; rolling back means the old binary plus the backup, and
a human checks the duplicates.

**Acceptance** (what the tests hold the plugin to): old jobs, sealed files and the v1 API keep
working, and no plaintext credential reaches an export or the run index; form and JSON
round-trip, revision conflicts, disk failures, unknown fields and oversized numbers; overlap,
global capacity, a full queue and the snapshot a running job keeps across an edit; DST, resume
and restart misfires on an injected clock; constant buffers under heavy output with neither
pipe blocking the child; cancel, timeout and plugin stop leave no run or child tree behind; and
**disabling and enabling 100 times returns task, connection and cache counts to their idle
bounds** instead of growing (§testing.memory).

### §jobs.panel — The Jobs page

`panel/src/jobs.ts`, `jobs-v2.ts`, `views/jobs.ts`, `run-history.ts`, `polling.ts`.

- **Availability.** The page trusts the plugin inventory; only when it is missing does one
  bare `GET /api/jobs` decide (404 from an older gateway or 503 from a disabled plugin hides
  the page).
- **List.** Grouped rows in the tunnels row frame: dot, title (id, labels, `off`), command,
  schedule sentence, last and next. Poll-safe: a redraw only when the structural signature
  changes; otherwise dot, title, last/next, the Run now button and the footer are patched. The
  dot: running → starting, disabled → idle, last failed → down, last succeeded → up, never ran →
  idle. One builder (`jobsChipText`: `N jobs · M on · K failing`) feeds the footer and the nav
  count.
- **Run now** submits `{"async":true}` and polls `GET /api/runs/{id}` each second to a terminal
  state, at most 1800 times (30 minutes — a UI bound; the job's own timeout is the real one),
  then points at History. Closing the page never cancels a job.
- **Row menu:** Edit (a job with `editableInV1:false` opens the advanced form), History, Delete
  (the confirmation says history stays on disk).
- **The simple form** (v1 shape): a schedule builder with four presets (interval, daily,
  weekly, monthly) plus raw cron. **cronstrue** (vendored, MIT, loaded on first use) reads the
  expression as a sentence in the panel's language, and the sentence is the validation (exactly
  five fields — cronstrue also speaks dialects the API refuses). Newlines in the command fold to
  spaces with a preview (one job, one process); the timeout is in minutes; env is `KEY=VALUE`
  per line and a bad line blocks saving.
- **The advanced form** reads the row, replaces one definition and PUTs the whole row with the
  revision read. The action picker comes from `GET /api/actions` and the input form from that
  capability's schema; an unregistered type offers the JSON editor. The form writes only the
  keys it owns — **unknown but legal fields round-trip** — and the form and JSON editor are two
  views of one object.
- **History** is a sheet with cursor paging (`nextBefore` drives "earlier").

## §terminal — Terminal

A browser terminal on the panel: a **local shell** (ConPTY on Windows, openpty on unix) and
**remote shells** on the SSH connections the tunnels plugin already holds. It is a developer's
desk, not a bastion host: one user on loopback, a handful of sessions, no accounts. Multi-tenant
access, RBAC, credential vaults and approval workflows are what bastion products are for; none
of them belong here. The frontend is xterm.js, the one mature browser terminal (VS Code's
integrated terminal and every well-known web terminal use it).

**Where it lives.** The crate split keeps SSH and HTTP out of the session machine (§arch.crates):

| Piece | Location |
|---|---|
| Session machine: config, sessions, tickets, recording, local shells | `crates/swiss-terminal/src/terminal/` — no axum, no russh |
| Routes and the WebSocket pump | `src/plugins/terminal_api.rs` (inside the loopback guard and the plugin boundary) |
| Descriptor, config schema, validation | `src/plugins/terminal.rs` |
| The PTY seam | `crates/swiss-core/src/platform/pty/` (`conpty.rs`, `openpty.rs`) |
| The shell capability seat | `crates/swiss-host/src/services/shell.rs` |
| The SSH shell provider | `crates/swiss-tunnels/src/tunnel/shell.rs` |
| Panel | `panel/src/views/terminal.ts`, `terminal-core.ts` (pure, unit-tested), `views/terminal-settings.ts`, `term-overlay.ts` |

**Descriptor.** id `terminal`, version `0.1`, config schema version 1, one page (order 70,
`sidebar: false` — the MCP sidebar is the MCP page's chrome), routes `["/api/terminal"]`,
`restart_on_config_change: true` (saving the config restarts the plugin and closes every
session), and `requires: []`. It does **not** require the SSH shell capability: a local session
needs no SSH, so the remote half reports the provider's presence instead of gating the plugin
on it (§terminal.remote).

**Out of scope, deliberately:**

- **A command blocklist.** Filtering commands on an interactive PTY byte stream is security
  theater — `e""cho`, aliases, pastes, TUI programs, `base64 -d | sh` all walk past it. Restrict on
  the remote host with real mechanisms (sudoers, a restricted shell).
- Multi-tenancy, RBAC, a credential vault, ticketed approval (see above).
- SFTP or a file manager — a different plugin with its own section, if ever.
- Session sharing or co-viewing: in a single-user loopback process its only use is a demo.
- Widening the panel's asset types (fonts, wasm) — §terminal.panel.
- New `PageDescriptor` fields or a different `/api/plugins` shape.

### §terminal.api — Routes, tickets and the wire

| Route | Does |
|---|---|
| `GET /api/terminal/targets` | What can be opened: `local {enabled, shell, shells}` and `remote {presence, reason?, targets}` |
| `GET /api/terminal/sessions` | The live sessions: `id, target, label, opened, bytesOut, attached, recording` |
| `POST /api/terminal/sessions` | Open `{target, cols, rows, shell?}` → 201 `{id, ticket, recording}` |
| `POST /api/terminal/sessions/{id}/ticket` | Mint a fresh one-time ticket for a reconnect → `{ticket}` |
| `GET /api/terminal/sessions/{id}/stream?ticket=` | The WebSocket |
| `POST /api/terminal/sessions/{id}/resize` | `{cols, rows}` → `{id, resized: true}` |
| `DELETE /api/terminal/sessions/{id}` | Close → `{id, closed: true}` |

- **Strict bodies.** The open body takes only `target`, `cols`, `rows` and `shell`; an unknown
  field is a 400 naming the known ones. `target` is a non-empty string (`local` or a connection
  id). `shell` is an optional one-session override of the local program.
- **Geometry** is `1..=1000` on both axes, checked as a `u64` before the narrowing cast (70000 as
  `u16` would wrap into range). Out of range is a 400, never clamped: a 0-column PTY is undefined
  behaviour on the far side. `PtySize::new` in the shell seat is the single authority.
- **Errors.** `NoSession` 404, `Ticket` 403, `Refused` 409 (a cap, a disabled local shell, a
  target not in `allowedTargets`), `Shell` 502 (the provider failed). The machine's message
  passes through verbatim. Every route answers a structured 503 while the plugin is not running.
- **Targets view.** `local.shell` is the **resolved absolute path** of the program a session
  would get, and `local.shells` is `[{program, label}]`, the candidates probed at plugin start
  (§terminal.local). `remote.presence` is `serving`, `stopping` or `absent`; `reason` names the
  provider plugin whenever the list is empty for a reason other than "you have no connections",
  because an empty list alone reads as the wrong story. Each remote target carries
  `id, label, host, port, username, state`. The `local.enabled`/`local.shell` fields keep their
  shape for older panels.

**Tickets.** The loopback guard (§security) checks peer, `Host` and `Origin`, and a browser's
WebSocket handshake carries `Origin`, so the guard already holds for the stream. The ticket makes
"can open a terminal" explicit, so that loosening the Origin check some day cannot loosen the
terminal with it. A ticket is 24 hex characters (96 bits from the OS CSPRNG), lives 10 s, is
single use and bound to one session id. Presenting it for the wrong session burns it; closing a
session forgets its tickets. The ticket minted at open cannot serve a reconnect (10 s against a
60 s grace window), so the panel mints a fresh one for every connect. The upgrade extractor is
checked **before** the ticket is spent — a handshake that cannot upgrade must not cost the ticket
— and the ticket verdicts are HTTP statuses decided before the 101, not a socket that opens and
dies. A missing ticket is a 400 naming the mint route.

**Frames.** Binary frames are raw PTY bytes in both directions — no base64, no JSON wrapping.
Text frames are control objects: `{"t":"resize","cols","rows"}` from the client;
`{"t":"exit","code"}`, `{"t":"error","message"}` and `{"t":"stalled"}` from the server. Data, exit
and error share one ordered channel, so an exit never overtakes the output before it. An unknown
control object is ignored, never fatal. Liveness is WebSocket ping/pong. Keystrokes have their own
write side: a parked outbound send never blocks input, so Ctrl-C reaches a program that is
flooding the terminal.

**Resize has two channels on purpose.** The `resize` frame while the socket is up, `POST
/resize` while it is not (a reconnect gap). Both land on the same machine call.

**Transport.** WebSocket through axum's `ws` feature; the net new crates are `tokio-tungstenite`
and `tungstenite` (the rest of the feature — hyper, sha1, base64 — was already in the graph).
SSE plus a POST input channel was rejected: it trades one crate for three bug classes (input
ordering, half-open liveness, an output stream that died while input lives on).

### §terminal.sessions — The session machine

- **One driver per session** owns the PTY or channel and takes commands over a bounded queue
  (32). The HTTP layer never touches a PTY directly.
- **Fan-out attach.** Any number of sockets can attach to one session and all receive its output
  — a second panel tab must not blind the first. The last detach starts the grace clock.
- **Grace and catch-up.** A dropped socket is not a dead session: for `graceSeconds` (60) the
  output goes into a catch-up buffer and a reconnect replays it. A closed laptop lid must not kill
  a running compile. The buffer is **64 KB per session, a constant, not config** — it is a line in
  the memory budget (§terminal.budget). It drops from the front and tells the reattaching client
  so: `[gateway] N bytes of output were dropped while this terminal …`.
- **Backpressure, never loss.** Each attached client has a 16-frame queue. When it is full the
  driver **stops reading** the PTY or SSH channel; the pipe and the SSH window carry the pressure
  back to the program, which is how a real tty behaves. Bytes are never dropped: one lost fragment
  of an escape sequence corrupts the display for the rest of the session. After 1 s of a full
  queue the client gets `{"t":"stalled"}`; a queue full for `stallSeconds` (30) is a client that
  is gone, and the session closes with that reason.
- **Four clocks:** idle (`idleTimeoutMinutes`, 30; output counts as activity; 0 = never), grace,
  stall, and the recording cap (§terminal.recording).
- **Caps.** `maxSessions` (4) and `maxSessionsPerTarget` (2) govern **remote targets only**. Local
  sessions are uncapped and do not spend the remote budget (owner decision 2026-09-12). A
  reservation holds the slot from the cap check to the open, so two concurrent opens cannot both
  pass the last free slot.
- **Every close says why.** The sentence goes into the terminal as a visible `[gateway]` line
  (except when the shell exited on its own — it already said goodbye), into the recording's end
  marker and into the `error` frame. Final frames get 250 ms to flush.

| Close reason | Sentence |
|---|---|
| Exited | `the shell exited` / `the shell exited with code N` |
| Failed | the provider's own message |
| Idle | `closed after N minutes with no activity (idleTimeoutMinutes)` |
| Stalled | `closed: this terminal accepted no output for N seconds (stallSeconds)` |
| Abandoned | `closed: nothing reconnected within N seconds of the socket dropping (graceSeconds)` |
| Requested | `closed on request` (DELETE, or the panel closing the tab) |
| Shutdown | `closed: the terminal plugin is stopping` |

Stopping the plugin closes every session with the Shutdown reason; drivers, PTYs and recordings
follow the sessions down (§host.lifecycle).

### §terminal.config — Config

`plugins.terminal.config`, parsed strictly (`TerminalConfig::parse`): an absent key takes its
default, a present key of the wrong shape is a user-facing 400 through `validate_config` naming
the key. A mistyped limit that silently fell back to the default would be a limit the user
believes in and does not have.

| Key | Default | Rule |
|---|---|---|
| `local.enabled` | `false` | The local shell is off until the user turns it on |
| `local.shell` | `""` | Empty = the platform default (§terminal.local) |
| `allowedTargets` | `[]` | Exact connection ids; empty = every connection the provider lists |
| `maxSessions` | 4 | 1..=64; remote only. Zero is refused — a plugin that can open nothing is a disabled plugin |
| `maxSessionsPerTarget` | 2 | 1..=64, and never above `maxSessions` |
| `idleTimeoutMinutes` | 30 | 0..=86400; 0 = no clock |
| `graceSeconds` | 60 | 0..=86400 |
| `stallSeconds` | 30 | 0..=86400 |
| `recording` | `true` | Off is honest: the list and the open response report `recording: null` |

**Why the local shell is off by default.** The gateway already runs as the user, so a local shell
gives a local attacker nothing new — but it turns a loopback HTTP port into arbitrary code
execution, and that upgrade is worth one deliberate click. The switch lives on the terminal page
(§terminal.panel). The terminal never holds a credential: remote shells arrive as byte streams
from the provider, and anything secret in config follows §host.refs.

### §terminal.recording — Recording

- **asciicast v2**, one file per session at `~/.swiss/terminal/<sessionId>.cast`: a header object
  (`env.TERM = "xterm-256color"`), then one `[elapsed, "o", text]` line per chunk. Elapsed time is
  monotonic. UTF-8 is reassembled across chunk boundaries (at most 3 bytes held back).
- **Output only.** That is what asciicast is — and it does **not** mean passwords stay out: a
  shell echoes what is typed, and an echoed secret is in the output stream. Only unechoed input
  (a sudo prompt) is truly absent. Treat every `.cast` as sensitive: `*.cast` is gitignored, and
  recordings never go into a commit, an issue or a report.
- **8 MB cap, no rotation.** At the cap recording stops with a visible marker (`[gateway]
  recording stopped at the 8 MB cap; the session is still running …`); a session end writes
  `[gateway] session ended: <reason>`. Silent rotation was rejected: a recording that quietly lost
  its beginning is worse than one that says where it stopped.
- The directory is created at plugin start, `0700`; files are `0600` (§host.vault's private-file
  helper). A recording failure warns and never takes the session down.
- `recording: false` in config turns it off for new sessions.

### §terminal.local — Local shells

- **The PTY seam** is `swiss_core::platform::pty`: ConPTY on Windows, `openpty` on unix. The ConPTY
  FFI is written here rather than taken from `portable-pty` (ADR-011: the seam is small, and the
  crate would bring its own process and thread model).
- **One blocking read thread per session** (`spawn_blocking`, 32 KB reads, a depth-1 channel):
  a ConPTY's output is an anonymous pipe, and parking on the full channel **is** the backpressure.
- **Environment:** `TERM=xterm-256color`, `COLORTERM=truecolor`, `NO_COLOR` removed.
- **The program, in precedence order:** the open request's `shell`, then `local.shell`, then the
  platform default. The default on Windows is `pwsh.exe` if PATH resolves it, then
  `powershell.exe`, then `COMSPEC`, then `cmd.exe`; on unix `$SHELL`, then `/bin/sh`.
- **PATH lookup** (`find_on_path`, a pure function taking PATH as an argument so tests never touch
  the process environment): the name is tried as given first; `PATHEXT` is appended (lowercased)
  only when the name has no extension; a hit is `metadata().is_file()`. No `canonicalize` — the
  Store build of pwsh is an execution alias under `WindowsApps`, and canonicalizing resolves it
  into a versioned package path that is ugly and breaks on the next update.
- **Candidates** for the settings sheet — PowerShell 7, Windows PowerShell, cmd, plus
  `%ProgramFiles%\PowerShell\7\pwsh.exe` and `%ProgramFiles%\Git\bin\bash.exe` — are probed once
  at plugin start and cached, never per request, and never from the registry. Labels are
  `PowerShell 7`, `Windows PowerShell`, `cmd`, `Git Bash`, or the file's basename.
- **Process tree.** The child joins a kill-on-close job object (`KillOnCloseJob`, ADR-008); unix
  kills the process group. Killing `swiss` leaves no orphan shell (§process).

### §terminal.remote — Remote shells

- **The seat.** `ShellRegistry` in `RuntimeServices` (`swiss-host/src/services/shell.rs`) holds at
  most one provider. A second registration is refused naming the incumbent. A provider that
  withdraws stops new sessions at once and gives the live ones a 3 s drain. `PtySession` is
  transport-neutral (bounded input and output queues of 32 chunks); ledger leases are scoped to
  the session. This seat is why the terminal crate never depends on the tunnels crate
  (§arch.crates, ADR-011).
- **The provider** is the tunnels plugin (`TunnelShells`, provider id `tunnels`): a target is an
  existing tunnels connection, and `TunnelManager::open_shell` opens a PTY channel on it — no second
  credential, no second host-key decision (§tunnels.ssh). Waiting for the `want_reply` receipt of
  the PTY and shell requests must skip `WindowAdjusted` messages. The connection guard moves into
  the pump, so the connection stays up exactly as long as the shell.
- **Presence.** With the tunnels plugin disabled the remote list is empty **and the reason names
  the tunnels plugin**; while it stops, the reason says no new remote sessions start. Local
  sessions are unaffected either way.
- `allowedTargets` narrows the list; a target outside it is refused with a 409.

### §terminal.panel — The terminal page

**Vendoring xterm.** xterm 5.5.0 with addon-fit 0.10.0, addon-unicode11 0.8.0, addon-web-links
0.11.0, addon-webgl 0.18.0 and addon-search 0.16.0 (lazy-loaded on first use), under
`vendor/xterm/<package>-<version>/` (§panel.vendor):

- The npm **dist** UMD files, byte-identical — no minification, no patches; wrap in our own code
  instead. Each directory carries an `index.js` shim naming the package and exact version, loaded
  through `load-classic.js`. Never build from the xterm.js source tree (it is 6.0-dev, with
  incompatible internals).
- An upgrade is a new directory plus the import change, and the old directory is deleted in the
  same commit, so `git log` answers which xterm shipped.
- The asset pipeline serves only UTF-8 `.html`, `.css`, `.js` and `.svg` (`mime_of`), which is part
  of why the tree can be compared byte for byte. So: the system monospace stack
  (`ui-monospace, SFMono-Regular, Consolas, "Cascadia Mono", monospace`), no web or Nerd fonts, and
  no wasm. Rendering is WebGL with a DOM fallback on context loss (there is no canvas addon in 5.5).
  After a font zoom the WebGL texture atlas is cleared.
- Facts verified against 5.5 (do not build on the opposite): no `bellStyle`/`bellSound` options (the
  beep is generated); `getOption`/`setOption` are gone (use live `term.options`); load Unicode11
  before setting `activeVersion`; search decorations need `allowProposedApi: true`. Addons checked
  compatible with the vendored core: search 0.16.0, clipboard 0.2.0, serialize 0.14.0, progress 0.2.0.

**Keys (Windows Terminal semantics, not xterm's Linux defaults).** Pure logic in
`terminal-core.ts` (`keyAction`, `mouseAction`), pinned by `test/admin-terminal.test.ts`:

| Input | No selection | With a selection |
|---|---|---|
| Ctrl+V, Ctrl+Shift+V, Shift+Insert | paste | paste |
| Ctrl+C | `^C` — SIGINT, never changed | copy and clear the selection; no `^C` |
| Ctrl+Shift+C, Ctrl+Insert | nothing | copy |
| Right-click | paste the clipboard | copy and clear the selection |
| Shift+right-click | the browser's menu (the escape hatch) | same |
| Ctrl+= / Ctrl+- / Ctrl+0, Ctrl+wheel | zoom 8–32 px (default 13), remembered per browser | |
| Ctrl+Shift+F | find bar | |
| Alt+1..9, Alt+←/→, Alt+W | jump to tab, previous/next, close (the browser keeps Ctrl+Shift+W and Ctrl+Tab) | |

- Keyboard paste returns `false` from the custom key handler **without** `preventDefault`, so the
  browser's native `paste` event reaches xterm's textarea — no clipboard permission prompt. Right-
  click paste uses `navigator.clipboard.readText()` and `term.paste()` (bracketed paste); a refusal
  toasts "use Ctrl+V". Copy is `writeText` then `clearSelection`; a failure toasts, never silent.
  Focus returns to the terminal after every action.
- Shift+Tab is `preventDefault` but returns `true` (the key must reach the terminal, or browser
  focus escapes). Alt+F4 passes through.
- The paste payload is never rewritten (no `\n → \r`): bracketed paste is what bash and PSReadLine
  expect. A paste of more than one line asks first (a native confirm previewing the first 1000
  characters) — except on the alternate screen, where multi-line is normal.

**Tabs and sessions.** Every session is a tab; a page mount adopts the sessions already open; a
closed session keeps its tab so the scrollback stays readable. The label precedence is a manual
name, then the shell's OSC 0/2 title, then the connection name (`tabLabel`); an empty title falls
back, and shells that never send one do not stall anything. Rename by double-click or right-click
on the tab; middle-click closes. A mount epoch invalidates callbacks from a previous mount.

**Reconnect.** Back off from 500 ms doubling to 5 s, minting a fresh ticket each attempt. After a
reconnect the page resets terminal modes (`ESC[?1000l ?1002l ?1003l ?1006l ?2004l`) so a catch-up
replay cannot leak mouse or bracketed-paste modes into visible text. Keystrokes typed before the
first socket opens are queued (up to 64) and sent on open; a reconnect gap still drops them.

**Parity features** (the "local terminal feel", measured against native and web terminals):

- **Bell:** a dot on the tab by default; badge-plus-sound (an 880 Hz WebAudio beep) or off are
  per-browser preferences.
- **Find:** Ctrl+Shift+F opens a bar (regex, case, whole word; Enter / Shift+Enter; the count comes
  from `onDidChangeResults`, asynchronous and already debounced). Esc closes, clears the
  decorations and refocuses the terminal. The page-level shortcut listener yields the whole
  `.term-holder` subtree, or the bar opens and shuts in one keystroke.
- **Scroll pin:** a user scrolled away stays put while output lands, and a "N new ↓" chip counts
  the lines below; clicking it returns to the bottom. xterm's `onScroll` does not fire for wheel or
  keys, so the pin is judged by a capture-phase wheel listener plus `viewportY >= baseY - 1`, and
  captured before each write. Typing re-pins. Leaving the alternate screen returns to the bottom.
  Never patch xterm internals (`core.scrollToBottom`).
- **Copy on select**, on by default, trims trailing whitespace, and is guarded so search
  navigation (which selects each match) never overwrites the clipboard.
- **Overlay pill:** `cols×rows` on resize, ✂ on copy, the reconnect state.
- **Guidance:** a one-shot orientation hint after the first attach (once per browser) and a `?`
  sheet listing every key and gesture.
- Preferences live in `localStorage` (`swiss.terminal.fontSize`, `.bell`, `.copyOnSelect`,
  `.hint`), wrapped in try/catch.
- The terminal palette reads the panel's `--term-*` tokens and follows the theme switch.

**The Local shell sheet.** A gear beside the target picker opens it; with the local shell off, the
empty state line "Local shell is off — turn it on" opens it too. It has the Enabled switch (with
the one-line reason from §terminal.config) and a Shell field with the probed candidates as
suggestions (any path may be typed). Save reads the current config and revision, changes **only**
`local`, and PUTs the rest back untouched (§host.config's revision lock). With live sessions it
confirms first: "Saving restarts the terminal plugin and closes N sessions." — the honest cost of
`restart_on_config_change`. After the save the targets reload and the local row is selected,
labelled `local · <candidate label>` (basename when no candidate matches).

**Repaint memo.** The tab bar skips identical repaints; any code path that changes the DOM outside
the memo (Escape out of a rename, a remount, a settings save) must reset it, or the bar strands a
rename input or paints empty.

### §terminal.verify — Acceptance and live verification

Acceptance (the numbering is stable; code cites it as "item N"):

1. The gates pass, and `cargo tree -d` shows only `tokio-tungstenite`/`tungstenite` as new.
2. The panel tree check passes (§panel).
3. In a browser, a session on a configured tunnel host logs in, runs `vim` and `htop`, renders CJK
   and emoji without misalignment, and `stty size` follows a window resize.
4. Disabling the tunnels plugin empties the remote list with text naming the tunnels plugin; local
   sessions are unaffected.
5. Disabling the terminal plugin turns every `/api/terminal/*` into a structured 503, and the
   navigation shows it off.
6. A socket dropped for less than the grace window reconnects to the same session with the missing
   output replayed; past the window the session is closed and gone from the list.
7. `yes` for ten seconds, then Ctrl-C: RSS does not climb (backpressure) and the display is intact
   (no lost bytes); the Ctrl-C gets through.
8. Killing `swiss` leaves no shell child behind (ADR-008).
9. `asciinema play ~/.swiss/terminal/<id>.cast` replays.
10. Every §terminal.budget row has a measured number within budget.

**Live verification** runs on a test instance, never on production (§testing.live). Specific to
this page:

- Rebuild with the test target directory and kill the old instance **by its port's owning PID**
  first — a running instance locks the binary and cargo fails late with `os error 5`. After every
  restart read the build hash from `/health`: a restart that silently failed leaves the old binary
  serving.
- **Wait for the prompt before typing.** A cold pwsh start is slow, and the pre-open queue covers
  the socket gap, not a slow child process.
- The matrix: OSC 0 title sync; rename by right-click and by double-click; a manual name beats a
  later OSC title; the bell dot; the find bar opened with focus on the page **and** on the terminal
  (it must stay open); the live match count; find navigation never copies; Esc closes the bar; the
  pin and chip; a multi-line paste confirm; Alt+1/Alt+2 between two tabs; the tab bar after Esc
  from a rename and after navigating away and back; a clean console at the end.
- **The pin test needs choreography:** typing re-pins, so typed output can never show the chip.
  Print ~300 filler lines, run `Start-Sleep 4; <print 25 lines>`, wheel up during the sleep; the
  view must hold, the chip must read `25 new ↓`, and clicking it must return to the bottom.

Automation traps, each of which once produced a wrong verdict:

- A synthetic `dblclick` proves nothing about double-click (it bypasses click sequencing); verify
  that the tab node survives one real click instead.
- Wheel events must target `.xterm-viewport`, not the holder.
- Select the visible terminal (`.term-holder:not([hidden])`); a page with adopted sessions has
  hidden holders of zero size, and assertions on them are vacuous.
- A native `confirm()` parks the renderer; answer it with a raw key press.
- A shortcut is tested twice — focus in the terminal and focus on the page.
- Check that there is scrollback at all (`scrollHeight > clientHeight`) before blaming the pin.
- A test home's recordings are real session output: never commit them.

### §terminal.budget — Budget

| Item | Budget | Measured |
|---|---|---|
| Binary size (the terminal's share, vendored xterm included) | +≤ 800 KB | within budget |
| RSS, plugin loaded with zero sessions | +≤ 1.0 MB | ≈ 0 (in the noise) |
| Each idle remote session | +≤ 256 KB | 250.7 KB |
| Each idle local session | +≤ 1.0 MB | 108 KB plus the read thread's stack (the child process is its own) |
| Catch-up buffer | 64 KB × sessions, a hard cap | a constant, not config |

Measure per §testing.memory. Over budget: first check whether the WebGL addon is the weight, then
whether a buffer lost its bound. The last resorts — the whole plugin behind a cargo feature, or
`portable-pty` for local sessions — each need a §decisions entry, never a quiet swap.

### §terminal.roadmap — Not built yet

The parity work above is P0 and part of P1 of a longer list. What remains, in order of value per
cost:

- **P1:** OSC 52 remote clipboard (addon-clipboard 0.2.0, write-only — refuse the `?` read-back
  query, a clipboard-theft vector — with a 128 KB raw / 75 KB decoded cap, sharing the paste code
  path); OSC 9;4 progress on the tab (addon-progress 0.2.0); a settings sheet for palette, cursor
  and scrollback (1k–50k, default 5000) applied live through `term.options`; a per-tab shell picker
  (the API already takes `shell`).
- **P2, shell integration:** parse OSC 633/133/7 and 9;9 (Windows needs OSC markers — ConPTY
  swallows DCS). It unlocks an exit-code badge (130 = Ctrl-C and 141 = SIGPIPE are not failures),
  the cwd in the tab, Ctrl+↑/↓ prompt navigation, completion notices for long commands, and a
  JSONL command history (2k entries, drop-oldest). Injected scripts fail open to plain passthrough
  for unknown shells, and marker bytes are filtered out of recordings.
- **P3, recordings:** list, download and delete routes; a small in-panel cast player (a JSONL
  scheduler, not the upstream wasm player); `idle_time_limit: 2` in the header; transcript
  download; serialize-snapshot restore on adopt; secret redaction in the recording stream.
- **P4, protocol:** a resumable chunk ring replacing the one-shot catch-up (within the 256 KB
  per-session line); an RTT badge; typeahead for SSH tabs.

Not planned: sixel and images, ligatures and web fonts, zmodem, serial ports, split panes (a large
item needing its own evaluation), collaboration or end-to-end encryption, AI features, desktop
chrome (tray, quake mode, updater).

## §remote — Remote execution

The agent-facing way to run a build or a test on a machine the gateway reaches over SSH,
without the agent holding a credential or an interactive shell. A **target** is an alias the
agent types; the gateway resolves it to a tunnels connection, runs the command there and keeps
the record. A remote action **is a run** through the host's coordinator (§host.actions) — the
same queue, cancel, cursor and history as every other run — not a second job system.

| Where | What |
|---|---|
| `crates/swiss-remote` | `target` (the table), `actions` (the five actions), `sync` (file movement), `project` (the binding), `history` (the run log), `groups` (the targets scope), `api` (`/api/remote`) |
| `src/plugins/remote.rs` | The descriptor, start/stop, the builtin MCP mount |
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

### §remote.targets — Targets and the routes

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
- **Store.** The sealed `~/.swiss/remote.json` (§formats.sealed), written through on every
  mutation. A pre-groups file (a bare array) still opens.

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

### §remote.actions — The five actions

| Action | Input (strict; unknown fields refused) | Needs |
|---|---|---|
| `remote.exec` | `{target, argv[], env?: {NAME: value}, cwd?, timeoutMs?}` | `exec` |
| `remote.sync` | `{target, source, exclude?[], verbose?, to?}` — upload a local tree, or one file (`to` names it remotely) | `sync` |
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
- **Sync** is upload-only and **never deletes** — a sync that removes remote files is a footgun
  no agent should hold. It walks the local tree, skips the default excludes (`.git/`,
  `.swiss/`, `target/`, `node_modules/`, by path prefix) plus the binding's and the input's,
  and uploads a file whose remote size differs or that is missing. Size is the only comparison:
  an edit that keeps the byte count is not re-sent (a known gap). Caps: 20,000 files, depth
  32. Pull caps: 2,000 files, depth 32. Both report compactly — a summary line
  (`sync: N scanned, N uploaded (N bytes), N up to date`), one line per file only when
  `verbose`.
- **Writes are not atomic**: a remote file is streamed in 64 KiB chunks over one SFTP session
  and overwritten in place.
- **Cancel is an outcome**, not an error: `canceled: true`, `ok: false`. The channel is closed
  from this side, a bounded drain (3 s) collects what the far side flushes, and the run
  resolves as canceled — no future-dropping pretence.

### §remote.security — What a target can and cannot do

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
  data. No `join(" ")` shortcut exists anywhere.
- **Vault references in exec.** argv, env values and cwd may carry `${secret://NAME}`
  references (§host.vault); a `${UPPER}` stays literal text. `validate_input` checks syntax
  only. Resolution happens after the run is recorded — the record keeps the input as typed —
  and a missing secret fails before any channel opens, naming the field. Every resolved value
  of 8 bytes or more is masked (`••••••••`) before a byte reaches the live buffer, the record or
  the outcome; a `:-default` is not masked. Prefer env over argv (argv is visible in `ps` on the
  far side) and single-quote a reference in a shell so the local shell leaves it alone.
- **Recorded, not trusted.** Every run is recorded with who asked (§remote.history); the CLI's
  actor is self-declared, the MCP's is the authenticated token.

### §remote.transport — The transport seat

The host defines the seat (`services/remote.rs`, §host.seats); tunnels provides it
(`TunnelRemote`, §tunnels.providers).

- **Operations:** `list` endpoints, `exec` (stdout and stderr streamed as they arrive, never
  read to the end; returns the exit code), and the SFTP family — `stat`, list, streaming read
  and streaming write in 64 KiB chunks (`REMOTE_CHUNK_BYTES`).
- **Backpressure.** Exec output crosses a bounded queue (`EXEC_EVENT_QUEUE`, 64 events); a
  command that floods slows the transport down instead of being buffered in memory.
- **Leases.** Every operation takes an operation-scoped connection lease on the shared SSH
  client (§tunnels.ssh); the lease travels with the exec future or the SFTP reader, so the
  connection reference lives exactly as long as the work.
- **Withdraw first.** On tunnels stop the transport withdraws before anything else, with no
  drain: a remote exec is a run with its own deadline and cancel, so new calls are refused and
  in-flight ones end through their cancel path.
- **Errors** map onto a transport-neutral `RemoteError`; cancellation keeps its own kind so the
  outcome can be an honest "canceled".

### §remote.utf8 — UTF-8 end to end

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
  already be UTF-8. The CLI usage and the skill say so.

### §remote.history — The run log and the audit

The coordinator forgets a finished run after 32 more; the build an agent started at 3 a.m. must
still be readable in the morning. The remote plugin registers a `RunHistorySink` at start and
removes it at stop.

- **Layout.** `~/.swiss/logs/remote/runs.jsonl` holds one line per **finished** run: the run
  view exactly as `/api/runs` showed it plus what was asked — target, argv, cwd, **env keys
  only** (`envKeys`; values may be secrets), a write's content as its size only. The full
  output is teed as it flows into `out/<runId>.txt`. Private modes (0700/0600).
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
- **Run ids survive restarts.** The coordinator persists its next id in `~/.swiss/runs.seq`
  (tmp + rename on every allocation), and the sink's `last_run_id` is the floor — without it a
  restart reused numbers and the log carried two runs under one id.
- **The query.** `GET /api/remote/runs?before=&limit=&target=&since=&until=&actor=` → `{runs,
  active, limits {maxAgeMs, maxTotalBytes, maxRuns, maxOutputBytes, auditWindowMs}, usage,
  nextBefore}`. Newest first, 20 per page by default (at most 100), paged by the older-than
  cursor; `active` are the remote runs the coordinator holds that are not yet recorded, so a
  page paints from one read. `since` is inclusive and `until` exclusive, both on `endedAt`, as
  epoch ms or ISO-8601; a malformed time is a 400, never a silently widened window. Reading
  walks the index from its end in blocks, so a page costs a page.
- **Output reads.** `/output?after=&max=` reads the stored file (128 KiB at most per read) with
  the same cursor contract as the live one; `/content` answers the sealed write content or 404
  when none was kept.
- **Actor** — §host.actions: `cli:<user>@<host>`, `mcp:<token label>`, `panel`, `jobs`, or `api`
  (blank or over 128 bytes also falls back). The MCP actor comes from the call source captured
  when the server was built, not a task-local read inside the call (rmcp drives `call_tool` from
  its own task, where the task-local is empty).

### §remote.cli — `swiss remote` and `swiss run`

```
swiss remote endpoints | targets | resolve [name]
swiss remote target add <id> --endpoint <id|unique-label> --root <path> [--caps exec,sync,files]
swiss remote target set <id> [--endpoint …] [--root …] [--caps …] [--label …] | target remove <id>
swiss remote exec [name] [--cwd DIR] [--env NAME=VALUE]… [--timeout 2h] [--detach] [--] ARGV…
swiss remote sync [name] [--source PATH] [--exclude PATTERN]… [--verbose]
swiss remote push [name] <file> [--to NAME] | cat [name] <path> | write [name] <path> | pull [name] <path> [--to LOCAL]
swiss run status <id> | logs <id> [-f] | cancel <id>
swiss run audit [--since 7d|36h|90m|30s|ISO] [--until ISO] [--target t] [--actor a] [--json] [--export DIR]
```

- **The hard rule.** Everything after a bare `--` is argv for the far side, passed through
  untouched. For `exec`, argv also begins at the first command word: `exec dev ls -a` equals
  `exec dev -- ls -a`.
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
- **A fast run prints whole.** A run that finished before the first poll has already been
  compacted to its last 64 KiB, so a follower whose first read (cursor 0) comes back truncated
  from a terminal run prints the stored record instead — it is written before the run turns
  terminal. Without this a `cat` or a quick build silently lost its head.
- **Audit.** One line per run — time to the second, actor, target, action, argv quoted by the
  POSIX rules, result, duration, bytes (a `*` when the output was evicted), `#id`. The last
  7 days by default; `--export DIR` writes `runs.jsonl` plus `out/<id>.txt` for handing the
  evidence to a person.

### §remote.project — Project binding, notes and the skill

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
hold a credential.

**The skills.** `src/skill_assets/SKILL.md` (`swiss`: what the gateway is, its boundaries, the
panel and `swiss --help`) and `src/skill_assets/remote/SKILL.md` (`swiss-remote`: the command
reference, the MCP tools, the UTF-8 and audit contracts, the notes). `swiss skill install`
writes both (§host.cli). `disable-model-invocation: true` is the owner's choice; keep it.

### §remote.mcp — `/mcp/remote`

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
  `timeoutMs` gets the same 2 h the CLI gives.

### §remote.panel — The Targets and Runs pages

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
  runs fold into ×N. An open live row follows its output every 1.5 s through
  `/api/runs/{id}/output`; the list itself polls every 6 s and never rebuilds an open row.
  Reads are 128 KiB.
- **The whole command, copyable.** An opened row shows the command exactly — argv quoted word
  by word by the same rule as `quote_posix`, so a pasted copy runs as the same argv — with a
  Copy button.
- **The actor shows the surface only** (`cli`, `mcp`, `panel`, …), never the machine's user or
  host name, and is never translated.
- Filter by target; Clear sits in the ⋯ menu (a destructive verb gets no standing button). The
  budget line states the limits and that the last 7 days are always traceable; an evicted row
  says its output was cleared and the record kept.

## §process — Processes and the supervisor

How the gateway starts, watches, captures and tears down an external command. Three layers:
the **process plugin** contributes two capability actions; the host's **run coordinator**
(§host.actions) queues and deadlines them; the host's **supervisor** spawns and reaps every
child — the jobs' one-shot commands and the `proc` MCPs' resident children alike — on top of
the platform calls in `swiss_core::platform`.

| Where | What |
|---|---|
| `crates/swiss-host/src/services/actions.rs` | `process.exec`, `process.legacy-command` |
| `crates/swiss-host/src/services/process.rs` | The supervisor: bounded capture, spawn, teardown, the legacy tokenizer, output decoding |
| `crates/swiss-host/src/services/runs.rs` | `RunCoordinator` |
| `crates/swiss-host/src/proc_pids.rs` | The port-scoped PID ledger and boot settlement |
| `crates/swiss-core/src/platform/` | Job objects (`job.rs`), Toolhelp32 walks and working sets (`windows.rs`), process groups (`unix.rs`), the shared descendant walk (`mod.rs`) |
| `src/builtin.rs` | `ProcessPlugin` |

**The plugin** — id `process`, label Process, version 0.1, config schema version 1 with an
empty schema, `restart_on_config_change: false`, `requires: []`. **No pages and no routes**: a
capability plugin is reached through the host's generic `/api/actions` and `/api/runs`, and a
navigation tab for it would be a page nobody asked for. Its only surface is its row on the
Plugins page.

- **Registered last, so it starts first** (boot walks the registration list in reverse): a
  capability provider is online before anything can submit a run against it.
- **Start registers both actions or neither.** If the second registration fails, the first is
  rolled back — no plugin reports active with half its capabilities.
- **Stop withdraws, then cancels.** Both actions leave the registry first (a new submission
  gets 400 "unknown action type" instead of silently doing nothing), then every run already
  executing one of them is cancelled and **awaited** — tree killed, readers joined — before
  stop returns. Runs of other capabilities are untouched, and producers keep running: a
  disabled process plugin leaves the Jobs scheduler ticking, and its process jobs report the
  missing capability (§jobs).

### §process.actions — The two actions and their rules

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

### §process.runs — The coordinator's invariants

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

### §process.supervisor — Spawn, capture, teardown

`Supervisor::run` owns the whole life of one child:

- **Spawn.** stdin null, stdout and stderr piped, `kill_on_drop`. Windows adds
  `CREATE_NO_WINDOW` (a console-less daemon must not flash a console per child) and assigns the
  child to a kill-on-close job object at spawn (`KillOnCloseJob::assign`, ADR-008). Unix starts
  a private process group (`process_group(0)`), so one `kill(-pgid)` reaches any depth. A
  `PidGuard` registers the root pid for the memory reading and unregisters on drop — a panic
  leaves no stale entry.
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
uses the same spawn flags, the same job object and the same PID ledger: lifecycles separate,
platform mechanics shared.

### §process.tree — Process trees on each platform

- **Windows.** One kill-on-close job object per child tree: when the handle closes — clean
  exit, crash, End Task, a debugger detaching — the OS kills the tree. PID-reuse safe by
  construction. The walk (for the memory reading and the boot settlement) is a Toolhelp32
  snapshot; working sets come from `GetProcessMemoryInfo`. No PowerShell anywhere
  (§arch.rules).
- **Unix.** A private process group per child; teardown is `kill(-pgid)`.
- **The shared walk** (`walk_descendants`) is one breadth-first pass with cycle protection and
  an "exclude my own pid" option, used by the memory reading (§host.memory) and the orphan
  settlement.
- **The PID ledger** (`.proc-pids-<port>.json`, §formats) covers what the job object cannot: a
  child that outlived a previous generation of the gateway. A successful `proc` spawn records
  its pid, a clean close removes it (temp + rename, so a kill mid-write never tears it). At
  boot, before any `proc` MCP starts, the ledger is read and cleared and every live pid that is
  a descendant of the recorded tree and not this instance's is tree-killed; if the snapshot
  fails, nothing is killed this round (a stray left for next time beats killing a child this
  instance just started). Port-scoped, so two instances never reap each other (§host.daemon).
- **The local terminal** assigns its shells through the same two functions (§terminal.local):
  killing `swiss` leaves no child anywhere.
- **FFI `unsafe` lives only here**, in `swiss_core::platform`, with a SAFETY comment per item
  stating handle ownership and lifetime; no other crate calls Win32 or libc directly. The only
  other `unsafe` is edition 2024's `env::set_var`/`remove_var` — at process start before any
  thread exists (with a SAFETY comment saying so) and in tests.

## §panel — The admin panel

One page application, embedded in the binary, served on the same loopback port as everything
else. It is held to the product's four properties like the binary is (§product): pixels are
as accountable as bytes. No framework, no bundler, no runtime npm dependency, no web font
download.

| Where | What |
|---|---|
| `crates/swiss-panel/panel/src/` | The TypeScript source: shell (`main.ts`, `page-registry.ts`, `page-core.ts`, `plugin-palette.ts`, `last-page.ts`, `immersive.ts`, `pane-scroll.ts`), views (`views/*.ts`, one per page), the UI library (`ui/`), state slices, i18n (`i18n.ts`, `locales/`) |
| `crates/swiss-panel/panel/test/` | Vitest (happy-dom): behaviour tests and the gates (§panel.ui, §panel.i18n, §panel.lint) |
| `crates/swiss-panel/src/admin_assets/` | What is served: `index.html` (the shell and the SVG sprite), `ui.html` (the gallery), `styles/{base,ui,views}.css`, `js/` (the committed emit, §panel.toolchain), `js/vendor/` (§panel.vendor), `logo.svg` |
| `crates/swiss-panel/src/admin.rs` | The embedded asset server |

**Serving** (`admin.rs`). `rust-embed` over `admin_assets/`: a debug build reads every file from
disk per request (edit, emit, reload — no gateway restart), a release build compiles them in.
`admin_html()` is served `no-store`. `mime_of` is a whitelist: a file whose type is not on it
is refused, never guessed. `admin_asset` guards the path (a traversal is a 404). The **panel
version stamp** is a SHA-1 over every file's path and content length, handed to the panel by
`/api/info`; a panel that sees the stamp change reloads itself, so a rebuilt gateway is never
driven by a stale tab. After `npm run build` a release build only notices changed assets when
the crate recompiles: `touch crates/swiss-panel/src/lib.rs`.

**The rendering contract.** The page body is rebuilt only by an explicit load or a user action,
never by a poll and never while something in it has focus. A poll patches in place — the
sidebar rows, the detail header, a status cell — so a caret, a scroll position, an open menu
or a half-typed field survives every tick.

**Polling.** One `setInterval(poll, 6000)`; a tick is skipped while the tab is hidden, and
returning to it polls at once (`visibilitychange`). A poll is the memory reading
(`/api/memory`, §host.memory), `/api/info` (the version stamp) and the active page's own
`poll()`. The memory chip is a reading, not a reload button: clicking it refreshes the reading
only. The one explicit view refresh is the `r` key (`refreshNow`), which keeps Data's manual
reload alive.

**Pages.** A view module exports a `PageModule`:

```ts
interface PageModule {
  mount?: (ctx: { signal: AbortSignal }) => void | Promise<void>;
  unmount?: () => void | Promise<void>;
  poll?: () => void | Promise<void>;
  refresh?: () => void | Promise<void>;
  canLeave?: () => boolean;          // false = navigation, a language flip, a reload are held
  countText?: () => string;          // the context bar's count readout
  hasPendingChanges?: () => boolean; // the beforeunload guard
}
```

`mount` gets an `AbortSignal` that fires on leave; listeners and fetches hang off it. A late
module load that lost the race to a newer navigation is discarded (a sequence ticket). The
`beforeunload` guard asks the browser to confirm while `hasPendingChanges()` is true (staged
Data edits, an unsaved form).

**The API helper.** `api(path, opts)` adds the JSON content type; a 401 from `/api/*` is the
session gate (§host.session) and sends the tab to `/`, which answers with the sign-in page.
`apiJson<T>()` is the one fetch → parse → report path for a mutation: a failure toasts the
API's own `{error}` (or "HTTP N"), a network failure toasts "is the gateway running", and
both return `null` — every caller is `if (!j) return;`, and none of them may fail silently.
API types are copied from the Rust serde structs field by field (§panel.toolchain).

**Theme.** `swiss_theme` holds the preference — `light`, `dark`, or absent for auto (the OS's
`prefers-color-scheme`, followed live). The result is `<html data-theme>`. The button shows what
a click switches to (a moon on light, a sun on dark) and the first click pins the choice for
this browser. Blocked storage keeps the choice for the page load.

**Keyboard.** The shell listens at `document`. Escape closes the outermost layer first: sheet,
then menu, then the MCP run history popover, then Focus mode. Outside a text field: `r`
refreshes the view; on the sidebar page `/` focuses the filter, ↑/↓ walk the rows actually on
screen (a folded group is stepped over) and Alt+↑/↓ move the selected MCP through the list
(the drag-free reorder). A layer that handles a key stops its propagation. A sheet is modal:
its keys stay in it, Escape aside (`ui/sheet.ts initSheet`).

**Browser storage.** Panel-only preferences; nothing a store needs lives here, and every read
and write tolerates blocked storage.

| Key | Holds |
|---|---|
| `swiss_theme`, `swiss_lang` | Theme and language preference (absent = auto / English) |
| `swiss.lastPage` | `{pluginId: pageId}` — the last page per plugin (§panel.nav) |
| `swiss.rail.pinned` | The rail's pinned plugin list |
| `swiss.tokenId` | Which token the copied connect commands embed |
| `swiss.groups.<scope>.collapsed`, `swiss.groups.<scope>.last` | Fold state per group (carried across a rename) and the last group used (§panel.groups) |
| `swiss.dbGrid.<conn>_<schema.table>`, `swiss.dbSqlHistory`, `swiss.dbFavorites` | Data column widths, SQL history, favourites (§data) |
| `swiss.terminal.fontSize`, `.bell`, `.copyOnSelect`, `.hint` | Terminal preferences (§terminal.panel) |

### §panel.nav — Navigation, the shell and page layouts

**Four levels, each with one owner.** A page never invents another L1 or L2 mechanism.

| Level | What | Owner |
|---|---|---|
| L0 | The app | — |
| L1 | A plugin (MCP, Tunnels, Data, Jobs, Terminal, Remote, Settings…) | The **rail** |
| L2 | A plugin's pages (MCP → Servers / Traffic / Token) | The **context bar** |
| L3 | Page-local navigation (an MCP's Tools / Resources / … ; Data's objects; Terminal's sessions) | The **page body** |

**Navigation is data.** The panel has no hard-coded page list: pages come from the plugin
descriptors (§host.plugins) through `GET /api/plugins`, each a `PageDescriptor` — `id`,
`pluginId`, `label`, `order`, `path` (`#id`), `entry` (the view module's URL), `sidebar`,
`layout`. The host pages are added by the panel itself: Plugins (order 1000), Secrets (1001)
and System (1002), `pluginId: "host"`, shown as the Settings plugin. The built-in pages:

| id | Plugin | Label | Order | Layout |
|---|---|---|---|---|
| `mcps` | mcp | Servers | 10 | resource (`sidebar: true`) |
| `traffic` | mcp | Traffic | 20 | page |
| `tokens` | mcp | Token | 30 | page |
| `tunnels` | tunnels | SSH Connections | 30 | page |
| `tunnel-forwards` | tunnels | Port Forwards | 35 | page |
| `data` | data | Data | 40 | workspace |
| `jobs` | jobs | Jobs | 50 | page |
| `terminal` | terminal | Terminal | 70 | workspace |
| `remote` | remote | Targets | 75 | page |
| `remote-runs` | remote | Runs | 76 | page |
| `plugins`, `secrets`, `system` | host | Plugins, Secrets, System | 1000–1002 | page |

- **Grouping** (`page-core.ts groupPages`): one group per plugin; a group sorts by the
  **smallest** order among its pages, pages sort by order inside it, ties keep registration
  order. A plugin that is disabled, failed, waiting for a dependency or not built keeps its
  pages registered and marked unavailable: its seat stays, and its page paints that state.
- **Routing** is the hash: `#id`. An unknown hash lands on the first page; no hash is `#mcps`.
  A `hashchange` to a registered id navigates; `canLeave()` returning false puts the old hash
  back and stays.
- **Layouts** (`layoutOf`) frame the page body and nothing else — the shell always draws the
  rail and the context bar. `resource`: master-detail, the shell's sidebar holds the list;
  `page`: a content page under a measure; `workspace`: full-bleed, no padding, no measure, no
  scroll of its own. The descriptor states it; absent, `sidebar: true` means resource and
  anything else page. A workspace is never guessed, and every built-in states its layout.
- **`sidebar: true`** means "this page renders into the shell sidebar's layout" — only `mcps`
  does. The shell hides the sidebar for every other page.
- **A legacy manifest** (the seven pre-plugin pages) stands in when `/api/plugins` answers 404.
  The embedded panel always meets its own gateway, so the branch is dead weight kept for a
  debug build pointed at an old tree; a removal candidate.

**The shell** is invariant and owns: the rail, the context bar, the global readouts, Focus
mode, the pane's scrolling (its pinned-head measurements and back to top, `pane-scroll.ts`)
and app-level responsiveness. A page owns only its body.

```text
┌────────┬────────────────────────────────────────────────────┐
│ Plugin │ Context bar                                        │
│ rail   ├────────────────────────────────────────────────────┤
│        │ Page body                                          │
└────────┴────────────────────────────────────────────────────┘
```

**The rail** — the only L1 navigation.

- 56 px wide, no edge of its own (beside a page the ground change is the edge; beside the MCP
  sidebar the two are one surface). One 42 px seat per pinned plugin: an 18 px glyph over its
  name.
- **The caption is one size for the whole rail**: 10 px, stepped down together in half-pixel
  steps to a 9 px floor by `fitRailLabels` only when a served name is longer than the seat; a
  name that fits nowhere ellipsizes and keeps its full text in the seat's `title`. The rail
  is never icon-only.
- **Active seat**: the `--hover` ground plus a 2 px `--accent` notch on the leading edge — the
  chrome's one accent.
- **An unavailable plugin keeps its seat**, `aria-disabled`, the reason in its title; its page
  is the empty state, never a blank.
- **The pinned shortlist**: at most seven seats (`RAIL_LIMIT`), the choice stored in
  `swiss.rail.pinned`. The `⋯` seat (glyph only) opens the **plugin palette** — the complete,
  searchable list. More plugins never widen the rail or add a second app sidebar.
- A seat carries `data-group` and `data-view`; which page a click opens is computed per click:
  the plugin's **last page** (`swiss.lastPage`, set on every navigation, `targetPageFor`), or
  its first page when the remembered one is gone.
- The brand mark is `logo.svg`. Plugin pages never appear as separate seats.

**The context bar** — always present in normal mode, 40 px, including single-page plugins
(the body's vertical origin never jumps between plugins).

- The title is the plugin's glyph (16 px) and name. A multi-page plugin adds
  `<nav id="pageTabs" aria-label="Pages">`: one underline tab per page, a real
  `<a class="ctx-tab" href="#id">` with `aria-current`, the current one marked by a 2 px accent
  on the bar's own bottom hairline. It is navigation, not a tablist. A single-page plugin
  draws the title alone — no one-entry tab strip.
- **Overflow**: `fitTabs()` (pure) plus a `ResizeObserver` move what does not fit into a `⋯`
  tab that lists every page; the current page is always visible.
- **At most five pages per plugin.** A plugin whose bar needs the `⋯` at normal widths is
  spending L3 content on L2; restructure its pages.
- **Readouts** at the right: the page's `countText()`, a `·`, the memory chip (mono,
  `--f-caption`), all `--text-3`. Then the language flip (文/A, §panel.i18n), the theme
  button, and Focus at the far right.
- The underline tab is the L2 shape; the pill `seg()` inside a body is the L3 shape. The two are
  never interchangeable.

**Focus mode** (`immersive.ts`) — the shell's, not browser F11. Entry and exit are the same
control at the far right of the context bar. The rail folds away and the bar shrinks to a
minimal in-flow app bar holding only the app zone (language, theme, Focus) — the title, the
tabs and the readouts hide — so shell controls never cover page actions. A workspace may
declare `[data-shell-focus-slot]`: Terminal hosts the same app zone in its own toolbar (moved,
never cloned) and the bar collapses completely. Escape exits last in the Escape chain. Every
mode change dispatches `resize`, so Terminal and Data remeasure. A page never adds its own
fullscreen control.

**Body templates** (`pane()` / `paneBody()` draw them):

| Template | For | Shape |
|---|---|---|
| Content | Lists, settings, management (Tunnels, Jobs, Traffic, Token, Settings) | `paneBody({wide})` + `paneHead`; the measure; the head pins; its actions at its right |
| Resource | Master-detail (MCP Servers) | A source list beside a detail whose `resHead` pins the name row and the resource's tabs while its description scrolls away |
| Workspace | Dense tools (Data, Terminal) | `pane({full})`: full-bleed, no padding, no measure, no scroll; still below the context bar and beside the rail |

A workspace is never wrapped in a giant decorative card: the shell is already its boundary.

**Action placement** — one hierarchy, the same corner on every page: app-global → context
bar; page → the page head's right; selected resource → the resource head; row → the row's
end or its `⋯`. A body header exists only when it adds identity, a description, the primary
action or filter state — never to repeat the location the bar already shows. Data's object tab
strip is L3 made visible, not a new layer (§data.tabs).

**Responsive.** Narrow widths never invent a different hierarchy: L1/L2/L3 keep their shapes,
dense bodies stack, the location and Focus stay reachable, the app never scrolls sideways.
At 960 px the rail does not clip and rows wrap their sub-line instead of overflowing.

### §panel.design — The design language

A management UI in the Apple System Settings / Linear lineage: a source list beside a detail
pane for the thing with many instances, a left-aligned page under a measure for the rest,
grouped inset lists, one primary action per view with the rest behind `⋯`, a five-step type
ramp, four weights, a 4 pt grid, hairlines instead of shadows. Restraint is the style: no
glass, no gradients, no large radii, no second accent. **A thing on screen is one surface
with one edge**; what the user does most is reachable without hovering, what they do rarely
or destructively waits behind `⋯`; the page teaches by its structure, and prose that explains
a gesture means the structure failed.

**Tokens** — the block at the head of `styles/base.css`, whose header comment states the rules
everything below derives from. Every colour, size, weight and radius is a token, never a
literal (G4); a new component size lands in the block, named, before any rule uses it. Dark is
the same names under `:root[data-theme="dark"]`: a rule written once, in tokens, is themed.

| Family | Tokens |
|---|---|
| Type | `--f-title 22`, `--f-head 15`, `--f-body 13`, `--f-label 12`, `--f-caption 11`; `--sans` (the system stack, Inter first where installed, never shipped; judged in Segoe UI Variable and SF), `--mono` (the system mono stack) |
| Weights | `--w-body 400` running text, `--w-name 450` identity in a list, `--w-emph 500` a selected tab / a button / a caption / a band, `--w-title 600` titles — **four and only four** (G3); UA bold is re-pointed to `--w-emph` |
| Spacing | `--s1`…`--s8` = 4/8/12/16/20/24/32 |
| Radii | `--r-card 8`, `--r-row 6`, `--r-btn 6`, `--r-pill` |
| Sizes | `--ic-s 12` (a chevron), `--ic-m 14` (a glyph in a button), `--dot 6`, `--row-h 42`; icon size and ink through the `--ic` / `--ic-ink` knobs |
| Surfaces | `--bg`, `--sidebar`, `--bar`, `--card`, `--field`; `--hover` |
| Text, lines | `--text`, `--text-2`, `--text-3`; `--sep`, `--sep-soft` |
| Colour | `--accent`; state `--green`, `--red`, `--amber`; syntax `--syn-*` (code blocks only) |
| Terminal | `--term-*`, derived from the panel's colours (§panel.pages) |
| Measures | `--measure 920`, `--measure-wide 1180` |

**The rules.** Numbered, and cited as "rule N" in code comments.

1. **Monospace is for values you would copy** — a path, a command, a key, JSON, a tool name —
   never for prose identity. `tnum` for every number in a column.
2. **Saturation is for state**: dots, the one accent, error red, amber. Type chips, tags,
   group names and descriptions are monochrome; a column's annotation is `--text-3`. The one
   exception is syntax colour inside a code block (the JSON view, the SQL highlighter), in the
   five muted `--syn-*` tokens only.
3. **Text needs a measure.** Nothing is full-bleed except a workspace; content hugs the left
   edge and is never centred a second time.
4. **One primary action per view; the rest behind `⋯`.** A row holds at most one non-icon
   button. Red never appears on a row: destructive items are last in the overflow, after a
   separator, in `--red` text, or the row's trailing glyph. (The System page's Quit is the
   page's one act and stays red.)
5. **Hierarchy is surface and indent, not size and caps.** A user-named container is a header
   band over its members (§panel.groups). Uppercase 11 px captions are for sections the product
   named, never for containers the user named.
6. **Every "new" says where it goes**: a Group field prefilled from the header `+` that opened
   it or the last group used; the title carries the group ("New job in *learn*").
7. **One glyph, one meaning per page** (`folder-plus` for a new group, `plus` for a new item).
8. **One persistent glyph per container header, and the whole header drags**: `+` always
   visible (dimmed), `⋯` on hover/focus; its buttons cancel the drag at `dragstart`.
9. **Icons are the sprite**: Lucide-style, 24 viewBox, 1.5 stroke, `currentColor`, hand-written
   `<symbol>`s in `index.html`, drawn by `iconNode(name)`. No Unicode glyphs as icons, no icon
   package; the gallery shows every symbol.
10. **Status is a dot plus neutral text**: `dot(state, words)`, 6 px; filled green up, filled
    red down, a hollow ring for "starts on demand"; every dot has a `title` in words. A
    failure in a list is a red `tag()`, not a red row.
11. **Empty states use one template**, `emptyNode({icon, title, hint, action})`. An empty
    container is one quiet row the height of a real one ("No items — drop here or press +");
    a list filtered to nothing is one `note()` line.
12. **Surfaces are a grey ladder**: cards lift by a hairline ring; shadows belong to floating
    layers (sheet, menu, back to top). Dark is a warm near-black read as paper.
13. **Motion is 150 ms ease or nothing**, and nothing under `prefers-reduced-motion`.
14. **Copy is short and declarative, in both languages**: no exclamation marks, no "please",
    no emoji; a confirmation states the consequence and what is *not* destroyed. Every visible
    string goes through `tr()` (§panel.i18n).
15. **Selection is a bar and a tint**: a 2 px `--accent` leading edge over an 8 % accent tint;
    the selected name goes `--w-emph`, not blue.
16. **One surface, one edge**: a container is drawn once — never a label and a rail and a box.
17. **Air is a token step, never zero**: `--s5` after a page header and between a form and its
    list, `--s6` between sections, `--s3` between cards, fields and pairs, `--s1` between a
    band and its first member. Two blocks touching means a rule is missing.
18. **Trailing controls share one column**: a row's last glyph and its band's sit at the same x.
19. **The frequent gesture needs no discovery**; hover-only affordances are for the rare.
20. **Structure teaches; prose confirms**: a page description is one sentence on one line at
    the measure, never an explanation of an interaction. Placeholders are sentence case and
    name the value; a form never calls a field optional that the store will refuse.
21. **Four weights** (above).
22. **Lines are inset, lists are open**: a separator inside a card starts at the text column;
    the event list draws no box and no row lines; a data grid draws no vertical rules.
23. **The head stays**: page heads and resource heads pin; their hairline appears only once
    content is under them. Nothing re-implements a sticky header.
24. **Every "this happened" list is the event list**, `timeline()` (§panel.ui).
25. **One screen never repeats itself**: no count the context bar already shows, no per-row
    meta that is the same on every row, no caption repeating the tab above it.

**Accessibility.** Focus is always visible; an icon-only button has an `aria-label`; a dot has
a `title`; Enter submits a sheet's primary; arrows move where a list is a listbox.

**Verification.** A visual change is walked on a test instance (§testing.live) in light and
dark, English and Chinese, at about 1440 px and 960 px — never on the user's instance.

### §panel.ui — The UI library

The panel has one component library, in the tree (ADR-029). Every shape a page draws is a
`ui/` function; a page composes them and never writes a library class into an `h()` class
literal.

- **Where.** `panel/src/ui/*.ts`, exported whole by `ui/index.ts`; its classes, and only
  those, in `styles/ui.css`. No npm package, no separate crate, no third-party component
  library.
- **The boundary** (G1): `ui/` imports only `../h.js`, `../i18n.js` and `./*` — no API, no
  state, no views. That is what lets the gallery render every component with nothing else
  loaded.
- **Three categories.** Pure markup lives in `ui/`; a mechanism that depends only on the DOM
  (menus, sheets, the select, `toTop`) lives in `ui/`; behaviour bound to the app (the groups
  component's API calls and state, `groups.ts`) stays with the app and is built on the library.
- **CSS in three layers**, loaded base → ui → views: `base.css` = tokens, reset and the shell
  (rail, context bar, sidebar); `ui.css` = every component's classes; `views.css` = a
  workspace's skeleton and page-local layout the library has no shape for. `views.css` never
  restyles a library class (G2).
- **Library first.** A shape a page lacks goes into the library before the page uses it: the
  function in `ui/`, its classes in `ui.css`, a gallery section, a test in
  `ui-components.test.ts`. A page that needs a missing shape stops until the library has it.
- **A design is a gallery scene**: a new page or a redesign is drawn in `ui-scenes.ts` from
  `ui/`, `h` and `i18n` with made-up data, reviewed in the gallery, then built. Never a
  hand-written HTML mock with its own style sheet.
- **Migrating deletes.** A page moved onto the library loses its old CSS and class names in the
  same change; no compatibility class survives, and the gate rows go down with it.
- **The gallery** — `/admin/ui.html` (`ui-gallery.ts`, `ui-scenes.ts`; it borrows the sprite
  from `index.html` at boot): every export in every state, with `?theme=dark&lang=zh&w=960` in
  the query string. It ships in the binary, reached by typing it; it is not in the rail.
- **Back to top** (`toTop(scroller)`): the shell mounts one on `#pane`; it appears past one
  screen of scroll and glides back (jumps under reduced motion). A workspace never scrolls, so
  never shows it.

**The vocabulary.** Specs and class names use these words; a design that needs another adds it
to the library first.

| Word | Function | Classes |
|---|---|---|
| pane, page head, resource head | `pane({wide, full})`, `paneBody({wide})`, `paneHead({desc, sub, actions})`, `resHead({title, desc, sub, actions, nav})` | `.pane`, `.pane-head`, `.pane-head.res`, `.pane-nav` |
| section, card | `section({cap, note, tools}, …)`, `card(…rows)` | `.sec*`, `.group` |
| row | `row({lead, name, sub, err, cols, toggle, primary, more, detail})`; `kvRow(label, value, {mono})`; `sideRow({name, lead, tail, selected})` | `.lrow*`, `.kv*`, `base.css .side-row` |
| band | `groupNode(…)` (mounted by `groups.ts`) | `.grp`, `.grp-head` |
| segmented control | `seg(items, current, {key, label, fill})` — L3 only | `.seg` |
| event list | `timeline(items, {open, body})`, `timelineMeta(parts)`, `timelineToggle()` | `.tl*` |
| code block | `jsonCodeNode(value, all, {oneLine})`, `textNode()`, `valueBlock({label, notes, tools, text}, …)` | `pre.jv`, `pre.logs`, `.vblock*` |
| form | `form`, `field({label, control, required, meta, hint, action, group})`, `checkField`, `pair`, `formActions`, `formCap`, `formFold`, `hint`; `inlineForm(…)` | `.form`, `.fld`, `.two`, `.inline-form` |
| filter, pager, note | `filterInput(…)`, `pager({label, status, prev, next})`, `note(body, {busy, err})`, `failNote(…)` | `input.filter`, `.pager`, `.note` |
| status | `dot(state, words)`, `heldDot(words)`, `tag(text, {mono, tone})`, `spinner()` | `.dot`, `.tag`, `.spin` |
| controls | `sw(on, label)`, `btn(label, {kind, icon})`, `iconBtn`, `moreBtn`; native `<select>` styled by `initSelects` | `.sw`, `.btn`, `.dd*` |
| layers | `popupMenu(anchor, items)`, `anchoredMenu(host, items)`, `sheet({title, sub, body, foot})` + `showSheet`, `stackSheet`, `openFieldSheet(spec)` | `.menu`, `.sheet*`, `.backdrop` |
| page furniture | `emptyNode(…)`, `pageFoot({note, rev})`, `toTop(scroller)`; the toast is the shell's (`util.ts toast()`) | `.empty`, `.page-foot`, `.to-top`, `.toast` |

**The event list** (`timeline`) is every "this happened" list — MCP Logs (the reference),
Traffic's activity, Remote runs, a job's runs: the time in its own column (the date is a day
heading, never repeated per row, and it sticks under whichever head is pinned), the title, one
line of arguments, the who column only when it varies, ×N for consecutive identical items, a
failure as a red tag, the duration right-aligned (amber past a second). What every row says
the same moves into the open row's meta line.

**The gates** — all in `panel/test/`, all inside `npm run check`:

| Gate | Test | Fails when | Floor |
|---|---|---|---|
| G1 | `ui-boundary` | a `ui/` module imports anything but `../h.js`, `../i18n.js`, `./*` | zero |
| G2 | `ui-css-ownership` | `views.css` makes a `ui.css` class the subject of a selector (`svg`/`use` count as `.ic`) | `FROZEN_VIOLATIONS = 0` |
| G3 | `css-weights` | a `font-weight` is not one of the four `--w-*` tokens | zero |
| G4 | `css-literals` | a sheet gains a px / hex / rgb literal outside its token block (`0`, `1px`, `2px`, `-1px` and `%` exempt) | base 22, ui 72, views 159 |
| G5 | `ui-class-ratchet` | a file outside `ui/` draws a `ui.css` class (a file with no row is held at 0) | the table is empty |
| G6 | `ui-gallery` | an export of `ui/index.ts` is claimed by no gallery section or scene (or by `HELPERS` with a reason), a claimed shape is missing from the DOM, or a class the gallery draws is unstyled | zero |
| G7 | `css-size` | `views.css` grows past its byte ceiling | `FROZEN_VIEWS_BYTES = 59932` |

**The ratchet convention** (every frozen table in the panel, and §panel.lint's): the table
lives in the test, its numbers only go down, and a change that lowers a count lowers the row
in the same commit. A new file defaults to zero.

### §panel.pages — Per-page look rules

What each page does with the library, beyond the rules every page obeys.

**Everywhere.** Thin scrollbars: 10 px, transparent track, no arrows, a rounded thumb
(`scrollbar-width: thin` in Firefox; Terminal re-points `--scroll-thumb`). A content page's
`paneHead` pins and `pane-scroll.ts` adds `.pane.scrolled` (the hairline) once something is
under it. Back to top sits below the menu (z 30), the sheet backdrop (40) and the toast (50),
and is relabelled by the chrome repaint.

**MCP Servers.**

- The resource head pins in two layers (name row, then the tabs); a `ResizeObserver` measures
  them into `--pin-title-h` / `--pane-head-h` so the day headings stick beneath. The detail is
  `.wide`.
- **Logs** is the reference event list: the time only, day headings, a duration column; via /
  client / size in the open row's meta line; ×N for the same tool + arguments + result, folded
  per page; which rows are open survives a poll; bodies load lazily.
- JSON of 80 characters or fewer renders compact on one line. One visible copy button; Copy raw
  behind `⋯`.
- Tools, Resources and Prompts are `row()`s with a `<code>` name, Try and an enable toggle; the
  detail is a native `<details>`; disabled tools get their own section.
- **Run**: the form is built from the input schema (`argFieldsNode`, shared with Jobs); the
  result is a `valueBlock` (past 200 lines: plain text scrolling at 60 vh).
- **Config** is `kvRow`s; revisions are rows (Restore and a trash glyph); a tunnel dependency is
  a row with a dot.
- `.two` is always a grid, and `:is(.fld,.two) + :is(.fld,.two)` spaces them.

**Traffic.** No page title. An Actions / Everything `seg` on the Activity section; Clear
behind `⋯`. The activity is an event list whose who column is client · server; the client
table sits in a card. The page loads the MCP list for its count.

**Token.** A one-line description; `folder-plus` for New group; the token id in mono;
`row()`s with a tag. The one-time secret shows once, in a form card.

**Tunnels.**

- SSH Connections: the rule count is a column; no count at the bottom.
- Port Forwards: New, `⋯` (Start all / Stop all) and `folder-plus` in the head; the local port
  is a column; an error is a red sub-line.
- Sheets are `sheet()` + form, with `formCap`, `formFold` and `field({action})` for Browse.
  `stackSheet` opens a sheet over a sheet with its own backdrop; Escape closes only it.
- The tunnel dot: an amber pulse while reconnecting, grey when stopped (`dot(state, null)`);
  rows show drag feedback.

**Jobs.**

- Row columns: the schedule (`schedToBody().say`, spoken by cronstrue, the raw expression in
  the title), the next run, the last run (`relTime` via `Intl.RelativeTimeFormat`: "OK · 8 hr.
  ago", a red tag on failure, "Never run").
- A disabled job gets an Off tag. No leading dot, except an amber pulse while it runs.
- The runs sheet is an event list: the trigger as title, the first output line as argument, a
  failure as a red tag ("exit 1"), a skipped run as an amber tag with no duration, ×N.
- `field({group})` renders `role=group`; the sheet body and `.tl` use `minmax(0, 1fr)`.
- A stopped Jobs plugin shows `emptyNode` with the clock icon.

**Remote Targets.** The head's sub-line counts the endpoints served by tunnels. A row's sub
is label · endpoint · the root (mono) · capabilities. A dot only when not resting (amber
connecting, red error). The last-run column (No runs; a live pulse while running or queued;
`relTime` with a red tag on failure) is fed by `/api/remote/runs` plus one
`?limit=1&target=` read per uncovered target once per page entry; an API error drops the column
silently, and the poll patches the cell. Editing a target always sends its `id`.

**Remote Runs.** An event list whose who column is target · source; the title is the action,
the argument the command (a sync reads `src → dst`). A failure is a red tag; a live run is an
amber pulse (`live: {text, queued}`). The open row: the meta line, the Output `valueBlock` (a of
b, Load more), the tail block, and live output with Cancel. The head's actions are the target
filter select and `⋯` (Clear, red); a `pager()` below.

**Data.**

- Column headers have three lines: the name (PK / FK / sort marks), the type (`.db-col-type`,
  capped at 320 px with an ellipsis) and the comment (`.db-col-comment`, `--text-3`; an empty
  one still holds its line). Comments are `--text-2` in the hover card and the Columns tab.
- Selection is `.db-table.sel`: the 8 % accent tint, a 2 px inset bar, `--w-emph`. The drawers
  use a tick column.
- The grid draws no vertical lines; `td` padding 6 px, `th` 4 px. A row's checkbox and delete
  show on hover, focus, checked or staged (opacity 0, still tabbable). An inserted row always
  has its ×.
- `seg()` for the section switches, `popupMenu` (`.menu.float`) for context menus, `tag()` for
  chips; one `⋯` per head; the status bar does not repeat the editable note. Sheets use
  `sheet({sub})`; a staged write is a `heldDot`; a disabled primary keeps its accent, faded.
- The Redis key's `⋯` offers Set TTL… / Rename… / Delete… (one confirm naming the key) and
  Command. `openFieldSheet` takes `sub` and `allowEmpty` (an empty TTL is PERSIST). A typed-name
  confirm stays only for DROP and TRUNCATE. Stream Follow is a `sw()` switch.

**Terminal.** The stage has its own tokens: `--term-*`, dark `#0b0c0e` (one step below
`--bg`), and a dark stage `#17181b` on the light theme; `.term-page` re-points the panel tokens,
while the accent and the state colours stay the panel's. xterm's background, cursor and
selection come from those tokens (`termTheme` / `readTermTokens` / `applyTermTheme` in
`terminal-core.ts`, re-applied by a `MutationObserver` on `data-theme`); the 16 ANSI colours
are untouched. Session tabs are object tabs (a bell is a `heldDot`); the target picker is a
plain styled select; `.term-jump` sits at z 3; the count is "N live" through `trn`.

**Settings** — §panel.settings.

**Known gaps.** The i18n ratchet does not scan function return values (§panel.i18n).

### §panel.groups — The grouped list

The one grouped-list component for all seven scopes (§host.groups): `groups.ts` (the DOM and
the API calls), `group-logic.ts` (the pure half the tests pin), `ui/group.ts groupNode` (the
drawing). Everything a grouped list does lives here once; no page carries a private copy. The
caller keeps what is
genuinely its own: the row markup, what opening a row does, the flat order it stores and the
noun the delete confirm names.

**Anatomy.** A header **band** over the members, in two densities — the band is what says
"container"; no folder glyph, no guide line, no second surface.

```text
Page (Content template) — .wide measure
┌──────────────────────────────────────────────────────────────────────┐
│ one-sentence description, --text-2                  [⊞]  [Primary]  │  paneHead, pinned; [⊞] = New group
│ one status line, --f-label                                           │
│                               ─ --s5 ─                               │
│ [ Field ……………………… ] [ Field … ] [ group ▾ ] [ Store ]                │  inlineForm (pages that create in place)
│                               ─ --s5 ─                               │
│ ┌──────────────────────────────────────────────────────────────────┐ │
│ │ ▾ default  3                                             +   ⋯  │ │  36 px band, --sep-soft; the whole band drags
│ ├──────────────────────────────────────────────────────────────────┤ │
│ │ ● name                                          [One btn]    ⋯  │ │  row(); its ⋯ lines up with the band's
│ │   mono sub-line                                                  │ │
│ │   ├──────────────────────────────────────────────────────────────┤ │  inset separator, from the text column
│ │ ○ name                                          [One btn]    ⋯  │ │
│ └──────────────────────────────────────────────────────────────────┘ │
│                               ─ --s4 ─                               │
│ ┌──────────────────────────────────────────────────────────────────┐ │
│ │ ▾ test  0                                                +   ⋯  │ │
│ │   No items — drop here or press +                                │ │  a 42 px empty row, not a caption
│ └──────────────────────────────────────────────────────────────────┘ │
│ page foot: prose left, the revision (mono) right                     │
└──────────────────────────────────────────────────────────────────────┘

Sidebar — the same band, smaller, no card
┌────────────────────────────────┐
│ ▾ default  9             +  ⋯  │  28 px band, --r-row
│   ● name-a                 ⌗   │  sideRow() 30 px, one grid step (--s4) in
└────────────────────────────────┘  groups --s2 apart
```

- **The band**: chevron, the name (`--f-body`, `--w-emph`, mixed case), the count
  (`--text-3`, tnum), then `+` (always visible) and `⋯` (hover/focus: Move, Rename, Delete).
  Alignment: on a page the chevron sits in the rows' dot column and the name over their names;
  in the sidebar a member's dot sits under the band's name.
- **Drag.** The whole band moves the group (grab cursor; its buttons opt out at `dragstart`).
  A group lands before or after the whole group under the pointer; a row lands on another row
  (reorder and re-home in one gesture), on a band or on an empty line (append). Feedback:
  `.grp.drop-before/after`, a `.drop-into` ring, `.grp.dragging` dims the group. Moving a
  member between groups never rewrites the flat order (`slice()` / `groupOf()`).
- **Folding** is per scope in `swiss.groups.<scope>.collapsed`, keyed by group name and carried
  across a rename (a renamed group never springs open). The map is prototype-free: a group or
  a keyspace called `constructor` or `__proto__` folds like any other. A filter shows every
  group open.
- **Creating** (rule 6): the create form's Group select lists the scope's groups plus "New
  group…"; it is prefilled from the band's `+` that opened it, else the last group used
  (`swiss.groups.<scope>.last`), else the first. The title names the group.
- **Deleting a group** confirms with the consequence and what survives ("Its 3 jobs move to
  'default'. Nothing is removed."); a member of a deleted group falls to the first group.
- **One request shape**: `saveGroupNames` / rename / assign / `saveOrder` over
  `/api/groups/{scope}` (§host.groups); every write reports its failure.

**Rejected**: a transparent tree head with a folder glyph and a guide line (three visual things
for one container, and at page width the `+` a screen from its name); a hover-only grip as the
drag handle (the user could not find it); a red Delete on every row.

### §panel.settings — Plugins, Secrets, System

Host-owned pages (`pluginId: "host"`, the Settings plugin), always reachable: a disabled plugin
cannot serve the page that re-enables it.

**Plugins** (`views/plugins.ts`) — one row per plugin with its enable switch.

- Enable and disable carry the host revision: a stale one is a visible 409. A plugin that fails
  to start is not a failed request — 200 with state `failed` and `lastError` — and Enable again
  is the retry.
- A dot only when the state disagrees with the switch: an amber pulse in flight, red on failure
  with `lastError` as the row's error line. An unmet requirement is a warn tag ("no provider")
  on the sub-line. The sub-line counts the plugin's pages (the list in its title); the page
  foot is the revision.
- The poll patches rows in place, so a switch keeps its focus.
- A **Start at sign-in** section drives `/api/autostart` (the OS store, no revision;
  §host.cli).

**Secrets** (`views/secrets.ts`) — the vault (§host.vault). Names only, write-only: a value is
never shown again, and a forgotten one is re-stored, never revealed.

- A row shows the **reference** to copy (`${secret://name}`, a Copy ref ghost button).
- Writes carry the vault revision.
- Groups through the groups component; rows drag within and across groups; the stored order is
  the vault's third list (empty = by name).
- Create in place: an `inlineForm` with a Group select, preselected by the band's `+`.
- Replace value lives in the row's `⋯`: a store under the same name, after which the gateway
  rebuilds every MCP that references it and the toast names them.

**System** (`views/system.ts`) — the one running process: one `row()`, and Quit through a
confirming sheet. Quit stays red: it is the page's one act (the rule 4 exception).

### §panel.i18n — Languages

English and Simplified Chinese. The scope is the **panel's own copy**: a string the server
sends is data, shown as sent inside a translated frame (`{name}` placeholders); identifiers —
names, keys, SQL, paths — are never translated.

- **Keys are symbolic**, `<module>.<semanticId>` (`addSheet.addMcp`), in `locales/en.ts` and
  `locales/zh.ts` (TS sources, about 1,800 entries each). English is a static import and the
  floor; Chinese is a lazy import. The switch to symbolic keys was one codemod
  (`panel/scripts/migrate-i18n-keys.mjs`, kept as the record; it only runs on the pre-migration
  tree).
- `tr(key, vars)` looks up the active dictionary, then English, then answers the key itself.
  `trn(n, oneKey, otherKey, vars)` picks through `Intl.PluralRules` of the installed locale
  (Chinese has only `other`). `tk(key)` is an identity marker for module-level tables: module
  top level never calls `tr`, because the language is not loaded yet. `wireLabel(served)` maps
  a gateway-served English label (a plugin or page name) to a `wire.*` key.
- `locale()` answers `en` or `zh-CN` and is what every `toLocale*` / `Intl` call receives.
- **The preference** is `swiss_lang`; absent is English. `navigator.language` is not
  consulted. `<html lang>` follows it. A top-level `await loadLocale()` runs before the first
  paint.
- **The flip** (文/A in the context bar) is `setLang` → repaint the chrome → re-navigate the
  current page, without a reload. A page whose `canLeave()` refuses blocks it, and then the
  preference is not written. Blocked storage keeps the choice in memory; a failed Chinese load
  falls back to English.
- **No context mechanism**: a string that needs different words in two places gets two keys.
- **Chinese style**: full-width punctuation after Han characters.

**The gates** (vitest, inside `npm run check`):

| Test | Fails when |
|---|---|
| `i18n-complete` | a `tr`/`tk` key, or a `trn` other-key, found by walking the AST has no entry — or an entry has no user |
| `i18n-ratchet` | a bare English literal sits in a visible position: an `h()` child (except in `code`, `kbd`, `pre`), a `title` / `placeholder` / `aria-label` prop, the first argument of `toast` / `confirm` / `prompt` / `say`, an `emptyNode` title / hint / action. Zero allowed. It is an AST test rather than an eslint rule because `no-restricted-syntax` is held by the `innerHTML` rule. Known gap: it does not scan function return values |
| `i18n-zh-style` | a Chinese value has a half-width `,` after a Han character |
| `i18n-toggle`, `i18n-shell-zh`, `i18n-load-fail`, `i18n` | The flip, the Chinese shell, the English fallback, the helpers |

### §panel.toolchain — TypeScript, the build and the emit

The panel is written in TypeScript and served as the JavaScript it erases to (ADR-024).

- **Home**: `crates/swiss-panel/panel/` — `package.json`, `build.mjs`, two tsconfigs,
  `eslint.config.js`, `src/`, `test/`, `scripts/`. Node ≥ 24.
- **The emit is type erasure, line for line.** `build.mjs` runs `ts-blank-space` over each
  `src/**/*.ts` into `crates/swiss-panel/src/admin_assets/js/`: types become spaces, so a line
  and column in the browser are the line and column in the source. No bundle, no minify, no
  source map, no rewriting of imports. It writes only changed files, skips `.d.ts`, never
  deletes and never touches `js/vendor/`.
- **The emit is committed** and marked `linguist-generated`, so the binary builds with no Node
  on the machine. It is never edited by hand. `panel-emit.test` re-emits in memory and fails
  on a stale file or an orphan.
- **tsconfig**: `strict`, `verbatimModuleSyntax`, `erasableSyntaxOnly` (nothing that needs a
  transform: no enums, no namespaces, no parameter properties), `isolatedModules`, target
  es2022, module esnext, resolution bundler, lib es2022 + dom + dom.iterable. Two projects: the
  source (no Node types) and the tests.
- **Imports** use the emitted `./x.js` specifier; types come in through `import type`.
- **A behaviour change is named and tested.** A commit that changes how the panel behaves (not
  only how it is written) says so in its message and carries the vitest case that pins it;
  deleting a test to pass a gate is never allowed.
- **Types.** The `any` budget is zero. API types are copied from the Rust serde structs field
  by field (`types/api.d.ts`), index signatures included; a field the panel reads that Rust
  does not send is a bug. The `.d.ts` files export their types explicitly. No augmentation of
  built-in types. Event handlers use `e.currentTarget`, never `this`; an error is shown through
  `errText(e)`.
- **DOM building.** `h()` / `frag()` / `fill()` build nodes; nothing writes `innerHTML`
  (§panel.lint), and there are no inline `on…` attributes. A view wires one delegated listener
  and dispatches on `data-act`. **A delegated handler resolves its record, key and buffer from
  live state at event time**, never from what the render captured: a menu built at click time
  must still act on the right object after a tab switch or a reload under it.
- **State** is six slices — `db-state`, `job-state`, `mcp-state`, `traffic`, `tunnel-state`,
  `ui-state` — each owning its data and its setters; `util.ts` exports no `state`.
- **Scripts.** `build` (`node build.mjs`), `build:check` (fail if the committed emit is stale),
  `dev` (`--watch`), `typecheck` (both projects), `lint` (`eslint .`), `test` (`vitest run`),
  and **`check` = typecheck && lint && build:check && test** — the panel's one gate.
- **Dev tools**: `typescript` ~6.0, `ts-blank-space` ~0.9, `eslint` 10 with
  `typescript-eslint` 8, `vitest` 5 on `happy-dom`, `vite` 8 (vitest's), `@types/node` 24.
  Dev-only: nothing from npm reaches the browser.
- **The dev loop**: `npm run dev` beside a debug gateway — save the `.ts`, the emit updates,
  reload the page (debug builds read assets from disk).
- **Gates.** `scripts/deploy.ps1` runs `npm run check` after clippy, so a deploy can never
  serve a panel whose committed emit predates its sources; CI has a panel job (Ubuntu, Node 24, `npm ci`, `npm run check`). Vitest runs
  capped (`--maxWorkers=4`).

### §panel.lint — The lint ratchet

`eslint.config.js`, type-aware over both tsconfigs. No prettier: formatting is by hand, like
the Rust tree. No whole-file disable; a single-line disable carries its reason.

| Scope | Rule | Level |
|---|---|---|
| src | `no-explicit-any` | error |
| src | `no-floating-promises` | error |
| src | `no-var`, `prefer-const` | error |
| src | `consistent-type-imports` (separate type imports) | error |
| src | `no-unused-vars` (`^_` arguments ignored, caught errors not checked) | error |
| src | `no-non-null-assertion` | error, plus a per-file warn allowlist frozen by `test/non-null-ratchet.test.ts` |
| src | `no-restricted-imports`: `state` from `util.js` | error |
| src | `no-restricted-syntax`: any `.innerHTML =` or `+=` | error, no allowlist |
| src | `no-unnecessary-condition` | warn — a burn-down (496 sites on 2026-09-20) that flips to error at zero |
| `src/types/**/*.d.ts` | `no-restricted-syntax`: an interface named after a built-in (`Function`, `EventTarget`, `RegExp`, `Window`, `Element`, `Array`, `String`, `Number`, `Boolean`, `Object`, `Promise`) — narrow at the call site instead | error |
| test | `no-explicit-any`, `no-floating-promises` | warn |

The non-null allowlist and every other frozen count follow the ratchet convention
(§panel.ui): only down, in the same commit.

### §panel.vendor — Vendored browser libraries

A browser library the panel needs is vendored, never an npm runtime dependency and never a
CDN:

| Package | Version | Used by |
|---|---|---|
| `@xterm/xterm` + addons fit 0.10.0, search 0.16.0, unicode11 0.8.0, web-links 0.11.0, webgl 0.18.0 | 5.5.0 | Terminal (§terminal.panel) |
| `cronstrue` | 2.52.0 | Jobs: the spoken schedule, loaded on first use |
| `shlex` | 3.0.0 | Data: splitting a Redis console line (§data.redis-console) |

- Files live in `src/admin_assets/js/vendor/<pkg>/<version>/` beside the package's `LICENSE`,
  byte-identical to the npm dist files — no patches. xterm adds `load-classic.js` and one
  `index.js` shim per package.
- The types are a hand-written mirror in `panel/src/vendor/<pkg>/<version>/index.d.ts` that
  declares only the exports the panel uses.
- An upgrade is one commit: the new directory, the import change, the old directory deleted.

## §security — The security model in one place

swiss holds the credentials a developer's machine runs on: SSH keys and passphrases, database
URLs, API keys, OAuth tokens and its own bearer tokens. The model is two walls — **the loopback
boundary** and **a device-bound sealed store** — plus a handful of rules that keep a value from
leaking between them. Each rule is specified where it is implemented; this section is the map,
and a change that weakens any line of it is a security change whatever else it does.

| Layer | Rule | Where |
|---|---|---|
| Bind | Only a loopback host is accepted; a non-loopback host is refused at config load | §host.boundary |
| Peer, `Host`, `Origin` | Every route checks all three; a refusal is `403 {error}` (DNS-rebinding defence) | §host.boundary |
| Framing | Every answer forbids framing (`frame-ancestors 'none'`, `X-Frame-Options: DENY`) | §host.boundary |
| Admin surface | Loopback **and** a credential: a single-use login token → a signed `HttpOnly; SameSite=Strict` cookie that counts only from the panel's own origin, or the rotating CLI key in `X-Swiss-Key` | §host.session |
| MCP clients | Named bearer tokens, compared timing-safe before the body is read | §host.tokens |
| Terminal stream | A 10 s single-use ticket bound to one session on top of the loopback guard, so loosening `Origin` can never loosen the terminal | §terminal.api |
| At rest | Every state file sealed (AES-256-GCM under a key derived from the device-bound master key); unix modes 0700/0600 | §formats.sealed, §formats.masterkey, §formats.home |
| Credentials in config | References only — `${ENV}` or `${secret://name}`; one resolver at every use point | §host.refs |
| The vault | Values go in and never come out; not merged into any child environment | §host.vault |
| Echo | Literal secrets in echoed config are masked with a sentinel and restored on PUT | §host.refs |
| Run output | Every value a reference resolved to (≥ 8 bytes) is masked before it reaches a buffer, a record or a log | §process.actions, §remote.security |
| Logs and the call log | Params redacted and bounded; no credential ever written in the clear | §formats.logs, §mcp.calls |
| Children | A config file is a code-execution boundary: nothing runs from viewing a page or an import preview; no shell unless the command names one | §process.actions |
| SSH | Host keys are trust-on-first-use with an explicit trust action; a mismatch refuses | §tunnels.ssh |
| Remote | A run has exactly the login user's rights; argv is quoted once by one function | §remote.security |

### §security.threats — What is in scope and what is not

**In scope** — anything that lets:

- a non-loopback peer reach the gateway or the panel (bind, `Host`/`Origin` checks, tunnels);
- a page on another local port, or any other page, use a signed-in browser's session on
  `/api/*` or frame the panel;
- a caller read a stored secret back out, or read another client's traffic or token;
- a `${secret://…}` reference, an env reference or a masked field leak its value through a
  log, an API answer, an error message or a run record;
- a proc/HTTP MCP definition, a job command or a remote target escape the process boundary
  the definition declares;
- a sealed state file be opened on a machine other than the one that sealed it.

**Out of scope:** an attacker who already runs code as the same user on the same machine (that
user owns the seal key by construction), and anything reachable only by deliberately widening
the bind past loopback — the config loader refuses it.

**Honest notes inside the boundary.**

- On Windows the seal key is DPAPI-bound to the user; on Linux the machine-id binds to the
  machine, not the user, so on a shared host another local user can derive it. macOS has no
  master-key source yet (§formats.masterkey).
- A bearer token **can** be read back in plaintext (`GET /api/tokens/{id}/secret`). Tokens and
  vault secrets are two credential classes with two threat models: a token is compared in
  plaintext anyway and sits in `managed.json`; the separate route makes the read an explicit
  act and saves a rotate before every copy of a connect command. Vault values never come back.
- No zeroize (ADR-007): resolved values must stay resident to be used; the attack surface is
  the file and the children, not the heap.
- Masking cannot catch a program that re-encodes its own secret; output access and file modes
  remain the wall behind it.

### §security.reporting — Disclosure

`SECURITY.md` is the public policy: report privately (GitHub private vulnerability reporting,
or a private advisory draft), never in a public issue; acknowledgement within 7 days, a fix or
a decision within 30; only the latest release is supported, and CVE IDs go through GitHub
Security Advisories when a fix ships. `SECURITY.md` and this section must agree — change both
in one commit.

### §security.rules — Rules for every change

- A new route inherits the loopback guard and the session check by being mounted in the
  guarded trees; an unguarded tree merged into the app is a boundary hole, and route
  registration refuses clashes and reserved prefixes (§host.routes).
- A new credential field is a reference, resolved through `swiss_core::secure::refs` at its use
  point, masked when echoed, and covered by a test at that use point (§host.refs).
- A new state file is sealed, private-moded and, when it is per-instance, keyed by port
  (§formats.home).
- A new child process goes through the supervisor or the same platform calls — job object or
  process group, the PID ledger, bounded capture, masking (§process.supervisor).
- Error messages name the reference, the field and the use point — never the value.
- Tests and fixtures use neutral names only (§release.hygiene); a test key is a documented
  fixed test key, never a real one.

## §testing — Gates and suites

A change is done when its gates are green **and** the behaviour was seen working where it
runs. Tests are the evidence; a claim without a command and its output is not.

**The gates** (every commit that touches code):

```
cargo test --workspace                                   # gate 1: unit + in-process integration; any machine
cargo test -p swiss-it --features it                     # gate 2: real databases and child processes; Docker or SWISS_IT_*_URL
cargo clippy --workspace --all-targets -- -D warnings    # add --features it when swiss-it changed
npm run check                                            # in crates/swiss-panel/panel, when the panel changed (§panel.toolchain)
```

- **`--workspace` is not optional** (§arch.crates): without it cargo builds the root package
  alone, the member crates where most tests live never compile, and the run still says ok.
- **Gate 2 is mandatory** when the diff touches `crates/swiss-mcp/src/adapters/{mysql,pg,redis}*.rs`,
  `sql.rs`, `resources.rs`, `proc.rs`, `crates/swiss-host/src/dbbrowser.rs`,
  `crates/swiss-data/src/dbbrowser_api.rs`, `crates/swiss-core/src/secure/`, `src/app.rs`,
  `src/mcp_link.rs`, or `crates/swiss-it/**`. Any other change may skip it, and the commit
  message says it was not run.
- **A manifest change** also runs `cargo tree -d -e normal,build` and `cargo deny check`
  (§arch.deps, §release.ci).
- **Deploys re-run the gates.** `scripts/deploy.ps1` runs gate 1, then gate 2 (setting
  `DOCKER_HOST` for that one run and removing it after), then clippy, then the panel's
  `npm run check`; any failure stops with "production left untouched" (§host.ops).

### §testing.layers — Where a test goes

| Layer | Location | Drives |
|---|---|---|
| Root integration | `tests/*.rs`: `adminapi` (the admin API, route by route), `app` (client endpoints, boundaries, asset serving), `plugin_host` (plugin gating, actions and runs, the catalog), `terminal_ws` (the WebSocket stream), `http_adapter` (the http adapter as a real proxy), `groups_e2e` (a real socket across a restart), `admin_session`, `env_precedence`, `envelope_compat`, `memory` | `swiss::app::build_app` + `tower::ServiceExt::oneshot`; no port unless the category needs one |
| Crate integration | `crates/swiss-mcp/tests/dbbrowser_wiring.rs`; `crates/swiss-core/tests/seal_bench.rs` (`#[ignore]`) | embedded fixtures |
| Crate inline | `#[cfg(test)] mod tests` in every module that has behaviour | pure functions, injected fakes |
| Gate 2 | `crates/swiss-it` (§testing.it) | real MySQL, PostgreSQL, Redis and a real stdio MCP child |
| Panel | `crates/swiss-panel/panel/test/*.test.ts` (vitest) | hand-built DOMs following the view layer's idioms (§panel.toolchain) |

**Locating rule.** Behaviour on the HTTP surface gets an end-to-end assertion in the root
`tests/`; pure functions and data structures live inline in their crate. Both count as "this
point has a test". `tests/adminapi.rs` is the admin API's contract suite: a new endpoint copies
a neighbour's test from there.

**Modules without inline tests, by design:**

| Module | Why, and where it is covered |
|---|---|
| `swiss-mcp` `adapters/mysql_browser.rs` | Orchestration over a live connection; the identifier quoting it relies on (`dbbrowser::quote_ident`) is tested in `swiss-host`; the paths themselves run on real engines in gate 2. The pure logic once buried between awaits was lifted out and tested (`redis_browser::scan_args`, `pg_browser::total_of`) |
| `swiss-core` `platform/windows.rs` | Win32 FFI that needs the OS to answer; the process walk both platforms share lives un-`cfg`'d in `platform/mod.rs` and is tested on any host. Both platform halves re-export from one list, so a signature divergence is a build error |
| `swiss` `adminapi.rs`, `app.rs` | Covered end to end by `tests/adminapi.rs` and `tests/app.rs`, where a route contract belongs |
| Re-export shells (`lib.rs`/`main.rs`, `secure/mod.rs`, `tunnel/mod.rs`) | No behaviour of their own |

`server.rs` is tested at `register_one` (where a panel Stop, a persisted tool toggle and the lazy
rule must survive a restart), not at the boot that binds a port and never returns.
`admin.rs`'s SHA-1 is tested against the published vectors and every block-padding boundary.
One branch is deliberately uncovered: `daemon::StopResult::Forced` — reaching it needs a live
pid to kill, and the only one a test owns is its own runner.

### §testing.rules — Environment, time and shared state

- **`SWISS_MASTER_KEY` in every test binary** — deterministic bytes set once in a `OnceLock`
  (with the SAFETY comment on `set_var`), so the suite never touches an OS key source and CI
  can run it.
- **Scratch homes** are `temp_dir().join(format!("swiss-<tag>-{}", random_hex(8)))` +
  `SWISS_HOME`; no `tempfile` crate (§arch.deps).
- **Time.** Tickets, idle reaping and stalls use `#[tokio::test(start_paused = true)]` +
  `advance()` (the paused clock creeps one second per park; `tests/terminal_ws.rs` carries the
  countermeasure). Jobs never use tokio time: they take an injected `Clock` that can
  synthesise DST.
- **No bare sleeps.** Poll at ~20 ms with a cap, or inject a clock. On Windows the long-running
  child is `ping -n 60 127.0.0.1` (there is no shell sleep). A test never spawns PowerShell.
- **Environment changes** only inside an integration binary that owns its process
  (`tests/env_precedence.rs`), restored afterwards.
- **Databases.** Unit tests stay connection-free (pure functions over defs and SQL strings);
  anything that needs a real engine belongs to gate 2.
- **Process-global state is locked; the rest is per harness.** Each test composition owns its
  traffic ring (memory-only), so no traffic lock exists. The vault is process-wide:
  `VAULT_LOCK` serialises the tests that touch it, and writes to the data dir go through
  `DATA_DIR_LOCK`. A new test that touches global state takes the lock first.
- **Three categories cannot use `oneshot`:** a real rmcp HTTP client (listen on
  `127.0.0.1:0`, `tokio::spawn(axum::serve)`, abort at the end — `tests/app.rs`), a WebSocket
  (the `terminal_ws` rig with fake shells), and the http/rest adapter's peer (an echo gateway
  started in-process — `tests/http_adapter.rs`).
- **Helpers before new helpers.** `tests/adminapi.rs`: `sandbox()`, `Harness`, `Fixture` (an
  echo tool, one resource, ping, rename). `tests/plugin_host.rs`: `full_app`,
  `full_app_file_backed`, `await_run`, cross-platform `echo_input`/`legacy_echo`.
  `tests/terminal_ws.rs`: `rig`, `recv` (5 s timeout), `wait_detached`. Crate level behind
  `#[cfg(any(test, feature = "test-utils"))]`: `swiss_core::paths::test_home()`,
  `swiss_mcp::calls::test_log()`, the jobs fake `Clock`, the tunnels `SshLike`/`FakeConn`
  (counting and fault injection, no sshd). Product visibility is never widened for a test —
  use the `test-utils` feature.
- **Shape.** Write the failing test first for a behaviour change. Pin shapes, not just statuses:
  absent-not-null, the JSON-RPC error for a client versus `{error}` for the panel. Test names
  are English sentences (`boot_disabled_plugins_guard_every_route_they_own`).
- **Fixtures are neutral** (§release.hygiene). `tests/fixtures/` holds envelopes sealed by the
  Node build under a documented fixed test key; `tests/envelope_compat.rs` opens them, which
  catches an HKDF argument-order or base64 mistake the moment it is made.

### §testing.it — The integration harness (gate 2)

`crates/swiss-it` proves the database adapters, the browsers and the proc path against real
engines (ADR-028). It is a dev-only leaf: every dependency sits behind the `it` feature, so
without it the crate compiles to empty targets and the shipping graph never changes.

| Where | What |
|---|---|
| `src/engine.rs` | One engine per kind per test process; resolution; readiness; the failure report |
| `src/seed.rs`, `seed/` | The committed seeds and `Fresh` (a database per test) |
| `src/exit.rs`, `src/docker_raw.rs`, `src/bin/it-reaper.rs` | Container reaping |
| `src/bin/it-mcp-server.rs` | The in-repo stdio MCP server for L3 |
| `tests/it/main.rs` | One test binary; suites as modules: `smoke`, `seed`, `mysql`, `pg`, `redis`, `gateway`, `proc`, `reaper` |

**Engines.** `mysql:8.4`, `postgres:17`, `redis:7`. Resolution order, printed on failure:
(1) `SWISS_IT_MYSQL_URL` / `SWISS_IT_POSTGRES_URL` / `SWISS_IT_REDIS_URL` — an existing server
used as is; a set but dead override is a hard failure, never a silent fallback; (2)
testcontainers wherever `DOCKER_HOST` points; (3) neither — the test **fails, never skips**,
with a three-part message (what was tried, why it failed, how to fix it). Ports are random;
nothing hard-codes 3306/5432/6379. Containers carry the label `org.swiss-it.owned=1` (the prune
scope; a pid label is informational only). bollard runs on one dedicated worker thread.

**Reaping, three layers.** (1) The `it-reaper` watchdog child reads the docker endpoint and
then one container id per line on stdin; when the test process dies its stdin hits EOF and it
force-deletes every id. It exists because libtest's failure path is `exit(101)` — on Windows
`ExitProcess`, which skips the atexit table exactly on the red runs that get re-run most. On
Windows it spawns with `CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB` (falling back to
a new group alone). (2) An atexit hook deletes the same ids on the green path; the reaper's
duplicate DELETE reads 404 as done. Both share the raw DELETE in `docker_raw.rs` — HTTP/1.1
over tcp/unix/npipe, 5 s timeouts, no `/v1.xx` prefix. (3) At start, owned containers older
than one hour are pruned — age, not identity, so a live engine of a parallel run is never
mistaken for debris. `tests/it/reaper.rs` drives a real second test process for both paths
(`SWISS_IT_FORCE_RED=1` is that variable's only use).

**Isolation — `Fresh`.** Every test gets its own database, named after it:

- **mysql** — `CREATE DATABASE it_<tag>_<hex8>` (utf8mb4, `utf8mb4_0900_ai_ci`), seeded by root,
  then granted to the non-superuser `it`.
- **postgres** — `CREATE DATABASE it_<tag>_<hex8> TEMPLATE it_seed OWNER it`; the template is
  seeded as `it` (a clone copies ownership) and carries `it_meta.seed_meta.version`
  (`SEED_VERSION`); a changed seed rebuilds it, even on a reused override server.
- **redis** — one of 16 db indexes leased from a semaphore, `FLUSHDB`, seeded in pipelined
  batches of 500; returned flushed, so a later lease never inherits keys.
  `fresh_redis_with_neighbor` leases a second index for keyspace tests.
- Defs use the non-superuser `it` account, so L1/L2 hit the privilege surface a real deploy
  has; the superuser is only for the harness (creating databases, counting connections).
- Drop is best-effort (`DROP DATABASE`, pg `WITH (FORCE)`); a leak dies with the container.

**The seed** (`seed/{mysql,postgres}/{schema,data}.sql`, `seed/redis/keys.txt`) is shaped by
bugs already met, and uses **neutral names only**: `users` (a type matrix — BIGINT past 2^53
and u64::MAX, DECIMAL(20,4) edges, JSON, an enum, binary with PNG magic, µs timestamps, UTF-8
including emoji and RTL), `orders` (composite primary key + FK), `events` (no primary key, two
byte-identical rows), `wide` (60 columns), `big_rows` (1,000 rows), the view `active_users`, and
on pg the schemas `app` (uuid key, `text[]`, a cross-schema FK) and `audit` (a partial index).
Redis: every type, a TTL pair (`SETEX` and a `PERSIST`ed twin), a three-level `tree:` namespace,
3,000 `bulk:` keys for `SCAN` paging — 3,016 keys in all, pinned by a guard test — plus streams
generated by `seed.rs` (a 10,000-entry stream with a consumer group, an empty one, a
one-entry one, a ragged one). `tests/it/seed.rs` verifies the seed and the isolation itself;
whoever changes a seed changes it first.

**The layers.**

- **L1 — browsers on real engines** (`tests/it/{mysql,pg,redis}.rs`): each builds the browser
  by the adapter's own recipe from a def and calls the `DbBrowser`/`RedisBrowser` trait
  directly — list, read (every filter operator, paging, sort), describe, export (csv, json, the
  streamed SQL dump), import, `apply_edits` (primary key, composite key, keyless refusal on
  ambiguity), the console (a SELECT without LIMIT capped and reported), DDL, activity and kill,
  completion, the database list; for Redis the full-keyspace `SCAN` with no duplicate and no
  miss, type-aware reads, TTLs, the command guard, the atomic pipeline. The route layer is not
  involved; `dbbrowser_api.rs` guards that with stubs.
- **L2 — adapters through `/mcp/<name>`** (`tests/it/gateway.rs`): a real gateway on
  `127.0.0.1:0` (with the health probe and the 1 s idle-reap sweeper a daemon runs) and a real
  rmcp client. `list_tools` is exactly the adapter's set; each tool hits the seed; resources
  read. **Credentials are references** — a `${secret://…}` password registered through
  `/api/secrets` before the def, echoed as the reference, never the value. **Stopping an MCP
  releases its connections** — a baseline above zero, then `POST /api/mcps/<name>/stop`, then
  the engine's own connection count for that database must reach 0 within 2 s; red here is a
  product bug, never a test to weaken. One test pins that the gateway's `/api/mcps` rows match
  the root `tests/adminapi.rs` contract.
- **L3 — proc with the in-repo server** (`tests/it/proc.rs`): `it-mcp-server` writes a line to
  stderr at start (noise must never reach the protocol stream) and serves `echo`,
  `blob` (exact bytes, compared byte for byte — the forwarding path is `&RawValue`), `sleep`,
  `fail` (an MCP error, the process survives) and `env` (the child's variable names — the
  launch-environment scrub holds for proc children). Lazy start, idle reap within the window,
  wake on the next call, and stop kills the tree (job object on Windows, process group on
  unix). The group runs serially: it counts the whole process table by exe name, and an
  unreadable table counts as "many", never as "none".

**Mutation check.** A suite that only adds tests proves itself by biting: each L1/L2/L3 change
records one deliberate breakage (a `<` turned `<=` in paging, a skipped `SCAN` batch, an idle
timer never reset), the red test's name, and the green after reverting. A group that no
mutation turns red is not accepted.

**Weight.** testcontainers and bollard are dev-only: `cargo tree -e normal,build -p swiss` does
not change, the duplicate check scopes to `-e normal,build`, and `THIRD_PARTY_NOTICES.md`
excludes dev-only crates.

**Where it runs.** CI's ubuntu `integration` job (§release.ci). Locally on Windows, the tests
stay native and only the containers live in WSL: `dockerd` in a WSL distribution listens on
its unix socket and on `tcp://127.0.0.1:2375` (loopback only — the Docker socket is root), and
the user sets `DOCKER_HOST=tcp://127.0.0.1:2375`; WSL's localhost forwarding makes the published
ports reachable. No Docker Desktop. If the distribution is not running, neither is `dockerd`;
start it first. Running cargo inside WSL on `/mnt/c` is an order of magnitude slower and is not
the path.

**Not in the harness:** MariaDB (a fourth engine is one more row in `engine.rs`); a golden
capture of every `/api/*` response; real-engine tests for the panel (its contract is the
`/api/db` shape, pinned with stubs); Windows and macOS CI integration jobs; embedded database
binaries.

### §testing.memory — The memory guard

`tests/memory.rs` asserts a **delta, not a ceiling**. The working set readable inside
`cargo test` belongs to the test binary — harness, rmcp client, reqwest, every dev-dependency —
so an absolute number there measures the wrong process. Growth under load needs no baseline,
and growth is the regression shape: a leak, an unbounded buffer, a payload-proportional
allocation on a forwarding path. It is its own file (cargo gives it its own process) and one
test (two would measure each other). Its phases — trivial requests, tool calls, and a large
volume through the adapter — pin "no `serde_json::Value` on a forwarding path" (§arch.rules).
The budgets derive from the request counts, so changing a count keeps the assertion honest.

The **shipped** figure is measured from the release binary on real state by the
`swiss-memory-record` skill and recorded in §product.memory — a record, never a gate.

### §testing.live — Live verification

**19999 is production** for the human on the machine. Iteration never stops, restarts,
redeploys or sends traffic to it — the one allowed request is a read-only `GET /health`.
Production changes only through `scripts/deploy.ps1` (§host.ops).

**19998 is the test instance** (`scripts/test-instance.ps1`, wrapped by the
`swiss-live-verify` skill, with a POSIX twin in the skill):

1. Build into its own target tree (`CARGO_TARGET_DIR=target-test`), never `target/`, which the
   next deploy copies to production. After a panel change, `npm run build` first and touch
   `crates/swiss-panel/src/lib.rs` — the embed fingerprint does not include asset content.
2. Pin the token (`SWISS_TOKEN`) before starting, so the session has a known bearer.
3. Start on a snapshot of real state (default) or an empty home (`-Fresh`). A port already
   held means a stale instance serving the wrong build: stop it first. Never pass `--port` to
   `swiss` by hand — `start` and `serve` write the port into the config.
4. Prove the build: `/health`'s `build.hash` must equal the exe just built.
5. Verify, then stop **by the port's owning PID** (`-Stop`) — never by image name, which would
   kill production too.

`scripts/acceptance-16.ps1` drives the boot, health and environment-scrub checks against 19998
and proves isolation by checking that the production config's mtime did not change.
State that must survive a restart is verified by stopping and restarting the instance.

**Panel proof of life** (`.agents/rules/panel-proof-of-life.md` is the checklist). A panel
change is done only when a real browser, freshly loaded, opened the page and every visible
control was clicked with **real pointer events** and answered. vitest and a syntax check are
the entry ticket: they cannot see a named import that fails to link in the browser (the whole
module dies), a sheet rendered into a hidden container, or a click an overlay swallows. Assert
what a user sees — `hidden === false` and the computed `display`, or the bounding rect — never
bare existence. Walk every level of navigation, every seg, every sheet, every primary action
and the empty states; a change to visible copy is walked a second time in Chinese
(§panel.i18n). A flow that cannot be verified is reported as **not verified** with the reason.

**Reporting.** Which exe served (the build hash), what was checked, the exact responses, and
that 19999 was untouched. Carry the evidence forward, not the scrollback.

### §testing.gaps — Known gaps

Open tests, most important first. Each is a contract the panel or a client already depends on.

- **Security and wire contracts (P0).** Host: `a_foreign_peer_address_is_refused_and_a_mapped_one_passes`,
  `a_foreign_host_is_refused_across_the_extra_tree_too`,
  `registration_refuses_duplicate_ids_route_clashes_and_reserved_prefixes`,
  `a_vault_put_without_value_or_rev_is_a_400_and_the_store_stays_sealed`,
  `oversized_admin_body_answers_the_node_413_message`,
  `an_unparseable_body_answers_the_node_400_message`,
  `shutdown_answers_ok_then_signals_the_watch_channel`. MCP:
  `toggling_a_tool_off_hides_it_from_the_clients_broadcast_list`,
  `the_resources_master_switch_empties_the_list_and_persists`,
  `a_lazy_mcp_is_woken_and_served_by_its_own_wakeup_request`,
  `a_failed_start_answers_503_jsonrpc_and_an_admin_error_respectively`,
  `token_routes_survive_an_mcp_plugin_disable`. Data: `api_db_refuses_foreign_hosts_and_origins`,
  `an_oversized_api_db_body_is_refused_413`, `browser_errors_pass_through_verbatim_as_400`,
  `invalid_json_bodies_are_refused_before_the_handler`. Tunnels:
  `connection_credentials_go_out_masked_and_disk_keeps_the_env_reference`,
  `stop_and_delete_guard_answers_409_with_dependents_until_forced`, CRUD round trips over HTTP
  for connections and rules. Jobs and runs:
  `capture_none_records_the_run_without_output_chars_or_preview`,
  `a_busy_label_answers_409_while_the_same_job_runs`,
  `a_full_pool_answers_429_naming_the_capacity_and_a_queued_successor_runs`. Terminal:
  `an_open_body_is_validated_field_by_field`, `opening_an_unknown_target_is_a_409_naming_it`,
  `a_provider_that_fails_to_open_is_a_502`. Process:
  `a_legacy_unset_reference_runs_empty_where_exec_refuses`.
- **Main paths (P1).** The applied-false gap after a failed apply; the unified wrong-method 404;
  the env-lookup precedence; MCP restart, rename (the call log follows, the old path dies),
  the panel Stop persisted per source, a cursor walk to the end, honest connection tests, a
  REST tool round trip; the Data caps (16 filters, 1,000 edits, 10,000 import rows, the Redis
  1,000-element reply cap) and the pure helpers around them; the terminal's second-tab fan-out,
  stall warning, delete-while-attached and config-restart closing sessions; the process
  plugin's truncation flag, API deadline, disable-cancels-runs and tagged stderr; `swiss-remote`
  `sync.rs` coverage.
- **Harness-level.** The golden `/api/*` capture; macOS runs lint but not tests in CI (no
  master-key source yet, and a killed local shell's pty never reaches EOF there).
- **Cosmetic watch.** A stream's meta entry count during Follow; a suspected, never reproduced
  flake in `tests/adminapi.rs`.

A gap closed is deleted from this list in the commit that closes it.

## §release — CI, releases and repository hygiene

swiss ships as one binary per platform from GitHub Releases (ADR-006); there is no installer
and no self-update — `swiss update` only compares the running build with the newest release
and prints how to swap the exe (§host.cli). Nothing goes to crates.io: the workspace sets
`publish = false`, since a stray `cargo publish` would claim ten crate names for one binary.
The workflow is `.github/workflows/build.yml`.

### §release.ci — The workflow

| Event | Jobs |
|---|---|
| Pull request or push to master | `check` on Linux and Windows, plus clippy on the arm Mac; `panel`, `deny`, `integration` |
| `v*` tag or a manual run | Everything above, plus `dist` for all five targets; a tag also runs `tag-version` and `release` |
| A change under `docs/` alone | Nothing |

- **Concurrency.** A newer push to the same ref cancels the run in flight; a tag's run always
  finishes.
- **Least privilege.** The workflow is `contents: read`; only `release` gets `contents: write`.
- **`check`** (30 min ceiling, matrix not fail-fast): `cargo clippy --workspace --all-targets
  --locked -- -D warnings`; the duplicate check — print `cargo tree -d -e normal,build`, then
  fail only when `tokio`, `rustls`, `native-tls`, `openssl`, `openssl-sys`, `hyper` or
  `aws-lc-rs` appears twice at column 0 (plain `cargo tree -d` exits 0 whatever it finds, and
  the RustCrypto spread is expected; dev edges never count, §arch.deps); then
  `cargo test --workspace --locked --no-fail-fast`, skipped on macOS (§testing.gaps). Linux
  installs cmake and clang for aws-lc-rs.
- **`panel`** — `npm ci` + `npm run check` on Node 24, independent of every Rust job: the
  committed emit is what ships, so the Rust jobs need no node (§panel.toolchain).
- **`deny`** — `cargo deny check --locked` against `deny.toml` (licence allowlist, advisories,
  registry sources); no Rust build.
- **`integration`** — gate 2 on ubuntu's own dockerd (§testing.it); on failure it prints the
  container list and the MySQL log tail.
- **`tag-version`** — the tag must equal `v` + `[workspace.package] version`.
- **`dist`** — `cargo build --release --locked` for `x86_64-pc-windows-msvc`,
  `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (arm runner),
  `aarch64-apple-darwin`, and `x86_64-apple-darwin` (cross-built on the arm Mac). Both Linux
  targets build on Ubuntu 22.04 runners: a binary needs at least its build host's glibc, so the
  archives hold the floor at glibc 2.35 (Ubuntu 22.04, Debian 12). Each archive
  is `swiss-<version>-<target>` (`.zip` on Windows, `.tar.gz` elsewhere; a manual run's version
  is `dev-<sha8>`) and holds the binary, `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md` and the
  two skills (`skills/swiss/SKILL.md`, `skills/swiss-remote/SKILL.md`, from `src/skill_assets`).
- **`release`** — needs `tag-version`, `check`, `dist`, `panel`, `deny` and `integration`;
  writes and verifies `SHA256SUMS` over the archives and publishes a GitHub Release with
  generated notes.

TLS: Windows links schannel, unix links rustls, so no unix binary depends on system OpenSSL.
Dependabot (`.github/dependabot.yml`) proposes cargo, npm (the panel) and Actions updates
monthly; a bump is reviewed like any dependency change (§arch.deps), and a manifest moves
together with its lockfile.

### §release.hygiene — What the repository may contain

- **Authorship.** The only author is `young1lin`: `authors` in the workspace manifest, the
  licence headers and `NOTICE` carry the bare name, and commits carry no `Co-Authored-By`
  trailer. **No file carries an email address**; contact goes through GitHub's private
  channels (§security.reporting).
- **Licence.** Apache-2.0 for the whole tree. Every source file starts with the header
  `Copyright 2026 young1lin`; `scripts/add-apache-headers.ps1` adds it idempotently to files
  that lack it (never to `docs/`, `assets/`, vendored or generated trees). Manifest metadata
  (`authors`, `license`, `repository`, `rust-version`) lives in `[workspace.package]` and the
  members inherit it.
- **Third-party attribution is kept** — a licence obligation, not authorship: `NOTICE`,
  `THIRD_PARTY_NOTICES.md` (the hand-written entries plus a crate → licence table generated from
  the `cargo tree -e normal,build` set and `cargo metadata`, excluding dev-only crates;
  regenerated when `Cargo.lock` changes), the upstream licence text next to each vendored
  panel library (`admin_assets/js/vendor/*/LICENSE`, embedded with them), and the Z.AI notice on
  the prompt texts the zai-vision adapter reproduces (`zai_prompts.rs`).
- **Standard files.** `SECURITY.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md`
  (Keep a Changelog, SemVer), the issue and PR templates, `dependabot.yml`, `deny.toml`, and a
  `rust-version` that is the toolchain the suite is green on.
- **No real environment data.** No real hostnames, users, database names, keys, measurements
  or screenshots of a real instance: fixtures, examples and seeds use neutral names
  (`acme_app_*`, `shop_*`, `jdoe`, a target called `dev`), and screenshots taken against a real
  instance are git-ignored, never committed. State files, logs and recordings are ignored
  (`gateway.config.json`, `managed.json`, `master.key`, `.env*`, `*.log`, `*.cast`).
- **No absolute personal paths.** The repository is `<repo>`, a home directory is `~`, a remote
  example path is `/home/dev/app`; a script finds its files relative to itself.
- **The brand.** The mark is a red (`#DA291C`) rounded square with three tools fanned open —
  no shield, no cross — in `assets/logo.svg`, `assets/logo-wordmark.svg` and
  `crates/swiss-panel/src/admin_assets/logo.svg`, which stay identical. The tagline is "A
  developer's pocket multitool"; the name `swiss` is unchanged.
- **History.** A rewrite of published history is the owner's decision alone. When one happens,
  the short hashes the tree cites are remapped in a single follow-up commit, and the rules above
  are what the rewrite enforces — the replacement tables themselves are never committed.
- **Before going public**, a pass re-checks this list against the whole history, not just the
  working tree.

## §decisions — Decision log

Each entry is an architecture decision that is hard to reverse, surprising without its
context, and a real trade-off. Entries keep their `ADR-NNN` numbers forever (code cites them by
number); a new decision takes the next number. An entry states the decision in force, what it
cost, and what was rejected. A superseded entry keeps one paragraph saying what replaced it.

### ADR-001 — No third-party adapter module door

**Accepted.** The Node build let a config entry name a module to load at runtime
(`"adapter": "./my-adapter.mjs"`). Rust links statically; the options were a Node sidecar (re-imports
the memory the port removed), WASM plugins (a `wasmtime` store costs more than the whole budget),
or native `dylib` plugins (no stable ABI, in a process holding database credentials). The door is
dropped: `make_adapter` (`crates/swiss-mcp/src/adapters/mod.rs`) matches the built-in type names and
nothing else, and an unknown `type` fails at build time listing them. An external adapter becomes a
`proc` or `http` MCP. What is lost: a third-party adapter cannot appear in the Data view.

### ADR-002 — One crate, not a workspace

**Superseded by ADR-010.** What forced the revisit was not compile time but that "MCP is a plugin,
not the trunk" was a claim nothing checked.

### ADR-003 — `current_thread` runtime, `Arc`/`RwLock` state

**Accepted.** A local gateway serving one machine has no use for work stealing, and each worker
thread costs a stack plus allocator caches. The trap: `current_thread` does not permit `Rc` —
`axum::serve` spawns connections through `tokio::spawn`, which requires `Send` futures whatever the
flavour. State is therefore `Arc<RwLock<…>>`, uncontended on one thread. Do not attempt a `LocalSet`
plus a hand-rolled accept loop to recover `Rc`.

### ADR-004 — `mongo` as an opt-in compile feature

**Superseded by ADR-012** (the feature and the adapter were deleted).

### ADR-005 — The traffic ring is disk-backed

**Accepted in part.** `traffic.rs` writes a JSONL tail beside the call log and restores from it at
boot, so a restart no longer blanks the view. Reads are still served from the in-memory ring of
`KEEP = 500` entries (~1 MB resident) rather than paged off the file; the file format already
permits finishing it, which stays a deferred optimisation.

### ADR-006 — How it ships

**Accepted — GitHub Releases.** CI builds five targets and attaches one archive per target (the
binary, the licence files and the skills) plus `SHA256SUMS` to a GitHub Release on a `v*` tag
(§release.ci). There is no npm package and no npm publish step; the per-platform
`optionalDependencies` package remains possible release plumbing on top of the same artefacts.
Open: `-C target-feature=+crt-static` is not applied — the Windows binary links the CRT
dynamically. Turn it on and verify with `dumpbin /dependents` before a release goes to a machine
this repository never built on.

### ADR-007 — No `tracing`, no `regex`

**Accepted.** The logger is a hand-rolled JSON-line writer of about thirty lines. `regex` would be
imported for three patterns — `${ENV_VAR}` expansion, the secret-name wordlist, the loopback host
check — each a short hand-written scanner, and the regex engine's code segment is resident memory in
a process budgeted in single-digit megabytes. Recorded because both crates are reflexive to reach
for. (This is not a licence to hand-roll a real grammar: shell quoting, JSON, YAML and the like use
mature crates — §arch.deps.)

### ADR-008 — Job Objects replace tree-kill

**Accepted.** On Windows each `proc` child is assigned to a Job Object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the OS reaps the whole subtree when the gateway's handle
closes — crash, End Task and debugger detach included. The PID ledger stays: the job object covers
*this* gateway dying; the ledger covers a child that outlived a previous generation. The Node
build's command-line sweep for known MCP packages is gone.

### ADR-009 — The panel is copied, never forked

**Superseded by ADR-016.** While the Node build was the reference, `admin_assets/` was a
byte-for-byte copy guarded by a tree-equality test.

### ADR-010 — Workspace crates, still one binary

**Accepted. Supersedes ADR-002.** The source is a cargo workspace (§arch.crates); the product is one
static `swiss` binary. The split buys nothing at runtime, and saying otherwise is forbidden. What it
buys is that the architecture stops being a claim: `swiss-data` has no `swiss-mcp` in its manifest,
so a change that reintroduces that edge does not compile. When a change appears to need such an
edge, the host contract is missing something (the connection catalog exists for exactly that). The
cost: manifests to keep in step, and gate commands that need `--workspace` or they silently check
the root package alone. Measured: +1.1% exe size for the crate-boundary codegen, idle footprint
flat.

### ADR-011 — The terminal: WebSocket, hand-written ConPTY, no tunnels edge

**Accepted.**

- **WebSocket, not SSE.** A terminal is a bidirectional byte stream; over SSE every PTY byte would
  need base64 or JSON wrapping on the hot path, and keystrokes would need a second channel. axum's
  `ws` feature adds `tokio-tungstenite` + `tungstenite` and nothing else. Binary frames carry raw
  PTY bytes both ways; the only JSON is the resize message and the server's exit/error/stalled
  notices. Measured +662 KB exe against a +800 KB budget (~440 KB of it the vendored xterm tree,
  read-only and never paged in).
- **Hand-written ConPTY FFI, not `portable-pty`.** ~300 lines against the `windows` crate already
  linked. `open_pty` returns a cheap cloneable `PtyHandle` plus exactly one `PtyPump` on a
  `spawn_blocking` thread — that thread is why local sessions have a lower cap than remote ones.
  Measured +233 KB working set per attached idle local session (budget 1.0 MB).
- **`swiss-terminal` links no SSH and has no edge to `swiss-tunnels`.** Remote shells arrive through
  the `ssh-shell` capability seat in `swiss-host`: tunnels registers a provider on start and
  withdraws it on stop, and the terminal dials through the tunnel manager's own refcount. The
  plugin's `requires` stays empty so local shells work on a machine with no tunnels; when tunnels
  is absent, `/api/terminal/targets` says so by name. Marginal remote session cost measured at
  ~251 KB (a record, not a gate).

### ADR-012 — MongoDB support is deleted, not feature-gated

**Accepted. Supersedes ADR-004.** No user, and the heaviest driver in the set (an estimated 3–5 MB).
Deleted whole: the cargo feature, the `mongodb` dependency, the adapter and its resources, the
`MongoBrowser` trait and flavour, its `/api/db` routes and its admin form column. A `mongo`-typed
MCP fails like any other unknown type. The build is plain `cargo build --release`, and the gates run
one feature combination.

### ADR-013 — The RustCrypto duplicate stack

**Accepted.** Every direct crypto dependency of ours sits on the newest generation (aes-gcm 0.11,
sha2 0.11, hkdf 0.13; rand 0.10, whose OS RNG is getrandom's `SysRng` — a CSPRNG error is a broken
machine and is expected away). The sealed envelope is a byte-level contract (§formats.sealed) and
the committed fixture proves the primitives did not move. The duplicates that remain are pinned
by others and named rather than fought: the older RustCrypto/rand generation by sqlx, base64 by
axum, older `windows` by russh/rmcp, and proc-macro-only pairs that cost nothing in the binary.
Verify any change with `cargo tree -d -e normal,build` and `cargo tree -i`.

### ADR-014 — The secret vault is a sealed file of its own

**Accepted; its reference-syntax clause is superseded by ADR-019.** Spec: §host.vault.
The env store doubles as every child's environment, so a key stored there is readable by every
proc MCP, job and shell, and a missing `${ENV}` resolves to empty — the wrong failure for a
credential. The vault is `secrets.json`, sealed like every state file:

- names are lowercase kebab `[a-z][a-z0-9-]{0,63}` — a different character class from
  `${UPPER_SNAKE}`;
- references resolve at every use point through one resolver shared with env refs; a missing
  name is a hard refusal naming the surface and the reference (never a value);
- write-only: no API reads a value back;
- nothing merges the vault into a child environment — the only exit is substitution at a use point;
- mutations are rev-checked (stale rev → 409 with both numbers, missing rev → 400);
- `export` carries the vault and `import` merges it;
- host-owned, not a plugin: every plugin may depend on it.

### ADR-015 — Groups are host mechanism

**Accepted.** Spec: §host.groups. `swiss-host` owns one `Groups` type with the rules, a
`GroupScope` registry each scope registers into (a six-method trait), and one route family,
`/api/groups/{scope}`. The panel renders every grouped list through one component. Rejected:
per-plugin grouping (six diverging copies), and unifying only the panel (the server sides already
differed). The per-scope group routes that predate the family are retired. Secrets and tokens refuse
the family's `order` verb (they list by name and creation order).

### ADR-016 — The Node reference build is retired; the panel is edited here

**Accepted. Supersedes ADR-009.** `crates/swiss-panel/` is the panel's source of truth. What
survives: the panel is served straight from embedded assets with no bundler; the panel's code is
the spec for the admin API (every `/api/*` shape change ships on both sides in one commit); the
panel's test suite lives in this repository; the sealed-envelope format stays frozen with its
committed fixture.

### ADR-017 — SQL completion is computed server-side, on the leased connection

**Accepted.** Spec: §data.completion. Completion is a per-keystroke service, not a dataset: the
rejected alternative walks the whole catalog into the browser before the first keystroke. One
route, one lease: `POST /api/db/{name}/completion {sql, caret}`. Candidates fold most-specific
first; the cache lives with the connection, loads lazily, serves for ten minutes, drops on DDL, and
is capped (a wide schema degrades to keywords + tables instead of growing without bound). Redis is
excluded.

### ADR-018 — Path-space partition: `/mcp/*` is the MCP plugin's domain

**Accepted.** Spec: §mcp.endpoint. `/mcp/<name>` is the only MCP endpoint shape. Hard cutover, no
alias: a root single-segment POST answers 404 with a "moved to /mcp/<name>" hint when the name is a
registered MCP. There are no host-reserved words inside `/mcp/`; the root belongs to host chrome
(`/`, `/admin`, `/health`, `/api/*`) and to whatever a future plugin claims. Rejected: a permanent
or deprecation-window root alias, and a growing reserved-name list. The partition deletes a rule
instead of growing one.

### ADR-019 — Vault references wear the `${...}` envelope

**Accepted. Supersedes ADR-014's reference-syntax clause.** Spec: §host.refs. A bare scheme
(`secret://name`) has no token boundary inside URLs, headers and command lines — the scanner
claimed `secret://aaa` inside `https://test.com/secret://aaa/test`. One envelope, two families:
`${UPPER_SNAKE}` stays lenient, `${secret://kebab}` hard-fails; outside `${...}` there are no
references. A malformed name inside the envelope refuses rather than guessing. Legacy whole-value
bare refs are rewritten in memory at each loader with one boot note per file; mixed strings are not
auto-migrated.

### ADR-020 — HTTP MCP OAuth: impersonate the allowlisted client, own the token

**Accepted.** Spec: §mcp.oauth.

- Providers whose dynamic client registration admits only allowlisted `client_name`s are answered
  with a default client name per provider; `oauthClientName` overrides it. Nothing else lies.
- Endpoints are discovered, never hard-coded (RFC 9728 → RFC 8414).
- Grants live in their own sealed file, `mcp-oauth.json`, keyed by MCP name — not the vault;
  export/import carries them.
- The def surface is `auth: "oauth"` plus the optional `oauthClientName`; a hand-written
  Authorization header alongside is refused.
- `POST /api/mcps/{name}/authorize` single-flights a flow per name. The 401 path refreshes once,
  retries once, then clears the grant and shows "needs authorization" instead of looping.
- The single-flight dedups on the refused token, not on freshness.
- No new dependency; the loopback callback is one ephemeral listener per flow.

Rejected: a general provider UI, the device flow, token reveal, auto-reauthorize.

### ADR-021 — The figma adapter type

**Accepted.** `type: "figma"` is sugar over http + OAuth: its def is a name and an optional
description, and `make_adapter` expands it to the full http def. The stored def refuses `url`,
`auth`, `oauthClientName`, `headers` and `proxy`. One predicate, `is_oauth(def)`, answers "is this
an OAuth MCP". The row reports the def type, not the adapter kind. The form has no Test button.
Rejected: a panel preset (a saved URL the operator never chose) and a generic provider registry
(one provider does not justify a table — the second one reopens this ADR).

### ADR-022 — The zai-vision type: ported native

**Accepted.** The upstream vision MCP is eight tools, each a fixed system prompt plus one
multimodal chat-completions call, so it is an `Engine` compiled into the binary
(`adapters/zai.rs`) instead of a Node child. Parity is the contract: tool names and prompts are
byte-identical, the prompts extracted verbatim by `scripts/extract-zai-prompts.js`; descriptions
and schemas match upstream except where the gateway is not the client's child (§mcp.zai:
absolute local paths, video formats, strict schemas), and retries skip non-429 4xx. The key is
always a `${...}` reference (a literal is refused). No ping and no Test button: a metered endpoint
is never probed. The def names only `mode` or `baseUrl`, and optional `model`, `proxy`,
`timeoutMs`.

### ADR-023 — The name is the service, the def is a revision

**Accepted.** Spec: §mcp.revisions. Two live same-named defs would contend for name-keyed state
(OAuth grants, call log, groups, tunnel links), so a replaced def is parked as an inert revision
(managed.json `revisions`, at most five per name). A revision is never registered, started or
resolved. Replace builds first, so a bad def changes nothing; a def that builds but will not start
is a 200 with `restartError`. Restore takes before it parks. A def swap is not a start. Rename
carries revisions; delete clears them. Rejected: a disabled-shadow registry.

### ADR-024 — The panel is authored in TypeScript, erased to the same JS

**Accepted.** Spec: §panel.toolchain. Source is `crates/swiss-panel/panel/src/*.ts`; ts-blank-space
blanks type-only syntax line by line, so each emitted `.js` line keeps its `.ts` line number. The
emit is committed under `admin_assets/js`: the repo builds with cargo alone, node is a dev-only
dependency. Rejected: a bundler (a build step and a second artefact), in-browser erasure, staying on
hand-written JS.

### ADR-025 — The panel's gate is the check suite, not byte-equality

**Accepted.** The early rule that emitted JS stay byte-identical to the pre-port JS forced bad
TypeScript and is revoked. The gate is `npm run check` (typecheck ×2 + lint + emit freshness +
vitest), per-file test coverage for rewritten modules, and the live walk on 19998. The machine gate
is the eslint ratchet (§panel.lint): no-var, prefer-const, no-non-null-assertion (shrinking
whitelist), consistent-type-imports, and a zero-allowlist ban on any `.innerHTML =` write.

### ADR-026 — Data's resource navigation is object tabs

**Accepted, amended.** Spec: §data.tabs. The Data page holds several objects as a tab strip — L3
resource navigation made visible, not a new layer — with a third tab shape (card + glyph + ×).
Costs: a tab cap (eight when accepted, twelve today — §data.tabs), a background tab's rows are
dropped and re-fetched on activation, a dirty tab is never evicted, and the navigation guard asks
about every tab. Opening an object when every tab is dirty is refused. Amendment: the strip's `+` force-opens a new SQL tab even then (going
over the cap beats refusing an explicit open). Rejected: one object with a split toolbar, a bottom
dock, a command palette.

### ADR-027 — Data's many databases: primary writable, the rest read-only, no per-database pools

**Accepted.** Spec: §data.databases. MySQL browses same-instance databases by qualifying names
(free); Postgres lists other databases but disables them (a connection is bound to one database);
Redis reads its keyspace from INFO. Secondary databases are read-only, period: the configured
database is the connection's write boundary. Rejected: one database per connection, and a pool per
database. A proxy that refuses `CLIENT INFO` falls back to db0 rather than failing the catalog.

### ADR-028 — The integration harness: real engines in a dev-only crate

**Accepted.** Spec: §testing.it. `crates/swiss-it` (feature `it`, default off) runs real MySQL,
Postgres and Redis through testcontainers (or `SWISS_IT_*_URL`), restores per-test databases from
the repo's seeds, and drives L1 browser tests, L2 adapter tests through a real rmcp client on a
real listener, and an L3 proc group over the repo's own stdio server. Rejected: sqlx-layer mocks
(the exact-string BIGINT, completion casing and udt_name gaps were invisible to them) and a
developer-installed local engine (a missing engine would have to skip — a silent green). Costs: the
dev graph grows testcontainers/bollard (never shipped; the CI duplicate check gates
`-e normal,build`), gate 2 is mandatory on DB-path diffs, and the seeds are a maintained surface.
A machine without Docker fails gate 2 with setup copy, never skips.

### ADR-029 — The panel's UI library lives in-tree

**Accepted.** Spec: §panel.ui. `panel/src/ui/*.ts` components plus one `ui.css` that owns their
classes, with ratchet gates in `npm run check`. `ui/` may import only `h.ts`, `i18n.ts` and itself;
`views.css` may not style a class `ui.css` owns; font weights are four tokens; the hidden gallery
page `/admin/ui.html` renders every component, and design mockups are gallery scenes built from
the real components. Rejected: rules-only (the drift it produced was measured), a separate package
(needs a bundler and breaks "cargo builds without node"), a second embed crate, a third-party
component library.

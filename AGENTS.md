# AGENTS.md

Guidance for AI coding agents working in this repo. `CLAUDE.md` points here.

## What this is

**swiss** is the developer's pocket multitool: one local process that holds the small tools a
developer reaches for while writing code — MCP servers, database browsing, SSH tunnels,
scheduled jobs, terminals, remote execution — behind one loopback port (`127.0.0.1:19999`) and
one panel, shipped as a single static `.exe`. MCP servers live on HTTP paths under `/mcp/`
(SPEC §mcp.endpoint, ADR-018); the root belongs to the host.

What the system does, and why, lives in one document, `docs/SPEC.md` (cited as
`SPEC §area.sub`; the decision log is its §decisions), and in the code comments — when a piece
of behavior looks odd, the module's comment usually records the bug that was paid for once
already.

The four properties in `SPEC §product.properties` — ruthlessly small, plugin-shaped,
hot-pluggable, three ways to bring a tool in — decide every trade-off. A change that trades one
away for convenience is the wrong change, however much shorter it makes the diff. Idle memory is
the number that matters (`SPEC §product.memory`).

MCP is the most important plugin, not the trunk: Data, Tunnels, Jobs, Terminal, Remote and
Process are its peers, and a new tool reaches the panel by contributing a descriptor, an action
and a page — never by editing a match arm in the host (`SPEC §host.plugins`).

## Where the code lives

A cargo workspace that still ships one static `swiss.exe`. The edges are the architecture:

```
swiss-core  ←  swiss-host  ←  { swiss-mcp, swiss-data, swiss-tunnels, swiss-jobs, swiss-terminal, swiss-remote, swiss-panel }  ←  swiss
```

`swiss-core` knows nothing about gateways (paths, logging, sealed files, platform calls).
`swiss-host` is the mechanism every subsystem shares — the plugin host, actions, runs, the process
supervisor, config, the security boundary. The seven subsystem crates are peers that never depend
on each other; `swiss` (the root `src/`) is composition and nothing else. If a change seems to need
an edge between two subsystem crates, the host contract is missing something — add it there
instead. `swiss-it` is the dev-only integration harness. Full map in `SPEC §arch`.

## Load-bearing rules — do not break these

Each one is a security or correctness boundary, not style.

- **Loopback-only is security, not a default.** Bind `127.0.0.1` and refuse any request whose
  `Host`/origin is not a loopback address. Never weaken it to "reach it remotely" — forward the
  port over SSH instead. A non-loopback `host` in config is refused at load.
- **`/api/*` and the panel shell need the admin session (SPEC §host.session).** A socket request
  needs the CLI key (`X-Swiss-Key`, rotated each start, sealed in `session.json`) or the session
  cookie a one-time `/?token=` link set. The gate fails closed. Never add an exemption for an
  `/api` path, never accept the MCP bearer there, and never print the CLI key — scripts use
  `swiss api`.
- **Credentials are `${ENV_VAR}` or `${secret://name}` references, never literals** (SPEC
  §host.refs). They expand only at use time, so `gateway.config.json`, `managed.json`,
  `tunnels.json` and the jobs config hold the reference, and the panel masks it back out.
- **`http` / `rest` adapters have no health `ping` on purpose.** They are metered third-party
  endpoints; the registry reports them "unknown" rather than spend real requests every 15 s.
- **A `proc` MCP is lazy by default.** Idle at boot, woken by its first request, reaped after
  idling. Its child is the only thing here that costs real memory — do not make anything start
  eagerly "for simplicity".
- **The sealed-envelope format is frozen.** A build that cannot open an existing state file has
  lost the user's configuration. See `SPEC §formats` before touching `secure/`; the fixture is
  committed under `tests/` (`scripts/seal-fixture.mts` is kept as the record of how it was made
  and never needs to run).

## Rust rules

The full list is `SPEC §arch.rules` and `SPEC §arch.deps`; the ones that bite most often:

- **No `serde_json::Value` on a forwarding path.** `proc`, `http` and `rest` pass payloads through
  as `&RawValue`, parsing only the envelope fields they route on.
- **The runtime is `current_thread`.** No `multi_thread` without a measured reason in the commit
  message.
- **No subprocess where a syscall exists.** A new `Command::new("powershell")` needs a very good
  excuse.
- **Every new dependency justifies its weight.** `default-features = false` first. A second TLS
  stack, async runtime or thread pool is a bug, not a dependency.
- **No `unsafe`** outside the platform FFI, and **no `.unwrap()`** on config, network, database
  or filesystem results — one failing MCP must never take down the others.
- **The tree is hand-formatted.** Never run `cargo fmt` or `rustfmt`.

## The panel

The source of truth is `crates/swiss-panel/panel/src/*.ts` (SPEC §panel.toolchain, ADR-024):
TypeScript erased to JS line-for-line, with the emit COMMITTED under
`crates/swiss-panel/src/admin_assets/js` — no bundler, no minify, and `cargo build` needs no
node. Edit the `.ts`, run `npm run build`, never hand-edit the emit. The gate is `npm run check`
in `crates/swiss-panel/panel/` (typecheck + eslint + emit freshness + the vitest suite). The
panel is the spec for the admin API: a `/api/*` shape change ships on both sides in one commit.

## Commands

```bash
cargo build --release             # the shipping exe (target/release/swiss.exe) - ADR-012
cargo test --workspace            # gate 1: runs on any machine
cargo test -p swiss-it --features it  # gate 2: real MySQL/PG/Redis (SPEC §testing.it) - needs Docker
                                  # (DOCKER_HOST) or SWISS_IT_*_URL; mandatory when the diff
                                  # touches the DB adapters/browsers or swiss-it itself
cargo clippy --workspace --all-targets -- -D warnings                  # must be clean
cargo tree -d -e normal,build     # a duplicated TLS stack or runtime must fail review
cargo run -- start --no-open      # the gateway itself, on 127.0.0.1:19999
```

**`--workspace` is not optional.** Without it cargo tests the root package alone and the member
crates, where most of the tests live, are never built — yet the run still reports ok. The same
applies to clippy.

A commit that edits any `Cargo.toml` carries the regenerated `Cargo.lock`: glance at
`git status --short` before committing and self-check with
`cargo tree --locked --offline --workspace --depth 0` — every CI command is `--locked`.

The release profile (opt-level z, fat LTO, one codegen unit) is deliberately slow and faster
variants were measured and rejected (SPEC §arch.deps). Keep `target` warm; a 4-minute full build
means fingerprints were invalidated, not that everyday work costs 4 minutes.

## Live testing ports — 19998 tests, 19999 is the user's

Port **19999 is production for the human on this machine**: their panel is open against it.
**Never stop, restart, or redeploy 19999 as a step of iterating on a change.** All live
browser/API verification runs on a second instance bound to **19998** (SPEC §testing.live):

```powershell
scripts/test-instance.ps1            # -Start (default): snapshot state into the test home, serve on 19998
scripts/test-instance.ps1 -Fresh     # wipe the test home first — a clean instance
scripts/test-instance.ps1 -Stop      # kill by the port's owning PID, never by process name
```

- The script copies the sealed state files into `%LOCALAPPDATA%\swiss-test-home` and points
  `SWISS_HOME` there, so a save on 19998 writes the test home, never the user's config.
- It serves `target-test\release\swiss.exe`; build it with
  `$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`, never into `target\`, which the
  next deploy copies to production.
- Never pass `--port`/`-p` to either instance: `start` and `serve` write the port into the config.
- Kill 19998 only **by the port's owning PID** (`Get-NetTCPConnection -LocalPort 19998`);
  `Get-Process swiss` would kill the user's instance too.
- **Deploying to 19999 is the last step, done once**: after every gate passes AND live
  verification on 19998 succeeds, run `scripts/deploy.ps1` (SPEC §host.ops). It owns the whole
  order and proves the served `/health` build hash equals the new exe. Never improvise a
  stop/build/start sequence beside it.

Two Windows traps that are not this repo's doing: **Smart App Control**, if enabled, blocks
freshly linked unsigned executables with `os error 4551` — the binary never ran, and re-running
usually gets past it; and full debuginfo exhausts a small paging file (`os error 1455`), which is
why `[profile.dev]` keeps line tables only.

## Making changes

- **A behaviour change ships with a test** that fails before it and passes after. Integration
  tests drive the axum app through `tower::ServiceExt::oneshot` — no real port, no real sleep.
- **A panel change ships with its vitest case** in `crates/swiss-panel/panel/test/`, the panel's
  acceptance spec (SPEC §testing), and with a real-browser walk
  (`.agents/rules/panel-proof-of-life.md`).
- **Every visible panel string goes through `tr()`/`trn()`** (SPEC §panel.i18n): keys are
  `<module>.<semanticId>` (e.g. `terminal.bar.open`), English lives in `panel/src/locales/en.ts`
  and every other locale (currently `zh.ts`) carries the same keys; both entries are part of the
  change, and `npm run check` fails a missing key or a bare literal. A nav label the gateway
  SERVES as text goes through `wireLabel()` (i18n.ts) instead.
- **LF everywhere.** `.gitattributes` enforces `* text=auto eol=lf`; editors on Windows must not
  convert back.
- **Write all code comments in English**, including in docs code samples.
- **Never commit** `gateway.config.json`, `.env`, `managed.json`, `tunnels.json`, `master.key`,
  `session.json`, `*.log` or `~/.swiss/terminal/*.cast` — all carry real secrets locally (a
  terminal recording can hold a password the shell echoed).

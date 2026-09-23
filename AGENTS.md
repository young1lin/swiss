# AGENTS.md

Guidance for AI coding agents working in this repo. Single source of truth for agent context;
`CLAUDE.md` points here.

## What this is

The project is named **swiss** — the developer's pocket multitool. It began as the Rust
port of `local-mcp-gateway` (the Node original — retired as the reference on 2026-09-13, see
docs/07; the sibling checkout is no longer needed or consulted). One local process, every MCP
server on an HTTP path under the `/mcp/` prefix on `127.0.0.1:19999` (docs/24, ADR-018 —
the prefix is the MCP plugin's domain; the root belongs to host chrome and future plugins),
shipped as a single static `.exe`. The port exists for one reason: memory. See
`docs/01-goals-and-memory-budget.md`.

The port is complete and is the product itself: this repository owns every layer, the panel
included. The reasons behind ported shapes live in `docs/` and in the code comments — when a
piece of behavior looks odd, the module's comment usually records the bug that was paid for
once already.

## The product: a developer's pocket multitool

The port is the starting point, not the destination. What this is being built into is simple to
state: **one local process that holds every small tool a developer reaches for while writing
code** — MCP servers, database browsing, SSH tunnels, scheduled jobs, and whatever the next one
turns out to be — behind one loopback port and one panel.

Four properties define it. A change that trades any of them away for convenience is the wrong
change, however much shorter it makes the diff:

- **Ruthlessly small.** Memory is the product, not an optimisation pass at the end. The whole
  point of a toolbox that is always running is that you can forget it is running. Idle cost is
  the number that matters; a tool nobody is using right now should cost close to nothing.
- **Plugin-shaped.** Every capability is a plugin with its own descriptor, routes, pages and
  resources. The host keeps only cross-cutting mechanism — the security boundary, config,
  registration, lifecycle, run accounting. No business logic climbs back up into it.
- **Hot-pluggable.** Enabled, running and visible are three different states, switchable at
  runtime. Disabling a plugin really releases what it held (tasks, connections, children,
  routes); it does not just hide a tab. Nothing else in the process notices.
- **Three ways to bring a tool in.** A stdio child process, an HTTP endpoint, or Rust compiled
  straight into the binary. The first two cost a process or a socket and are how foreign tools
  arrive; the third costs almost nothing and is how the tools worth keeping end up shipping.

MCP is the most important plugin, not the trunk everything else hangs off. Data, Tunnels, Jobs
and Process are peers of it, and a new tool should reach the panel by contributing a descriptor,
an action and a page — never by editing a match arm in the host. See
`docs/09-toolbox-plugin-architecture.md` for the contracts and `docs/10-config-driven-jobs.md`
for how configuration drives them.

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
instead. Full map in `docs/02-architecture.md`.

## Load-bearing rules — do not break these

These carry over from the Node build unchanged. They are not style; each one is a security or
correctness boundary.

- **Loopback-only is security, not a default.** Bind `127.0.0.1` and refuse any request whose
  `Host`/origin is not a loopback address. Never weaken it or change the bind host to "reach it
  remotely" — forward the port over SSH instead. A non-loopback `host` in config is refused at
  load, not warned about.
- **Credentials are `${ENV_VAR}` or `${secret://name}` references, never literals.** They expand
  only at use time — one `${...}` envelope, two families; a bare `secret://` outside the envelope
  is literal text (docs/25) — so `gateway.config.json`, `managed.json`, `tunnels.json` and the
  jobs config hold the reference, not the secret. The panel masks them back out. Keep it that way.
- **`http` / `rest` adapters have no health `ping` on purpose.** They are metered third-party
  endpoints; the registry deliberately reports them "unknown" rather than spending real requests
  every 15 s. Don't add a ping.
- **A `proc` MCP is lazy by default.** Idle at boot, woken by its first request, reaped after
  idling. Its child is the only thing here that costs real memory. This is the single biggest
  memory feature in the product — do not make anything start eagerly "for simplicity".
- **The sealed-envelope format is frozen.** State files are AES-256-GCM under an HKDF-derived
  per-file key. A Rust build that cannot open a file the Node build sealed has lost the user's
  configuration. See `docs/05-wire-compatibility.md` before touching `secure/`.

## Rust-specific rules

- **The panel is edited here, directly.** The panel's source of truth is
  `crates/swiss-panel/panel/src/*.ts` (docs/36, ADR-024): TypeScript, erased to JS by
  ts-blank-space line-for-line, with the emit COMMITTED under `crates/swiss-panel/src/admin_assets/js`
  — still no bundler, no minify, no source map, and `cargo build` needs no node. The gate is
  `npm run check` in `crates/swiss-panel/panel/` (typecheck + eslint + emit-freshness + the
  vitest acceptance suite); node is a dev-only dependency there, never a build step of the
  exe. The panel remains the spec for the admin API: every `/api/*` response must stay
  shape-identical to what the panel reads, and a shape change ships on both sides in one commit.
- **No `serde_json::Value` on a forwarding path.** The proxying adapters (`proc`, `http`, `rest`)
  must pass payloads through as `&RawValue`, parsing only the envelope fields they route on.
  Materialising a 2 MB body into a DOM is the single most expensive thing this process can do.
- **The runtime is `current_thread`.** A local gateway does not need a work-stealing pool, and
  every worker thread costs a stack plus allocator caches. Do not reach for
  `flavor = "multi_thread"` without a measured reason in the commit message.
- **No subprocess where a syscall exists.** The Node build shells out to `powershell.exe` for the
  process-tree walk and for DPAPI; each spawn is ~65 MB of transient working set. In Rust these
  are direct Win32 calls. A new `Command::new("powershell")` needs a very good excuse.
- **Every new dependency justifies its weight.** `default-features = false` first, add back what
  you need. A crate that pulls a second TLS stack, a second async runtime, or its own thread pool
  is a bug, not a dependency.
- **Don't reach for `unsafe`** to make a memory number look better. The FFI at the Windows
  boundary is where it belongs; nowhere else.
- **Don't reach for `.unwrap()`** on anything that touches config, the network, a database or the
  filesystem. One failing MCP must never take down the other seven — that invariant is everywhere
  in the original and it is easy to lose in a port.

## Commands

```bash
cargo build --release             # the shipping exe (target/release/swiss.exe) - ADR-012;
                                  # 19999 runs a COPY of it, bin/swiss.exe (scripts/deploy.ps1)
cargo test --workspace            # gate 1: the one feature combination there is; any machine
cargo test -p swiss-it --features it  # gate 2: real MySQL/PG/Redis (docs/44) - needs Docker
                                  # (DOCKER_HOST) or SWISS_IT_*_URL; mandatory when the diff
                                  # touches the DB adapters/browsers or swiss-it itself
cargo clippy --workspace --all-targets -- -D warnings                  # must be clean
cargo tree -d -e normal,build  # a duplicated TLS stack or runtime must fail review
                                  # (normal+build edges only: the dev graph - testcontainers
                                  # included - never ships and may carry its own copies)
cargo run -- start --no-open   # the gateway itself, on 127.0.0.1:19999
```

A commit that edits any `Cargo.toml` carries the regenerated `Cargo.lock` in the same
commit: glance at `git status --short` before committing (a dirty `Cargo.lock` left behind
is how two commits on this branch shipped unbuildable under `--locked`), and self-check the
result with `cargo tree --locked --offline --workspace --depth 0` — every CI command is
`--locked`, and a stale lock fails the checkout itself, not just the build.

The build links with `rust-lld` (`.cargo/config.toml`) - measured ~28% off a
cold `cargo test --workspace` on this machine; drop it only with new numbers in hand.

The release profile is deliberately the slowest thing here (opt-level z, fat LTO,
codegen-units 1 - the memory budget pays for it). Two speedups were measured and
REJECTED, with numbers, at docs/16 follow-up time: incremental release (incompatible
with fat LTO by construction) and a thin-LTO/CGU-16 fast-lane profile (full build
236 s vs 246 s, touch rebuild 160 s, exe +27% - the floor is dependency codegen at
opt-level z, not linking). Keep `target` warm; a 4-minute full build
means fingerprints were invalidated, not that everyday work costs 4 minutes.

**`--workspace` is not optional.** Without it cargo selects the root package alone — a small
minority of the suite — and the nine member crates, where most of the tests live, are never
even built. The run still reports ok. Exact counts rot; the shape does not: only a green
`--workspace` run means "the suite passed". The same applies to clippy. `swiss start` / `stop` / `status` / `logs` / `token`
are the CLI; `swiss token` manages the bearer token; `SWISS_TOKEN` pins it. A boot rewrites a
legacy `tokenEnv` (`MCP_GATEWAY_TOKEN`) to `SWISS_TOKEN` once, carrying the token across names
in the sealed store (bootstrap.rs `migrate_token_env`); the Node-era name stays honored as the
pair partner, so a pre-rename shell keeps authenticating unchanged.

The sealed-envelope format is frozen (docs/05) and its fixture is committed under `tests/`.
The Node-era regeneration script `scripts/seal-fixture.mts` needs the retired sibling checkout
and is kept only as a record: the format must not change, so it must never need to run.

## Live testing ports — 19998 tests, 19999 is the user's

Port **19999 is production for the human on this machine**: their panel is open against it.
**Never stop, restart, or redeploy 19999 as a step of iterating on a change.** All live
browser/API verification runs on a second instance bound to **19998**:

```powershell
scripts/test-instance.ps1            # -Start (default): snapshot state into the test home, serve on 19998
scripts/test-instance.ps1 -Fresh     # wipe the test home first — a clean instance
scripts/test-instance.ps1 -Stop      # kill by the port's owning PID, never by process name
```

- The script (`scripts/test-instance.ps1`, docs/16 H2) copies the sealed state files into
  `%LOCALAPPDATA%\swiss-test-home` (DPAPI opens a copied `master.key` on the same machine under
  the same user — docs/05) and points `SWISS_HOME` there, so a save on 19998 writes the
  **test home**, never the user's config — the script sets `SWISS_HOME`. No read-only
  discipline needed any more — the snapshot is the isolation.
- It serves from `target-test\release\swiss.exe`: the 19999 daemon holds `target\release\swiss.exe`,
  so iteration builds still go to a separate directory
  (`$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`).
- Still never `--port`/`-p` on either instance: both `start` and `serve` write the port into
  the config. The env vars the script sets are not persisted.
- Kill 19998 **by the port's owning PID** (`Get-NetTCPConnection -LocalPort 19998`), never by
  process name (`Get-Process swiss` kills the user's instance too). `-Stop` does exactly this.
- **Deploying to 19999 is the last step, done once**: only after every gate passes AND live
  verification on 19998 succeeds, run `scripts/deploy.ps1` — it owns the entire order (gates
  while production is still up → stop-before-build → build the main target → start → prove
  the served `/health` build hash equals the freshly built exe). Never improvise a
  stop/build/start sequence beside it. Treat a deploy as a deployment, not a test.

On Windows, note two environment traps that are not this repo's doing. Windows **Smart App
Control**, if enabled, blocks freshly linked unsigned executables — cargo's build scripts and test
binaries — with `os error 4551`; the failure looks like a broken test but the binary never ran, and
re-running usually gets past it. And full debuginfo across eight link targets exhausts the paging
file (`os error 1455`), which is why `[profile.dev]` in the root manifest keeps line tables only.

## Line endings - LF everywhere

`.gitattributes` enforces `* text=auto eol=lf`: every text file is LF in the index AND in the
working tree, on every platform — CRLF never enters a commit.

- New files are written with LF. Editors on Windows must not convert back — the attributes file
  covers a fresh checkout, but a misconfigured editor can still dirty an existing tree.
- Binary types are marked binary in `.gitattributes`; never let git normalize them.
- The panel emit tree `crates/swiss-panel/src/admin_assets` is committed, not hand-edited — edit `panel/src/*.ts` and run `npm run build` there; the repo-wide
  LF policy above is the only one it needs.

## Making changes

- **A behaviour change ships with a test** that fails before it and passes after. Integration
  tests drive the axum app through `tower::ServiceExt::oneshot` — no real port, no real sleep.
- **A panel change ships with its vitest case** in `crates/swiss-panel/panel/test/` — that
  suite is the panel's acceptance spec; see `docs/08-testing.md`.
- **Every visible panel string goes through `tr()`/`trn()`** (docs/38, normalized
  2026-10-30): keys are symbolic `<module>.<semanticId>` (e.g. `terminal.bar.open`),
  English copy lives in `panel/src/locales/en.ts`, and each locale — currently `zh.ts` —
  is its own table over the same keys; both entries are part of the change. The two machine
  gates in `npm run check` (dictionary completeness across every locale + the bare-literal
  zero gate) enforce both. A new literal that skips `tr()` fails the suite, not review. A
  nav label the gateway SERVES as text goes through `wireLabel()` (i18n.ts) instead.
- **Write all code comments in English**, including in docs code samples.
- **Never commit** `gateway.config.json`, `.env`, `managed.json`, `tunnels.json`, `master.key` or
  `*.log` — all gitignored, all carry real secrets locally. The same goes for
  `~/.swiss/terminal/*.cast`: a terminal recording is output-only by design, but shells echo
  what was typed, so a recording can still hold a password that was entered.

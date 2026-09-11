# AGENTS.md

Guidance for AI coding agents working in this repo. Single source of truth for agent context;
`CLAUDE.md` points here.

## What this is

The Rust port of `local-mcp-gateway` (the Node original lives at `../local-mcp-gateway` and is the
**reference implementation** — when this document and that code disagree, that code is right).
One local process, every MCP server on an HTTP path under `127.0.0.1:19999`, shipped as a single
static `.exe`. The port exists for one reason: memory. See `docs/01-goals-and-memory-budget.md`.

While the port is in progress, the Node build is the living spec. Read the original module before
porting it — its comments carry the *reasons*, and most of them record a bug that was paid for
once already. Port the reason, not just the code.

## The product: a developer's Swiss Army knife

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

A cargo workspace that still ships one static `lmg.exe`. The edges are the architecture:

```
lmg-core  ←  lmg-host  ←  { lmg-mcp, lmg-data, lmg-tunnels, lmg-jobs, lmg-terminal, lmg-panel }  ←  lmg
```

`lmg-core` knows nothing about gateways (paths, logging, sealed files, platform calls).
`lmg-host` is the mechanism every subsystem shares — the plugin host, actions, runs, the process
supervisor, config, the security boundary. The six subsystem crates are peers that never depend
on each other; `lmg` (the root `src/`) is composition and nothing else. If a change seems to need
an edge between two subsystem crates, the host contract is missing something — add it there
instead. Full map in `docs/02-architecture.md`.

## Load-bearing rules — do not break these

These carry over from the Node build unchanged. They are not style; each one is a security or
correctness boundary.

- **Loopback-only is security, not a default.** Bind `127.0.0.1` and refuse any request whose
  `Host`/origin is not a loopback address. Never weaken it or change the bind host to "reach it
  remotely" — forward the port over SSH instead. A non-loopback `host` in config is refused at
  load, not warned about.
- **Credentials are `${ENV_VAR}` references, never literals.** They expand only at adapter build
  time, so `gateway.config.json`, `managed.json` and `tunnels.json` hold the reference, not the
  secret. The panel masks them back out. Keep it that way.
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

- **The panel's JavaScript is the spec for the admin API.** `crates/lmg-panel/src/admin_assets/`
  is copied from `../local-mcp-gateway/src/admin` byte for byte and is not to be edited here.
  Every `/api/*` response must therefore be shape-identical to what the Node build returns. If a
  response shape feels wrong, fix it in the Node build first and copy the panel over again — never
  fork the panel. `the_tree_is_byte_for_byte_the_node_builds` in `crates/lmg-panel/src/admin.rs`
  enforces this whenever the sibling checkout is present, which on a developer's machine it is.
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
cargo build --release             # the shipping exe (target/release/lmg.exe) - ADR-012
cargo test --workspace            # the one feature combination there is
cargo clippy --workspace --all-targets -- -D warnings                  # must be clean
cargo tree -d                  # a duplicated TLS stack or runtime must fail review
cargo run -- start --no-open   # the gateway itself, on 127.0.0.1:19999
```

**`--workspace` is not optional.** Without it cargo selects the root package alone — 199 of the
suite's 941 tests — and the seven member crates, most of the tests, are never even built. The run
still reports ok. The same applies to clippy. `lmg start` / `stop` / `status` / `logs` / `token` are the CLI; `MCP_GATEWAY_TOKEN` pins
the bearer token when you want a fixed one.

The Node-sealed envelope fixture is regenerated with `cd ../local-mcp-gateway && npx tsx
../local-mcp-gateway-rust/scripts/seal-fixture.mts` when the envelope format ever changes (it
must not — docs/05).

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
  `%LOCALAPPDATA%\lmg-test-home` (DPAPI opens a copied `master.key` on the same machine under
  the same user — docs/05) and points `MCP_GATEWAY_HOME` there, so a save on 19998 writes the
  **test home**, never the user's config. No read-only discipline needed any more — the
  snapshot is the isolation.
- It serves from `target-test\release\lmg.exe`: the 19999 daemon holds `target\release\lmg.exe`,
  so iteration builds still go to a separate directory
  (`$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`).
- Still never `--port`/`-p` on either instance: both `start` and `serve` write the port into
  the config. The env vars the script sets are not persisted.
- Kill 19998 **by the port's owning PID** (`Get-NetTCPConnection -LocalPort 19998`), never by
  process name (`Get-Process lmg` kills the user's instance too). `-Stop` does exactly this.
- **Deploying to 19999 is the last step, done once**: only after every gate passes AND live
  verification on 19998 succeeds, stop 19999, rebuild the main target, start it again — and
treat that as a deployment, not a test.

On Windows, note two environment traps that are not this repo's doing. Windows **Smart App
Control**, if enabled, blocks freshly linked unsigned executables — cargo's build scripts and test
binaries — with `os error 4551`; the failure looks like a broken test but the binary never ran, and
re-running usually gets past it. And full debuginfo across eight link targets exhausts the paging
file (`os error 1455`), which is why `[profile.dev]` in the root manifest keeps line tables only.

## Making changes

- **A behaviour change ships with a test** that fails before it and passes after. Integration
  tests drive the axum app through `tower::ServiceExt::oneshot` — no real port, no real sleep.
- **Porting a module is not done until its Node test file is ported too.** The vitest suite is the
  acceptance spec; see `docs/08-testing.md`.
- **Write all code comments in English**, including in docs code samples.
- **Never commit** `gateway.config.json`, `.env`, `managed.json`, `tunnels.json`, `master.key` or
  `*.log` — all gitignored, all carry real secrets locally. The same goes for
  `~/.mcp-gateway/terminal/*.cast`: a terminal recording is output-only by design, but shells echo
  what was typed, so a recording can still hold a password that was entered.

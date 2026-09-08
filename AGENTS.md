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

- **The panel's JavaScript is the spec for the admin API.** `src/admin/` is copied from the Node
  build byte for byte and is not to be edited here. Every `/api/*` response must therefore be
  shape-identical to what the Node build returns. If a response shape feels wrong, fix it in the
  Node build first and copy the panel over again — never fork the panel.
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
cargo build --release --features mongo   # the shipping exe (target/release/lmg.exe) - ADR-004
cargo test --features mongo    # unit + integration (tests/spike.rs drives a real MCP client)
cargo test                     # and again with default features: the boundary must hold
cargo test --lib -- --ignored  # the live DPAPI hand-check; needs this machine's ~/.mcp-gateway
cargo clippy --all-targets -- -D warnings                   # must be clean
cargo clippy --all-targets --features mongo -- -D warnings  # both combinations
cargo tree -d                  # a duplicated TLS stack or runtime must fail review
cargo run                      # Phase 0 spike: echo MCP on 127.0.0.1:19998
```

The Phase 0 spike runs until Phase 1's `start` subcommand replaces it; `MCP_GATEWAY_TOKEN` pins
its bearer token (a random one is generated and printed otherwise). The Node-sealed envelope
fixture is regenerated with `cd ../local-mcp-gateway && npx tsx
../local-mcp-gateway-rust/scripts/seal-fixture.mts` when the envelope format ever changes (it
must not — docs/05).

## Making changes

- **A behaviour change ships with a test** that fails before it and passes after. Integration
  tests drive the axum app through `tower::ServiceExt::oneshot` — no real port, no real sleep.
- **Porting a module is not done until its Node test file is ported too.** The vitest suite is the
  acceptance spec; see `docs/08-testing.md`.
- **Write all code comments in English**, including in docs code samples.
- **Never commit** `gateway.config.json`, `.env`, `managed.json`, `tunnels.json`, `master.key` or
  `*.log` — all gitignored, all carry real secrets locally.

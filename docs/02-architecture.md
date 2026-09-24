# 02 — Architecture

> 2026-09-22 amendment (`062928a`, cleanup in `78cd561`, merged at `f3b6899`): the workspace
> gained a tenth member, `crates/swiss-it` — the dev-only integration-test harness behind
> gate 2 (docs/44). Heading, tree and the dev-graph note below are updated for it; the
> shipping edges are unchanged.

## Ten crates, one binary

```
swiss/
  Cargo.toml                            # the workspace, and the `swiss` composition package
  build.rs                              # Windows manifest + version resource only
  crates/
    swiss-core/src/                       # knows nothing about gateways
      paths.rs log.rs util.rs atomic_json.rs
      secure/ mod.rs envelope.rs key.rs statefile.rs envstore.rs
      platform/ mod.rs windows.rs unix.rs privfs.rs   # process tree, job objects, DPAPI
    swiss-host/src/                       # the mechanism every subsystem shares
      host/ mod.rs descriptor.rs factory.rs engine.rs api.rs scope.rs
      services/ mod.rs action.rs actions.rs runs.rs process.rs catalog.rs api.rs
      config.rs config_store.rs managed.rs token.rs auth.rs local_only.rs
      mask.rs mem.rs pathenv.rs proc_pids.rs dbbrowser.rs reply.rs
    swiss-mcp/src/                        # MCP itself: the largest crate
      registry.rs calls.rs traffic.rs paging.rs mcp_import.rs introspect.rs
      adapters/ mod.rs echo.rs proc.rs http.rs rest.rs direct.rs proxy.rs
                sql.rs mysql.rs pg.rs redis.rs
                resources.rs tool_server.rs *_browser.rs *_resources.rs
    swiss-data/src/     dbbrowser_api.rs   # /api/data/* over the connection catalog
    swiss-tunnels/src/  tunnel/…           # types, store, manager, forward, port, ssh, mcpmatch, import, api
    swiss-jobs/src/     jobs/…             # def, migrate, schedule, clock, state, runner, runlog, api
    swiss-terminal/src/ terminal/…         # config, session, tickets, recording, local shells (no axum, no SSH)
    swiss-remote/src/   target.rs actions.rs sync.rs project.rs api.rs
                                          # the R1-R5 remote-execution surface (docs/34): target
                                          # table, remote.exec/sync/pull actions, /api/remote
    swiss-panel/src/    admin.rs
                      admin_assets/      # the committed ts-blank-space emit rust_embed serves
        panel/          src/ test/       # authored here since ADR-024 (docs/36); npm run check
                                         # is its gate; renders the two-level navigation (docs/13)
    swiss-it/                            # the dev-only integration harness (docs/44): empty
                                         # without its `it` feature, never in the exe
      src/ lib.rs engine.rs docker_raw.rs exit.rs seed.rs
      bin/ it-reaper.rs it-mcp-server.rs # the cleanup watchdog; the L3 stdio far side
      seed/ mysql/ postgres/ redis/      # committed schema/data every test restores
      tests/it/ *.rs                     # gate 2's ONE binary: L1 mysql/pg/redis browsers,
                                         # L2 real-listener gateway, L3 proc, reaper, seed, smoke
  src/                                   # the composition crate: what wires the rest together
    main.rs lib.rs                       # thin argv parse; what the integration tests drive
    app.rs server.rs adminapi.rs         # axum Router assembly, /api/*
    builtin.rs plugins/                  # every plugin descriptor
    bootstrap.rs subsystems.rs port.rs mcp_link.rs
    autostart.rs update_check.rs         # OS start-at-sign-in; the `swiss update` check
    daemon.rs cli.rs remote_cli.rs pidfile.rs   # the `swiss` command (+ `swiss remote/run`, docs/34)
    skill_install.rs
  tests/                                 # integration; mostly lib.rs through tower::oneshot,
                                         # plus the real-socket legs (groups_e2e, terminal_ws)
```

**A workspace, and the dependency edges are the point.** It buys nothing at runtime — the product
is still one static `swiss.exe`, and the split cost +1.1% of binary size in crate-boundary codegen —
but it makes the plugin architecture a fact the compiler enforces rather than a claim in a
document. The edges are exactly:

```
swiss-core  ←  swiss-host  ←  { swiss-mcp, swiss-data, swiss-tunnels, swiss-jobs, swiss-terminal, swiss-remote, swiss-panel }  ←  swiss
```

No subsystem crate depends on another. That is not decoration: Data used to reach into MCP for
its connections, and the connection catalog in `crates/swiss-host/src/services/catalog.rs` exists so that
edge could be deleted (docs/09 P4). If a future change needs `swiss-data → swiss-mcp` back, the
contract is missing something — add it to the catalog rather than the edge.

`crates/swiss-it` is the tenth member and deliberately absent from that diagram: nothing depends
on it — it is a leaf, dev-only by construction (docs/44 §2.1, ADR-028) — and its `it`-gated
dependencies (testcontainers plus the product crates themselves, for gate 2's real-database
suites) live in the dev graph only. The shipping graph that AGENTS.md's duplicate check owns
(`cargo tree -e normal,build`) never sees a byte of it; the dev graph may carry its own copies.

`src/` is deliberately the smallest crate that could hold what is left: composition names every
other crate, so nothing else has to.

## Runtime model — and the trap in it

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> { … }
```

**`current_thread` does not mean you may use `Rc`.** `axum::serve` spawns each connection with
`tokio::spawn`, which requires `Send` futures regardless of runtime flavour. Shared state is
therefore `Arc<RwLock<…>>` — but on a single-threaded runtime the lock is never contended, so it
costs an atomic and nothing else. Do not fight this with `LocalSet` and a hand-rolled
`hyper::server::conn` loop; the complexity is real and the saving is not.

```rust
#[derive(Clone)]
pub struct AppState {
    registry: Arc<RwLock<Registry>>,
    tokens:   Arc<TokenManager>,
    store:    Arc<ManagedStore>,
    tunnels:  Option<Arc<TunnelState>>,
}
```

## How an MCP endpoint is served

The Node build resolves MCP paths dynamically from the registry, so endpoints can appear and
disappear at runtime. That rules out mounting each MCP as a static axum route. Instead, mirror
`router.ts` exactly: one catch-all that resolves the entry and drives its service by hand.
Since docs/24 (ADR-018) the catch-all sits under the `/mcp/` prefix — the MCP plugin's
domain — leaving the root to host chrome and whatever a future plugin claims there.

```rust
// POST /mcp/:name — the whole MCP surface. (Node served /:name at the root; docs/24 moved it.)
async fn mcp_endpoint(
    State(st): State<AppState>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    // 1. bearer check BEFORE the body is read (Handler.refuse in the Node build)
    // 2. entry lookup; 503 with a JSON-RPC shaped error when unknown
    // 3. lifecycle == Idle -> await ensure_started(): this request IS the wakeup
    // 4. note_activity(): served traffic pushes the idle-reap deadline back
    // 5. cached StreamableHttpService for this entry's generation -> service.call(req)
    // 6. record_traffic() with the captured response
}
```

Per-entry services are cached by generation, exactly as `handlers: Map<string, EntryHandler>` is
today, and evicted on rename/delete through the same `set_evictor` callback. Getting this wrong
leaks a service per rename — the Node build has a comment explaining that bug; do not re-earn it.

## The MCP protocol layer

`rmcp` 3.x carries it. The pieces that map one-to-one:

| Node | Rust |
| --- | --- |
| `createMcpHandler(factory, { legacy: "stateless" })` | `StreamableHttpService` with `legacy_session_mode(false)` |
| `toNodeHandler(handler)` | it is already a `tower::Service` — call it directly |
| `Adapter.makeServer()` (fresh server per request) | the service's per-request handler factory |
| `handler.notify.toolsChanged()` etc. | rmcp's transport-neutral subscription streams |
| `handler.bus.subscribe(...)` → `recordBusEvent` | subscribe once per entry at build time, same sink |
| `StdioClientTransport` (proc adapter) | `TokioChildProcess` (`transport-child-process`) |
| zod schema → `tools/list` JSON Schema | `schemars` (`schemars` feature) |

Required features: `server`, `client`, `transport-streamable-http-server`,
`transport-child-process`, `schemars`. Deliberately **not** `transport-io` (the gateway is never
itself a stdio server) and not `transport-worker`.

## The Adapter trait

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn kind(&self) -> &str;

    /// Build the MCP server once, holding its own connection. Not bound to a transport.
    async fn build(&self) -> Result<ServerHandle>;

    /// Reachability probe. `None` means this kind deliberately has no ping —
    /// http/rest are metered third-party endpoints and must not be polled. Do not "fix" this.
    async fn ping(&self) -> Option<Result<()>> { None }

    async fn close(&self) -> Result<()> { Ok(()) }

    /// Captured child stderr, for the panel's log view. proc only.
    fn logs(&self) -> Option<String> { None }

    /// Root PIDs of any spawned subtree, so the memory view knows what to measure. proc only.
    fn pids(&self) -> Vec<u32> { Vec::new() }

    /// Live, shared tool-toggle state: the admin API mutates it, the server reads it.
    fn tool_toggle(&self) -> Option<Arc<RwLock<HashSet<Box<str>>>>> { None }
    fn resource_toggle(&self) -> Option<Arc<AtomicBool>> { None }

    fn db_browser(&self)    -> Option<&dyn DbBrowser>    { None }
    fn redis_browser(&self) -> Option<&dyn RedisBrowser> { None }
}
```

Default methods stand in for TypeScript's optional members, so a new adapter implements two methods
and inherits the rest. The `Option<Result<()>>` return on `ping` is deliberate: it encodes
"deliberately has no ping" separately from "the ping failed", which is the distinction the registry
needs to report `unknown` instead of `down`.

## What is NOT re-architected

The registry's shape is load-bearing and hard-won — the operation queue that serialises a single
entry's lifecycle, the `gen` counter that lets a late health probe know its result is stale, the
tombstone flag that stops a queued start from resurrecting a deleted entry. Port these as they
are. Each exists because of a race that was diagnosed once already, and the comments in
`registry.ts` name them.

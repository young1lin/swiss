# 02 — Architecture

## One crate, one binary

```
local-mcp-gateway-rust/
  Cargo.toml
  build.rs                  # Windows manifest + version resource only
  src/
    main.rs                 # thin: parse argv, hand off to lib
    lib.rs                  # pub mod wiring; what the integration tests drive
    config.rs               # GatewayConfig / ServerDef, ${ENV} expansion
    app.rs                  # axum Router assembly  (was: router.ts + http.ts)
    local_only.rs           # loopback enforcement   (SECURITY — port the tests first)
    auth.rs  token.rs       # bearer parsing, the named-token set
    registry.rs             # lifecycle, health probe, lazy wake, idle reap
    calls.rs  traffic.rs    # the two on-disk logs
    mask.rs                 # the shared secret wordlist + panel masking
    paging.rs               # tool/resource/prompt page cache
    admin/
      mod.rs                # asset embedding + serving
      api.rs                # /api/*                (was: adminapi.ts, 914 lines)
      dbbrowser.rs          # /api/data/*           (was: dbbrowser*.ts, 1104 lines)
    adapters/
      mod.rs                # the Adapter trait
      factory.rs            # type -> adapter
      echo.rs proc.rs http.rs rest.rs
      sql.rs mysql.rs pg.rs redis.rs mongo.rs
      resources.rs tool_server.rs proxy.rs
    tunnels/
      mod.rs manager.rs forward.rs port.rs store.rs ssh.rs api.rs mcpmatch.rs
    secure/
      mod.rs envelope.rs key.rs statefile.rs envstore.rs
    daemon.rs cli.rs pidfile.rs         # the `lmg` command
    platform/
      mod.rs windows.rs unix.rs         # process tree, job objects, DPAPI, machine id
    admin_assets/                       # copied verbatim from ../local-mcp-gateway/src/admin
  tests/                                # integration; drives lib.rs through tower::oneshot
```

Single crate, not a workspace. A workspace buys nothing at runtime and costs build complexity;
`src/lib.rs` alongside `src/main.rs` is already enough for `tests/` to drive the real app.

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

```rust
// POST /:name — the whole MCP surface. Mirrors router.ts's nodeHandlerFor + POST /:path.
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
    fn mongo_browser(&self) -> Option<&dyn MongoBrowser> { None }
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

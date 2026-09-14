# swiss Integration-Test Master Plan (how to write, where to write, entry point to the functionality-point matrices)

> Source material: an analysis of the whole repo's test infrastructure (the 8 root tests/ files + every crate's inline tests, inventoried one by one) + 5 per-domain test plans.
> Goal: whenever a functionality point needs a test, come here first for patterns and helpers, then go to the domain matrix for that point's assertion essentials.

## 1. Test Landscape (about 940 tests in total)

| Layer | Location | Scale | Driving method |
| --- | --- | --- | --- |
| Root integration | tests/{app,adminapi,plugin_host,terminal_ws,http_adapter,memory,env_precedence,envelope_compat}.rs | ~5,977 lines / 139 tests | swiss::app::build_app(ctx, extra) + tower::ServiceExt::oneshot, no real port |
| Crate integration | crates/swiss-mcp/tests/dbbrowser_wiring.rs, crates/swiss-core/tests/seal_bench.rs (#[ignore]) | 6 tests | embedded include_str! fixtures |
| Crate inline | per-crate #[cfg(test)] (~86 files: swiss-mcp ~266 tests, swiss-host ~174, swiss-jobs ~86, swiss-tunnels ~59, swiss-core ~46, swiss-data 18, swiss-panel 9) | ~800 tests | unit / pure functions / injected fakes |

Locating rule: **behavior on the HTTP surface gets end-to-end assertions in the root tests/; pure functions and data structures live in crate inline tests; both count as "this functionality point has a test"**. adminapi.rs (71 tests) is a route-by-route port of the Node adminapi.test.ts — when adding an endpoint, find a neighbor there first and copy it.

## 2. Standard Test Shape (copy this skeleton)

```rust
// Setup: scratch dir + deterministic master key + empty registry + build_app
let scratch = std::env::temp_dir()
    .join(format!("swiss-<tag>-{}", swiss_core::util::random_hex(8)));
let calls = Arc::new(swiss_mcp::calls::CallLog::at(scratch.join("calls")));
let registry = Registry::new(3_600_000, calls.clone());
let store = Arc::new(ManagedStore::open_at(scratch.join("managed.json")));
let adapter = make_adapter(&echo_def(), "echo", &calls).expect("echo adapter");
registry.register("echo", Source::Config, echo_def(), adapter).expect("register");
let ctx = AppContext::new(registry, tokens, store, calls, "MCP_GATEWAY_TOKEN", 19998);
let app = build_app(ctx, None);

// Request: patch in a loopback Host header (the loopback guard reads it before routing) → oneshot → read body
let mut req = Request::get("/api/mcps").body(Body::empty()).unwrap();
req.headers_mut().entry(header::HOST)
    .or_insert(header::HeaderValue::from_static("127.0.0.1:19999"));
let res = app.oneshot(req).await.expect("router answers");
assert_eq!(res.status(), StatusCode::OK);
```

## 3. Shared Helpers Quick Reference (the existing ones — reuse before building new)

- **tests/adminapi.rs**: `sandbox()` (a one-shot OnceLock setting MCP_GATEWAY_HOME + MASTER_KEY), `Harness` (setup/register/register_fixture/get/post/put/delete/send/mcp/names), `Fixture` (a Rust stand-in for the Node stdio-echo.mjs: an echo tool + a single resource + ping + rename), `traffic_lock()`/`VAULT_LOCK` (process-level global-state mutexes), `row_named`/`field_of`/`json_req`/`rest_def`.
- **tests/plugin_host.rs**: `full_app(tag, raw)` (BuiltinDeps fully assembled), `full_app_file_backed` (seals jobs.json, then boots; uses "gone/..." to force a persistence failure), `await_run(app, run_id)` (20ms polling to a terminal state), `echo_input`/`legacy_echo` (cross-platform cmd/sh spellings), `plugin_row`.
- **tests/terminal_ws.rs**: `rig(tag, config)` (full assembly + tunnels disabled + FakeShells taking the shell seat + a real temp port), `recv` (5s timeout), `wait_detached`.
- **Crate level** (`#[cfg(any(test, feature = "test-utils"))]`): `swiss_core::paths::test_home()` + `DATA_DIR_LOCK` (serializes data-dir writes), `swiss_mcp::calls::test_log()`, `random_hex`, jobs `clock.rs testing` (an injectable fake Clock, synthesizing DST), tunnels `SshLike`/`FakeConn` (counting + fault injection, zero sshd).

## 4. Environment and Time Iron Rules

1. **Every test binary sets MCP_GATEWAY_MASTER_KEY** (deterministic bytes, e.g. "ab"×32; a one-shot unsafe set_var inside a OnceLock, the comment arguing "before any read") — the suite never spawns the keystore, so CI can run.
2. **Temp home**: `temp_dir().join(format!("swiss-<tag>-{}", random_hex(8)))`; no tempfile crate (ADR-007).
3. **Time**: ticket/idle/stall use `#[tokio::test(start_paused = true)]` + `advance()` (gotcha: the paused clock creeps forward 1s per park; terminal_ws.rs:562 has the full countermeasure); jobs does not use tokio time, it goes through Clock injection.
4. **env changes**: only inside an integration binary that owns the process (the env_precedence.rs pattern), with restore() putting things back.
5. **DB**: the current DB surface is distilled into connection-free functions (quote_ident/is_read_only_sql/clamp_row_limit); live-DB tests are left for the future, and must then self-skip when credentials are absent (the docs/08 rule).
6. **Child processes**: Windows has no shell sleep — use `ping -n 60 127.0.0.1` as the long runner; poll at 20ms×cap, never a bare sleep.

## 5. Three Test Categories That Cannot Use oneshot (each has an established pattern)

| Category | Pattern | Example |
| --- | --- | --- |
| A real rmcp HTTP client | temp port 127.0.0.1:0 + tokio::spawn(axum::serve) + server.abort() at the end | tests/app.rs:332 |
| WebSocket (upgrade cannot oneshot) | terminal_ws rig: real socket + FakeShells | tests/terminal_ws.rs |
| http/rest adapter peer | an echo gateway started in this process, remote_echo() | tests/http_adapter.rs |

## 6. Mutual Exclusion for Process-Level Global State

The traffic ring and the secret vault are process-global: same-file static tokio Mutexes (`traffic_lock()`/`VAULT_LOCK`, guards held across await) serialize them; tests that write the data dir go through `DATA_DIR_LOCK`. When a new test touches global state, find the lock first, then act.

## 7. Panel Test Boundaries (important)

- **Tests of panel behavior (rendering/interaction/keyboard) live in the Node repo** (the vitest of `../local-mcp-gateway`: test/admin-pages.test.ts, test/admin-panel.test.ts, plus pure-function unit tests of page-core.js). This repo's `admin_assets` is frozen byte for byte; not a character changes.
- **This repo tests exactly three things**: /api/* response shapes (the panel JS is the admin API's spec, ADR-009), asset serving and path guards (tests/app.rs), and the byte-equivalence guard (swiss-panel/admin.rs, enforced when the sibling checkout is present).

## 8. Per-Domain Matrices (one row per functionality point: existing tests → gaps → new test name + Arrange/Act/Assert)

| Domain | Matrix doc | Main existing coverage |
| --- | --- | --- |
| MCP gateway | [tests/mcp.md](mcp.md) | tests/{app,adminapi,http_adapter}.rs + ~266 inline in crates/swiss-mcp |
| Data browsing | [tests/data.md](data.md) | crates/swiss-data/dbbrowser_api.rs 19 + swiss-host/dbbrowser.rs 30 + dbbrowser_wiring.rs 5 |
| Tunnels + Jobs | [tests/tunnels-jobs.md](tunnels-jobs.md) | manager.rs 37 (FakeConn) + the whole jobs family (clock injection) |
| Terminal + Process | [tests/terminal-process.md](terminal-process.md) | terminal_ws.rs 15 + session/tickets/recording + the plugin_host execution surface |
| Host/core/CLI | [tests/host.md](host.md) | plugin_host.rs 28 + envelope_compat.rs + ~46 core inline |

## 9. New Tests: Do / Don't

**Do**: write the failing test first for a behavior change; pin shapes in the asserts (absent-not-null, the JSON-RPC vs {error} dual tracks); English sentence-style test names (consistent with existing ones, e.g. `boot_disabled_plugins_guard_every_route_they_own`); the gates are always `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`.

**Don't**: no bare sleeps (poll or inject a clock); never spawn powershell in a test; never edit admin_assets; never step on global state in parallel without holding the lock; never run tests on the root package only (--workspace is not optional); never widen product-code visibility just for a test (use the test-utils feature).

# swiss Integration-Test Master Plan (how to write, where to write, entry point to the functionality-point matrices)

> Source material: an analysis of the whole repo's test infrastructure (the 9 root tests/ files + every crate's inline tests, inventoried one by one) + 5 per-domain test plans.
> Goal: whenever a functionality point needs a test, come here first for patterns and helpers, then go to the domain matrix for that point's assertion essentials.

## 1. Test Landscape (about 1,431 tests in total, plus 78 behind the swiss-it feature)

| Layer | Location | Scale | Driving method |
| --- | --- | --- | --- |
| Root integration | tests/{app,adminapi,plugin_host,terminal_ws,http_adapter,memory,env_precedence,envelope_compat,groups_e2e}.rs | 9 files / ~8,351 lines / 184 tests | swiss::app::build_app(ctx, extra) + tower::ServiceExt::oneshot, no real port (groups_e2e.rs additionally serves on a real ephemeral loopback socket and drives a reqwest client across a full app restart) |
| Crate integration | crates/swiss-mcp/tests/dbbrowser_wiring.rs, crates/swiss-core/tests/seal_bench.rs (#[ignore]) | 6 tests | embedded include_str! fixtures |
| Crate inline | per-crate #[cfg(test)] (114 files across the nine member crates + root src/: swiss-mcp 332 tests, swiss-host 298, root src/ 115, swiss-tunnels 120, swiss-core 111, swiss-remote 68, swiss-jobs 90, swiss-terminal 50, swiss-data 46, swiss-panel 8) | ~1,240 tests | unit / pure functions / injected fakes |

Locating rule: **behavior on the HTTP surface gets end-to-end assertions in the root tests/; pure functions and data structures live in crate inline tests; both count as "this functionality point has a test"**. adminapi.rs (111 tests) is a route-by-route port of the Node adminapi.test.ts — when adding an endpoint, find a neighbor there first and copy it.

## 2. The Second Gate: swiss-it (real databases, fail-not-skip)

Live-database tests are no longer a future self-skipping extension of the unit suites: they live in the dev-only crate `crates/swiss-it` behind the `it` feature (docs/44), and asking for the feature is asking for real databases.

- **Command**: `cargo test -p swiss-it --features it` — gate 2. Gate 1 stays `cargo test --workspace`, green on any machine: without the feature swiss-it compiles to three empty targets, and nothing in the shipping graph depends on the crate.
- **Engines (crates/swiss-it/src/engine.rs)**: one engine per kind per test process, resolution order fixed and printed on failure — (1) `SWISS_IT_MYSQL_URL` / `SWISS_IT_POSTGRES_URL` / `SWISS_IT_REDIS_URL` supplies an existing database used as-is (a set-but-dead override is a hard failure: no silent fallback to containers, or a typo'd URL would mask itself as a green run); (2) testcontainers starts mysql:8.4 / postgres:17 / redis:7 wherever `DOCKER_HOST` points; (3) neither answers → the test FAILS, not skips, carrying the fixed three-part failure report.
- **Shape**: one test binary (crates/swiss-it/tests/it/main.rs, `#![cfg(feature = "it")]`, suites as modules — 78 tests: mysql 16, pg 16, redis 19, gateway 7, proc 7, seed 8, reaper 2, smoke 3). L1 drives the product's own MysqlBrowser/PgBrowser/RedisDataBrowser straight from a def; L2 boots the real gateway on 127.0.0.1:0 and speaks rmcp plus the admin API over real HTTP; L3 proves the proc adapter against this repo's own stdio MCP server binary. Every test restores its own database from the committed seeds (crates/swiss-it/seed/); an it-reaper watchdog child plus an atexit hook delete started containers on every exit path.
- **Enforcement**: CI's ubuntu `integration` job runs `cargo test -p swiss-it --features it --locked` (.github/workflows/build.yml) and the tag-gated `release` job needs it; scripts/deploy.ps1 runs the same command as gate 2 before touching production. The gate is mandatory when the diff touches the DB adapters/browsers or swiss-it itself (AGENTS.md).

## 3. Standard Test Shape (copy this skeleton)

```rust
// Setup: scratch dir + deterministic master key + empty registry + build_app
let scratch = std::env::temp_dir()
    .join(format!("swiss-<tag>-{}", swiss_core::util::random_hex(8)));
let calls = Arc::new(swiss_mcp::calls::CallLog::at(scratch.join("calls")));
let registry = Registry::new(3_600_000, calls.clone());
let store = Arc::new(ManagedStore::open_at(scratch.join("managed.json")));
let adapter = make_adapter(&echo_def(), "echo", &calls).expect("echo adapter");
registry.register("echo", Source::Config, echo_def(), adapter).expect("register");
let ctx = AppContext::new(registry, tokens, store, calls, "SWISS_TOKEN", 19998);
let app = build_app(ctx, None);

// Request: patch in a loopback Host header (the loopback guard reads it before routing) → oneshot → read body
let mut req = Request::get("/api/mcps").body(Body::empty()).unwrap();
req.headers_mut().entry(header::HOST)
    .or_insert(header::HeaderValue::from_static("127.0.0.1:19999"));
let res = app.oneshot(req).await.expect("router answers");
assert_eq!(res.status(), StatusCode::OK);
```

## 4. Shared Helpers Quick Reference (the existing ones — reuse before building new)

- **tests/adminapi.rs**: `sandbox()` (a one-shot OnceLock setting SWISS_HOME + MASTER_KEY), `Harness` (setup/register/register_fixture/get/post/put/delete/send/mcp/names), `Fixture` (a Rust stand-in for the Node stdio-echo.mjs: an echo tool + a single resource + ping + rename), `traffic_lock()`/`VAULT_LOCK` (process-level global-state mutexes), `row_named`/`field_of`/`json_req`/`rest_def`.
- **tests/plugin_host.rs**: `full_app(tag, raw)` (BuiltinDeps fully assembled), `full_app_file_backed` (seals jobs.json, then boots; uses "gone/..." to force a persistence failure), `await_run(app, run_id)` (20ms polling to a terminal state), `echo_input`/`legacy_echo` (cross-platform cmd/sh spellings), `plugin_row`.
- **tests/terminal_ws.rs**: `rig(tag, config)` (full assembly + tunnels disabled + FakeShells taking the shell seat + a real temp port), `recv` (5s timeout), `wait_detached`.
- **Crate level** (`#[cfg(any(test, feature = "test-utils"))]`): `swiss_core::paths::test_home()` + `DATA_DIR_LOCK` (serializes data-dir writes), `swiss_mcp::calls::test_log()`, `random_hex`, jobs `clock.rs testing` (an injectable fake Clock, synthesizing DST), tunnels `SshLike`/`FakeConn` (counting + fault injection, zero sshd).

## 5. Environment and Time Iron Rules

1. **Every test binary sets SWISS_MASTER_KEY** (deterministic bytes, e.g. "ab"×32; a one-shot unsafe set_var inside a OnceLock, the comment arguing "before any read") — the suite never spawns the keystore, so CI can run.
2. **Temp home**: `temp_dir().join(format!("swiss-<tag>-{}", random_hex(8)))`; no tempfile crate (ADR-007).
3. **Time**: ticket/idle/stall use `#[tokio::test(start_paused = true)]` + `advance()` (gotcha: the paused clock creeps forward 1s per park; terminal_ws.rs:588-594 has the full countermeasure); jobs does not use tokio time, it goes through Clock injection.
4. **env changes**: only inside an integration binary that owns the process (the env_precedence.rs pattern), with restore() putting things back.
5. **DB**: unit tests stay connection-free (pure functions over defs and SQL strings); real-database tests belong to gate 2 (swiss-it, section 2) — behind `--features it` they FAIL, not skip, when neither `SWISS_IT_*_URL` nor Docker answers (engine.rs resolution order; docs/08's successor paragraph, docs/44).
6. **Child processes**: Windows has no shell sleep — use `ping -n 60 127.0.0.1` as the long runner; poll at 20ms×cap, never a bare sleep.

## 6. Three Test Categories That Cannot Use oneshot (each has an established pattern)

| Category | Pattern | Example |
| --- | --- | --- |
| A real rmcp HTTP client | temp port 127.0.0.1:0 + tokio::spawn(axum::serve) + server.abort() at the end | tests/app.rs:437 (mcp_endpoint_serves_a_real_client) |
| WebSocket (upgrade cannot oneshot) | terminal_ws rig: real socket + FakeShells | tests/terminal_ws.rs |
| http/rest adapter peer | an echo gateway started in this process, remote_echo() | tests/http_adapter.rs |

## 7. Mutual Exclusion for Process-Level Global State

The traffic ring and the secret vault are process-global: same-file static tokio Mutexes (`traffic_lock()`/`VAULT_LOCK`, guards held across await) serialize them; tests that write the data dir go through `DATA_DIR_LOCK`. When a new test touches global state, find the lock first, then act.

## 8. Panel Test Boundaries (important)

- **Tests of panel behavior (rendering/interaction/keyboard) live in THIS repo**: the vitest suite under `crates/swiss-panel/panel/test/` — 86 `*.test.ts` files (admin-pages/admin-panel plus per-module suites such as admin-data-grid, admin-terminal, admin-tunnels-pages, data-stream). The panel is authored in TypeScript at `crates/swiss-panel/panel/src/*.ts` (ADR-024, docs/36); `npm run check` in `crates/swiss-panel/panel/` runs typecheck ×2 + eslint + emit-freshness + that vitest suite and is the panel's gate. A panel change ships its vitest case in the same commit (AGENTS.md).
- **The Rust side of this repo tests**: /api/* response shapes (the panel TS is the admin API's spec, ADR-009) and asset serving + path guards (tests/app.rs, swiss-panel/admin.rs inline: SHA-1 vectors, extension whitelist, path guard, embedded shell, stamp). The byte-equivalence guard against the Node checkout was retired with ADR-025 — the check suite replaced it.

## 9. Per-Domain Matrices (one row per functionality point: existing tests → gaps → new test name + Arrange/Act/Assert)

| Domain | Matrix doc | Main existing coverage |
| --- | --- | --- |
| MCP gateway | [tests/mcp.md](mcp.md) | tests/{app,adminapi,http_adapter}.rs + 332 inline in crates/swiss-mcp |
| Data browsing | [tests/data.md](data.md) | crates/swiss-data/dbbrowser_api.rs 46 + swiss-host/dbbrowser.rs 88 + dbbrowser_wiring.rs 5 + the swiss-it live-DB suites (gate 2) |
| Tunnels + Jobs | [tests/tunnels-jobs.md](tunnels-jobs.md) | manager.rs 52 (FakeConn) + the whole jobs family (clock injection) |
| Terminal + Process | [tests/terminal-process.md](terminal-process.md) | terminal_ws.rs 15 + session/tickets/recording + the plugin_host execution surface |
| Host/core/CLI | [tests/host.md](host.md) | plugin_host.rs 28 + envelope_compat.rs + 111 core inline |

## 10. New Tests: Do / Don't

**Do**: write the failing test first for a behavior change; pin shapes in the asserts (absent-not-null, the JSON-RPC vs {error} dual tracks); English sentence-style test names (consistent with existing ones, e.g. `boot_disabled_plugins_guard_every_route_they_own`); the gates are always `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`, plus gate 2 (`cargo test -p swiss-it --features it`) when the diff touches the DB adapters/browsers or swiss-it itself.

**Don't**: no bare sleeps (poll or inject a clock); never spawn powershell in a test; never hand-edit `crates/swiss-panel/src/admin_assets` (edit `crates/swiss-panel/panel/src/*.ts` and emit — ADR-024); never step on global state in parallel without holding the lock; never run tests on the root package only (--workspace is not optional); never widen product-code visibility just for a test (use the test-utils feature).

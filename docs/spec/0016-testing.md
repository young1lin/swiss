# §testing — Gates and suites

Chapter 0016 of the [swiss specification](../SPEC.md).

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
  `sql.rs`, `resources.rs`, `proc.rs`, `crates/swiss-host/src/dbbrowser/`,
  `crates/swiss-data/src/dbbrowser_api/`, `crates/swiss-core/src/secure/`,
  `crates/swiss-core/src/platform/`, `crates/swiss-host/src/services/{process,detached,children}.rs`,
  `src/app.rs`, `src/mcp_link.rs`, or `crates/swiss-it/**`. Any other change may skip it, and the commit
  message says it was not run.
- **A manifest change** also runs `cargo tree -d -e normal,build` and `cargo deny check`
  (§arch.deps, §release.ci).
- **Deploys re-run the gates.** `scripts/deploy.ps1` runs gate 1, then gate 2 (setting
  `DOCKER_HOST` for that one run and removing it after), then clippy, then the panel's
  `npm run check`; any failure stops with "production left untouched" (§host.ops).

## §testing.layers — Where a test goes

| Layer | Location | Drives |
|---|---|---|
| Root integration | `tests/*.rs`: `adminapi` (the admin API, route by route), `app` (client endpoints, boundaries, asset serving), `plugin_host` (plugin gating, actions and runs, the catalog), `terminal_ws` (the WebSocket stream), `http_adapter` (the http adapter as a real proxy), `groups_e2e` (a real socket across a restart), `admin_session`, `env_precedence`, `envelope_compat`, `memory`, `process_apps` (managed apps and the children view over fakes, §process.verify) | `swiss::app::build_app` + `tower::ServiceExt::oneshot`; no port unless the category needs one |
| Crate integration | `crates/swiss-mcp/tests/dbbrowser_wiring.rs`; `crates/swiss-core/tests/seal_bench.rs` (`#[ignore]`); `crates/swiss-host/tests/history_engine.rs` (the linked SQLite engine, §testing.rules) | embedded fixtures |
| Crate inline | `#[cfg(test)] mod tests` in every module that has behaviour | pure functions, injected fakes |
| Gate 2 | `crates/swiss-it` (§testing.it) | real MySQL, PostgreSQL, Redis, a real stdio MCP child, and real detached processes (`detached.rs` with the `it-app` helper, §process.verify) |
| Panel | `crates/swiss-panel/panel/test/*.test.ts` (vitest) | hand-built DOMs following the view layer's idioms (§panel.toolchain) |

**Locating rule.** Behaviour on the HTTP surface gets an end-to-end assertion in the root
`tests/`; pure functions and data structures live inline in their crate. Both count as "this
point has a test". `tests/adminapi/` is the admin API's contract suite, a file per area: a new
endpoint copies a neighbour's test from there.

**Modules without inline tests, by design:**

| Module | Why, and where it is covered |
|---|---|
| `swiss-mcp` `adapters/mysql_browser.rs` | Orchestration over a live connection; the identifier quoting it relies on (`dbbrowser::quote_ident`) is tested in `swiss-host`; the paths themselves run on real engines in gate 2. The pure logic once buried between awaits was lifted out and tested (`redis_browser::scan_args`, `pg_browser::total_of`) |
| `swiss-core` `platform/windows.rs` | Win32 FFI that needs the OS to answer; the process walk both platforms share lives un-`cfg`'d in `platform/mod.rs` and is tested on any host. Both platform halves re-export from one list, so a signature divergence is a build error |
| `swiss` `adminapi.rs`, `app.rs` | Covered end to end by `tests/adminapi/` and `tests/app.rs`, where a route contract belongs |
| Re-export shells (`lib.rs`/`main.rs`, `secure/mod.rs`, `tunnel/mod.rs`) | No behaviour of their own |

`server.rs` is tested at `register_one` (where a panel Stop, a persisted tool toggle and the lazy
rule must survive a restart), not at the boot that binds a port and never returns.
`admin.rs`'s SHA-1 is tested against the published vectors and every block-padding boundary.
One branch is deliberately uncovered: `daemon::StopResult::Forced` — reaching it needs a live
pid to kill, and the only one a test owns is its own runner.

## §testing.rules — Environment, time and shared state

- **`SWISS_MASTER_KEY` in every test binary** — deterministic bytes set once in a `OnceLock`
  (with the SAFETY comment on `set_var`), so the suite never touches an OS key source and CI
  can run it.
- **Scratch homes** are `temp_dir().join(format!("swiss-<tag>-{}", random_hex(8)))` +
  `SWISS_HOME`; no `tempfile` crate (§arch.deps).
- **Time.** Tickets, idle reaping and stalls use `#[tokio::test(start_paused = true)]` +
  `advance()` (the paused clock creeps one second per park; `tests/terminal_ws.rs` carries the
  countermeasure). Jobs never use tokio time: they take an injected `Clock` that can
  synthesise DST.
- **The paused clock cannot boot a module-history store.** Opening one of the stores
  (§host.history) does real blocking work (sqlite); tokio's auto-advance fires every
  pending timer the moment the runtime parks behind it — including the host's 30 s plugin
  start deadline, which kills a healthy start instantly. A test that boots a rig whose
  plugins open stores runs on the real clock and pays its sleeps once
  (`an_expired_ticket_is_refused` is the worked example).
- **No bare sleeps.** Poll at ~20 ms with a cap, or inject a clock. On Windows the long-running
  child is `ping -n 60 127.0.0.1` (there is no shell sleep). A test never spawns PowerShell.
- **Environment changes** only inside an integration binary that owns its process
  (`tests/env_precedence.rs`), restored afterwards.
- **The machine is not a fixture.** No test writes the operator's OS registrations: the admin
  API tests mount fakes on the `AppContext` seams, and the registry helpers are tested against
  a scratch HKCU key of their own, deleted on drop. (A suite that drove the real Run value
  left start-at-sign-in removed after every run.)
- **Databases.** Unit tests stay connection-free (pure functions over defs and SQL strings);
  anything that needs a real engine belongs to gate 2 — one narrow exception: the workspace's
  own embedded SQLite, the bundled engine `swiss-host` links (§arch.deps), may back crate
  tests against scratch homes, because it resolves no service, needs no container, and its
  files live and die under `temp_dir()`. `crates/swiss-host/tests/history_engine.rs` is the
  precedent and the boundary: it exercises the linked library itself (the engine gate and
  capability probes), never an operator's files. MySQL, PostgreSQL, Redis and MongoDB stay
  gate-2-only, unconditionally.
- **Process-global state is locked; the rest is per harness.** Each test composition owns its
  traffic ring (memory-only), so no traffic lock exists. The vault is process-wide:
  `VAULT_LOCK` serialises the tests that touch it, and writes to the data dir go through
  `DATA_DIR_LOCK`. A new test that touches global state takes the lock first.
- **Three categories cannot use `oneshot`:** a real rmcp HTTP client (listen on
  `127.0.0.1:0`, `tokio::spawn(axum::serve)`, abort at the end — `tests/app.rs`), a WebSocket
  (the `terminal_ws` rig with fake shells), and the http/rest adapter's peer (an echo gateway
  started in-process — `tests/http_adapter.rs`).
- **Helpers before new helpers.** `tests/adminapi/`: `sandbox()`, `Harness`, `Fixture` (an
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

## §testing.it — The integration harness (gate 2)

`crates/swiss-it` proves the database adapters, the browsers and the proc path against real
engines (ADR-028), and the detached spawn against real processes (§process.detached). It is a dev-only leaf: every dependency sits behind the `it` feature, so
without it the crate compiles to empty targets and the shipping graph never changes.

| Where | What |
|---|---|
| `src/engine.rs` | One engine per kind per test process; resolution; readiness; the failure report |
| `src/seed.rs`, `seed/` | The committed seeds and `Fresh` (a database per test) |
| `src/exit.rs`, `src/docker_raw.rs`, `src/bin/it-reaper.rs` | Container reaping |
| `src/bin/it-mcp-server.rs` | The in-repo stdio MCP server for L3 |
| `src/bin/it-app.rs` | The managed-app stand-in for L4 (`serve`, `launcher`, `flood`, §process.verify) |
| `tests/it/main.rs` | One test binary; suites as modules: `smoke`, `seed`, `mysql`, `pg`, `redis`, `mongo`, `gateway`, `proc`, `reaper`, `detached` |

**Engines.** `mysql:8.4`, `postgres:17`, `redis:7`, `mongo:8` (a one-member replica set the
harness initiates through the driver, so transactions exist). Resolution order, printed on failure:
(1) `SWISS_IT_MYSQL_URL` / `SWISS_IT_POSTGRES_URL` / `SWISS_IT_REDIS_URL` / `SWISS_IT_MONGO_URL`
— an existing server
used as is; a set but dead override is a hard failure, never a silent fallback; (2)
testcontainers wherever `DOCKER_HOST` points; (3) neither — the test **fails, never skips**,
with a three-part message (what was tried, why it failed, how to fix it). Ports are random;
nothing hard-codes 3306/5432/6379. Containers carry the label `org.swiss-it.owned=1` (the prune
scope; a pid label is informational only). bollard runs on one dedicated worker thread. postgres
starts with `shared_preload_libraries=pg_stat_statements`, which `pg_inspect`'s `top_queries`
test needs — an override server must preload it too. A mongo override must be a **replica set**
(4.4+; one member is enough — the transaction and replication tests need a set), and nothing
in the group names a version, so pointing `SWISS_IT_MONGO_URL` at another release is how a
release is checked: a `mongod` started by hand, or by an embedded-test launcher such as
mongodb-memory-server's `MongoMemoryReplSet` (wiredTiger — its in-memory engine has no
transactions).

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
- **mongo** — a database per test (`it_<tag>_<hex8>`), seeded in code by `seed_mongo` (BSON
  types have no portable text seed): 57 `orders` carrying every type the panel prints, a
  validated `customers`, two indexes, the view `open_orders`. The def is the engine's URL plus
  `database`; the container runs without access control, so unlike the SQL engines the mongo
  suite does not exercise a reduced privilege surface. Dropped with `dropDatabase`.
- Defs use the non-superuser `it` account (mongo aside, above), so L1/L2 hit the privilege surface
  a real deploy has; the superuser is only for the harness (creating databases, counting
  connections).
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
  involved; `dbbrowser_api`'s tests guard that with stubs.
- **L2 — adapters through `/mcp/<name>`** (`tests/it/gateway.rs`): a real gateway on
  `127.0.0.1:0` (with the health probe and the 1 s idle-reap sweeper a daemon runs) and a real
  rmcp client. `list_tools` is exactly the adapter's set; each tool hits the seed; resources
  read. **Credentials are references** — a `${secret://…}` password registered through
  `/api/secrets` before the def, echoed as the reference, never the value. **Stopping an MCP
  releases its connections** — a baseline above zero, then `POST /api/mcps/<name>/stop`, then
  the engine's own connection count for that database must reach 0 within 2 s; red here is a
  product bug, never a test to weaken. One test pins that the gateway's `/api/mcps` rows match
  the root `tests/adminapi/` contract.
- **L3 — proc with the in-repo server** (`tests/it/proc.rs`): `it-mcp-server` writes a line to
  stderr at start (noise must never reach the protocol stream) and serves `echo`,
  `blob` (exact bytes, compared byte for byte — the forwarding path is `&RawValue`), `sleep`,
  `fail` (an MCP error, the process survives) and `env` (the child's variable names — the
  launch-environment scrub holds for proc children). Lazy start, idle reap within the window,
  wake on the next call, and stop kills the tree (job object on Windows, process group on
  unix). The group runs serially: it counts the whole process table by exe name, and an
  unreadable table counts as "many", never as "none".
- **L4 — detached processes** (`tests/it/detached.rs`): `spawn_detached` and the platform probes
  against real children of `it-app`. The test names are in §process.verify. A child survives
  its spawner's kill-on-close job (Windows) or session (Unix). A megabyte of output never
  blocks it. It never holds its spawner's log open. A launcher's child is found as its port's
  owner. A creation time is stable while the process lives. The tree kill reports no
  survivors. The group asks for no engine, so `cargo test -p swiss-it --features it detached`
  runs without Docker.

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

## §testing.memory — The memory guard

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

## §testing.live — Live verification

**19999 is production** for the human on the machine. Iteration never stops, restarts,
redeploys or sends traffic to it — the one allowed request is a read-only `GET /health`.
Production changes only through `scripts/deploy.ps1` (§host.ops).

**19998 is the test instance** (`scripts/test-instance.ps1`, wrapped by the
`swiss-live-verify` skill, with a POSIX twin in the skill):

1. Build into its own target tree (`CARGO_TARGET_DIR=target-test`), never `target/`, which the
   next deploy copies to production. After a panel change, `npm run build` first; the release
   build then re-embeds the changed tree by itself (swiss-panel's `build.rs` tracks it, §panel).
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

## §testing.gaps — Known gaps

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
  flake in `tests/adminapi/`.

A gap closed is deleted from this list in the commit that closes it.

# 03 — Dependency map

> 2026-09-22 amendment (`062928a`, cleanup ownership in `78cd561`, merged at `f3b6899`): the
> workspace gained a dev-only tenth member, `crates/swiss-it`, whose dependency set is recorded
> in the new section below. It maps to nothing in the npm table — the Node build had no such
> harness — and none of it ships: the rows above are unchanged.

## npm → crate

| npm | crate | Notes |
| --- | --- | --- |
| `@modelcontextprotocol/server` `/client` `/node` | **`rmcp` 3.x** | Same 2026-07-28 revision; `legacy_session_mode(false)` is the `legacy: "stateless"` fallback; `TokioChildProcess` replaces `StdioClientTransport` |
| `node:http` + the hand-written `Router` | **`axum`** | The Node build wrote its own router to avoid express's ~9.6 MB. axum has no such problem — that comment does not transfer |
| `mysql2`, `pg` | **`sqlx`** | One crate for both. **Do not use the `query!` macros** — they need a live database at compile time, and this is a generic SQL browser anyway. Runtime `query()` throughout |
| `ioredis` | **`redis`** (redis-rs) | `fred` is the richer client and the heavier one; nothing here needs it |
| `undici` | **`reqwest`** | Carries the `http`/`rest` adapters *and* their HTTP-proxy support, which is why undici was there |
| `ssh2` | **`russh`** + `russh-keys` | Pure Rust, async, no libssh2 build step |
| `zod` | **`serde`** + `schemars` | `schemars` generates the JSON Schema that `tools/list` publishes |
| `dotenv` | *(none)* | `secure/envstore.ts` already replaced plaintext `.env` with a sealed store; port that, not dotenv |
| `AsyncLocalStorage` (call attribution) | **`tokio::task_local!`** | Same shape: set once at the auth boundary, read deep inside the tool handler |

## Deliberately not taken

| Tempting | Why not |
| --- | --- |
| `tracing` + `tracing-subscriber` | `log.ts` is **3 lines**. A hand-rolled JSON-line logger is ~30 lines and costs nothing. Do not import an instrumentation framework to print structured lines |
| `regex` | Adds a large code segment for three trivial patterns: `${ENV_VAR}` expansion (a scanner), the secret wordlist in `mask.ts` (lowercase substring test), and the loopback host check. Hand-roll all three |
| `once_cell`, `lazy_static` | `std::sync::OnceLock` and `LazyLock` are in std |
| `tower-http` | Nothing needed from it: assets are embedded and served by hand, and there is no compression or CORS layer to add |
| `tokio` `rt-multi-thread` | See ADR-003 |
| `openssl` / `native-tls` | One TLS stack only, and it is `rustls`. A second one is a bug — check `cargo tree -d` in CI |
| `chrono` | `time` with `default-features = false` is smaller and RFC3339 is all that is needed |

## Test-harness deps (`crates/swiss-it`, dev-only)

The docs/44 integration harness behind gate 2 is a leaf nothing depends on, so it has no npm
ancestor to map from. Every dependency below is optional and enters only through the crate's
`it` feature — without it, `cargo test --workspace` builds three empty targets and a machine
without Docker sees no difference at all.

| crate | why it is there |
| --- | --- |
| `testcontainers` 0.27 | starts the real engines for gate 2. `default-features = false`: the defaults pull testcontainers' `ring` TLS for bollard, and the harness speaks plain `tcp://`, `unix://` and `npipe://` docker endpoints only — an `https://` `DOCKER_HOST` is a clean resolution failure, not a second TLS stack |
| `testcontainers-modules` 0.15 | just the three module images (mysql, postgres, redis), again without the default `ring`. Tags live in one table in `src/engine.rs`: mysql:8.4, postgres:17, redis:7 |
| `sqlx` 0.8, `redis` 0.27, `rmcp` 3.2, `reqwest` 0.13, `axum` 0.8, `tokio`, `futures-util`, `serde_json` | the same versions the shipping crates already carry, so the dev graph holds ONE copy of each (`cargo tree -d` stays quiet): L1 seeds databases and drives the product's own browsers, L2 boots the real gateway and speaks rmcp over streamable HTTP |
| `windows-sys` 0.61 (Windows) | Toolhelp32 to count live children by exe name in the L3 proc suite — a direct API call, not a `tasklist` subprocess (the repo rule) |
| `swiss-core`, `swiss-host`, `swiss-mcp` (test-utils), `swiss` | path deps into the product itself: the browsers under test, the launcher-noise scrub the L3 env test replays (docs/16 H1), and `build_app`/`Gateway::boot` for the L2 real listener — the one edge that can never ship, since nothing depends on this crate |

No Docker client crate, deliberately. The two cleaners that may run without a tokio runtime —
the atexit hook and the `it-reaper` watchdog (`src/bin/it-reaper.rs`, this repo's ryuk minus
the container, `78cd561`) — share one hand-rolled blocking `DELETE /containers/{id}?force=1&v=1`
over TCP, unix socket or Windows named pipe in `src/docker_raw.rs`, deliberately unversioned so
dockerd serves its own current API version. hyper and reqwest both need an async runtime the
dead-parent path cannot assume.

Dev graph versus shipping graph, as AGENTS.md states the rule: nothing depends on `swiss-it`, so
`cargo tree -e normal,build` — the graph the duplicate check owns — never sees any of the above,
while the dev graph (testcontainers included) never ships and may carry its own copies. That is
why the hygiene command below is scoped to normal+build edges.

## Draft `Cargo.toml`

Not yet committed as a real manifest — versions get pinned when Phase 0 actually compiles.

```toml
[package]
name = "local-mcp-gateway"
edition = "2024"

[[bin]]
name = "swiss"
path = "src/main.rs"

[dependencies]
rmcp = { version = "3", default-features = false, features = [
    "server", "client", "transport-streamable-http-server",
    "transport-child-process", "schemars",
] }
axum = { version = "0.8", default-features = false, features = ["http1", "json", "query", "matched-path"] }
tokio = { version = "1", default-features = false, features = [
    "rt", "macros", "net", "io-util", "process", "signal", "time", "fs", "sync",
] }
tower = { version = "0.5", default-features = false, features = ["util"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["raw_value", "preserve_order"] }   # preserve wire field order

sqlx = { version = "0.8", default-features = false, features = [
    "runtime-tokio", "tls-rustls-ring", "mysql", "postgres", "json",
] }
redis = { version = "0.27", default-features = false, features = ["tokio-comp", "streams", "acl"] }
reqwest = { version = "0.13", default-features = false, features = ["json"] }
russh = { version = "0.63.2", default-features = false, features = ["ring", "rsa", "des"] }

aes-gcm = "0.10"      # the sealed-envelope format — see docs/05
hkdf = "0.12"
sha2 = "0.10"
rand = "0.8"

encoding_rs = "0.8"   # GBK child stderr on a Chinese Windows console
time = { version = "0.3", default-features = false, features = ["std", "formatting", "parsing", "macros"] }
clap = { version = "4", default-features = false, features = ["std", "derive", "help", "usage", "error-context"] }
anyhow = "1"
thiserror = "2"
rust-embed = { version = "8", features = ["interpolate-folder-path"] }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Security_Cryptography",           # DPAPI, replacing the powershell spawn
    "Win32_System_Diagnostics_ToolHelp",     # process tree, replacing the other powershell spawn
    "Win32_System_JobObjects",               # kill-on-close, replacing tree-kill
    "Win32_System_Threading",
] }

[features]
default = []

[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

## Hygiene to wire into CI from day one

```bash
cargo tree -d -e normal,build # the SHIPPING graph — a duplicated TLS stack or runtime must
                               # fail the build; dev edges (swiss-it's harness among them)
                               # never ship and may carry their own copies (AGENTS.md, docs/44)
cargo tree -e features        # what actually got pulled in
cargo bloat --release --crates    # which crate owns the binary
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo tree -d` earns its place: the usual way a 15 MB target becomes a 30 MB one is a transitive
dependency quietly dragging in `native-tls` beside `rustls`, and nothing else reports it.

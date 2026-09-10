# 03 — Dependency map

## npm → crate

| npm | crate | Notes |
| --- | --- | --- |
| `@modelcontextprotocol/server` `/client` `/node` | **`rmcp` 3.x** | Same 2026-07-28 revision; `legacy_session_mode(false)` is the `legacy: "stateless"` fallback; `TokioChildProcess` replaces `StdioClientTransport` |
| `node:http` + the hand-written `Router` | **`axum`** | The Node build wrote its own router to avoid express's ~9.6 MB. axum has no such problem — that comment does not transfer |
| `mysql2`, `pg` | **`sqlx`** | One crate for both. **Do not use the `query!` macros** — they need a live database at compile time, and this is a generic SQL browser anyway. Runtime `query()` throughout |
| `ioredis` | **`redis`** (redis-rs) | `fred` is the richer client and the heavier one; nothing here needs it |
| `mongodb` | **`mongodb`** | The fattest crate in the set. Behind an off-by-default Cargo feature — ADR-004 |
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

## Draft `Cargo.toml`

Not yet committed as a real manifest — versions get pinned when Phase 0 actually compiles.

```toml
[package]
name = "local-mcp-gateway"
edition = "2024"

[[bin]]
name = "lmg"
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
mongodb = { version = "3", default-features = false, features = ["compat-3-0-0", "rustls-tls"], optional = true }
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
mongo = ["dep:mongodb"]

[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

## Hygiene to wire into CI from day one

```bash
cargo tree -d                 # a duplicated TLS stack or runtime must fail the build
cargo tree -e features        # what actually got pulled in
cargo bloat --release --crates    # which crate owns the binary
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo tree -d` earns its place: the usual way a 15 MB target becomes a 30 MB one is a transitive
dependency quietly dragging in `native-tls` beside `rustls`, and nothing else reports it.

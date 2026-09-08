# 08 — Testing

## The premise

The Node build ships **10,859 lines of vitest across 54 files**, and its `AGENTS.md` requires that
every behaviour change arrive with a test that fails before it and passes after. That suite is the
most valuable artifact in this port: it is a specification of what the gateway does, written by
someone who had already hit the bugs.

**Treat it as the acceptance spec, not as code to translate.** Port each file's *assertions* to
Rust alongside the module it covers. A ported module whose test file has not been ported is not
done — that is the rule from the Node build and it carries over unchanged.

Do not attempt mechanical conversion. Do not redesign the tests either: where a test looks
over-specific, it is usually pinning a bug. Two examples from the git log, both worth reading before
porting the module they cover:

- `bodyGone` must be **absent, not `false`**, while a payload is still readable.
- Forward byte accounting is pinned against a stop that lands mid-transfer.

## Mapping the machinery

| vitest | Rust |
| --- | --- |
| `supertest(app).get(...)` | `app.oneshot(Request::builder()…)` via `tower::ServiceExt` — no real port, no real listen |
| `describe` / `it` | `mod tests` / `#[tokio::test]` |
| `vi.useFakeTimers()` | `tokio::time::pause()` + `advance()` |
| `vi.mock()` of a module | Inject a trait object. Where the Node build mocks a module, the Rust port takes a `&dyn` parameter — this is where the port improves on the original |
| `test/fixtures/` | `tests/fixtures/`, copied verbatim |
| `beforeEach` tmp data dir | `std::env::temp_dir().join(format!("lmg-<tag>-{}", random_hex(8)))` + `MCP_GATEWAY_HOME` — no `tempfile` crate, in the spirit of ADR-007 |

Two environment rules carry over exactly:

- **`MCP_GATEWAY_MASTER_KEY` in every test.** It overrides every OS key source, so the suite never
  spawns a keystore helper (`powershell`, `security`, `secret-tool`) and runs identically in CI.
- **DB tests self-skip without credentials.** `direct-adapters`, `dbbrowser`, `sql`, `db-resources`
  and `mongo-resources` must skip cleanly, not fail, on a machine with no database.

## Test inventory, by phase

### Phase 1 — port these first, then make them pass

| Test file | Lines | Covers |
| --- | --- | --- |
| `local-only.test.ts` | 139 | **The security boundary. Port before any server code.** |
| `auth.test.ts` | 20 | Bearer parsing |
| `token-pick.test.ts` | 29 | Named token selection |
| `secure-store.test.ts` | 208 | **The sealed envelope. Blocking — see docs/05.** |
| `atomic-json.test.ts` | 56 | Write-rename |
| `config.test.ts` | 126 | Load, validation, `${ENV}` |
| `managed.test.ts` | 298 | `managed.json` semantics |
| `bootstrap.test.ts` | 133 | First-run seeding |
| `mask.test.ts` | 98 | Secret masking |
| `router.test.ts` | 200 | Routing, body limit, refuse-before-body |
| `http-refuse.test.ts` | 32 | 401 before the body is read |
| `registry.test.ts` | 241 | Lifecycle, generations, races |
| `paging.test.ts` | 141 | Page cache |
| `mem.test.ts` | 72 | Memory reporting |
| `admin-panel.test.ts` | 197 | Asset serving |
| `factory-registry.test.ts` | 95 | Adapter registration — **adjust for ADR-001** |

Add one file with no Node counterpart: **the golden-response harness** (docs/05 §3), capturing every
`/api/*` response from the Node build and asserting the Rust build matches.

### Phase 2

| Test file | Lines |
| --- | --- |
| `adminapi.test.ts` | 1,028 |
| `dbbrowser.test.ts` | 818 |
| `direct-adapters.test.ts` | 752 |
| `calls.test.ts` | 365 |
| `sql.test.ts` | 334 |
| `db-resources.test.ts` | 273 |
| `tool-server.test.ts` | 213 |
| `mongo-resources.test.ts` | 174 |
| `traffic.test.ts` | 163 |
| `resources.test.ts` | 150 |
| `admin-data-grid.test.ts` | 66 |
| `calls-atomic.test.ts` | 60 |
| `mcp-import.test.ts` | 126 |
| `mcp-test.test.ts` | 171 |
| `shutdown-api.test.ts` | 61 |

`adminapi.test.ts` at 1,028 lines is the single biggest file in the suite. Port it incrementally,
route by route, alongside the routes themselves — not as one push at the end of the phase.

### Phase 3

| Test file | Lines |
| --- | --- |
| `proc.test.ts` | 139 |
| `lazy-proc.test.ts` | 209 |
| `proc-pids.test.ts` | 79 |
| `process-tree.test.ts` | 33 |
| `proxy.test.ts` | 49 |
| `proxy-paging.test.ts` | 141 |
| `pathenv.test.ts` | 28 |

`process-tree.test.ts` needs the most rethinking: it tests the PowerShell walk, and the Rust build
does not have one. Port the *behaviour* (children are found, orphans are reaped, a live neighbour's
children are not touched), not the mechanism.

### Phase 4

| Test file | Lines |
| --- | --- |
| `tunnel-manager.test.ts` | 555 |
| `tunnel-api.test.ts` | 467 |
| `tunnel-store.test.ts` | 278 |
| `tunnel-forward.test.ts` | 238 |
| `tunnel-ssh.test.ts` | 188 |
| `tunnel-import.test.ts` | 93 |
| `tunnel-port.test.ts` | 81 |
| `tunnel-mcpmatch.test.ts` | 51 |
| `tunnels-browse.test.ts` | 42 |
| `http-adapter.test.ts` | 121 |
| `rest-adapter.test.ts` | 257 |
| `rest-template.test.ts` | 106 |

1,993 lines of tunnel tests against 1,907 lines of tunnel source. That ratio is a fair signal of how
much of this subsystem is edge cases, and a good reason to port the tests first.

### Phase 5

| Test file | Lines |
| --- | --- |
| `cli.test.ts` | 346 |
| `daemon.test.ts` | 280 |
| `pidfile.test.ts` | 119 |
| `skill-install.test.ts` | 107 |

## Where the port actually stands

The phase tables above are the plan the port was written against. This is the result, so the
remaining gap is visible without re-deriving it:

```
cargo test                     564 lib + 65 adminapi + 12 app + 4 envelope_compat + 1 memory
cargo test --features mongo    582 lib  (the extra 18 are the mongo adapter's)
```

On a unix host add 6 more: `platform/unix.rs` compiles only there.

Every module in `src/` carries an inline `#[cfg(test)] mod tests` **except** these:

| Module | Lines | Why not, and what it would take |
| --- | --- | --- |
| `adapters/pg_browser.rs`, `mysql_browser.rs`, `redis_browser.rs` | 1,056 | Every path needs a live server. They belong with the self-skipping DB tests, not with the unit suite. |
| `platform/windows.rs` | 378 | Win32 FFI: DPAPI, Toolhelp, registry. Its process walk is covered — the BFS both platforms share now lives un-`cfg`'d in `platform/mod.rs` and is tested on whatever host runs the suite. What is left is the FFI itself, which needs the OS to answer. |
| `server.rs`, `admin.rs` | 448 | Wiring and asset serving. Reachable through `tests/app.rs`; no direct tests. |
| `adminapi.rs`, `app.rs` | 2,028 | No *inline* tests by design — covered end-to-end from `tests/adminapi.rs` (65) and `tests/app.rs` (12), which is where a route contract belongs. |
| `lib.rs`, `main.rs`, `secure/mod.rs`, `tunnel/mod.rs` | 109 | Re-export shells with no behaviour of their own. |

The two platform halves must export the same names with the same signatures: `mod.rs` re-exports
both from one list, so a divergence is a build error. They did diverge once — `descendant_pids`
returned a bare `Vec` on unix and an `Option` on Windows, and the unix build simply did not
compile. Nothing caught it because CI had never run. It runs clippy on both feature sets now.

One branch is deliberately uncovered: `daemon::StopResult::Forced`. Reaching it means handing
`tree_kill` a live pid, and the only live pid a test could produce is the test runner's own. The
module header says so at the top of its `mod tests`.

Still open from the wish list below: the golden `/api/*` capture. The envelope round-trip against
Node-sealed fixtures is `tests/envelope_compat.rs`, and the RSS guard is `tests/memory.rs`.

## Tests worth adding that the Node build could not have

- **RSS regression test.** ✅ `tests/memory.rs`. It asserts a DELTA, not a ceiling: the number
  readable from inside `cargo test` is the test binary's working set, which carries the harness,
  an rmcp client and every dev-dependency, so an absolute figure there would be measuring the
  wrong process. Growth under load needs no baseline, and growth is the actual regression shape —
  a leak, an unbounded buffer, a payload-proportional allocation on a forwarding path. It has its
  own file so cargo gives it its own process, and one test so the phases cannot measure each
  other. Current margins: 1,500 trivial requests +0.0 MB, 1,500 tool calls +0.2 MB, and 120 MB
  pushed through the adapter +0.4 MB — the last of which is what pins AGENTS.md's "no
  `serde_json::Value` on a forwarding path".
- **`cargo tree -d` in CI.** ✅ In `build.yml`, but not as written here: `cargo tree -d` exits 0
  whatever it finds, so it gates nothing on its own, and the tree already carries a spread of
  RustCrypto versions because russh is a generation ahead of our aes-gcm. The step prints the full
  picture and fails only on a second tokio, TLS stack or hyper — which is what the megabytes
  actually ride on.
- **Envelope round-trip against Node-sealed fixtures.** Check in a fixture sealed by the Node build
  and assert Rust opens it. This catches HKDF argument-order and base64 mistakes at the exact moment
  they are introduced rather than on a user's machine.
- **Golden `/api/*` responses.** The panel is not adapted to the port, so response shape is a
  contract — see ADR-009.

## CI

```bash
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features mongo -- -D warnings
cargo test
cargo test --features mongo
cargo tree -d
cargo build --release --features mongo   # the shipping binary (ADR-004); record its size
```

Mirror the Node build's rule: **`cargo clippy` and `cargo test` must both be green before a change
is considered done.**

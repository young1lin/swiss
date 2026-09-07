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
| `beforeEach` tmp data dir | `tempfile::TempDir` + `MCP_GATEWAY_HOME` |

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

## Tests worth adding that the Node build could not have

- **RSS regression test.** Boot with a fixed config, serve a scripted workload, assert RSS stays
  under a ceiling. Cheap in Rust, impossible to make stable in Node. This is the one number the
  project exists for — guard it.
- **`cargo tree -d` in CI.** Fail the build on a duplicated TLS stack or async runtime. This is how
  a 15 MB target quietly becomes 30 MB.
- **Envelope round-trip against Node-sealed fixtures.** Check in a fixture sealed by the Node build
  and assert Rust opens it. This catches HKDF argument-order and base64 mistakes at the exact moment
  they are introduced rather than on a user's machine.
- **Golden `/api/*` responses.** The panel is not adapted to the port, so response shape is a
  contract — see ADR-009.

## CI

```bash
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --features mongo
cargo tree -d
cargo build --release          # and record the binary size
```

Mirror the Node build's rule: **`cargo clippy` and `cargo test` must both be green before a change
is considered done.**

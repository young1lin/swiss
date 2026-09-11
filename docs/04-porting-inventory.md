# 04 — Porting inventory

Every backend source file in the Node build, with its destination, phase and risk. Line counts are
from `../local-mcp-gateway/src` on 2026-09-07. **15,395 lines of backend TypeScript** is the real
size of this job — the other 7,193 lines of that tree are the panel, which is copied, not ported.

> **The port is complete; the destinations below are pre-workspace paths.** Every `src/…`
> destination in this table names the single-crate layout the port was planned in. The code has
> since been split into eight crates (`4147e8a`) — `src/adapters/x.rs` is now
> `crates/swiss-mcp/src/adapters/x.rs`, `src/secure/` is `crates/swiss-core/src/secure/`, and so on.
> The current map is in `docs/02-architecture.md`; this table is kept for what it actually
> records — which Node module became which Rust module, and what each one cost.

Risk is about *unknowns*, not size: a 600-line SQL adapter is long but mechanical; a 137-line
process-tree walk is short and full of platform traps.

## Phase 1 — Skeleton: boot, serve the panel, serve `echo`

| File | Lines | Destination | Risk | Note |
| --- | --- | --- | --- | --- |
| `log.ts` | 3 | `log.rs` | Low | Hand-rolled JSON lines. No `tracing` |
| `datadir.ts` | 22 | `paths.rs` | Low | |
| `port.ts` | 20 | `paths.rs` | Low | |
| `privfs.ts` | 23 | `platform/` | Low | 0600 / Windows ACL on state files |
| `introspect.ts` | 15 | `admin/mod.rs` | Low | |
| `auth.ts` | 22 | `auth.rs` | Low | Bearer parsing |
| `local-only.ts` | 91 | `local_only.rs` | Med | Security boundary. Port its 139-line test file first, then make it pass |
| `atomic-json.ts` | 68 | `atomic_json.rs` | Low | Write-rename; a torn config is unbootable |
| `secure/envelope.ts` | 78 | `secure/envelope.rs` | **High** | AES-256-GCM + HKDF. Frozen format — see docs/05 |
| `secure/key.ts` | 234 | `secure/key.rs` + `platform/` | **High** | DPAPI / Keychain / secret-tool / machine-id. Must open blobs Node sealed |
| `secure/statefile.ts` | 62 | `secure/statefile.rs` | Med | |
| `secure/envstore.ts` | 107 | `secure/envstore.rs` | Med | Sealed replacement for `.env`; injects into the process env |
| `config.ts` | 156 | `config.rs` | Med | `${ENV}` expansion stays build-time, not load-time — the credential model rests on it |
| `mask.ts` | 154 | `mask.rs` | Low | Shared secret wordlist. Hand-rolled matcher, no `regex` |
| `managed.ts` | 451 | `managed.rs` | Med | `managed.json`: user MCPs, overrides, toggles, enabled flags |
| `bootstrap.ts` | 162 | `bootstrap.rs` | Med | First-run seeding; must be a no-op on an existing data dir |
| `token.ts` | 128 | `token.rs` | Low | Named per-client tokens |
| `paging.ts` | 137 | `paging.rs` | Low | |
| `http.ts` | 240 | *(deleted)* | — | axum replaces it. `BODY_LIMIT` (2 MB) and the refuse-before-body rule survive as middleware |
| `router.ts` | 253 | `app.rs` | **High** | The per-generation service cache and the evictor are load-bearing — see docs/02 |
| `index.ts` | 194 | `lib.rs` | Med | Boot order: PATH fix, first-run, reap, tunnels, MCPs, listen |
| `admin.ts` | 53 | `admin/mod.rs` | Low | `rust-embed`, served from `&'static [u8]` |
| `adapters/types.ts` | 43 | `adapters/mod.rs` | Low | The trait |
| `adapters/factory.ts` | 205 | `adapters/factory.rs` | Med | Minus `ExternalAdapter` — **ADR-001** |
| `adapters/echo.ts` | 58 | `adapters/echo.rs` | Low | First real rmcp server; the Phase 0 spike grows into this |
| `registry.ts` | 473 | `registry.rs` | **High** | Op queue, `gen` counter, tombstones, lazy wake, idle reap |
| `mem.ts` | 131 | `mem.rs` + `platform/` | Med | Toolhelp32 instead of PowerShell; measurement becomes free |
| `adminapi.ts` (part) | ~350 of 914 | `admin/api.rs` | Med | `/api/info`, `/api/mcps`, `/api/memory`, `/api/tokens`, lifecycle routes |

**Exit criterion:** the Rust binary boots on the real data dir, the panel loads and is fully
navigable, `echo` answers a real MCP client, and `/api/memory` reports a number worth writing into
docs/01.

## Phase 2 — The direct adapters

| File | Lines | Destination | Risk | Note |
| --- | --- | --- | --- | --- |
| `adapters/direct.ts` | 194 | `adapters/direct.rs` | Med | Shared lifecycle for the in-process drivers |
| `adapters/tool-server.ts` | 299 | `adapters/tool_server.rs` | Med | Where tool toggles and the call log meet |
| `adapters/resources.ts` | 201 | `adapters/resources.rs` | Med | |
| `adapters/sql.ts` | 254 | `adapters/sql.rs` | Med | Result rendering, the 256 KB cap |
| `adapters/mysql.ts` | 591 | `adapters/mysql.rs` | Med | Long, mechanical |
| `adapters/mysql-resources.ts` | 184 | same | Low | |
| `adapters/pg.ts` | 529 | `adapters/pg.rs` | Med | |
| `adapters/pg-resources.ts` | 190 | same | Low | |
| `adapters/redis.ts` | 628 | `adapters/redis.rs` | Med | The command allowlist is a security surface — port it exactly |
| `adapters/redis-resources.ts` | 149 | same | Low | |
| `calls.ts` | 682 | `calls.rs` | **High** | Two-layer on-disk log; its retention arithmetic already cost one fix (see the Node git log) |
| `traffic.ts` | 520 | `traffic.rs` | Med | Ring becomes disk-backed while porting: **ADR-005** |
| `adminapi.ts` (rest) | ~560 | `admin/api.rs` | Med | |
| `dbbrowser.ts` | 755 | `admin/dbbrowser.rs` | Med | Biggest single file after adminapi. Mechanical but wide |
| `dbbrowser-api.ts` | 349 | `admin/dbbrowser.rs` | Med | |
| `mcp-import.ts` | 150 | `mcp_import.rs` | Low | |

**Exit criterion:** mysql, pg and both redis MCPs serve a real client; the Data view browses tables,
runs SQL and exports CSV identically. First honest RSS comparison against the Node build.

## Phase 3 — `proc`, and the process lifecycle around it

| File | Lines | Destination | Risk | Note |
| --- | --- | --- | --- | --- |
| `adapters/proc.ts` | 233 | `adapters/proc.rs` | **High** | `TokioChildProcess`; keep both timeouts (60 s handshake, 180 s call) and the GBK stderr decode |
| `adapters/proxy.ts` | 129 | `adapters/proxy.rs` | **High** | Child-to-client proxying; the `&RawValue` passthrough lives here |
| `adapters/proxy-fetch.ts` | 43 | same | Low | |
| `process-tree.ts` | 137 | `platform/windows.rs` | **High** | Job Objects with kill-on-close replace tree-kill — strictly better, and it deletes a whole orphan class |
| `proc-pids.ts` | 96 | `proc_pids.rs` | Med | The port-scoped ledger. Keep it: the job object covers crashes, the ledger covers hard kills |
| `pathenv.ts` | 30 | `pathenv.rs` | Low | Restores `~/.local/bin` so `uvx` resolves |

**Exit criterion:** a `uvx`-launched MCP wakes lazily on first request, is reaped after idling, and
leaves no orphan when the gateway is killed with Task Manager's End Task.

## Phase 4 — Tunnels and the remaining adapters

| File | Lines | Destination | Risk | Note |
| --- | --- | --- | --- | --- |
| `tunnels/types.ts` | 118 | `tunnels/mod.rs` | Low | |
| `tunnels/store.ts` | 384 | `tunnels/store.rs` | Med | |
| `tunnels/port.ts` | 114 | `tunnels/port.rs` | Low | |
| `tunnels/ssh.ts` | 289 | `tunnels/ssh.rs` | **High** | russh; trust-on-first-use host keys must behave identically |
| `tunnels/forward.ts` | 180 | `tunnels/forward.rs` | **High** | Byte accounting is pinned by a test — see the Node git log |
| `tunnels/manager.ts` | 644 | `tunnels/manager.rs` | **High** | "A local port is bound only while its tunnel can carry traffic" is the rule the whole view rests on |
| `tunnels/mcpmatch.ts` | 83 | `tunnels/mcpmatch.rs` | Low | |
| `tunnels/import.ts` | 95 | `tunnels/import.rs` | Low | One-time forward-port adoption |
| `tunnels/api.ts` | 384 | `tunnels/api.rs` | Med | |
| `adapters/http.ts` | 91 | `adapters/http.rs` | Low | reqwest; keep the proxy support undici was there for |
| `adapters/rest.ts` | 128 | `adapters/rest.rs` | Low | |
| `adapters/rest-template.ts` | 122 | `adapters/rest.rs` | Low | |

**Exit criterion:** feature parity. Every panel view works against the Rust binary.

## Phase 5 — The `swiss` command and shipping

| File | Lines | Destination | Risk | Note |
| --- | --- | --- | --- | --- |
| `bin.ts` | 12 | `main.rs` | Low | |
| `cli.ts` | 533 | `cli.rs` | Med | clap derive; keep every subcommand name and flag |
| `daemon.ts` | 470 | `daemon.rs` | **High** | Detached spawn on Windows; the V8 flags disappear; graceful-then-force stop stays |
| `pidfile.ts` | 114 | `pidfile.rs` | Med | `gateway-<port>.pid`, and the refuse-if-unsure rule |
| `skilldir.ts` | 10 | `skill.rs` | Low | |
| `skill-install.ts` | 63 | `skill.rs` | Low | Ship `.agents/skills` beside the exe |
| `types.d.ts` | 3 | *(deleted)* | — | |

**Exit criterion:** `swiss start` / `stop` / `status` behave identically, and a single `.exe` runs on a
machine with no Node installed.

## Not ported — carried across

| What | Size | Handling |
| --- | --- | --- |
| `src/admin/` (28 JS, 2 CSS, 1 HTML) | 7,193 lines | Copied verbatim, embedded, never edited here. It is the spec for the admin API — see AGENTS.md |
| `test/` (54 files) | 10,859 lines | The acceptance spec, ported alongside each module — docs/08 |
| `.agents/skills/` | — | Shipped beside the binary |
| `gateway.config.example.json` | — | Copied |

## Totals by phase

| Phase | Backend lines | Share |
| --- | --- | --- |
| 1 — Skeleton | ~3,300 | 21% |
| 2 — Direct adapters | ~5,900 | 38% |
| 3 — proc | ~630 | 4% |
| 4 — Tunnels + remaining adapters | ~3,400 | 22% |
| 5 — CLI + shipping | ~1,200 | 8% |
| Unassigned / glue | ~965 | 6% |

Phase 2 is the bulk of the typing but the least of the risk. Phases 3 and 4 are 26% of the lines and
most of the ways this can go wrong.

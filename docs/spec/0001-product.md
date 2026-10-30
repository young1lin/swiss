# §product — What swiss is

Chapter 0001 of the [swiss specification](../SPEC.md).

swiss is a developer's pocket multitool: **one local process that holds every small tool a
developer reaches for while writing code** — MCP servers, database browsing, SSH tunnels,
scheduled jobs, terminals, remote execution, and whatever the next one turns out to be —
behind one loopback port (`127.0.0.1:19999` by default) and one panel. It ships as one
self-contained binary.

Memory is why it is Rust: the same program's practical floor on Node was 45–60 MB, and the
Node build it replaced is retired (ADR-016). This repository owns every layer, the panel
included. When a piece of behaviour looks odd, the module's comment
usually records the bug that was paid for once already.

## §product.properties — The four properties

A change that trades any of these away for convenience is the wrong change, however much
shorter it makes the diff.

- **Ruthlessly small.** Memory is the product, not an optimisation pass. Idle cost is the number
  that matters; a tool nobody is using right now should cost close to nothing.
- **Plugin-shaped.** Every capability is a plugin with its own descriptor, routes, pages and
  resources. The host keeps only cross-cutting mechanism — the security boundary, config,
  registration, lifecycle, run accounting. No business logic climbs back up into it (§host.plugins).
- **Hot-pluggable.** Enabled, running and visible are three different states, switchable at
  runtime. Disabling a plugin really releases what it held — tasks, connections, children,
  routes — and nothing else in the process notices.
- **Three ways to bring a tool in.** A stdio child process, an HTTP endpoint, or Rust compiled
  into the binary. The first two cost a process or a socket and are how foreign tools arrive;
  the third costs almost nothing and is how the tools worth keeping end up shipping.

MCP is the most important plugin, not the trunk everything hangs off. Data, Tunnels, Jobs,
Terminal, Remote and Process are its peers, and a new tool reaches the panel by contributing a
descriptor, an action and a page — never by editing a match arm in the host.

## §product.memory — The memory record

Memory numbers are **records, not gates**: no band is enforced, and a debug build costing more
is fine. What stays load-bearing is the direction — every megabyte of idle cost has to be
argued for, and a change that moves the number the wrong way is worth stopping for.

The instrument is the gateway's own `GET /api/memory` (`gatewayMb`, the process tree's
`childrenMb`, `processCount`). Measure a change by running it on 19998 against the same data
and traffic as a baseline, and add a row here; the `swiss-memory-record` skill does the
measuring.

| Date | Build | Workload | Working set | Note |
|---|---|---|---|---|
| 2026-09-07 | Node | mysql, pg, 2×redis, 2×http, echo; 2×proc asleep | 117.5 MB | the baseline |
| 2026-09-07 | Rust spike | echo only | 9.1 MB | axum + rmcp stateless, 1.05 MB exe |
| 2026-09-10 | Rust | echo + panel | 14.0 MB | whole gateway present, scratch home |
| 2026-09-10 | Rust + terminal | plugin loaded, zero sessions | 13.7 MB | flat; +233 KB per idle local session |
| 2026-09-11 | Rust | mysql, pg, 2×redis over its own tunnels | 21.8 MB | 40/40 calls ok, private 8.2 MB |
| 2026-09-11 | Rust | full workload, 60 calls, 9 tunnel rules | **22.4 MB** | private **8.6 MB**, 6 threads |
| 2026-09-11 | Node | same set, same data dir, same traffic | 113.8 MB | private 124.9 MB, 13 threads |
| 2026-09-13 | Rust | 100k-row pg table exported | peak 32.5 MB | `format=sql` streams; csv materialises (§data.export) |
| 2026-09-15 | Rust + MCP OAuth | full workload, a grant stored | 19.9 MB | the OAuth module costs nothing idle |
| 2026-09-15 | Rust + native zai-vision | full workload | 21.8 MB | the retired Node child measured 66.6 MB alive (ADR-022) |
| 2026-09-22 | Rust | real-state snapshot, nothing awake | 27.0 MB | cold boot, zero traffic; private 14.0 MB |
| 2026-10-04 | Rust 83b8977-dirty | real-state snapshot on 19998, memory plugin active | 15.4 MB | private ~3.2 MB; children 0 MB (1 proc idle); the SQLite memory store costs nothing measurable idle (§memory) |
| 2026-10-09 | Rust b0c89ba (module-owned log storage, D0-D7) | real-state snapshot on 19998, mcp.db holding recorded bodies after the browser walk | 28.6 MB | private ~14.1 MB; children 0 MB (1 idle proc). The +1.6 MB working set over 2026-09-22 is the module SQLite stores open with page caches over recorded bodies (§host.history), not a private-heap move: +0.1 MB. |
| 2026-10-09 | Rust ed82dc6 (terminal store, ADR-035) | real-state snapshot on 19998, terminal.db holding 106 imported recordings (5.4 MB physical) | 32.2 MB | private 17.4 MB, children 0. The +3.6/+3.3 MB over b0c89ba is the third store's pool and page cache over the imported casts — proportional to data admitted, not a per-session cost; the sweep and quota bound it. |
| 2026-10-09 | Rust Process refactor (`1d96f5b-dirty`) | real-state snapshot on 19998; native mysql/pg/redis loaded, proc asleep, tunnel ports already occupied, no owned children | 27.5 MB | private 13.4 MB; saved earlier `a3e916f-dirty` release on the same snapshot measured 27.5 / 13.4 MB too. Working set +0.5 MB and private -0.6 MB vs the dated 2026-09-22 record; not a new dependency or resident watcher. |
| 2026-10-09 | Rust Process refactor (`1d96f5b-dirty`) | user-selected private `.swiss-process` home on 19994; six demo app definitions, panel open, default echo/remote built-ins only; no external MCP/tunnels or owned children | 16.1 MB | private 5.4 MB; minimal-state reading, not comparable to the real-state rows. Detached demo helpers are outside gateway/child accounting. |
| 2026-10-10 | Rust `128b27a` (saved test release) | 19998 startup sample: production sealed-config snapshot, pre-existing test history databases, before panel traffic; no owned children | 27.0 MB | private 12.1 MB; children 0 MB. +10.9/+6.7 MB versus the preceding minimal-state row is a different workload, not a regression delta. Health/version hashes matched. Startup sample, not a confirmed settled-idle baseline; no optimization applied. |
| 2026-10-10 | Rust `a629563` (production) | existing 19999 after ordinary use; OS-only memory sampling, no restart or iteration traffic; 41 post-boot traffic rows | 30.1–31.8 MB | private commit ~21.1 MB. Read-only page classification: image resident 22.4 MB (exe 10.4, DLLs 12.0 including Bitdefender 1.4), private resident 7.2 MB, mapped resident 0.4 MB. Persisted post-boot traffic body/response/params total 34,472 B, not a multi-MB ring explanation. Versus the preceding test startup sample, +3.1–4.8 MB working set / +9.0 MB private is not a controlled delta: different histories and uptime. No memory optimization applied. |
| 2026-10-10 | Rust `d42a6fe` (production, hash from GET /health) | existing 19999 after ordinary use, started 16:26 +08:00; read-only OS page audit, no restart or workload control | 29.95 MB | private commit 19.64 MB. Image resident 21.85 MB (exe 10.13, DLLs 11.72 including Bitdefender 1.35), private-region resident 7.64 MB, mapped resident 0.43 MB; page sum 29.93 MB. Versus the preceding sample, working set -1.8 to -0.1 MB / private -1.5 MB is not an optimization delta: different binary and uptime. SQLite cache/statement allocations on the live connections have NOT been measured; the default cache budget is not actual usage. No memory optimization applied by this audit. |
| 2026-10-13 | Rust `3f9a487`-dirty, exe SHA `E24EE1F…B4AA29` (controlled snapshot launch, guarded inherited parent console) | frozen real-state snapshot, memory plugin active, MCP 11 (idle 1 / started 10), no owned children (RAW 0); strict idle proof TRUE in this launch context | 28.4 MB | private 12.4 MB (both API heap fields map private bytes — [mem.rs](<../../crates/swiss-host/src/mem.rs#L73-L84>); API-rounded). Context-only record, NOT a production ordinary-use delta vs the 29.95 MB row; no band gate. |
| 2026-10-15 | Rust `bbe8fc2`-dirty (llm run context + recheck looks) | real-state snapshot on 19998, idle read after boot; every proc MCP lazy, no owned children | 21.4 MB | private ~6 MB; children 0 MB (1 proc). Not comparable 1:1 with the 10-13 row (that launch kept 10 proc MCPs started): the llm/memory change adds no idle cost and no child. |
| 2026-10-15 | Rust `2f14017-dirty` (Memory lifetime + proxy validation), exe SHA `C8C8E2D0…D8E5609D` | real-state snapshot on 19998; startup read after lifecycle verification and a restart, no owned children | 21.1 MB | private 5.8 MB; children 0 MB (1 proc). Working set -0.3 MB / private about -0.2 MB vs the preceding row; startup timing and retained test-history databases differ, so this is not a controlled optimization delta. |
| 2026-10-15 | Rust `1105c3c-dirty` (review fixes 1-12: quota handoff, terminal orphan adoption, git hardening, port owners, sheet/restore guards) | real-state snapshot on 19998, idle read after the panel proof-of-life walk; proc MCPs lazy, no owned children | 21.4 MB | private ~5.9/5.9 MB; children 0 MB (1 proc). Working set +0.3 MB vs the preceding row; the test home carries the walk's knowledge database, so this is not a controlled optimization delta. |
| 2026-10-15 | Rust `1105c3c-dirty` (physical quota, migration locking, recovery fence, owner/restore guards, stacked-sheet focus), exe SHA `39328937…C9D40BD6` | real-state snapshot on 19998; startup read after a recording-owner crash/restart, proc MCPs lazy, no owned children | 21.3 MB | private 5.9 MB; children 0 MB (1 proc). Working set -0.1 MB / private unchanged vs the preceding row. The test home retains browser fixtures and startup timing differs; this is not a controlled optimization delta. |
| 2026-10-23 | Rust `aedef733`-dirty (panel embedded gzip, bodies borrowed from the binary, assets `no-cache` + ETag), exe 18.78 MiB against 23.49 | controlled A/B against the deployed `aedef733` exe: the same empty scratch home on 19997, then every panel asset (150) fetched three times with gzip accepted | 21.99 MB after the panel load (baseline 26.83; peak 21.99 against 29.57) | idle 20.06 MB / private 7.98 MB, the baseline's 20.09 / 7.96 - no idle cost. After the load private 7.93 against 8.61 MB. The working-set drop is the embedded panel paged in: 2.0 MB of gzip instead of 7 MB raw; the private drop is the per-request copies the allocator kept. A full load is 1.90 MB on the wire (6.63); a revalidating reload is 150 bodiless 304s. One run each; a minimal-state reading, not comparable to the real-state rows. |
| 2026-10-25 | Rust `5ae9ad94`-dirty (database MCPs connect on demand and release after 10 idle minutes, the scheduler sleeps toward its next entry, the history ceiling enforced) | controlled A/B against the `5ae9ad94` release: the same real-state snapshot on 19998 (mysql, pg, three redis), ~400 s with no traffic each, then `/api/mcps` read every 10 s for 2 min and left alone | 27.46–27.83 MB idle (baseline 29.02–29.27) | private 11.71–12.16 MB against 11.91–12.32. Resident database connections 0 against 5; gateway CPU over the idle window 31 ms against 171 ms. Watching brought the 5 connections back (the first read 1.05 s while the pools were built, the rest from the 1 s watch window); 10 minutes after the last read all were gone again, with threads 7 to 3, working set 27.38 and private 10.89 MB, and CPU flat from then on. One run each. |
| 2026-10-26 | Rust `b77e147b`-dirty (conversation layout and the Agent Test’s full tool catalog) | real-state snapshot on 19998; about 30 s after boot, before opening the panel; no proc child awake | 27.7 MB | private 11.8 MB; children 0 MB (gateway only). Within -0.13 to +0.24 MB of the previous 27.46–27.83 MB record; a startup observation, not a controlled optimization comparison with its longer idle window. |
| 2026-10-26 | Rust `b77e147b`-dirty (docked composer, interactive Bash jobs and full-inbox Auto-update) | real-state snapshot on 19998; about 20 s after boot, before opening the panel; no command or proc child awake | 27.7 MB | private 11.9 MB; children 0 MB (gateway only). Working set unchanged and private +0.1 MB from the preceding startup observation; no idle shell or background polling worker. |

The hashes in the rows from 2026-10-09 to 2026-10-15 name commits from before the
memory-plugin history was squashed into one commit per feature (2026-10-22); the tag
`archive/memory-plugin-pre-squash` keeps them.

Private bytes are the honest "owned memory" figure; the working set carries paged-in image
sections. The regression guard inside the test suite is a **delta**, not a ceiling
(§testing.memory).

## §product.nongoals — What this does not buy

1. **`proc` MCPs are untouched.** An `npx`/`uvx` child is 50–150 MB of Node or Python, and
   nothing here changes that. The saving holds only while children are asleep, which is why a
   `proc` MCP is lazy by default (§mcp.proc).
2. **Nothing gets faster.** The gateway is idle almost all the time; latency is the databases'
   and children's. Do not sell this as a performance tool.

Two things come free and are real: no PowerShell spawns (the process-tree walk and DPAPI are
direct Win32 calls, so the memory view is always current), and no `npx` wrapper holding
`cmd.exe` + npx resident for the life of the process.

## §product.tactics — Where the memory goes, and the levers

In descending order of payoff:

1. **No `serde_json::Value` on a forwarding path.** The proxying adapters parse only the
   envelope (`&RawValue` for `params`/`id`) and pass the rest through untouched. A body at the
   2 MiB limit then costs one buffer instead of hundreds of thousands of allocations
   (§arch.rules).
2. **`current_thread` runtime** (ADR-003).
3. **Pools are the per-connection cost.** Database pools are built lazily — by the first call,
   never by boot or a health probe nobody reads (§mcp.registry) — kept tiny
   (`min_connections(0)`, a small max, idle timeout) and let go after ten unused minutes
   (§mcp.adapters); what must stay lazy is pool construction, not code loading — code that
   never executes is never paged in.
4. **The release profile.** `opt-level = "z"`, fat LTO, one codegen unit, `panic = "abort"`,
   stripped symbols. The text segment is resident memory; `panic = "abort"` is also why
   `.unwrap()` on fallible paths is banned.
5. **Allocator.** Windows' system allocator is fine. On glibc, measure `MALLOC_ARENA_MAX=2` or
   mimalloc before choosing.
6. **On-disk logs** (ADR-005, §mcp.logs).
7. **Small strings in long-lived structures** (`Arc<str>`/`Box<str>`), last.

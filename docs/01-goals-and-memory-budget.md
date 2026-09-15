# 01 — Goals and the memory budget

## The measurement this project starts from

Taken from the running Node gateway on 2026-09-07 via its own `/api/memory`, with 1×mysql, 1×pg,
2×redis, 2×http and 1×echo started, and 2×proc idle (asleep, costing nothing):

```json
{"gatewayMb":117.5,"heapUsedMb":46.8,"heapTotalMb":52.3,"externalMb":22.3,
 "processCount":1,"childrenMb":0}
```

Where those 117.5 MB go:

| Slice | ~MB | What it is |
| --- | --- | --- |
| V8 / Node floor | ~45 | Interpreter, ICU, startup snapshot, the loaded module graph |
| JS heap (`heapTotal`) | 52.3 | Driver libraries (`mysql2`, `ioredis`, `pg`, `mongodb`, `undici`, `zod`) plus the MCP SDK and gateway objects |
| External | 22.3 | `Buffer`s, undici connection pools, TLS state |

The Node build already applies `--max-semi-space-size=2 --max-old-space-size=256` (measured worth
~15 MB on this workload). That is close to the end of what tuning can do: **Node's practical floor
for this program is 45–60 MB.** Getting under it is the entire reason this repository exists.

## Target

As small as the same adapter set allows, in a single self-contained `.exe`. The original
working figure was ~12–20 MB RSS; per the user's call (2026-09-11) memory numbers are **records,
not gates** — no band is enforced anywhere, and a debug build costing more is fine. What stays
load-bearing is the direction: every MB of idle cost has to be argued for.

The estimate, built up rather than guessed:

| Component | ~MB |
| --- | --- |
| axum/hyper + tokio (`current_thread`) baseline | 4–6 |
| `sqlx` MySQL + Postgres, pools at `min_connections(0) max_connections(2)` | 2–4 |
| `redis-rs`, two connections | ~1 |
| `rustls` + roots (shared by `reqwest` and `sqlx`) | 2–3 |
| Embedded panel assets, served from `&'static [u8]` | ~0.3 |
| Registry, traffic, call-log working set | 1–2 |
| **Total** | **~11–17** |

## Measured result (2026-09-11, current build side by side)

The pre-split monolith's acceptance below (2026-09-07) read 20.3 MB. The split binary, with the
terminal plugin shipped and MongoDB deleted (ADR-012, exe 10,649,600 → 7,948,288 B), re-measured
the same workload family this morning against a fresh Node run on the same data directory:

| | Node | Rust (`3c3fd7f`) | |
| --- | --- | --- | --- |
| Gateway RSS | 113.8 MB | **22.4 MB** | −80%, ~91 MB back |
| Private bytes | 124.9 MB | **8.6 MB** | −93% |
| Threads | 13 | **6** | current_thread runtime |
| Binary | node_modules + runtime | **7.9 MB** single `.exe` | self-contained |

Private bytes are the honest "owned memory" story — the working set carries ~14 MB of paged-in
image and section pages. The ~8 MB over the no-DB Phase 1 reading is the named slice: sqlx mysql
+ pg pools, 2×redis connections, and two `russh` sessions carrying 9 forwarding rules (the DBs
themselves all arrive through those tunnels on this machine). Numbers are records, not gates.

## Measured result (2026-09-07, the pre-split monolith)

Same workload as the Node measurement above — 1×mysql, 1×pg, 2×redis, 2×http and 1×echo started,
2×proc idle, plus the SSH-tunnel subsystem running its rules — read from the finished port's own
`/api/memory` on port 19998:

```json
{"gatewayMb":20.3,"processCount":1,"childrenMb":0}
```

| | Node | Rust | |
| --- | --- | --- | --- |
| Gateway RSS | 117.5 MB | **20.3 MB** | −83%, ~97 MB back |
| Binary | node_modules + runtime | **6.4 MB** single `.exe` | self-contained |
| Tests | — | 224 (lib) | green |

20.3 MB, and the gap to the estimate is accounted for: the
estimate table above predates the tunnel subsystem, whose `russh` sessions (key exchange state,
per-rule buffers) are the slice the estimate did not name. The naive-RSS caveat in
`docs/08-testing.md` applies to both columns equally.

## What this does NOT buy — read before celebrating

1. **`proc` MCPs are untouched.** An `npx`/`uvx` child is 50–150 MB of Node or Python and this port
   does not change one byte of that. On the workload measured above both `proc` MCPs were asleep,
   which is why `childrenMb` is 0. **The ceiling on this project's saving is ~100 MB, and only
   while the children are asleep.** The lazy-start + idle-reap design in the Node build already
   captured the larger prize; this is the second-largest one.
2. **The ~15 MB from the V8 flags disappears with V8.** It is already inside the 117.5 MB figure —
   do not count it twice as a saving.
3. **Nothing gets faster.** The gateway is idle almost all the time and its latency is dominated by
   the databases and children at the other end. Do not sell this as a performance port.

Two things do come free with the move, and both are real:

- **The PowerShell spawns go away.** `mem.ts` walks the process tree with `powershell.exe`
  (~65 MB working set, ~350 ms) and `secure/key.ts` shells out again for DPAPI. In Rust both are
  direct Win32 calls. This removes the transient peaks *and* lets the memory view stop being
  opt-in — it can go back to being free and always current.
- **The npx wrapper stops being a foot-gun.** The Node README has to warn that
  `npx local-mcp-gateway` keeps ~103 MB of `cmd.exe` + npx resident for the life of the process.
  A single exe has nothing to warn about.

## The tactics, in descending order of payoff

### 1. No `serde_json::Value` on a forwarding path

The `proc`, `http` and `rest` adapters are proxies. The Node build parses every payload into a JS
object and re-serialises it. In Rust, parse only the envelope and pass the rest through untouched:

```rust
#[derive(Deserialize)]
struct Envelope<'a> {
    method: &'a str,
    // Never materialized into a DOM: the body is forwarded as the bytes that arrived.
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    #[serde(borrow)]
    id: Option<&'a RawValue>,
}
```

A body at the 2 MB `BODY_LIMIT` then costs one buffer instead of several hundred thousand heap
allocations. This is the largest single runtime lever in the whole port.

### 2. `current_thread` runtime

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> { … }
```

A local gateway has no use for work stealing. Each worker thread costs a stack plus a per-thread
allocator cache, and the multi-thread runtime forces `Send + Sync` on state that would otherwise be
a plain `Rc`/`RefCell`. Single-threaded is smaller *and* simpler here.

### 3. Connection pools are the real per-MCP cost

`sqlx`'s default is `max_connections(10)`. For this workload:

```rust
PoolOptions::new()
    .min_connections(0)          // an idle MCP holds nothing open
    .max_connections(2)
    .idle_timeout(Duration::from_secs(300))
    .acquire_timeout(Duration::from_secs(10))
```

The Node build's "import the driver on first use" trick has no direct analogue — Rust links
statically — but it does not need one: code that is never executed is never paged in. What must
stay lazy is **pool construction**, not code loading.

### 4. Release profile

```toml
[profile.release]
opt-level = "z"      # measure "s" too; "z" is smaller but can be meaningfully slower
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

The binary's text segment is resident memory. These lines are worth several MB, and `panic =
"abort"` additionally drops the unwinding tables. It also means a panic kills the process — which
is why `AGENTS.md` bans `.unwrap()` on anything that can legitimately fail.

### 5. Allocator

Windows' system allocator is fine. glibc is not: it retains per-arena free lists and RSS ratchets
upward. On Linux either set `MALLOC_ARENA_MAX=2` or link `mimalloc` with a short purge delay.
**Measure both before choosing** — on a program this small the difference between allocators is
occasionally larger than the difference between languages.

### 6. Put the traffic ring on disk

`calls.ts` already reached this conclusion for the call log, and wrote the reason down:

> the previous in-memory ring cost ~135 KB per MCP resident and still lost everything on restart —
> worse on both counts

`traffic.ts` still keeps 500 entries resident. The port should give traffic the same two-layer
treatment the call log has: a JSONL tail on disk, pages read from the end of the file, nothing
accumulating in the heap. See ADR-005.

### 7. Small-string discipline in long-lived structures

MCP names repeat across the registry, the call log and the traffic log. `Arc<str>` for the shared
name, `Box<str>` over `String` for fields that are written once and never grown, `SmallVec` for
argument lists. This is worth low single-digit MB at this scale — real, but do it last, after the
six items above.

## How to know whether it worked

`/api/memory` is already the instrument, and the Rust build must serve the same JSON shape.
The acceptance test is direct:

1. Run the Node build on 19999 and the Rust build on 19998, **against the same data directory**.
2. Point a real MCP client at each in turn; exercise mysql, pg, redis and an http adapter.
3. Compare `gatewayMb` after each has served the same traffic.

Record the numbers in this file as they come in. A phase that does not move the number, or moves it
the wrong way, is worth stopping for.

| Date | Build | Adapters live | gatewayMb | Note |
| --- | --- | --- | --- | --- |
| 2026-09-07 | Node (baseline) | mysql, pg, 2×redis, 2×http, echo | 117.5 | 2×proc asleep |
| 2026-09-07 | Rust Phase 0 (spike) | echo only | **9.1** | release, after 60 requests; 1 thread, 4.0 MB private, 1.05 MB exe |
| 2026-09-10 | Rust Phase 1 | echo + panel | **14.0** | release+mongo, scratch home, echo settled; measured across the crate split (`4147e8a`: 14.2 → 14.0) |
| 2026-09-10 | Rust + terminal plugin | echo + panel + terminal | **13.7** | release+mongo, scratch home, plugin loaded, zero sessions (−0.3 vs Phase 1, i.e. noise); exe 10,649,600 B, +662 KB over the 9,972,224 B ADR-010 baseline — +138 KB under the +800 KB budget; +233 KB per attached local session (budget 1.0 MB, `docs/14` §7) |
| | Rust Phase 2 | mysql, pg, 2×redis | **21.8** | 2026-09-11, `3c3fd7f` release; all three DBs ride the gateway's own SSH tunnels; 40/40 calls ok; WS 21.8, private 8.2, 3 threads; 2×http started not driven, 2×proc asleep |
| | Rust Phase 4 | full workload | **22.4** | 2026-09-11, `3c3fd7f` release; 60/60 calls ok incl. 12× web-reader (http); 9 tunnel rules over 2 SSH connections live; WS 22.4, private 8.6, 6 threads |
| 2026-09-11 | Node (re-measure) | same set, same data dir, same 60-call traffic | **113.8** | fresh side-by-side: private 124.9 MB, 13 threads — consistent with the 117.5 baseline |
| | Rust streaming SQL dump | mysql, pg (data view) | peak **32.5** | 2026-09-13, `9aee217` release; 100k-row/114 MB pg table exported whole-body: csv materialises it (peak WS 164.2, heap 157.9) while format=sql streams (docs/22 W4.4): peak WS 32.5, heap 22.9 at a 20.
| | Rust + MCP OAuth | full workload, figma grant stored | **19.9** | 2026-09-15, `bc7b4c2` release (docs/24); idle after a real Figma authorization round-trip on 19998 — the oauth module is code plus a lazily-read sealed map, so idle cost is unchanged-to-down vs the 2026-09-13 rows measured under heavier tunnels/children; WS 19.9, private 6.5, children asleep.7 baseline — O(chunk) held; the gap grows with table size |

Phase 1 reading: 14.0 MB with the whole gateway present — panel embedded, registry, both on-disk
logs, the plugin host and every adapter family compiled in — against a 9.1 MB spike that had none
of it. The target workload was still to come at that point, and it was the
last row this project could fill without live databases: Phase 2 and Phase 4 need mysql, pg and
redis actually running, plus a real MCP client driving them (docs/12 W2).

Phase 0 reading: the spike (axum + rmcp stateless + bearer gate, no panel, no registry, no DB
pools) settles at 9.1 MB working set after traffic — below the 4–6 MB "baseline" estimate plus
the protocol stack, which is where it should be. It is **not** the final number: the panel
embed, the registry, the call/traffic logs and two TLS-carrying drivers are still to come, and
they are what the estimate table had not named. The premise is confirmed at the floor.

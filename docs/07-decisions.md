# 07 — Decisions

Architecture decisions for the port. Nothing here is still waiting on a human call: the two that
were marked **NEEDS DECISION** before Phase 1 were both settled by what shipped, and each entry now
records the choice and what it cost.

> **Settled during the port:** ADR-001 took option A (no third-party module door). ADR-006 shipped
> option B (GitHub Releases) and left the npm path unbuilt. ADR-005 shipped half of itself — the
> durability half; see its entry.

---

## ADR-001 — Drop the third-party adapter module door

**Status: Accepted — option A.** Shipped.

**Context.** `factory.ts` lets a config entry name a module to load at runtime:
`"type": "mine", "adapter": "my-lmg-adapter"` or `"./my-adapter.mjs"` resolved against the data dir.
The module exports `createAdapter(def, name)` and behaves exactly like a built-in — same lifecycle,
same call log, same panel. `AGENTS.md` in the Node build documents this as the same door built-ins
take, so it is a published contract, not an accident.

Rust links statically. There is no `import()`.

**Options.**

| | Approach | Cost |
| --- | --- | --- |
| **A** | **Drop it. An external adapter becomes a `proc` or `http` MCP.** | A published extension point disappears. Third-party adapters lose in-process speed and the `dbBrowser` hooks |
| B | Keep a Node sidecar for external adapters only | Re-imports the entire memory cost the port exists to remove. Self-defeating |
| C | WASM plugins (`wasmtime`) | A `wasmtime` store plus the host bindings costs more than the whole gateway budget, and every adapter would need rewriting anyway |
| D | Native `dylib`/`cdylib` plugins | No stable Rust ABI. A plugin compiled against a different compiler version is undefined behaviour, in a process holding database credentials |

**Recommendation: A.** The extension point survives in a different shape — anyone can still add an
MCP the gateway hosts, it just runs out-of-process, which is where untrusted third-party code
arguably belonged. What is genuinely lost is the in-process `dbBrowser` / `redisBrowser` /
`mongoBrowser` hooks: a third-party adapter can no longer appear in the Data view.

**What shipped.** `make_adapter` in `crates/lmg-mcp/src/adapters/mod.rs` matches the built-in type names and
nothing else; an unrecognised `type` fails at adapter-build time with an error that lists them
(`Unknown adapter type: … (built-in: echo | mysql | pg | redis | mongo | proc | http | rest)`).
There is no `adapter` field, no module resolution against the data dir, and no runtime loading of
any kind. This is a breaking change to a contract the Node `AGENTS.md` published; no adapter on the
reference machine used the door, so nothing had to migrate, and the loss that remains is the one
named above — a third-party adapter can no longer appear in the Data view.

---

## ADR-002 — One crate, not a workspace

**Status: Superseded by ADR-010 (`4147e8a`).**

A workspace buys nothing at runtime and costs build and navigation complexity. `src/lib.rs` beside
`src/main.rs` already lets `tests/` drive the real application. Revisit only if compile times become
the bottleneck, which at ~15,000 lines they will not.

*What actually forced the revisit was not compile time.* It was that "MCP is a plugin, not the
trunk" became a claim no one could check: Data reached into MCP, and nothing but review caught it.
See ADR-010.

---

## ADR-003 — `current_thread` runtime, `Arc`/`RwLock` state

**Status: Accepted.**

A local gateway serving one machine has no use for work stealing. Each worker thread costs a stack
plus per-thread allocator caches.

The trap, stated plainly because it will otherwise be discovered the hard way: **`current_thread`
does not permit `Rc`.** `axum::serve` spawns connections through `tokio::spawn`, which requires
`Send` futures whatever the runtime flavour. State is therefore `Arc<RwLock<…>>` — uncontended on
one thread, so it costs an atomic and nothing more. Do not attempt a `LocalSet` plus a hand-rolled
`hyper::server::conn` accept loop to recover `Rc`; the complexity is certain and the saving is not.

Consequence: the two V8 flags in the Node daemon (`--max-semi-space-size=2`,
`--max-old-space-size=256`, measured worth ~15 MB) disappear along with V8. They are already inside
the 117.5 MB baseline, so this is not a regression — but it is also not a saving to count twice.

---

## ADR-004 — `mongo` is an opt-in compile feature

**Status: Superseded by ADR-012 (2026-09-11): the feature and the adapter were deleted outright.**
Kept as history: for a year of the port's life the `mongodb` driver sat behind an off-by-default
Cargo feature, forwarded `mongo = ["lmg-mcp/mongo"]` from the bin crate, and the shipped release
was built with `--features mongo`.

The original reasoning: the `mongodb` crate is the heaviest dependency in the set, worth an
estimated 3–5 MB of the target budget, and no MCP on the reference machine used it (live types on
2026-09-07: 2×redis, 2×proc, 2×http, 1×pg, 1×mysql, 1×echo). A `mongo` MCP configured on a
featureless build had to fail with a clear startup error naming the feature, never a panic and
never a silently-missing endpoint.

---

## ADR-005 — The traffic ring becomes disk-backed

**Status: Accepted in part.** The durability half shipped; the memory half did not.

`calls.ts` already reached this conclusion for the call log and wrote the reason down:

> the previous in-memory ring cost ~135 KB per MCP resident and still lost everything on restart —
> worse on both counts

`traffic.ts` still keeps 500 entries resident and restores a tail from disk at boot. Give it the
call log's two-layer treatment: JSONL on disk, pages read from the end of the file, nothing
accumulating in the heap.

**The file format does not change** — only where the read path gets its data. The panel cannot tell
the difference, which is what makes this safe to do while porting rather than as a separate change.

Worth a measurement before committing: at 500 entries with clipped bodies this is roughly 1 MB. Real
but not decisive. If it complicates the port, defer it — it is an optimisation, not a requirement.

**What shipped.** `traffic.rs` writes a JSONL tail beside the call log and restores from it at boot,
so a restart no longer blanks the view — but reads are still served from the in-memory ring of
`KEEP = 500` entries rather than paged off the end of the file. The durability win is in; the ~1 MB
is still resident. Finishing it means pointing the read path at the file, which the file format
already permits — it stays deferred as the optimisation this ADR always said it was.

---

## ADR-006 — How it ships

**Status: Accepted — option B shipped. Option A is not built.**

The Node build is an npm package with a `lmg` bin. A Rust binary can ship either way:

| | Approach | Effect |
| --- | --- | --- |
| **A** | **Keep npm.** Per-platform `optionalDependencies` carrying prebuilt binaries, the way esbuild and swc do it | `npm i -g local-mcp-gateway` and `lmg start` keep working. Existing users notice nothing. Keeps the package name, the README, the install instructions |
| **B** | **GitHub Releases only.** A bare `.exe` per platform | Simplest to build. Every existing user has to change how they install, and `npx local-mcp-gateway` stops existing |
| **C** | Both | A little release plumbing; nobody has to move |

**Recommendation: C.** The npm path costs one CI matrix and preserves the whole existing install
story; the direct `.exe` is what you actually asked for and what a user without Node needs.

Build the Windows binary with `-C target-feature=+crt-static` so the exe is genuinely
self-contained — no MSVC redistributable to chase on a user's machine. Verify with `dumpbin
/dependents` that only system DLLs remain.

**What shipped.** `.github/workflows/build.yml` builds five targets and attaches the bare binaries
to a GitHub Release on a `v*` tag. There is no `package.json` in this repository and no npm publish
step, so `npm i -g local-mcp-gateway` and `npx local-mcp-gateway` still resolve to the **Node**
build. That is the safe order while the two builds run side by side (Phase 6) — the npm name keeps
pointing at the thing it has always pointed at, and nobody is upgraded onto the port by surprise.
Option C stays available: adding the per-platform `optionalDependencies` package is release
plumbing on top of artefacts CI already produces, and the open sub-question below is what it turns
on when someone does it.

Two things this leaves open, recorded rather than resolved:

- **`-C target-feature=+crt-static` is not applied.** There is no `.cargo/config.toml` and no
  `RUSTFLAGS` in CI, so the Windows binary links the CRT dynamically. Turn it on and verify with
  `dumpbin /dependents` before the first release that is handed to a machine this repository has
  never built on.
- **Does the npm package keep the same name?** Publishing a Rust binary under `local-mcp-gateway`
  replaces the Node build for existing users on upgrade. Given the shared data directory and
  identical CLI that is probably right, but it deserves a deliberate yes — and it is only a
  question once option A is actually built.

---

## ADR-007 — No `tracing`, no `regex`

**Status: Accepted.**

`log.ts` is three lines. A hand-rolled JSON-line logger is about thirty and pulls in nothing.
`regex` would be imported for three patterns — `${ENV_VAR}` expansion, the `mask.ts` secret
wordlist, and the loopback host check — each of which is a short hand-written scanner. The regex
engine's code segment is resident memory in a process whose entire budget is 15 MB.

This is a small decision recorded only because both crates are so reflexive to reach for that the
choice would otherwise be re-litigated in review.

---

## ADR-008 — Job Objects replace tree-kill

**Status: Accepted.**

The Node build tree-kills `proc` children on shutdown and keeps a PID ledger to reap orphans a
previous hard-killed instance left behind. It also runs a command-line sweep for known MCP packages
as a backstop, which it must skip when another gateway instance is alive because a command line
cannot distinguish another instance's live child from an orphan.

On Windows, assigning each child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` makes the
OS reap the entire subtree when the gateway's handle closes — including on crash, End Task, and
debugger detach. This is strictly stronger than the graceful path and removes the case the sweep
exists to catch.

**Keep the PID ledger anyway.** The job object covers *this* gateway dying; the ledger covers a
child that outlived a previous generation of the gateway. They are different failure modes. The
command-line sweep can go.

---

## ADR-009 — The panel is copied, never forked

**Status: Accepted.**

`crates/lmg-panel/src/admin_assets/` is copied byte for byte from the Node build's `src/admin/` and
is not edited in this repository. It follows that every `/api/*` response must be shape-identical
to the Node build's.

This looks like a constraint and is actually the plan's best asset: 7,193 lines that need no porting
and no review, plus an executable specification for the admin API that cannot drift, because it is
the same file. A panel change belongs in the Node build, followed by a re-copy.

Enforcement: a golden-response test harness built in Phase 1 (docs/05 §3), plus
`the_tree_is_byte_for_byte_the_node_builds` in `crates/lmg-panel/src/admin.rs`, which compares the
two trees as sets and by content whenever the sibling checkout is present. It refuses to pass by
skipping: a developer machine without the sibling is a failure, not an excuse, because a check that
quietly compares nothing is worse than no check — it reports ok.

---

## ADR-010 — Eight crates, still one binary

**Status: Accepted (`4147e8a`). Supersedes ADR-002.**

The source is a cargo workspace: `lmg-core`, `lmg-host`, the five subsystem crates (`lmg-mcp`,
`lmg-data`, `lmg-tunnels`, `lmg-jobs`, `lmg-panel`) and the `lmg` composition crate. The product is
unchanged — one static `lmg.exe`, measured at +1.1% (9,861,632 → 9,972,224 bytes, release+mongo)
for the crate-boundary codegen, with the idle footprint flat at ~14 MB.

**The split buys nothing at runtime, and saying otherwise is forbidden.** What it buys is that the
architecture stops being a claim. "MCP is a plugin, not the trunk" and "no subsystem depends on
another" were true only as long as everyone remembered; now `lmg-data` has no `lmg-mcp` in its
manifest, so the next change that would reintroduce that edge does not compile. When some future
change appears to need such an edge, the honest reading is that the host contract is missing
something — the connection catalog exists because of exactly that (docs/09 P4).

The cost is real and accepted: eight manifests to keep in step, and gate commands that need
`--workspace` or they silently check the root package alone.

## ADR-011 — The terminal plugin: WebSocket, hand-written ConPTY, no tunnels edge

**Status: Accepted (`310ffe2` … `616b530`, T1–T7 of docs/14). Grows ADR-010's set to nine crates.**

Three decisions, each with the number that justified it:

**WebSocket, not SSE.** A terminal is a bidirectional byte stream; SSE is a one-way text channel.
Over SSE every PTY byte would need base64 or JSON wrapping — a flooding terminal produces MBs per
second, and wrapping burns CPU and memory on exactly that hot path — while client→server would
still need POSTs for keystrokes and resizes: a second channel with its own ordering problems.
axum's `ws` feature costs `tokio-tungstenite` + `tungstenite` in the lock and nothing else: it is
already axum's own websocket stack, so `cargo tree -d` gains no second TLS stack, runtime, or RNG.
Binary frames carry raw PTY bytes both ways; the only JSON on the socket is one tagged resize
struct and the server's exit/error/stalled notices.
Measured: the exe goes 9,972,224 → 10,649,600 B (release+mongo), **+662 KB against a +800 KB
budget** (docs/14 §7); ~440 KB of that is the vendored xterm panel tree that rust-embed puts in
the read-only section — code that is never executed is never paged in, so it is exe weight, not
RSS.

**Hand-written ConPTY FFI, not `portable-pty`.** ~300 lines against the `windows` crate the
gateway already links, versus a dependency tree carrying its own winpty compatibility path in a
process budgeted at 15 MB — ADR-010's "every dependency justifies its weight" decided outright.
`open_pty` returns a `PtyHandle` + `PtyPump` pair: the handle is cheap and cloneable, the pump is
exactly one and lives on a `spawn_blocking` thread — that thread is why local sessions carry a
lower cap than remote ones (docs/14 §7). `cargo tree -d` after T3 was byte-identical to before
it: the seam added zero dependencies.
Measured: **+233 KB working set per attached idle local session** (budget 1.0 MB), and the plugin
loaded with zero sessions reads **13.7 MB against the 14.0 MB ADR-010 baseline** — a −0.3 MB
delta that is measurement noise, not a saving; the honest claim is "flat".

**`lmg-terminal` links no SSH and has no edge to `lmg-tunnels`.** Remote shells arrive through
the `ssh-shell` capability seat in `lmg-host`: the tunnels plugin registers a provider on start
and withdraws it on stop, and the terminal dials through the tunnel manager's own refcount —
sharing the client the tunnels already hold instead of dialing a second one. The plugin's
`requires` stays empty on purpose: a terminal with local shells enabled must work on a machine
with no tunnels at all, and `requires: ["ssh-shell"]` would park it in waitingDependency instead
of honestly listing targets. When tunnels is absent or stopped, `/api/terminal/targets` says so
by name and the panel shows the reason instead of an empty list.

Per-remote-session RSS, measured 2026-09-11 against the two real hosts (开发机 + 构建机,
release build `3c3fd7f`): baseline 23,258 KB → one attached session 23,844 KB (+586 KB, including
the one-time SSH channel setup and code paging) → four attached sessions (2+2) 24,596 KB. Marginal
cost (4−1)/3 = **250.7 KB per session** — inside the ≤ 256 KB row, though only just; a record, not
a gate.

---

## ADR-012 — MongoDB support is deleted, not feature-gated

**Status: Accepted (2026-09-11). Supersedes ADR-004.**

The user's call, made after a year of the port shipping with the feature on and never once using
it: this machine runs no MongoDB, and the driver's weight (heaviest dependency in the set, an
estimated 3–5 MB) bought nothing. ADR-004's compromise — keep the code, hide it behind a flag —
cost two build combinations on every gate run and a second adapter surface to review, for a
capability with zero users.

What was deleted, whole:

- the `mongo` cargo feature in the bin crate and in `lmg-mcp`, and the `mongodb` dependency;
- `adapters/mongo.rs` and `adapters/mongo_resources.rs` (engine, tools, resources);
- the `MongoBrowser` trait, the `BrowserFlavor::Mongo` variant, `lease_mongo`, and the
  `/api/db/{name}/collections` + `/api/db/{name}/docs` routes;
- the mongo column of the admin form (`DIRECT_FIELDS`, `REQUIRED_FIELD`, `TESTABLE_TYPES`) and
  `mongo` in every built-in list a user can be shown.

What survives, deliberately: a `mongo`-typed MCP in a migrated config fails at `make_adapter`
with "the mongo adapter was removed from lmg (ADR-012); this build has no MongoDB support" —
the Node build DOES have mongo (docs/05), so a migrated config must hear what happened, not hunt
for a typo. The shipped build is now plain `cargo build --release`; the gate suite runs one
feature combination.

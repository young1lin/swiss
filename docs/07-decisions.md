# 07 — Decisions

Architecture decisions for the port. Two of these need a human call before Phase 1 starts; they are
marked **NEEDS DECISION** and named at the top so they do not get lost.

> **Open, blocking Phase 1:** ADR-001 (third-party adapters), ADR-006 (how it ships).

---

## ADR-001 — Drop the third-party adapter module door

**Status: NEEDS DECISION.** Recommended: accept option A.

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

**This is a breaking change to a documented contract and needs your explicit sign-off.** If any real
adapter is using this door today, say so — the answer changes.

---

## ADR-002 — One crate, not a workspace

**Status: Accepted.**

A workspace buys nothing at runtime and costs build and navigation complexity. `src/lib.rs` beside
`src/main.rs` already lets `tests/` drive the real application. Revisit only if compile times become
the bottleneck, which at ~15,000 lines they will not.

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

**Status: Accepted.**

The `mongodb` crate is the heaviest dependency in the set, worth an estimated 3–5 MB of the target
budget. No MCP on the reference machine uses it (live types on 2026-09-07: 2×redis, 2×proc, 2×http,
1×pg, 1×mysql, 1×echo).

`default = []`, `mongo = ["dep:mongodb"]`. The shipped release build turns it **on** — a published
binary must serve every adapter type the config schema documents — but a user building for their own
machine gets a smaller binary for free, and CI gains a cheap check that the feature boundary is real.

If a `mongo` MCP is configured and the binary was built without the feature, the failure must be a
clear startup error naming the feature, not a panic and not a silently-missing endpoint.

---

## ADR-005 — The traffic ring becomes disk-backed

**Status: Proposed.** Low risk; decide during Phase 2.

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

---

## ADR-006 — How it ships

**Status: NEEDS DECISION.** Recommended: both.

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

Open sub-question: **does the npm package keep the same name?** Publishing a Rust binary under
`local-mcp-gateway` replaces the Node build for existing users on upgrade. Given the shared data
directory and identical CLI that is probably right, but it deserves a deliberate yes.

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

`src/admin/` is copied byte for byte from the Node build and is not edited in this repository. It
follows that every `/api/*` response must be shape-identical to the Node build's.

This looks like a constraint and is actually the plan's best asset: 7,193 lines that need no porting
and no review, plus an executable specification for the admin API that cannot drift, because it is
the same file. A panel change belongs in the Node build, followed by a re-copy.

Enforcement: a golden-response test harness built in Phase 1 (docs/05 §3), and a CI check that
`src/admin_assets/` matches the upstream tree.

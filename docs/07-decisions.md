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
`"type": "mine", "adapter": "my-swiss-adapter"` or `"./my-adapter.mjs"` resolved against the data dir.
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

**What shipped.** `make_adapter` in `crates/swiss-mcp/src/adapters/mod.rs` matches the built-in type names and
nothing else; an unrecognised `type` fails at adapter-build time with an error that lists them
(`Unknown adapter type: … (built-in: echo | mysql | pg | redis | proc | http | rest)`).
There is no `adapter` field, no module resolution against the data dir, and no runtime loading of
any kind. This is a breaking change to a contract the Node `AGENTS.md` published; no adapter on the
reference machine used the door, so nothing had to migrate, and the loss that remains is the one
named above — a third-party adapter can no longer appear in the Data view.

---

## ADR-002 — One crate, not a workspace

**Status: Superseded by ADR-010 (`2937034`).**

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
Cargo feature, forwarded `mongo = ["swiss-mcp/mongo"]` from the bin crate, and the shipped release
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

The Node build is an npm package with a `swiss` bin. A Rust binary can ship either way:

| | Approach | Effect |
| --- | --- | --- |
| **A** | **Keep npm.** Per-platform `optionalDependencies` carrying prebuilt binaries, the way esbuild and swc do it | `npm i -g local-mcp-gateway` and `swiss start` keep working. Existing users notice nothing. Keeps the package name, the README, the install instructions |
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

*Update (2026-09-22): two present-tense claims above have drifted with the tree — the repository
now carries a `.cargo/config.toml` (the rust-lld linker config) and a `crates/swiss-panel/panel/
package.json` (ADR-024's dev-only toolchain; never a build step of the exe). Neither changes this
ADR's decision: GitHub Releases remains how the binary ships, and crt-static is still open.*

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

`crates/swiss-panel/src/admin_assets/` is copied byte for byte from the Node build's `src/admin/` and
is not edited in this repository. It follows that every `/api/*` response must be shape-identical
to the Node build's.

This looks like a constraint and is actually the plan's best asset: 7,193 lines that need no porting
and no review, plus an executable specification for the admin API that cannot drift, because it is
the same file. A panel change belongs in the Node build, followed by a re-copy.

Enforcement: a golden-response test harness built in Phase 1 (docs/05 §3), plus
`the_tree_is_byte_for_byte_the_node_builds` in `crates/swiss-panel/src/admin.rs`, which compares the
two trees as sets and by content whenever the sibling checkout is present. It refuses to pass by
skipping: a developer machine without the sibling is a failure, not an excuse, because a check that
quietly compares nothing is worse than no check — it reports ok.

---

## ADR-010 — Eight crates, still one binary

**Status: Accepted (`2937034`). Supersedes ADR-002.**

The source is a cargo workspace: `swiss-core`, `swiss-host`, the five subsystem crates (`swiss-mcp`,
`swiss-data`, `swiss-tunnels`, `swiss-jobs`, `swiss-panel`) and the `swiss` composition crate. The product is
unchanged — one static `swiss.exe`, measured at +1.1% (9,861,632 → 9,972,224 bytes, release+mongo)
for the crate-boundary codegen, with the idle footprint flat at ~14 MB.

*Update (2026-09-22): `swiss-terminal` (docs/14) and `swiss-remote` (docs/34) have since joined the
eight — ten crates now link into the one binary, plus the dev-only `swiss-it` (docs/44) which
ships nothing. Still one `swiss.exe`, still the same argument.

**The split buys nothing at runtime, and saying otherwise is forbidden.** What it buys is that the
architecture stops being a claim. "MCP is a plugin, not the trunk" and "no subsystem depends on
another" were true only as long as everyone remembered; now `swiss-data` has no `swiss-mcp` in its
manifest, so the next change that would reintroduce that edge does not compile. When some future
change appears to need such an edge, the honest reading is that the host contract is missing
something — the connection catalog exists because of exactly that (docs/09 P4).

The cost is real and accepted: eight manifests to keep in step, and gate commands that need
`--workspace` or they silently check the root package alone.

## ADR-011 — The terminal plugin: WebSocket, hand-written ConPTY, no tunnels edge

**Status: Accepted (`c405cb8` … `3361429`, T1–T7 of docs/14). Grows ADR-010's set to nine crates.**

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

**`swiss-terminal` links no SSH and has no edge to `swiss-tunnels`.** Remote shells arrive through
the `ssh-shell` capability seat in `swiss-host`: the tunnels plugin registers a provider on start
and withdraws it on stop, and the terminal dials through the tunnel manager's own refcount —
sharing the client the tunnels already hold instead of dialing a second one. The plugin's
`requires` stays empty on purpose: a terminal with local shells enabled must work on a machine
with no tunnels at all, and `requires: ["ssh-shell"]` would park it in waitingDependency instead
of honestly listing targets. When tunnels is absent or stopped, `/api/terminal/targets` says so
by name and the panel shows the reason instead of an empty list.

Per-remote-session RSS, measured 2026-09-11 against the two real hosts (开发机 + 构建机,
release build `3b4934f`): baseline 23,258 KB → one attached session 23,844 KB (+586 KB, including
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

- the `mongo` cargo feature in the bin crate and in `swiss-mcp`, and the `mongodb` dependency;
- `adapters/mongo.rs` and `adapters/mongo_resources.rs` (engine, tools, resources);
- the `MongoBrowser` trait, the `BrowserFlavor::Mongo` variant, `lease_mongo`, and the
  `/api/db/{name}/collections` + `/api/db/{name}/docs` routes;
- the mongo column of the admin form (`DIRECT_FIELDS`, `REQUIRED_FIELD`, `TESTABLE_TYPES`) and
  `mongo` in every built-in list a user can be shown.

The shipped build is now plain `cargo build --release`; the gate suite runs one feature
combination.

**Follow-up (2026-09-11, later the same day).** The Node build dropped MongoDB too
(`local-mcp-gateway` d1957fe: adapter, resources, browser, panel, driver), so the one thing this
ADR had kept — a `"mongo"` arm in `make_adapter` that named the removal for a config migrated
from Node — lost its reason and went with it. A `mongo`-typed MCP now fails like any other
unknown type, listing the built-ins. Neither build carries the word.
---

## ADR-013 — The RustCrypto duplicate stack: merge our own generation away, then name who pins the rest

**Status: Accepted (2026-09-12).**

`cargo tree -d` showed the RustCrypto stack twice: our own sealing path sat on the
aes-gcm 0.10 / digest 0.10 generation while russh 0.63 pulls the aes-gcm 0.11 / digest 0.11
generation. The audit (docs/16 H5) moved every direct crypto dependency of ours — swiss-core,
swiss-host, swiss-tunnels, swiss-mcp — onto the newer generation: aes-gcm 0.11, sha2 0.11,
hkdf 0.13, rand 0.9 (rand 0.9's OS RNG speaks the fallible TryRngCore; a CSPRNG error is a
broken machine and is expected away). The sealed-envelope format is a byte-level contract
(docs/05): the Node-sealed fixture still opens, which is the gate that proves the primitives
did not move.

What the merge removed outright (nine pairs, both copies gone): aead, aes, aes-gcm, cipher,
ctr, ghash, polyval, universal-hash, inout.

What the merge could not remove, and who pins it (all verified with `cargo tree -i`):

| Duplicate | Pinned by | Movable from here? |
|---|---|---|
| sha2/sha1/digest/block-buffer/crypto-common/generic-array 0.10, hkdf/hmac 0.12, rand 0.8, rand_core 0.6, rand_chacha 0.3, getrandom 0.2 | sqlx 0.8.6 (sqlx-core/-mysql/-postgres; rand 0.8 also via rsa 0.9 to num-bigint-dig) | not until sqlx moves generations |
| base64 0.22 vs 0.23 | axum 0.8.9 pins 0.22 | not until axum moves |
| windows 0.62 vs our 0.58 | pageant (russh) + process-wrap (rmcp) | yes in principle — bumping our own windows dep to 0.62 is a real follow-up, and a large diff |
| syn 2/3, windows-implement/interface 0.58/0.6x | proc-macro crates only | build-time only, no binary cost |

Binary cost, `target-release-swiss.exe` (path separators flattened for this table): 7,972,352 to 7,966,208 bytes (-6 KB, -0.08%). The honest
reading: sqlx still links the 0.10 generation, so the disk saving is small; the win is that the
workspace no longer OWNS the old generation, and the aes-gcm chain (the one crate family we
could fully merge) is single-copy. Hard constraints held: no new duplicate pair appeared, and
the TLS stack and async runtime stay single.

## ADR-014 — The secret vault is a sealed file of its own, referenced by `secret://name`

**Status: Accepted (2026-09-14); the reference-syntax clause is superseded by ADR-019.** Spec: docs/19.

Credentials needed one more home. The env store doubles as the environment every child
process reads, so a key stored there for one http MCP is readable by every proc MCP, job
script and local shell; `${ENV_VAR}` also resolves missing names to empty, which is the wrong
failure for a credential. The vault (docs/19) is a separate sealed file, `secrets.json`, in
the same frozen envelope as every state file, holding `{rev, secrets: {name -> value}}` —
D2 said "flat like the env store"; the rev needs one counter, so the values are flat under
one key (recorded as a spec deviation in docs/19 §3).

The decisions that shape the implementation:

- **Names are lowercase kebab, `[a-z][a-z0-9-]{0,63}`** — a different character class than
  `${UPPER_SNAKE}`, impossible to confuse by eye or grammar, and already the idiom of plugin
  and page ids the panel speaks.
- **`secret://name` resolves at every use point** — adapter build, ssh connect, rest test,
  job run — through one resolver shared with env refs. Missing names are a hard refusal
  naming the surface and the reference (never the value); missing env names stay lenient
  (empty) because that contract already shipped.
- **Write-only.** No API reads a value back; the panel lists names. A forgotten value can
  only be re-stored, and the masking stack keys off `is_env_ref`, which now recognises
  `secret://` too.
- **Nothing merges the vault into a child environment.** The one exit for a value is
  substitution at a use point (D8) — a vault value never reaches a proc child's `set` dump.
- **Mutations are rev-checked** like the plugins API: PUT/DELETE name the rev they planned
  against; a stale rev is a 409 with both numbers, a missing rev an honest 400.
- **`export` carries the vault, `import` merges it** — values included, because the bundle is
  already the one plaintext escape; a restore never deletes a name the bundle omits.
- **Host-owned surface, not a plugin.** Every plugin may depend on the vault, so it cannot be
disabled from the page that would do the disabling. The panel section lives on the host's own
Plugins page.

Node-parity rule held throughout: the resolver, the store, the API contract and the export
section are shape-identical in both builds, and the panel is written once in the Node tree.

## ADR-015 — Groups are host mechanism: one model, one scope table, one route family

**Status: Accepted (2026-09-13).** Spec: docs/20.

Six panels grew six ways to group a list, and the seventh (Jobs) would have copied the worst
of them. The decision: grouping is not any subsystem's business. `swiss-host` owns one
`Groups` type with the rules (canonical casing, the first-group sink, rename keeping its
slot), a `GroupScope` registry each scope registers into, and ONE route family —
`/api/groups/{scope}` plus `rename`/`members/{id}`/`order` — mounted beside `/api/tokens`,
the other host-owned mechanism. The panel owns one `groups.js` component every list renders
through.

The alternatives, priced:

- **A. every plugin implements its own grouping and routes** (the status quo's trajectory):
  the third copy already diverged; six code paths, six test suites, six panel adapters.
- **B. the host type + registry + family (chosen):** each scope implements a 6-method trait
  (~40 lines); the host gains one module; the old per-scope routes retire.
- **C. unify only the panel, keep both server sides:** cheap, but even the rename bodies
  differed — the adapters would carry the difference forever, and Jobs/Secrets/Tokens still
  needed writing from scratch.

Retired with the family (docs/20 §3): `PUT /api/order`, `PUT /api/groups`,
`POST /api/groups/{name}/rename`, `PUT /api/mcps/{name}/group`, `PUT /api/tunnels/groups/{kind}`,
`POST /api/tunnels/groups/{kind}/rename`, `PUT /api/tunnels/groups/{kind}/{id}`,
`PUT /api/tunnels/order`. Nothing in-repo called them but the panel and its tests.

Two scopes refuse the family's `order` verb on purpose: secrets and tokens list in name and
creation order respectively (docs/20 §2.1), so their `set_order` answers the same 400 the
family serves for any other refusal.

## ADR-016 — The Node reference build is retired; the panel is edited here

**Status: Accepted (2026-09-13, by the repo owner's decision.)**

The port is complete and has been production for weeks; the two-repo workflow (edit the panel
in the Node sibling, recopy byte-for-byte, guard the copy) now costs more than it protects.
The decision: `crates/swiss-panel/src/admin_assets/` becomes the panel's source of truth and
is edited directly. `the_tree_is_byte_for_byte_the_node_builds` is deleted with it - a guard
that compares against a tree nobody edits is a tripwire pointed at nothing. What survives,
unchanged in force:

- the panel stays plain ES modules served straight from disk - no bundler, no build step;
- the panel's JavaScript stays the spec for the admin API: every `/api/*` shape change ships
  on both sides in one commit;
- the panel's vitest suite moves home: 35 files, 329 tests, now at
  `crates/swiss-panel/panel-tests/` (node is a dev-only test dependency, never a build step;
  since ADR-024 the suite lives at `crates/swiss-panel/panel/test/` behind `npm run check`).
  The one server-coupled file (`admin-panel.test.ts`) shed its five HTTP tests - the Rust
  crate's own suite pins serving, the path guard and the stamp - and its module-graph walk
  was rewritten against the filesystem;
- the sealed-envelope format stays frozen (docs/05) with its committed fixture;
  `scripts/seal-fixture.mts` remains only as a record - the format must not change, so it
  must never need to run.

What is deliberately NOT scrubbed: the historical record. Docs that narrate the port
(gap analyses, specs, the memory numbers in README) keep their references - they are
provenance, not workflow. The sibling checkout itself is deleted by its owner when ready.

## ADR-017 — SQL completion is computed server-side, on the leased connection

**Status: Accepted (2026-09-14).** Spec: docs/22 W3.1.

The Data view's console needed completion. The candidates are dialect keywords (~150 static
words per dialect), the connection's table names, and the columns of the table the caret's
statement reads FROM — and all three live on the other side of the lease. The anti-example is
pgadmin's client-side dbinfo: it walks the whole database into the browser up front (every
table, every column, every type) so the editor can complete offline, and on a real schema
that is megabytes of catalog nobody asked to look at, fetched before the first keystroke, on
every session, forever. Completion is a per-keystroke service, not a dataset.

The decisions that shape the implementation:

- **One route, one lease: `POST /api/db/{name}/completion {sql, caret}`** answers
  `{items: [{label, kind, detail}]}`. The body carries the whole console text and the caret
  as a byte offset (the panel converts its UTF-16 selection index); the reply is bounded by
  construction — the panel shows at most 8.
- **Candidates fold most-specific first**: the FROM-nearest table's columns, then table
  names, then keywords. FROM-nearest is the nearest `FROM <ident>` before the caret — the
  dbgate resolution, without pretending to resolve aliases.
- **The cache lives with the connection, not the request.** Table names and column lists
  load lazily on first use, serve for ten minutes, and drop whole the moment DDL runs
  (console statement or the DDL route — `sql_touches_schema` decides by first word).
- **A 64KB ceiling on cached column NAMES per connection.** Past it the cache degrades to
  keywords + tables rather than growing without bound on a wide schema — a table list is one
  bounded query; every column of every table is not.
- **Redis is excluded**: it has no keywords to offer, and the 404 names what it is, as ever.
- **The panel debounces 150ms** and only asks when the caret ends a word
  (`[A-Za-z0-9_.$]+`); the list borrows the SQL overlay's mirror trick to sit at the caret
  and owns only the keys it consumed — arrows, Tab/Enter, Esc — leaving Ctrl+Enter to Run.

## ADR-018 — Path-space partition: the root is host chrome and future plugins; /mcp/* is the MCP plugin's domain

**Status: Accepted (2026-10, by the repo owner's decision.)** Spec: docs/24.

MCP endpoints lived at the root (`/{name}`), so every MCP name competed with every
future root claim, and the host carried two hand-synced RESERVED lists
(`["api","health","admin"]`, in adminapi.rs and mcp_import.rs) to keep names out of
the root's way. The decision: partition the path space instead of policing names.

- **`/mcp/<name>` is the one and only MCP endpoint shape.** POST and DELETE (session
  delete), exactly as before, one route pattern in `build_app`.
- **Hard cutover, no alias.** The owner's explicit choice: this is a single-user local
  tool and stale client configs get re-pointed once, in exchange for never carrying a
  legacy route. A root single-segment POST answers 404; the spec's P4 gives that 404 a
  "moved to /mcp/<name>" hint when the name is a registered MCP.
- **No host reserved words inside a plugin's domain.** `/mcp/health`, `/mcp/api`,
  `/mcp/mcp` are reachable and legal — the owner's framing: whatever lives under
  `/mcp/` is the MCP plugin's business, not the host's. Both RESERVED lists retire;
  NAME_RE (charset/length) stays, that is path safety, not collision defence.
- **The root belongs to host chrome (`/`, `/admin`, `/health`, `/api/*`) and to
  whatever a future plugin claims there.** A new plugin no longer has to negotiate with
  MCP names, and MCP URLs no longer shift when one arrives — the independence the
  owner asked for.

Alternatives priced: a permanent `/{name}` alias (zero client migration, but a
promise that can never be withdrawn and re-couples root claims to MCP names); a
deprecation-window alias (the same debt with a date on it); keeping RESERVED and
merely adding "mcp" (one more hand-synced entry, still a root-policing mindset).
Hard partition was chosen because it deletes a rule instead of growing one — and
because the reserved lists were load-bearing only while MCP names lived at the root.

## ADR-019 — Vault references wear the `${...}` envelope: `${secret://name}`

**Status: Accepted (2026-09-15).** Spec: docs/25; supersedes the reference-syntax clause of
ADR-014 — its storage, rev, write-only and isolation clauses stand unchanged.

ADR-014's D1 chose the bare scheme `secret://name`: self-describing, greppable, a different
character class than `${ENV}`. What that survey missed is that those virtues belong to
references occupying a whole dedicated field (1Password `op://`, LiteLLM `os.environ/`),
not to tokens embedded in arbitrary strings. Our refs live inside headers (`Bearer ...`),
URLs and command lines, and a scheme substring has no token boundary: the shipped scanner
claimed `secret://aaa` inside `https://test.com/secret://aaa/test` — with the name in the
vault the URL was silently rewritten and the secret landed in it; without it the whole
config was refused. That URL is not a corner case; it is the shape these strings often are.

| | Bare scheme (docs/19 D1) | `${secret://name}` envelope | Dual grammar forever |
|---|---|---|---|
| Token boundary | charset guess | the braces | both |
| URL collision | unresolved | impossible — no `${`, no ref | unresolved |
| Self-describing errors | yes | yes, the scheme stays inside | yes |
| Cost | — | one-time migration | the ambiguity forever |

What shipped (docs/25): one envelope, two families — `${UPPER_SNAKE}` lenient as ever,
`${secret://kebab}` hard-failing as ever; outside `${...}` there are no references, only
byte-identical passthrough whatever the vault holds. Inside the envelope the scheme is a
declared intent: a malformed name refuses rather than guessing. `migrate_legacy` rewrites
whole-value bare refs in memory at each loader (config store, managed, tunnels, jobs v1
mapping) with one boot note per file; the disk spelling survives until the next save.
Mixed strings (`Bearer secret://x`) are NOT auto-migrated — rewriting mid-string tokens
would need exactly the ambiguous scan the envelope exists to replace; the panel's Copy-ref
makes the manual re-save one paste.

The cost: every persisted bare ref rides a transition (whole-value ones invisibly, mixed
ones by hand). Survey evidence in `<vendor>/model-apikey-config-survey.md`: no product
scans a bare scheme in arbitrary strings, and the envelope-with-scheme form has direct
precedents (MCPHost `${env://VAR}`, Cursor `${env:NAME}`, Continue `${{ secrets.X }}`).
---

## ADR-020 — HTTP MCP OAuth: impersonate the allowlisted client, own the token

**Status: Accepted (2026-09-15).** Spec: docs/24 (in the main worktree).

Remote MCPs behind OAuth — Figma being the one that started this — gate every request behind a
provider bearer, and the provider's dynamic client registration (RFC 7591) admits only two
exact `client_name` strings: `"Claude Code"` and `"Codex"`. Anything else registers 403. So the
question was never "which OAuth library" — it was how a gateway whose whole identity is *not*
being Claude Code gets through that door, and where the resulting tokens live.

The decisions that shape the implementation:

- **Impersonation is a default, not a disguise.** `oauthClientName` on the http def overrides
  the provider default (`"Claude Code"` for Figma, per `provider_defaults`); nothing else about
  the client lies. The allowlist is the provider's bug to carry, and the refusal a def-level
  `oauthClientName` still gets is answered with the two accepted names spelled out.
- **Endpoints are discovered, never hard-coded.** RFC 9728 → RFC 8414, every flow. Figma moved
  its scopes to the protected-resource document; discovery reads them there first.
- **Credentials are a sealed file of their own — `mcp-oauth.json`, keyed by MCP name — not the
  vault (docs/19).** The vault is operator-typed values referenced from defs; OAuth grants are
  machine-issued pairs the def never names. `swiss export/import` carries the section, so a
  machine move keeps its grants (ADR-014's plaintext-escape reasoning applied twice).
- **The def surface is two keys**: `auth: "oauth"` turns Authorization over to the gateway (a
  hand-written Authorization header alongside is refused at construction), and the optional
  `oauthClientName`. Everything else — discovery URLs, client registration, PKCE, refresh —
  is the adapter's business, invisible in config.
- **"Anytime auth" is one POST away.** `POST /api/mcps/{name}/authorize` single-flights a flow
  per name (a live flow is handed back, a terminal one replaced); the panel's Authorize button
  opens the consent URL in a real browser window once and polls to `approved`, which stores the
  grant atomically and starts the MCP. A dead refresh token degrades to exactly this button —
  the 401 path refreshes once, retries once, then clears the credentials and surfaces the
  `needs authorization` marker instead of looping.
- **The single-flight dedups on the refused token, not on freshness.** A no-expiry access token
  reads "fresh" right up to the 401 that proves it dead; shortcutting on freshness hands the
  corpse back to the retry. The check is "has the stored token *changed* since this one was
  refused" — the bug class the integration suite pinned.
- **No new dependency.** sha2 0.11 (PKCE S256) reuses the units swiss-core already links for
  HKDF; `cargo tree -d` is byte-identical to the baseline. The flow's loopback callback is one
  ephemeral axum listener per authorize click, gone when the flow ends — zero idle cost.

What was rejected: a general provider-UI (this is one flow, Figma-shaped, with defaults per
provider at the code level); the device flow (the loopback redirect is strictly better on a
desktop with a browser); token reveal in the panel (D7 redaction — the grant is never shown);
and auto-reauthorize (a grant that died needs a human consent click, by design).

---

## ADR-021 — The figma adapter type: sugar over http+oauth, one field long

**Status: Accepted (2026-09-15).** Supersedes the "no provider type" line in docs/24 §5 —
the operator asked for the simple thing the spec talked itself out of.

ADR-020 shipped OAuth as two keys on the http def. For Figma — the provider this whole flow
exists for — those keys are always the same values, and the URL is not the operator's
decision either. So `type: "figma"` is its own adapter type whose def is a name and an
optional description: `make_adapter` expands it into the full http def (the
`FIGMA_MCP_URL` constant, `auth: "oauth"`) and builds exactly the adapter a hand-written
def would get. Every OAuth behavior — refresh, badge, authorize route, 401-retry — runs the
ADR-020 chain with no case of its own.

The decisions that keep it honest:

- **The stored def never grows the fields the type implies.** `build_def` refuses `url`,
  `auth`, `oauthClientName`, `headers` and `proxy` on a figma def — a second way to say what
  the type already says is a way to configure it wrong.
- **One predicate answers "is this an OAuth MCP"** — `is_oauth(def)`: http defs that say so,
  plus the figma type. The badge, the authorize guard and the panel note all read it, so the
  next OAuth provider type touches one function.
- **The row reports the def type, not the adapter kind.** A figma MCP builds an http adapter;
  the sidebar row, the tag chip and the edit form all say `figma`. `tag_of` and the row's
  `type` read `def.type_()`.
- **The panel's figma form is a description and nothing else.** No Test button (a keyless
  handshake is always 401); the detail view's Authorize button is the one step after Save.

What was rejected: keeping Figma as an http preset the panel fills in (a saved def then
carries a URL the operator never chose, and "add Figma" still meant five fields); and a
generic per-provider type registry (one provider does not justify a table — the second one
reopens this ADR).
---

## ADR-022 — The zai-vision type: @z_ai/mcp-server ported native, the Node child retired

**Status: Accepted (2026-09-16).** The operator asked whether the GLM vision MCP could stop
being a Node child and become a type of its own, the way figma did.

`zai-vision` was the last proc def whose child sat in memory for a metered HTTP API. What
the upstream `@z_ai/mcp-server` (0.1.5) actually is: eight tools, each a fixed system prompt
plus ONE multimodal chat-completions call against an OpenAI-compatible endpoint, URL or
base64 media in, message text out. No state, no streaming, no stdio protocol worth a process.
So the port is an `Engine` compiled into the binary (`adapters/zai.rs`): idle cost zero, the
Node child and its ~tens of MB working set gone.

The decisions that keep it honest:

- **Parity is the contract.** Tool names, descriptions, JSON schemas, the optional-argument
  prompt weaving (`<language_hint>` etc.), URL passthrough vs base64 data URLs, ZHIPU/ZAI
  bases, thinking enabled, 300 s timeout: all byte-identical to the deployed build. The
  system prompts live in `zai_prompts.rs` extracted VERBATIM by
  `scripts/extract-zai-prompts.js` — never retyped by hand. One deliberate deviation, noted
  in code: retries skip non-429 4xx (upstream retried everything).
- **The key is always a `${...}` reference.** `build_def` refuses a literal apiKey: a
  literal would ride to the panel unmasked ("apiKey" is not a whole-key secret name), and
  the sealed env store is where a credential belongs (docs/19 D4 reached through the type).
- **No ping, no Test button.** Same rule as http/rest: a metered endpoint must not be
  probed on a timer; the registry honestly reports unknown. The panel's runConnTest says
  so instead of firing a call.
- **The def says only what the engine cannot know.** `mode` (ZHIPU = open.bigmodel.cn,
  ZAI = api.z.ai) or a `baseUrl` override for self-hosted GLM, an optional `model`, optional
  `proxy`/`timeoutMs`. No `url` — that would be a second way to pick the endpoint.

What was rejected: keeping the proc def and shrinking its idle footprint (a lazy child still
costs a wakeup, an npx cache copy, and a process per use — all to wrap one HTTP POST); and a
generic "OpenAI-compatible vision" type (the eight prompts ARE the product; a generic type
would have to carry them as config, which is retyping the prompts by another door).
---

## ADR-023 — The name is the service, the def is a revision

**Status: Accepted (2026-09-16).** The operator wanted to replace an MCP's def under the same
name with the old one kept for rollback (docs/28): "同名可以,但是同名只有一个可以生效."
The first design sketch — a second registry of disabled defs, with runtime arbitration
picking the live one — was rejected before it was built: OAuth credentials, the call log,
group membership and tunnel links are all keyed by name, so two live same-named defs would
contend for one set of state.

The accepted model is the one Kubernetes Deployments and systemd units already share: the
name is the logical service; defs are revisions of it. managed.json gains a `revisions` map
(def snapshots, capped at five per name, oldest evicted). A revision is inert data — never
registered, never started, never resolved; it keeps its `${...}` references verbatim. The
active def stays the only thing the registry and the boot path ever read, so "one live def
per name" is a structural invariant, not a runtime arbitration.

The decisions that keep it honest:

- **Replace is transaction-shaped at the front: build first.** build_def and make_adapter
  run before anything is written, so a bad def changes nothing. After the swap, a def that
  builds but will not start is a 200 with `restartError` — the rollback path must stay
  reachable, not drown under a 500 (the add route treats start failures the same way).
- **Restore takes before it parks.** Parking first could evict the very revision being
  restored when the list sits at the cap.
- **A def swap is not a start.** A stopped MCP stays stopped through replace and restore,
  the rule the edit route already established.
- **Rename carries revisions; delete clears them** — with the OAuth credentials, the parked
  snapshots describe the logical service, so they follow the name and die with it. A future
  same-named MCP starts clean.
What was rejected: the disabled-shadow registry (state contention above); and "rename the old
one first" as the only answer (it works — ADR-less — but the operator asked for rollback
under one name, and a rename that loses the name loses the rollback).

## ADR-024 — The panel is authored in TypeScript, erased to the same JS (docs/36)

**Status: Accepted (2026-10-24).** The panel's source of truth moved from
`crates/swiss-panel/src/admin_assets/js` (hand-written JS, served as-is) to
`crates/swiss-panel/panel/src/*.ts`. The emit pipeline is ts-blank-space: type-only syntax is
blanked out line-by-line, so each emitted `.js` line keeps the number of its `.ts` source
line. The emit is COMMITTED under `admin_assets/js` — the repo stays buildable with cargo
alone, node is a dev-only dependency (npm ci + npm run check in crates/swiss-panel/panel),
and the served tree, rust_embed, include_str! contracts and the /admin/js/main.js entry are
byte-compatible with the pre-port layout. The options table behind this choice is docs/36 §7.2.

What was rejected: shipping .ts with a bundler (a build step the repo never had, plus a
second artifact to keep honest); serving TS and erasing in the browser (type-checking leaves
the machine); keeping hand-written JS (docs/37 §0.2 — the language was ten years behind while
the code carried 1,041 non-null assertions and types bent to fit it).

## ADR-025 — D9 revoked: the panel's gate is the check suite, not byte-equality (docs/37)

**Status: Accepted (2026-10-24).** D9 accepted the modern-TypeScript port on one condition:
the emitted JS must stay byte-identical to the pre-port JS, so behavior change could be
proved by diff. That condition is what made the port write BAD TypeScript — var kept
because const emits longer, catch (e) untyped because errText(e) changes tokens, five
global built-ins bent in types/dom.d.ts so call sites would not need casts (docs/37 §0.2).
D9 is revoked: the acceptance line is now npm run check green (typecheck ×2 + lint +
emit-freshness + vitest), per-file test coverage for every rewritten module, and the
proof-of-life walk on 19998. The machine gate that replaces byte-equality is the eslint
ratchet (docs/37 §9): rules turn to errors stage by stage — no-var, prefer-const,
no-non-null-assertion (with a shrinking whitelist), consistent-type-imports, and
a no-restricted-syntax row that fails ANY .innerHTML = write — zero allowlist: the closeout
converted the last static skeletons too, so none were left to whitelist.

Options considered — A) keep D9 (rejected: the §0.2 bills only grow with every edit);
B) revoke with no replacement gate (rejected: house style forks per-author);
C) revoke and gate on the ratchet (chosen); D) tsc/esbuild emit + source maps (rejected:
loses line-for-line emit, adds a debugging indirection, and does not stop the types lying).
## ADR-026 — Data's resource navigation is object tabs, half of L3, not a new layer (docs/42)

**Status: Accepted (2026-09-22).** The Data page used to hold one object at a time:
opening another table dropped the first one's filters, paging and buffered edits, and an
FK jump replaced the source table. Mature web database clients grow tabs of their own
because the browser's own tab is useless for a database session — a new tab is a cold
panel with no connection and no lease; the tab strip is the window's replacement on the web.

The options were weighed in docs/42 §10: keep the single object and only split the
toolbar (smallest change, did not address the lost work, ruled out by the owner); a
bottom dock (grid + console visible together, but still one object held and it eats
vertical space permanently); a command palette (densest, but a desktop idiom with poor
discoverability that changes nothing about how many objects are held). The decision is
object tabs: the strip is L3 "resource navigation" made visible, **not a new layer** —
the precedent is MCP/Servers' "sidebar picks the resource, segment picks the section";
Data differs only in that it can hold several. Tabs use a third shape (card + glyph +
closable ×) so they are distinguishable at a glance from L2's underlined tabs and L3's
pill segments.

The costs, stated plainly: each tab holds up to one page of 500 rows → the strip is
capped at eight, a background tab's data is dropped and re-fetched on activation, and a
dirty tab is never evicted; the navigation guard went from asking about one record to
asking about every tab (docs/42 D5) — missing one would silently drop the user's edits;
and when all eight are dirty, opening a ninth is REFUSED rather than dropping anything
silently. That last refusal is the direct concession to "ruthlessly small" — bounded by
a cap instead of unbounded memory. Rollback is real too: T1 (the state split) is an
invisible refactor; tabs T2-T4 sit on top of it, and a true retreat removes those three
commits while T1's split stays (a one-tab array is exactly the old single record).

> **Amendment (2026-09-22, `bfb48f8`, recorded in docs/43 增补).** The ninth-tab refusal no
> longer applies to the strip's `+`: it force-opens a new SQL tab even when all eight are
> dirty — the cap shrinks the strip only when a clean tab can be evicted, and going
> over-cap is preferred to refusing an explicit open. Opening an object (a table, key or
> console card) still refuses when every tab is dirty. The bounded-memory concession
> itself stands — eviction on open still bounds what a session holds; refusing the
> user's explicit "+" was the wrong side of the trade.

## ADR-027 — Data's many databases: the primary is writable, the rest are read-only, no per-database pools (docs/43)

**Status: Accepted (2026-09-22).** A database MCP configuration names one database, but
the same instance usually hosts others. The panel answers whether — and how — those are
reachable.

Options: (a) keep one connection = one database; (b) open same-instance cross-database
BROWSING for MySQL only, every other dialect lists-but-disables; (c) give every database
its own pooled connection, uniform across dialects. The decision is (b). The reasoning:
MySQL's cross-database qualification is free — the statements already carry the database
prefix — while pg and Redis would pay a whole "pool per database + lease + credentials"
machinery for nothing but browsing convenience, and (c) contradicts docs/42 §1.5's
"connection leasing does not move".

The costs: the dialects' capabilities are asymmetric, and the panel says so in the
server's own words instead of greying a button with no explanation — a Postgres row is
disabled with "A Postgres connection is bound to one database; browsing this one needs
its own connection", and a foreign MySQL database flips the page read-only with the
configured database named in the status line. The write boundary: secondary databases
are read-only, period — the write path stays pinned to the CONFIGURED database, because
"the database written in the config" is this connection's permission boundary; being
able to browse another database is not authorization to write in it. The Redis catalog
reads the proxy's keyspace from INFO (db0..N); a proxy that refuses CLIENT INFO (the
K_LINE one does) falls back to db0 rather than failing the catalog.

## ADR-028 — The integration harness: real engines in a dev-only crate, never in the shipping graph (docs/44)

**Status: Accepted (2026-09-22).** Seven thousand lines of database code shipped with zero
tests against a real engine: every DB path was "orchestration only" (see docs/08's exception
table) and the self-skipping adapter tests proved the self-skip, not the SQL. docs/44 decided
how that debt gets paid without the binary paying for it.

Options: (a) keep mocking at the sqlx layer and accept that quoting, typing and wire
quirks stay untested; (b) run the workspace suite against a developer-installed local
MySQL/PG/Redis, environment-gated; (c) a separate dev-only crate (`crates/swiss-it`, feature
`it` default-off) whose testcontainers-run engines restore per-test databases from the repo's
own seeds, with L1 browser tests, L2 adapter tests through a real rmcp client on a real
listener, and an L3 proc group over the repo's own stdio MCP server. The decision is (c).
The reasoning: (a) is the status quo the spec opens by rejecting — the exact-string BIGINT,
the completion upper-casing and the DESCRIBE udt_name gap were all invisible to mocks;
(b) makes "the suite passed" mean "passed on Jdoe's machine, this week", and a missing engine
would have to skip, which is a silent green — the one thing the harness may never produce.

The costs, paid and accepted: the dev graph grows testcontainers/bollard (MIT/Apache-2.0)
with its own hyper copy — never shipped, and the CI duplicate check now gates
`cargo tree -d -e normal,build` so dev edges cannot fail it; every swiss-it dependency is
optional behind the one feature, so `cargo test --workspace` on a Docker-less machine
compiles the same three empty targets it did before the crate existed; and gate 2
(`cargo test -p swiss-it --features it`, ~20 s warm on this machine) is now mandatory on
diffs that touch the DB paths, the secure store or swiss-it itself — enforced in AGENTS.md,
CONTRIBUTING.md, the swiss-verify skill, deploy.ps1 and a dedicated ubuntu CI job that the
release job needs.

What shipped (branch `integration-harness`, commits per item): engine lifecycle and the
failure-path copy (I0 `062928a`), seeds with guard tests (I1 `4740a9b`), the three L1
browser suites (I2 `0349c41` MySQL, I3 `239895e` Postgres, I4 `f707247` Redis), the owned
cleanup story — an it-reaper watchdog child and age-based prune (78cd561) — the L2 gateway
group over a real listener, connection-zero proof included (I5 `3cf62a8`), the L3 proc group
over `it-mcp-server` (I6 `2145a44`), and the CI/docs wiring (I7). Each item carried a
mutation check (spec §2.9): one product line changed, exactly one named test red, reverted.
(I5/I6 were redone the same day so each carries its Cargo.lock row — the first cuts left
the lock in the working tree, and a `--locked` checkout of them refused to build.)

The cost that stays: a red machine without Docker still cannot run gate 2 — the answer is the
harness's own failure copy pointing at docs/44-wsl-docker-setup.md, not a skip; and the seeds
are a maintained surface (a schema change in a test's expectation is a seed change, reviewed
as such).

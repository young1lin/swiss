# swiss e2e suite

Per-module end-to-end tests for the real `swiss.exe` binary. No dependencies:
Node's built-in test runner style, sequential per-module files via `node e2e/run.mjs`.

## Constraints (fixed by the user)

- Home: `SWISS_HOME=%USERPROFILE%\swiss-e2e` — never the production home.
- Port: **18999** — dedicated to e2e. 19999 is the user's production instance and
  19998 is the live-verify instance; neither is ever touched here.
- The suite spawns `target-test\release\swiss.exe serve` (the isolated build) with
  those env vars, polls `/health`, drives the API through the `swiss api` CLI (the
  session key is sealed; it is never printed), and kills the instance **only by
  the PID that owns port 18999**. The deploy source (`target\release`) and the
  running production binary (`bin`) are refused by the safety gate.
- Never pass `--port`: `serve` reads `SWISS_PORT`.

## Run

```
$env:CARGO_TARGET_DIR = "target-test"
cargo build --release            # the isolated build the suite runs; a plain
                                 # build would refresh the deploy source
node e2e/run.mjs                 # all modules, sequentially
node e2e/jobs.test.mjs           # one module directly
```

`SWISS_E2E_KEEP_HOME=1` keeps the e2e home between runs; the default wipes it
fresh before every module file. A failed run leaves nothing behind: `stop()`
runs in `finally` — and on a failed `boot()` too.

## Coverage map

| File | Module | What it proves |
|---|---|---|
| host.test.mjs | host/session | /health shape, fail-closed /api gate, /api/info boot contract, plugin registry rows |
| jobs.test.mjs | jobs | weekly-cron save -> edit round trip (BUG#1 data path), interval job fires, run recorded, cleanup |
| remote.test.mjs | remote | targets/endpoints/runs honest shapes, budgets ride along, unknown run 404 |
| process.test.mjs | process | rows/children/history lists, unknown app 404; golden path: a declared app (node itself, typed argv) starts, reads running with a live pid, its stdout lands in the capture, a repeat start is a no-op, the hard-kill stop leaves the OS pid gone, and both outcomes are in the history |
| terminal.test.mjs | terminal | targets list, session open/resize/close when a target exists, honest refusal otherwise |
| tunnels.test.mjs | tunnels | conns/rules/groups honest empty lists |
| data.test.mjs | data | empty catalog, unknown db 404 |
| mcp.test.mjs | mcp | registry lists echo, /mcp/* fails closed without bearer; golden path: a minted token is the bearer (a wrong one is 401), tools/list and tools/call answer, the traffic log names the call, a revoked token is 401 again |
| panel.test.mjs | panel | shell serves, assets no-store, unknown asset 404 |
| memory.test.mjs | memory | project create/list/delete lifecycle on real SQLite; golden path: a bound repo is how an agent's cwd finds the project, a panel note is found by memory_search over /mcp/memory, an agent's memory_save is found by the panel search (lexical, no embedder) |
| restart.test.mjs | host, all | restart persistence: enabled plugins, a client token, a memory note and a declared app all survive a stop/start on the same home, and a bare serve starts no app |
| memory-init.test.mjs | memory, llm | LIVE, `.env`-gated (below): a layered Java shop with `sql/` beside `src/` - `init --full --plan` cuts slices and names the index, `init --full --redraw --wait` draws business domains (never `src` or a bare layer), notes land, and with an embedding model they are indexed |
| llm.test.mjs | llm | status + history read side without a provider key |
| llm-chat.test.mjs | llm | RED contract suite: continuing conversation (>=3 user turns, one context, exact request chronology), cancel/close/open-state wire (strict done.cancelled, partial visible text kept), interleaving, tool round, generic reasoning (local fake: streaming + durability only, no official-host resend promise), burst delivery, auth mask, sealed-past-preview (WAL-aware), restart resume (deterministic fake provider, no network) |
| panel-llm-chat.test.mjs | llm panel | RED real-browser walk of the Test dialogue: live turns, stop, New gating, reload restore, history run sheet |
| safety.test.mjs | harness | pure preflight: port/home/exe refusals (no gateway, no browser, no deletes) |
| panel-*.test.mjs | panel, all modules | real-browser walks (see Browser e2e below) |

Scope note on the llm cancel cases: the e2e cancel-mid-tool-loop scenario
cancels while the NEXT provider round trip after a COMPLETED tool hangs. It
proves honest cancel bookkeeping around a finished tool round - not the
interruption of an in-flight side-effecting pending tool, which the backend
covers with its own unit tests. Both are useful; neither subsumes the other.

## Browser e2e (panel-*.test.mjs)

Every module's panel page walked in a real Chromium (Chrome or Edge, found
automatically or via `SWISS_E2E_BROWSER=<path>`): a one-time session ticket is
spent through `/?token=`, each rail seat is clicked with REAL pointer events
(`Input.dispatchMouseEvent`, never `element.click()`), and visibility is
asserted the way a human sees it — not `hidden`, not `display:none`, non-zero
rect. Uncaught page exceptions and `console.error` entries fail the walk.
The CDP driver (`browser.mjs`) needs Node >= 22 (global `WebSocket`), launches
the browser with a private profile on debug port 19222 (`SWISS_E2E_CDP_PORT`),
and kills only the PID it spawned. On Node < 22 or a machine without a
Chromium the panel-*.test.mjs files SKIP themselves honestly (one line, exit 0).
`panel-settings.test.mjs` is the second-language pass: the language toggle
flips the whole panel to zh-CN and a rail label must read Chinese.

## Live models (memory-init.test.mjs)

The default suite is fake and lexical: no network, no embedder. `memory-init.test.mjs` is the
one live file - it runs a full memory init against real models named in the repo's `.env`
(never committed; `SWISS_LIVE_ENV=<path>` points elsewhere) and SKIPs without them:

```
SWISS_LIVE_LLM_BASE_URL=https://open.bigmodel.cn/api/anthropic # the chat model's endpoint
SWISS_LIVE_LLM_MODEL=glm-5.3-flash
SWISS_LIVE_LLM_STYLE=anthropic                                 # or openai (the default)
SWISS_LIVE_LLM_API_KEY=...                                     # optional
SWISS_LIVE_EMBED_BASE_URL=https://open.bigmodel.cn/api/paas/v4 # optional: semantic search
SWISS_LIVE_EMBED_MODEL=embedding-3
SWISS_LIVE_EMBED_STYLE=openai
SWISS_LIVE_EMBED_API_KEY=...                                   # optional
```

The key reaches the gateway as a `${SWISS_LIVE_*_API_KEY}` reference, never as a stored value.
Without the `EMBED` pair the file proves the lexical path (`embedding: off`); with it, that the
notes are indexed. The runner gives this file 20 minutes.

## Known-bug regression notes

- **Weekly edit crash (panel)**: the crash lived in the panel sheet JS
  (`schedFromJob` read `m[4]` off a 3-group regex); its regression test is the
  vitest case `admin-jobs-sheets.test.ts` ("the weekly m[4] crash"). The e2e file
  `jobs.test.mjs` covers the data path that fed the sheet: weekly-cron save,
  read-back, edit, read-back — if the API ever mangles the cron on save or edit,
  this fails here, not in the user's browser.
- **Remote stdin loss after the SQLite migration**: sealed for new runs by
  `seal_stdin` in `crates/swiss-remote/src/history.rs`; the Rust integration test
  `tests/remote_history.rs::an_exec_stdin_is_sealed_beside_the_record_and_unseals_on_show`
  pins the seal + unseal. Driving a real SSH exec from e2e needs a target host; set
  `SWISS_E2E_SSH_TARGET` (documented inline) to extend `remote.test.mjs` with a
  live exec + `stdinStored` assertion on a machine that has a loopback SSH server.
## Verification overlay: 19998 + target-test (this workstream)

The suite default port stays 18999 and the default exe is the target-test
build. Verification runs for the LLM contract work pin the same discipline
and the harness REFUSES a dangerous overlay before anything boots, stops or
deletes (the pure checks live in e2e/safety.mjs, proven by e2e/safety.test.mjs):

- the port must be 18999 or 19998 - 19999 is production and is never a test
  target; any other port fails the run at import time, and stop() re-checks
  before it kills anything;
- the home must be a dedicated e2e home (a swiss-e2e path segment). The
  production home (~/.swiss or a pinned SWISS_HOME, and anything under it),
  swiss-test-home (the live-verify snapshot), the user profile, the workspace
  root and the filesystem root are all refused; the delete inside boot()
  re-verifies its resolved absolute target every single time;
- the exe must not be target/release/swiss.exe (the deploy source) or
  bin/swiss.exe (the running production binary).

Overlay for our runs (stop any 19998 live-verify instance first):

    scripts/test-instance.ps1 -Stop
    $env:SWISS_E2E_PORT = "19998"
    $env:SWISS_E2E_EXE = "$PWD/target-test/release/swiss.exe"   # from the repo root
    $env:SWISS_E2E_HOME = "$env:LOCALAPPDATA/swiss-e2e-19998"
    node e2e/run.mjs                # or: node e2e/run.mjs llm-chat / panel-llm-chat / safety

Also pinned here: never cargo build --release for these runs - target/release
is what deploy copies to production. Build only with CARGO_TARGET_DIR=target-test,
and only after stopping 19998 (a served exe is a locked link by the time the
fat-LTO link runs). Prove the binary before trusting it: GET /health build.hash
equals the exe's --version hash. One heavy job at a time on this machine; never
vitest in parallel with cargo.

## Final acceptance notes (the live phase)

- The e2e suite always runs on its dedicated synthetic home (swiss-e2e-19998).
  The dated idle-memory record is a SEPARATE phase: it snapshots REAL state onto
  the isolated 19998 instance per swiss-memory-record - the shipped workload is
  never inferred from the fake-only e2e home. Verify each absolute home before
  switching phases, and stop/collect every owned e2e/browser/gateway job first.
- Memory probes use the admin-session API and metrics only; credentials in the
  snapshot stay sealed and are never printed. Record awake children, the
  workload, the private commit, and the previous dated row alongside the number.
- Before any live claim: the restart proof (an open conversation survives a
  stop/start) and the freshness proofs (served panel JS bytes, /health
  build.hash equals the exe --version hash) must all have passed on the
  current target-test build.
- No production 19999 traffic or deployment from this lane; no real paid-provider
  asks anywhere in the suite.

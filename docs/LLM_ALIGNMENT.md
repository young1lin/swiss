# LLM alignment report — memory-plugin vs master, and vs the DSH reference

Status document for the LLM work on branch `memory-plugin`. **RED-witnessed** means the
failing tests existed and were seen to fail ahead of the implementation (RED-first, not
post-facto); assertions amended after the fact are labeled as such, never counted as
pre-fix witnesses. **Final verification is COMPLETE for the requested scope**: every
serialized gate is green on the final source, the release exe identity is proven, and the
live, browser, restart, matrix and (launch-context) memory confirmations below are EARNED.
What is NOT verified is listed at the end; no live or memory green was ever inferred from
suite green.

**Chronology**: the E24 release, shipping-mtime freeze and 19999-untouched statements
below describe the pre-deployment verification cut. The subsequent user-authorized
production deployment has its own [receipt below](<#production-deployment-2026-10-13>).

**Evidence and hashes**: the logs, receipts and screenshots cited below were written to the
author's local `%TEMP%/swiss-evidence` and were never part of the repository, so they are
named, not linked. The commit hashes (`3f9a487`, `a73548c`, `d29f1a7`, `d42a6fe`) are those of
the branch before it was squashed into one commit per feature on 2026-10-22; that history is
kept under the tag `archive/memory-plugin-pre-squash`.

## Branch scope (audited read-only; fresh recompute 2026-10-13)

- `memory-plugin` @ `3f9a487`, `master` = `origin/master` = `7772e1ae` is the direct
  merge-base: 88 commits ahead, 0 behind, no divergence from master (two internal
  merge commits, a73548c and d29f1a7).
- Committed diff: 393 paths = 132 M / 258 A / 3 D, +130399/−16527.
- Working tree: 41 tracked modified, 1 tracked deleted, 14 physical untracked (2
  directories → 4 files, plus 10 standalone). The one deleted path was added on the
  branch and deleted in the tree — it cancels in the net.
- Unique TOUCHED union: 408 = |C ∪ W ∪ U| — C committed-changed 393, W tracked worktree
  status 42 (41 M + 1 D), U physical untracked 14; C∩W = 41, C∩U and W∩U empty; the one
  W-minus-C path [tests/env_precedence.rs](<../tests/env_precedence.rs>); one branch-added
  file is deleted in the tree and cancels in the net. CURRENT NET vs master: 407 = 393 net
  tracked + 14 untracked. Never summed as 449.
- Freeze: no shipping file (Rust source, panel emit, Cargo) in the changed set has an
  mtime after the E24 cut `2026-10-12T16:45:22.0516810+08`. The only post-cut
  shipping/e2e-set writes are the two linked e2e fixtures
  ([llm-chat.test.mjs](<../e2e/llm-chat.test.mjs>) 2026-10-13T00:28:52,
  [panel-llm-chat.test.mjs](<../e2e/panel-llm-chat.test.mjs>) 2026-10-12T23:42:23). The
  final docs passes are later and excluded.
- LLM-relevant layers: [crates/swiss-llm](<../crates/swiss-llm/src/lib.rs>) (13 files,
  +7013, new), [crates/swiss-plugins/src/llm_api.rs](<../crates/swiss-plugins/src/llm_api.rs>) +
  [llm.rs](<../crates/swiss-plugins/src/llm.rs>)/[llm_tools.rs](<../crates/swiss-plugins/src/llm_tools.rs>),
  panel views [llm-*.ts](<../crates/swiss-panel/panel/src/views/llm-providers.ts>) +
  [types/llm.d.ts](<../crates/swiss-panel/panel/src/types/llm.d.ts>), `logs/llm.db` history
  on the shared §host.history engine,
  [e2e/llm.test.mjs](<../e2e/llm.test.mjs>) +
  [panel-llm.test.mjs](<../e2e/panel-llm.test.mjs>).

## Gate table (CURRENT — final source)

| Gate | Result |
|---|---|
| cargo test --workspace | GREEN — `gate-llm-regression-staged-20261012-103536/full2r-workspace-20261012075128.log` (4501 lines, main-parsed): 59/59 bins, **2221 pass / 0 fail / 1 ignored**, exit 0 |
| cargo test -p swiss-it --features it (real DB) | GREEN — `gate2-db-20261012-091418`: exit 0, 233.8 s; primary it/main 108/0/0 (71.67 s); preflight verified (PID 348 stable, ping <=5 s); unique modules mongo 13 / mysql 16 / pg 16 / gateway 9 / proc 8 / reaper 2 / redis 27 / seed 8 / detached 6 / smoke 3; the designed FORCE_RED smoke proves the reaper; reserved ports untouched |
| cargo clippy --workspace --all-targets -- -D warnings | GREEN — `gate-clippy-r3-20261012-091112` and `gate-llm-regression-staged-20261012-103536/full3r-clippy-20261012075717.log` (final source, 22 lines, zero diagnostics), no suppression |
| cargo tree -d -e normal,build | GREEN — depth-0 exit 0 (14 members); shipping `-p swiss -d -e normal,build` exit 0; no prohibited duplicates; TLS/runtime as declared in SPEC; manifest AFTER proof GREEN (feature union unchanged) |
| npm run check (panel) | GREEN — `gate-llm-regression-staged-20261012-103536/full4r-check-20261012075745.log`: 124 files, **1668 pass**, lint 0 errors / 822 ratchet warnings |
| node e2e/run.mjs (FULL26) | GREEN — `full26-runner-20261013-003743-dznkro.log` + `full26-run-all-20261013-003743-dznkro.log`: exact 26 alphabetical modules, every module ok, all passed; job 1165 collected exit 0 |

Historical mid-stream checkpoints — workspace `gate-workspace-r2-20261012-083144` (59/59,
2218/0/1) and panel `panel-full-check-v2-20261011-193435` (124 files, 1653/1653) — stay
earned evidence; the rows above are the final-source results.
**Formatting checks, qualified**: [SPEC.md](<SPEC.md>) and the
[alignment ledger](<LLM_ALIGNMENT.md>) are LF-only and free of trailing whitespace;
the docs-scoped diff check is CLEAN for tracked [SPEC.md](<SPEC.md>) changes. The
[alignment ledger](<LLM_ALIGNMENT.md>) is untracked and excluded from Git's diff check;
its LF/trailing-whitespace properties were checked directly. No workspace- or
branch-wide `git diff --check` green is claimed. Non-doc checks remain NONZERO; the branch-vs-master
diff also contains warnings outside the generated panel emit. Line/column-preserving
type-erasure padding explains the reviewed generated-emit subset, not every warning
in the branch. These warnings predate the final docs passes; no source or emit edits,
reformat, or code fix were made in those passes. Core gates green does NOT mean a
global diff-check green.

## Release (pre-deployment verification; target-test identity)

- Accepted FINAL exe: SHA
  `E24EE1FECE55B8C32D46326A971DAD6B81D2311D8FAD3B7402F73F5609B4AA29`, 24333824 bytes,
  mtime `2026-10-12T16:45:22.0516810+08`; managed release3, main job 992, exit 0, Finished
  5m03s (fingerprint `gate-release-staged-20261012-112312/release3-fingerprint-20261012-164523.json`: SHA / bytes / 303 s /
  job 992 / cut). Scope `target-test` — **production 19999 was never touched and no deploy
  ran**. Freshness is proven by the SHA plus the served bytes and wire pins; the cached
  version stamp (2026-10-12T01:25:11Z) is NOT freshness proof.
- No newer target-test release was needed for that verification cut: Rust/emit/Cargo
  froze before E24 (scope freeze above); the later verification writes were e2e fixtures
  and docs. The subsequent production build is a separate identity proof.
- HISTORICAL: PH0 `gate-ph0-release-20261012-092434` (0 errors, 459.1 s, npm 0 written /
  117 unchanged, exe `298C…BA36D`, 24321536 bytes) — superseded. RELEASE2 was killed by the
  original 570 s Node-launch ceiling: cargo exit null, NO cargo RED, stage UNKNOWN (its
  stage claim is retracted); the old `30087E4E…E37E` exe (24333824 bytes, 227.7 s) remains
  an immutable SNAPSHOT provenance record only — never an E24 alias.

## Live, browser, restart — earned

- **Live privacy, EARNED and repeated** (PH4 on E24, then PH5 / PH6 / FULL26): the
  >2.6 KiB answer read in full through the unseal path on a running instance while its tail
  is absent from the raw database — **WAL sidecar as well as main file**; 8 sealed pieces;
  turnBytes 2625 = the exact LONG_ANSWER; callBytes 6488 = the v2-API lexemes-derived
  request; both classes sealed 1 / plain 0; db + wal + shm all three marker-free. The
  earlier "live proof owed" marker is closed.
- **Continuation contract, live**: llm-chat cancel shape 200 / done / partial retention /
  idempotent; canonical replay — five ordered cancel-replay slots (S1, S2, S3, HOLD, AFTER
  — [llm-chat.test.mjs :443-544](<../e2e/llm-chat.test.mjs#L443-L544>)) with the assistant
  prefix ONCE and the unsent-completion negation untouched; interleaving isolated to
  per-agent calls (two each); restart EXACTLY 8 ordered including
  HOLD; a same-run send after the ct point returned **409 `unresolved-tail`** at the time -
  since 2026-11-03 the cancelled wait settles and the same run continues (SPEC §llm.history:
  an interrupted round never strands a conversation; llm-chat.test.mjs pins the continuation); FRESH distinct
  `continued=false` clean recovery — a DIFFERENT run, not a same-run continuation (the
  earlier same-run claim is retracted). The registered E2E unchanged-pair risk closed with
  these observations.
- **Panel**: the two-module pair (llm-chat + panel-llm-chat) first full green together (PH10); a
  fifth consecutive complete in FULL26 (close-1 / close 200 / no postmark GETs / 4 chats /
  0010); ownership 4/4; zh settings green; New closes a known open id even while the
  dialogue still loads, and a late retired restore cannot resurrect old turns or a dead id
  (earned-unit semantics).
- **Matrix (real browser, final run matrix-4)**: all 8 cells — en/zh × light/dark ×
  1440/960 — complete, 32 PNGs (8 cells × 4), no page errors; the old rail-187 timeout did
  not recur. Scored as 8 complete cells: 15 primary step entries per cell + 2 global prep
  (not "8×16"). Both the fallback-after-private-preference-clear and the
  remembered-seat-real-FROM-REMOTE → exact-history flows are EARNED under the current
  contract; the retrospective old-post-hash cause is NOT claimed. Remote content is
  scope-qualified: a real control with typed Enter and surviving text — the actual external
  search-result content is NOT asserted. Artifacts: OUTDIR `matrix-sgF7lL` (32 PNGs; under
  `%TEMP%`/swiss-evidence), wrapper `matrix-runs/20261013-004922-9cd87b/driver.log` (133 lines) /
  `matrix-runs/20261013-004922-9cd87b/receipts.json` (16), job 1173 collected, driver
  exit 0. Main visually inspected at full 1440x900 the wide
  history (`matrix-sgF7lL/en-light-1440/02-history.png`) /
  gallery (`matrix-sgF7lL/en-light-1440/04-gallery.png`) PNGs and the zh-dark-960
  01 Test (`matrix-sgF7lL/zh-dark-960/01-test.png`) shot — 3 of 32 by eye; the history
  pane is positively open with user/task copy visible, and the final body extends below
  the fold — no all-text-simultaneously-visible claim.
- **Terminal, honest refusal**: the terminal route refused opening / no session — no
  successful terminal creation is inferred.

## Memory (launch-context record, not a gate)

- ACTUAL6 PRIMARY (`memory-actual6-diag-20261013-004434-3mjjca.log` with receipt `memory-idle-booleans-20261013-004446.json`, job 1171, collected
  exit 0): guarded launch with an inherited, pre-spawn-attached PARENT console (true /
  count 8); RAW child count **0**, rows empty, classes empty; procStarted 0; strict
  `idleProofRuntime` **TRUE** (S1/S2 true) with the original gate unchanged and NO
  exemption; API 28.4 WS working set with heapUsed 12.4 / heapTotal 12.4 (both heap fields
  map PRIVATE bytes — [mem.rs](<../crates/swiss-host/src/mem.rs#L73-L84>); an API-rounded figure, not a literal allocator-heap
  measurement); OS WS 29773824→29732864, private 13053952→13033472; manifest 9 / drift 0 /
  E24 / owner / version-info true; MCP 11 (idle 1 / started 10), tunnels 3 of 10; cleanup
  handle/port/env all true.
- This is a **context-only controlled-idle record on a frozen real-state snapshot with an
  inherited proved parent console** — it supports the launch-mode explanation. It is NOT a
  production footprint, NOT a system-image identity claim, and NOT an optimization delta
  against the ordinary-production 29.95 MB row; no band is enforced.
- HISTORICAL (kept, not void; actual6 does not void them): actual4
  `memory-actual4-diag-20261012-235207-ezfllk.log`
  measured RAW child 1 with identity UNKNOWN; actual5
  `memory-actual5-diag-20261013-001334-1fxazh.log`
  observed ONE child of class console-host (pid 23808 / parent 31072 — basename class only,
  not a proven system image). BOTH REFUSED the strict idle proof —
  "idle-proof-not-established", exit 1. actual6 guarded context supersedes without
  contradicting them.

## Design now specified (SPEC §llm.history / §llm.api)

| Item | Status | Evidence |
|---|---|---|
| Embedding round trip records full input texts + vectors (6dp), quota-bounded, 180-day sweep | Implemented (commit d42a6fe) | [llm_history.rs](<../crates/swiss-llm/tests/llm_history.rs>) |
| `call.run_id` nullable (v2), backfilled by window for legacy calls | Implemented | schema step 2 in [history/mod.rs](<../crates/swiss-llm/src/history/mod.rs>) |
| Runs filed for builtin-loop Tests (turns + calls) | Implemented | [llm_api.rs](<../crates/swiss-plugins/src/llm_api.rs>) `run_recorder` |
| Test dialogue = persisted conversation, >=3 user turns, incremental filing | Earned: suite + FULL26 + live | go7-r3 + `full2r` + `full26-run-all-…dznkro` |
| v3 `run.kind` discriminator (conversation = 1 visible/open; oneshot placeholder kind NULL, hidden, made real by UPDATE at finish; NULL = legacy) | Earned: suite + FULL26 + live | go7-r3 + `full2r` + `full26-run-all-…dznkro` |
| ID reservation: SQLite reservation against real rows; explicit `run_id`; window linkage migration-only; fails closed on submit/receiver/SQL error, no window fallback | Earned: suite + FULL26 + live | go7-r3 + `full2r` + `full26-run-all-…dznkro` |
| Early SSE `run` event; `done` carries run id; optional `runId` resume, no row/body duplication | Earned: suite + FULL26 + live ladder | go7-r3 + `full2r` + `full26-run-all-…dznkro` |
| `POST agents/test/cancel`, `POST history/runs/{id}/close`; discovery via EXISTING `history/runs?agent=&open=true` | Earned: suite + FULL26 + live ladder | go7-r3 + `full2r` + `full26-run-all-…dznkro` |
| Fail-closed rejects (unresolved agent/provider/model/system, body gone, byte bounds = decoded UTF-8 bytes) — reject, never clip | Earned: suite + FULL26 + live | go7-r3 + `full2r` + `full26-run-all-…dznkro` |
| THREE distinct bounds: (1) outward SSE queue = 32 queued LOOP EVENTS via the awaited sink on ALL native asking paths incl. genai fallback; (2) raw HTTP/SSE caps = OpenAI transport scope ONLY; (3) record/context = 512 KiB decoded materialization / serialized request | Specified; seams earned incl. FULL26 | go7-r3 + authority-postfix + `full2r` |
| v6 authority/accounting: nullable `call.attempt` proven in settle TX before writes; sealed row+body one insert TX, sweep paired; terminal tailNULL + `tail_plain` 0 atomic | Earned: suite + FULL26 | gate-authority-postfix + `full2r` L2852 rider |

## Privacy boundary of the LLM record (as the code behaves)

- Each call/turn row keeps a **~2 KiB plaintext preview inline in the index, by design**:
  the head of a text is readable in the raw local database file. The **full bodies seal**
  as 64 KiB `§host.history` chunks — the tail seals. A plaintext `note.txt` beside the
  store is intentional, not a sealing failure.
- Suite proof GREEN (go7-r3): strict sealed-tail — exact decode, nonempty refs,
  EVERY-chunk `is_sealed`, mandatory main file, NotFound-only-sidecar sentinel absence.
- The LIVE proof is EARNED (PH4 on E24; repeated PH5 / PH6 / FULL26): the >2.6
  KiB answer read in full through the unseal path on a running instance while its tail is
  absent from the raw database — WAL sidecar as well as main file — and db + wal + shm all
  three marker-free (details under Live, browser, restart).
- Embedding calls record full input texts + vectors (6dp) intentionally (commit d42a6fe):
  quota-bounded, swept at 180 days, admin-only behind the session, never exported. Sealing
  is confidentiality, not anonymization.

## Decisions

- **Provider coverage (budgets scoped)**: the native Anthropic/genai fallback is PRESERVED
  — no new native wire or signature mapping. The bounded raw adapter applies to the
  OpenAI stream and owns model and error collection; the native path and the remaining
  blocking-genai hidden buffers are NOT fixed by the SSE library. No claim that ALL
  providers are bounded.
- **Parser dependency — CLOSED (human-approved)**: `sse-stream` 0.2.6 is a direct
  dependency of swiss-llm, LANDED and wired. `eventsource` 0.2.3 was rejected for a
  verified BOM panic; genai own source is not reused. Suites, the full workspace gate,
  clippy and the official graph are all GREEN.
- **Manifest — AFTER proof GREEN**: package set and enabled feature union unchanged; the
  lock only changed by the single swiss-llm sse-stream member line plus 3 edge-path
  reroute lines (no new package headers); the `reqwest` `stream` feature is declared in
  both target sections and was already active via genai.
- **Authority witness A semantics**: a test-driven policy flip on the EXISTING facade
  `set_quota` seam — production keeps the default policy; no new operator config
  feature, no same-threshold claim, no paid proof.
- **Run-id reservation**: MAX-only in-memory counter rejected (restart orphan-id reuse,
  cross-process collision). SQLite reservation against real rows; oneshot placeholder
  kind NULL hidden, finalized by UPDATE; conversations kind 1, visible/open; explicit
  `run_id` for scoped calls; window linkage migration-only; reservation fails closed on
  submit/receiver/SQL error — no window fallback.
- **Byte bounds**: decoded UTF-8 bytes — never characters, previews, or a cipher
  footprint; materialization (decoded PLAIN bytes) and context
  (system/prompt/tools/thinking) caps defined separately; the OpenAI serializer measures
  the actual body, genai upstream-private serializer means canonical ChatRequest
  serialization is not byte-identical native-wire proof; no new native mapper or scope.
- **Async internals**: internal boolean sink becomes `Option<Reply>` with an explicit
  stop — internal only; no wire field, no API break.
- **Reasoning replay, v1 scope**: exact official DeepSeek host + currently verified model
  + tools; `reasoning_content` of ALL previous assistant turns required with tools
  (provider-verified); DSH native signed thinking IS replayed across user turns. No
  universal exclusion; proxy/local-alias capability deferred, no UI knob.
- **Verification release identity**: the FINAL target-test exe is E24
  (`E24EE1F…B4AA29`, fingerprint proven); the
  3008 exe is immutable snapshot provenance, never an alias; freshness is SHA + served
  bytes + wire pins, never the cached stamp.
- **Memory measurement context**: controlled snapshot launch records are context-only;
  no production footprint, system-image or optimization claims ride on them.
- **Retracted unsafe preflight claims (explicit, historical)**: RxErr does not mean the
  DB is absent; reservation existence is not activity; a simple bool assign is not CAS
  exclusion; an any-call transaction clear is insufficient; an old migration before a NEW
  outbound cannot prove the OLD tail. The implementation handles ambiguity conservatively
  and preserves fresh-unused positives and the history audit.
- **TLS/runtime audit — closed, no changes**; **logger — no live leak, no change** (no
  subscriber today, ADR-007); **retries** — auto-retry optional, the mandatory core is
  typed retryability.

## Risk matrix

| Risk | Disposition |
|---|---|
| R1: embedding record stores full texts + vectors | **Intentional, kept** (d42a6fe). Admin-only, quota-bounded, 180-day sweep, never exported; ~2 KiB plaintext previews are part of the designed boundary. |
| Provider-safety fix | **LANDED** (sse-stream 0.2.6 direct, wired); suites, workspace, clippy and graph all green. |
| Durable no-fork counters | **CLOSED** in suite, workspace AND the live ladder (public no-fork green under go7-r3, workspace r2/full2r, live continuations). |
| Deferred (not invented) | Agent goals; preset catalogs beyond the panel three Add offers; context compaction; native opaque signatures. |
| Keys | Reference-only (`secret://` / env), never literals, never logged, never in the record. |

## Retractions (explicit — one line each, historical)

- Census "Count certain finding" — PS 7.6.6 dot-KEY precedence is correct; retracted.
- "Current literal-backslash separator" — the codepoints [92,110,92,110] are two SOURCE
  backslashes producing real runtime NEWLINES; retracted.
- Same-run tool continuation — the FRESH path is a distinct run (`continued=false`); the
  same-run claim is retracted.
- RELEASE2 stage — UNKNOWN (no cargo RED, exit null); the stage claim is retracted.
- History-loss on restart — `body.runs` was UNDEFINED (not an empty array), HTTP status
  unrecorded; no loss claim stands.
- 503 retro-body — none; never claimed again.
- "Zero artifacts" — `01-test.png` exists (the first two PNGs are preserved); retracted.
- "Empty-then-remount" — the wait was an uninvoked arrow (DOM query 0); retracted.
- 7b6 snapshot-differs-from-measured causal claim — retracted (raw child 1 was the whole
  fact).
- bf extra TEMP synthetic config-envelope read — outside the requested review scope;
  struck from evidence; no secret plaintext printed, no further state reads.
- Matrix old-295 / old-343 — 295 was a TASKED-vs-render mismatch on a raw pin; 343 a
  single-sample async-fill observation (old body/loading unrecorded). Both superseded: the
  CURRENT API-exact, rendered-UI, reload and history flows are EARNED (matrix SECOND
  primary 1155 + matrix-4). No retro-cause claim.
- it-module agent tallies 14/17/17/7 — retracted for the main-parsed unique module list.
- The 66-char hand-typed PIN — a runner defect, not product support; the old 212-KiB
  budget attribution is VOID (filler under `request.options` never landed).

## Historical REDs and early evidence (concise, retained)

| Layer | Result | Artifact |
|---|---|---|
| Backend routes | 5 RED + 6 old GREEN | `red-a16-llm-api-20261011-173459` |
| Provider stream | 6 RED, 0 compile errors | `red-0b-provider-streamed-20261011-174010` |
| History UI | 2 RED + 12 old GREEN | `vitest-fe4b-admin-llm-history-20261011-174046` |
| Dialog UI | 11 RED + 27 old GREEN | `vitest-fe4b-admin-llm-174217` |

Caveat kept: two backend tests were AMENDED to the approved open-until-explicit-close
semantics — their amended checks are NOT pre-fix RED witnesses, and are not counted as
earned REDs.

- `gate-workspace-20261012-044227` = 56/59 bins, 2215/3/1, exit 101 — all three failures
  fixture/precondition artifacts with attribution retractions kept (env: an inherited valid
  Swiss pin; maintain: MAIN-only denominator with pre-existing refusals, no sweep executed;
  process: paused clock + SQLx always-acquire deadline).
- Clippy iterations, attribution kept: FIRST pass 7 LLM diagnostics = six expressions +
  `expect_err` only; SECOND pass 3 ROOT diagnostics = the private cold `Box<Response>`
  helper + the eight same-refusal paths + one unboxing caller, plus two single-literal
  concat unwraps.
- In-batch closure REDs: env L681; maintain L2965 (designed share-warn to ok at L2966);
  both process clocks L3903-04 — artifacts `gate-env-actual-…045626`,
  `gate-quota-diag-…045736` / `gate-quota-fix-…050910`, `gate-process-discrim-…045132` /
  `gate-process-fix-…050542` / `gate-process-helper-…051725` (70/70).
- Authority lane GREEN (`gate-authority-postfix-20261012-043636`, validated 64, zero
  degraded; pre-fix REDs `gate-authority-20261012-042711`); witness A caveat above.
- Coherent run `gate-go7-r3-20261012-041943` (lib 105/105; llm_history 24/24; llm_agent
  1/1; ROOT llm_api 22/22) + owning host gate `gate-host-20261012-034132` 394/394.
- PIN record (`runner-repair-20261012-0310`: valid-64 exit 0, 66-char exit 9) and the
  two-witnesses repair (`gate-two-witnesses-20261012-032643`) — kept.

## DSH reference (read-only)

- The documented preset path `apps/cli/config/agent-presets/code/agent.cordis.yml` does
  not exist in the reference checkout; `~/.dsh/.agent-presets` is empty. Real artifacts:
  `apps/cli/config/examples/*.cordis.yml` and fixtures under
  `packages/subagent/subagent-in-process-driver/tests/fixtures/presets/`.
- Existing alignment hooks: cc-otel Claude-Code event vocabulary, claude/pi agent
  runtimes, openai + anthropic wire styles incl. GLM endpoints, picked-never-typed
  models.

## NOT verified (qualified, final)

- External remote search-result CONTENT — a real control with typed Enter and surviving
  text only; the content itself is unasserted.
- Matrix PNGs: machine-verified complete (receipts + steps); visually inspected only in
  part (wide history/gallery + zh-dark-960 `01 Test`).
- Terminal open — the route refused (no session); no success claimed.
- Production API/UI smoke and paid-provider requests — not run during deployment;
  production binary, process and health identity are proven separately below.
- Memory in an ordinary production launch context — only the controlled snapshot context
  is measured; no production delta or optimization is claimed.
- PH3 historical root for the retired-restore seam — still undetermined.
- The two amended backend assertions — no fresh pre-fix RED witness exists (caveat kept
  above); they are green now, never counted as earned REDs.

## Production deployment 2026-10-13

- User-authorized deployment is **COMPLETE**, checked at `2026-10-13T09:11:39+08:00`.
  The unpatched [official script](<../scripts/deploy.ps1>) owned the full sequence;
  no `-SkipGates` was used. Successful managed job **`pwsh-1230`**, exit **0**, collected.
  The earlier launch-message label `pwsh-1233` was a reporting typo, not another run.
- The first attempt (`pwsh-1222`, exit 1) stopped before the outage: the temporary Cargo
  forwarder lost PowerShell's bare `--` boundary, not a Rust-code diagnostic. Its
  footer (`deploy-official-20261013-090000.log` lines 2823-2833)
  explicitly leaves production untouched. The corrected retry reran all gates.
- Fresh retry gates, serial and locked/offline with at most four Cargo jobs: workspace
  **59/59 result bins, 2221 pass / 0 fail / 1 ignored**; real-DB main **108 pass / 0 fail**
  (footer: `deploy-official-20261013-092500.log` lines 2770-2783);
  Clippy all-targets **zero diagnostics**; panel typecheck/lint/emit freshness plus
  **124 files / 1668 tests passed** (summary: `deploy-official-20261013-092500.log` lines 4089-4098).
- Staging: `npm run build` recorded **0 written / 117 unchanged**; a timestamp-only
  [lib.rs](<../crates/swiss-panel/src/lib.rs>) touch verified identical bytes before
  and after (staging log: `deploy-official-20261013-092500.log` lines 1-18).
  This metadata-only operation follows the historical mtime-freeze cut, without
  shipping-source edits or hand-edited emit. Release build finished in **6m 59s**.
- Fresh [TARGET exe](<../target/release/swiss.exe>) and installed
  [BIN exe](<../bin/swiss.exe>) are identical: **24333824 bytes**, SHA-256
  `9C971C7F440D54DCED5E1A966BD492F058E23EC271D4097EE9F2EA657F8BA7F2`.
  Main independently re-hashed both files after collection. This production SHA is
  separate from E24; matching E24 bytes was never a deployment requirement.
- BIN version: `swiss 0.1.4 (3f9a487, 2026-10-11T09:08:58Z)`; 19999 `/health` is
  `ok=true`, `build.hash=3f9a487`, matching the BIN version stamp. The cached timestamp
  is **not freshness proof**. Listener pid **36072**, image BIN; old pid 2920 stopped.
  Official measured outage **8.3 s**, lock released, `ENV_RESTORED=True`.
  Success footer (`deploy-official-20261013-092500.log` lines 4103-4121)
  and whitelist receipt (`deploy-v4-success-receipt-20261013-091000.json`).
- Production validation was limited to health, CLI status/version and binary/process
  identity. No production API/UI walkthrough, session ticket or paid LLM request was
  made; the earlier real-browser proof remains the isolated 19998 verification.

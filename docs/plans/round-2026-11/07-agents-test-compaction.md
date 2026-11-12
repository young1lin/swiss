# 07 — Agents Test: compact the conversation (`/compact`, a Compact button)

Implementation brief, not SPEC. The SPEC amendments in §7 land in the commit that makes each
paragraph true. Shared rules: [00-handoff.md](00-handoff.md). Independent of 02–06; needs 01 for
its strings.

## 1. Goal

The Data page can compact its conversation (SPEC §data.agent Compact, `POST /api/llm/db/compact`).
The Agents page's Test sheet cannot. A long Test dialogue on a builtin agent fills
`maxContextKiB` and has nowhere to go but New. SPEC §llm.roadmap lists "context compaction of
the Agents Test's conversations" as not built. This brief builds it on the same engine: the same
`conversation::fold`, `run_compaction`, `COMPACT_FRAMING` and `undo_fold`, behind a new Test
route, with `/compact [focus]` and a button in the Test sheet.

## 2. Facts (verified 2026-11-12)

**Server**

- The Data page's route is `crates/swiss-plugins/src/llm_db/routes.rs:707-830`, `compact()`. Its
  steps, in order:
  1. Parse the body keys.
  2. Check `focus` against `FOCUS_MAX` (2 000, `llm_db/mod.rs:91`).
  3. Busy check: `state.seat_run_of(&seat)`.
  4. `within(scope(&conn), conversation::fold(&history, joining, &name, runtime, &chat, &base))`
     gives `Fold {lane, prior, system, from}`.
  5. `state.seat(&seat, run_id, cancel, shut_steer())`. When the seat is lost it calls
     `run.release()` and `undo_fold`.
  6. Build the instruction: `DB_COMPACT_INSTRUCTION`, plus the focus line.
  7. Tools: `DbTools::new(..).specs()`, declared but never served.
  8. Send the `run` frame `{runId, from, agent, conn, provider, model, compacting: true}`.
  9. Spawn the drive, which calls
     `swiss_llm::agent::run_compaction(&chat, &system, &prior, &instruction, COMPACT_FRAMING, tools, &session, &mut run, &mut on_event)`.
     It runs under the `tx.closed()` watch, and its result is checked for spoilage: cancelled,
     `cut` at `maxTokens`, or an empty summary.
  10. Send `done` `{ok, compacted, runId, from}`. When no checkpoint landed, `undo_fold` runs
      first, and `runId` becomes the old id, or null if it could not reopen.
  11. `unseat`.
- The Test stream is `crates/swiss-plugins/src/llm_api.rs:850-990`, `agent_test_stream`.
  - Body keys `["agent","prompt","runId","tools","cwd"]`.
  - Refusals:
    - a db agent (`!agent.runtime.serves_tasks()`);
    - a spawned runtime (`!agent.runtime.asks_provider()`);
    - no history store with a `runId` (503).
  - `TestPlan::of(&agent, &body, services)` reads `tools` and `cwd`. `base` is the agent's
    system prompt, or `DEFAULT_SYSTEM_PROMPT`.
  - `conversation::join(&history, joining, &name, runtime, &chat, &base)` is called **without**
    a `within` scope. The Test runs under the ambient `swiss_llm::history::context()`.
  - Busy check: `state.seat_run_of(&name)` → `refuse("busy", "this agent's test is already streaming")`.
  - Session: `state.session(&agent, "llm.test")`.
- `conversation::fold` (`crates/swiss-llm/src/conversation.rs:294`) refuses with:
  - `empty` when no turn has been answered;
  - `gone` when the run is not an open conversation of the agent.

  The new run's system is the one the old conversation opened with. For the Test that includes
  its `# Environment` snapshot, so compaction does not retake the snapshot.
- `llm_api.rs` is 3282 lines, its inline `pub(crate) mod tests` included. `llm_db/routes.rs`
  already imports `awaited_sink, bad, body_of, field, loop_answer, ok, refuse, sse_of, sse_send,
  SSE_FRAMES` from `crate::llm_api`. `TestPlan` is private to `llm_api.rs`.
- The fixtures that the existing compaction tests use (`llm_db/tests.rs:1993-2100`) show the
  test shape: a captured fake provider, `events_of`, `spoken`, and asserting the replay.
  `llm_api::tests` holds the Test stream's own fixtures
  (`a_streamed_conversation_opens_with_its_snapshot_and_replays_it`, line 1995).

**Panel**

- The Test sheet is `views/llm-agents.ts`, about lines 500–900. It has:
  - `prompt` (`#laTestPrompt`) and `queued` (a `chatQueue`);
  - `out` (`#laTestOut`) and `column()`/`addTurn()`;
  - `runId`, `gen`, `live`, `asked`, `named` and `controller`;
  - the foot `[grow, fresh, close, run, stop]`, where `fresh` is New and is shown only when `asks`
    (builtin or chat).

  The ask goes through `streamAsk("/api/llm/agents/test/stream", body, hooks, addTurn, signal)`
  (line ~853, from `views/llm-stream.ts`).
- The Data page's compaction is `data-agent.ts:1362`, `compact(talk, focus)`. It:
  1. adds a box holding `note(tr("dataAgent.compacting"), {busy: true})` and a `streamed` div;
  2. calls `streamAsk("/api/llm/db/compact", …)`, with `onRunId` taking the new id and
     cancelling it if Stop came first;
  3. on success, `fill(box, factLine([tr("llm.history.compacted"), tr("llm.history.compactedFrom", {id})]), formFold({summary: tr("llm.history.checkpoint")}, …streamed))`;
  4. otherwise, re-reads the open run, and shows a cancelled note or the error.

  Stop during compaction POSTs `/api/llm/agents/test/cancel` `{runId}`. `COMPACT_RE` is
  `/^\/compact(?:\s+([\s\S]*))?$/` (line 100). The "/" completion is `chatCommands(ta, commands)`
  in `ui/chat.ts:388`, which returns the list for the caller to place above the box.
- `factLine` is in `ui/flow.ts`, `formFold` in `ui/form.ts`, and `note` in `ui/page.ts`.
- `views/llm-agents.ts` is 1049 lines and `data-agent.ts` 1614.

## 3. Server

### 3.1 One compaction drive for both routes (refactor commit first)

Steps 5–11 of the Data page's route are the same for the Test, apart from five inputs: the
instruction, the tools, the seat key, the run scope, and the extra `run` frame fields.
Move them into a new `crates/swiss-plugins/src/llm_compact.rs`:

```rust
pub(crate) struct Compaction {
    pub state: Arc<LlmState>,
    pub seat: String,          // the seat key: the agent's name (Test) or the DB seat
    pub agent_name: String,
    pub history: Arc<LlmHistory>,
    pub scope: RunContext,     // scope(&conn) for the Data page; swiss_llm::history::context() for the Test
    pub chat: Chat,
    pub session: Session,
    pub instruction: String,
    pub tools: Vec<ToolSpec>,  // whatever type run_compaction takes today
    pub run_frame: Value,      // the frame's extra fields (conn for the Data page); runId/from/compacting are added
}
/// Fold already done by the caller; seats, frames `run`, drives, checks spoilage, undoes on failure, frames `done`, unseats.
pub(crate) fn drive(c: Compaction, fold: conversation::Fold, busy: &'static str) -> Response;
```

The exact signature is yours. The rules:

- The Data page's `compact()` calls it, and its behaviour does not change.
- The three existing DB compaction tests pass unchanged, before and after the move:
  - `a_compaction_carries_the_dialogue_on_from_its_checkpoint`;
  - `a_failed_compaction_reopens_the_old_conversation`;
  - `a_send_and_its_compaction_run_as_long_as_their_work_takes`.

  Do not edit their assertions. Run them before the move, after it, and paste both outputs in
  the report.
- `within(scope, …)` wraps every history call the drive makes (`undo_fold`), exactly as today.
- The message strings stay byte-identical. That covers the spoil messages and BUSY.

Commit: `refactor(llm): one compaction drive for the Data page and the Test`.

If the move turns out not to be clean (for example, the borrow of `run` across the pinned
future fights the struct), stop after one honest attempt. Then copy the drive into the Test's
module instead, and say so in the report under *Deviations*. Do not change `run_compaction`.

### 3.2 `POST /api/llm/agents/test/compact`

Put it in a child module of `llm_api`: `crates/swiss-plugins/src/llm_api/compact.rs`, declared
with `mod compact;` in `llm_api.rs`. A child module sees `TestPlan` without widening its
visibility. Register the route beside the other `agents/test/*` routes (`llm_api.rs:76-80`).

Body `{agent, runId, focus?, tools?, cwd?}` (`body_of` with exactly those keys):

- `agent` is required. `runId` is required: `bad("runId is required: the conversation to compact")`.
- `focus` is at most `FOCUS_MAX` characters. Move `FOCUS_MAX` into `llm_compact.rs` and use it
  from both routes, so there is one constant.
- `tools` and `cwd` are read through `TestPlan::of(&agent, &body, services)` as the stream reads
  them. The panel sends the same picks its asks send. They decide only which tool **definitions**
  are declared. None runs.

Then, in order:

1. Refuse what the stream refuses: unknown agent (400), db agent (400, the stream's sentence),
   spawned runtime (400 "a spawned runtime has no conversation to compact").
2. No history store → 503 "compaction needs the llm history store; this home has none".
3. Build `chat` with `.recording(state.history(), Some(&agent.name))` and `base` exactly as the
   stream does.
4. Busy: `state.seat_run_of(&name)` → `refuse("busy", "this agent's test is already streaming")`.
5. `conversation::fold(&history, joining, &name, runtime, &chat, &base)`, with **no** `within`
   scope, matching `join`. Its `Refused {code, message}` goes out through `refuse(code, message)`
   (409 `empty` or `gone`), and `Unavailable` goes out as 503.
6. The instruction is `TEST_COMPACT_INSTRUCTION` (§3.3), plus the focus line
   `"\n\nThe reader asks the checkpoint to keep in particular: {focus}"`. That is the Data
   page's wording, so share it: a `fn instruction(base: &str, focus: Option<&str>) -> String` in
   `llm_compact.rs`.
7. Tools: the definitions the Test would declare for this agent and these picks. A chat runtime
   declares none. A builtin declares what `TestPlan`'s `tools()` would offer, built the way
   `llm_api.rs:246` builds the specs list. If `TestTools` has a `specs()` method, use it. Serve
   none: `run_compaction` already never serves.
8. Session: `state.session(&agent, "llm.test")`.
9. Call the drive (§3.1). The seat key is `name`, the scope is `swiss_llm::history::context()`,
   and the `run` frame is `{runId, from, agent, provider, model, compacting: true}`.

Frames, as the Data page's: `run`, the checkpoint's `thinking`/`delta`, `usage`, `final`,
`done {ok, compacted, runId, from}`. `POST /api/llm/agents/test/cancel {runId: <the new id>}`
stops it, and the old conversation reopens. That needs no new code, because the cancel route
works by run id.

A Test stream and a compaction for the same agent exclude each other through the one seat.

### 3.3 `TEST_COMPACT_INSTRUCTION`

It goes in `llm_compact.rs` beside the shared helpers. It has the same frame as
`DB_COMPACT_INSTRUCTION`: the same opening paragraph, the same "Output EXACTLY…" paragraph and
"(none)" rule, and the same Rules shape. The sections are general rather than database-specific:

```
## Requests and Intent
- [what the reader asked, first to latest; quote them where the exact wording matters]

## Facts Learned
- [what the dialogue established: files and their roles, commands and what they showed, versions, paths, names]

## Work Done
- [each change made or command run that mattered, and its outcome; exact paths, commands and short code where the next step needs them]

## Errors and Fixes
- [an error met, quoted exactly, and how it was fixed]

## Pending
- [what the reader asked that is not done yet]

## Current Work
- [precisely where the dialogue stood: the last question and the answer it got]

## Critical Context
- [the reader's corrections and preferences, decisions and their reasons, constraints, open questions]
```

Rules: keep exact file paths, commands, identifiers, error strings and numbers. Quote no file
contents beyond the few lines a later step needs. Write no secret, token or key, even if one
appeared in the dialogue.

## 4. Panel (`views/llm-agents.ts`, and a new `views/llm-agents-compact.ts`)

`llm-agents.ts` already holds 1049 lines. Put the compaction flow in a new
`views/llm-agents-compact.ts`, exporting one function that the Test sheet calls with what it
needs: the column's `addTurn`, the sheet's state accessors, the `tools`/`cwd` picks and the
`runId`. Grow `llm-agents.ts` by the wiring only.

- **Entry points.** Both appear only when `asks`, so a spawned runtime has neither.
  - The `/compact [focus]` command, through `chatCommands(prompt, [{name: "compact", arg:
    tr("llm.test.cmdCompactArg"), hint: tr("llm.test.cmdCompact")}])`. Place the returned list
    above the box inside the compose area. The `COMPACT_RE` test runs on Ask before anything is
    queued or sent.
  - A ghost button **Compact** (`#laTestCompact`) in the foot, beside New, with its title
    `tr("llm.test.compactTip")`.
- **When it is allowed.** Only when a conversation is open (`runId != null`) and nothing streams
  (`!live`). Otherwise:
  - The button is disabled with its title saying why (`llm.test.compactNone` with no run,
    `llm.test.compactLive` while streaming).
  - `/compact` typed while live toasts `llm.test.compactLive` and queues nothing.
  - `/compact` with no run toasts `llm.test.compactNone`.
- **Flow.** It mirrors `data-agent.ts`'s `compact()`, steps (1)–(4) in §2:
  1. Set `live`, bump `gen`, arm Stop, and add a box holding `note(tr("llm.test.compacting"),
     {busy: true})` and a `streamed` div.
  2. Call `streamAsk("/api/llm/agents/test/compact", {agent, runId, focus?, tools, cwd}, …)`,
     with the same tools and cwd body fields that an ask on this sheet would send. Extract the
     sheet's body builder if it is inline, so both use one.
  3. In `onRunId`, take the new id. A Stop pressed before the id arrived cancels it the moment
     it does.
  4. On success, fill the box with `factLine([compacted, compactedFrom {id}])` and a
     `formFold({summary: checkpoint}, …streamed)`, and set `runId` to the new run.
  5. On cancel or error, set `runId` to `done.runId`: the old one, or null, in which case the
     next ask starts fresh. Then fill a cancelled note or the error note.
- **Stop while compacting** POSTs `/api/llm/agents/test/cancel {runId: <the new id>}`, and the
  stream is read to its end (the server reopens the old conversation first, then says so). Use
  the sheet's existing Stop button, not a second one.
- **Queue.** Words queued during a compaction wait. After a compaction that landed, the first
  queued message goes as the next ask (`sendNext()`), exactly as after an answer. After one that
  did not, they stay queued.
- **Strings.** New `llm.test.*` keys in `en/llm.ts` and `zh/llm.ts`. Reuse the existing
  `llm.history.compacted`, `llm.history.compactedFrom` and `llm.history.checkpoint`. Add:
  - `compact`, `compactTip`, `compacting`, `compactCancelled`, `compactNone`, `compactLive`;
  - `cmdCompact`, `cmdCompactArg`.

  The refusal codes `busy`, `empty` and `gone` show the server's message through the existing
  path (`streamAsk` already reads a non-2xx `{error}`).

## 5. Tests

**Rust** (`cargo test -j 2 -p swiss-plugins -- --test-threads=1 <filter>`):

- The refactor commit: the three DB compaction tests, unchanged, green before and after.
- The new tests go in `crates/swiss-plugins/src/llm_api/compact_tests.rs` (wired as a
  `#[cfg(test)] mod` of `compact`), reusing `llm_api::tests`' fixtures:
  1. `a_test_compaction_carries_the_dialogue_on_from_its_checkpoint`:
     - one Test send, then compact with a focus;
     - frames `run, delta, usage, final, done`;
     - `run.compacting` is true, and `from` is the old id;
     - the condensing ask's system is the conversation's own, snapshot included (byte-equal to
       the first ask's system);
     - its last user turn starts with `TEST_COMPACT_INSTRUCTION` and ends with the focus line;
     - the next stream send with the new `runId` replays exactly
       `[system, (user, COMPACT_FRAMING), (assistant, checkpoint), (user, next prompt)]`;
     - the new run's `source` is `compact:<from>`.
  2. `a_failed_test_compaction_reopens_the_old_conversation`: an empty checkpoint gives
     `done.compacted:false` with `runId == from`, and the next send on `from` continues it.
  3. `test_compaction_refuses_what_the_stream_refuses`:
     - a db agent, a spawned runtime, a missing `runId`, and a focus over `FOCUS_MAX` → 400;
     - a never-answered run → 409 `empty`;
     - a closed run → 409 `gone`.
  4. `a_test_compaction_and_a_test_stream_share_the_seat`: while one holds the seat, the other is
     409 `busy`.
  5. `test_compaction_declares_the_picked_tools_and_serves_none`:
     - a builtin with tools picked: the condensing ask declares exactly that list;
     - a chat agent declares none;
     - a scripted tool call in the compaction round answers as an error, and the round's cap
       ends it without a checkpoint (`compacted:false`).

**Vitest** (`--maxWorkers=2`, touched files only, `npm run check` once at the end), in
`admin-llm.test.ts` or a new `admin-llm-compact.test.ts`:

- The button is disabled with no run, and disabled while live. It is absent for a spawned runtime.
- `/compact keep the paths` POSTs `{agent, runId, focus: "keep the paths", tools, cwd}` to
  `/api/llm/agents/test/compact`, and nothing to `/stream`.
- Success renders the fact line and the Checkpoint fold, and the next ask carries the new `runId`.
- A cancel or failed outcome restores `runId` from `done`, and shows the note.
- Stop during compaction POSTs `/api/llm/agents/test/cancel` with the **new** id. A Stop pressed
  before the run frame cancels when the id arrives.
- A message queued during compaction goes after success, and stays after failure.
- `i18n-complete` and the parts test stay green.

## 6. Live proof (19998, GLM from the repo `.env`, never a local ollama)

On the 19998 test instance:

1. Make a builtin agent on the GLM provider (the repo `.env`'s key, through the vault reference
   the other live runs use).
2. In its Test sheet, ask three things that read files in a scratch `cwd`.
3. Type `/compact keep the file names`, and watch the checkpoint stream into its fold.
4. Ask a follow-up that needs a fact from before the compaction. The answer uses it.
5. Open History: the old run is closed, and the new one reads *compacted from run N*.
6. Start a second compaction and press Stop mid-stream: the old conversation goes on.

Screenshots of the streaming box, the landed fold and History. Paste the checkpoint text,
masked if anything secret-looking appears.

## 7. SPEC and docs in the same commit

- §llm.agents Compaction (`docs/spec/0013-llm.md:277`): "the Data page's today" becomes "the
  Data page's and the Agents Test's". Name the Test's instruction sections and say that the
  Test's tools are the sheet's picks, declared.
- §llm.api (the route list around `agents/test/cancel`, line ~657): the new `POST
  agents/test/compact` entry, its body, frames and refusals. The cancel line gains "and a
  compaction (by the new run id; the old one reopens)".
- The Test sheet paragraph (line ~813): the Compact button and `/compact [focus]`.
- §llm.roadmap (line ~964): remove "context **compaction** of the Agents Test's conversations
  (…)" from the not-built list.
- CHANGELOG `[Unreleased]`: one Added entry.

## 8. Delivery

Three commits:

1. `refactor(llm): one compaction drive for the Data page and the Test`, with no behaviour change.
2. `feat(llm): the Agents Test compacts its conversation (POST agents/test/compact)`, with the
   Rust tests and the SPEC route lines.
3. `feat(panel): Compact and /compact in the Agents Test sheet`, with the vitest tests, strings,
   SPEC panel lines and emit.

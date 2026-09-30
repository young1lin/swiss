---
name: swiss-debug
description: Use when diagnosing any bug, failing or flaky test, unexpected behavior, regression, or performance problem in swiss — before proposing or applying a fix.
---

# Debugging swiss

No fixes without a root-cause investigation first. The whole method is one sentence: **build a
tight feedback loop — one command that goes red on this bug — before theorizing.** Everything after
that consumes the loop.

## Phase 1 — build the loop (in this repo's order of preference)

1. **A targeted cargo test filter** at the seam that reaches the bug:
   `cargo test -p swiss-mcp <name>`. Most seams already have tests; a failing one is the loop.
2. **A one-shot integration test** through `tower::ServiceExt::oneshot` — the axum app with no
   real listen and no real sleep (SPEC §testing). This is where new seams get created.
3. **A curl/Invoke-RestMethod script against the 19998 instance** for anything that needs a real
   running gateway ([swiss-live-verify](../swiss-live-verify/SKILL.md) for the boot procedure).
4. **Replay a captured artifact** — a line from `logs/traffic.jsonl` or a
   `logs/calls/bodies/<mcp>/<seq>.txt` replayed through the code path in isolation.

Tighten the loop until it is seconds, deterministic, and asserts the *user's exact symptom* (not
"didn't crash"). For non-deterministic bugs, raise the reproduction rate (loop the trigger, stress,
narrow the window) before debugging a 1% flake. If no loop can be built, say so and stop — no
hypothesis without a red-capable command.

## Phase 2 — reproduce and minimise

Run the loop red. Confirm it produces the failure the user described, not a nearby one. Cut inputs,
callers, and config one at a time until every remaining element is load-bearing. The minimised
repro becomes the regression test.

## Phase 3 — hypothesise, ranked

Write 3–5 falsifiable hypotheses before testing any ("if X is the cause, changing Y makes it
disappear"). Check the owning SPEC section and the module's comments for the intended behavior
before ranking — a comment often records the bug that was paid for once already.
Present the list to the user — cheap checkpoint, frequent re-ranks — but don't block: proceed with
your ranking if they are away, and re-rank when they answer.

## Phase 4 — instrument, one variable at a time

- Read the error completely first; this codebase's errors usually name their owner.
- Check recent changes (`git log`, `git diff`) before blaming the platform.
- Tag every temporary log with a unique prefix (`[DEBUG-a4f2]`) so cleanup is one grep.
- Performance problems: measure first (baseline, then bisect) — logs are usually the wrong tool.
  For build speed, SPEC §arch.deps records what was measured and rejected (incremental
  release, a thin-LTO fast lane); a settled decision is not re-litigated without new numbers.
- Windows traps that masquerade as bugs: `os error 4551` (Smart App Control blocked the binary —
  it never ran; rerun), `os error 1455` (paging file exhausted by link debuginfo), asynchronous
  file-handle release making an immediate rename fail.

## Red flags — stop and return to Phase 1

| Thought | Reality |
| --- | --- |
| "I can see the problem, no need to reproduce it" | Seeing a symptom is not a root cause. Build the loop first. |
| "The operator already named the fix — just apply it" | Apply it as hypothesis #1 through the loop, never as a fix without evidence. |
| "One more small change and it'll pass" (after 2+ attempts) | Three strikes is an architecture signal (below). Stop at three; talk it through. |
| "It only fails sometimes — no loop is possible" | Raise the reproduction rate (Phase 1). Debugging a 1% flake blind costs more than the loop. |
| "I'll batch both suspected fixes and test together" | One variable at a time; a green batch teaches you nothing. |

## Phase 5 — fix at the root, with a regression test

Turn the minimised repro into a test at the **correct seam** — one that exercises the real failure
pattern as it occurs at the call site. Watch it fail, apply one fix, watch it pass, then run the
owning crate's suite ([swiss-verify](../swiss-verify/SKILL.md)). If no correct seam exists, that
itself is the finding: the architecture prevents the bug from being locked down — report it rather
than leaving a shallow test that fakes confidence.

**Three or more failed fixes is an architecture signal, not a luck problem.** Stop, question the
design, and talk it through with the user before attempt four. Never claim fixed without fresh
evidence.

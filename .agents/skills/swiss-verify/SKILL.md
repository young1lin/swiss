---
name: swiss-verify
description: Use when selecting or running build, test, or clippy evidence for a change in this repo — before claiming a change compiles or passes, before committing, after a fix, or whenever a cargo command must be chosen for a swiss diff.
---

# swiss verification gates

Evidence before claims. A status is proven by a fresh command run in the current task, never by a
previous run, a green adjacent check, or confidence. Report every verdict as the exact command plus
what its output actually said.

## The command inventory

```powershell
cargo build --release                                  # the shipping exe
cargo test --workspace                                 # the one feature combination there is
cargo clippy --workspace --all-targets -- -D warnings  # must be clean
cargo tree -d                                          # duplicated TLS/runtime must fail review
```

**`--workspace` is not optional.** Without it cargo selects the root package alone — about a
fifth of the suite — and none of the eight member crates' tests (the large majority; see the
workspace members in the root Cargo.toml) are ever compiled or run, yet the run still reports ok.
The same applies to clippy. Any full-suite claim made without `--workspace` is false by
construction. (AGENTS.md states this by shape, not counts — counts rot, the shape doesn't.)

## Select the narrowest honest evidence

Do not reflexively run the full suite. Pick the smallest command that can go red for the change:

- **A crate-local change:** `cargo check -p swiss-mcp` as the fast compile probe, then
  `cargo test -p swiss-mcp` and `cargo clippy -p swiss-mcp --all-targets -- -D warnings`.
- **One behavior:** `cargo test -p swiss-host <test_name_substring>`. Most tests live in
  `mod tests` inside `src/`; integration tests drive the axum app through
  `tower::ServiceExt::oneshot` — no real listen, no real sleep (a few integration tests do use
  real loopback sockets by design). Never add a sleep to make one pass.
- **A behavior change ships with a test that fails before and passes after.** Watch it fail (RED)
  before writing the fix, then green. A test written after the fact and never seen red is not
  evidence.
- **Cross-cutting changes** (host contract, config, secure store, panel wiring, workspace
  manifests): that is what the full `cargo test --workspace` rehearsal is for.

## Know the environment rules

- **Sealing tests never spawn an OS keystore helper.** Unit tests inject key material directly;
  any test that boots the real key path pins the env override instead — `SWISS_MASTER_KEY` (new
  name, checked first) or `MCP_GATEWAY_MASTER_KEY` — which bypasses every OS key source
  (`crates/swiss-core/src/secure/key.rs`).
- **DB tests self-skip without credentials** (`direct-adapters`, `dbbrowser`, `sql`,
  `db-resources`). A skip is not a pass: name what was skipped and why when reporting.
- **Windows traps that look like test bugs but are not** (full list in AGENTS.md):
  `os error 4551` (Smart App Control blocked the binary — it never ran; re-running usually gets
  past it) and `os error 1455` (paging file exhausted by link debuginfo — a build-environment
  condition, not a test failure).

## Red flags — the claim is not yet supported

| Thought | Reality |
| --- | --- |
| "Should pass now" / "probably fine" | No evidence. Run the command; quote its output. |
| "The suite passed before the refactor" | A green run proves only the tree it ran on. Re-run. |
| "It's just a refactor, no behavior change" | Then the full run is cheap insurance. Run it. |
| Editing a test to make it pass (weaker assert, wider tolerance) | You are hiding the failure, not fixing it. Decide which side is wrong — out loud — before touching either. |
| "The failure is flaky / pre-existing, not mine" | Prove it on the pre-change tree (stash; run; restore). Then it is a bug to report, not a check to bypass. |
| "It worked when I ran it by hand" | A manual run is not evidence a reviewer can re-run. Put it in a test. |

## Context between phases

Phases: understand → change → verify → live-verify → review → commit → deploy. At each boundary
pick the cheapest move that keeps the reasoning the next phase needs:

- **Understand → change → verify: continue.** The Node reasoning and the witnessed RED test
  belong to the window that writes the fix; don't compact between them.
- **Verify → live-verify: subagent when it fits, else continue.** The raw HTTP/browser transcript
  is noise to everything after it — carry the evidence (commands, responses, build hash), not the
  scrollback.
- **Review: fresh eyes beat author eyes.** When subagents are available, run the swiss-review
  audit in one and apply its findings back here with full context.
- **Commit → deploy: start clean.** Deployment is an operator checklist, not the tail of an
  iteration session.

## After the evidence is green

- User-visible behavior (panel, API shape, MCP, tunnels, terminal):
  [swiss-live-verify](../swiss-live-verify/SKILL.md) on the 19998 instance.
- The change touched an adapter, plugin, payload path, or dependency:
  [swiss-memory-record](../swiss-memory-record/SKILL.md) for the memory number.
- Before committing: [swiss-review](../swiss-review/SKILL.md).
- Deployment to 19999 is owned by [swiss-deploy](../swiss-deploy/SKILL.md) and never part of
  iterating on a change.

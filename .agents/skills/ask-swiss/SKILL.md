---
name: ask-swiss
description: Router over the swiss skill suite — ask which skill or flow fits a development situation in this repo.
disable-model-invocation: true
---

# Ask swiss

You don't remember every skill, so ask. This maps a situation to the skills that own it. The
project-specific skills are model-invoked and fire on their own when their triggers match; this
router exists for the human who wants to steer deliberately.

## The main flow: change → ship

The route most work travels:

1. **Make the change** — work from the capability's docs/NN spec when one exists. Porting or
   repairing against the Node reference? That's **swiss-node-reference**: read the original module
   and its vitest file first, port the reason, port the tests. New capability? If none exists and
   it won't fit one sitting, **swiss-spec** first (interview it into a spec with acceptance tests
   and a hand-off prompt); the implementation itself is **swiss-add-plugin** (descriptor/action/
   page through the host contract, never a host match arm — and a plan named before any code).
2. **swiss-verify** — select and run the narrowest honest cargo evidence. `--workspace` when the
   change is cross-cutting; a targeted `-p <crate> <filter>` when it is not. A behavior change
   ships with a test that failed before and passes after.
3. **swiss-live-verify** (user-visible changes — when in doubt, it is user-visible) — a real
   gateway on the isolated 19998 instance, never by touching production 19999.
4. **swiss-review** — audit the diff against the load-bearing rules (loopback, credentials,
   frozen envelope, `&RawValue`, `current_thread`, panel byte-for-byte, dependency weight).
5. **Commit**, and stop here by default.
6. **swiss-deploy** — only when the operator explicitly asks; `scripts/deploy.ps1` owns the order
   and proves the served build hash.

## On-ramps

- **An idea, not yet a change** — "what if swiss could …" → **swiss-spec**: interview it into a
  numbered docs/ spec with acceptance tests and a hand-off prompt before any code. Straight to
  swiss-add-plugin only when it fits one sitting and one commit.
- **Something's broken** — a failing test, a flake, a regression → **swiss-debug** (tight feedback
  loop first; the Node build is the ground-truth oracle). A "why is it built like that" question
  with nothing broken is **swiss-node-reference**, not debug.
- **A dependency question** — "should we pull in crate X", any `Cargo.toml` edit →
  **swiss-dependency-review** (`default-features = false`, no second TLS/runtime, `cargo tree -d`).
- **"How much memory does swiss use" / could this change move idle cost** → **swiss-memory-record**
  (measure on 19998, append a dated row to docs/01 — records, not gates).
- **Anything the panel shows** — a new page, list, sheet, row, icon, colour, spacing, or a spec
  with a visual side → **swiss-design** first: the design language, the component vocabulary and
  the pre-copy checklist. The panel change itself still ships through swiss-node-reference's
  edit-sibling-then-copy loop and swiss-live-verify on 19998.
- **"Make it smaller / faster"** → **swiss-debug**'s measure-first discipline (baseline, then
  bisect) with docs/01 as the scoreboard — and AGENTS.md's measured-and-rejected record is
  standing law: no change ships a number it didn't measure.

## Vocabulary underneath

AGENTS.md — the four product properties and every load-bearing rule — outranks everything here and
is present in every session; the docs/ tree owns the contracts (09 plugins, 10 jobs, 05 wire).
No generic craft layer is installed in this environment — the disciplines ride inside the project
skills: evidence and RED-before-green in **swiss-verify**, root-cause method in **swiss-debug**,
audit posture in **swiss-review**. When a discipline feels missing, strengthen those skills; do
not assume an external suite provides it.

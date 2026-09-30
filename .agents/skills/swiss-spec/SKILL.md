---
name: swiss-spec
description: Turn an idea into an amendment of docs/SPEC.md, the one living swiss specification — interview, amend the owning section in place, acceptance tests, delivery order; a decision worth recording becomes an ADR entry in SPEC §decisions.
disable-model-invocation: true
---

# From idea to a SPEC amendment

swiss has exactly one specification, `docs/SPEC.md`: what the system does **now**, organised by
area, with the decision log as its last section (SPEC §about). There are no numbered spec files
and no hand-off prompts. A new capability or a behaviour change is argued and written into the
section it touches first; the code then lands against that text, and each commit carries the
slice of the amendment it makes true.

Skip this skill when the change fits one sitting and moves no documented behaviour — go straight
to swiss-add-plugin (or just fix it), and still amend the sentence in SPEC in the same commit if
one moved.

## 1. Interview before writing

- Work in rounds: ask every frontier question (each one whose prerequisites are settled) in one
  numbered round, each with your recommended answer; wait; recompute the frontier. Don't block on
  an absent user — proceed with your recommendations and re-rank when they answer.
- Facts are your job, decisions are the user's. Before asking anything you could look up, read
  AGENTS.md, the owning SPEC section (`grep -n '^##' docs/SPEC.md` is the map), SPEC §decisions
  for ADRs already made in this territory, SPEC §arch.crates for where the code lives, and the
  owning crate itself.
- The user's requirement, verbatim with its date and follow-up confirmations, goes into the
  commit message that lands the amendment — SPEC states the resulting rule, not who asked when.

## 2. Check the idea against the constitution

- The four product properties (SPEC §product.properties, AGENTS.md): a change that trades one
  away for convenience is the wrong change. Say which properties the idea pulls on; stop here if
  one breaks.
- The load-bearing rules of the area; SPEC §formats if anything on disk or on the wire moves;
  SPEC §security if the boundary moves; SPEC §product.memory if idle cost can move.

## 3. Amend the section in place

- Find the owner. A new facet of an area is a new `### §area.sub — Title` under it; a new plugin
  is a new `## §name — Title` plus a row in the §about contents table.
  Never open a parallel document.
- Write the behaviour as it will be, in the present tense: routes, fields, files, limits, errors,
  what the panel shows. English, like every code comment.
- Replace what the change contradicts; delete the old wording rather than striking it through.
  No status headers, no "phase"/"stage"/"rev" labels, no dates except in records — history is
  git's job.
- *Must*, *never* and *always* need an enforcing test; name it in the text or in §4's list.
- An anchor is a contract: renaming one rewrites every citation in the same commit
  (`git grep 'SPEC §old'`). Cite neighbours as `§area.sub` inside SPEC and as `SPEC §area.sub`
  from code, tests and skills.
- Say what is out of scope only where the boundary would surprise a reader.

## 4. Acceptance tests

- Acceptance tests are code, not prose. For each amended subsection list them up front: the test
  name, its file, and what fails before the change and passes after. Write each test first.
- Each test cites its rule in a comment (`// SPEC §area.sub: …`).
- What only a live walk can prove (a real browser, a real remote host) goes into the owning
  section's verification list — §terminal.verify is the shape — and is walked on 19998.

## 5. Delivery order

- Present the SPEC diff, the acceptance list and the delivery order together: one commit per
  item, dependencies stated, each commit carrying its code, its tests and its slice of the SPEC
  amendment. SPEC never describes behaviour the tree at that commit does not have.
- Gates for every item: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo tree -d` when dependencies move, `npm run check` when the panel moves.
- If a fresh session will implement it, the SPEC diff plus this delivery order is the brief —
  no prompt file.

## 6. Record a decision when the test is met

Hard to reverse + surprising without context + a real trade-off → a new `### ADR-NNN — Title`
at the end of SPEC §decisions, numbered after the last one (`grep '^### ADR-' docs/SPEC.md |
tail -1`). The entry states the decision in force, what it cost and what was rejected. Replacing
an earlier decision shrinks that entry to one paragraph naming its successor; numbers are never
reused. Skip the ADR if any of the three is missing — the section text is enough.

## 7. Then build it

The implementation runs through swiss-add-plugin (a new plugin or tool) or directly, then the
swiss-verify → swiss-live-verify → swiss-review flow. A review that finds code and SPEC
disagreeing fixes whichever side is wrong in the same change.

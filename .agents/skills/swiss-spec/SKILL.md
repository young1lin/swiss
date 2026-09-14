---
name: swiss-spec
description: Sharpen an idea into a numbered docs/ spec for swiss — verbatim requirement, decisions, per-section acceptance tests, delivery order, and a hand-off prompt for the implementing session.
disable-model-invocation: true
---

# From idea to docs/NN spec

New capability in this repo is spec-first: docs/15 through docs/19 were each written, argued, and
revised before implementation started, and 15/16 shipped with a companion hand-off prompt
(docs/16-operations-hardening-prompt.md). This skill runs that flow. Skip it when the change fits
one sitting and one commit — go straight to swiss-add-plugin or swiss-node-reference.

## 1. Interview before writing

- Work in rounds: ask every frontier question (each one whose prerequisites are settled) in one
  numbered round, each with your recommended answer; wait; recompute the frontier. Don't block on
  an absent user — proceed with your recommendations and re-rank when they answer.
- Facts are your job, decisions are the user's: read AGENTS.md, docs/07 (ADRs already made in this
  territory), docs/04 (does a Node counterpart exist?), the Node build, and the owning crate before
  asking anything you could look up yourself.
- Record the user's requirement verbatim in the spec header, with date and follow-up
  confirmations — the docs/19 header pattern.

## 2. Check the idea against the constitution before writing

- The four product properties (AGENTS.md): a change that trades one away for convenience is the
  wrong change. Say which properties this idea pulls on; stop here if one breaks.
- Load-bearing rules that touch the area; docs/05 if anything on disk or on the wire moves.

## 3. Write docs/NN-<slug>-spec.md in the house shape

Match neighboring specs' prose language; code comments and UI copy stay English.

- Status header: state + baseline commit (the docs/09 line-3 pattern).
- The verbatim requirement quote.
- §0 current state and gap — why this, why now, with evidence.
- One section per piece, each carrying its own acceptance tests (test-first per item, as docs/16
  did per H-item); a behavior change ships with a test that fails before and passes after.
- Gate commands block (cargo test --workspace / clippy -D warnings / cargo tree -d) and delivery
  order: one commit per item, dependencies stated.
- Out of scope, explicit. Add the row to README's docs table.

## 4. Offer an ADR when the test is met

Hard to reverse + surprising without context + a real trade-off → an entry in docs/07 (options
table, recommendation, what shipped, cost). ADR-013 is the recent precedent. Skip if any of the
three is missing.

## 5. Write the hand-off prompt, then stop

- docs/NN-prompt.md (the docs/16 pattern): a self-contained prompt for a FRESH implementing
  session — working dir, the sibling Node repo, task summary, files to read first, delivery order,
  gates, commit rules.
- The session that wrote the spec does not implement it. Implementation starts fresh from the
  prompt + spec via swiss-add-plugin / swiss-node-reference, then the
  swiss-verify → swiss-live-verify → swiss-review flow.

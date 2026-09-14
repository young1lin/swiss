---
name: swiss-memory-record
description: Use when a swiss change could move the process's idle memory cost — after touching an adapter, plugin, payload path, or dependency, or when asked how much memory swiss uses — to measure the running gateway on the isolated 19998 instance and record the dated result in docs/01.
---

# Recording the memory number

Memory is why this repo exists (docs/01), and the user's 2026-09-11 call made the numbers
**records, not gates**: no band is enforced anywhere, and a debug build costing more is fine.
This ritual keeps the record honest without ever gating a change on the number.

## What measures what

- `tests/memory.rs` — the suite's delta guard: serving must not GROW the footprint. It cannot
  assert an absolute ceiling (the test binary carries the harness and dev-dependencies; see its
  own header). Green is necessary, never sufficient.
- The shipped number — measured from the real binary against real state, below. This is the
  figure docs/01 records. `gatewayMb` is the working set; the heap fields carry the private-commit
  figure; `childrenMb`/`processCount` name the proc children (crates/swiss-host/src/mem.rs).

## The measurement

`scripts/measure.ps1` (Windows) / `scripts/measure.sh` (macOS/Linux) in this skill run the
whole loop:

```powershell
& .agents\skills\swiss-memory-record\scripts\measure.ps1            # build + boot + measure
& .agents\skills\swiss-memory-record\scripts\measure.ps1 -SkipBuild # reuse target-test exe
```

```bash
bash .agents/skills/swiss-memory-record/scripts/measure.sh              # build + boot + measure
bash .agents/skills/swiss-memory-record/scripts/measure.sh --skip-build # reuse target-test exe
```

It builds into `target-test`, boots the isolated 19998 instance on a REAL-state snapshot (the
workload is the point — never 19999), pins the token per
[swiss-live-verify](../swiss-live-verify/SKILL.md), reads `/health` (build hash) and
`/api/memory`, and prints a ready-to-append docs/01 row next to the current last row for the
delta. Let proc MCPs go idle before reading the idle number and note which children were awake.

## The record

Append to docs/01's "Measured result" section — never overwrite a dated row — with date, build
hash, workload (which MCPs/tunnels were up), gatewayMb + private figure, and the delta vs the
previous row. If the number moved and you cannot name the slice, that is a
[swiss-debug](../swiss-debug/SKILL.md) finding, not a footnote.

## Reporting

State the number, the workload, and the previous record together. Never say "memory is fine"
without a fresh measurement; never block a change on the number — record it and name the cost.

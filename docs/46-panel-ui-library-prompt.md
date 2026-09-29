# 46 — hand-off prompt: the panel UI library and direction B

Self-contained brief for a session that continues docs/46. The owner asked the session that wrote
the spec to implement it too ("没问题，写 spec 后，review 一下，有问题就改，没问题就直接干了"),
so this file doubles as the resume note after a context compaction.

## Where

- Worktree `.agents/worktrees/panel-ui`, branch `panel-ui` (from master `db36e95`). Never work in
  the main checkout; never touch the `mcp` or `review-fixes` worktrees.
- Panel sources `crates/swiss-panel/panel/src/*.ts`; emit `crates/swiss-panel/src/admin_assets/js`
  (committed, produced by `npm run build`, never hand-edited); CSS `admin_assets/styles/*.css`.

## Read first

1. `docs/46-panel-ui-library-spec.md` — the spec. §1.3 decisions U1–U17, §2 component shapes,
   §3 per-page acceptance, §4 gates G1–G7, §5 delivery order, §9 review record.
2. `.agents/skills/swiss-ui-design/SKILL.md` — the constitution P1c rewrites.
3. `.agents/rules/panel-proof-of-life.md` — what "done" means for a panel change.
4. `docs/assets/46/directions.html` — the approved visual target (preset B).
5. `test/non-null-ratchet.test.ts` — the ratchet idiom every G-gate copies.

## Order

P1a-1 (pure CSS move, pixel-identical) → P1a-2 (tokens + global B + G2/G3/G4/G7) → P1b (ui/
components + gallery + scenes + G1/G5/G6) → P1c (skill + style-design.md) → P2 MCP Servers →
P3 Traffic + Token → P4 Tunnels → P5 Settings → P6 Jobs + Remote → P7 Data → P8 Terminal → P9
closeout. Library first (U17): no page migrates before P1b's gallery walk passes. A shape missing
during a migration goes into the library first.

## Gates, every step

```
cd crates/swiss-panel/panel && npm run check        # includes G1–G7
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Live walk on 19998 (never 19999): `npm run build`, `touch crates/swiss-panel/src/lib.rs`,
`CARGO_TARGET_DIR=target-test cargo build --release`, `scripts/test-instance.ps1 -Stop` then
`-Fresh`; agent-browser with real pointer events, cold load, 1440 and 960, light and dark,
English and Chinese. Anything that cannot be walked is reported as NOT verified with the reason.

## Commit rules

- One commit per step in §5 (P1a-1, P1a-2, …), subject `panel: <what> (docs/46 P<n>)`; the body
  lists the walk that was done. Identity is the repo-local one (`young1lin`); **no
  Co-Authored-By trailer** - the owner's rule since the 2026-09-21 history rewrite ("作者只能是我").
- A commit that lowers a ratchet lowers its frozen table in the same commit.
- Every visible string through `tr()`, en and zh together.
- Do not push, do not merge to master, do not deploy — those are the owner's calls.

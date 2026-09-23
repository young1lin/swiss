# Fix Plan (disposition of audit exceptions)

> Principle: deliberate asymmetries are not fixed (to prevent mistaken fixes); the panel side is edited IN THIS REPO —
> ADR-024 (docs/07, docs/36) moved the panel's source of truth to `crates/swiss-panel/panel/src/*.ts` (TypeScript,
> ts-blank-space-erased line-for-line, the emit committed under `crates/swiss-panel/src/admin_assets/js`): fix there,
> run `npm run check` in `crates/swiss-panel/panel/` (typecheck ×2 + eslint + emit-freshness + vitest), then `npm run build`
> to emit the committed JS; `crates/swiss-panel/src/admin_assets` is never hand-edited; every fix must carry an acceptance criterion.
>
> **Three audit facts corrected by this item-by-item re-verification** (see the corresponding entries):
> 1. mcp.md's claim that "the make_adapter main path still uses lenient resolve_def" is stale — the `make_adapter` at
>    `crates/swiss-mcp/src/adapters/mod.rs:297` already uses `resolve_def_checked`; the lenient chain has no caller left
>    on production paths, so the closure is downgraded from "migration" to "delete dead code + fix comments" (#10).
> 2. panel.md's "↑/↓ (sort direction)" actually lives at `data-view.js:261`, written as `\u2191/\u2193` escapes and hard
>    to catch with a literal grep; the header sort arrow `.db-sort` is already a CSS triangle (views.css:449-452), no fix
>    needed (noted inside #14, to prevent over-fixing).
> 3. Same-class glyphs the audit did not name, found during verification: the 📁/📄 at `tunnel-sheets.js:100-101` (key
>    browser directory/file icons), folded into #14's V2 cleanup; body-text status characters (✓/✗/→/·) are typography,
>    not icons, and are explicitly excluded.
>
> **Superseded precondition**: the Node checkout is no longer needed or consulted for panel work — ADR-024 moved the
> panel's source into this repo (`crates/swiss-panel/panel/src`), and the byte-guard test
> `the_tree_is_byte_for_byte_the_node_builds` no longer exists at all (ADR-025 retired byte-equality in favor of the
> `npm run check` suite as the panel's gate). Batch 3 is no longer blocked on anything external.

## Closure (2026-09-23, release-audit pass)

Every actionable item is landed; what remains is listed in the Won't-Fix List below by design.

| # | Landed as | Commit |
| --- | --- | --- |
| 1-#5,#7,#8,#11 | comments/docs batch | `b9c2cb0` |
| 9 | TrafficLog instantiation + isolation test | `187ff25` |
| 12,#14,#16,#17 | panel sheet/sprite/storage batch | `cd70fb9` + `8f182bd` |
| 14 (tail) | find-bar chevrons + vendor shim headers | `e6f9da2` |
| 10 | resolve_def chain deleted, comments/docs repointed | this pass (`post-1807005`) |
| 6,#13,#15 | already done earlier (status lines / TS rewrite / V5) | see items |
| 18-#27 | won't fix by design | see list below |

#24 stays the one owner decision (enforce `retention.maxHistoryBytes` or mark advisory).

## Overview Table

| # | Problem | Location (file:line) | Category | Batch |
| --- | --- | --- | --- | --- |
| 1 | mem.rs comment says `/api/mem`; it is actually `/api/memory` | crates/swiss-host/src/mem.rs:18,53 (cross-check adminapi.rs:1035, daemon.rs:611,758) | this-repo comment | 1 |
| 2 | plugins/mod.rs comment points at the no-longer-existing `src/host` | src/plugins/mod.rs:3-6 | this-repo comment | 1 |
| 3 | the pg_browser.rs module header says edit/CSV/DDL are "not available in this build"; the implementation is all there | crates/swiss-mcp/src/adapters/pg_browser.rs:1-5 (implementation 338-531) | this-repo comment | 1 |
| 4 | the server-side `with_explain` copy has no production caller; its comment does not say "test anchoring + panel mirror only" | crates/swiss-host/src/dbbrowser.rs:1235-1256 (mirror data-sql.js:141-146; tests 2065-2070) | this-repo comment | 1 |
| 5 | the docs/17 status line still says "to do" | docs/17-panel-design-canvas-spec.md:3 | docs document | 1 |
| 6 | the docs/18 status line still says "to do" (V1-V7 have landed) | docs/18-panel-visual-refresh-spec.md:3 | docs document | 1 — **done** (docs/18:3 now reads "已实施（V1–V7）") |
| 7 | the docs/13 §1 current-state snapshot is stale (mcp two pages, seven level-1 tiles) | docs/13-panel-navigation-spec.md:31-52 | docs document | 1 |
| 8 | the docs/09 state machine still draws WaitingDependency (the implementation is a requires/requiresMet projection) | docs/09-toolbox-plugin-architecture.md:111-126 (implementation descriptor.rs:78-95, engine.rs:478-485) | docs document | 1 |
| 9 | the traffic.rs ring state is still a process-level OnceLock singleton (CallLog is already instantiated) | crates/swiss-mcp/src/traffic.rs:63-113 (cross-check calls.rs:98-117, app.rs:61-64) | this-repo code | 2 |
| 10 | resolve_def lenient/strict coexist: the lenient chain is now dead code; comments and docs/19 lag | crates/swiss-host/src/config.rs:76-126 (production already goes all through checked) | this-repo code | 2 |
| 11 | the terminal.rs comment references the unimplemented waitingDependency state | src/plugins/terminal.rs:137-139 | this-repo comment | 2 (pending #8's verdict) |
| 12 | the data-view header comment says "default 500"; the code serves 50, the server BROWSE_DEFAULT_PAGE=50 | crates/swiss-panel/panel/src/data-view.ts:51,57 (cross-check dbbrowser.rs:218) | panel side (this repo) | 3 |
| 13 | the util.js state.view comment lists only five views | superseded: the TS rewrite removed that comment (panel/src/util.ts has no state.view enumeration) | panel side (this repo) | 3 — **done by the rewrite** |
| 14 | V2 Unicode glyph leftovers: ⚙/⚿/✕/↩/↺/↑↓/▾/↑Up/📁/📄 | crates/swiss-panel/panel/src: data-grid.ts:880,974,1019,1029;data-filters.ts:170;data-browsers.ts:359,469-480;run-history.ts:44;tunnel-sheets.ts:193,196 (+ the ↑↓/▾↑Up copies in locales/en.ts+zh.ts — copy, not icons) | panel side (this repo) | 3 |
| 15 | V5 red-button-into-menu not covered: Tokens Revoke, Secrets Delete | crates/swiss-panel/panel/src/views/tokens.ts:346-349;views/secrets.ts:280-281 | panel side (this repo) | 3 — **done** (both already ride the ⋯ popupMenu with danger items) |
| 16 | three renames, three interactions (prompt ×2, sheet ×1) | crates/swiss-panel/panel/src: detail.ts:68-72;data-edit.ts:74-77 (prompt);add-sheet.ts openGroupSheet (the sheet) | panel side (this repo) | 3 |
| 17 | two generations of localStorage key names coexist (swiss.* and swiss_*) | crates/swiss-panel/panel/src: util.ts:24;data-view.ts:58;data-grid.ts:45;data-sql.ts:405 | panel side (this repo) | 3 (low priority) |
| 18 | /api/tokens/{id}/secret can be read back in plaintext | src/adminapi.rs:428-444 | won't fix | — |
| 19 | CLI open uses `cmd /c start` | src/cli.rs:687-711 | won't fix | — |
| 20 | local terminal sessions have no cap | crates/swiss-terminal/src/terminal/session.rs:606-614 | won't fix | — |
| 21 | process.legacy-command lenient refs coexist with process.exec strict | crates/swiss-host/src/services/actions.rs:9-16,193,269 | won't fix | — |
| 22 | tunnels replicates JS Number() lenient coercion (+ the declared expect in new_id) | crates/swiss-tunnels/src/tunnel/types.rs:14-61,66-77 | won't fix | — |
| 23 | with_explain keeps both copies (only #4's comment clarification) | dbbrowser.rs:2785 + panel/src/data-sql.ts:334 (dbWithExplain) | won't fix | — |
| 24 | retention.maxHistoryBytes parsed/stored but not enforced across files | crates/swiss-jobs/src/jobs/runlog.rs:59-63 | won't fix | — |
| 25 | jobs.js boot probe (a 404 hides the tab) | Node-era paths; the panel source is now panel/src/views/jobs.ts | won't fix | — |
| 26 | the sidebar mechanism is coupled to the MCP-list content | docs/13:347-352; js/sidebar.js | won't fix | — |
| 27 | the other declared small exceptions (merged: wide routes / unsafe / copy / caching / ordering, 16 items) | see the item-by-item list at the end of the Won't-Fix List | won't fix | — |

## Batch 1: Zero risk — stale comments and doc status lines

### 1.1 The /api/mem comment in mem.rs (#1)

- **Status**: `mem.rs:18` says "The JSON the admin API answers for `/api/mem` and `/api/mem?tree=1`", and `mem.rs:53` says "(and `/api/mem?tree=1`)". The actual route is `/api/memory` (registered at adminapi.rs:1035; called as `/api/memory?tree=1` at daemon.rs:611,758); a repo-wide grep finds no code reference to `/api/mem` (non-memory) — a Node-era leftover sentence.
- **Fix**: both `/api/mem` → `/api/memory`. Pure `//` comments, no behavior.
- **Acceptance**: `grep -rn "api/mem[^o]" crates/ src/` zero hits; `cargo test --workspace` green as usual (comment changes must still compile, to guard against doc-comment breakage).

### 1.2 The src/host comment in plugins/mod.rs (#2)

- **Status**: `src/plugins/mod.rs:3-6` says "The host and the built-ins live in src/host … exactly one register line in server.rs per plugin". After the P5 workspace split: the host lives in `crates/swiss-host/src/host` (the src/host directory no longer exists; glob verification came back empty), the registration table of boot built-in plugins is `register_all` at `src/builtin.rs:66-86`; the terminal in this directory does get wired in through one register line at `src/server.rs:230`. Two of the three sentences are stale.
- **Fix**: rewrite that module-header comment: the host and the boot built-ins live in `crates/swiss-host` and `src/builtin.rs` (register_all); what this directory demonstrates is the other half — tools that are not boot built-ins get in through the PUBLIC contract (descriptor/action/page), the terminal through one register line in server.rs. English comment, stating the "why".
- **Acceptance**: `src/host` no longer appears in the comment; `cargo test --workspace` green.

### 1.3 The pg_browser.rs module header contradicts the implementation (#3)

- **Status**: the module header (pg_browser.rs:1-5) claims this build only has "list/read/describe plus the read-only console; the buffered-edit grid, CSV export/import and DDL ops answer 'not available in this build'". Yet the same file fully implements `apply_edits` (338), `export_table` (381), `import_table` (434), `ddl_op` (501) — the comment is a mid-port leftover, directly contradicting the implementation and misleading later readers into thinking the capability is missing.
- **Fix**: rewrite the module header as a capability statement aligned with mysql_browser: list/read/describe, read-only console, buffered edit grid, CSV export/import, DDL ops; keep the one-line lineage statement (port of dbBrowser() in Node's pg adapter).
- **Acceptance**: the module header no longer contains "not available in this build"; pg_browser.rs's existing tests (`#[cfg(test)]` from 534 on) stay untouched and green.

### 1.4 Clarifying the server-side with_explain comment (#4, paired with won't-fix #23)

- **Status**: the `with_explain` at `dbbrowser.rs:2785` is referenced on the Rust side only by its own tests (5263-5268); the EXPLAIN prefix is actually added by the panel client (data-sql.js:143-146 `dbWithExplain`, whose comment says "Mirrored here because the server helper is TypeScript" — in the swiss context the server is Rust, so that sentence too is a Node-era leftover). The two implementations agree semantically (idempotent, stripping a single trailing terminator), but the server copy never spells out "why it exists, who uses it".
- **Fix**: change only the doc comment at dbbrowser.rs (above `with_explain`, now around :2770): state that this function is not on any production path — the console's plan view gets its prefix added by the panel; this function is the semantic spec and the test anchor, pinning the idempotency / terminator-stripping rules and keeping the two sides from drifting. No code change, no deletion (see #23's argument in the Won't-Fix List).
- **Acceptance**: the comment contains "not on any production path" and a pointer to the data-sql.js mirror; the `with_explain_prefixes_once_and_strips_the_terminator` test keeps passing.

### 1.5 The docs/17 status line (#5)

- **Status**: `docs/17:3` has status "to do" and defines the deliverable as a /design canvas; but docs/18 §2 is specific enough that implementation followed 18 directly (V1-V7 are in the asset tree, see #6's evidence), so the canvas step was in effect skipped, with no URL backfill.
- **Fix**: change the status line to the truth: "the design canvas was not produced separately; the visual refresh was implemented directly per docs/18 §2 (V1-V7 landing evidence and leftovers: see the docs/18 status line and .agents/docs/fix-plan.md)". Do not fabricate canvas history.
- **Acceptance**: docs/17 no longer carries a "to do" status line; the rest of the body is untouched.

### 1.6 The docs/18 status line (#6) — DONE

- **Evidence**: `docs/18:3` now reads "已实施（V1–V7，`b7131aa` 起）" — the status line no longer says "to do"; the original count of "(docs/18 Vx)" markers belongs to the retired hand-written-JS asset tree and is not re-verifiable after the TS rewrite.
- ~~**Status**: `docs/18:3` has status "to do"; meanwhile the asset tree carries 43 "(docs/18 Vx)" markers (index.html:2, base.css:10, views.css:9, views/plugins.js:4, polling.js:3, util.js:3, jobs.js:2, menu.js:2, tunnels.js:2, one each elsewhere), covering V1-V7 (sprite, token, left-stick, one primary + one ⋯, monochrome tags, dot-title, emptyHtml).
- ~~**Fix**: change the status line to "implemented" (with the completion-baseline commit, filled in by the executor); append one line of known-leftover pointers: the uncovered V2 glyph and V5 red-button points are governed by this file's Batch 3.
- ~~**Acceptance**: the docs/18 status line no longer says "to do"~~ (met; the asset-marker spot-check targets belong to the retired JS tree — the TS rewrite's equivalents live under panel/src).

### 1.7 The docs/13 §1 current-state snapshot (#7)

- **Status**: §1 at `docs/13:31-52` says level 1 is seven flat tiles and mcp contributes the two pages mcps/traffic. The current state (builtin.rs:108-116, plugins/terminal.rs:118-132, page-registry.js:5-20): level 1 is six groups (MCP, Tunnels, Data, Jobs, Terminal, Gateway), the MCP group has three pages (Servers 10 sidebar / Traffic 20 / Token 30), the Gateway group's two pages are synthesized by the frontend (Plugins 1000 / Secrets 1001), and process has no pages. The document header's "Status: implemented" is right; what is broken is the in-body snapshot.
- **Fix**: rewrite §1's tab rows and contribution table per the table above, adding a line saying "snapshot date + /api/plugins and builtin.rs are authoritative"; §7 "what we do not do" stays untouched (it is #26's provenance).
- **Acceptance**: the table has `tokens` (order 30) and `secrets` (1001) rows; a `grep -n` for the "seven tiles" phrasing in docs/13 returns zero hits (or that sentence has been rewritten as historical background and marked stale).

### 1.8 Adding as-built to the docs/09 state-machine section (#8)

- **Status**: the state machine at `docs/09:111-118` draws `WaitingDependency`, and line 120 requires the enum to distinguish "dependency missing"; the implementation (descriptor.rs:78-95) is Disabled/Idle/Starting/Active/Stopping/Failed, with dependencies as a `requires/requiresMet` data projection (engine.rs:478-485; the panel's plugins.js:47 renders "needs connection-catalog (no provider)" from it). The design language ran ahead of the code, which is why the terminal.rs:137 comment references a state that does not exist (#11).
- **Fix**: append an as-built subsection to the states-and-updates section: list the six-state enum; `WaitingDependency` is unimplemented, replaced by the requires/requiresMet projection, with the reasoning (a dependency is a statement of data, not a lifecycle park; a capability being withdrawn reaches the projection through register/unregister events and does not lock the state machine); note that the panel's legacy status-string table (`waitingDependency` at page-registry.js:36) is an old-gateway compatibility leftover. Keep the original diagram at 114-118 as design history, annotated "not implemented as drawn".
- **Acceptance**: an as-built enum and a projection explanation appear in that docs/09 section; every `grep -n "WaitingDependency" docs/09` hit carries an "unimplemented / replaced by the projection" qualifier.

## Batch 2: Code fixes

### 2.1 traffic.rs OnceLock instantiation (#9)

- **Status and root cause**: traffic's ring state is three process-level OnceLocks (`state()` traffic.rs:77-88, `write_queue()` 91-94, `write_progress()` 105-113), with a public face of eight free functions (`record_traffic` 283, `read_traffic` 464, `read_traffic_entry` 509, `traffic_clients` 523, `clear_traffic` 603, `init_traffic_log` 648, `flush_traffic` 778, `client_key_of` 414). CallLog has finished the same instantiation (calls.rs:98-117, `Arc<CallLog>` into AppContext; the app.rs:61-64 comment calls itself "the S2 instantiation"), and runlog is instantiated likewise; docs/09:134 says outright that "Traffic/calls/runlog global OnceLock state also needs to be instantiated step by step". traffic is the last one. The cost is visible: swiss-mcp unit tests serialize through the `serialized()` lock (traffic.rs:815, about 24 call sites), and the integration tests/adminapi.rs:798-808 likewise need `traffic_lock()` process-level mutual exclusion + `clear_traffic(None)` cleanup — two apps in one process see each other's rings.
- **Fix steps**:
  1. following the CallLog shape, create `pub struct TrafficLog` (ring/seq/known_client/file/bytes + a per-instance write queue and a flush watch sender), `TrafficLog::at(dir: PathBuf)` (persistent) plus a non-persistent constructor for tests (file: None, i.e. a pure in-memory ring, behaviorally equal to today's never-init form);
  2. the eight free functions become `&TrafficLog` methods (signatures gain only self); `init_traffic_log` becomes a two-phase construct / attach-the-file: server.rs:197 changes to construct at assembly time and put it into AppContext;
  3. AppContext gains `pub traffic: Arc<TrafficLog>` (right beside `calls`, its comment citing S2 likewise); migrate the call sites: app.rs:30,409 (proxy layer, already inside AppContext methods), adminapi.rs:573-601 (handlers already hold State(ctx)), server.rs:197,361 (boot and graceful shutdown);
  4. delete the three OnceLocks and the `serialized()` test lock; unit tests hold their own TrafficLog instance; tests/adminapi.rs drops `traffic_lock()`, each test using its own handler/ctx;
  5. rewrite the docs/09:134 OnceLock sentence in sync (calls/runlog/traffic are all instantiated now; the sentence's remaining reminder is the by-design process-level exceptions such as mem.rs's tree-walk cache — name them and set them apart).
- **Blast radius**: crates/swiss-mcp (all of traffic.rs and its tests), src/app.rs, src/adminapi.rs, src/server.rs, tests/adminapi.rs; zero wire-shape change (/api/traffic responses untouched); no other subsystem crate involved.
- **Acceptance tests**: add one isolation test (unit level is enough): two TrafficLog instances in one process each `record_traffic`, and neither `read_traffic` sees the other — that test cannot be written before the change (shared global), so it pins the behavior change; all existing traffic unit tests and the traffic passages of tests/adminapi.rs pass without the global lock; `grep -n "OnceLock" crates/swiss-mcp/src/traffic.rs` has only comments left or zero hits; `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` green.
- **Risk and rollback**: medium — touches the recording calls on the proxy hot path and the shutdown flush; split into two commits (first struct + method-ization keeping thin free-function wrappers, then pull the statics), each step independently revertible; if flush timing breaks, reverting the second commit restores the global state. No idle-memory regression is expected (the same ring, just owned by an instance); if a number is needed, measure and record it on 19998 per the swiss-memory-record convention.

### 2.2 resolve_def lenient/strict closure (#10)

- **Status and root cause**: the audit's "the main path is still lenient" is stale: all three production sites go through strict `resolve_def_checked` — make_adapter (adapters/mod.rs:297), the rest connection test (adminapi.rs:929), tunnel matching (mcpmatch.rs:78). The lenient chain — `resolve_env_refs` (config.rs:84, its comment calling itself "for the callers that have not been moved to the strict contract yet", and no such caller remains), `resolve_value`/`resolve_obj` (88-101), `resolve_def` (124-126) — has zero production calls, referenced only by its own unit tests (348-356, 365-380); five adapter doc comments still say "the resolve_def() clone make_adapter hands over" (direct.rs:90,99, http.rs:404, mysql.rs:550, proc.rs:479, rest.rs:502); docs/19:17,238 still points at `resolve_env_refs` as where the shared resolver lives. What actually remains of the lenient/strict coexistence is "dead code + stale comments", so the closure is exactly that cleanup.
- **Fix steps**: delete `resolve_def`, `resolve_env_refs`, `resolve_value`, `resolve_obj` and their tests; point the five adapter comments at the `resolve_def_checked()` clone instead (and while there, align docs/19 D1's one-line "precheck semantics"); check swiss-host's lib.rs re-exports and delete any same-named ones.
- **Blast radius**: crates/swiss-host/src/config.rs, comments in five adapter files, two docs/19 lines; no behavior change (what is deleted has zero callers).
- **Acceptance tests**: `cargo test --workspace` green; `clippy --all-targets -D warnings` reports no dead_code; `grep -rn "resolve_def\b|resolve_env_refs" crates/ src/ tests/` leaves only `resolve_def_checked` and the docs' historical narrative (e.g. docs/19's updated pointer to swiss-core refs.rs).
- **Risk and rollback**: very low; the only care point is that one of `resolve_env_refs`' unit tests documents the lenient semantics ("$NOPE {a}" returned as-is) — deleting the function deletes the test, and that semantic documentation moves, with the docs/19 update, under the `swiss_core::secure::refs::resolve` entry, so no knowledge is lost. One commit for the whole item; git revert rolls it back.

### 2.3 Rewriting the terminal.rs WaitingDependency comment (#11)

- **Status and root cause**: the comment at `src/plugins/terminal.rs:137-139`: "an unmet requirement would park the whole plugin in waitingDependency". That state was never implemented (the descriptor.rs enum lacks it; docs/09 designed it, and #8 has added as-built): the real behavior is that the plugin starts as usual, the inventory row carries `requires/requiresMet=false`, and the panel shows "needs ssh-shell (no provider)". The comment cites a nonexistent mechanism as its design reason; a reader following it comes up empty.
- **Fix**: rewrite per #8's verdict: the reason for NOT ["ssh-shell"] becomes (a) local sessions need no SSH — `requires` expresses a "functional dependency", not a hard gate; (b) declaring it would hang long-standing requiresMet=false noise on the inventory row, while the targets route already says honestly, per target, whether the remote is reachable. English comment.
- **Blast radius**: that comment only; depends on #8 landing first (docs/09 must carry the as-built wording before the comment can cite it).
- **Acceptance tests**: `grep -n "waitingDependency" src/ crates/` zero hits (case-sensitive; page-registry.js's legacy string is panel-side, outside this repo's grep scope); `cargo test --workspace` green; the existing terminal factory test (the descriptor assertions inside builtin.rs) untouched.
- **Risk and rollback**: a zero-risk comment change; single commit, revertible.

## Batch 3: Panel side (this repo, crates/swiss-panel/panel/src — ADR-024)

> The uniform action for every entry: edit the `.ts` files under `crates/swiss-panel/panel/src` → `npm run check` in `crates/swiss-panel/panel/` (typecheck ×2 + eslint + emit-freshness + the full vitest suite, not just the changed files) → `npm run build` there so the emit under `crates/swiss-panel/src/admin_assets/js` is committed fresh → this repo's `cargo test --workspace` + clippy (and `touch crates/swiss-panel/src/lib.rs` before a release rebuild — the rust_embed fingerprint trap) → live check on 19998 (scripts/test-instance.ps1; 19999 is production, do not touch). `admin_assets/js` is never hand-edited.

### 3.1 The data-view header comment "default 500" (#12)

- **File**: crates/swiss-panel/panel/src/data-view.ts:51 (module header; `DB_PAGE_SIZES` sits at :57).
- **What to change**: "(10/20/50/100/200/500, default 500)" → "default 50", consistent with the served page default and the server's `BROWSE_DEFAULT_PAGE = 50` (dbbrowser.rs:218). One comment line.
- **vitest cases**: no new case needed (pure comment); still run the full suite against accidents.
- **Acceptance**: `grep -n "default 500" crates/swiss-panel/panel/src/data-view.ts` returns zero hits and the emitted js no longer carries it.

### 3.2 The util state.view comment (#13) — DONE BY THE TS REWRITE

- **Evidence**: `crates/swiss-panel/panel/src/util.ts` carries no `state.view` view enumeration at all — the rewrite (ADR-024) did not port the stale five-view comment, so the list no longer exists anywhere. Nothing left to fix.

### 3.3 V2 Unicode glyph cleanup (#14)

- **Files and point-by-point changes** (all under `crates/swiss-panel/panel/src`; the sprite already has a 24x24 Lucide-style symbol mechanism with `iconNode(name)` from util.ts; the former terminal.js `⚙` is already gone):
  - data-grid.ts:880 `⚿` (the PK column marker) → `iconNode("i-key")` (new), title keeps "primary key";
  - `✕` at data-grid.ts:974,1019,1029, data-filters.ts:170, data-browsers.ts:359,469,480 (remove filter / buffered row / cell menus) → `iconNode("i-x")` (new);
  - `↩` at data-grid.ts:1019,1029 and data-browsers.ts:359,469-480 (undo delete / undo) → `iconNode("i-undo")`;
  - run-history.ts:44 `↺` (the Past runs control label) → `iconNode("i-history")` + the text "Past runs (n)";
  - tunnel-sheets.ts:193,196 `↑ Up`/`📁`/`📄` → `iconNode("i-arrow-up")`+"Up", `iconNode("i-folder")`, `iconNode("i-file")`;
  - the ↑/↓/▾ copies now live in the locale dictionaries (en.ts and zh.ts: `dataStream.pendingNew`, `dataStructure.table` "Table ▾", `terminal.nNewDown`, `tunnelSheets.text` "↑ Up") — they are visible copy, so any swap ships in BOTH locales in one change (docs/38);
  - **explicitly untouched**: the body-text typographic characters (…, —, ·, →, the ✓/✗ status letters, the ≠/≥/≤ filter-operator labels) are text, not icons; the header `.db-sort` is already CSS. To prevent over-fixing.
- **vitest cases**: add assertions to the admin-panel/admin-pages/admin-data-grid suites: the rendered HTML of the buttons/labels above contains `<svg` (or icon output) and none of the original glyph characters; plus one counter-case pinning "the filter-operator dropdown still outputs ≠/≥ text" (so that nobody "fixes" the typographic characters in passing).
- **Acceptance**: `grep -rn "⚙|⚿|✕|↩|↺|📁|📄" crates/swiss-panel/panel/src` returns zero hits outside the locale dictionaries (those judged separately above); `npm run check` green; manual check of the Data grid on 19998.

### 3.4 V5 red buttons into the ⋯ menu (#15) — DONE

- **Evidence**: crates/swiss-panel/panel/src/views/tokens.ts:346-349 opens `popupMenu(...)` with `{ label: tr("tokens.revoke"), danger: true, fn: revokeToken }`, and views/secrets.ts:280-281 does the same for `{ label: tr("secrets.delete"), danger: true, fn: removeSecret }` — both destructive verbs already ride the row-end ⋯ menu in the red style, using the shared menu.js component; the row menu is covered by panel/test/admin-row-menu.test.ts.

### 3.5 Unifying the rename interactions (#16)

- **Files** (crates/swiss-panel/panel/src): detail.ts:68-72 (renameMcp uses `prompt`), data-edit.ts:74-77 (table Rename uses `prompt`), add-sheet.ts (openGroupSheet is already the sheet pattern — the sheet idiom to generalize).
- **What to change**: generalize openGroupSheet into a general single-field sheet (title, default value, submit callback, inline error on validation failure — the table-name regex `/^[A-Za-z0-9_$]{1,64}$/ ` and the MCP-name validation stay inside submit); renameMcp and table Rename call it; the group-name path's behavior is unchanged. The browser prompt's ESC/empty-string early-exit semantics are carried by the sheet's Cancel.
- **vitest cases**: three — after an MCP rename the sheet-submitted POST /rename payload is right; an illegal table name errors inside the sheet and sends nothing; the group-name rename path is regression-unchanged.
- **Acceptance**: `grep -n "prompt(" crates/swiss-panel/panel/src` returns zero CALL sites (the word appears in doc comments at add-sheet.ts:87/connect.ts:51 — comments don't count); the three renames share one surface; on 19998, manually test one MCP rename + one table rename.

### 3.6 localStorage key unification and migration (#17, low priority)

- **Files** (crates/swiss-panel/panel/src): util.ts:24 (`swiss.tokenId`; `swiss_theme` beside it is already new-style, and the old collapsed keys are gone), data-view.ts:58 (`swiss.dbSqlHistory`), data-grid.ts:45 (`swiss.dbGrid.*` prefix), data-sql.ts:405 (`swiss.dbFavorites`).
- **What to change**: unify into the `swiss.*` namespace (e.g. `swiss.collapsed`, `swiss.tunCollapsed`, `swiss.tokenId`, `swiss.dbSqlHistory`); one-shot migration at the read sites: new key missing, old key present → read the old value, write the new key, delete the old one. Current values are confirmed to be small JSON/short strings, so the migration has no blast surface. If judged not worth it (purely panel-local preferences, near-zero loss), it can be downgraded to won't-fix — the executor decides at run time; the default is to do it.
- **vitest cases**: a migration case — seed the old key, after the first read assert the new key has the value and the old key is deleted; the fresh-install path asserts only the new key is written.
- **Acceptance**: `grep -rn "swiss." crates/swiss-panel/panel/src` returns zero hits (unless an old-key constant is kept solely for migration reads); after one refresh on 19998, the old keys are gone in DevTools.

## Won't-Fix List (item by item: why deliberate + provenance)

1. **#18 /api/tokens/{id}/secret plaintext read-back**: the comment's self-defense holds — verify() compares in plaintext anyway, the secret is `cat`-able inside managed.json, a loopback caller reading it buys no secrecy, and what it saves is one rotate before every copy of the connect command; the separate route makes "read the secret" an explicit action. Against the vault's "values go in but never out", this is two threat models for two credential classes, not an inconsistency. (adminapi.rs:428-444)
2. **#19 CLI open uses cmd /c start**: the platform default opener has no direct-Win32-call equivalent; "no subprocess where a syscall exists" yields here to opener semantics, and CREATE_NO_WINDOW (0x08000000) + null on all three streams has already driven the cost to the floor. A trade-off on record. (cli.rs:687-711)
3. **#20 local terminal sessions have no cap**: the user's call on 2026-09-12 — a local PTY's one thread stack is spent on the operator's own machine; remote targets keep both caps, and locals do not count against the budget. The deviation is recorded in docs/14 §6/§8.1 and the session.rs:606-614 comment. (session.rs:606-614; docs/14:206,249)
4. **#21 legacy-command lenient refs**: preserving the historical semantics of the Node era's jobs.json (unset variable → empty string) from silently changing meaning; exec's strict is the model for new code, and the docs say plainly that the compatibility shape is not the model. Two contracts in one layer is a dividing line between eras. (actions.rs:9-16; the x-env-refs declaration 193 strict/269 lenient)
5. **#22 tunnels' JS Number() replication**: tunnels.json/API must stay byte-compatible with the Node build — NaN defaults, string numbers, boolean coercion, and error messages rendering the coerced value must all stay; jobs' strict def.rs is the new native-Rust world — two philosophies, each declared by its comments, both deliberate. Side note: the `.expect()` in `new_id` at types.rs:70-72 is the repo's declared exemption from "no unwrap on config/network/disk" (an OS CSPRNG failure means the machine is broken). (types.rs:14-61,66-77)
6. **#23 with_explain keeps both copies**: the server copy is the semantic spec + the test anchor (pinning idempotency and semicolon stripping), the panel copy is what actually runs; deleting the server copy would lose the Rust-side contract pin. Only #4's comment clarification happens; no deletion.
7. **#24 retention.maxHistoryBytes not enforced**: parsing, storage, and schema presentation are all there, and the comment says plainly that the cross-file total budget "arrives with the S5 semantics that need it" — accepted but not enforced, explicitly accounted for by comment + doc, which is not silent lying. It lands together with S5 and is outside this plan's scope. (runlog.rs:59-63)
8. **#25 jobs boot probe**: docs/09 §6 opposes the frontend probing endpoints to derive the capability tree, but this probe is a legacy-gateway (no /api/plugins) compatibility layer, and already a dual track of inventory-first + probe-fallback; deleting it would cost the panel its whole Jobs tab on old gateways. Kept; the comment already states its transitional nature. (panel/src/jobs.ts:55 — the Node-era views/jobs.js:14-17 / js/jobs.js:31-32 paths were retired by the ADR-024 TS port)
9. **#26 the sidebar mechanism coupled to content**: `page.sidebar` is given per page while sidebar.js renders a fixed MCP list — docs/13 §7 says explicitly: do not fix; among the current pages only mcps opens a sidebar, so the wart has no live instance; a fix needs its own spec (pages declaring their own sidebar content), not mixed into this plan. (docs/13:347-352)
10. **#27 the remaining declared small exceptions** (each named by an audit sub-document, verified true, deliberate or hard constraints):
    - the `/api/mcps/{name}/{kind}` wide route + the hand-rolled kind-whitelist 404: a direct translation of the Node route shape (mcp.md exception 4).
    - proc's FFI unsafe concentrated in the win module: the project's one allowed unsafe scenario (mcp.md exception 3; the platform/mod.rs:3-4 boundary).
    - redis's `editable` computed at the route layer (dialect != "redis"): the RedisBrowser trait has no dialect(), a small seam (data.md).
    - the console read-only error copy replaced with a panel-specific sentence: console read-only is a contract, not the result of a readonly flag (mysql_browser.rs:213-221; pg_browser.rs:297-306).
    - `with_explain` letting EXPLAIN ANALYZE through and rejecting EXPLAIN INSERT: read-only semantics decided uniformly at the statement-shape layer (dbbrowser.rs:1239-1241).
    - the `state.tun.keys` permanent cache: a deliberate trade-off for a low-frequency static directory, unlike the 6 s-polled data (tunnels.md).
    - stop-all's 409 blocker list reusing the Dependents structure: a wide use of the same destructive-action funnel, "users to stop" rather than "users to delete" (tunnels.md).
    - Jobs is Rust-native with no Node counterpart: a deliberate divergence on record; once the panel page is complete, the /api/jobs shape is the contract (jobs.md; mod.rs:4-7).
    - Run now's 30-minute polling cap is a UI-added bound; the backend's real bound is timeoutMs: the comment states the division (jobs.js:145-147).
    - the ticket route absent from the docs/14 §8 original route table: cast during implementation, the deviation recorded in §8.1 T5, the historical table not rewritten back (docs/14:357-369).
    - a recording failure warns once and does not block the session: a recording that cannot be written is not worth the user losing a terminal (session.rs:417-430).
    - one blocking thread per local session: the hard cost of ConPTY's anonymous pipes not being pollable; the stack cost is booked in the docs/14 §7 budget (local.rs:12-21).
    - the proc MCP child staying out of RunCoordinator, the process plugin registering last to start first, and the mem JSON heap field's honest approximation (private-commit standing in for heap, externalMb=0): layering and honesty choices on record (process.md; mem.rs:8-12).
    - the audit document data.md's "task-scope /api/data vs /api/db": the audit document already self-annotates /api/db as authoritative; no repo files involved, no action needed.

## Execution Order and Gates

1. **Batch 1 (#1-#8) → one or more small commits**. Gates: `cargo test --workspace` (comments live inside compilation units too, guarding against doc-comment breakage) + `cargo clippy --workspace --all-targets -- -D warnings`; docs changes self-checked with the grep acceptance items. No deployment action.
2. **Batch 2 (#9→#10→#11, in order)**. #9 adds the isolation test first (red), then the struct, then pulls the statics; may be two commits; #10 and #11 one commit each (#11 after #8). Every commit runs: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`. `--workspace` is not optional (AGENTS.md: without it cargo selects the root package alone, the eight member crates are not even built, and the run still reports ok). When the change touches the runtime (#9), verify live on 19998 (scripts/test-instance.ps1 -Start; 19999 is the user's production — never stop, restart, or redeploy it); if memory numbers are to be claimed, measure live and record in docs/01 per the swiss-memory-record convention.
3. **Batch 3 (#12-#17) → in this repo, no external precondition** (ADR-024; the Node checkout is retired as the panel reference, docs/07). Gates: `npm run check` in full + `npm run build` in crates/swiss-panel/panel (emit committed fresh); this repo's `cargo test --workspace` + clippy; live browser verification on 19998 of each item's manual check points. One commit carries both the .ts change and the emitted js.
4. **Every fix carries a test or a grep-able acceptance**; nothing moves on to the next item until acceptance passes; while Batch 3 is not in place, Batch 1/2's results stand on their own and merge independently, blocking each other in no way.
5. **Discipline restated**: `crates/swiss-panel/src/admin_assets` is never hand-edited (edit `crates/swiss-panel/panel/src` and emit — ADR-024); no new agent presets; the new-features-on-by-default principle does not apply to this plan (it is all closure and alignment, no new switches); all new comments in English, stating the why.

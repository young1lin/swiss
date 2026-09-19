# Panel changes: proof of life before "done"

Written after the agent-page shipped dead three times (2026-10): a view the user could not
open, a sheet that rendered nothing, all reported as "live verified". These rules are
binding for every change under `crates/swiss-panel/src/admin_assets/`.

## The rule in one line

A panel change is done only after a REAL browser, freshly loaded, opened the page and every
visible control was clicked with real pointer events and answered. vitest green plus
`node --check` is the entry ticket, never the proof.

## Why vitest cannot catch this class

- The vitest transform does not validate named imports at link time. `import { closeSheet }
  from "../util.js"` when the export lives in `add-sheet.js` passes every test and kills the
  whole module in the browser (ES modules refuse to link), leaving the page dead.
- DOM-existence assertions are blind to visibility. `!!document.querySelector(".sheet")` is
  true while the container still has `hidden` - the exact bug where sheet markup rendered
  into a hidden box and the user's click "did nothing".
- Synthetic `.click()` fires the handler directly and bypasses coordinate hit-testing,
  overlays and focus. It proves the handler exists, not that a human click works.

## The checklist (all of it, every panel change)

1. **Edit `panel/src/*.ts`, never hand-edit `js/`** — the served tree is emitted
   (`npm run build` in `crates/swiss-panel/panel`). Read the house idiom before writing
   wiring: one existing view that already does the thing is the spec — sheets ->
   `panel/src/add-sheet.ts` + `panel/src/views/terminal-settings.ts`
   (`$("sheet").hidden = false` BEFORE innerHTML; `closeSheet` comes from add-sheet);
   empty states -> `util.ts emptyHtml`; icons -> the `i-*` sprite via `icon(name)`.
   Never invent a parallel mechanism.
2. **`npm run check` in `crates/swiss-panel/panel`** (typecheck ×2 + lint + emit
   freshness + vitest). Catches syntax, link-time import errors and pure-function
   regressions.
3. **Rebuild and restart 19998** (`scripts/test-instance.ps1 -Stop`, build with
   `CARGO_TARGET_DIR=target-test`, then `-Fresh`). Never verify against a stale binary.
   The panel is now authored in TypeScript (`crates/swiss-panel/panel/src`, docs/36): run
   `npm run build` there first so the committed emit in `admin_assets/js` is fresh, THEN
   `touch crates/swiss-panel/src/lib.rs` before the release build — the rust_embed
   fingerprint trap below is unchanged by the port.
4. **Real-browser walk on a fresh page load.** Using browser automation with REAL clicks
   (CDP input events, not `element.click()`), on a page navigated from scratch:
   - every level of navigation reaches the view (top tab -> page -> seg);
   - every seg switches and shows its pane;
   - every sheet opener OPENS VISIBLY - assert `sheet.hidden === false` AND computed
     `display` of the dialog, or read its bounding rect (position:fixed makes
     `offsetParent` null - do not use it as a visibility probe);
   - every primary action (Save / Send / Test / Delete) runs its request and the UI
     reflects the result;
   - empty states render when the store is empty.
5. **Honest reporting.** A flow that cannot be verified (missing credential, external
   endpoint) is listed as NOT verified with the reason - never marked with a checkmark.
   "Done" claims a dead page is a lie that costs the user's trust and their time as your
   test runner.

## Mechanical traps that shipped broken code (do not repeat)

- Inside code-splicing templates, `${` interpolates even under `String.raw`. Write
  `"${" + "VAR}"` for literal env-ref placeholders.
- A real newline inside a JS string literal (from careless splicing) is a syntax error that
  kills the module. Use `String.fromCharCode(10)` when building newline-containing code
  programmatically.
- PowerShell `cd` inside a chained command breaks every later relative path. Run build and
  instance scripts from the repo root in their own call.
- `rust_embed` fingerprints do not include asset content: editing `admin_assets` does NOT
  recompile swiss-panel, so a release build says "Finished" while the binary still embeds
  the OLD panel (found live during docs/27 C4, 2026-09-15). Before every release rebuild on
  19998, `touch crates/swiss-panel/src/lib.rs` — then verify the served bytes, not just the
  build status.
- A "successful" UI assertion that only checks existence is worse than no assertion: it
  manufactures false confidence. Assert state changes the user can see.
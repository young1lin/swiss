# Panel changes: proof of life before "done"

A panel page once shipped dead three times running — a view the user could not open, a sheet
that rendered nothing — each reported as "live verified". These rules bind every change under
`crates/swiss-panel/`.

## The rule in one line

A panel change is done only after a REAL browser, freshly loaded, opened the page and every
visible control was clicked with real pointer events and answered. `npm run check` green is the
entry ticket, never the proof.

## Why the suite cannot catch this class

- vitest loads modules one at a time through its own transform. A page that fails at load in
  the browser — a module that throws at top level, wiring that expects an element the markup
  lacks — never shows up there.
- DOM-existence assertions are blind to visibility. `!!document.querySelector(".sheet")` is
  true while the container still has `hidden` — the exact bug where a sheet rendered into a
  hidden box and the user's click "did nothing".
- Synthetic `.click()` calls the handler directly and bypasses hit-testing, overlays and focus.
  It proves the handler exists, not that a human click works.

## The checklist (all of it, every panel change)

1. **Edit `panel/src/*.ts`, never the emit** in `admin_assets/js`. Draw with the library
   (swiss-ui-design §1, SPEC §panel.ui): sheets `showSheet(sheet({ title, body, foot }))` from
   `ui/sheet.ts` (it unhides `#sheet` before filling it and closes on a backdrop click;
   `add-sheet.ts` is the worked example), menus `popupMenu` (`ui/menu.ts`), empty states
   `emptyNode` (`ui/page.ts`), icons `iconNode(name)` (`ui/icon.ts`), markup `h()` and `fill()`
   (`h.ts`). Never invent a parallel mechanism.
2. **`npm run check`** in `crates/swiss-panel/panel` (typecheck + lint + emit freshness +
   vitest).
3. **Rebuild and restart 19998**: `npm run build`, `touch crates/swiss-panel/src/lib.rs` (the
   rust_embed trap below), then the swiss-live-verify loop. Never verify against a stale binary.
4. **Real-browser walk on a fresh page load**, with REAL clicks (CDP input events, not
   `element.click()`), on a page navigated from scratch:
   - every level of navigation reaches the view (rail → page → section);
   - every section switch shows its pane;
   - every sheet opener OPENS VISIBLY — assert `sheet.hidden === false` and the dialog's
     computed `display`, or read its bounding rect (`position: fixed` makes `offsetParent`
     null; it is not a visibility probe);
   - every primary action (Save / Send / Test / Delete) runs its request and the UI reflects
     the result;
   - empty states render when the store is empty.
5. **Honest reporting.** A flow that cannot be verified (a missing credential, an external
   endpoint) is listed as NOT verified with the reason, never ticked. A "done" on a dead page
   makes the user your test runner.
6. **The second-language pass.** A change that touches visible copy is walked again in
   Chinese: click 文/A, confirm `document.documentElement.lang === "zh-CN"`, re-read the words
   (SPEC §panel.i18n).

## Mechanical traps that shipped broken code

- `rust_embed` fingerprints can miss an asset change: the release build says "Finished" while
  the binary still embeds the OLD panel. Touch `crates/swiss-panel/src/lib.rs` before every
  release rebuild, then verify the served bytes, not the build status.
- A script that splices code: `${` interpolates even under `String.raw` (write
  `"${" + "VAR}"`), and a real newline inside a JS string literal kills the module (use
  `String.fromCharCode(10)`).
- PowerShell `cd` inside a chained command breaks every later relative path. Run build and
  instance scripts from the repo root, each in its own call.
- An assertion that only checks existence is worse than none: it manufactures confidence.
  Assert state changes the user can see.

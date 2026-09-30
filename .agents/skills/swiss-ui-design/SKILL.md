---
name: swiss-ui-design
description: Enforce Swiss's UI architecture and visual consistency whenever designing, reviewing, implementing, refactoring, or fixing the Swiss panel UI, navigation, page layouts, plugin pages, toolbars, sidebars, focus/fullscreen behavior, responsive behavior, or visual components. The panel draws through its in-tree UI library (crates/swiss-panel/panel/src/ui, styles/ui.css, the /admin/ui.html gallery); this skill says how to use and extend it, where the design language lives in SPEC §panel, and the checklists any panel change must pass. Invoke automatically for any Swiss UI/UX task.
---

# swiss-ui-design

The working manual for every UI/UX change in the **swiss** panel. Its purpose is not to make
every page look identical: every page obeys the same information hierarchy, navigation
ownership, action placement and interaction model, and draws them all with the same parts.

> **Same information level = same UI mechanism.** A different content type may use a different
> body template, but it stays inside the same shell.

The normative text is SPEC §panel. Read the sections the change touches before designing — and
the page's own source; never design from screenshots alone.

| Section | What it fixes |
| --- | --- |
| SPEC §panel.nav | the four levels (L0–L3) and their owners, the shell, the rail, the context bar, Focus mode, the body templates (Content / Resource / Workspace), action placement, responsive behaviour |
| SPEC §panel.design | the direction, the tokens, the 25 numbered rules — code cites them as "rule N" |
| SPEC §panel.ui | the UI library, its vocabulary, the gallery, the gates G1–G7 |
| SPEC §panel.groups | the grouped list: the band, drag, folding, create and delete |
| SPEC §panel.pages | per-page look rules |
| SPEC §panel.i18n | every visible string through `tr()`; English and 简体中文 |

When this skill and SPEC disagree, the code decides which one is right (SPEC §about) and both
are fixed in the same change.

## 1. The UI library is the only way to draw

- **`crates/swiss-panel/panel/src/ui/*.ts`**, exported whole by `ui/index.ts`: every shape a page
  draws. Its classes, and only those, live in `styles/ui.css`.
- **The gallery, `/admin/ui.html`** (`ui-gallery.ts`, `ui-scenes.ts`): every component in every
  state; `?theme=dark&lang=zh&w=960` switches theme, language and width. It is reached by typing
  it; it is not in the rail.
- **Shell parts are not `ui/`**: the rail and the context bar (`page-registry.ts`, `base.css`),
  the plugin palette (`plugin-palette.ts`), Focus mode (`immersive.ts`), the pane's scrolling
  (`pane-scroll.ts`), the toast (`util.ts`).

How to work with it:

1. **A page composes `ui/`.** It builds its own data hooks (ids, `data-*` for its delegated
   listener) and hands them to library functions; it never writes a library class into an
   `h()` class literal.
2. **A shape the library lacks goes into the library first**: the function in `ui/`, its
   classes in `ui.css`, a gallery section or scene, a test in `ui-components.test.ts` — then the
   page uses it.
3. **`views.css` lays a page out; it never restyles a library class** (G2). A workspace's
   skeleton (Data, Terminal) and page-local layout live there, nothing the library owns.
4. **A design is a gallery scene**: drawn in `ui-scenes.ts` from `ui/`, `h` and `i18n` with
   made-up data, reviewed in the gallery, then built — never a hand-written HTML mock.
5. **`ui/` imports nothing but `../h.js`, `../i18n.js` and itself** (G1), which is what lets the
   gallery render every component with nothing else loaded.

The seven gates (SPEC §panel.ui) all run inside `npm run check`. G4, G5 and G7 are ratchets:
when a file's count drops, lower its row in the same change.

## 2. Hierarchy and ownership, in short

SPEC §panel.nav is the full text; these are the calls a change most often gets wrong.

- **L1 plugin → the rail. L2 page → the context bar. L3 → the page body.** Sibling pages of one
  plugin (MCP → Servers / Traffic / Token) are underline tabs in the context bar, never
  segmented tabs in the body. A pill `seg()` is only for L3: sections of the current resource
  (an MCP's Tools / Resources / Prompts / Run / Config / Logs). Data's object tabs are L3 made
  visible, not a new layer.
- **The shell is invariant.** It owns the rail, the context bar, the readouts, Focus mode, the
  pane's scrolling and back to top. A page owns only its body and never recreates chrome.
- **One body template per page**: Content (a measure, a pinned head, actions at its right),
  Resource (a source list beside a pinned `resHead`), Workspace (`pane({ full })`: full-bleed
  but still under the context bar and beside the rail, never wrapped in a decorative card).
- **Actions keep their corner**: app-global → context bar; page → the head's right; selected
  resource → the resource head; row → the row's end or its `⋯`. A body header exists only for
  identity, a description, the primary action or filter state — never to repeat the location.
- **Focus mode** is one shell control at the context bar's far right, and exits from the same
  anchor. A page never adds its own fullscreen button; every toggle dispatches `resize`.
- **Growth** never widens the rail: it is a pinned shortlist and `⋯` opens the palette. A
  plugin needing more than five pages is spending L3 content on L2.
- **Narrow widths** keep the hierarchy: dense bodies stack, the location and Focus stay
  reachable, nothing scrolls the app sideways.

## 3. Working in this repository

1. **Start from `ui/index.ts` and the gallery** — the shape probably exists, in every state.
   Then read the page's own module under `crates/swiss-panel/panel/src/` for its wiring.
2. **Edit TypeScript, run `npm run build`**; the emit under `src/admin_assets/js` is committed
   and never hand-edited (SPEC §panel.toolchain). No bundler or framework migration for a UI
   cleanup, no npm runtime dependency, no preprocessor, no web font.
3. **CSS is three layers**, loaded `base.css` (tokens, reset, the shell) → `ui.css` (library
   classes) → `views.css` (workspace skeletons and page-local layout).
4. **Tokens first.** Read the token block at the head of `styles/base.css` before adding a value
   — it probably exists. A new size lands there, named, before any rule uses it (G4).
5. **Icons are the sprite**: a new icon is one `<symbol>` in `index.html`, drawn by
   `iconNode(name)`; never a Unicode glyph.
6. **A behaviour change ships with vitest coverage** under `crates/swiss-panel/panel/test/`.
7. **Migrating a page deletes its old CSS and class names in the same change**: no
   compatibility class survives, and the gate rows go down with it.
8. **Keep API contracts and deep links** unless there is a concrete reason; do not reshape an
   API to make rendering easier when the descriptors already express the structure.
9. **Verify on 19998** with the agent-browser skill (SPEC §testing.live,
   `.agents/rules/panel-proof-of-life.md`): light and dark, English and Chinese, at ~1440px and
   ~960px. 19999 is the user's; never touch it.

## 4. Review checklist — hierarchy first

Before accepting any UI change:

- Is this L1, L2 or L3 navigation, and is that level drawn the same way everywhere else?
- Who owns this control: shell, page, resource or row?
- Did the page create its own app chrome? Does switching plugins make the body jump vertically?
- Is a dropdown visibly a dropdown rather than a breadcrumb-looking mystery control?
- Is a segmented control used for a true page-local sibling view, not plugin-page navigation?
- Does the plugin need more than five pages, or the `⋯` tab at normal widths? Restructure.
- Is a large card merely decorating a workspace that already has an app boundary?
- Is Focus mode in the same place on every page, and does it exit from the same anchor?
- Would this still work with 30 plugins? Would a new plugin author know where their L1/L2/L3 UI
  belongs?
- Is every shape a `ui/` function, and is a new component or token standing in for an
  existing one?

If an answer exposes inconsistent hierarchy or ownership, fix the architecture before polishing
CSS.

## 5. Checklist before you call it done

- `npm run check` in `crates/swiss-panel/panel` is green (typecheck, lint, emit freshness,
  vitest — the seven gates are inside it); a ratchet row that can go down went down.
- If `ui/` or `ui.css` changed: the gallery walked with real clicks, light and dark; the new
  shape has a gallery section and a `ui-components.test.ts` case.
- Every colour, size, weight and radius is a token; four weights only (rule 21); nothing is
  defined only under dark.
- No new Unicode icon, no `innerHTML` of a glyph.
- No row with two non-icon buttons, no red word button on a row — the second action and every
  destructive one live behind the row's `⋯` (danger last) or are its trailing glyph (rule 4).
- Mono only for values you would copy (rule 1); saturation only for state (rule 2).
- Vertical rhythm: nothing is glued to the block above it (rule 17).
- A create form says which group the item joins before submit (rule 6); user-named containers
  are bands in mixed case, product-named sections are uppercase captions (rule 5).
- Every new dot has a `title`; every icon-only button has an `aria-label`.
- Keyboard: focus visible, Escape closes, Enter submits the sheet's primary, arrows move in a
  listbox; a layer that handles a key stops its propagation (the shell listens at document).
- 960px: the rail does not clip, the context bar stays, rows wrap their sub-line.
- Chinese walked too (文/A): the words fit, nothing reads as English on a Chinese screen.
- The CSS comment beside a changed rule still states the *reason*.
- A grouped page matches SPEC §panel.groups' anatomy — build it from the gallery's content
  scene, do not reinterpret it.

## 6. When rules conflict

1. The security and correctness rules in `AGENTS.md`.
2. Information hierarchy and navigation ownership (SPEC §panel.nav).
3. Interaction consistency and accessibility.
4. The existing tokens and library components.
5. Local page aesthetics.

An old visual decision that violates a higher level is not kept because it exists: update the
implementation, the tests and SPEC together so the repository has one current rule.

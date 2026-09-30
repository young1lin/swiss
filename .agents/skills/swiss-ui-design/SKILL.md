---
name: swiss-ui-design
description: Enforce Swiss's UI architecture and visual consistency whenever designing, reviewing, implementing, refactoring, or fixing the Swiss panel UI, navigation, page layouts, plugin pages, toolbars, sidebars, focus/fullscreen behavior, responsive behavior, or visual components. The panel draws through its in-tree UI library (crates/swiss-panel/panel/src/ui, styles/ui.css, the /admin/ui.html gallery); this skill says how to use and extend it, plus the design language - tokens, colours, spacing, icons, sheets, empty states, component vocabulary - and the checklist any panel change must pass. Invoke automatically for any Swiss UI/UX task.
---

# swiss-ui-design

Use this skill for every UI/UX change in the **swiss** panel. Its purpose is not to make every page look identical. Its purpose is to make every page obey the same information hierarchy, navigation ownership, action placement, layout vocabulary, and interaction model — and to draw every one of them with the same parts.

The core rule is:

> **Same information level = same UI mechanism. Different content type may use a different body layout, but it must remain inside the same App Shell.**

Before changing UI, read the repository `AGENTS.md` and the relevant existing panel code. Do not design from screenshots alone when source is available.

This skill also absorbs the former `swiss-design` skill: the design language, the token names, the rules and the component vocabulary live in §15–§19. Historical references elsewhere to "swiss-design rule N" mean §16 below. SPEC §panel is normative for the panel and this skill is its working manual: when they disagree, fix both in one change.

## 0. The UI library is the only way to draw

The panel has one component library, in the tree (SPEC §panel.ui, ADR-029):

- **`crates/swiss-panel/panel/src/ui/*.ts`**, exported whole by `ui/index.ts`: every shape a page draws — buttons, status marks, the switch, the styled select, page scaffolding, the list row, the group band, the segmented control, the event list, the code block, forms, menus, sheets, the empty state, back to top. §17 maps each word to its function.
- **`crates/swiss-panel/src/admin_assets/styles/ui.css`**: the classes those functions draw, and only those.
- **The gallery, `/admin/ui.html`** (`ui-gallery.ts`, `ui-scenes.ts`): every component in every state, light / dark, English / Chinese, 1440 / 960 — the switches live in the query string (`?theme=dark&lang=zh&w=960`). It is reached by typing it; it is not in the rail.

How to work with it:

1. **A page composes `ui/`.** It builds its own controls' data hooks (ids, `data-*` for its delegated listener) and hands them to library functions; it never writes a library class into an `h()` class literal.
2. **A shape the library lacks goes into the library first**: the function in `ui/`, its classes in `ui.css`, a section (or a scene) in the gallery, a test in `ui-components.test.ts` — then the page uses it. A page that needs a shape stops until the library has it (U17).
3. **`views.css` lays a page out; it never restyles a library class** (G2). A workspace's skeleton (Data, Terminal) and a page's own local layout live there, nothing the library owns.
4. **A design is a gallery scene.** A new page or a redesign is drawn in `ui-scenes.ts` from `ui/`, `h` and `i18n` with made-up data, reviewed in the gallery, and only then built — never a hand-written HTML mock with its own style sheet.
5. **`ui/` imports nothing but `../h.js`, `../i18n.js` and itself** (G1): no API, no state, no views. That is what lets the gallery render every component with nothing else loaded.

The seven gates (all in `crates/swiss-panel/panel/test/`, all inside `npm run check`):

| Gate | Test | It fails when |
| --- | --- | --- |
| G1 | `ui-boundary.test.ts` | a `ui/` module imports anything but `../h.js`, `../i18n.js` or `./*` |
| G2 | `ui-css-ownership.test.ts` | `views.css` makes a `ui.css` class the subject of a selector (a ratchet) |
| G3 | `css-weights.test.ts` | a `font-weight` is not one of the four `--w-*` tokens |
| G4 | `css-literals.test.ts` | a sheet gains a px / hex / rgb literal outside the token block (a per-sheet ceiling that only falls) |
| G5 | `ui-class-ratchet.test.ts` | a file outside `ui/` draws more `ui.css` classes than its frozen row — a file may only go down, and when it goes down its row goes down with it; a file with no row is held at 0 |
| G6 | `ui-gallery.test.ts` | an export of `ui/index.ts` is claimed by no gallery section or scene (or by `HELPERS` with the reason it draws nothing), a claim's shape is missing from the DOM, or a class the gallery draws is unstyled |
| G7 | `css-size.test.ts` | `views.css` grows past its frozen byte ceiling |

## 1. Fixed information hierarchy

Swiss has four UI levels. Do not collapse them or represent the same level differently on different pages.

```text
L0  App
    swiss

L1  Plugin
    MCP / Tunnels / Data / Jobs / Terminal / Remote / Settings / ...

L2  Plugin Page
    MCP      -> Servers / Traffic / Token
    Tunnels  -> SSH Connections / Port Forwards
    Settings -> Plugins / Secrets / System
    Remote   -> Targets / Runs
    Data     -> single page
    Jobs     -> single page
    Terminal -> single page

L3  Page-local navigation / resource navigation
    MCP Server -> Tools / Resources / Prompts / Run / Config / Logs
    Data       -> database / schema / table / object
    Terminal   -> sessions / terminal-local controls
    Jobs       -> filters / job detail
```

Ownership is strict:

- L1 belongs to the **Plugin Rail**.
- L2 belongs to the **Context Bar**.
- L3 belongs to the **page body**.
- A page must never invent another L1 or L2 navigation mechanism.

## 2. The App Shell is invariant

Normal mode always has the same shell:

```text
┌────────┬────────────────────────────────────────────────────┐
│ Plugin │ Context Bar                                        │
│ Rail   ├────────────────────────────────────────────────────┤
│        │ Page Body                                          │
│        │                                                    │
└────────┴────────────────────────────────────────────────────┘
```

The shell, not plugins, owns:

- Plugin Rail
- Context Bar
- global status/readouts
- Focus Mode entry/exit
- the pane's scrolling, its pinned head measurements and the back-to-top button (`pane-scroll.ts`)
- app-level responsive behavior

A plugin/page owns only its Page Body.

Never let a page replace or recreate app chrome.

## 3. Plugin Rail rules

The left rail is the only L1 navigation: 56px wide, one 42px seat per pinned plugin — its 18px glyph over its name. The caption is one size for the whole rail: 10px, stepped down together (to a 9px floor) by `page-registry.ts fitRailLabels` only when a gateway-served name is longer than the seat; a name that fits nowhere ellipsizes and keeps its whole text in the seat's `title`. (SPEC §panel.nav once shipped it icon-only; the owner reversed that for clarity.)

- `More` (`⋯`, glyph only — an affordance, not a domain) opens the complete/searchable plugin list.
- The active seat is the chrome's one accent: a 2px notch on the seat's leading edge over the hover ground.
- The rail has no edge of its own (U7): beside a page the ground change is the edge, beside the MCP sidebar the two are one surface.
- Plugin pages never appear as separate rail seats.
- A disabled/unavailable plugin keeps its seat and is marked unavailable instead of disappearing (the title carries the reason); its page is the empty state, never a blank.
- Do not create another plugin navigation row inside the page.

## 4. Context Bar rules

The Context Bar is **always present in normal mode**, including single-page plugins and workspace pages.

Its job is to answer:

1. Where am I?
2. If this plugin has multiple pages, how do I switch pages?
3. What global/page-level status is relevant?
4. Where is Focus Mode?

### Multi-page plugin

The title names the plugin once (its rail glyph beside its label); every page is a visible underline tab — a real `<a href="#id">` link — and the current one is marked by a 2px accent on the bar's own bottom hairline:

```text
[⛏] MCP   Servers  Traffic  Token            10 MCPs · 4 up   [⛶]
[🔌] Tunnels   SSH Connections  Port Forwards   3 connections  [⛶]
[⚙] Settings   Plugins  Secrets  System          6 plugins       [⛶]
```

The underline tab is the L2 shape; the pill segment inside a page body is the L3 shape. The two are never interchangeable: a plugin's pages never render as pills in the bar, and page-body panes never render as underline tabs. A trailing `⋯` seat catches overflow below the support floor; needing more than ~5 tabs means L3 content is being spent on L2 (see §13).

### Single-page plugin

Keep the same bar height and alignment; draw the title alone — no fake one-entry tabs:

```text
Data                                         mysql · 1,697 tables   [⛶]
Jobs                                         12 jobs · 1 running    [⛶]
Terminal                                     3 sessions             [⛶]
```

The vertical origin of Page Body must not jump when switching between single-page and multi-page plugins.

## 5. L2 page navigation must never be recreated inside Page Body

If two views are sibling pages of one plugin, they belong in the Context Bar.

Examples:

- `MCP -> Servers / Traffic / Token`: Context Bar.
- `Tunnels -> SSH Connections / Port Forwards`: Context Bar.
- `Settings -> Plugins / Secrets / System`: Context Bar.

Do **not** implement these as page-local segmented tabs.

Use page-local tabs only for L3 sections of the current resource, e.g.:

```text
shop-search
[Tools] [Resources] [Prompts] [Run] [Config] [Logs]
```

These are valid because they operate within one selected MCP server.

## 6. Page-body layout vocabulary

Unified UI does not mean every body is the same. Every page chooses one of a small set of body templates — `pane()` / `paneBody()` draw them.

### A. Content

For list/settings/management pages (`paneBody({ wide })` + `paneHead`).

Examples: Tunnels, Settings › Plugins, Jobs, Traffic, Token.

- normal page padding, the readable measure (`--measure`, or `--measure-wide` for genuinely wide rows)
- the head pins (U9); its actions at its right
- one primary action at most unless the workflow genuinely requires more

### B. Split / Resource

For master-detail workflows: a source list (`sideRow`, `groupNode` at side density) beside a resource detail whose head is `resHead` — the name row and the resource's tabs pin, its description and state line scroll away beneath them.

Examples: MCP Servers.

```text
resource list | resource detail
```

The left pane is part of the Page Body, not app navigation.

### C. Workspace / Full-bleed

For dense tools that need all remaining space (`pane({ full })`: no padding, no measure, no scroll of its own).

Examples: Data explorer, Terminal.

`workspace` means **full-bleed Page Body**, not “plugin owns all chrome”.

A workspace still lives below the Context Bar and beside the Plugin Rail in normal mode.

### D. Data explorer special case

Data may use:

```text
table/database browser | data workspace
```

Do not wrap the entire Data workspace in a giant decorative card. The shell already provides the application boundary. Use dividers and localized surfaces where they communicate real grouping.

## 7. Header and action placement

Avoid duplicate location titles.

If the Context Bar already says `Tunnels / SSH Connections`, Page Body should not need a giant `Tunnels` heading merely to repeat location.

A body header is justified only when it contributes one or more of:

- current resource identity
- useful description
- primary page action
- scope/filter state

Actions follow a stable hierarchy:

- app-global action -> Context Bar
- page action -> page header/right side
- selected-resource action -> resource/detail header
- row action -> row end / overflow menu

Do not move equivalent actions to different corners on different pages.

One page's navigation is a special case worth naming (SPEC §data.tabs): Data's resource
navigation is OBJECT TABS — each open table/key/console is a tab on the strip, and the
per-object header carries exactly one primary action plus an overflow (rule 4 as a machine
gate: at most one non-icon .btn per toolbar). The tab strip is the L3 half made visible,
not a new layer: same pill vocabulary as a section segment, card-shaped, closable, capped
(twelve, `DB_TAB_MAX`: the least recently used clean, inactive tab makes room; when every
tab is busy a new one is refused rather than dropping edits silently).

## 8. Focus Mode

Swiss has one shell-owned Focus Mode. It is not browser F11.

Rules:

- Entry control lives at the far right of the Context Bar.
- The same spatial anchor is used to exit Focus Mode.
- Page modules must not create their own fullscreen/focus button.
- In Focus Mode, app chrome may fold away, but the exit control remains obvious at the top-right.
- Pressing `Esc` may exit according to the existing global Esc chain.
- Toggling Focus Mode must continue to dispatch resize so Terminal/Data can remeasure.

Never place the normal entry in the Rail footer and the exit in another corner. Spatial memory must remain stable.

## 9. Visual language

Use the existing Swiss visual system before inventing new tokens.

- 4px spacing grid.
- The type ramp, and **four weights and only four** (U5): `--w-body 400` running text, `--w-name 450` identity in a list, `--w-emph 500` a selected tab / a button / a caption / a band, `--w-title 600` titles. No other `font-weight` exists (G3).
- **Fewer lines** (U7): a row separator starts at the row's text column (inset, as System Settings draws it), not at the card's edge; the event list has no box and no row lines — the one open row is the card; Data's grid has no vertical rules; the rail has no edge.
- **Thin scrollbars** (U8): 10px, transparent track, no arrows, a rounded thumb; Firefox gets `scrollbar-width: thin`.
- **Pinned heads** (U9): a content page's head pins and draws its hairline only once something has scrolled under it; a resource head pins as name row + tabs while its words scroll away; an event list's day heading sticks under whichever head is pinned. Past one screen of scroll, the shell's back-to-top button appears (U18).
- Neutral surfaces; saturation mainly for state and active interaction.
- Accent color is for selection, focus and primary action, not decoration.
- Mono only for values/code/IDs/addresses where mono carries meaning.
- Prefer dividers to nested cards.
- Avoid giant outer cards around an entire workspace.
- Avoid gratuitous shadows, gradients, pills, rounded boxes, and decorative containers.
- Keep controls compact but visibly interactive.
- Do not introduce Unicode icon substitutes if an existing sprite/icon is available.

A page should visually feel like another tool inside Swiss, not a separate embedded application. The concrete token names, the rules and the component vocabulary live in §15–§19.

## 10. Responsive behavior

Do not solve narrow widths by inventing a different navigation hierarchy.

- Preserve L1/L2/L3 semantics.
- Let dense body layouts adapt or stack as needed.
- Keep the current location and Focus control reachable.
- Avoid horizontal app-level overflow.
- Do not hide essential navigation merely because the viewport shrinks.

## 11. Plugin scalability

The UI must continue to work when Swiss has many plugins.

- Rail is a pinned shortlist, not the complete registry.
- `More` / Plugin Palette is the scalable discovery surface.
- Adding plugin pages must not consume more L1 rail seats.
- A plugin with 2–8 pages still uses the same Context Bar page switcher.
- Do not solve growth by making the rail wider or adding a second permanent sidebar for app navigation.

## 12. Implementation discipline for this repository

When implementing Swiss panel changes:

1. Read `AGENTS.md` first.
2. **Start from `ui/index.ts` and the gallery (`/admin/ui.html`)** — the shape probably exists, in every state. Then read the page's own module under `crates/swiss-panel/panel/src/` for its wiring.
3. Respect plain ES modules: no bundler/framework migration for a UI cleanup.
4. **Compose the library**; reuse `page-registry`, the rail, icons and tokens. A shape the library lacks goes into the library first (§0), with its gallery section and test, before the page uses it.
5. A behavior change ships with Vitest coverage under `crates/swiss-panel/panel/test/`.
6. Update design docs when an old decision is superseded; do not leave contradictory “implemented” rules as the apparent source of truth.
7. Preserve deep links and compatibility unless there is a concrete reason not to.
8. Do not change API contracts merely to make frontend rendering easier when the existing descriptors already express the needed structure.
9. Code comments are English; project design prose follows repository conventions.
10. Run the relevant panel tests, then the repository gates required by `AGENTS.md`.
11. **Migrate a page, delete its old CSS and class names in the same change** (U16): no compatibility class left behind, and the G5 / G7 / G4 rows go down with it.

## 13. UI review checklist

Before accepting any UI change, answer all of these:

- Is this L1, L2, or L3 navigation?
- Is that same level represented the same way everywhere else?
- Who owns this control: shell, page, resource, or row?
- Did this page create its own app chrome?
- Does switching plugins cause the body to jump vertically?
- Is a dropdown visibly a dropdown rather than a breadcrumb-looking mystery control?
- Is a segmented control being used for a true page-local sibling view rather than plugin-page navigation?
- Does the plugin need more than 5 pages? If its context bar needs a second row — or the ⋯ seat at normal widths — L3 content is being spent on L2; restructure the pages.
- Is a large card merely decorating a workspace that already has an app boundary?
- Is Focus Mode available in the same place on every page?
- Does Focus Mode exit from the same spatial anchor?
- Would this still work with 30 plugins?
- Would a new plugin author know exactly where their L1/L2/L3 UI belongs?
- Is every shape on the page a `ui/` function? Is there a new component/token where an existing Swiss primitive would suffice?
- Did the change add/update tests for the behavior it changed?

If any answer exposes inconsistent hierarchy or ownership, fix the architecture before polishing CSS.

## 14. Decision priority

When rules conflict, use this order:

1. Repository security/correctness rules in `AGENTS.md`.
2. Information hierarchy and navigation ownership in this skill.
3. Interaction consistency and accessibility.
4. Existing Swiss design tokens/primitives.
5. Local page aesthetics.

Do not preserve an old visual decision merely because it already exists if it violates the higher-level hierarchy above. Update the implementation, tests, and design documentation together so the repository has one current rule.

## 15. The design language

One process, one loopback port, one panel. The panel is the product's face and it is held to the same four properties as the binary (AGENTS.md): small, plugin-shaped, hot-pluggable, honest about where a tool came from. Pixels are as accountable as bytes. This skill is the working manual for them. The normative text is SPEC §panel — §panel.design (the direction and the tokens), §panel.groups (the grouped page), §panel.ui (the library and direction B) — with SPEC §host.groups for groups and hierarchy; `styles/base.css` (the tokens), `styles/ui.css` and the vitest gates are what the CSS actually says today. When this file and SPEC disagree, the code decides which one is right (SPEC §about) and both are fixed in the same change.

### Where it lives, how it changes

- Source: TypeScript in `crates/swiss-panel/panel/src/` with the emit committed under `src/admin_assets/js` (ADR-024, `npm run build`; never hand-edit the emit). The library is `panel/src/ui/`; the gallery is `admin_assets/ui.html` + `ui-gallery.ts` + `ui-scenes.ts`; the SVG sprite lives in `index.html` (the gallery borrows it at boot).
- CSS is three layers, loaded in this order (U3): `styles/base.css` — tokens, reset and the shell (rail, context bar, sidebar); `styles/ui.css` — every library component's classes; `styles/views.css` — only a workspace's skeleton and page-local layout the library has no shape for.
- No bundler, no framework, no npm dependency, no preprocessor, no web font download at runtime. A visual change that needs a build step beyond the emit is the wrong change.
- Type: the system stack (`--sans`), Inter first where it is installed but never shipped; the design is judged in Segoe UI Variable (Windows) and SF (macOS) (U6). `--mono` is the system mono stack.
- Tokens are the block at the head of `styles/base.css`; the header comment there states the rules everything below derives from. Read it before adding a value — the value probably exists. A new component size lands there, named, before any rule uses it (G4).
- Verify on a test instance (19998, or another port from `scripts/test-instance.ps1`) with the agent-browser skill: light and dark, English and Chinese, at ~1440px and ~960px. 19999 is the user's; never touch it.

### The shape: structure over palette

The idiom is a management UI in the Apple System Settings / Linear lineage: a source list beside a detail pane for the thing with many instances (MCP), a left-aligned page under a measure for the rest, grouped inset lists, one primary action per view with the rest behind `⋯`, a five-step type ramp, four weights, a 4pt grid, hairlines instead of shadows. Restraint is the style. Not glass, not gradients, not large radii, not a second accent.

The taste behind it, in one paragraph (SPEC §panel.groups is the worked example): **a thing on screen is one surface with one edge.** A container is a band over its rows, not a label plus a line plus a card; a page is header, form, list with a full step of air between them, never a stack of blocks touching; the controls of one kind sit in one column so the eye finds them once. Whatever the user does most — click `+`, pick up a group, read a row — is reachable without hovering to discover it, and whatever they do rarely or destructively waits behind `⋯`. The page teaches by its structure; prose that explains a gesture is a sign the structure failed.

**Tokens, by name (never by literal):** type `--f-title 22 / --f-head 15 / --f-body 13 / --f-label 12 / --f-caption 11`; weights `--w-body 400 / --w-name 450 / --w-emph 500 / --w-title 600`; spacing `--s1..--s8` (4/8/12/16/20/24/32); radii `--r-card 8 / --r-row 6 / --r-btn 6 / --r-pill`; component sizes `--ic-s 12` (a chevron) `/ --ic-m 14` (a glyph in a button) `/ --dot 6 / --row-h 42`; surfaces `--bg --sidebar --bar --card --field`; text `--text --text-2 --text-3`; lines `--sep --sep-soft`; `--hover`; `--accent`; state `--green --red --amber`; syntax `--syn-*` (code blocks only, rule 2); terminal `--term-*` (derived from the panel's colours, U12); measures `--measure 920 / --measure-wide 1180`. Dark is the same names under `:root[data-theme="dark"]` — write a rule once, in tokens, and it is themed.

## 16. The rules

1. **Monospace is for values you would copy** — a path, a command, a key, JSON, a tool name. Never for identity that is prose (titles, labels, descriptions, the panel's own words). `tnum` for every number that sits in a column.
2. **Saturation is for state.** Dots, the one accent, error red, amber. Type chips, launch-method tags, group names and everything descriptive are monochrome, and a column's annotation (a count, a unit, a time) is grey `--text-3`. A colour that carries no information is noise; `mysql` and `redis` both being red taught that. The one exception is syntax colour inside a code block (the JSON view; the SQL highlighter): there the hue says which token a character belongs to, which IS information. It uses only the five muted `--syn-*` tokens, never on chrome, labels or rows.
3. **Text needs a measure.** Nothing is full-bleed except a workspace. `--measure` by default, `--measure-wide` for genuinely wide rows (logs, traffic). Content hugs the left edge; it is never centred a second time inside the pane.
4. **One primary action per view; the rest go behind `⋯`.** A row holds at most one non-icon `.btn` (`row({ primary })` takes one). Red never appears on a row — destructive items live in the overflow menu, last, after a separator, in `--red` text, or as the row's trailing icon glyph.
5. **Hierarchy is drawn with surface and indent, not with size and caps.** A user-named container is a header **band** over its members — one shape everywhere (SPEC §panel.groups, §19): a `--sep-soft` band, `--r-row` corners, chevron + name (`--f-body`, `--w-emph`, mixed case) + count (`--text-3` tnum), `+` and `⋯` at its end. No folder glyph, no guide line, no second surface under the head: the band *is* the container marker. In the **sidebar** the band is 28px and the members sit one grid step (`--s4`) in beneath it; on a **page** the group *is* the card, the band is 36px across its top and the rows run edge to edge under it. Uppercase 11px captions are for *section* titles the product wrote ("Scheduled commands"), never for containers the user named. `ui/group.ts groupNode` is the only implementation of it (mounted by `groups.ts`).
6. **Every "new" says where it goes.** A create sheet or inline form has a Group field — a select over the scope's groups plus "New group…" — prefilled from the header `+` that opened it or from the last group used; the sheet title carries the group ("New job in *learn*"). Nothing lands in a group silently.
7. **One glyph, one meaning per page.** Two `plus` icons on one screen must do the same thing. A different act gets a different icon (`folder-plus` for a new group, `plus` for a new item).
8. **One persistent glyph per container header, and the whole header drags.** `+` stays visible (dimmed) because adding is frequent; `⋯` appears on hover/focus. The header itself is the drag surface for reordering groups (SPEC §panel.groups) — pick it up by the name, the count or the empty band; there is no grip to find. The buttons on it cancel the drag at `dragstart` (`groups.ts wireHeadDrag`), so a twitch while clicking `+` or `⋯` still lands the click. Any new draggable container follows the same two rules: draggable whole, buttons opt out.
9. **Icons are the sprite.** Lucide-style, 24 viewBox, 1.5 stroke, `currentColor`, hand-written paths in `index.html`, used through `iconNode(name)` (`ui/icon.ts`; an `HChild`, built with `h()`/`fill()`). No Unicode glyphs as icons, no icon packages. Adding one icon means adding one `<symbol>`; the gallery's icon section shows every symbol.
10. **Status is a dot plus neutral text.** `dot(state, words)`: 6px; filled green up, filled red down/error, hollow ring for "not running, will start on demand" — the shape says idle, not a colour. Every dot carries a `title` that says the state in words. A failure in a list is a red `tag()`, not a red row.
11. **Empty states use one template** — `emptyNode({icon, title, hint, action})` (`ui/page.ts`): an icon in `--text-3`, an `--f-head` title, an `--f-label` hint under 44ch, an optional ghost action (`[data-empty-action]`). An empty *container* is not an empty state: it shows one quiet row ("No items — drop here or press +") the height of a real row, so it still reads as a place and a drop target. A list filtered to nothing, or a page past the end, is one `note()` line.
12. **Surfaces are a grey ladder, not a shadow stack.** Cards are lifted by a hairline ring only; shadows belong to floating layers (sheet, popup menu, back to top). Dark mode is a warm near-black that reads as paper, with brightness as the ladder.
13. **Motion is 150ms ease or nothing**, and nothing under `prefers-reduced-motion`. Chevrons rotate; back to top glides; nothing slides, bounces or fades in.
14. **Copy is short and declarative, in both languages.** Titles name the thing ("Tunnels"), descriptions say what it does, hints say what to do next. No exclamation marks, no "please", no emoji. Confirmations state consequences and what is *not* destroyed: "Its 3 jobs move to 'default'. Nothing is removed." Every visible string goes through `tr()` (SPEC §panel.i18n); a value the same in every language (a port, a table name) is a named constant.
15. **Selection is a bar and a tint**, not a floating card: 2px `--accent` on the leading edge over an 8% accent tint; the selected name goes `--w-emph`, not blue.
16. **One surface, one edge.** A container is drawn once — a band over its members, a ring around a card — never as a label *and* a rail *and* a box that the reader must reassemble into one thing. If a group needs a folder glyph or a guide line to be recognised as a group, its surface is wrong; fix the surface, do not add a hint.
17. **Air is a token step, never zero.** Between the page header and what follows, and between a form and the list it feeds, `--s5`; between sections `--s6`; between two cards in one section `--s3`; between stacked fields and pairs `--s3`; between a band and its first member `--s1`. Two blocks touching means one of them has no rule — the Add sheet's pairs one div deeper than `.sheet-body` stacked with no gap until `.two` became a grid everywhere (SPEC §panel.pages) — so find the missing rule rather than nudge a margin.
18. **Trailing controls share one column.** A row's last glyph and its band's last glyph sit at the same x, so every `⋯` on a page is found once. Actions gather at the right edge; nothing trails halfway across a row.
19. **The frequent gesture needs no discovery.** Anything done often — add, collapse, pick up and move — is reachable from the visible surface the user is already looking at, at the size a hand hits: the whole band drags, `+` is always shown. Hover-only affordances are for the rare (`⋯`), never for the primary way to do something. A control that must be hunted for is a control the user will report as missing.
20. **Structure teaches; prose confirms.** A page description is **one sentence, one line** at `--measure` (U11) saying what the page is. It never explains an interaction ("drag a row to reorder") — if the list does not make that obvious, the list is the bug. Placeholders are sentence case and name the value ("Label, e.g. claude-code"), and a form must never say *optional* about a field the store will refuse.
21. **Four weights.** `--w-body`, `--w-name`, `--w-emph`, `--w-title` — a fifth weight is a fifth meaning nobody can read (U5, G3).
22. **Lines are inset, lists are open.** A separator inside a card starts at the text column; the event list draws no box and no row lines (the open row is the card); a data grid draws no vertical rules (U7).
23. **The head stays.** A content page's head and a resource's name row + tabs pin while the body scrolls; the hairline under them appears only once content is under it (U9). Nothing on the page re-implements a sticky header.
24. **Every "this happened" list is the event list** — `timeline()` (U10): the time in its own column (the date is a day heading, never repeated per row), the title, one line of arguments, the who column only when it varies, ×N for consecutive identical items, a failure as a red tag, the duration right-aligned (amber past a second). What every row says the same (the transport, the client, the size) moves into the open row's meta line (`timelineMeta`). MCP Logs is the reference; Traffic, Remote runs and a job's runs follow it.
25. **One screen never repeats itself.** No count the context bar already shows, no per-row meta that is the same on every row, no caption that repeats the tab it sits under. If the eye has read it once, the screen does not say it again.

## 17. Component vocabulary

Use these words in specs and class names; if a design needs a word not here, add it to the library (§0) and to this table first.

| Word | Function (`ui/`) | Classes (`ui.css` unless noted) |
| --- | --- | --- |
| **rail** | shell (`page-registry.ts`, `index.html`) | `base.css .rail*` |
| **plugin palette** | shell (`plugin-palette.ts`) | `.pal*` |
| **context bar** | shell (`page-registry.ts`) | `base.css .ctxbar` |
| **pane** | `pane({ wide, full })`, `paneBody({ wide })` | `.pane`, `.wide`, `.pane.full` |
| **page head** | `paneHead({ desc, sub, actions })` — pins on a content page | `.pane-head`, `.pane-desc`, `.pane-sub`, `.pane-actions` |
| **resource head** | `resHead({ title, desc, sub, actions, nav })` — name row + tabs pin | `.pane-head.res`, `.res-meta`, `.pane-nav` |
| **section** | `section({ cap, note, tools }, …body)` | `.sec`, `.sec-head`, `.sec-cap`, `.sec-tools` |
| **card** | `card(…rows)` | `.group` (historical name) |
| **row** | `row({ lead, name, sub, err, cols, toggle, primary, more, detail })` — `detail` opens the item's record in place | `.lrow*`, `.lrow-disc` |
| **label / value row** | `kvRow(label, value, { mono })` | `.kv*` |
| **source-list row** | `sideRow({ name, lead, tail, selected })` | `base.css .side-row` |
| **band / group** | `groupNode(...)` (mounted by `groups.ts`) | `.grp`, `.grp-head` |
| **segmented control** | `seg(items, current, { key, label })` — L3 only (§5) | `.seg` |
| **event list** | `timeline(items, { open, body })`, `timelineMeta(parts)`, `timelineToggle()` | `.tl*` |
| **code block** | `jsonCodeNode(value, all, { oneLine })`, `textNode()`; `valueBlock({ label, notes, tools, text }, …body)` around it | `pre.jv`, `pre.logs`, `.vblock*` |
| **form** | `form`, `field({ label, control, required, meta, hint })`, `checkField`, `pair`, `formActions`, `hint` | `.form`, `.fld`, `label.field`, `label.check`, `.two`, `.form-actions`, `.hint` |
| **inline form** | `inlineForm(…controls)` — create in place (Tokens, Secrets) | `.inline-form` |
| **filter** | `filterInput({ placeholder, label })` — in a section's tools | `input.filter` |
| **pager** | `pager({ label, status, prev, next })` | `.pager`, `.pager-status` |
| **note** | `note(body, { busy, err })`, `failNote({ text, why, action })` | `.note`, `.fail-note` |
| **status** | `dot(state, words)`, `heldDot(words)` (writes held until Commit), `tag(text, { mono, tone })`, `spinner()` | `.dot`, `.db-tab-dot`, `.tag`, `.spin` |
| **switch** | `sw(on, label)` | `.sw` |
| **buttons** | `btn(label, { kind, icon })`, `iconBtn(icon, label)`, `moreBtn(label)` | `.btn` |
| **select** | a native `<select>` styled by `styleSelect` / `initSelects` | `.dd*` |
| **popup menu** | `popupMenu(anchor, items)` (floating), `anchoredMenu(host, items)` (hung in a head's actions) | `.menu` |
| **sheet** | `sheet({ title, sub, body, foot })` + `showSheet` (`sub`: the mono value it acts on), `openFieldSheet(spec)` | `.sheet*`, `.backdrop` |
| **empty state** | `emptyNode({ icon, title, hint, action })` | `.empty` |
| **page foot** | `pageFoot({ note, rev })` | `.page-foot` |
| **back to top** | `toTop(scroller)` — mounted once by the shell | `.btn.to-top` |
| **toast** | shell (`util.ts toast()`) | `.toast` |
| **gallery** | `/admin/ui.html` — every export above, every state; scenes are designs | — |

## 18. Checklist before you call it done

- `npm run check` in `crates/swiss-panel/panel` is green: typecheck ×2, lint, emit freshness, vitest — the seven gates are inside it. When a file drops in G5, G4 or G7, lower its row in the same change.
- If `ui/` or `ui.css` changed: the gallery (`/admin/ui.html`) walked with real clicks, a screenshot in light and one in dark; the new shape is in a gallery section and in `ui-components.test.ts`.
- Every colour, size, weight and radius is a token; nothing new outside the token block (G4 says so).
- Both themes screenshotted; nothing is defined only under dark.
- No new Unicode icon, no `innerHTML` of a glyph; every new icon is a `<symbol>`.
- No row with two non-icon buttons; no red word button inside a row — the second action and every destructive one live behind the row's `⋯` (danger last) or are its trailing glyph.
- Vertical rhythm (§19, rule 17): nothing is glued to the block above it.
- Every new list that a user can add to shows which container the item joins before submit.
- Group headers and section captions are not confused: user-named → band + mixed case; product-named → uppercase caption.
- Every new `.dot` has a `title`; every icon-only button has `aria-label`.
- Keyboard: focus visible, Escape closes, Enter submits the sheet's primary, arrows move where a list is a listbox; a layer that handles a key stops its propagation (the shell listens at document).
- 960px width: the rail does not clip, the context bar stays, rows wrap their sub-line instead of overflowing.
- Chinese walked too (文/A): the words fit, nothing reads as English on a Chinese screen.
- The CSS comment beside a changed rule still states the *reason*; if the reason changed, the comment changed.
- And §13 above — hierarchy and ownership first, polish second.

## 19. Reference anatomy: the grouped list page (SPEC §panel.groups, 2026-09-18; SPEC §panel.ui)

The one shape every grouped page is built from; rules 16.16–16.20 are what it obeys. Build it from the library (the gallery's content scene is this page); do not reinterpret it.

```text
Page body (Content template, §6A) — .wide measure
┌──────────────────────────────────────────────────────────────────────────────┐
│ pane-desc: one sentence, one line, --text-2                  [⊞]  [Primary] │  paneHead — PINNED; its hairline shows once
│ pane-sub: one status line, --f-label                                         │  scrolled. [⊞] = New group, the folder-plus iconBtn
│                                        ─ --s5 ─                              │
│ [ Field ………………………………………… ] [ Field …… ] [ group ▾ ] [ Store ]                 │  inlineForm (only pages that create in place)
│                                        ─ --s5 ─                              │
│ ┌ groupNode (page density) ──────────────────────────────────────────────┐   │
│ │ ▾ default  3                                                   +   ⋯  │   │  36px band, --sep-soft; whole band drags
│ ├────────────────────────────────────────────────────────────────────────┤   │
│ │ ● name                                                 [One btn]   ⋯  │   │  row(); the ⋯ column lines up with the band's
│ │   mono sub-line                                                        │   │
│ │   ├────────────────────────────────────────────────────────────────────┤   │  the separator is INSET: it starts at the text
│ │ ○ name                                                 [One btn]   ⋯  │   │  column, past the dot (U7)
│ └────────────────────────────────────────────────────────────────────────┘   │
│                                        ─ --s4 ─                              │
│ ┌ groupNode (page density) ──────────────────────────────────────────────┐   │
│ │ ▾ test  0                                                      +   ⋯  │   │
│ ├────────────────────────────────────────────────────────────────────────┤   │
│ │   No items — drop here or press +                                      │   │  42px, --f-label, --text-3: an empty row, not a caption
│ └────────────────────────────────────────────────────────────────────────┘   │
│ page foot: prose left, the revision (mono) right                             │  pageFoot
└──────────────────────────────────────────────────────────────────────────────┘

Sidebar (source list) — the same band, smaller, no card
┌ groupNode (side density) ──────────┐
│ ▾ default  9                 +  ⋯  │  28px band, --sep-soft, --r-row
│   ● mysql                      ⌗   │  sideRow() 30px, one grid step (--s4) in
│   ● redis                      ⌗   │
└────────────────────────────────────┘  groups --s2 apart
```

- **Where things go.** Page-level actions (New group as the `folder-plus` icon button, then the sheet-opening primary) in `pane-actions` at the header's right; the in-place primary (Create / Store) at the end of the inline form; per-row: at most one `.btn` plus `⋯`; per-group: `+` (always) and `⋯` (hover) on the band.
- **Drag.** The band moves the group: grab cursor on it, buttons opt out at dragstart. A group lands before/after the *whole* group under the pointer; a row lands on another row (reorder + re-home), on a band or on an empty line (append). Feedback: `.grp.drop-before/after` edge on the block, `.drop-into` ring on the band or the empty line, `.grp.dragging` dims the whole group.
- **Alignment contract** (the groups section of `ui.css` states the numbers): the band's chevron sits in the rows' dot column and its name over their names on the card; in the sidebar a member's dot sits under the band's name.
- **What was tried and rejected.** A transparent tree head with a folder glyph and a guide line (SPEC §panel.groups first revision): three visual things for one container, and at page width the `+` a screen from its name. A hover-only grip as the drag handle: the user could not find it. A red `Delete`/`Revoke` on every row: noise on a page whose rows are read far more often than deleted. A hand-written HTML mock with its own `.m-*` style sheet as "the design" (SPEC §panel.ui directions): a second style system to reconcile; designs are gallery scenes now.

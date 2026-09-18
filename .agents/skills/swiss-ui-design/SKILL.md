---
name: swiss-ui-design
description: Enforce Swiss's UI architecture and visual consistency whenever designing, reviewing, implementing, refactoring, or fixing the Swiss panel UI, navigation, page layouts, plugin pages, toolbars, sidebars, focus/fullscreen behavior, responsive behavior, or visual components. Also the design language of the product — tokens, colours, spacing, icons, sheets, empty states, component vocabulary — and the checklist any panel change must pass. Invoke automatically for any Swiss UI/UX task.
---

# swiss-ui-design

Use this skill for every UI/UX change in the **swiss** panel. Its purpose is not to make every page look identical. Its purpose is to make every page obey the same information hierarchy, navigation ownership, action placement, layout vocabulary, and interaction model.

The core rule is:

> **Same information level = same UI mechanism. Different content type may use a different body layout, but it must remain inside the same App Shell.**

Before changing UI, read the repository `AGENTS.md` and the relevant existing panel code. Do not design from screenshots alone when source is available.

This skill also absorbs the former `swiss-design` skill: the design language, the token names, the fifteen rules and the component vocabulary now live in §15–§18. Historical references elsewhere to "swiss-design rule N" mean §16 below.

## 1. Fixed information hierarchy

Swiss has four UI levels. Do not collapse them or represent the same level differently on different pages.

```text
L0  App
    swiss

L1  Plugin
    MCP / Tunnels / Data / Jobs / Terminal / Gateway / ...

L2  Plugin Page
    MCP      -> Servers / Traffic / Token
    Tunnels  -> SSH Connections / Port Forwards
    Gateway  -> Plugins / Secrets
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
- app-level responsive behavior

A plugin/page owns only its Page Body.

Never let a page replace or recreate app chrome.

## 3. Plugin Rail rules

The left rail is the only L1 navigation.

- One seat per pinned plugin.
- `More` opens the complete/searchable plugin list.
- Active plugin uses the existing restrained accent treatment.
- Plugin pages never appear as separate rail seats.
- A disabled/unavailable plugin remains reachable and is marked unavailable instead of disappearing.
- Do not create another plugin navigation row inside the page.

## 4. Context Bar rules

The Context Bar is **always present in normal mode**, including single-page plugins and workspace pages.

Its job is to answer:

1. Where am I?
2. If this plugin has multiple pages, how do I switch pages?
3. What global/page-level status is relevant?
4. Where is Focus Mode?

### Multi-page plugin

Render a stable plugin label plus an obvious page switcher:

```text
MCP / [ Servers ▾ ]                         8 MCPs · 4 up   [⛶]
Tunnels / [ SSH Connections ▾ ]             3 connections  [⛶]
Gateway / [ Plugins ▾ ]                     6 plugins       [⛶]
```

The current page control must look interactive. Do not disguise it as a breadcrumb with a vague chevron.

### Single-page plugin

Keep the same bar height and alignment, but do not render a fake dropdown or redundant `Data / Data`:

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
- `Gateway -> Plugins / Secrets`: Context Bar.

Do **not** implement these as page-local segmented tabs.

Use page-local tabs only for L3 sections of the current resource, e.g.:

```text
shop
[Tools] [Resources] [Prompts] [Run] [Config] [Logs]
```

These are valid because they operate within one selected MCP server.

## 6. Page-body layout vocabulary

Unified UI does not mean every body is the same. Every page must choose one of a small set of body templates.

### A. Content

For list/settings/management pages.

Examples: Tunnels page, Gateway Plugins, Jobs, Traffic, Token.

Characteristics:

- normal page padding
- optional readable measure
- page actions aligned consistently
- one primary action at most unless the workflow genuinely requires more

### B. Split / Resource

For master-detail workflows.

Examples: MCP Servers.

```text
resource list | resource detail
```

The left pane is part of the Page Body, not app navigation.

### C. Workspace / Full-bleed

For dense tools that need all remaining space.

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
- Existing type scale.
- Hairline separators for structural boundaries.
- Neutral surfaces; saturation mainly for state and active interaction.
- Accent color is for selection, focus and primary action, not decoration.
- Mono only for values/code/IDs/addresses where mono carries meaning.
- Prefer dividers to nested cards.
- Avoid giant outer cards around an entire workspace.
- Avoid gratuitous shadows, gradients, pills, rounded boxes, and decorative containers.
- Keep controls compact but visibly interactive.
- Do not introduce Unicode icon substitutes if an existing sprite/icon is available.

A page should visually feel like another tool inside Swiss, not a separate embedded application. The concrete token names, the fifteen design rules and the component vocabulary live in §15–§18.

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
2. Inspect `crates/swiss-panel/src/admin_assets/` before proposing new primitives.
3. Respect plain ES modules: no bundler/framework migration for a UI cleanup.
4. Reuse existing `page-registry`, menu, rail, icon, spacing and color mechanisms where possible.
5. A behavior change ships with Vitest coverage under `crates/swiss-panel/panel-tests/`.
6. Update design docs when an old decision is superseded; do not leave contradictory “implemented” rules as the apparent source of truth.
7. Preserve deep links and compatibility unless there is a concrete reason not to.
8. Do not change API contracts merely to make frontend rendering easier when the existing descriptors already express the needed structure.
9. Code comments are English; project design prose follows repository conventions.
10. Run the relevant panel tests, then the repository gates required by `AGENTS.md`.

## 13. UI review checklist

Before accepting any UI change, answer all of these:

- Is this L1, L2, or L3 navigation?
- Is that same level represented the same way everywhere else?
- Who owns this control: shell, page, resource, or row?
- Did this page create its own app chrome?
- Does switching plugins cause the body to jump vertically?
- Is a dropdown visibly a dropdown rather than a breadcrumb-looking mystery control?
- Is a segmented control being used for a true page-local sibling view rather than plugin-page navigation?
- Is a large card merely decorating a workspace that already has an app boundary?
- Is Focus Mode available in the same place on every page?
- Does Focus Mode exit from the same spatial anchor?
- Would this still work with 30 plugins?
- Would a new plugin author know exactly where their L1/L2/L3 UI belongs?
- Is there a new component/token where an existing Swiss primitive would suffice?
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

One process, one loopback port, one panel. The panel is the product's face and it is held to the same four properties as the binary (AGENTS.md): small, plugin-shaped, hot-pluggable, honest about where a tool came from. Pixels are as accountable as bytes. This skill is the constitution for them; docs/17 (direction), docs/18 (the refresh that set the tokens), docs/20 (groups and hierarchy) are its case law, and `.agents/docs/style-design.md` is the audited inventory of what the CSS actually says today. When this file and a docs/NN disagree, this file wins — and the docs/NN gets fixed.

### Where it lives, how it changes

- Source: `crates/swiss-panel/src/admin_assets/` — two CSS files (`styles/base.css`, `styles/views.css`), native ES modules in `js/`, an inline SVG sprite in `index.html`. It is edited here directly; there is no sibling checkout and no copy-back step.
- No bundler, no framework, no npm dependency, no preprocessor, no web font download at runtime. A visual change that needs a build step is the wrong change.
- Tokens are the block at the head of `styles/base.css`; the header comment there states the three rules everything below derives from. Read it before adding a value — the value probably exists.
- Verify on 19998 with the agent-browser skill, light and dark, at ~1440px and at ~900px width. 19999 is the user's; never touch it.

### The shape: structure over palette

The idiom is a management UI in the Apple System Settings / Linear lineage: a source list beside a detail pane for the thing with many instances (MCP), a left-aligned page under a measure for the rest, grouped inset lists, one primary action per view with the rest behind `⋯`, a five-step type ramp, a 4pt grid, hairlines instead of shadows. Restraint is the style. Not glass, not gradients, not large radii, not a second accent.

**Tokens, by name (never by literal):** type `--f-title 22 / --f-head 15 / --f-body 13 / --f-label 12 / --f-caption 11`; spacing `--s1..--s8` (4/8/12/16/20/24/32); radii `--r-card 8 / --r-row 6 / --r-btn 6 / --r-pill`; surfaces `--bg --sidebar --bar --card --field`; text `--text --text-2 --text-3`; lines `--sep --sep-soft`; `--hover`; `--accent`; state `--green --red --amber`; measures `--measure 920 / --measure-wide 1180`. Dark is the same names under `:root[data-theme="dark"]` — write a rule once, in tokens, and it is themed.

## 16. The rules

1. **Monospace is for values you would copy** — a path, a command, a key, JSON. Never for identity (names, titles, labels). `tnum` for every number that sits in a column.
2. **Saturation is for state.** Dots, the one accent, error red, amber. Type chips, launch-method tags, group names and everything descriptive are monochrome. A colour that carries no information is noise; `mysql` and `redis` both being red taught that.
3. **Text needs a measure.** Nothing is full-bleed except the terminal. `--measure` by default, `--measure-wide` for genuinely wide rows (logs, traffic). Content hugs the left edge; it is never centred a second time inside the pane.
4. **One primary action per view; the rest go behind `⋯`.** A row holds at most one non-icon `.btn`. Red never appears on a row — destructive items live in the overflow menu, last, after a separator, in `--red` text.
5. **Hierarchy is drawn with surface and indent, not with size and caps.** A user-named container is a header **band** over its members — one shape everywhere (docs/35, §19): a `--sep-soft` band, `--r-row` corners, chevron + name (`--f-body` 600 mixed case) + count (`--text-3` tnum), `+` and `⋯` at its end. No folder glyph, no guide line, no second surface under the head: the band *is* the container marker. In the **sidebar** the band is 28px and the members sit one grid step (`--s4`) in beneath it; on a **page** the group *is* the `.group` card, the band is 36px across its top and the rows run edge to edge under it. Never a transparent tree head — at page width its `+` drifts a screen from its name and nothing says where the group ends; in the sidebar it read as a label, a line and a card instead of one thing. Uppercase 11px captions are for *section* titles the product wrote ("Scheduled commands"), never for containers the user named. This is docs/20 §4 as revised; the group component `groups.js` is the only implementation of it.
6. **Every "new" says where it goes.** A create sheet or inline form has a Group field — a select over the scope's groups plus "New group…" — prefilled from the header `+` that opened it or from the last group used; the sheet title carries the group ("New job in *learn*"). Nothing lands in a group silently.
7. **One glyph, one meaning per page.** Two `plus` icons on one screen must do the same thing. A different act gets a different icon (`folder-plus` for a new group, `plus` for a new item).
8. **One persistent glyph per container header, and the whole header drags.** `+` stays visible (dimmed) because adding is frequent; `⋯` appears on hover/focus. The header itself is the drag surface for reordering groups (docs/35) — pick it up by the name, the count or the empty band; there is no grip to find. The buttons on it cancel the drag at `dragstart` (`groups.js wireHeadDrag`), so a twitch while clicking `+` or `⋯` still lands the click. Any new draggable container follows the same two rules: draggable whole, buttons opt out.
9. **Icons are the sprite.** Lucide-style, 24 viewBox, 1.5 stroke, `currentColor`, hand-written paths in `index.html`, used through `icon(name)`. No Unicode glyphs as icons, no icon packages. Adding one icon means adding one `<symbol>`; twenty-odd symbols under 4 KB is the budget.
10. **Status is a dot plus neutral text.** 6px; filled green up, filled red down/error, hollow ring (`.dot.idle`) for "not running, will start on demand" — the shape says idle, not a colour. Every dot carries a `title` that says the state in words.
11. **Empty states use one template** — `emptyHtml({icon, title, hint, action})`: an icon in `--text-3`, an `--f-head` title, an `--f-label` hint under 44ch, an optional ghost action. An empty *container* is not an empty state: it shows one quiet row ("No items — drop here or press +") the height of a real row, so it still reads as a place and a drop target.
12. **Surfaces are a grey ladder, not a shadow stack.** Cards are lifted by a hairline ring only; shadows belong to floating layers (sheet, popup menu). Dark mode is a warm near-black that reads as paper, with brightness as the ladder.
13. **Motion is 150ms ease or nothing**, and nothing under `prefers-reduced-motion`. Chevrons rotate; nothing slides, bounces or fades in.
14. **Copy is English, short, declarative.** Titles name the thing ("Tunnels"), descriptions say what it does in one sentence, hints say what to do next. No exclamation marks, no "please", no emoji. Confirmations state consequences and what is *not* destroyed: "Its 3 jobs move to 'default'. Nothing is removed."
15. **Selection is a bar and a tint**, not a floating card: 2px `--accent` on the leading edge over an 8% accent tint; the selected name goes 600, not blue.

## 17. Component vocabulary

Use these words in specs and class names; if a design needs a word not here, add it here first.

| Word | What it is | Where it lives |
| --- | --- | --- |
| **rail** | Global navigation: brand, one seat per pinned plugin, `...` opening the palette, the rail foot | `index.html`, `base.css .rail` |
| **plugin palette** | The rail's `...` seat: the searchable list of every plugin | `js/plugin-palette.js` |
| **context bar** | The always-present second row: `Plugin / [Page ▾]` switcher (or single-page location text), count chip, Focus at right | `index.html #ctxBar`, `js/page-registry.js`, `base.css .ctxbar` |
| **pane** | The content area under the bars | `views.css .pane` |
| **sidebar** | The 248px source list beside a detail pane | `base.css .side*` |
| **group** (`.grp`) | A user-named, ordered, collapsible container of rows: a header band (draggable whole) with `+` and `⋯`, over the members; 28px in the sidebar, 36px on the card it is at page density | docs/20 §4, docs/35, §19, `js/groups.js` |
| **inline form** (`.inline-form`) | Create-in-place instead of a sheet (Tokens, Secrets): ONE row of fields + the Group select + the one primary, `--s5` of air before the list; New group lives in the page header's `pane-actions` | docs/35 §3, `views.css .inline-form` |
| **section** | A product-named division of a page: uppercase caption, optional actions at right | `.sec-head`, `.sec-cap` |
| **card** (`.group` in CSS — historical) | A hairline-ringed white surface holding rows | `views.css .group` |
| **row** | One item: dot · name · mono sub-line · one primary `.btn` · `⋯` | `.row`, `.tun-row`, `.side-row` |
| **chip** | A monochrome mono tag (launch method, dialect) or the count chip | `.side-type`, context-bar chip |
| **sheet** | A modal form with head / body / foot; foot holds Cancel + one primary | `.sheet*` |
| **popup menu** | `popupMenu(anchor, items)`: 30px items, separators, danger last | `menu.js` |
| **segmented control** | L3 tabs inside one resource's detail (a server's Tools / Resources / Prompts); never plugin-page navigation (§5) | `.seg` |
| **empty state** | `emptyHtml(...)` | `util.js` |
| **toast** | One-line transient confirmation, bottom | `toast()` |

## 18. Checklist before you call it done

- Every colour, size and radius is a token; `grep -n "#[0-9a-f]\{6\}\|[0-9]\+px" styles/views.css` on your diff shows nothing new outside the token block.
- Both themes screenshotted; nothing is defined only under dark.
- No new Unicode icon, no `innerHTML` of a glyph; every new icon is a `<symbol>`.
- No row with two non-icon buttons; no `.btn.danger` inside a row — the second action and every destructive one live behind the row's `⋯` (`popupMenu`, danger last).
- Vertical rhythm on a list page (§19): header → `--s5` → inline form → `--s5` → list; cards `--s4` apart; nothing is glued to the block above it.
- Every new list that a user can add to shows which container the item joins before submit.
- Group headers and section captions are not confused: user-named → band + mixed case; product-named → uppercase caption.
- Every new `.dot` has a `title`; every `.ic`-only button has `aria-label`.
- Keyboard: focus visible, Escape closes, Enter submits the sheet's primary, arrows move where a list is a listbox.
- 900px width: the rail does not clip, the context bar stays, rows wrap their sub-line instead of overflowing.
- The CSS comment beside a changed rule still states the *reason*; if the reason changed, the comment changed.
- And §13 above — hierarchy and ownership first, polish second.

## 19. Reference anatomy: the grouped list page (docs/35, 2026-09-18)

The one shape every grouped page is built from. Copy it; do not reinterpret it.

```text
Page body (Content template, §6A) — .wide measure
┌──────────────────────────────────────────────────────────────────────────────┐
│ pane-desc: one or two sentences, --text-2, 68ch      [New group]  [Primary] │  pane-head; actions right
│ pane-sub: one status line, --f-label                                         │
│                                        ─ --s5 ─                              │
│ [ Field ………………………………………… ] [ Field …… ] [ group ▾ ] [ Store ]                 │  inline form (only pages that create in place)
│                                        ─ --s5 ─                              │
│ ┌ .grp.grp--page.group ──────────────────────────────────────────────────┐   │
│ │ ▾ default  3                                                   +   ⋯  │   │  36px band, --sep-soft; whole band drags
│ ├────────────────────────────────────────────────────────────────────────┤   │
│ │ ● name                                                 [One btn]   ⋯  │   │  row; the ⋯ column lines up with the band's
│ │   mono sub-line                                                        │   │
│ ├────────────────────────────────────────────────────────────────────────┤   │
│ │ ○ name                                                 [One btn]   ⋯  │   │
│ └────────────────────────────────────────────────────────────────────────┘   │
│                                        ─ --s4 ─                              │
│ ┌ .grp.grp--page.group ──────────────────────────────────────────────────┐   │
│ │ ▾ test  0                                                      +   ⋯  │   │
│ ├────────────────────────────────────────────────────────────────────────┤   │
│ │   No items — drop here or press +                                      │   │  42px, --f-label, --text-3: an empty row, not a caption
│ └────────────────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────────────┘

Sidebar (source list) — the same band, smaller, no card
┌ .grp.grp--side ────────────────────┐
│ ▾ default  9                 +  ⋯  │  28px band, --sep-soft, --r-row
│   ● mysql                      ⌗   │  .side-row 30px, one grid step (--s4) in
│   ● redis                      ⌗   │
└────────────────────────────────────┘  groups --s2 apart
```

- **Where things go.** Page-level actions (`New group`, the sheet-opening primary) in `pane-actions` at the header's right; the in-place primary (Create / Store) at the end of the inline form; per-row: at most one `.btn` plus `⋯`; per-group: `+` (always) and `⋯` (hover) on the band.
- **Drag.** The band moves the group: grab cursor on it, buttons opt out at dragstart. A group lands before/after the *whole* group under the pointer; a row lands on another row (reorder + re-home), on a band or on an empty line (append). Feedback: `.grp.drop-before/after` edge on the block, `.drop-into` ring on the band or the empty line, `.grp.dragging` dims the whole group.
- **Alignment contract** (base.css groups section states the numbers): the band's chevron sits in the rows' dot column and its name over their names on the card; in the sidebar a member's dot sits under the band's name.
- **What was tried and rejected.** A transparent tree head with a folder glyph and a guide line (docs/20 §4.1 first revision): three visual things for one container, and at page width the `+` a screen from its name. A hover-only grip as the drag handle: the user could not find it. A red `Delete`/`Revoke` on every row: noise on a page whose rows are read far more often than deleted.

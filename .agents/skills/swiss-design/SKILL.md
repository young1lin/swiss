---
name: swiss-design
description: Use before changing anything the swiss panel shows — a new page, list, sheet, row, button, icon, empty state, colour or spacing — or when writing a spec that has a visual side. The design language of the whole product, its component vocabulary, and the checklist a panel change must pass.
---

# The swiss design language

One process, one loopback port, one panel. The panel is the product's face and it is held to the
same four properties as the binary (AGENTS.md): small, plugin-shaped, hot-pluggable, honest about
where a tool came from. Pixels are as accountable as bytes. This skill is the constitution for
them; docs/17 (direction), docs/18 (the refresh that set the tokens), docs/20 (groups and
hierarchy) are its case law, and `.agents/docs/style-design.md` is the audited inventory of what
the CSS actually says today. When this file and a docs/NN disagree, this file wins — and the
docs/NN gets fixed.

## Where it lives, how it changes

- Source: `../local-mcp-gateway/src/admin/` (two CSS files, native ES modules, an inline SVG
  sprite in `index.html`). `crates/swiss-panel/src/admin_assets/` is a byte-for-byte copy; edit
  the sibling, copy the whole tree back, rebuild (`CARGO_TARGET_DIR=target-test`), restart 19998.
  The test `the_tree_is_byte_for_byte_the_node_builds` guards the copy.
- No bundler, no framework, no npm dependency, no preprocessor, no web font download at runtime.
  A visual change that needs a build step is the wrong change.
- Tokens are the first 110 lines of `styles/base.css`; the header comment there states the three
  rules everything below derives from. Read it before adding a value — the value probably exists.
- Verify on 19998 with the agent-browser skill, light and dark, at ~1440px and at ~900px width.
  19999 is the user's; never touch it.

## The shape: structure over palette

The idiom is a management UI in the Apple System Settings / Linear lineage: sidebar + detail pane
for the thing with many instances (MCP), a left-aligned page under a measure for the rest, grouped
inset lists, one primary action per view with the rest behind `⋯`, a five-step type ramp, a 4pt
grid, hairlines instead of shadows. Restraint is the style. Not glass, not gradients, not large
radii, not a second accent.

**Tokens, by name (never by literal):** type `--f-title 22 / --f-head 15 / --f-body 13 /
--f-label 12 / --f-caption 11`; spacing `--s1..--s8` (4/8/12/16/20/24/32); radii `--r-card 8 /
--r-row 6 / --r-btn 6 / --r-pill`; surfaces `--bg --sidebar --bar --card --field`; text
`--text --text-2 --text-3`; lines `--sep --sep-soft`; `--hover`; `--accent`; state
`--green --red --amber`; measures `--measure 920 / --measure-wide 1180`. Dark is the same names
under `:root[data-theme="dark"]` — write a rule once, in tokens, and it is themed.

## The rules

1. **Monospace is for values you would copy** — a path, a command, a key, JSON. Never for identity
   (names, titles, labels). `tnum` for every number that sits in a column.
2. **Saturation is for state.** Dots, the one accent, error red, amber. Type chips, launch-method
   tags, group names and everything descriptive are monochrome. A colour that carries no
   information is noise; `mysql` and `redis` both being red taught that.
3. **Text needs a measure.** Nothing is full-bleed except the terminal. `--measure` by default,
   `--measure-wide` for genuinely wide rows (logs, traffic). Content hugs the left edge; it is never
   centred a second time inside the pane.
4. **One primary action per view; the rest go behind `⋯`.** A row holds at most one non-icon
   `.btn`. Red never appears on a row — destructive items live in the overflow menu, last, after a
   separator, in `--red` text.
5. **Hierarchy is drawn with surface and indent, not with size and caps.** A container's header is
   a 28px (sidebar) / 36px (page) band on `--sep-soft`, its name in `--f-body` 600 mixed case,
   its count in `--text-3` tnum. Children sit on a transparent ground, indented 20px, with a 1px
   `--sep` guide line from the header's bottom edge to the last child. Uppercase 11px captions are
   for *section* titles the product wrote ("Scheduled commands"), never for containers the user
   named. This is docs/20 §4; the group component `groups.js` is the only implementation of it.
6. **Every "new" says where it goes.** A create sheet or inline form has a Group field — a select
   over the scope's groups plus "New group…" — prefilled from the header `+` that opened it or from
   the last group used; the sheet title carries the group ("New job in *learn*"). Nothing lands
   in a group silently.
7. **One glyph, one meaning per page.** Two `plus` icons on one screen must do the same thing. A
   different act gets a different icon (`folder-plus` for a new group, `plus` for a new item).
8. **One persistent glyph per container header.** `+` stays visible (dimmed) because adding is
   frequent; `⋯` and the drag grip appear on hover/focus. Buttons never live inside a draggable
   element — the grip is the only draggable part of a header, so a twitch while clicking cannot
   cancel the click.
9. **Icons are the sprite.** Lucide-style, 24 viewBox, 1.5 stroke, `currentColor`, hand-written
   paths in `index.html`, used through `icon(name)`. No Unicode glyphs as icons, no icon packages.
   Adding one icon means adding one `<symbol>`; twenty-odd symbols under 4 KB is the budget.
10. **Status is a dot plus neutral text.** 6px; filled green up, filled red down/error, hollow
    ring (`.dot.idle`) for "not running, will start on demand" — the shape says idle, not a colour.
    Every dot carries a `title` that says the state in words.
11. **Empty states use one template** — `emptyHtml({icon, title, hint, action})`: an icon in
    `--text-3`, an `--f-head` title, a `--f-label` hint under 44ch, an optional ghost action. An
    empty *container* is not an empty state: it shows one quiet row ("Empty — drop rows here or
    press +") so it still reads as a place.
12. **Surfaces are a grey ladder, not a shadow stack.** Cards are lifted by a hairline ring only;
    shadows belong to floating layers (sheet, popup menu). Dark mode is a warm near-black that
    reads as paper, with brightness as the ladder.
13. **Motion is 150ms ease or nothing**, and nothing under `prefers-reduced-motion`. Chevrons
    rotate; nothing slides, bounces or fades in.
14. **Copy is English, short, declarative.** Titles name the thing ("Tunnels"), descriptions say
    what it does in one sentence, hints say what to do next. No exclamation marks, no "please",
    no emoji. Confirmations state consequences and what is *not* destroyed: "Its 3 jobs move to
    'default'. Nothing is removed."
15. **Selection is a bar and a tint**, not a floating card: 2px `--accent` on the leading edge over
    an 8% accent tint; the selected name goes 600, not blue.

## Component vocabulary

Use these words in specs and class names; if a design needs a word not here, add it here first.

| Word | What it is | Where it lives |
| --- | --- | --- |
| **toolbar** | The top bar: brand, level-one tabs, memory chip, appearance | `index.html`, `base.css .toolbar` |
| **page bar** | The always-present 36px second row: the group's pages or the single page's name, count chip at right | docs/13 D5 as revised by docs/18 V3 |
| **pane** | The content area under the bars | `views.css .pane` |
| **sidebar** | The 248px source list beside a detail pane | `base.css .side*` |
| **group** (`.grp`) | A user-named, ordered, collapsible container of rows with a header band, `+`, `⋯`, grip; two densities | docs/20 §4, `js/groups.js` |
| **section** | A product-named division of a page: uppercase caption, optional actions at right | `.sec-head`, `.sec-cap` |
| **card** (`.group` in CSS — historical) | A hairline-ringed white surface holding rows | `views.css .group` |
| **row** | One item: dot · name · mono sub-line · one primary `.btn` · `⋯` | `.row`, `.tun-row`, `.side-row` |
| **chip** | A monochrome mono tag (launch method, dialect) or the count chip | `.side-type`, page-bar chip |
| **sheet** | A modal form with head / body / foot; foot holds Cancel + one primary | `.sheet*` |
| **popup menu** | `popupMenu(anchor, items)`: 30px items, separators, danger last | `menu.js` |
| **segmented control** | Tabs inside a page (`SSH Connections / Port Forwards`) | `.seg` |
| **empty state** | `emptyHtml(...)` | `util.js` |
| **toast** | One-line transient confirmation, bottom | `toast()` |

## Checklist before you copy the tree back

- Every colour, size and radius is a token; `grep -n "#[0-9a-f]\{6\}\|[0-9]\+px" styles/views.css`
  on your diff shows nothing new outside the token block.
- Both themes screenshotted; nothing is defined only under dark.
- No new Unicode icon, no `innerHTML` of a glyph; every new icon is a `<symbol>`.
- No row with two non-icon buttons; no `.btn.danger` inside a row.
- Every new list that a user can add to shows which container the item joins before submit.
- Group headers and section captions are not confused: user-named → band + mixed case;
  product-named → uppercase caption.
- Every new `.dot` has a `title`; every `.ic`-only button has `aria-label`.
- Keyboard: focus visible, Escape closes, Enter submits the sheet's primary, arrows move where a
  list is a listbox.
- 900px width: the toolbar does not clip, the page bar stays, rows wrap their sub-line instead of
  overflowing.
- The CSS comment beside a changed rule still states the *reason*; if the reason changed, the
  comment changed.

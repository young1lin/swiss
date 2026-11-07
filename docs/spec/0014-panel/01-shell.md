# The shell and the library

Part 01 of [§panel](README.md), chapter 0014 of the [swiss specification](../../SPEC.md).

## §panel.nav — Navigation, the shell and page layouts

**Four levels, each with one owner.** A page never invents another L1 or L2 mechanism.

| Level | What | Owner |
|---|---|---|
| L0 | The app | — |
| L1 | A plugin (MCP, Tunnels, Data, Jobs, Process, Terminal, Remote, Settings…) | The **rail** |
| L2 | A plugin's pages (MCP → Servers / Traffic / Token) | The **context bar** |
| L3 | Page-local navigation (an MCP's Tools / Resources / … ; Data's objects; Terminal's sessions) | The **page body** |

**Navigation is data.** The panel has no hard-coded page list: pages come from the plugin
descriptors (§host.plugins) through `GET /api/plugins`, each a `PageDescriptor` — `id`,
`pluginId`, `label`, `order`, `path` (the canonical hash route), `entry` (the view module's URL), `sidebar`,
`layout`. The host pages are added by the panel itself: System (order 1000), Plugins (1001)
and Secrets (1002), `pluginId: "host"`, shown as the Settings plugin. The built-in pages:

| id | Plugin | Label | Order | Layout |
|---|---|---|---|---|
| `mcps` | mcp | Servers | 10 | resource (`sidebar: true`) |
| `traffic` | mcp | Traffic | 20 | page |
| `tokens` | mcp | Token | 30 | page |
| `tunnels` | tunnels | SSH Connections | 30 | page |
| `tunnel-forwards` | tunnels | Port Forwards | 35 | page |
| `data` | data | Data | 40 | workspace |
| `jobs` | jobs | Jobs | 50 | page |
| `process` | process | Process | 55 | page |
| `terminal` | terminal | Terminal | 70 | workspace |
| `remote` | remote | Targets | 75 | page |
| `remote-runs` | remote | Runs | 76 | page |
| `system`, `plugins`, `secrets` | host | System, Plugins, Secrets | 1000–1002 | page |

- **Grouping** (`page-core.ts groupPages`): one group per plugin; a group sorts by the
  **smallest** order among its pages, pages sort by order inside it, ties keep registration
  order. A plugin that is disabled, failed, waiting for a dependency or not built keeps its
  pages registered and marked unavailable: its seat stays, and its page paints that state.
- **Routing** follows each descriptor's `path`: MCP's default Servers page is `#mcps`, with
  `#mcps-traffic` and `#mcps-tokens` as its sibling pages, matching Remote's `#remote` /
  `#remote-runs` convention. Stable ids (`traffic`, `tokens`) and module URLs stay unchanged;
  old `#traffic` / `#tokens` links resolve by id and are replaced by the canonical path,
  preserving the hash query. An unknown initial hash lands on the first page; no hash is
  `#mcps`. A `hashchange` to a registered path or id navigates; `canLeave()` returning false
  restores the active page's canonical path and stays. Page membership comes from
  `pluginId`, never from a route prefix.
- **Layouts** (`layoutOf`) frame the page body and nothing else — the shell always draws the
  rail and the context bar. `resource`: master-detail, the shell's sidebar holds the list;
  `page`: a content page under a measure; `workspace`: full-bleed, no padding, no measure, no
  scroll of its own. The descriptor states it; absent, `sidebar: true` means resource and
  anything else page. A workspace is never guessed, and every built-in states its layout.
- **`sidebar: true`** means "this page renders into the shell sidebar's layout" — only `mcps`
  does. The shell hides the sidebar for every other page.
- **A legacy manifest** (the seven pre-plugin pages) stands in when `/api/plugins` answers 404.
  The embedded panel always meets its own gateway, so the branch is dead weight kept for a
  debug build pointed at an old tree; a removal candidate.

**The shell** is invariant and owns: the rail, the context bar, the global readouts, Focus
mode, the pane's scrolling (its pinned-head measurements and back to top, `pane-scroll.ts`)
and app-level responsiveness. A page owns only its body.

**Desktop window.** The same panel gains `data-desktop` before paint only in `swiss gui`
(§host.gui). Browser and desktop share 14/13/12 px reading tokens (body/label/caption), with
secondary contrast defined per theme. The desktop rail is 64 px
with 48 px seats and 11 px captions (10 px floor), and the context bar is 44 px. The native
caption owns the app mark, so the desktop rail omits its duplicate brand mark. Secondary
text uses the shared theme tokens. The browser keeps its shell dimensions. `desktop.ts` follows
the panel's resolved theme and plugin title and handles desktop text-zoom chords; it adds no
listeners or native behavior to ordinary browser tabs.

```text
┌────────┬────────────────────────────────────────────────────┐
│ Plugin │ Context bar                                        │
│ rail   ├────────────────────────────────────────────────────┤
│        │ Page body                                          │
└────────┴────────────────────────────────────────────────────┘
```

**The rail** — the only L1 navigation.

- 56 px wide, no edge of its own (beside a page the ground change is the edge; beside the MCP
  sidebar the two are one surface). One 42 px seat per pinned plugin: an 18 px glyph over its
  name.
- **The caption is one size for the whole rail**: 10 px, stepped down together in half-pixel
  steps to a 9 px floor by `fitRailLabels` only when a served name is longer than the seat; a
  name that fits nowhere ellipsizes and keeps its full text in the seat's `title`. The rail
  is never icon-only.
- **Active seat**: the `--hover` ground plus a 2 px `--accent` notch on the leading edge — the
  chrome's one accent.
- **An unavailable plugin keeps its seat**, `aria-disabled`, the reason in its title; its page
  is the empty state, never a blank.
- **The pinned shortlist**: at most fifteen seats (`RAIL_LIMIT`), the choice stored in
  `swiss.rail.pinned`. The default seats every served group up to the cap — a new module is on
  the rail the moment it serves, not waiting in the palette for a pin; the default applies only
  while no choice is stored, and the first star toggle snapshots it. The cap binds saved pins
  too, and the star refuses a pin beyond it (disabled, the reason in its title) so a plugin is
  never starred without a seat. The `⋯` seat (glyph only) opens the **plugin palette** — the
  complete, searchable list. More plugins never widen the rail or add a second app sidebar.
- A seat carries `data-group` and `data-view`; which page a click opens is computed per click:
  the plugin's **last page** (`swiss.lastPage`, set on every navigation, `targetPageFor`), or
  its first page when the remembered one is gone.
- The brand mark is `logo.svg`. Plugin pages never appear as separate seats.

**The context bar** — always present in normal mode, 40 px, including single-page plugins
(the body's vertical origin never jumps between plugins).

- The title is the plugin's glyph (16 px) and name. A multi-page plugin adds
  `<nav id="pageTabs" aria-label="Pages">`: one underline tab per page, a real
  `<a class="ctx-tab" href="#id">` with `aria-current`, the current one marked by a 2 px accent
  on the bar's own bottom hairline. It is navigation, not a tablist. A single-page plugin
  draws the title alone — no one-entry tab strip.
- **Overflow**: `fitTabs()` (pure) plus a `ResizeObserver` move what does not fit into a `⋯`
  tab that lists every page; the current page is always visible.
- **At most five pages per plugin.** A plugin whose bar needs the `⋯` at normal widths is
  spending L3 content on L2; restructure its pages.
- **Readouts** at the right: the page's `countText()`, a `·`, the memory chip (mono,
  `--f-caption`), all `--text-3`. Then the language flip (文/A, §panel.i18n), the theme
  button, and Focus at the far right.
- The underline tab is the L2 shape; the pill `seg()` inside a body is the L3 shape. The two are
  never interchangeable.

**Focus mode** (`immersive.ts`) — the shell's, not browser F11. Entry and exit are the same
control at the far right of the context bar. The rail folds away and the bar shrinks to a
minimal in-flow app bar holding only the app zone (language, theme, Focus) — the title, the
tabs and the readouts hide — so shell controls never cover page actions. A workspace may
declare `[data-shell-focus-slot]`: Terminal hosts the same app zone in its own toolbar (moved,
never cloned) and the bar collapses completely. Escape exits last in the Escape chain. Every
mode change dispatches `resize`, so Terminal and Data remeasure. A page never adds its own
fullscreen control.

**Body templates** (`pane()` / `paneBody()` draw them):

| Template | For | Shape |
|---|---|---|
| Content | Lists, settings, management (Tunnels, Jobs, Traffic, Token, Settings) | `paneBody({wide})` + `paneHead`; the measure; the head pins; its actions at its right |
| Resource | Master-detail (MCP Servers) | A source list beside a detail whose `resHead` pins the name row and the resource's tabs while its description scrolls away |
| Workspace | Dense tools (Data, Terminal) | `pane({full})`: full-bleed, no padding, no measure, no scroll; still below the context bar and beside the rail |

A workspace is never wrapped in a giant decorative card: the shell is already its boundary.

**Action placement** — one hierarchy, the same corner on every page: app-global → context
bar; page → the page head's right; selected resource → the resource head; row → the row's
end or its `⋯`. A body header exists only when it adds identity, a description, the primary
action or filter state — never to repeat the location the bar already shows. Data's object tab
strip is L3 made visible, not a new layer (§data.tabs).

**Responsive.** Narrow widths never invent a different hierarchy: L1/L2/L3 keep their shapes,
dense bodies stack, the location and Focus stay reachable, the app never scrolls sideways.
At 960 px the rail does not clip and rows wrap their sub-line instead of overflowing.

## §panel.design — The design language

A management UI in the Apple System Settings / Linear lineage: a source list beside a detail
pane for the thing with many instances, a left-aligned page under a measure for the rest,
grouped inset lists, one primary action per view with the rest behind `⋯`, a five-step type
ramp, four weights, a 4 pt grid, hairlines instead of shadows. Restraint is the style: no
glass, no gradients, no large radii, no second accent. **A thing on screen is one surface
with one edge**; what the user does most is reachable without hovering, what they do rarely
or destructively waits behind `⋯`; the page teaches by its structure, and prose that explains
a gesture means the structure failed.

**Tokens** — the block at the head of `styles/base.css`, whose header comment states the rules
everything below derives from. Every colour, size, weight and radius is a token, never a
literal (G4); a new component size lands in the block, named, before any rule uses it. Dark is
the same names under `:root[data-theme="dark"]`: a rule written once, in tokens, is themed.
Code blocks take their colours from a scheme of their own under `:root[data-code="…"]`
(§panel.code).

| Family | Tokens |
|---|---|
| Type | `--f-title 22`, `--f-head 15`, `--f-body 13`, `--f-label 12`, `--f-caption 11`; `--sans` (the system stack, Inter first where installed, never shipped; judged in Segoe UI Variable and SF), `--mono` (the system mono stack) |
| Weights | `--w-body 400` running text, `--w-name 450` identity in a list, `--w-emph 500` a selected tab / a button / a caption / a band, `--w-title 600` titles — **four and only four** (G3); UA bold is re-pointed to `--w-emph` |
| Spacing | `--s1`…`--s8` = 4/8/12/16/20/24/32 |
| Radii | `--r-card 8`, `--r-row 6`, `--r-btn 6`, `--r-pill` |
| Sizes | `--ic-s 12` (a chevron), `--ic-m 14` (a glyph in a button), `--dot 6`, `--row-h 42`; icon size and ink through the `--ic` / `--ic-ink` knobs |
| Surfaces | `--bg`, `--sidebar`, `--bar`, `--card`, `--field`; `--hover` |
| Text, lines | `--text`, `--text-2`, `--text-3`; `--sep`, `--sep-soft` |
| Colour | `--accent`; state `--green`, `--red`, `--amber`; syntax `--syn-*` (code blocks only) |
| Code | `--code-font`, `--code-bg`, `--code-fg`, `--code-edge`, `--code-gutter`, `--code-sel`, `--code-caret` and `--syn-kw`, `--syn-str`, `--syn-num`, `--syn-com`, `--syn-fn`, `--syn-key`, `--syn-lit`, `--syn-punct` — one set per code colour scheme (§panel.code) |
| Terminal | `--term-*`, derived from the panel's colours (§panel.pages) |
| Measures | `--measure 920`, `--measure-wide 1180` |

**The rules.** Numbered, and cited as "rule N" in code comments.

1. **Monospace is for values you would copy** — a path, a command, a key, JSON, a tool name —
   never for prose identity. `tnum` for every number in a column.
2. **Saturation is for state**: dots, the one accent, error red, amber. Type chips, tags,
   group names and descriptions are monochrome; a column's annotation is `--text-3`. Two
   exceptions. Syntax colour inside a code block (the JSON view, the SQL highlighter, a
   document card) uses the active code scheme's `--syn-*` tokens only (§panel.code). The stop
   glyph is the other: a pressable Stop draws its square in `--red`, the one honest warning that
   a click ends a running thing. The table sets the colour (`data-tone="stop"`, `ui.css`); it is
   grey while the button is disabled, and it never fills a button or a row (rules 4 and 7). No
   other action glyph takes a colour.
3. **Text needs a measure.** Nothing is full-bleed except a workspace; content hugs the left
   edge and is never centred a second time.
4. **One primary action per view; the rest behind `⋯`.** A row holds at most one non-icon
   button. Red never appears on a row: destructive items are last in the overflow, after a
   separator, in `--red` text, or the row's trailing glyph. The one exception is a pressable
   stop's square (rule 2): the glyph alone, never a red row, a red name or a red-filled button,
   and no large danger button. (The System page's Quit is the page's one act and stays red.)
5. **Hierarchy is surface and indent, not size and caps.** A user-named container is a header
   band over its members (§panel.groups). Uppercase 11 px captions are for sections the product
   named, never for containers the user named.
6. **Every "new" says where it goes**: a Group field prefilled from the header `+` that opened
   it or the last group used; the title carries the group ("New job in *learn*").
7. **One glyph, one meaning per page** (`folder-plus` for a new group, `plus` for a new item).
   A short action's glyph is the action's, never the state's: a started MCP shows the
   *Disable* glyph (`power-off`), a running port forward the *Stop* glyph (`stop`) — the
   state itself is the dot and the status words. The transport glyphs are solid and each has
   its own shape: `play` (a triangle) starts, `stop` (a square) stops, `pause` (two bars) pauses,
   and pause appears only where a thing can actually be paused. The two halves of a verb never
   share one glyph (`power` is Enable, `power-off` is Disable), and
   near-neighbours stay apart: refresh (`refresh-cw`) re-reads, restart (`rotate-cw`) tears
   down and rebuilds, stop is not disable. Run all uses list-play to distinguish its scope
   from the adjacent Run. Saving and unsaving an analysis use bookmark and bookmark-minus.
   The verbs live in one table, `ui/action.ts` (`ACTION_ICON`, `actionBtn(verb, label)`,
   `setActionBtn`, `setActionBusy`): a page never picks its own glyph for a verb the table
   names, and a verb the table lacks joins the table first. The word rides in the accessible
   name and the tooltip (`iconBtn`), not as text beside the glyph. A stop's red is the one
   colour an action carries (rule 2): the table sets it (`data-tone="stop"`), so a page never
   colours a button itself. While a request runs, `setActionBusy` disables the button and the
   name states the verb in motion ("Stopping…"), never a bare ellipsis; when the request fails
   it restores the verb with its own words and tone. A state flip patches the same node
   (`setIconBtn` / `setActionBtn`) — a repaint through `textContent` would drop the sprite.
8. **One persistent glyph per container header, and the whole header drags**: `+` always
   visible (dimmed), `⋯` on hover/focus; its buttons cancel the drag at `dragstart`.
9. **Icons are the sprite**: Lucide-style, 24 viewBox, 1.5 stroke, `currentColor`, hand-written
   `<symbol>`s in `index.html`, drawn by `iconNode(name)`. No Unicode glyphs as icons, no icon
   package; the gallery shows every symbol.
10. **Status is a dot plus neutral text**: `dot(state, words)`, 6 px; filled green up, filled
    red down, a hollow ring for "starts on demand"; every dot has a `title` in words. A
    failure in a list is a red `tag()`, not a red row.
11. **Empty states use one template**, `emptyNode({icon, title, hint, action})`. An empty
    container is one quiet row the height of a real one ("No items — drop here or press +");
    a list filtered to nothing is one `note()` line.
12. **Surfaces are a grey ladder**: cards lift by a hairline ring; shadows belong to floating
    layers (sheet, menu, back to top). Dark is a warm near-black read as paper.
13. **Motion is 150 ms ease or nothing**, and nothing under `prefers-reduced-motion`.
14. **Copy is short and declarative, in both languages**: no exclamation marks, no "please",
    no emoji; a confirmation states the consequence and what is *not* destroyed. Every visible
    string goes through `tr()` (§panel.i18n).
15. **Selection is a bar and a tint**: a 2 px `--accent` leading edge over an 8 % accent tint;
    the selected name goes `--w-emph`, not blue.
16. **One surface, one edge**: a container is drawn once — never a label and a rail and a box.
17. **Air is a token step, never zero**: `--s5` after a page header and between a form and its
    list, `--s6` between sections, `--s3` between cards, fields and pairs, `--s1` between a
    band and its first member. Two blocks touching means a rule is missing.
18. **Trailing controls share one column**: a row's last glyph and its band's sit at the same x.
19. **The frequent gesture needs no discovery**; hover-only affordances are for the rare.
20. **Structure teaches; prose confirms**: a page description is one sentence on one line at
    the measure, never an explanation of an interaction. Placeholders are sentence case and
    name the value; a form never calls a field optional that the store will refuse.
21. **Four weights** (above).
22. **Lines are inset, lists are open**: a separator inside a card starts at the text column;
    the event list and the flow draw no box and no row lines; a data grid draws no vertical rules.
23. **The head stays**: page heads and resource heads pin; their hairline appears only once
    content is under them. Nothing re-implements a sticky header.
24. **Every "this happened" list is the event list**, `timeline()` (§panel.ui); **work read as
    it went** - an agent's rounds and tool calls, a sync's runs - **is the flow**, `flow()`.
25. **One screen never repeats itself**: no count the context bar already shows, no per-row
    meta that is the same on every row, no caption repeating the tab above it.

**Accessibility.** Focus is always visible; an icon-only button has an `aria-label`; a dot has
a `title`; Enter submits a sheet's primary; arrows move where a list is a listbox.

**Verification.** A visual change is walked on a test instance (§testing.live) in light and
dark, English and Chinese, at about 1440 px and 960 px — never on the user's instance.

## §panel.code — Code blocks and their colour schemes

Every code block is an editor surface: the JSON view (`pre.jv`), a Mongo document card, the
SQL console, a DDL pane or preview, a raw value. It is painted in a **code colour scheme**
that is its own choice beside the theme, named after the IDE schemes its colours copy, and
reads like DataGrip on SQL it has not resolved against a schema.

- **Schemes.** `auto` (the default: IntelliJ Light under the light theme, IntelliJ Dark under
  the dark one), `intellij-light`, `darcula`, `intellij-dark` (the New UI's Dark) and `github`
  (its light or dark half with the theme). The three IDE schemes stay what they are whatever
  the theme — a Darcula block on the light panel is a choice, as in the IDE.
- **Where it lives.** The preference is per browser (`localStorage` `swiss_code`, like the
  theme). `<html data-code>` always holds a concrete scheme (`intellij-light`, `darcula`,
  `intellij-dark`, `github-light`, `github-dark`), resolved by `ui/code-scheme.ts`
  `codeSchemeFor` and repeated in both pages' pre-paint scripts so no block flashes in the
  wrong one (a test runs the copies against the library). `styles/base.css` writes each
  scheme once: IntelliJ Light in `:root`, the rest under `:root[data-code="…"]`, which the
  token gates (G4) count as token blocks.
- **The switch** is the header's palette button: a menu of the five, the current one
  checked; a pick applies at once, so the open page is the preview. The gallery takes
  `?code=`.
- **Token roles** — one family for every printer (`ui/json-view.ts`), weight never changes:

  | Class | Role | Token |
  |---|---|---|
  | `jv-w` | a keyword or a type (`SELECT`, `NOT`, `varchar`, `new`) | `--syn-kw` |
  | `jv-f` | a function or a constructor (`count(`, `ObjectId(`) | `--syn-fn` |
  | `jv-k` | a field: a JSON or document key, a column | `--syn-key` |
  | `jv-s` / `jv-n` | a string / a number | `--syn-str` / `--syn-num` |
  | `jv-l` | `true`, `false`, `null` | `--syn-lit` |
  | `jv-c` | a comment, italic | `--syn-com` |
  | `jv-p` | punctuation | `--syn-punct` |

- **SQL** (`data-filters.ts` `dbSqlTokens`, a tokenizer, never a parser): a word is a column
  where the text says so — the column list a `(` after a table name opens (`CREATE TABLE t (`,
  `INSERT INTO t (`, `REFERENCES t (`, `ON t (`), anything nested deeper inside it (index
  columns, `CHECK`), the half after a qualifier's dot unless the qualifier is a table, the word
  after `COLUMN`; a table, an index, an alias stay the plain text colour. A `"double-quoted"`
  run is an identifier on PostgreSQL and a string on MySQL. Punctuation never takes a quote,
  so ``(`id`)`` lexes as three tokens and a `;` inside `('a;b')` never ends a statement.
  The Mongo console uses `dbShellTokens`: a key before `:`, a constructor before `(`.
- **Editors.** The SQL console and a cell's editor sheet are one layered editor: a
  transparent textarea over a highlighted `<pre>`, both in `.db-sql-face` so every metric
  matches, the scrollbar's gutter reserved in both so a line wraps at the same place in each.
  The layer is repainted on every input, Format JSON and boolean toggle, and follows the
  textarea's scroll. For console text of at least 64,000 characters, browsers supporting the
  CSS Highlight API color ordinary tokens with native text ranges instead of an element per
  token; comments and numbers retain spans for their font styles. The original span renderer is
  the fallback, and the range registry is cleared when repainting or leaving Data. The cell editor paints a value that starts with `{` or `[` through
  `dbShellTokens` (JSON is a subset of the shell's syntax, so a half-typed value still
  colours); any other value is one plain run. Its textarea stays in flow, so its own height
  and resize handle size the box.
- **Numbered lines.** The DDL pane, the DDL sheet's preview and the commit previews number
  their lines (`tokenSpans({ lined })`): one `.jv-line` per line, the number generated content
  in a gutter, so a selection copies the code and never a number.

## §panel.ui — The UI library

The panel has one component library, in the tree (ADR-029). Every shape a page draws is a
`ui/` function; a page composes them and never writes a library class into an `h()` class
literal.

- **Where.** `panel/src/ui/*.ts`, exported whole by `ui/index.ts`; its classes, and only
  those, in `styles/ui.css`. No npm package, no separate crate, no third-party component
  library.
- **The boundary** (G1): `ui/` imports only `../h.js`, `../i18n.js`, `./*` and a vendored
  library's shim, `../vendor/<pkg>/<version>/index.js` (§panel.vendor: markdown-it and mermaid
  for `ui/markdown.ts`) — no API, no state, no views. That is what lets the gallery render every
  component with nothing else loaded.
- **Three categories.** Pure markup lives in `ui/`; a mechanism that depends only on the DOM
  (menus, sheets, the select, `toTop`) lives in `ui/`; behaviour bound to the app (the groups
  component's API calls and state, `groups.ts`) stays with the app and is built on the library.
- **CSS in three layers**, loaded base → ui → views: `base.css` = tokens, reset and the shell
  (rail, context bar, sidebar); `ui.css` = every component's classes; `views.css` = a
  workspace's skeleton and page-local layout the library has no shape for. `views.css` never
  restyles a library class (G2).
- **Library first.** A shape a page lacks goes into the library before the page uses it: the
  function in `ui/`, its classes in `ui.css`, a gallery section, a test in
  `ui-components.test.ts`. A page that needs a missing shape stops until the library has it.
- **A design is a gallery scene**: a new page or a redesign is drawn in `ui-scenes.ts` from
  `ui/`, `h` and `i18n` with made-up data, reviewed in the gallery, then built. Never a
  hand-written HTML mock with its own style sheet.
- **Migrating deletes.** A page moved onto the library loses its old CSS and class names in the
  same change; no compatibility class survives, and the gate rows go down with it.
- **The gallery** — `/admin/ui.html` (`ui-gallery.ts`, `ui-scenes.ts`; it borrows the sprite
  from `index.html` at boot): every export in every state, with `?theme=dark&lang=zh&w=960` in
  the query string. It ships in the binary, reached by typing it; it is not in the rail.
- **Back to top** (`toTop(scroller)`): the shell mounts one on `#pane`; it appears past one
  screen of scroll and glides back (jumps under reduced motion). A workspace never scrolls, so
  never shows it.
- **Every sidebar is the reader's width** (`grip(pane, {axis, key, size, min, max, label, fold})`):
  the edge after a pane drags it wider or narrower, and a foldable one folds away - a drag past
  half its minimum, a double-click, Enter on the focused edge, or the chevron the edge shows
  while the sidebar is under the pointer - leaving a thin strip whose click brings it back. The
  shell's MCP list (`side`), every workspace's side column (`ws-side`) and Data's browser
  (`db-side`) fold. The size and the fold are remembered per browser (`swiss_grip_<key>`);
  with no `size` the stylesheet's width stands until the reader moves the edge. A change ends
  with a window `resize`, so the pages that measure their space remeasure. `folded` starts a
  foldable pane folded while nothing is remembered (a pane the reader opens when they want
  it); `end` turns the edge around for a column at the page's end edge - the pane it sizes is
  the one AFTER it, a drag or arrow towards the start widens it, its chevron points the other
  way. A page's own control folds or brings back the pane through `gripFold(edge, folded)`,
  remembered as the reader's own fold; `gripFolded(edge)` reads it.
- **An assistant sits at the end edge** (`chatPane({id, cap, tools, body, input, commands,
  queued, note, actions, stats})` after a `grip({end, fold})`): Data's agent (§data.agent). A column on the
  sidebar's surface, `--chat-pane-w` wide before a drag: the head (its caption, then a
  picker, New and close - one line at any width: the subject ellipsizes, a pane narrower than
  360px drops the caption, the tools never wrap under it), the conversation that scrolls on its own (`chatFollow(pane)` keeps
  its end in view on every streamed paint), and the compose box under it - the words, then a
  line with a note at its start (what the pane works on) and the send at its end (a page may
  turn it into the stop while an answer streams; `chatAfterWrite(box, fn)` re-reads it on every
  write to the box, typed or set),
  which stays put however long the conversation grows. `queued` stands above the words box:
  what was sent while an answer streams, waiting its turn (a `chatQueue`'s node, below, no
  room taken while empty). `stats` stands under that line: the conversation's account (a
  `chatStats`'s node, below, hidden while it has nothing to say). The page wires the
  controls; the pane follows the latest content until the reader leaves it: a step up of any
  size, a scroll down that stops short of the end, or a fold they open, whose row stays under
  the click. A circular **Back to bottom** button above the composer appears while away from
  the end; it returns there and resumes following. Sending a new prompt also resumes
  following. Layout changes after markdown, process folds and composer resizing keep a
  following reader at the end. A body that scrolls in place inside the column (a long prompt,
  a long thought) hands the wheel on at its end, so a pointer resting there never holds the
  column still.
  The words box's manners are the pane's own:
  - **It grows with its words** (`chatGrow`): its rows are the floor, `max-height: 40vh` the
    cap, a scroll past it, no drag handle. It refits on every write - typed, pasted, or set
    by the page (`box.value = ""` after a send, words handed back: the instance's own `value`
    setter, as a styled select's) - on focus, and when its width changes; a box not laid out
    (a folded pane) keeps its rows.
  - **A "/" at its start lists the commands** (`commands: [{name, arg?, hint}]`,
    `chatCommands`): those whose name begins with what follows, then those that hold it, on
    a floating card above the compose area - one line each, `/name` in mono with its `arg`
    dim and the hint after it. The box is the combobox and keeps the focus
    (`aria-controls`, `aria-expanded`, `aria-activedescendant` on a `role=listbox`): the
    arrows walk the list (wrapping), Tab or Enter completes the one marked as `/name ` (the
    box goes on to its argument), Escape shuts the list until the words change, a click
    (mousedown) completes and leaves the caret in the box, and leaving the box shuts it. A key
    the list takes goes no further - not to the page's Enter, not to the panel's Escape - while
    Enter on a name typed whole, and Shift+Enter, stay the page's. Words without the focus,
    with a space, or naming no command list nothing.
  - **What waits to be sent is a queue** (`chatQueue({id, max, onSteer, onDelete, onEdited})`
    → `{node, items, add, remove, steer, editing}`), Codex's: a card over the words box (a
    hairline, the card's radius) whose head names how many wait (*3 queued messages*) and
    folds the rows away, then one quiet line per message - its first line (the whole words in
    the title, " …" when more follow), then Edit, Delete and ↑ (*Steer now*) at its end. ↑
    puts the row first and hands it to the page (`onSteer`), which steers it into the live
    answer or, with nothing streaming, sends it as the ask; a steered row is dim, says
    *steering* where the arrow was, and its Delete reads *Withdraw*. Edit turns the row into a
    box in place (`chatGrow`, `max` its cap): Enter or leaving it saves, Escape keeps the
    words as they were, saved empty the row is deleted, and neither key goes on to the page or
    the panel. The queue holds the rows and says what the reader did (`onDelete`,
    `onEdited`); the page owns the sending: one row per turn after a clean answer, in order,
    `remove` when the loop read one or it went, `steer(id, false)` when the answer ended
    before reading it. The page's words box reads *Enter queues · Ctrl+Enter steers every
    queued message now* while an answer streams.
  - **What the conversation spent reads under the box** (`chatStats({id, note})` →
    `{node, set(usage | null)}`), dsh's: one quiet line centred under the box in the label's
    size and ink, three pills. The first counts the turns the reader took, the steps the
    model ran across them and its output speed (*2 turns 5 steps · 78 tok/s*, the gauge
    icon) and opens **Session statistics**: the counts in its title, then LLM time, Tool
    time, the average time to first token (TTFT) and tokens per second (TPS) - output over
    the time after each round's first token (`decodeTokens / decodeMs`), else output over
    the LLM time; durations read *45.2s* under a minute, *2m42s* past it. It is disabled
    while nothing timed it. The second counts the tokens used and the share the provider's
    cache served (*45.4K tok · Cache hit 87%*, the database icon) and opens **Token usage**:
    the exact total in its title (*13,096,027 tok*), then the cache hit, uncached input,
    cached input, cache write (only when some was written) and output, each exact. The third
    is a ring that fills as the next request nears the agent's budget, its percent beside it
    - accent, amber from 75 %, red from 90 % - and opens the context: the used and the limit
    in its title (*124K / 256K*), a bar of what fills it, a row each for the system prompt,
    the tool definitions and the messages, the caller's `note` under them. One card shows at
    a time, centred above its pill inside the line (`role=dialog`, the pills
    `aria-haspopup=dialog` and `aria-expanded`); a pill's click opens or closes its own,
    another pill's swaps it. Escape closes it without reaching the page (the focus back on
    its pill), a press outside closes it, an open card follows the counts and closes when
    its pill hides. The page keeps the counts (`ChatUsage {turns, steps, tokensIn,
    cacheRead, cacheWrite, tokensOut, llmMs, toolMs, ttftMs, ttftSteps, decodeMs,
    decodeTokens, context}`) and hands them over whole. The context is bytes against a byte
    budget (`ChatContext {bytes, cap, system, tools, limit?, used?, perByte?}`): with
    `limit` (the budget in tokens) and `used` (the provider's count of the last round, its
    request and answer) it reads exact; with `limit` and `perByte` alone - nothing counted
    what the conversation now carries - `used` is estimated from the bytes (`~`); without
    `limit` it reads in KiB. The rows split `used` by the parts' byte shares. With neither a
    step nor a context the line hides; a context alone shows the ring alone.
- **What the reader or a model wrote copies** (`copyBtn(text, {label})`, `copyText(text)`):
  a quiet icon button that writes its text to the clipboard (the async clipboard first, a
  hidden textarea's copy second, for an origin that withholds it) and says so in place - a
  check and *Copied* for a moment, a cross and *Copy failed* when both refuse. A
  transcript turn with `copy` ends on one under its words (`.tc-acts`): the reader's turn
  copies the words as typed, the model's the answer whole, its markdown source. A
  `markdownNode(src, {copy})` wraps each code block in a box with its own at the top end
  (`.md-codebox`), the block's text exactly, no fence; a transcript's markdown turn always
  draws them so.
- **A pane over another is a dock** (`dock(top, bar, bottom, {key, size, min, restMin, label,
  restLabel})`): Data's editor over its results. The bar between them (the results' tabs and
  facts) is the handle: a press anywhere on it but a control drags the top pane's height (a
  hairline at its top edge turns accent after a beat under the pointer; the edge is the
  keyboard's separator: arrows step 16 px, Home/End go to the ends, Enter folds), kept between
  `min` and the column less `restMin`. Past the bottom by half `restMin` the bottom pane folds
  down to the bar, and the top fills the room; above half `min` the top folds away. A fold by
  drag keeps the size the drag started from. A double-click on the bar folds the bottom pane
  or brings back what is folded. Two quiet buttons end the bar: Hide/Show {restLabel}
  (chevron) and Maximize {restLabel}/Show {label} (expand/collapse). The size and a folded
  bottom are remembered per browser (`swiss_dock_<key>`, nothing kept at the defaults); a
  maximized bottom is a look, not a layout, and is not kept. `dockShow(key, which)` lets the
  page bring a pane back (a run answering into folded results). A hidden top pane takes the bar
  with it. A change ends with a window `resize`.

**The vocabulary.** Specs and class names use these words; a design that needs another adds it
to the library first.

| Word | Function | Classes |
|---|---|---|
| pane, page head, resource head | `pane({wide, full})`, `paneBody({wide})`, `paneHead({desc, sub, actions})`, `resHead({title, desc, sub, actions, nav})` | `.pane`, `.pane-head`, `.pane-head.res`, `.pane-nav` |
| section, card | `section({cap, note, tools}, …)`, `card(…rows)` | `.sec*`, `.group` |
| row | `row({lead, name, sub, err, cols, toggle, primary, more, detail})`; `kvRow(label, value, {mono})`; `sideRow({name, lead, tail, selected})` | `.lrow*`, `.kv*`, `base.css .side-row` |
| band | `groupNode(…)` (mounted by `groups.ts`) | `.grp`, `.grp-head` |
| segmented control | `seg(items, current, {key, label, fill})` — L3 only | `.seg` |
| event list | `timeline(items, {open, body})`, `timelineMeta(parts)`, `timelineToggle()` | `.tl*` |
| flow | `flow(items, {open, body, eager})`, `flowProse(…)`, `flowIO(parts)`, `flowHead(…lines)`, `factLine(parts, {lead})`, `flowToggle()` | `.fl*` |
| code block | `jsonCodeNode(value, all, {oneLine})`, `textNode()`, `valueBlock({label, notes, tools, text}, …)` | `pre.jv`, `pre.logs`, `.vblock*` |
| form | `form`, `field({label, control, required, meta, hint, action, group})`, `checkField`, `pair`, `formActions`, `formCap`, `formFold`, `hint`; `inlineForm(…)` | `.form`, `.fld`, `.two`, `.inline-form` |
| filter, pager, note | `filterInput(…)`, `pager({label, status, prev, next})`, `note(body, {busy, err})`, `failNote(…)` | `input.filter`, `.pager`, `.note` |
| status | `dot(state, words)`, `heldDot(words)`, `tag(text, {mono, tone})`, `spinner()` | `.dot`, `.tag`, `.spin` |
| fact chip | `portChip(port, {tone, title})` — a port the machine holds (up), a promise unkept (down) or a quiet declared-only one (held: no tone class) | `.pchip` |
| entry picker | `pickPanel(pathPlaceholder, goLabel, upLabel)` + `pickRow(kind, name, mono)` — the inline file browser a field opens under itself; its root is `.dirpick`, NEVER `.pick` (the styled select's option rows own that name) | `.dirpick*` |
| controls | `sw(on, label)`, `btn(label, {kind, icon})`, `iconBtn`, `moreBtn`, and the action table `ui/action.ts` — `actionBtn(verb, label)` / `setActionBtn` / `setActionBusy` / `ACTION_ICON` — one glyph per verb (rule 7, every verb a page may name); the stop square is the one coloured action (rule 2: `--red` while pressable, grey when disabled, `data-tone="stop"`); native `<select>` styled by `initSelects` | `.sw`, `.btn`, `.btn.icon[data-tone="stop"]`, `.dd*` |
| layers | `popupMenu(anchor, items, {filter})` - with `filter` (the placeholder) a find box over rows that scroll under it: typing narrows them, the arrows walk the box and the rows left, Enter runs the first, "Nothing matches" when none do - `anchoredMenu(host, items)`, `sheet({title, sub, body, foot})` + `showSheet`, `stackSheet`, `openFieldSheet(spec)` | `.menu`, `.menu.find`, `.menu-filter`, `.menu-rows`, `.sheet*`, `.backdrop` |
| page furniture | `emptyNode(…)`, `pageFoot({note, rev})`, `toTop(scroller)`; the toast is the shell's (`util.ts toast()`) | `.empty`, `.page-foot`, `.to-top`, `.toast` |
| grip | `grip(pane, {axis, key, size, min, max, label, fold, folded, end})`, `gripState(opts)`, `gripFold(edge, folded)`, `gripFolded(edge)` - the edge after a pane (before one, with `end`) that sizes it (and folds it) | `.grip`, `.grip.x`, `.grip.y`, `.grip.end`, `.grip.folded`, `.grip-btn` |
| chat pane | `chatPane({id, cap, heading, subject, tools, body, input, commands, queued, context, controls, note, actions, stats})`, `chatFollow(pane)`, `chatGrow(box)`, `chatCommands(box, commands)` - an assistant's column at the end edge: head, conversation, compose; the words box grows with its words and completes its commands | `.chat-pane`, `.chat-head`, `.chat-heading`, `.chat-subject`, `.chat-tools`, `.chat-body`, `.chat-compose`, `.chat-editor`, `.chat-acts`, `.chat-note`, `.chat-cmds`, `.chat-cmd`, `.chat-cmd-name`, `.chat-cmd-arg`, `.chat-cmd-hint` |
| chat queue | `chatQueue({id, max, onSteer, onDelete, onEdited})` → `{node, items, add, remove, steer, editing}` - what waits to be sent while an answer streams: a card over the words box, a row per message with Edit, Delete and steer-now | `.chat-queue`, `.chat-queue-head`, `.chat-queue-n`, `.chat-queue-list`, `.chat-queue-row`, `.chat-queue-text`, `.chat-queue-tag`, `.chat-queue-edit` |
| chat stats | `chatStats({id, note})` → `{node, set}` - a conversation's account centred under the words box: turns, steps and speed opening Session statistics, tokens and the cache's share opening Token usage, the context ring opening what fills it | `.chat-stats`, `.chat-stat`, `.chat-stat-label`, `.chat-stat-sep`, `.chat-ctx`, `.chat-ring`, `.chat-ring-track`, `.chat-ring-fill`, `.chat-ctx-pct`, `.chat-pop`, `.chat-pop-title`, `.chat-pop-label`, `.chat-pop-value`, `.chat-pop-rule`, `.chat-pop-rows`, `.chat-pop-note`, `.chat-ctx-bar`, `.chat-ctx-seg`, `.chat-ctx-key` |
| copy | `copyBtn(text, {label})`, `copyText(text)`; a transcript turn's `copy`, `markdownNode(src, {copy})` - a quiet button that copies a turn's words or a code block and says so in place | `.copy-btn`, `.tc-acts`, `.md-codebox` |
| dock | `dock(top, bar, bottom, {key, size, min, restMin, label, restLabel})`, `dockShow(key, which)` - a pane over another, the bar between them the handle that sizes and folds them | `.dock`, `.dock-top`, `.dock-head`, `.dock-edge`, `.dock-tools`, `.dock-bottom` |

**The event list** (`timeline`) is every "this happened" list — MCP Logs (the reference),
Traffic's activity, Remote runs, a job's runs, Data's Pub/Sub log: the time in its own column (the date is a day
heading, never repeated per row, and it sticks under whichever head is pinned), the title, one
line of arguments, the who column only when it varies, ×N for consecutive identical items, a
failure as a red tag, the duration right-aligned (amber past a second). What every row says
the same moves into the open row's meta line.

**The flow** (`flow`) is work read as it went - the LLM history's run sheet (the reference) and
the memory-sync sheet's runs. The event list is a log scanned by time; the flow is a story
followed in order, so it has no clock column. Each step is one quiet line:
- a glyph (an icon) or the caller's state dot, which gives way to a chevron on hover and while
  the step is open;
- the step's name (mono when it is a value you would type, a tool's name), a dot, then a
  one-line summary in the tertiary grey, cut with an ellipsis and whole on hover;
- an optional second line under it (a run's verdict), cut at two lines until opened;
- a failure as a toned tag, a duration or moment right-aligned, controls (a Stop) beside the
  row, outside its toggle.

Space, not rules, separates the steps. What the work said between steps (a model's words) is
prose at the title's indent (`flowProse`). A step opens in place at the same indent, with no
card; a tool call's input and output share one code surface (`flowIO`), each side under a
caption and scrolled past `--fl-io-h`. A step in flight shimmers its summary, and a pulse
stands before its meta unless its lead is already a state dot; reduced motion holds both
still. A head of fact lines (`flowHead`, `factLine`) sits above a run's steps: the first line
is the strong one (its state), the rest dot-joined and wrapping whole facts.

**The gates** — all in `panel/test/`, all inside `npm run check`:

| Gate | Test | Fails when | Floor |
|---|---|---|---|
| G1 | `ui-boundary` | a `ui/` module imports anything but `../h.js`, `../i18n.js`, `./*`, `../vendor/<pkg>/<version>/index.js` | zero |
| G2 | `ui-css-ownership` | `views.css` makes a `ui.css` class the subject of a selector (`svg`/`use` count as `.ic`) | `FROZEN_VIOLATIONS = 0` |
| G3 | `css-weights` | a `font-weight` is not one of the four `--w-*` tokens | zero |
| G4 | `css-literals` | a sheet gains a px / hex / rgb literal outside its token blocks — `:root`, `:root[data-theme=…]`, a code scheme's `:root[data-code=…]` (`0`, `1px`, `2px`, `-1px` and `%` exempt) | base 22, ui 72, views 159 |
| G5 | `ui-class-ratchet` | a file outside `ui/` draws a `ui.css` class (a file with no row is held at 0) | the table is empty |
| G6 | `ui-gallery` | an export of `ui/index.ts` is claimed by no gallery section or scene (or by `HELPERS` with a reason), a claimed shape is missing from the DOM, or a class the gallery draws is unstyled | zero |
| G7 | `css-size` | `views.css` grows past its byte ceiling | `FROZEN_VIEWS_BYTES = 59742` |

**The ratchet convention** (every frozen table in the panel, and §panel.lint's): the table
lives in the test, its numbers only go down, and a change that lowers a count lowers the row
in the same commit. A new file defaults to zero.


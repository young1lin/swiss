# §panel — The admin panel

Chapter 0014 of the [swiss specification](../SPEC.md).

One page application, embedded in the binary, served on the same loopback port as everything
else. It is held to the product's four properties like the binary is (§product): pixels are
as accountable as bytes. No framework, no bundler, no runtime npm dependency, no web font
download.

| Where | What |
|---|---|
| `crates/swiss-panel/panel/src/` | The TypeScript source: shell (`main.ts`, `page-registry.ts`, `page-core.ts`, `plugin-palette.ts`, `last-page.ts`, `immersive.ts`, `pane-scroll.ts`), views (`views/*.ts`, one per page), the UI library (`ui/`), state slices, i18n (`i18n.ts`, `locales/`) |
| `crates/swiss-panel/panel/test/` | Vitest (happy-dom): behaviour tests and the gates (§panel.ui, §panel.i18n, §panel.lint) |
| `crates/swiss-panel/src/admin_assets/` | What is served: `index.html` (the shell and the SVG sprite), `ui.html` (the gallery), `styles/{base,ui,views}.css`, `js/` (the committed emit, §panel.toolchain), `js/vendor/` (§panel.vendor), `logo.svg` |
| `crates/swiss-panel/src/admin.rs` | The embedded asset server |

**Serving** (`admin.rs`). `rust-embed` over `admin_assets/`: a debug build reads every file from
disk per request (edit, emit, reload — no gateway restart). A release build embeds the tree
gzip-compressed: swiss-panel's `build.rs` gzips each file under its own name into `$OUT_DIR`
(no mtime and no name in the header, so one tree always gives the same bytes), which keeps
~7 MB of panel to ~2 MB of binary. A body is the embedded bytes borrowed from the binary,
never a per-request copy: `Content-Encoding: gzip` (with `Vary: Accept-Encoding`) to a client
that takes it — every browser — and inflated for that one response to a client that does not.
The shell (`shell()`) is served `no-store`. An asset (`asset()`) is served `no-cache` with a
strong ETag over its SHA-256 and the encoding sent, so every load revalidates — a new build
shows at once, as under `no-store` — and an unchanged module is a bodiless 304. `mime_of` is a
whitelist: a file whose type is not on it is refused, never guessed. `asset` guards the path (a
traversal is a 404). The **panel version stamp** is a SHA-1 over every file's path and SHA-256
(rust-embed's, compile-time in release), handed to the panel by `/api/info`; a panel that sees
the stamp change reloads itself, so a rebuilt gateway is never driven by a stale tab. The build
script tracks `admin_assets/` (`rerun-if-changed` on the directory), so after `npm run build`
the next release build re-embeds what changed, an added or removed file included.

**The rendering contract.** The page body is rebuilt only by an explicit load or a user action,
never by a poll and never while something in it has focus. A poll patches in place — the
sidebar rows, the detail header, a status cell — so a caret, a scroll position, an open menu
or a half-typed field survives every tick.

**Polling.** One `setInterval(poll, 6000)`; a tick is skipped while the tab is hidden, and
returning to it polls at once (`visibilitychange`). A poll is the memory reading
(`/api/memory`, §host.memory), `/api/info` (the version stamp) and the active page's own
`poll()`. The memory chip is a reading, not a reload button: clicking it refreshes the reading
only. The one explicit view refresh is the `r` key (`refreshNow`), which keeps Data's manual
reload alive.

**Pages.** A view module exports a `PageModule`:

```ts
interface PageModule {
  mount?: (ctx: { signal: AbortSignal }) => void | Promise<void>;
  unmount?: () => void | Promise<void>;
  poll?: () => void | Promise<void>;
  refresh?: () => void | Promise<void>;
  canLeave?: () => boolean;          // false = navigation, a language flip, a reload are held
  countText?: () => string;          // the context bar's count readout
  hasPendingChanges?: () => boolean; // the beforeunload guard
}
```

`mount` gets an `AbortSignal` that fires on leave; listeners and fetches hang off it. A late
module load that lost the race to a newer navigation is discarded (a sequence ticket). The
`beforeunload` guard asks the browser to confirm while `hasPendingChanges()` is true (staged
Data edits, an unsaved form).

**The API helper.** `api(path, opts)` adds the JSON content type; a 401 from `/api/*` is the
session gate (§host.session) and sends the tab to `/`, which answers with the sign-in page.
`apiJson<T>()` is the one fetch → parse → report path for a mutation: a failure toasts the
API's own `{error}` (or "HTTP N"), a network failure toasts "is the gateway running", and
both return `null` — every caller is `if (!j) return;`, and none of them may fail silently.
API types are copied from the Rust serde structs field by field (§panel.toolchain).

**Theme.** `swiss_theme` holds the preference — `light`, `dark`, or absent for auto (the OS's
`prefers-color-scheme`, followed live). The result is `<html data-theme>`. The button shows what
a click switches to (a moon on light, a sun on dark) and the first click pins the choice for
this browser. Blocked storage keeps the choice for the page load.

**Keyboard.** The shell listens at `document`. Escape closes the outermost layer first: sheet,
then menu, then the MCP run history popover, then Focus mode. Outside a text field: `r`
refreshes the view; on the sidebar page `/` focuses the filter, ↑/↓ walk the rows actually on
screen (a folded group is stepped over) and Alt+↑/↓ move the selected MCP through the list
(the drag-free reorder). A layer that handles a key stops its propagation. A sheet is modal:
its keys stay in it, Escape aside (`ui/sheet.ts initSheet`).

**Browser storage.** Panel-only preferences; nothing a store needs lives here, and every read
and write tolerates blocked storage.

| Key | Holds |
|---|---|
| `swiss_theme`, `swiss_lang` | Theme and language preference (absent = auto / English) |
| `swiss.lastPage` | `{pluginId: pageId}` — the last page per plugin (§panel.nav) |
| `swiss.rail.pinned` | The rail's pinned plugin list |
| `swiss.tokenId` | Which token the copied connect commands embed |
| `swiss.groups.<scope>.collapsed`, `swiss.groups.<scope>.last` | Fold state per group (carried across a rename) and the last group used (§panel.groups) |
| `swiss.dbGrid.<conn>_<schema.table>`, `swiss.dbFavorites`, `swiss.dbRowLimit` | Data column widths, favourites, the console's row limit (§data; the console history is the server's, §data.history) |
| `swiss.terminal.fontSize`, `.bell`, `.copyOnSelect`, `.hint` | Terminal preferences (§terminal.panel) |

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
`pluginId`, `label`, `order`, `path` (`#id`), `entry` (the view module's URL), `sidebar`,
`layout`. The host pages are added by the panel itself: Plugins (order 1000), Secrets (1001)
and System (1002), `pluginId: "host"`, shown as the Settings plugin. The built-in pages:

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
| `plugins`, `secrets`, `system` | host | Plugins, Secrets, System | 1000–1002 | page |

- **Grouping** (`page-core.ts groupPages`): one group per plugin; a group sorts by the
  **smallest** order among its pages, pages sort by order inside it, ties keep registration
  order. A plugin that is disabled, failed, waiting for a dependency or not built keeps its
  pages registered and marked unavailable: its seat stays, and its page paints that state.
- **Routing** is the hash: `#id`. An unknown hash lands on the first page; no hash is `#mcps`.
  A `hashchange` to a registered id navigates; `canLeave()` returning false puts the old hash
  back and stays.
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
   group names and descriptions are monochrome; a column's annotation is `--text-3`. The one
   exception is syntax colour inside a code block (the JSON view, the SQL highlighter, a
   document card), in the active code scheme's `--syn-*` tokens only (§panel.code).
3. **Text needs a measure.** Nothing is full-bleed except a workspace; content hugs the left
   edge and is never centred a second time.
4. **One primary action per view; the rest behind `⋯`.** A row holds at most one non-icon
   button. Red never appears on a row: destructive items are last in the overflow, after a
   separator, in `--red` text, or the row's trailing glyph. (The System page's Quit is the
   page's one act and stays red.)
5. **Hierarchy is surface and indent, not size and caps.** A user-named container is a header
   band over its members (§panel.groups). Uppercase 11 px captions are for sections the product
   named, never for containers the user named.
6. **Every "new" says where it goes**: a Group field prefilled from the header `+` that opened
   it or the last group used; the title carries the group ("New job in *learn*").
7. **One glyph, one meaning per page** (`folder-plus` for a new group, `plus` for a new item).
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
  textarea's scroll. The cell editor paints a value that starts with `{` or `[` through
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
  picker, New and close), the conversation that scrolls on its own (`chatFollow(pane)` keeps
  its end in view while an answer streams), and the compose box under it - the words, then a
  line with a note at its start (what the pane works on) and the send and stop at its end,
  which stays put however long the conversation grows. `queued` stands above the words box:
  what was sent while an answer streams, waiting its turn (a `chatQueue`'s node, below, no
  room taken while empty). `stats` stands under that line: the conversation's account (a
  `chatStats`'s node, below, hidden while it has nothing to say). The page wires the
  controls; the words box's manners are the pane's own:
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
| controls | `sw(on, label)`, `btn(label, {kind, icon})`, `iconBtn`, `moreBtn`; native `<select>` styled by `initSelects` | `.sw`, `.btn`, `.dd*` |
| layers | `popupMenu(anchor, items, {filter})` - with `filter` (the placeholder) a find box over rows that scroll under it: typing narrows them, the arrows walk the box and the rows left, Enter runs the first, "Nothing matches" when none do - `anchoredMenu(host, items)`, `sheet({title, sub, body, foot})` + `showSheet`, `stackSheet`, `openFieldSheet(spec)` | `.menu`, `.menu.find`, `.menu-filter`, `.menu-rows`, `.sheet*`, `.backdrop` |
| page furniture | `emptyNode(…)`, `pageFoot({note, rev})`, `toTop(scroller)`; the toast is the shell's (`util.ts toast()`) | `.empty`, `.page-foot`, `.to-top`, `.toast` |
| grip | `grip(pane, {axis, key, size, min, max, label, fold, folded, end})`, `gripState(opts)`, `gripFold(edge, folded)`, `gripFolded(edge)` - the edge after a pane (before one, with `end`) that sizes it (and folds it) | `.grip`, `.grip.x`, `.grip.y`, `.grip.end`, `.grip.folded`, `.grip-btn` |
| chat pane | `chatPane({id, cap, tools, body, input, commands, queued, note, actions, stats})`, `chatFollow(pane)`, `chatGrow(box)`, `chatCommands(box, commands)` - an assistant's column at the end edge: head, conversation, compose; the words box grows with its words and completes its commands | `.chat-pane`, `.chat-head`, `.chat-tools`, `.chat-body`, `.chat-compose`, `.chat-acts`, `.chat-note`, `.chat-cmds`, `.chat-cmd`, `.chat-cmd-name`, `.chat-cmd-arg`, `.chat-cmd-hint` |
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

## §panel.pages — Per-page look rules

What each page does with the library, beyond the rules every page obeys.

**Everywhere.** Thin scrollbars: 10 px, transparent track, no arrows, a rounded thumb
(`scrollbar-width: thin` in Firefox; Terminal re-points `--scroll-thumb`). A content page's
`paneHead` pins and `pane-scroll.ts` adds `.pane.scrolled` (the hairline) once something is
under it. Back to top sits below the menu (z 30), the sheet backdrop (40) and the toast (50),
and is relabelled by the chrome repaint.

**MCP Servers.**

- The resource head pins in two layers (name row, then the tabs); a `ResizeObserver` measures
  them into `--pin-title-h` / `--pane-head-h` so the day headings stick beneath. The detail is
  `.wide`.
- **Logs** is the reference event list: the time only, day headings, a duration column; via /
  client / size in the open row's meta line; ×N for the same tool + arguments + result, folded
  per page; which rows are open survives a poll; bodies load lazily.
- JSON of 80 characters or fewer renders compact on one line. One visible copy button; Copy raw
  behind `⋯`.
- Tools, Resources and Prompts are `row()`s with a `<code>` name, Try and an enable toggle; the
  detail is a native `<details>`; disabled tools get their own section.
- **Run**: the form is built from the input schema (`argFieldsNode`, shared with Jobs); the
  result is a `valueBlock` (past 200 lines: plain text scrolling at 60 vh).
- **Config** is `kvRow`s; revisions are rows (Restore and a trash glyph); a tunnel dependency is
  a row with a dot.
- `.two` is always a grid, and `:is(.fld,.two) + :is(.fld,.two)` spaces them.

**Traffic.** No page title. An Actions / Everything `seg` on the Activity section; Clear
behind `⋯`. The activity is an event list whose who column is client · server; the client
table sits in a card. The page loads the MCP list for its count.

**Token.** A one-line description; `folder-plus` for New group; the token id in mono;
`row()`s with a tag. The one-time secret shows once, in a form card.

**Tunnels.**

- SSH Connections: the rule count is a column; no count at the bottom.
- Port Forwards: New, `⋯` (Start all / Stop all) and `folder-plus` in the head; the local port
  is a column; an error is a red sub-line.
- Sheets are `sheet()` + form, with `formCap`, `formFold` and `field({action})` for Browse.
  `stackSheet` opens a sheet over a sheet with its own backdrop and takes keyboard focus;
  repaint keeps that focus in the layer, and closing restores its connected opener.
  Escape closes only the focused layer, leaving the sheet underneath open.
- The tunnel dot: an amber pulse while reconnecting, grey when stopped (`dot(state, null)`);
  rows show drag feedback.

**Jobs.**

- Row columns: the schedule (`schedToBody().say`, spoken by cronstrue, the raw expression in
  the title), the next run, the last run (`relTime` via `Intl.RelativeTimeFormat`: "OK · 8 hr.
  ago", a red tag on failure, "Never run").
- A disabled job gets an Off tag. No leading dot, except an amber pulse while it runs.
- The runs sheet is an event list: the trigger as title, the first output line as argument, a
  failure as a red tag ("exit 1"), a skipped run as an amber tag with no duration, ×N.
- `field({group})` renders `role=group`; the sheet body and `.tl` use `minmax(0, 1fr)`.
- A stopped Jobs plugin shows `emptyNode` with the clock icon.

**Remote Targets.** The head's sub-line counts the endpoints served by tunnels. A row's sub
is label · endpoint · the root (mono) · capabilities. A dot only when not resting (amber
connecting, red error). The last-run column (No runs; a live pulse while running or queued;
`relTime` with a red tag on failure) is fed by `/api/remote/runs` plus one
`?limit=1&target=` read per uncovered target once per page entry; an API error drops the column
silently, and the poll patches the cell. Editing a target always sends its `id`.

**Remote Runs.** An event list whose who column is target · source; the title is the action,
the argument the command (a sync reads `src → dst`). A failure is a red tag; a live run is an
amber pulse (`live: {text, queued}`). The open row: the meta line, the Output `valueBlock` (a of
b, Load more), the tail block, and live output with Cancel. The head's actions are the search
box (`filterInput`, always shown), the target filter select and `⋯` (Clear, red); a `pager()`
below the list. While a needle is in the box, the list's place holds the search status, the
found runs as the same rows (`hit: {before, match, after}` draws the matched line under the
summary, with the match in a `<mark>`), and Keep searching; the pager is gone.

**Data.**

- Column headers have three lines: the name (PK / FK / sort marks), the type (`.db-col-type`,
  capped at 320 px with an ellipsis) and the comment (`.db-col-comment`, `--text-3`; an empty
  one still holds its line). Comments are `--text-2` in the hover card and the Columns tab.
- Selection is `.db-table.sel`: the 8 % accent tint, a 2 px inset bar, `--w-emph`. The drawers
  use a tick column.
- The grid draws no vertical lines; `td` padding 6 px, `th` 4 px. A row's checkbox and delete
  show on hover, focus, checked or staged (opacity 0, still tabbable). An inserted row always
  has its ×.
- `seg()` for the section switches, `popupMenu` (`.menu.float`) for context menus, `tag()` for
  chips; one `⋯` per head; the status bar does not repeat the editable note. Sheets use
  `sheet({sub})`; a staged write is a `heldDot`; a disabled primary keeps its accent, faded.
- The Redis key's `⋯` offers Set TTL… / Rename… / Delete… (one confirm naming the key) and
  Command. `openFieldSheet` takes `sub` and `allowEmpty` (an empty TTL is PERSIST). A typed-name
  confirm stays only for DROP and TRUNCATE. Stream Follow is a `sw()` switch.

**Terminal.** The stage has its own tokens: `--term-*`, dark `#0b0c0e` (one step below
`--bg`), and a dark stage `#17181b` on the light theme; `.term-page` re-points the panel tokens,
while the accent and the state colours stay the panel's. xterm's background, cursor and
selection come from those tokens (`termTheme` / `readTermTokens` / `applyTermTheme` in
`terminal-core.ts`, re-applied by a `MutationObserver` on `data-theme`); the 16 ANSI colours
are untouched. Session tabs are object tabs (a bell is a `heldDot`); the target picker is a
plain styled select; `.term-jump` sits at z 3; the count is "N live" through `trn`.

**Settings** — §panel.settings.

**Known gaps.** The i18n ratchet does not scan function return values (§panel.i18n).

## §panel.groups — The grouped list

The one grouped-list component for all seven scopes (§host.groups): `groups.ts` (the DOM and
the API calls), `group-logic.ts` (the pure half the tests pin), `ui/group.ts groupNode` (the
drawing). Everything a grouped list does lives here once; no page carries a private copy. The
caller keeps what is
genuinely its own: the row markup, what opening a row does, the flat order it stores and the
noun the delete confirm names.

**Anatomy.** A header **band** over the members, in two densities — the band is what says
"container"; no folder glyph, no guide line, no second surface.

```text
Page (Content template) — .wide measure
┌──────────────────────────────────────────────────────────────────────┐
│ one-sentence description, --text-2                  [⊞]  [Primary]  │  paneHead, pinned; [⊞] = New group
│ one status line, --f-label                                           │
│                               ─ --s5 ─                               │
│ [ Field ……………………… ] [ Field … ] [ group ▾ ] [ Store ]                │  inlineForm (pages that create in place)
│                               ─ --s5 ─                               │
│ ┌──────────────────────────────────────────────────────────────────┐ │
│ │ ▾ default  3                                             +   ⋯  │ │  36 px band, --sep-soft; the whole band drags
│ ├──────────────────────────────────────────────────────────────────┤ │
│ │ ● name                                          [One btn]    ⋯  │ │  row(); its ⋯ lines up with the band's
│ │   mono sub-line                                                  │ │
│ │   ├──────────────────────────────────────────────────────────────┤ │  inset separator, from the text column
│ │ ○ name                                          [One btn]    ⋯  │ │
│ └──────────────────────────────────────────────────────────────────┘ │
│                               ─ --s4 ─                               │
│ ┌──────────────────────────────────────────────────────────────────┐ │
│ │ ▾ test  0                                                +   ⋯  │ │
│ │   No items — drop here or press +                                │ │  a 42 px empty row, not a caption
│ └──────────────────────────────────────────────────────────────────┘ │
│ page foot: prose left, the revision (mono) right                     │
└──────────────────────────────────────────────────────────────────────┘

Sidebar — the same band, smaller, no card
┌────────────────────────────────┐
│ ▾ default  9             +  ⋯  │  28 px band, --r-row
│   ● name-a                 ⌗   │  sideRow() 30 px, one grid step (--s4) in
└────────────────────────────────┘  groups --s2 apart
```

- **The band**: chevron, the name (`--f-body`, `--w-emph`, mixed case), the count
  (`--text-3`, tnum), then `+` (always visible) and `⋯` (hover/focus: Move, Rename, Delete).
  Alignment: on a page the chevron sits in the rows' dot column and the name over their names;
  in the sidebar a member's dot sits under the band's name.
- **Drag.** The whole band moves the group (grab cursor; its buttons opt out at `dragstart`).
  A group lands before or after the whole group under the pointer; a row lands on another row
  (reorder and re-home in one gesture), on a band or on an empty line (append). Feedback:
  `.grp.drop-before/after`, a `.drop-into` ring, `.grp.dragging` dims the group. Moving a
  member between groups never rewrites the flat order (`slice()` / `groupOf()`).
- **Folding** is per scope in `swiss.groups.<scope>.collapsed`, keyed by group name and carried
  across a rename (a renamed group never springs open). The map is prototype-free: a group or
  a keyspace called `constructor` or `__proto__` folds like any other. A filter shows every
  group open.
- **Creating** (rule 6): the create form's Group select lists the scope's groups plus "New
  group…"; it is prefilled from the band's `+` that opened it, else the last group used
  (`swiss.groups.<scope>.last`), else the first. The title names the group.
- **Deleting a group** confirms with the consequence and what survives ("Its 3 jobs move to
  'default'. Nothing is removed."); a member of a deleted group falls to the first group.
- **One request shape**: `saveGroupNames` / rename / assign / `saveOrder` over
  `/api/groups/{scope}` (§host.groups); every write reports its failure.

**Rejected**: a transparent tree head with a folder glyph and a guide line (three visual things
for one container, and at page width the `+` a screen from its name); a hover-only grip as the
drag handle (the user could not find it); a red Delete on every row.

## §panel.settings — Plugins, Secrets, System

Host-owned pages (`pluginId: "host"`, the Settings plugin), always reachable: a disabled plugin
cannot serve the page that re-enables it.

**Plugins** (`views/plugins.ts`) — one row per plugin with its enable switch.

- Enable and disable carry the host revision: a stale one is a visible 409. A plugin that fails
  to start is not a failed request — 200 with state `failed` and `lastError` — and Enable again
  is the retry.
- A dot only when the state disagrees with the switch: an amber pulse in flight, red on failure
  with `lastError` as the row's error line. An unmet requirement is a warn tag ("no provider")
  on the sub-line. The sub-line counts the plugin's pages (the list in its title); the page
  foot is the revision.
- The poll patches rows in place, so a switch keeps its focus.
- A **This machine** section holds start at sign-in (`/api/autostart`) and swiss on the user's
  PATH (`/api/user-path`): the OS is the store, no revision (§host.cli). A PATH change toasts
  that only new terminals see it. A gateway without a route shows no row for it.

**Secrets** (`views/secrets.ts`) — the vault (§host.vault). Names only, write-only: a value is
never shown again, and a forgotten one is re-stored, never revealed.

- A row shows the **reference** to copy (`${secret://name}`, a Copy ref ghost button).
- Writes carry the vault revision.
- Groups through the groups component; rows drag within and across groups; the stored order is
  the vault's third list (empty = by name).
- Create in place: an `inlineForm` with a Group select, preselected by the band's `+`.
- Replace value lives in the row's `⋯`: a store under the same name, after which the gateway
  rebuilds every MCP that references it and the toast names them.

**System** (`views/system.ts`) — the one running process: one `row()`, and Quit through a
confirming sheet. Quit stays red: it is the page's one act (the rule 4 exception).

## §panel.i18n — Languages

English and Simplified Chinese. The scope is the **panel's own copy**: a string the server
sends is data, shown as sent inside a translated frame (`{name}` placeholders); identifiers —
names, keys, SQL, paths — are never translated.

- **Keys are symbolic**, `<module>.<semanticId>` (`addSheet.addMcp`), in `locales/en.ts` and
  `locales/zh.ts` (TS sources, about 1,800 entries each). English is a static import and the
  floor; Chinese is a lazy import. The switch to symbolic keys was one codemod
  (`panel/scripts/migrate-i18n-keys.mjs`, kept as the record; it only runs on the pre-migration
  tree).
- `tr(key, vars)` looks up the active dictionary, then English, then answers the key itself.
  `trn(n, oneKey, otherKey, vars)` picks through `Intl.PluralRules` of the installed locale
  (Chinese has only `other`). `tk(key)` is an identity marker for module-level tables: module
  top level never calls `tr`, because the language is not loaded yet. `wireLabel(served)` maps
  a gateway-served English label (a plugin or page name) to a `wire.*` key.
- `locale()` answers `en` or `zh-CN` and is what every `toLocale*` / `Intl` call receives.
- **The preference** is `swiss_lang`; absent is English. `navigator.language` is not
  consulted. `<html lang>` follows it. A top-level `await loadLocale()` runs before the first
  paint.
- **The flip** (文/A in the context bar) is `setLang` → repaint the chrome → re-navigate the
  current page, without a reload. A page whose `canLeave()` refuses blocks it, and then the
  preference is not written. Blocked storage keeps the choice in memory; a failed Chinese load
  falls back to English.
- **No context mechanism**: a string that needs different words in two places gets two keys.
- **Chinese style**: full-width punctuation after Han characters.

**The gates** (vitest, inside `npm run check`):

| Test | Fails when |
|---|---|
| `i18n-complete` | a `tr`/`tk` key, or a `trn` other-key, found by walking the AST has no entry — or an entry has no user |
| `i18n-ratchet` | a bare English literal sits in a visible position: an `h()` child (except in `code`, `kbd`, `pre`), a `title` / `placeholder` / `aria-label` prop, the first argument of `toast` / `confirm` / `prompt` / `say`, an `emptyNode` title / hint / action. Zero allowed. It is an AST test rather than an eslint rule because `no-restricted-syntax` is held by the `innerHTML` rule. Known gap: it does not scan function return values |
| `i18n-zh-style` | a Chinese value has a half-width `,` after a Han character |
| `i18n-toggle`, `i18n-shell-zh`, `i18n-load-fail`, `i18n` | The flip, the Chinese shell, the English fallback, the helpers |

## §panel.toolchain — TypeScript, the build and the emit

The panel is written in TypeScript and served as the JavaScript it erases to (ADR-024).

- **Home**: `crates/swiss-panel/panel/` — `package.json`, `build.mjs`, two tsconfigs,
  `eslint.config.js`, `src/`, `test/`, `scripts/`. Node ≥ 24.
- **The emit is type erasure, line for line.** `build.mjs` runs `ts-blank-space` over each
  `src/**/*.ts` into `crates/swiss-panel/src/admin_assets/js/`: types become spaces, so a line
  and column in the browser are the line and column in the source. No bundle, no minify, no
  source map, no rewriting of imports. It writes only changed files, skips `.d.ts`, never
  deletes and never touches `js/vendor/`.
- **The emit is committed** and marked `linguist-generated`, so the binary builds with no Node
  on the machine. It is never edited by hand. `panel-emit.test` re-emits in memory and fails
  on a stale file or an orphan.
- **tsconfig**: `strict`, `verbatimModuleSyntax`, `erasableSyntaxOnly` (nothing that needs a
  transform: no enums, no namespaces, no parameter properties), `isolatedModules`, target
  es2022, module esnext, resolution bundler, lib es2022 + dom + dom.iterable. Two projects: the
  source (no Node types) and the tests.
- **Imports** use the emitted `./x.js` specifier; types come in through `import type`.
- **A behaviour change is named and tested.** A commit that changes how the panel behaves (not
  only how it is written) says so in its message and carries the vitest case that pins it;
  deleting a test to pass a gate is never allowed.
- **Types.** The `any` budget is zero. API types are copied from the Rust serde structs field
  by field (`types/api.d.ts`), index signatures included; a field the panel reads that Rust
  does not send is a bug. The `.d.ts` files export their types explicitly. No augmentation of
  built-in types. Event handlers use `e.currentTarget`, never `this`; an error is shown through
  `errText(e)`.
- **DOM building.** `h()` / `frag()` / `fill()` build nodes; nothing writes `innerHTML`
  (§panel.lint), and there are no inline `on…` attributes. The one HTML string the panel reads
  is a vendored library's own output (markdown-it's, mermaid's sandbox iframe): `ui/markdown.ts`
  parses it with `DOMParser` into an inert document and moves the nodes over. A view wires one delegated listener
  and dispatches on `data-act`. **A delegated handler resolves its record, key and buffer from
  live state at event time**, never from what the render captured: a menu built at click time
  must still act on the right object after a tab switch or a reload under it.
- **State** is six slices — `db-state`, `job-state`, `mcp-state`, `traffic`, `tunnel-state`,
  `ui-state` — each owning its data and its setters; `util.ts` exports no `state`.
- **Scripts.** `build` (`node build.mjs`), `build:check` (fail if the committed emit is stale),
  `dev` (`--watch`), `typecheck` (both projects), `lint` (`eslint .`), `test` (`vitest run`),
  and **`check` = typecheck && lint && build:check && test** — the panel's one gate.
- **Dev tools**: `typescript` ~6.0, `ts-blank-space` ~0.9, `eslint` 10 with
  `typescript-eslint` 8, `vitest` 5 on `happy-dom`, `vite` 8 (vitest's), `@types/node` 24.
  Dev-only: nothing from npm reaches the browser.
- **The dev loop**: `npm run dev` beside a debug gateway — save the `.ts`, the emit updates,
  reload the page (debug builds read assets from disk).
- **Gates.** `scripts/deploy.ps1` runs `npm run check` after clippy, so a deploy can never
  serve a panel whose committed emit predates its sources; CI has a panel job (Ubuntu, Node 24, `npm ci`, `npm run check`). Vitest runs
  capped (`--maxWorkers=4`).

## §panel.lint — The lint ratchet

`eslint.config.js`, type-aware over both tsconfigs. No prettier: formatting is by hand, like
the Rust tree. No whole-file disable; a single-line disable carries its reason.

| Scope | Rule | Level |
|---|---|---|
| src | `no-explicit-any` | error |
| src | `no-floating-promises` | error |
| src | `no-var`, `prefer-const` | error |
| src | `consistent-type-imports` (separate type imports) | error |
| src | `no-unused-vars` (`^_` arguments ignored, caught errors not checked) | error |
| src | `no-non-null-assertion` | error, plus a per-file warn allowlist frozen by `test/non-null-ratchet.test.ts` |
| src | `no-restricted-imports`: `state` from `util.js` | error |
| src | `no-restricted-syntax`: any `.innerHTML =` or `+=` | error, no allowlist |
| src | `no-unnecessary-condition` | warn — a burn-down (496 sites on 2026-09-20) that flips to error at zero |
| `src/types/**/*.d.ts` | `no-restricted-syntax`: an interface named after a built-in (`Function`, `EventTarget`, `RegExp`, `Window`, `Element`, `Array`, `String`, `Number`, `Boolean`, `Object`, `Promise`) — narrow at the call site instead | error |
| test | `no-explicit-any`, `no-floating-promises` | warn |

The non-null allowlist and every other frozen count follow the ratchet convention
(§panel.ui): only down, in the same commit.

## §panel.vendor — Vendored browser libraries

A browser library the panel needs is vendored, never an npm runtime dependency and never a
CDN:

| Package | Version | Used by |
|---|---|---|
| `@xterm/xterm` + addons fit 0.10.0, search 0.16.0, unicode11 0.8.0, web-links 0.11.0, webgl 0.18.0 | 5.5.0 | Terminal (§terminal.panel) |
| `cronstrue` | 2.52.0 | Jobs: the spoken schedule, loaded on first use |
| `shlex` | 3.0.0 | Data: splitting a Redis console line (§data.redis-console) |
| `cytoscape` | 3.34.3 | Memory: the project graph and a note's neighbourhood (§memory.panel), loaded on first use |
| `markdown-it` | 15.0.2 | Every Markdown the panel draws (`ui/markdown.ts`): notes, hubs, briefs, previews |
| `@mermaid-js/tiny` (mermaid's tiny build) | 12.1.0 | A ```` ```mermaid ```` fence in that Markdown, drawn as its diagram; loaded on first use |

- Files live in `src/admin_assets/js/vendor/<pkg>/<version>/` beside the package's `LICENSE`,
  byte-identical to the npm dist files — no patches. xterm adds `load-classic.js` and one
  `index.js` shim per package; the cytoscape and mermaid shims reuse that loader. An upstream
  that ships a self-contained ES module (shlex, markdown-it's `dist/browser/*.esm.min.mjs`) is
  re-exported as it is; the server types `.mjs` as JavaScript. The xterm addon-search, cytoscape,
  markdown-it and mermaid bundles are pinned by SHA-256 in the panel tests.
- A vendored library is MIT, BSD, ISC, Apache-2.0 or MPL-2.0, as `deny.toml` holds the crates, and
  so is everything it bundles. That is why mermaid is its tiny build: the full `mermaid.min.js`
  bundles the Eclipse ELK layout engine (EPL-2.0). Tiny lacks mindmap and architecture diagrams
  and KaTeX; such a fence stays as code, the reason in its title. `THIRD_PARTY_NOTICES.md` lists
  each vendored library and what it bundles.
- The types are a hand-written mirror in `panel/src/vendor/<pkg>/<version>/index.d.ts` that
  declares only the exports the panel uses.
- An upgrade is one commit: the new directory, the import change, the old directory deleted.

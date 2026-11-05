# Pages

Part 02 of [§panel](README.md), chapter 0014 of the [swiss specification](../../SPEC.md).

## §panel.pages — Per-page look rules

What each page does with the library, beyond the rules every page obeys.

**Everywhere.** Thin scrollbars: 10 px, transparent track, no arrows, a rounded thumb
(`scrollbar-width: thin` in Firefox; Terminal re-points `--scroll-thumb`). A content page's
`paneHead` pins and `pane-scroll.ts` adds `.pane.scrolled` (the hairline) once something is
under it. Back to top sits below the menu (z 30), the sheet backdrop (40) and the toast (50),
and is relabelled by the chrome repaint.

**MCP Servers.**

- In a desktop window, an unselected pane with registered servers shows a workspace landing
  page built from `paneHead`, `section`, `card` and `row`: up to six explicit Open actions,
  plus Add MCP. It reads the inventory only; rendering never requests tools or wakes a lazy
  process. Polls patch state marks and keep keyboard focus. The sidebar remains the complete
  list. An empty store retains the shared empty state. The gallery's `desktop-overview` scene
  shows the composition; `mcp-overview.test.ts` drives its actions, text safety and focus.
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

Menus clamp to eight-pixel viewport margins. A flex column keeps the find box above a scrolling
row list. Arrows scroll focused rows into view, Enter activates and Escape restores trigger focus.
Search drops empty headings; resize or outside scrolling closes stale placement. Anchored menus
remain owned by the head but use fixed, clamped geometry. The chatPane context slot sits between
queue and input, letting references change without replacing the editor. The header places a
subject beside its picker. The textarea and bottom toolbar share one bordered input card; leading
permission controls and trailing model/send/stop actions belong inside it. Statistics remain below
the card and the message queue remains above it.

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

## §panel.settings — System, Plugins, Secrets

Host-owned pages (`pluginId: "host"`, the Settings plugin), always reachable: a disabled plugin
cannot serve the page that re-enables it.

The context bar orders them System, Plugins, Secrets; a new user's Settings seat lands on
System. The per-plugin last-page preference still reopens an existing user's last page.

**Plugins** (`views/plugins.ts`) — one row per plugin with its enable switch.

- Enable and disable carry the host revision: a stale one is a visible 409. A plugin that fails
  to start is not a failed request — 200 with state `failed` and `lastError` — and Enable again
  is the retry.
- A dot only when the state disagrees with the switch: an amber pulse in flight, red on failure
  with `lastError` as the row's error line. An unmet requirement is a warn tag ("no provider")
  on the sub-line. The sub-line counts the plugin's pages (the list in its title); the page
  foot is the revision.
- The poll patches rows in place, so a switch keeps its focus.

**Secrets** (`views/secrets.ts`) — the vault (§host.vault). Names only, write-only: a value is
never shown again, and a forgotten one is re-stored, never revealed.

- A row shows the **reference** to copy (`${secret://name}`, a Copy ref ghost button).
- Writes carry the vault revision.
- Groups through the groups component; rows drag within and across groups; the stored order is
  the vault's third list (empty = by name).
- Create in place: an `inlineForm` with a Group select, preselected by the band's `+`.
- Replace value lives in the row's `⋯`: a store under the same name, after which the gateway
  rebuilds every MCP that references it and the toast names them.

**System** (`views/system.ts`) owns the machine and runtime settings:

- **This machine** holds start at sign-in (`/api/autostart`) and swiss on the user's PATH
  (`/api/user-path`): the OS is the store, no revision (§host.cli). A PATH change toasts that
  only new terminals see it. A gateway without a route shows no row for it.
- **AI skills** reads `GET /api/skills` on mount and explicit Refresh. Each shipped skill
  (`swiss`, `swiss-memory`, `swiss-remote`) has one row, with a status for `.claude` and
  `.agents` (§host.cli). A `SKILL.md` file means installed; a missing file means not installed;
  an unreadable path means cannot check, with the path and error on hover. The page never
  installs on mount or Refresh. Each skill/directory cell has a download action that installs
  or updates only that target through `POST /api/skills/install`; its accessible label names
  both the skill and directory. Only that cell is disabled during the write. Errors retain
  the prior status, successful writes update the selected cell, and late responses cannot
  repaint another page.
- **Runtime** keeps Quit through a confirming sheet. Quit stays red (the rule 4 exception).


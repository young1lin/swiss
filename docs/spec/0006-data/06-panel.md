# Object tabs and the session cache

Part 06 of [§data](README.md), chapter 0006 of the [swiss specification](../../SPEC.md).

## §data.tabs — Object tabs, the tree and the bars (ADR-026)

- **State.** Connection scope (`DbConnState`) is split from a tab list (`DbTab[]`, a discriminated
  union of `table`, `sql`, `key`, `coll` (§data.mongo), `activity`, `pubsub` (§data.pubsub)). One `dbOpenTab()` opens
  anything; opening an open object activates it; an FK jump opens a new tab and keeps the source.
- **Cap 12** (`DB_TAB_MAX`). Room is made by evicting the least recently used clean, inactive
  tab (the toast names it); when every tab is busy `dbOpenTab` refuses with a toast, but the
  strip's `+` always opens a SQL tab. Background tabs drop their rows and refetch at their
  offset and filters on activation, keeping edits. Closing a dirty tab confirms;
  `hasPendingChanges()`/`canLeave()` count every tab.
- **The strip** is a scroll area plus a pinned end (overflow menu, `+`); the active tab scrolls
  into view; a schema prefix equal to the current scope is dropped (the title is always full).
  The overflow menu lists every tab plus Close others / to the right / all. The wheel scrolls
  it, middle-click closes, right-click offers rename and close left/right. Card tabs are a third
  shape, distinct from underline tabs and pills (§panel.design).
- **Recently closed.** Consoles with text are saved before close, database reset and page exit;
  a blank one is forgotten, whatever its name or owner. They retain stable ID, text, names,
  creation/update times and separate close time. Equal text never merges identities. Storage holds at most 200 entries
  and one million characters across connections; older saved entries expire first, while a close
  that cannot save its incoming consoles is refused. Failed page-exit saves request the browser's
  leave confirmation. Restoration checks capacity before removing history, preserves identity
  and metadata, and never reruns SQL. Search groups by local update day, unknown times last
  (§data.agent); details expose ID and times. Once the page has read the server store
  (§data.agent), this list is those closed records: Clear deletes only them, open consoles
  stay, and the local list is dropped only after that delete succeeds. Without the store,
  Clear forgets this connection's local entries alone. The 200-entry and one-million-character
  bounds are that local fallback and the one-time import, not the server records. A reload of
  a store-backed connection restores the open consoles; it does not file them closed on the
  way out.
- **Object shortcuts.** SQL table/view menus toggle favorites. Favorites (100) and recent visits
  (20) are scoped by connection/database, with schema in each address. Optional single-level
  prefix groups use the first underscore, colon or dot. Groups mount rows only on expansion,
  200 per reveal, with creation/sort controls reachable. Storage holds addresses only: at most
  64 scopes and 500,000 characters; exceeding a bound refuses new preferences, preserving favorites.
- **Coordinated columns.** Rendered widths clamp without writing grip preferences. Below a
  1100 px workspace sidebar/Agent use at most 21%/40%; below 760 px the Agent becomes an end
  overlay with a visible close action. Widening restores saved widths.
- **The sidebar tree** has Tables / Views / Routines sections (collapsible groups, §panel.groups;
  `+` only on Tables); pg nests schema bands over them. Each section renders 200 rows
  (`DB_TREE_ROW_CAP`), "show N more" adds 500, "Show fewer" sits at the end; search results
  are uncapped. Connection and database pickers are drawers, one open at a time.
- **Toolbars** show only the active tab's controls: table = view segment + one primary + ⋯;
  sql = Run all and Run + ⋯; key = the type's primary;
  activity and pubsub = Refresh (an icon button, `actionBtn("refresh")` — §panel.design rule 7).
  Short actions use icon buttons. Run all uses list-play to distinguish its scope from Run.
  Refresh / CSV / Export / Import live in ⋯.
- **The status bar** — range, pager, page size, elapsed, editNote, connection — is separate
  from the commit bar. The pager and the page size are the **row views'** only (Data and
  Form): a schema pane (Structure's columns / indexes / foreign keys, or DDL) lists the
  table's shape, not its rows, and carries neither — the connection stays at the right edge.
- **Structure** folds its panes into four tabs: Data / Form / Structure / DDL.
- Not built: docking, drag-reorder, workspace persistence.

## §data.sessions — The session cache

- **Parking.** A session is the connection record, its tab strip and the active index, parked
  per connection name. Switching connection parks the current session and restores the target
  with no confirm; pending edits travel with their session. Leaving the page parks it and
  remembers the connection. `canLeave` asks about pending edits across **all** sessions.
- **Quiet re-validation.** A restored session paints at once, then re-reads the catalog and the
  front table page at their coordinates, repainting only if the answer changed; the front key's
  value too, unless it has pending edits. The Redis key list is re-walked quietly to the same
  depth on restore, on `r`, on switch-back and after each console command. Console results are
  never re-run.
- **Limits.** A soft cap of 8 parked sessions (`DB_PARKED_MAX`) evicts the least recently used
  clean one — a session with pending edits or a Console identity is never evicted; sessions of vanished connections
  are dropped. Parking stops the Activity poll.
- Not built: a (connection, database) key — switching database still resets; nothing survives
  a reload.

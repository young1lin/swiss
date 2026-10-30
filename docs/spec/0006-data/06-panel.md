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
- **Recently closed.** A console tab holding text is filed before it goes - its ×, a bulk
  close, a database switch, and the page going (`pagehide`: the live strip's and every parked
  session's) - with its text, its name and, for one of the Data agent's, the name the agent
  knows it by (`data-closed.ts`; `swiss.dbClosedTabs` in this browser, newest first, the
  same connection and text once, 30 at most across connections and under a million
  characters, the oldest going first). The overflow menu then offers *Recently closed* (shown
  with nothing open too): the connection's closed consoles, each its name and first line, its
  text on hover, a find box past nine; a pick opens it as a new console in front with its text
  and name - the agent's again while no open tab is the agent's under that name - and leaves
  the list. *Clear the list* forgets the connection's. A result is not kept: its rows are a
  read to run again. A table, key or collection tab is an address, opened again from the
  sidebar, and is not filed. Before this, a closed console was gone for good (2026-10-29; `a
  closed console can be brought back`).
- **The sidebar tree** has Tables / Views / Routines sections (collapsible groups, §panel.groups;
  `+` only on Tables); pg nests schema bands over them. Each section renders 200 rows
  (`DB_TREE_ROW_CAP`), "show N more" adds 500, "Show fewer" sits at the end; search results
  are uncapped. Connection and database pickers are drawers, one open at a time.
- **Toolbars** show only the active tab's controls, at most one non-icon button each (a test
  gates it): table = view segment + one primary + ⋯; sql = Run + ⋯; key = the type's primary;
  activity and pubsub = Refresh. Refresh / CSV / Export / Import live in ⋯.
- **The status bar** — range, pager, page size, elapsed, editNote, connection — is separate
  from the commit bar.
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
- **Limits.** At most 8 parked sessions (`DB_PARKED_MAX`), evicting the least recently used
  clean one — a session with pending edits is never evicted; sessions of vanished connections
  are dropped. Parking stops the Activity poll.
- Not built: a (connection, database) key — switching database still resets; nothing survives
  a reload.

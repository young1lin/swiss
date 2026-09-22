/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

                                                                                                                     

/* The Data view owns its state (docs/37 R4, slice 6 of 7; docs/42 §2 splits the record).

   docs/37 R4 made this record a never-null singleton with ONE read mouth (dbView) because
   every reader already lived in the db domain. docs/42 cuts that record in two along the
   line the object tabs need: DbConnState is what survives switching the open object, and
   DbTab is one open object — a discriminated union, so a read of the wrong domain is a
   typecheck error rather than a browser bug. The four accessors below replace dbView()
   with no compatibility alias: an alias would let callers keep writing the old shape.

   The lifecycle contract is unchanged: the records are never null. Between unmount and
   the next mount they are simply the fresh literals, so a debounce or a poll that
   outlives the view reads empties instead of throwing; whether the view is actually on
   screen stays dbIsMounted(). There is still no per-field accessor pair — the domain
   gets typed handles, not 98 forwarding functions.

   A leaf module, same reason as tunnel-state and job-state: data-view.ts is imported by
   every other data-* module, so hosting the record there closes a cycle. This file
   imports types only.

   T2 shape (docs/42 §4): the strip holds many tabs and an active index. What it never
   holds is zero tabs — closing the last one leaves a PLACEHOLDER (a table tab with no
   table, a key tab with no key), which is what a fresh mount starts from. The placeholder
   keeps dbTab() total, so no reader needs a null branch, and data-tabs.ts keeps it off the
   strip and out of the cap. The policy — open, close, activate, evict — lives there; this
   file owns only the records and the stamps. */
function freshConnState()              {
  return {
    conns: [],            // rows from /api/db
    conn: null,           // selected connection (MCP name)
    tables: [],           // the WHOLE catalog (one fetch, capped at DB_TREE_FETCH_LIMIT)
    tablesTotal: 0, more: false, grep: "", treeShowAll: {},
    schemaFilter: "",     // pg only: "" = every schema; the /tables schema param (docs/22 W1.1)
    sort: "name", sortDir: "asc", // the list's sort key/dir — SQL sorts server-side, redis client-side
    gridCfg: { widths: {}, hidden: [] }, // per-connection column widths/hides (docs/22 W2.1), reloaded per page
    history: [],         // last-run console queries, newest first (per-browser, localStorage)
    favorites: [],       // starred queries, same store (docs/22 W5.4)
    redis: null,         // { keys, cursor, done, total } while a redis connection is selected
    redisType: "",      // SCAN TYPE filter — "" walks every type (string/hash/list/set/zset/stream)
    redisError: false,  // the last /keys fetch FAILED (docs/22 closeout B1) — the list must say so, not "no keys"
    database: "",       // docs/43 M3: the SELECTED database ("" = the connection's configured one)
    databases: null,    // docs/43 M3: lazy /databases catalog (null = never asked; [] = no axis → no row)
  };
}

/* The strip's LRU clock (docs/42 D3). Monotonic and view-lifetime scoped: a tab born or
   activated later always outranks one born or activated earlier, which is the whole
   question eviction asks. It is deliberately not a wall clock — Date.now() would make the
   cap behave differently for a fast operator than a slow one, and two tabs opened in the
   same millisecond would tie. */
let tabClock = 0;

/** A fresh tab of one kind, carrying ONLY that kind's fields (the T1 acceptance pins the
 *  shapes: a sql tab has no table field, a key tab no sqlText — the union stays honest
 *  from the first builder). Overloaded so a literal kind yields the literal member type;
 *  the final DbTabKind overload lets a computed union kind (the conn-family switch in
 *  dbResetTabForConn) build without a cast. */
                                                    
                                                
                                                
                                                          
                                                 
export function freshTab(kind           )        {
  // Each branch spells its literal kind: a shared { kind } base would widen the
  // discriminant to DbTabKind and the member object would not narrow to its arm.
  if (kind === "table") {
    return {
      kind: "table", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false, touched: ++tabClock,
      table: null, schema: null,
      data: null,           // last /api/db/:name/data page
      filters: [],          // [{ column, op, value }] — server-side WHERE terms (AND-ed)
      pageSize: 50, offset: 0, order: null, dir: "asc",
      updates: {},          // pkKey -> { pk, changes: { col: value-or-null } }
      deletes: {},          // pkKey -> pk object
      inserts: [],          // [{ values: { col: value-or-null } }]
      pane: "data",         // data | form | columns | indexes | fks | ddl — Form is the data page's own second view (W5.1); six folds into four with docs/42 T4
      formIdx: 0,           // the Form tab's record — grid row space (inserts first), shared with the keyboard focus
      detail: null,         // last /api/db/:name/schema answer (BrowseTableDetail)
      detailBusy: false,
      conflict: null,       // the fetch observer's 409 mark; cleared when the row is re-edited
    };
  }
  if (kind === "sql") {
    return { kind: "sql", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false, touched: ++tabClock,
      sqlText: "", sqlResult: null, sqlResults: null, resultTab: 0, sqlBusy: false };
  }
  if (kind === "key") {
    return { kind: "key", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false, touched: ++tabClock,
      redisKey: null, redisValue: null, redisEdits: null };
  }
  return { kind: "activity", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false, touched: ++tabClock,
    activityRows: null };
}

let connRec              = freshConnState();
let tabs          = [freshTab("table")];
let activeIdx = 0;
let mounted = false;

/** The connection-scoped record: the sidebar's list, its filters, and what the console
 *  remembers. Shared by every open tab; reset when the connection changes. */
export function dbConn()              { return connRec; }

/** The active tab's record. Never null: the strip always holds at least the placeholder,
 *  so the view always has somewhere to paint. Narrow with `t.kind` before reading kind
 *  fields — the discriminant is the split's safety net. */
export function dbTab()        { return tabs[activeIdx]; }

/** Every open tab, in strip order — the LIVE array, so data-tabs.ts opens and closes by
 *  splicing it. The active one is `dbTabs()[dbActiveIndex()]`. */
export function dbTabs()          { return tabs; }

/** The active tab's index into dbTabs(). */
export function dbActiveIndex()         { return activeIdx; }

/** The sql tab the pane is showing, or null when the open object is not a console. The
 *  question `dbConn().sqlResult` used to answer when the console overlaid the pane
 *  (docs/42 T1) — every reader of a query reply asks it. */
export function dbSqlTab()                  {
  const t = tabs[activeIdx];
  return t.kind === "sql" ? t : null;
}

/** Activate the i'th tab and stamp it as the most recently used. An index outside the
 *  strip is clamped rather than trusted: a stale render's address must not leave the view
 *  pointing at a tab that no longer exists. */
export function dbSetActive(i        )       {
  activeIdx = Math.max(0, Math.min(tabs.length - 1, i | 0));
  tabs[activeIdx].touched = ++tabClock;
}

/** Replace the whole strip — the connection switch, which leaves every open object behind
 *  with the connection it belonged to. `tabs` is reassigned rather than emptied in place so
 *  a render still holding the old array paints the old tabs instead of an empty strip. */
export function dbResetTabs(next         , idx        )       {
  tabs = next.length ? next : [freshTab("table")];
  dbSetActive(idx);
}

/** Is the Data view mounted? The question the old `if (!state.db) return;` guards were really
 *  asking — an async continuation that crossed an unmount has nothing left to paint. */
export function dbIsMounted()          { return mounted; }

/** Entering the view. */
export function mountDbView()       { mounted = true; }

/** Leaving it: both records go back to the fresh literals, which is what nulling them used
 *  to buy — the rows, the buffered edits and the query results are dropped together. */
export function unmountDbView()       {
  connRec = freshConnState();
  tabs = [freshTab("table")];
  activeIdx = 0;
  mounted = false;
}

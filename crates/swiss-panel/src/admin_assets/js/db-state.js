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

                                                                                                                                

/* The Data view owns its state (SPEC §panel.toolchain, slice 6 of 7; SPEC §data.tabs splits the record).

   SPEC §panel.toolchain made this record a never-null singleton with ONE read mouth (dbView) because
   every reader already lived in the db domain. SPEC §data.tabs cuts that record in two along the
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

   T2 shape (SPEC §data.tabs): the strip holds many tabs and an active index. What it never
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
    tablesTotal: 0, more: false, grep: "", treeShown: {},
    schemaFilter: "",     // pg only: "" = every schema; the /tables schema param (SPEC §data.browse)
    sort: "name", sortDir: "asc", // the list's sort key/dir — SQL sorts server-side, redis client-side
    gridCfg: { widths: {}, hidden: [] }, // per-connection column widths/hides (SPEC §data.grid), reloaded per page
    history: [],         // last-run console queries, newest first (per-browser, localStorage)
    favorites: [],       // starred queries, same store (SPEC §data.console)
    redis: null,         // { keys, cursor, done, total } while a redis connection is selected
    redisType: "",      // SCAN TYPE filter — "" walks every type (string/hash/list/set/zset/stream)
    redisError: false,  // the last /keys fetch FAILED (SPEC §data) — the list must say so, not "no keys"
    database: "",       // SPEC §data.databases: the SELECTED database ("" = the connection's configured one)
    databases: null,    // SPEC §data.databases: lazy /databases catalog (null = never asked; [] = no axis → no row)
  };
}

/* The strip's LRU clock (SPEC §data.tabs). Monotonic and view-lifetime scoped: a tab born or
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
      pane: "data",         // data | form | columns | indexes | fks | ddl — Form is the data page's own second view (W5.1); six folds into four with SPEC §data.tabs
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
  if (kind === "coll") {
    return {
      kind: "coll", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false, touched: ++tabClock,
      db: null, coll: null,
      pane: "docs",         // docs | agg | schema | indexes | explain | validation (SPEC §data.mongo-panel)
      view: "list",         // the documents' presentation: list (shell text) | json (EJSON) | table
      qFilter: "", qProject: "", qSort: "", qSkip: 0, qLimit: 20, qMore: false, qError: null,
      docs: null, more: false, total: null, estimated: false, countTimedOut: false, elapsedMs: null,
      stages: [], aggText: null, aggResult: null, aggBusy: false,
      schema: null, schemaBusy: false, schemaSample: 1000,
      indexes: null, explain: null, explainOf: "find", validatorText: null,
    };
  }
  return { kind: "activity", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false, touched: ++tabClock,
    activityRows: null };
}

let connRec              = freshConnState();
let tabs          = [freshTab("table")];
let activeIdx = 0;
let mounted = false;

/* SPEC §data.sessions: one PARKED session per connection. A session is the connection record, its strip
   and the index in front - the three things the switch used to rebuild from nothing, which
   is how "MySQL, then Redis, then back" lost every open table. The park swaps whole objects
   instead of copying fields: a request that captured its record or tab before an await
   lands in the session it belongs to, never in the one now on screen.

   The Map's insertion order is the LRU order (a park re-inserts), so the oldest session is
   the first key. `lastConn` is the connection the page left on, which the next mount
   resumes instead of falling back to the registry's first row. */
                                                                           
const parked = new Map                   ();
let lastConn                = null;

/** How many sessions the park holds (SPEC §data.sessions). Connections are a handful; the cap only
 *  bounds the pathological case, and a session holding buffered writes is never the one to go. */
export const DB_PARKED_MAX = 8;

/** Park the live session under its connection. `pinned` says which sessions hold work that
 *  must not be evicted (the caller counts buffered writes; this leaf imports types only). A
 *  load cut off by the park must not come back as a spinner that never stops, so the busy
 *  flags reset here: the resume re-reads whatever the cut-off load was for. */
function park(pinned                            )       {
  const name = connRec.conn;
  if (!name) return;
  for (const t of tabs) {
    t.loading = false;
    if (t.kind === "table") t.detailBusy = false;
  }
  parked.delete(name);
  parked.set(name, { conn: connRec, tabs: tabs, activeIdx: activeIdx });
  for (const [k, v] of parked) {
    if (parked.size <= DB_PARKED_MAX) break;
    if (k !== name && !pinned(v.tabs)) parked.delete(k);
  }
}

/** Put a parked session back live. The registry list and the per-browser stores (query
 *  history, favorites) are the PAGE's, not the session's: they carry from the record being
 *  replaced, so a session parked an hour ago does not bring back an old connection list. */
function resume(s           , from             )       {
  connRec = s.conn;
  connRec.conns = from.conns;
  connRec.history = from.history;
  connRec.favorites = from.favorites;
  tabs = s.tabs;
  activeIdx = Math.max(0, Math.min(tabs.length - 1, s.activeIdx));
}

/** Switch the live session to connection `name` (SPEC §data.sessions): park the one on screen, then
 *  take `name`'s parked session back - or, for a connection not seen yet, start a fresh record
 *  whose strip the caller seeds (it knows the connection's family). True when a parked session
 *  came back. */
export function dbSwapConn(name        , pinned                            )          {
  park(pinned);
  return take(name);
}

/** The live connection left the registry: go to `name` (null = there is no connection at all)
 *  WITHOUT parking the one on screen - nothing can be read or committed through it any more. */
export function dbReplaceConn(name               )          {
  return take(name);
}

function take(name               )          {
  const from = connRec;
  lastConn = name;
  const s = name ? parked.get(name) : undefined;
  if (s && name) {
    parked.delete(name);
    resume(s, from);
    return true;
  }
  connRec = freshConnState();
  connRec.conns = from.conns;
  connRec.history = from.history;
  connRec.favorites = from.favorites;
  connRec.conn = name;
  tabs = [freshTab("table")];
  activeIdx = 0;
  return false;
}

/** Leaving the page (SPEC §data.sessions): the live session parks and is remembered, and the live
 *  records go back to the fresh literals so nothing unmounted reads as on screen. */
export function dbSuspendView(pinned                            )       {
  lastConn = connRec.conn;
  park(pinned);
  connRec = freshConnState();
  tabs = [freshTab("table")];
  activeIdx = 0;
  mounted = false;
}

/** The mount's half of the page leave: bring the session the page left on back live. False
 *  when there is none (a first visit, or its connection was forgotten meanwhile). */
export function dbResumeView()          {
  const s = lastConn ? parked.get(lastConn) : undefined;
  if (!s || !lastConn) return false;
  parked.delete(lastConn);
  // The session keeps its OWN registry list until /api/db answers: the fresh record it replaces
  // has none, and an empty list would dress a Redis session as SQL in the skeleton.
  resume(s, s.conn);
  return true;
}

/** Every parked session's strip - what the page-leave and unload guards count alongside the
 *  live one (SPEC §data.sessions), and what a confirmed leave drops the buffered writes from. */
export function dbParkedTabs()            {
  return Array.from(parked.values(), (s           )          => s.tabs);
}

/** The parked connections, oldest first. */
export function dbParkedNames()           {
  return Array.from(parked.keys());
}

/** Drop the sessions of connections that left the registry (SPEC §data.sessions): nothing can be read
 *  or committed through a connection that is gone. */
export function dbForgetParked(keep                           )       {
  for (const k of Array.from(parked.keys())) if (!keep(k)) parked.delete(k);
  if (lastConn && !keep(lastConn)) lastConn = null;
}

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
 *  (SPEC §data.tabs) — every reader of a query reply asks it. */
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

/** The FULL reset: both records back to the fresh literals and the park emptied - the rows,
 *  the buffered edits and the query results of every session dropped together. The page's own
 *  leave parks instead (dbSuspendView, SPEC §data.sessions); this is the reset a suite starts from. */
export function unmountDbView()       {
  connRec = freshConnState();
  tabs = [freshTab("table")];
  activeIdx = 0;
  mounted = false;
  parked.clear();
  lastConn = null;
}

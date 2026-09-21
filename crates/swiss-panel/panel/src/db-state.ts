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

import type { DbActivityTab, DbConnState, DbKeyTab, DbSqlTab, DbTab, DbTabKind, DbTableTab } from "./types/state.js";

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

   T1 shape (docs/42 §3): the strip holds EXACTLY ONE tab and there is no open/close
   entry yet — the tab's kind follows the connection's family (a redis connection browses
   keys, everything else browses tables), replaced by data-view's connection-switch path.
   The console (sqlOpen) and the activity flag stay connection-scoped here until T2 gives
   them kinds of their own. */
function freshConnState(): DbConnState {
  return {
    conns: [],            // rows from /api/db
    conn: null,           // selected connection (MCP name)
    tables: [],           // ONE page of the table list
    tablesTotal: 0, tablesPage: 0, tablesLimit: 200, more: false, grep: "",
    schemaFilter: "",     // pg only: "" = every schema; the /tables schema param (docs/22 W1.1)
    sort: "name", sortDir: "asc", // the list's sort key/dir — SQL sorts server-side, redis client-side
    gridCfg: { widths: {}, hidden: [] }, // per-connection column widths/hides (docs/22 W2.1), reloaded per page
    history: [],         // last-run console queries, newest first (per-browser, localStorage)
    favorites: [],       // starred queries, same store (docs/22 W5.4)
    redis: null,         // { keys, cursor, done, total } while a redis connection is selected
    redisType: "",      // SCAN TYPE filter — "" walks every type (string/hash/list/set/zset/stream)
    redisError: false,  // the last /keys fetch FAILED (docs/22 closeout B1) — the list must say so, not "no keys"
    sqlOpen: false, sqlText: "", sqlResult: null, sqlResults: null, resultTab: 0, sqlBusy: false,
                         // ^ transitional (docs/42 T1): the console is still a toggle over
                         //   the pane; T2 moves this block into the sql tab kind
    activity: false,     // the Activity section page is open (docs/22 W3.2) — SQL connections only; T2 retires the flag
    activityRows: null,  // last /activity answer, re-rendered by the 5s poll while open; T2 moves it into the activity tab kind
  };
}

/** A fresh tab of one kind, carrying ONLY that kind's fields (the T1 acceptance pins the
 *  shapes: a sql tab has no table field, a key tab no sqlText — the union stays honest
 *  from the first builder). Overloaded so a literal kind yields the literal member type;
 *  the final DbTabKind overload lets a computed union kind (the conn-family switch in
 *  dbResetTabForConn) build without a cast. */
export function freshTab(kind: "table"): DbTableTab;
export function freshTab(kind: "sql"): DbSqlTab;
export function freshTab(kind: "key"): DbKeyTab;
export function freshTab(kind: "activity"): DbActivityTab;
export function freshTab(kind: DbTabKind): DbTab;
export function freshTab(kind: DbTabKind): DbTab {
  // Each branch spells its literal kind: a shared { kind } base would widen the
  // discriminant to DbTabKind and the member object would not narrow to its arm.
  if (kind === "table") {
    return {
      kind: "table", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false,
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
    return { kind: "sql", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false,
      sqlText: "", sqlResult: null, sqlResults: null, resultTab: 0, sqlBusy: false };
  }
  if (kind === "key") {
    return { kind: "key", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false,
      redisKey: null, redisValue: null, redisEdits: null };
  }
  return { kind: "activity", loading: false, sel: {}, selAnchor: -1, focus: null, sqlPreview: false,
    activityRows: null };
}

let connRec: DbConnState = freshConnState();
let tabs: DbTab[] = [freshTab("table")];
let mounted = false;

/** The connection-scoped record: the sidebar's list, its filters, and what the console
 *  remembers. Shared by every open tab; reset when the connection changes. */
export function dbConn(): DbConnState { return connRec; }

/** The active tab's record. Never null: a fresh mount opens one empty table tab, so the
 *  view always has somewhere to paint. Narrow with `t.kind` before reading kind fields —
 *  the discriminant is the split's safety net. */
export function dbTab(): DbTab { return tabs[0]; }

/** Every open tab, in strip order. The active one is `dbTabs()[dbActiveIndex()]`. */
export function dbTabs(): DbTab[] { return tabs; }

/** The active tab's index into dbTabs(). */
export function dbActiveIndex(): number { return 0; }

/** Is the Data view mounted? The question the old `if (!state.db) return;` guards were really
 *  asking — an async continuation that crossed an unmount has nothing left to paint. */
export function dbIsMounted(): boolean { return mounted; }

/** Entering the view. */
export function mountDbView(): void { mounted = true; }

/** Leaving it: both records go back to the fresh literals, which is what nulling them used
 *  to buy — the rows, the buffered edits and the query results are dropped together. */
export function unmountDbView(): void {
  connRec = freshConnState();
  tabs = [freshTab("table")];
  mounted = false;
}

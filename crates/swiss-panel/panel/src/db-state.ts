/*
 * Copyright 2026 The swiss authors
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

import type { ApiDbActivityRow, ApiDbConnectionRow, ApiDbDataPage, ApiDbRedisKeyRow, ApiDbRedisValue, ApiDbTableDetail, ApiDbTableRow, DbQueryReply } from "./types/api.js";
import type { DbBufferedUpdate, DbFilterTerm, DbGridConfig, DbInsert, DbRedisEdits } from "./types/state.js";

/* The Data view owns its state (docs/37 R4, slice 6 of 7).

   This slice does NOT get one accessor pair per field, and that is deliberate: the record has
   47 of them, so the literal reading of M11 would mint 94 functions that only forward. The
   boundary that earns its keep here is a different one — every reader of this record already
   lives in the db domain (the fifteen data-*.ts modules and views/data.ts; nothing else in src
   ever touched state.db). So the module hands the domain ONE typed handle and keeps the
   lifecycle to itself.

   A leaf module, same reason as tunnel-state and job-state: data-view.ts is imported by every
   other data-* module, so hosting the record there closes a cycle. This file imports types only.

   What actually changes for callers: the record is never null. state.db was `DbState | null`,
   where null meant two different things at once — "no bag" and "the view is not mounted". Only
   the second is information, and it is now dbIsMounted(). That split is what retires the fifty
   `const d = state.db; const d_ = d!;` pairs docs/36 left behind, and with them the footgun the
   old dbFreshState comment had to warn about: there is no entry on which this can be null, so
   there is no second entry that dies halfway through renderDbView's wiring. */
export interface DbState {
  conns: ApiDbConnectionRow[];
  conn: string | null;
  tables: ApiDbTableRow[];
  tablesTotal: number;
  tablesPage: number;
  tablesLimit: number;
  more: boolean;
  grep: string;
  schemaFilter: string;
  sort: string;
  sortDir: string;
  table: string | null;
  schema: string | null;
  data: ApiDbDataPage | null;
  filters: DbFilterTerm[];
  pageSize: number;
  offset: number;
  order: string | null;
  dir: string;
  loading: boolean;
  gridCfg: DbGridConfig;
  sqlPreview: boolean;
  updates: Record<string, DbBufferedUpdate>;
  deletes: Record<string, Record<string, unknown>>;
  inserts: DbInsert[];
  sel: Record<string, boolean>;
  selAnchor: number;
  focus: { r: number; c: number } | null;
  sqlOpen: boolean;
  sqlText: string;
  sqlResult: DbQueryReply | null;
  sqlResults: DbQueryReply[] | null;
  sqlTab: number;
  sqlBusy: boolean;
  history: string[];
  favorites: string[];
  tab: string;
  formIdx: number;
  /* The SCAN cursor is the wire's string (redis cursors are big unsigned numbers the
     panel compares against "0" — never a JS number). */
  redis: { keys: ApiDbRedisKeyRow[]; cursor: string; done: boolean; total: number } | null;
  redisValue: ApiDbRedisValue | null;
  redisKey: string | null;
  redisEdits: DbRedisEdits | null;
  activity: boolean;
  activityRows: ApiDbActivityRow[] | null;
  redisType: string;
  redisError: boolean;
  detail: ApiDbTableDetail | null;
  detailBusy: boolean;
  /* The 409 observer's mark: which buffered row and columns lost a commit race. */
  conflict: { key: string; columns: string[] } | null;
}

function freshDbState(): DbState {
  return {
    conns: [],            // rows from /api/db
    conn: null,           // selected connection (MCP name)
    tables: [],           // ONE page of the table list
    tablesTotal: 0, tablesPage: 0, tablesLimit: 200, more: false, grep: "",
    schemaFilter: "",     // pg only: "" = every schema; the /tables schema param (docs/22 W1.1)
    sort: "name", sortDir: "asc", // the list's sort key/dir — SQL sorts server-side, redis client-side
    table: null, schema: null,
    data: null,           // last /api/db/:name/data page
    filters: [],          // [{ column, op, value }] — server-side WHERE terms (AND-ed)
    pageSize: 50, offset: 0, order: null, dir: "asc", loading: false,
    gridCfg: { widths: {}, hidden: [] }, // per-connection column widths/hides (docs/22 W2.1), reloaded per page
    sqlPreview: false,    // pending bar: show the SQL Commit will run
    updates: {},          // pkKey -> { pk, changes: { col: value-or-null } }
    deletes: {},          // pkKey -> pk object
    inserts: [],          // [{ values: { col: value-or-null } }]
    sel: {},             // rowKey -> true — checked rows the next Copy pulls (grid or query result)
    selAnchor: -1,       // visible row index of the last checkbox click (Shift range start)
    focus: null,         // {r, c} — the keyboard's focus cell, inserts-first grid rows (docs/22 W2.2)
    sqlOpen: false, sqlText: "", sqlResult: null, sqlResults: null, sqlTab: 0, sqlBusy: false,
                         // ^ W4.3: sqlResult is the ACTIVE tab's reply; sqlResults holds every
                         // statement's reply and sqlTab which one is showing
    history: [],         // last-run console queries, newest first (per-browser, localStorage)
    favorites: [],       // starred queries, same store (docs/22 W5.4)
    tab: "data",        // data | form | columns | indexes | ddl | fks — Form is the data page's own second view (W5.1)
    formIdx: 0,         // the Form tab's record — grid row space (inserts first), shared with the keyboard focus
    redis: null,         // { keys, cursor, done, total } while a redis connection is selected
    redisValue: null,    // the shown key's decoded value (docs/22 W3.3)
    redisKey: null,      // the key whose value is shown in the pane
    redisEdits: null,    // buffered typed-value edits for that key (docs/22 W3.3)
    activity: false,     // the Activity section page is open (docs/22 W3.2) — SQL connections only
    activityRows: null,  // last /activity answer, re-rendered by the 5s poll while open
    redisType: "",      // SCAN TYPE filter — "" walks every type (string/hash/list/set/zset/stream)
    redisError: false,  // the last /keys fetch FAILED (docs/22 closeout B1) — the list must say so, not "no keys"
    detail: null,       // last /api/db/:name/schema answer (BrowseTableDetail)
    detailBusy: false,
    conflict: null,      // the fetch observer's 409 mark; cleared when the row is re-edited
  };
}

let db: DbState = freshDbState();
let mounted = false;

/** The Data view's record. Never null: between unmount and the next mount it is simply the
 *  fresh literal, so a debounce or a poll that outlives the view reads empties instead of
 *  throwing. Whether the view is actually on screen is dbIsMounted(), and that is what such a
 *  late callback should be asking. */
export function dbView(): DbState { return db; }

/** Is the Data view mounted? The question the old `if (!state.db) return;` guards were really
 *  asking — an async continuation that crossed an unmount has nothing left to paint. */
export function dbIsMounted(): boolean { return mounted; }

/** Entering the view. */
export function mountDbView(): void { mounted = true; }

/** Leaving it: the whole record goes back to the fresh literal, which is what nulling it used
 *  to buy — the rows, the buffered edits and the query results are dropped together. */
export function unmountDbView(): void {
  db = freshDbState();
  mounted = false;
}

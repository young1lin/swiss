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

import { $, apiJson, dbReqGuard, el, icon, state } from "./util.js";
import { currentPageCount } from "./page-registry.js";
import { dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRedisPendingCount } from "./data-browsers.js";
import { dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbFavLoad, dbFavPush, dbFormatSql, dbHistoryLoad, dbHistoryRender, dbRunSql, renderDbBar } from "./data-sql.js";
import { dbActivityPane, dbActivityPollStop } from "./data-activity.js";
// dbLoadDetail (the Structure tabs' loader) is needed at the table-open path now: the FK
// jump (docs/22 W5.2) reads the detail's foreignKeys. The cycle data-view <-> data-structure
// is the same accepted shape as data-grid <-> data-cell — both sides only call across it
// inside functions, never at module scope.
import { dbLoadDetail } from "./data-structure.js";
import { openDbDdlSheet } from "./data-ddl.js";
import { dbSuggestHide, dbSuggestKeys, dbSuggestOnInput } from "./data-suggest.js";
import { popupMenu } from "./menu.js";

/* ================================================================================================
   Data view — a DBeaver-style browser over the mysql/pg MCPs.
   Left: a lazy, greppable, paged table list (never the whole instance at once). Right: one
   bounded page of rows (10/20/50/100/200/500, default 500) with click-to-sort headers.
   Edits are buffered client-side — an amber bar counts them — and nothing reaches the database
   until Commit posts the whole buffer as ONE transaction; Discard drops it without a query.
   Same rendering contract as everywhere else: the 6s poll never rebuilds this view.
   ================================================================================================ */

var DB_PAGE_SIZES = [10, 20, 50, 100, 200, 500];
var DB_HISTORY_KEY = "mcp_gateway_db_sql_history";
var DB_HISTORY_MAX = 50;

/* The view's whole state. Built by a FACTORY, not a module-top literal: leaving the view
   nulls state.db (views/data.js unmount frees it), and a dynamic import evaluates this
   module exactly once — a top-level literal would leave state.db null on every entry but
   the first, crashing renderDbView halfway through its wiring and leaving the SQL console
   and cell editing dead. Every entry rebuilds what unmount freed. */
function dbFreshState() {
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
    tab: "data",        // data | form | columns | indexes | ddl | fks — Form is the data page's own second view (W5.1)
    formIdx: 0,         // the Form tab's record — grid row space (inserts first), shared with the keyboard focus
    redis: null,         // { keys, cursor, done, total } while a redis connection is selected
    redisKey: null,      // the key whose value is shown in the pane
    redisEdits: null,    // buffered typed-value edits for that key (docs/22 W3.3)
    activity: false,     // the Activity section page is open (docs/22 W3.2) — SQL connections only
    activityRows: null,  // last /activity answer, re-rendered by the 5s poll while open
    redisType: "",      // SCAN TYPE filter — "" walks every type (string/hash/list/set/zset/stream)
    redisError: false,  // the last /keys fetch FAILED (docs/22 closeout B1) — the list must say so, not "no keys"
    detail: null,       // last /api/db/:name/schema answer (BrowseTableDetail)
    detailBusy: false,
  };
}
state.db = dbFreshState();

function dbPending(): number {
  var d = state.db;
  if (!d) return 0;  // between unmount and the next mount there is nothing buffered
  return Object.keys(d!.updates).length + Object.keys(d!.deletes).length + d!.inserts.length;
}

/** Ask before an action would drop buffered edits; false when the user said no. The redis
 *  value buffer counts too (docs/22 W3.3) — a key switch that ate buffered fields silently
 *  would break the same rule the row grid's guard exists for. */
function dbOkToDrop(): boolean {
  var n = dbPending() + dbRedisPendingCount();
  return !n || confirm("Discard " + n + " uncommitted change" + (n > 1 ? "s" : "") + "? Nothing has been written yet.");
}

function dbDropEdits() {
  var d = state.db;
  d!.updates = {}; d!.deletes = {}; d!.inserts = [];
  dbClearSel(); // selection addresses rows of the OLD page — it never survives a reload
  d!.sqlPreview = false;
}

/** Row selection is scoped to what is on screen: cleared whenever the grid identity changes. */
function dbClearSel(): void {
  var d = state.db;
  d!.sel = {};
  d!.selAnchor = -1;
}

function dbPkKey(pkCols: string[], row: Record<string, unknown>): string {
  return JSON.stringify(pkCols.map(function (c: string): unknown { return row[c]; }));
}

function dbPkVals(pkCols: string[], row: Record<string, unknown>): Record<string, unknown> {
  var out: Record<string, unknown> = {};
  pkCols.forEach(function (c: string): void { out[c] = row[c]; });
  return out;
}

/** docs/22 W4.3: a query-result row's selection key — the tab index namespaces it, so every
 *  result tab owns an independent checked-row set the same way the table grid's keys live
 *  apart from these under the "q" prefix. Pure. */
function dbResultKey(tab: number, i: number): string {
  return "q" + tab + ":" + i;
}

/* --- view skeleton ------------------------------------------------------------------------------- */

async function loadDbView(): Promise<void> {
  // The entry choke point: unmount freed state.db, and nothing else rebuilds it — the
  // module-top assignment ran exactly once, on first import. Without this every second
  // entry died reading state.db.grep halfway through renderDbView's wiring.
  if (!state.db) state.db = dbFreshState();
  renderDbView();
  var j = await apiJson<{ connections: ApiDbConnectionRow[] }>("/api/db");
  if (!j) return;
  var d = state.db;
  d!.conns = j.connections || [];
  var stillThere = d!.conns.some(function (c: ApiDbConnectionRow): boolean { return c.name === d!.conn; });
  if (!stillThere) {
    d!.conn = d!.conns.length ? d!.conns[0].name : null;
    d!.table = null; d!.schema = null; d!.data = null; d!.tables = [];
    d!.redis = null; d!.redisKey = null; d!.redisValue = null;
    d!.redisEdits = null;
    d!.sqlResult = null;
    d!.sqlResults = null; // docs/22 W4.3: the tab strip goes with the result it named
    d!.sqlTab = 0;
    dbDropEdits();
  }
  renderDbSide();
  // The skeleton above was drawn before /api/db answered, i.e. with no connection: its hint said
  // "No database MCP registered" and its console wore the SQL placeholder. With the connection
  // known, dress everything that depends on its KIND (redis / SQL) — a redis connection
  // entered first used to keep the SQL console and that hint until something else redrew them.
  dbSyncKind();
  renderDbToolbar(); renderDbFilters(); renderDbGrid();
  // A refresh re-fetches what is MISSING, not what is already on screen — reloading keys would
  // throw away the pages the user paged in with "More".
  if (d!.conn && dbIsRedis()) { if (!d!.redis) dbLoadKeys(true); else renderDbTables(); }
  else if (d!.conn) { if (!d!.tables.length) dbLoadTables(); else renderDbTables(); }
  else renderDbTables();
  if (d!.table && !d!.data && !dbIsRedis()) dbLoadData(true);
}

function renderDbView(): void {
  var pane = $("pane");
  // db-host turns the pane into a full-bleed workspace body (layout "workspace", docs/13
  // D5 as revised): no pane padding, no measure — the explorer divides its own space.
  // views/data.js takes the class back off on unmount so no other page inherits it.
  pane.classList.add("db-host");
  pane.innerHTML = "";
  var root = el("div", "db-root");
  root.innerHTML =
    '<div class="db-side">' +
      '<select id="dbConn" aria-label="Connection"></select>' +
      '<select id="dbSchema" aria-label="Schema" hidden></select>' +
      '<input id="dbGrep" type="search" placeholder="Filter tables" aria-label="Filter tables">' +
      '<div class="db-sortrow">' +
        '<select id="dbSort" aria-label="Sort by"></select>' +
        '<button class="btn icon" id="dbSortDir" type="button" title="Sort direction"></button>' +
      '</div>' +
      // docs/22 W4.6: the table list's own header band — the list names itself and carries
      // one persistent dimmed + (the docs/20 §4 container-header glyph) for New table….
      '<div class="db-list-head" id="dbListHead" hidden>' +
        '<span class="db-list-title">Tables</span>' +
        '<button class="btn icon grp-add" id="dbNewTable" type="button" aria-label="New table" title="New table…"></button>' +
      '</div>' +
      '<div class="db-tables" id="dbTables"><div class="db-hint">Loading…</div></div>' +
      '<div class="db-side-foot" id="dbTablesPager"></div>' +
    '</div>' +
    '<div class="db-main">' +
      '<div class="db-headrow">' +
        '<div class="db-head" id="dbHead"></div>' +
        // Pane-level actions live behind one ⋯ next to the head (docs/22 W3.2); the Activity
        // monitor is the first. Hidden until a SQL connection exists — redis has no sessions.
        '<button class="btn icon" id="dbMore" type="button" title="More pane actions" hidden></button>' +
      '</div>' +
      '<div class="db-filters" id="dbFilters"></div>' +
      '<div class="db-console" id="dbConsole" hidden>' +
        '<div class="db-sql-wrap">' +
          '<pre class="db-sql-hl db-sql-face" id="dbSqlHl" aria-hidden="true"></pre>' +
          '<textarea class="db-sql-face" id="dbSql" placeholder="SELECT / UPDATE / DELETE … — statements split on ;" spellcheck="false"></textarea>' +
        '</div>' +
        '<div class="db-console-row"><button class="btn" id="dbSqlRun">Run</button>' +
        '<button class="btn" id="dbSqlExplain">Explain</button>' +
        '<button class="btn" id="dbSqlFormat">Format</button>' +
        '<select id="dbSqlHistory" title="Query history"><option value="">History</option></select>' +
        '<button class="btn icon" id="dbSqlFav" type="button" aria-label="Save to favorites"></button>' +
        '<span class="hint" id="dbSqlHint">statements split on ; · Ctrl+Enter runs</span></div>' +
      '</div>' +
      '<div class="db-grid-wrap" id="dbGridWrap"></div>' +
      '<div class="db-bar" id="dbBar" hidden></div>' +
    '</div>';
  pane.appendChild(root);
  $<HTMLSelectElement>("dbConn").onchange = (e) => {
    const t = e.currentTarget as HTMLSelectElement;
    if (t.value === state.db!.conn) return;
    if (!dbOkToDrop()) { t.value = state.db!.conn || ""; return; }
    // The Activity page belongs to ONE connection's server; the switch leaves it behind —
    // restore the normal pane (and stop its poll) before the state it reads changes.
    if (state.db!.activity) dbActivityClose();
    var d = state.db;
    d!.conn = t.value; d!.table = null; d!.schema = null; d!.data = null;
    d!.tables = []; d!.tablesPage = 0; d!.order = null; d!.sqlResult = null;
    d!.sqlResults = null; d!.sqlTab = 0; // docs/22 W4.3: no stale tabs across a connection switch
    d!.schemaFilter = ""; // a schema pick was made against the other connection's catalog
    d!.redis = null; d!.redisKey = null; d!.redisValue = null;
    d!.redisEdits = null;
    d!.filters = [];
    d!.sort = "name"; d!.sortDir = "asc"; // the new connection's kind may not have the chosen key
    // A sidebar search is table-list-scoped: carrying "tsys_" from one connection into the next
    // silently filters the new list down to nothing. Reset it and the box that shows it.
    d!.grep = "";
    d!.redisType = ""; // same reasoning: a type filter is chosen against a key list, not inherited
    var gb = $<HTMLInputElement>("dbGrep");
    if (gb) gb.value = "";
    dbDropEdits();
    dbSyncKind();
    renderDbTables(); renderDbToolbar(); renderDbFilters(); renderDbGrid(); renderDbBar();
    // The page-bar count chip names the connection (views/data.js countText) and is otherwise
    // written only on navigation — the switch has to rewrite it here or the bar keeps naming
    // the connection this view just left behind.
    var chip = $("countChip");
    if (chip) chip.textContent = currentPageCount();
    if (dbIsRedis()) dbLoadKeys(true);
    else dbLoadTables();
  };
  // The view is REBUILT on every entry, but d.grep persists for the same table — seed the box
  // from state, or the list stays filtered by a term the (fresh, empty) input no longer shows.
  $<HTMLInputElement>("dbGrep").value = state.db!.grep || "";
  var t: ReturnType<typeof setTimeout> | undefined;
  $<HTMLInputElement>("dbGrep").oninput = (e) => {
    var v = (e.currentTarget as HTMLInputElement).value;
    clearTimeout(t);
    t = setTimeout(function (): void {
      // Leaving the view frees state.db (views/data.js unmount); a debounce pending across
      // that boundary once threw here. The view is gone — the filter belongs to no one.
      if (!state.db) return;
      state.db!.grep = v; state.db!.tablesPage = 0;
      if (dbIsRedis()) dbLoadKeys(true);
      else dbLoadTables();
    }, 300);
  };
  $<HTMLSelectElement>("dbSort").onchange = (e) => {
    var d = state.db;
    d!.sort = (e.currentTarget as HTMLSelectElement).value;
    d!.tablesPage = 0;
    if (dbIsRedis()) renderDbTables(); // keys sort in place over what has been scanned
    else dbLoadTables();
  };
  $("dbSortDir").onclick = function (): void {
    var d = state.db;
    d!.sortDir = d!.sortDir === "asc" ? "desc" : "asc";
    dbPaintSort();
    d!.tablesPage = 0;
    if (dbIsRedis()) renderDbTables();
    else dbLoadTables();
  };
  // The schema picker (pg only): picking one re-requests the table list inside that schema.
  // The wiring itself lives in dbWireSchemaSelect so a RE-CREATED picker (dbPaintSchemaOptions
  // removes the select for non-pg connections and brings it back for pg) gets it too.
  var schemaSel = $<HTMLSelectElement>("dbSchema");
  if (schemaSel) dbWireSchemaSelect(schemaSel);
  $<HTMLTextAreaElement>("dbSql").value = state.db!.sqlText;
  $<HTMLTextAreaElement>("dbSql").oninput = (e) => { const t = e.currentTarget as HTMLTextAreaElement; state.db!.sqlText = t.value; dbSqlPaint(); dbSuggestOnInput.call(t); };
  $<HTMLTextAreaElement>("dbSql").onscroll = (e) => {
    const t = e.currentTarget as HTMLTextAreaElement;
    var hl = $("dbSqlHl");
    if (hl) { hl.scrollTop = t.scrollTop; hl.scrollLeft = t.scrollLeft; }
    dbSuggestHide(); // the caret's point scrolled with the text; a list pinned to stale
    // coordinates would point at the wrong word. The next keystroke reopens it in place.
  };
  $<HTMLTextAreaElement>("dbSql").addEventListener("blur", dbSuggestHide);
  // The suggest hook runs beside the Run shortcut: it only ever consumes the keys the open
  // list owns (arrows / Tab / Enter / Esc) and leaves Ctrl+Enter to run the block.
  $<HTMLTextAreaElement>("dbSql").addEventListener("keydown", dbSuggestKeys);
  $<HTMLTextAreaElement>("dbSql").onkeydown = function (e: KeyboardEvent): void {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); dbRunSql(); }
  };
  dbSqlPaint();
  dbSyncKind();
  // wrapped: onclick hands the handler the click EVENT, and dbRunSql's first parameter is
  // `explain` — an event object is truthy, so a plain Run has been quietly running EXPLAIN.
  $<HTMLButtonElement>("dbSqlRun").onclick = function (): void { dbRunSql(false); };
  $<HTMLButtonElement>("dbSqlExplain").onclick = (ev: MouseEvent): void => {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without it the click that opens the menu also tears it down (same as the Export menu).
    ev.stopPropagation();
    popupMenu((ev.currentTarget as HTMLButtonElement).getBoundingClientRect(), [
      { label: "Explain", fn: function (): void { dbRunSql("plan"); } },
      { label: "Explain ANALYZE", fn: function (): void { dbRunSql("analyze"); } },
    ]);
  };
  $<HTMLSelectElement>("dbSqlHistory").onchange = (e) => {
    const t = e.currentTarget as HTMLSelectElement;
    if (t.value === "") return;
    // docs/22 W5.4: "f"+i is a favorite, a plain index history — both land in the console.
    var fav = t.value.charAt(0) === "f";
    var sql = fav ? state.db!.favorites![Number(t.value.slice(1))]
      : state.db!.history![Number(t.value)];
    t.value = ""; // back to the label, so the same entry can be picked again
    if (sql == null) return;
    state.db!.sqlText = sql;
    var ta = $<HTMLTextAreaElement>("dbSql");
    if (ta) { ta.value = sql; dbSqlPaint(); ta.focus(); }
  };
  // docs/22 W5.4: the star saves the console text to the favorites group; Format re-indents
  // it in place (whitespace only — the run result cannot change).
  var favBtn = $<HTMLButtonElement>("dbSqlFav");
  if (favBtn) {
    favBtn.innerHTML = icon("star");
    favBtn.title = "Save the console text to favorites";
    favBtn.onclick = function (): void { dbFavPush(state.db!.sqlText); };
  }
  var fmtBtn = $<HTMLButtonElement>("dbSqlFormat");
  if (fmtBtn) {
    fmtBtn.onclick = function (): void {
      var d = state.db;
      if (!d!.sqlText || !d!.sqlText.trim()) return;
      d!.sqlText = dbFormatSql(d!.sqlText);
      var ta = $<HTMLTextAreaElement>("dbSql");
      if (ta) { ta.value = d!.sqlText; dbSqlPaint(); ta.focus(); }
    };
  }
  dbHistoryLoad();
  dbFavLoad();
  dbHistoryRender();
  renderDbToolbar(); renderDbGrid(); renderDbBar();
  // docs/22 W4.6: the list band's + opens the New table sheet — SQL connections only (the
  // band itself is hidden for redis and for no-connection in dbSyncKind, with dbMore).
  var dbNewTable = $<HTMLButtonElement>("dbNewTable");
  if (dbNewTable) {
    dbNewTable.innerHTML = icon("plus");
    dbNewTable.onclick = function (): void {
      var d = state.db;
      if (!d!.conn || dbIsRedis()) return;
      openDbDdlSheet("table", {
        dialect: dbDialectOf(),
        conn: d!.conn,
        schema: dbIsPg() ? (d!.schemaFilter || "public") : "",
        schemas: dbIsPg() ? dbKnownSchemas() : [],
      });
    };
  }
  var dbMore = $<HTMLButtonElement>("dbMore");
  dbMore.innerHTML = icon("ellipsis");
  dbMore.onclick = (e: MouseEvent): void => {
    // stopPropagation: the document click closes popup menus — the opening click must not.
    e.stopPropagation();
    var d = state.db;
    popupMenu((e.currentTarget as HTMLButtonElement).getBoundingClientRect(), [
      { label: d!.activity ? "Close activity" : "Activity…", fn: dbActivityToggle },
    ]);
  };
  // The section page replaces the pane's content when (re)entered with it open — closing is
  // just another rebuild with the flag down, so nothing here needs a special restore path.
  if (state.db!.activity && state.db!.conn && !dbIsRedis()) dbActivityPane(dbActivityClose);
}

/* The Activity monitor (docs/22 W3.2): one flag drives it. Opening rebuilds the pane with the
   section in place; closing rebuilds it back — the same renderDbView that drew it. */
function dbActivityToggle(): void {
  var d = state.db;
  if (d!.activity) { dbActivityClose(); return; }
  if (!d!.conn || dbIsRedis()) return;
  d!.activity = true;
  renderDbView();
}

function dbActivityClose(): void {
  var d = state.db;
  d!.activity = false;
  d!.activityRows = null;
  dbActivityPollStop();
  renderDbView();
}

/* The list's sort row. SQL connections sort server-side (name/rows/size against the catalog);
 * redis sorts client-side over the keys SCAN has already handed over (key/TTL). One control,
 * one direction button; the option set repaints when the connection kind changes. */
function dbSortOptions(): { v: string; t: string }[] {
  return dbIsRedis()
    ? [{ v: "name", t: "Key" }, { v: "ttl", t: "TTL" }]
    : [{ v: "name", t: "Name" }, { v: "rows", t: "Rows" }, { v: "size", t: "Size" }];
}

function dbPaintSort(): void {
  var d = state.db;
  var sel = $<HTMLSelectElement>("dbSort"), dir = $("dbSortDir");
  if (!sel || !dir) return;
  var opts = dbSortOptions();
  if (!opts.some(function (o: { v: string; t: string }): boolean { return o.v === d!.sort; })) d!.sort = opts[0].v; // kind switched
  sel.innerHTML = "";
  opts.forEach(function (o: { v: string; t: string }): void {
    var op = el("option", "", o.t) as HTMLOptionElement;
    op.value = o.v;
    op.selected = o.v === d!.sort;
    sel.appendChild(op);
  });
  dir.textContent = d!.sortDir === "asc" ? "\u2191" : "\u2193";
  dir.setAttribute("aria-label", d!.sortDir === "asc" ? "Sort ascending" : "Sort descending");
}

/** Redis keys arrive in SCAN (hash-slot) order; the list sorts what has been loaded. Key names
 *  compare naturally — numeric runs by value, case folded (t2 < t10, foo == FOO) — because the
 *  raw byte order is exactly the ASCII sort nobody asked for. TTL puts expiring keys first; no
 *  expiry (-1) reads as forever. */
function dbRedisCompare(a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow): number {
  var d = state.db;
  if (d!.sort === "ttl") {
    var ta: number = a.ttl! < 0 ? Infinity : a.ttl!;
    var tb: number = b.ttl! < 0 ? Infinity : b.ttl!;
    if (ta !== tb) return d!.sortDir === "asc" ? ta - tb : tb - ta;
  }
  var c = String(a.key).localeCompare(String(b.key), undefined, { numeric: true, sensitivity: "base" });
  return d!.sortDir === "asc" ? c : -c;
}

/* Everything in the skeleton that reads differently per connection KIND: the sidebar filter's
   placeholder, the console's placeholder and hint, and whether Explain exists (EXPLAIN is SQL; a
   redis command has no plan). Called on mount once the connections are known, and again on every
   switch — it used to run once, at mount, before /api/db had even answered, so it never saw a
   redis connection. */
function dbSyncKind(): void {
  var grep = $<HTMLInputElement>("dbGrep"), sql = $<HTMLTextAreaElement>("dbSql"), explain = $("dbSqlExplain"), hint = $("dbSqlHint");
  if (!grep || !sql || !explain || !hint) return;
  var fmt = $("dbSqlFormat");
  dbPaintSort();
  if (dbIsRedis()) {
    grep.placeholder = "Filter keys"; grep.setAttribute("aria-label", "Filter keys");
    grep.title = "Filter keys (a SCAN MATCH pattern)";
    sql.placeholder = "SET k v · GET k · DEL k · HGETALL h · TTL k — one command per run";
    explain.hidden = true;
    if (fmt) fmt.hidden = true; // docs/22 W5.4: SQL formatting has nothing to say about a command
    hint.textContent = "writes run · KEYS is refused, use the key list · Ctrl+Enter runs";
  } else {
    // The placeholder IS the grammar (docs/22 W1.6): comma AND, | OR, * wildcard.
    grep.placeholder = "a*, b|c"; grep.setAttribute("aria-label", "Filter tables");
    grep.title = "Filter tables: comma-separated terms AND together, | is OR, * is a wildcard";
    sql.placeholder = "SELECT / UPDATE / DELETE … — statements split on ;";
    explain.hidden = false;
    if (fmt) fmt.hidden = false;
    // docs/22 W4.3: the ; split answers one result tab per statement; the blank-line block
    // rule (W1.8) still decides what a single Run covers.
    hint.textContent = "a blank line starts a new block · Ctrl+Enter runs the caret's block · ; splits it into one result tab per statement";
  }
  // The pane's ⋯ exists for the Activity page, which is a SQL-connection feature: a redis
  // connection (or none) hides the button rather than the menu hiding its one item. The
  // list band rides the same condition — redis keys are not tables, and there is nothing to
  // create without a connection.
  var more = $("dbMore");
  if (more) more.hidden = !state.db!.conn || dbIsRedis();
  var listHead = $("dbListHead");
  if (listHead) listHead.hidden = !state.db!.conn || dbIsRedis();
}

/** The one label a connection goes by — the sidebar dropdown's option text. The page-bar
 *  count chip (views/data.js countText) shows the SAME string, so the bar and the control
 *  that changes it can never disagree; two builders is how the bar ended up saying "Data"
 *  twice while the dropdown said "shop-redis · redis". */
function dbConnLabel(c: ApiDbConnectionRow): string {
  return c.name + " · " + c.dialect;
}

function renderDbSide(): void {
  var d = state.db;
  var sel = $<HTMLSelectElement>("dbConn");
  if (!sel) return;
  sel.innerHTML = "";
  if (!d!.conns.length) {
    var none = el("option", "", "No database MCPs") as HTMLOptionElement;
    none.value = "";
    sel.appendChild(none);
    sel.disabled = true;
    return;
  }
  sel.disabled = false;
  // docs/20 G5: each row carries the group its connection lists under. With more than one,
  // the options fold into one optgroup per group — groups in first-appearance order over the
  // flat list, members in the list's own order inside. A single group stays flat: an
  // optgroup around everything is noise that says nothing. An older gateway answers no
  // group at all, which reads as the one flat list it always drew.
  var order: string[] = [];
  var buckets: Record<string, ApiDbConnectionRow[]> = {};
  d!.conns.forEach(function (c: ApiDbConnectionRow): void {
    var g = c.group || "default";
    if (!buckets[g]) { buckets[g] = []; order.push(g); }
    buckets[g].push(c);
  });
  var addOption = function (parent: HTMLElement, c: ApiDbConnectionRow): void {
    var o = el("option", "", dbConnLabel(c)) as HTMLOptionElement;
    o.value = c.name;
    o.selected = c.name === d!.conn;
    parent.appendChild(o);
  };
  if (order.length < 2) {
    d!.conns.forEach(function (c: ApiDbConnectionRow): void { addOption(sel, c); });
    return;
  }
  order.forEach(function (g: string): void {
    var og = el("optgroup") as HTMLOptGroupElement;
    og.label = g;
    buckets[g].forEach(function (c: ApiDbConnectionRow): void { addOption(og, c); });
    sel.appendChild(og);
  });
}

/* --- lazy table list ---------------------------------------------------------------------------- */

// One /tables request chain: a slow answer for a page or filter the user just left must be
// dropped, or it would repopulate the sidebar with the stale page (docs/22 closeout audit).
var dbTablesReq = dbReqGuard();

async function dbLoadTables(): Promise<void> {
  var d = state.db;
  if (!d!.conn) return;
  var box = $("dbTables");
  if (box) { box.innerHTML = ""; box.appendChild(el("div", "db-hint", "Loading…")); }
  var q = "/api/db/" + encodeURIComponent(d!.conn) + "/tables?page=" + d!.tablesPage;
  if (d!.grep) q += "&grep=" + encodeURIComponent(d!.grep);
  if (d!.schemaFilter) q += "&schema=" + encodeURIComponent(d!.schemaFilter);
  if (d!.sort) q += "&sort=" + encodeURIComponent(d!.sort) + "&dir=" + encodeURIComponent(d!.sortDir || "asc");
  var token = dbTablesReq.issue();
  var j = await apiJson<ApiDbTablesResponse>(q);
  if (!dbTablesReq.accepts(token)) return; // superseded: a newer page/filter owns the list
  if (!j) { if ($("dbTables")) $("dbTables").innerHTML = ""; return; }
  d!.tables = j.tables || [];
  d!.tablesTotal = j.total || 0;
  d!.tablesLimit = j.limit || 200;
  d!.more = !!j.more;
  renderDbTables();
}

/** A Postgres connection (many schemas) vs everything else. The schema grouping and the
 *  schema picker exist only for it; MySQL is one database and redis has no schemas at all. */
function dbIsPg(): boolean {
  var c = state.db!.conns.find(function (x: ApiDbConnectionRow): boolean { return x.name === state.db!.conn; });
  return !!c && c.dialect === "pg";
}

/* The sidebar grep grammar (docs/22 W1.6): comma-separated terms AND together, "|" inside a
   term is OR, "*" is a wildcard, everything case-insensitive. A bare term with no "*" keeps
   the substring behaviour the list always had. Pure so the grammar can be pinned without a
   DOM; the server's table grep speaks the same language in SQL. */
function dbFilterMatches(tokens: string, name: unknown): boolean {
  var n = String(name).toLowerCase();
  var terms = String(tokens).toLowerCase().split(",")
    .map(function (s: string): string { return s.trim(); }).filter(Boolean);
  if (!terms.length) return true;
  return terms.every(function (term: string): boolean {
    return term.split("|").some(function (alt: string): boolean {
      alt = alt.trim();
      if (!alt) return false;
      if (alt.indexOf("*") < 0) return n.indexOf(alt) >= 0;
      var re = "^" + alt.split("*").map(function (p: string): string {
        return p.replace(/[.+?^\[\]{}()\\\-|]/g, "\\$&");
      }).join(".*") + "$";
      return new RegExp(re).test(n);
    });
  });
}

function renderDbTables(): void {
  var d = state.db;
  var box = $("dbTables");
  if (!box) return;
  box.innerHTML = "";
  dbPaintSchemaOptions();
  if (!d!.conn) {
    box.appendChild(el("div", "db-hint", "Add a mysql or pg MCP, then browse it here."));
    return;
  }
  if (dbIsRedis()) {
    var rr = d!.redis;
    if (!rr || !rr.keys.length) {
      // docs/22 closeout B1: a failed scan is a FAILURE, not an empty keyspace — the toast
      // carries the server's own text; this row keeps the list from pretending otherwise.
      box.appendChild(el("div", "db-hint", d!.redisError
        ? "Scan failed — the toast carries the server's error; this list is the last good page."
        : d!.grep ? 'No keys match "' + d!.grep + '"' : "No keys yet — scan returned none."));
    }
    var sortedKeys = (rr ? rr.keys : []).slice().sort(dbRedisCompare);
    sortedKeys.forEach(function (k: ApiDbRedisKeyRow): void {
      var b = el("button", "db-table" + (k.key === d!.redisKey ? " sel" : ""));
      b.title = k.key; // the row truncates with an ellipsis; the full key is one hover away
      b.appendChild(el("div", "db-table-name", k.key));
      var meta = k.type;
      if (k.ttl! >= 0) meta += " · ttl " + k.ttl + "s";
      b.appendChild(el("div", "db-table-meta", meta));
      // Switching keys drops the typed-value buffer (docs/22 W3.3): a different key cannot
      // adopt another key's fields, so the same guard the table switch uses asks first.
      b.onclick = function (): void {
        if (k.key === d!.redisKey || dbOkToDrop()) dbLoadRedisValue(k.key);
      };
      box.appendChild(b);
    });
    var foot2 = $("dbTablesPager");
    if (foot2) {
      foot2.innerHTML = "";
      var shown = rr ? rr.keys.length : 0;
      foot2.appendChild(el("span", "", shown.toLocaleString() + (rr && rr.total != null ? " of " + Number(rr.total).toLocaleString() + " keys" : " keys")));
      if (rr && !rr.done) {
        var more = el("button", "btn", "More");
        more.title = "Continue the SCAN";
        more.onclick = function (): void { dbLoadKeys(false); };
        foot2.appendChild(more);
      }
    }
    return;
  }
  if (!d!.tables.length) {
    box.appendChild(el("div", "db-hint", d!.grep ? 'No tables match "' + d!.grep + '".' : "No tables."));
  }
  // docs/22 W1.1: a Postgres catalog is many schemas, so the page renders grouped — the docs/20
  // §4 container vocabulary (band header, mixed-case name, tnum count, indented body behind the
  // guide line). MySQL is one database: the flat list, no headers, visually exactly as before.
  if (dbIsPg() && d!.tables.length) {
    var schemas: string[] = [];
    d!.tables.forEach(function (t: ApiDbTableRow): void {
      if (schemas.indexOf(t.schema) < 0) schemas.push(t.schema);
    });
    schemas.forEach(function (s: string): void {
      var rows = d!.tables.filter(function (t: ApiDbTableRow): boolean { return t.schema === s; });
      var g = el("div", "grp grp--side");
      var head = el("div", "grp-head");
      head.appendChild(el("span", "grp-toggle", s));
      head.appendChild(el("span", "grp-n", String(rows.length)));
      g.appendChild(head);
      var body = el("div", "grp-body");
      rows.forEach(function (t: ApiDbTableRow): void { body.appendChild(dbTableRow(t)); });
      g.appendChild(body);
      box.appendChild(g);
    });
  } else {
    d!.tables.forEach(function (t: ApiDbTableRow): void { box.appendChild(dbTableRow(t)); });
  }
  var foot = $("dbTablesPager");
  if (!foot) return;
  foot.innerHTML = "";
  var from = d!.tablesTotal ? d!.tablesPage * d!.tablesLimit + 1 : 0;
  var to = d!.tablesPage * d!.tablesLimit + d!.tables.length;
  foot.appendChild(el("span", "", from.toLocaleString() + "–" + to.toLocaleString() + " of " + d!.tablesTotal.toLocaleString()));
  var prev = el("button", "btn icon") as HTMLButtonElement;
  prev.innerHTML = icon("chevron-left");
  prev.title = "Previous page of tables";
  prev.disabled = d!.tablesPage === 0;
  prev.onclick = function (): void { d!.tablesPage--; dbLoadTables(); };
  var next = el("button", "btn icon") as HTMLButtonElement;
  next.innerHTML = icon("chevron-right");
  next.title = "Next page of tables";
  next.disabled = !d!.more;
  next.onclick = function (): void { d!.tablesPage++; dbLoadTables(); };
  foot.appendChild(prev);
  foot.appendChild(next);
}

/** One table row, shared by the flat list and every schema group. */
function dbTableRow(t: ApiDbTableRow): HTMLElement {
  var d = state.db;
  var b = el("button", "db-table" + (t.name === d!.table && t.schema === d!.schema ? " sel" : ""));
  b.title = t.name;
  b.appendChild(el("div", "db-table-name", t.name));
  b.appendChild(el("div", "db-table-meta",
    t.type + (t.approxRows != null ? " · ~" + Number(t.approxRows).toLocaleString() + " rows" : "") +
    (t.size ? " · " + t.size : "")));
  b.onclick = function (): void { dbOpenTable(t); };
  return b;
}

/** The schema picker's one behavior: a pick re-requests the table list inside that schema. */
function dbWireSchemaSelect(sel: HTMLSelectElement): void {
  sel.onchange = (e) => {
    const t = e.currentTarget as HTMLSelectElement;
    var d = state.db;
    if (t.value === d!.schemaFilter) return;
    d!.schemaFilter = t.value;
    d!.tablesPage = 0;
    dbLoadTables();
  };
}

/** The schema picker above the grep box (pg only): "All schemas" plus one option per schema
 *  on the current page, counts from the page the list is showing. A schema picked on an earlier
 *  page stays selectable even when this page does not carry it.
 *  docs/22 closeout B7: a non-pg connection carries NO picker — the select leaves the DOM,
 *  not hidden-with-options. A hidden native select still surfaces in automation accessibility
 *  trees as a live "Schema" button holding the previous pg connection's pick, which read as a
 *  redis page offering a schema dropdown; hidden options are state residue even unseen. */
function dbPaintSchemaOptions(): void {
  var d = state.db;
  var sel = $<HTMLSelectElement>("dbSchema");
  if (!d!.conn || !dbIsPg()) {
    if (sel) sel.remove();
    return;
  }
  if (!sel) {
    // Coming back to pg after a non-pg connection removed it: re-create it in the sidebar,
    // right after the connection picker, wired like the mount path wires it.
    sel = el("select") as HTMLSelectElement;
    sel.id = "dbSchema";
    sel.setAttribute("aria-label", "Schema");
    var conn = $("dbConn");
    if (conn && conn.parentNode) conn.parentNode.insertBefore(sel, conn.nextSibling);
    dbWireSchemaSelect(sel);
  }
  sel.hidden = false;
  var schemas: string[] = [];
  d!.tables.forEach(function (t: ApiDbTableRow): void {
    if (schemas.indexOf(t.schema) < 0) schemas.push(t.schema);
  });
  var current = d!.schemaFilter || "";
  if (current && schemas.indexOf(current) < 0) schemas.push(current);
  schemas.sort();
  sel.innerHTML = "";
  var all = el("option", "", "All schemas") as HTMLOptionElement;
  all.value = "";
  all.selected = current === "";
  sel.appendChild(all);
  schemas.forEach(function (s: string): void {
    var n = d!.tables.filter(function (t: ApiDbTableRow): boolean { return t.schema === s; }).length;
    var o = el("option", "", s + " (" + n + ")") as HTMLOptionElement;
    o.value = s;
    o.selected = s === current;
    sel.appendChild(o);
  });
}

/** The selected connection's dialect word ("mysql" | "pg") — the DDL sheets build their
 *  type suggestions and titles on it (docs/22 W4.6). */
function dbDialectOf(): string {
  var c = state.db!.conns.find(function (x: ApiDbConnectionRow): boolean { return x.name === state.db!.conn; });
  return (c && c.dialect) || "mysql";
}

/** Every schema the loaded pages have shown, "public" always among them — the New table
 *  sheet's where-it-goes select (docs/22 W4.6; swiss-ui-design rule 6). */
function dbKnownSchemas(): string[] {
  var d = state.db;
  var out: string[] = [];
  d!.tables.forEach(function (t: ApiDbTableRow): void {
    if (t.schema && out.indexOf(t.schema) < 0) out.push(t.schema);
  });
  if (out.indexOf("public") < 0) out.unshift("public");
  out.sort();
  return out;
}

function dbOpenTable(t: ApiDbTableRow): void {
  var d = state.db;
  if (t.name === d!.table && t.schema === d!.schema) return;
  if (!dbOkToDrop()) return;
  d!.table = t.name; d!.schema = t.schema;
  d!.offset = 0; d!.order = null; d!.dir = "asc"; d!.filters = [];
  d!.tab = "data"; d!.detail = null;
  d!.sqlResult = null;
  d!.sqlResults = null; d!.sqlTab = 0; // docs/22 W4.3: opening a table closes every result tab
  dbDropEdits();
  renderDbTables();
  dbLoadData();
  // docs/22 W5.2: the detail rides along with every open — the FK columns' header arrows
  // read it, and the Structure tabs were going to ask for it on their first click anyway.
  dbLoadDetail();
}

/** docs/22 W5.2: the FK jump's payload — one describe_table FK row plus the focused row's
 *  value become exactly the open-table state a typed filter would have built (the W1.5
 *  channel's shape), or nothing at all: col = NULL matches nothing, so a NULL has no jump.
 *  Pure. */
function dbFkJump(fk: ApiDbFkRow, value: unknown): { schema: string | null; table: string; filters: DbFilterTerm[] } | null {
  if (!fk || value === null || value === undefined) return null;
  return {
    schema: fk.refSchema || null,
    table: fk.refTable,
    filters: [{ column: fk.refColumn, op: "eq", value: value as string | number | boolean }],
  };
}

/** The focused row's value of one column — the header arrow's "current value". Buffered
 *  inserts sit in front of the page's rows exactly as the grid draws them. undefined means
 *  nothing is focused; null IS a value (a NULL cell) — the caller tells them apart. Pure. */
function dbFocusedColumnValue(d: DbState | null, column: string): unknown {
  if (!d || !d.focus || !d.data) return undefined;
  if (d!.focus.r < d!.inserts.length) return d!.inserts[d!.focus.r].values[column];
  var row = d!.data.rows[d!.focus.r - d!.inserts.length];
  return row ? row[column] : undefined;
}

/** Open the referenced table with the filter preset (adminer's select-link, dbgate's
 *  openReferenceForm — both land on the target table already filtered to one row). */
function dbFkOpen(fk: ApiDbFkRow, value: unknown): void {
  var j = dbFkJump(fk, value);
  if (!j || !dbOkToDrop()) return;
  var d = state.db;
  d!.table = j.table; d!.schema = j.schema;
  d!.offset = 0; d!.order = null; d!.dir = "asc"; d!.filters = j.filters;
  d!.tab = "data"; d!.detail = null;
  d!.sqlResult = null; d!.sqlResults = null; d!.sqlTab = 0;
  dbDropEdits();
  renderDbTables();
  dbLoadData();
  dbLoadDetail(); // the target table's own FK arrows arrive with its detail
}

export { DB_HISTORY_KEY, DB_HISTORY_MAX, DB_PAGE_SIZES, dbClearSel, dbConnLabel, dbDialectOf, dbDropEdits, dbFilterMatches, dbFocusedColumnValue, dbFkJump, dbFkOpen, dbIsPg, dbKnownSchemas, dbLoadTables, dbOkToDrop, dbOpenTable, dbPending, dbPkKey, dbPkVals, dbResultKey, loadDbView, renderDbSide, renderDbTables, renderDbView };

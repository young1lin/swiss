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

const DB_PAGE_SIZES = [10, 20, 50, 100, 200, 500];
const DB_HISTORY_KEY = "mcp_gateway_db_sql_history";
const DB_HISTORY_MAX = 50;

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

function dbPending()         {
  const d = state.db;
  if (!d) return 0;  // between unmount and the next mount there is nothing buffered
  const d_ = d ;
  return Object.keys(d_.updates).length + Object.keys(d_.deletes).length + d_.inserts.length;
}

/** Ask before an action would drop buffered edits; false when the user said no. The redis
 *  value buffer counts too (docs/22 W3.3) — a key switch that ate buffered fields silently
 *  would break the same rule the row grid's guard exists for. */
function dbOkToDrop()          {
  const n = dbPending() + dbRedisPendingCount();
  return !n || confirm("Discard " + n + " uncommitted change" + (n > 1 ? "s" : "") + "? Nothing has been written yet.");
}

function dbDropEdits() {
  const d = state.db;
  const d_ = d ;
  d_.updates = {}; d_.deletes = {}; d_.inserts = [];
  dbClearSel(); // selection addresses rows of the OLD page — it never survives a reload
  d_.sqlPreview = false;
}

/** Row selection is scoped to what is on screen: cleared whenever the grid identity changes. */
function dbClearSel()       {
  const d = state.db;
  const d_ = d ;
  d_.sel = {};
  d_.selAnchor = -1;
}

function dbPkKey(pkCols          , row                         )         {
  return JSON.stringify(pkCols.map((c        )          => { return row[c]; }));
}

function dbPkVals(pkCols          , row                         )                          {
  const out                          = {};
  pkCols.forEach((c        )       => { out[c] = row[c]; });
  return out;
}

/** docs/22 W4.3: a query-result row's selection key — the tab index namespaces it, so every
 *  result tab owns an independent checked-row set the same way the table grid's keys live
 *  apart from these under the "q" prefix. Pure. */
function dbResultKey(tab        , i        )         {
  return "q" + tab + ":" + i;
}

/* --- view skeleton ------------------------------------------------------------------------------- */

async function loadDbView()                {
  // The entry choke point: unmount freed state.db, and nothing else rebuilds it — the
  // module-top assignment ran exactly once, on first import. Without this every second
  // entry died reading state.db.grep halfway through renderDbView's wiring.
  if (!state.db) state.db = dbFreshState();
  renderDbView();
  const j = await apiJson                                       ("/api/db");
  if (!j) return;
  const d = state.db;
  const d_ = d ;
  d_.conns = j.connections || [];
  const stillThere = d_.conns.some((c                    )          => { return c.name === d?.conn; });
  if (!stillThere) {
    d_.conn = d_.conns.length ? d_.conns[0].name : null;
    d_.table = null; d_.schema = null; d_.data = null; d_.tables = [];
    d_.redis = null; d_.redisKey = null; d_.redisValue = null;
    d_.redisEdits = null;
    d_.sqlResult = null;
    d_.sqlResults = null; // docs/22 W4.3: the tab strip goes with the result it named
    d_.sqlTab = 0;
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
  if (d_.conn && dbIsRedis()) { if (!d_.redis) void dbLoadKeys(true); else renderDbTables(); }
  else if (d_.conn) { if (!d_.tables.length) void dbLoadTables(); else renderDbTables(); }
  else renderDbTables();
  if (d_.table && !d_.data && !dbIsRedis()) void dbLoadData(true);
}

function renderDbView()       {
  const pane = $("pane");
  // db-host turns the pane into a full-bleed workspace body (layout "workspace", docs/13
  // D5 as revised): no pane padding, no measure — the explorer divides its own space.
  // views/data.js takes the class back off on unmount so no other page inherits it.
  pane.classList.add("db-host");
  pane.innerHTML = "";
  const root = el("div", "db-root");
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
  $                   ("dbConn").onchange = (e) => {
    const t = e.currentTarget                     ;
    const db_ = state.db ;
    if (t.value === db_.conn) return;
    if (!dbOkToDrop()) { t.value = db_.conn || ""; return; }
    // The Activity page belongs to ONE connection's server; the switch leaves it behind —
    // restore the normal pane (and stop its poll) before the state it reads changes.
    if (db_.activity) dbActivityClose();
    const d = state.db;
    const d_ = d ;
    d_.conn = t.value; d_.table = null; d_.schema = null; d_.data = null;
    d_.tables = []; d_.tablesPage = 0; d_.order = null; d_.sqlResult = null;
    d_.sqlResults = null; d_.sqlTab = 0; // docs/22 W4.3: no stale tabs across a connection switch
    d_.schemaFilter = ""; // a schema pick was made against the other connection's catalog
    d_.redis = null; d_.redisKey = null; d_.redisValue = null;
    d_.redisEdits = null;
    d_.filters = [];
    d_.sort = "name"; d_.sortDir = "asc"; // the new connection's kind may not have the chosen key
    // A sidebar search is table-list-scoped: carrying "tsys_" from one connection into the next
    // silently filters the new list down to nothing. Reset it and the box that shows it.
    d_.grep = "";
    d_.redisType = ""; // same reasoning: a type filter is chosen against a key list, not inherited
    const gb = $                  ("dbGrep");
    if (gb) gb.value = "";
    dbDropEdits();
    dbSyncKind();
    renderDbTables(); renderDbToolbar(); renderDbFilters(); renderDbGrid(); renderDbBar();
    // The page-bar count chip names the connection (views/data.js countText) and is otherwise
    // written only on navigation — the switch has to rewrite it here or the bar keeps naming
    // the connection this view just left behind.
    const chip = $("countChip");
    if (chip) chip.textContent = currentPageCount();
    if (dbIsRedis()) void dbLoadKeys(true);
    else void dbLoadTables();
  };
  // The view is REBUILT on every entry, but d.grep persists for the same table — seed the box
  // from state, or the list stays filtered by a term the (fresh, empty) input no longer shows.
  const db_ = state.db ;
  $                  ("dbGrep").value = db_.grep || "";
  let t                                           ;
  $                  ("dbGrep").oninput = (e) => {
    const v = (e.currentTarget                    ).value;
    clearTimeout(t);
    t = setTimeout(()       => {
      // Leaving the view frees state.db (views/data.js unmount); a debounce pending across
      // that boundary once threw here. The view is gone — the filter belongs to no one.
      if (!state.db) return;
      const d = state.db ;
      d.grep = v; d.tablesPage = 0;
      if (dbIsRedis()) void dbLoadKeys(true);
      else void dbLoadTables();
    }, 300);
  };
  $                   ("dbSort").onchange = (e) => {
    const d = state.db;
    const d_ = d ;
    d_.sort = (e.currentTarget                     ).value;
    d_.tablesPage = 0;
    if (dbIsRedis()) renderDbTables(); // keys sort in place over what has been scanned
    else void dbLoadTables();
  };
  $("dbSortDir").onclick = ()       => {
    const d = state.db;
    const d_ = d ;
    d_.sortDir = d_.sortDir === "asc" ? "desc" : "asc";
    dbPaintSort();
    d_.tablesPage = 0;
    if (dbIsRedis()) renderDbTables();
    else void dbLoadTables();
  };
  // The schema picker (pg only): picking one re-requests the table list inside that schema.
  // The wiring itself lives in dbWireSchemaSelect so a RE-CREATED picker (dbPaintSchemaOptions
  // removes the select for non-pg connections and brings it back for pg) gets it too.
  const schemaSel = $                   ("dbSchema");
  if (schemaSel) dbWireSchemaSelect(schemaSel);
  $                     ("dbSql").value = db_.sqlText;
  $                     ("dbSql").oninput = (e) => { const t = e.currentTarget                       ; state.db .sqlText = t.value; dbSqlPaint(); dbSuggestOnInput.call(t); };
  $                     ("dbSql").onscroll = (e) => {
    const t = e.currentTarget                       ;
    const hl = $("dbSqlHl");
    if (hl) { hl.scrollTop = t.scrollTop; hl.scrollLeft = t.scrollLeft; }
    dbSuggestHide(); // the caret's point scrolled with the text; a list pinned to stale
    // coordinates would point at the wrong word. The next keystroke reopens it in place.
  };
  $                     ("dbSql").addEventListener("blur", dbSuggestHide);
  // The suggest hook runs beside the Run shortcut: it only ever consumes the keys the open
  // list owns (arrows / Tab / Enter / Esc) and leaves Ctrl+Enter to run the block.
  $                     ("dbSql").addEventListener("keydown", dbSuggestKeys);
  $                     ("dbSql").onkeydown = (e               )       => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); void dbRunSql(); }
  };
  dbSqlPaint();
  dbSyncKind();
  // wrapped: onclick hands the handler the click EVENT, and dbRunSql's first parameter is
  // `explain` — an event object is truthy, so a plain Run has been quietly running EXPLAIN.
  $                   ("dbSqlRun").onclick = ()       => { void dbRunSql(false); };
  $                   ("dbSqlExplain").onclick = (ev            )       => {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without it the click that opens the menu also tears it down (same as the Export menu).
    ev.stopPropagation();
    popupMenu((ev.currentTarget                     ).getBoundingClientRect(), [
      { label: "Explain", fn: ()       => { void dbRunSql("plan"); } },
      { label: "Explain ANALYZE", fn: ()       => { void dbRunSql("analyze"); } },
    ]);
  };
  $                   ("dbSqlHistory").onchange = (e) => {
    const t = e.currentTarget                     ;
    if (t.value === "") return;
    // docs/22 W5.4: "f"+i is a favorite, a plain index history — both land in the console.
    const fav = t.value.charAt(0) === "f";
    const db_ = state.db ;
    const sql = fav ? db_.favorites?.[Number(t.value.slice(1))]
      : db_.history?.[Number(t.value)];
    t.value = ""; // back to the label, so the same entry can be picked again
    if (sql == null) return;
    db_.sqlText = sql;
    const ta = $                     ("dbSql");
    if (ta) { ta.value = sql; dbSqlPaint(); ta.focus(); }
  };
  // docs/22 W5.4: the star saves the console text to the favorites group; Format re-indents
  // it in place (whitespace only — the run result cannot change).
  const favBtn = $                   ("dbSqlFav");
  if (favBtn) {
    favBtn.innerHTML = icon("star");
    favBtn.title = "Save the console text to favorites";
    favBtn.onclick = ()       => { dbFavPush(state.db .sqlText); };
  }
  const fmtBtn = $                   ("dbSqlFormat");
  if (fmtBtn) {
    fmtBtn.onclick = ()       => {
      const d = state.db;
      const d_ = d ;
      if (!d_.sqlText || !d_.sqlText.trim()) return;
      d_.sqlText = dbFormatSql(d_.sqlText);
      const ta = $                     ("dbSql");
      if (ta) { ta.value = d_.sqlText; dbSqlPaint(); ta.focus(); }
    };
  }
  dbHistoryLoad();
  dbFavLoad();
  dbHistoryRender();
  renderDbToolbar(); renderDbGrid(); renderDbBar();
  // docs/22 W4.6: the list band's + opens the New table sheet — SQL connections only (the
  // band itself is hidden for redis and for no-connection in dbSyncKind, with dbMore).
  const dbNewTable = $                   ("dbNewTable");
  if (dbNewTable) {
    dbNewTable.innerHTML = icon("plus");
    dbNewTable.onclick = ()       => {
      const d = state.db;
      const d_ = d ;
      if (!d_.conn || dbIsRedis()) return;
      openDbDdlSheet("table", {
        dialect: dbDialectOf(),
        conn: d_.conn,
        schema: dbIsPg() ? (d_.schemaFilter || "public") : "",
        schemas: dbIsPg() ? dbKnownSchemas() : [],
      });
    };
  }
  const dbMore = $                   ("dbMore");
  dbMore.innerHTML = icon("ellipsis");
  dbMore.onclick = (e            )       => {
    // stopPropagation: the document click closes popup menus — the opening click must not.
    e.stopPropagation();
    const d = state.db;
    popupMenu((e.currentTarget                     ).getBoundingClientRect(), [
      { label: d?.activity ? "Close activity" : "Activity…", fn: dbActivityToggle },
    ]);
  };
  // The section page replaces the pane's content when (re)entered with it open — closing is
  // just another rebuild with the flag down, so nothing here needs a special restore path.
  if (db_.activity && db_.conn && !dbIsRedis()) dbActivityPane(dbActivityClose);
}

/* The Activity monitor (docs/22 W3.2): one flag drives it. Opening rebuilds the pane with the
   section in place; closing rebuilds it back — the same renderDbView that drew it. */
function dbActivityToggle()       {
  const d = state.db;
  const d_ = d ;
  if (d_.activity) { dbActivityClose(); return; }
  if (!d_.conn || dbIsRedis()) return;
  d_.activity = true;
  renderDbView();
}

function dbActivityClose()       {
  const d = state.db;
  const d_ = d ;
  d_.activity = false;
  d_.activityRows = null;
  dbActivityPollStop();
  renderDbView();
}

/* The list's sort row. SQL connections sort server-side (name/rows/size against the catalog);
 * redis sorts client-side over the keys SCAN has already handed over (key/TTL). One control,
 * one direction button; the option set repaints when the connection kind changes. */
function dbSortOptions()                             {
  return dbIsRedis()
    ? [{ v: "name", t: "Key" }, { v: "ttl", t: "TTL" }]
    : [{ v: "name", t: "Name" }, { v: "rows", t: "Rows" }, { v: "size", t: "Size" }];
}

function dbPaintSort()       {
  const d = state.db;
  const sel = $                   ("dbSort"), dir = $("dbSortDir");
  if (!sel || !dir) return;
  const opts = dbSortOptions();
  const d_ = d ;
  if (!opts.some((o                          )          => { return o.v === d?.sort; })) d_.sort = opts[0].v; // kind switched
  sel.innerHTML = "";
  opts.forEach((o                          )       => {
    const op = el("option", "", o.t)                     ;
    op.value = o.v;
    op.selected = o.v === d?.sort;
    sel.appendChild(op);
  });
  dir.textContent = d_.sortDir === "asc" ? "\u2191" : "\u2193";
  dir.setAttribute("aria-label", d_.sortDir === "asc" ? "Sort ascending" : "Sort descending");
}

/** Redis keys arrive in SCAN (hash-slot) order; the list sorts what has been loaded. Key names
 *  compare naturally — numeric runs by value, case folded (t2 < t10, foo == FOO) — because the
 *  raw byte order is exactly the ASCII sort nobody asked for. TTL puts expiring keys first; no
 *  expiry (-1) reads as forever. */
function dbRedisCompare(a                  , b                  )         {
  const d = state.db;
  const d_ = d ;
  if (d_.sort === "ttl") {
    const ta         = a.ttl  < 0 ? Infinity : a.ttl ;
    const tb         = b.ttl  < 0 ? Infinity : b.ttl ;
    if (ta !== tb) return d_.sortDir === "asc" ? ta - tb : tb - ta;
  }
  const c = String(a.key).localeCompare(String(b.key), undefined, { numeric: true, sensitivity: "base" });
  return d_.sortDir === "asc" ? c : -c;
}

/* Everything in the skeleton that reads differently per connection KIND: the sidebar filter's
   placeholder, the console's placeholder and hint, and whether Explain exists (EXPLAIN is SQL; a
   redis command has no plan). Called on mount once the connections are known, and again on every
   switch — it used to run once, at mount, before /api/db had even answered, so it never saw a
   redis connection. */
function dbSyncKind()       {
  const grep = $                  ("dbGrep"), sql = $                     ("dbSql"), explain = $("dbSqlExplain"), hint = $("dbSqlHint");
  if (!grep || !sql || !explain || !hint) return;
  const fmt = $("dbSqlFormat");
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
  const more = $("dbMore");
  const d = state.db ;
  if (more) more.hidden = !d.conn || dbIsRedis();
  const listHead = $("dbListHead");
  if (listHead) listHead.hidden = !d.conn || dbIsRedis();
}

/** The one label a connection goes by — the sidebar dropdown's option text. The page-bar
 *  count chip (views/data.js countText) shows the SAME string, so the bar and the control
 *  that changes it can never disagree; two builders is how the bar ended up saying "Data"
 *  twice while the dropdown said "shop-redis · redis". */
function dbConnLabel(c                    )         {
  return c.name + " · " + c.dialect;
}

function renderDbSide()       {
  const d = state.db;
  const sel = $                   ("dbConn");
  if (!sel) return;
  sel.innerHTML = "";
  const d_ = d ;
  if (!d_.conns.length) {
    const none = el("option", "", "No database MCPs")                     ;
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
  const order           = [];
  const buckets                                       = {};
  d_.conns.forEach((c                    )       => {
    const g = c.group || "default";
    if (!buckets[g]) { buckets[g] = []; order.push(g); }
    buckets[g].push(c);
  });
  const addOption = (parent             , c                    )       => {
    const o = el("option", "", dbConnLabel(c))                     ;
    o.value = c.name;
    o.selected = c.name === d?.conn;
    parent.appendChild(o);
  };
  if (order.length < 2) {
    d_.conns.forEach((c                    )       => { addOption(sel, c); });
    return;
  }
  order.forEach((g        )       => {
    const og = el("optgroup")                       ;
    og.label = g;
    buckets[g].forEach((c                    )       => { addOption(og, c); });
    sel.appendChild(og);
  });
}

/* --- lazy table list ---------------------------------------------------------------------------- */

// One /tables request chain: a slow answer for a page or filter the user just left must be
// dropped, or it would repopulate the sidebar with the stale page (docs/22 closeout audit).
const dbTablesReq = dbReqGuard();

async function dbLoadTables()                {
  const d = state.db;
  const d_ = d ;
  if (!d_.conn) return;
  const box = $("dbTables");
  if (box) { box.innerHTML = ""; box.appendChild(el("div", "db-hint", "Loading…")); }
  let q = "/api/db/" + encodeURIComponent(d_.conn) + "/tables?page=" + d_.tablesPage;
  if (d_.grep) q += "&grep=" + encodeURIComponent(d_.grep);
  if (d_.schemaFilter) q += "&schema=" + encodeURIComponent(d_.schemaFilter);
  if (d_.sort) q += "&sort=" + encodeURIComponent(d_.sort) + "&dir=" + encodeURIComponent(d_.sortDir || "asc");
  const token = dbTablesReq.issue();
  const j = await apiJson                     (q);
  if (!dbTablesReq.accepts(token)) return; // superseded: a newer page/filter owns the list
  if (!j) { if ($("dbTables")) $("dbTables").innerHTML = ""; return; }
  d_.tables = j.tables || [];
  d_.tablesTotal = j.total || 0;
  d_.tablesLimit = j.limit || 200;
  d_.more = !!j.more;
  renderDbTables();
}

/** A Postgres connection (many schemas) vs everything else. The schema grouping and the
 *  schema picker exist only for it; MySQL is one database and redis has no schemas at all. */
function dbIsPg()          {
  const c = state.db?.conns.find((x                    )          => { return x.name === state.db?.conn; });
  return !!c && c.dialect === "pg";
}

/* The sidebar grep grammar (docs/22 W1.6): comma-separated terms AND together, "|" inside a
   term is OR, "*" is a wildcard, everything case-insensitive. A bare term with no "*" keeps
   the substring behaviour the list always had. Pure so the grammar can be pinned without a
   DOM; the server's table grep speaks the same language in SQL. */
function dbFilterMatches(tokens        , name         )          {
  const n = String(name).toLowerCase();
  const terms = String(tokens).toLowerCase().split(",")
    .map((s        )         => { return s.trim(); }).filter(Boolean);
  if (!terms.length) return true;
  return terms.every((term        )          => {
    return term.split("|").some((alt        )          => {
      alt = alt.trim();
      if (!alt) return false;
      if (alt.indexOf("*") < 0) return n.indexOf(alt) >= 0;
      const re = "^" + alt.split("*").map((p        )         => {
        return p.replace(/[.+?^\[\]{}()\\\-|]/g, "\\$&");
      }).join(".*") + "$";
      return new RegExp(re).test(n);
    });
  });
}

function renderDbTables()       {
  const d = state.db;
  const box = $("dbTables");
  if (!box) return;
  box.innerHTML = "";
  dbPaintSchemaOptions();
  const d_ = d ;
  if (!d_.conn) {
    box.appendChild(el("div", "db-hint", "Add a mysql or pg MCP, then browse it here."));
    return;
  }
  if (dbIsRedis()) {
    const rr = d_.redis;
    if (!rr || !rr.keys.length) {
      // docs/22 closeout B1: a failed scan is a FAILURE, not an empty keyspace — the toast
      // carries the server's own text; this row keeps the list from pretending otherwise.
      box.appendChild(el("div", "db-hint", d_.redisError
        ? "Scan failed — the toast carries the server's error; this list is the last good page."
        : d_.grep ? 'No keys match "' + d_.grep + '"' : "No keys yet — scan returned none."));
    }
    const sortedKeys = (rr ? rr.keys : []).slice().sort(dbRedisCompare);
    sortedKeys.forEach((k                  )       => {
      const b = el("button", "db-table" + (k.key === d?.redisKey ? " sel" : ""));
      b.title = k.key; // the row truncates with an ellipsis; the full key is one hover away
      b.appendChild(el("div", "db-table-name", k.key));
      let meta = k.type;
      if (k.ttl  >= 0) meta += " · ttl " + k.ttl + "s";
      b.appendChild(el("div", "db-table-meta", meta));
      // Switching keys drops the typed-value buffer (docs/22 W3.3): a different key cannot
      // adopt another key's fields, so the same guard the table switch uses asks first.
      b.onclick = ()       => {
        if (k.key === d?.redisKey || dbOkToDrop()) void dbLoadRedisValue(k.key);
      };
      box.appendChild(b);
    });
    const foot2 = $("dbTablesPager");
    if (foot2) {
      foot2.innerHTML = "";
      const shown = rr ? rr.keys.length : 0;
      foot2.appendChild(el("span", "", shown.toLocaleString() + (rr && rr.total != null ? " of " + Number(rr.total).toLocaleString() + " keys" : " keys")));
      if (rr && !rr.done) {
        const more = el("button", "btn", "More");
        more.title = "Continue the SCAN";
        more.onclick = ()       => { void dbLoadKeys(false); };
        foot2.appendChild(more);
      }
    }
    return;
  }
  if (!d_.tables.length) {
    box.appendChild(el("div", "db-hint", d_.grep ? 'No tables match "' + d_.grep + '".' : "No tables."));
  }
  // docs/22 W1.1: a Postgres catalog is many schemas, so the page renders grouped — the docs/20
  // §4 container vocabulary (band header, mixed-case name, tnum count, indented body behind the
  // guide line). MySQL is one database: the flat list, no headers, visually exactly as before.
  if (dbIsPg() && d_.tables.length) {
    const schemas           = [];
    d_.tables.forEach((t               )       => {
      if (schemas.indexOf(t.schema) < 0) schemas.push(t.schema);
    });
    schemas.forEach((s        )       => {
      const rows = d .tables.filter((t               )          => { return t.schema === s; });
      const g = el("div", "grp grp--side");
      const head = el("div", "grp-head");
      head.appendChild(el("span", "grp-toggle", s));
      head.appendChild(el("span", "grp-n", String(rows.length)));
      g.appendChild(head);
      const body = el("div", "grp-body");
      rows.forEach((t               )       => { body.appendChild(dbTableRow(t)); });
      g.appendChild(body);
      box.appendChild(g);
    });
  } else {
    d_.tables.forEach((t               )       => { box.appendChild(dbTableRow(t)); });
  }
  const foot = $("dbTablesPager");
  if (!foot) return;
  foot.innerHTML = "";
  const from = d_.tablesTotal ? d_.tablesPage * d_.tablesLimit + 1 : 0;
  const to = d_.tablesPage * d_.tablesLimit + d_.tables.length;
  foot.appendChild(el("span", "", from.toLocaleString() + "–" + to.toLocaleString() + " of " + d_.tablesTotal.toLocaleString()));
  const prev = el("button", "btn icon")                     ;
  prev.innerHTML = icon("chevron-left");
  prev.title = "Previous page of tables";
  prev.disabled = d_.tablesPage === 0;
  prev.onclick = ()       => { d .tablesPage--; void dbLoadTables(); };
  const next = el("button", "btn icon")                     ;
  next.innerHTML = icon("chevron-right");
  next.title = "Next page of tables";
  next.disabled = !d_.more;
  next.onclick = ()       => { d .tablesPage++; void dbLoadTables(); };
  foot.appendChild(prev);
  foot.appendChild(next);
}

/** One table row, shared by the flat list and every schema group. */
function dbTableRow(t               )              {
  const d = state.db;
  const d_ = d ;
  const b = el("button", "db-table" + (t.name === d_.table && t.schema === d_.schema ? " sel" : ""));
  b.title = t.name;
  b.appendChild(el("div", "db-table-name", t.name));
  b.appendChild(el("div", "db-table-meta",
    t.type + (t.approxRows != null ? " · ~" + Number(t.approxRows).toLocaleString() + " rows" : "") +
    (t.size ? " · " + t.size : "")));
  b.onclick = ()       => { dbOpenTable(t); };
  return b;
}

/** The schema picker's one behavior: a pick re-requests the table list inside that schema. */
function dbWireSchemaSelect(sel                   )       {
  sel.onchange = (e) => {
    const t = e.currentTarget                     ;
    const d = state.db;
    const d_ = d ;
    if (t.value === d_.schemaFilter) return;
    d_.schemaFilter = t.value;
    d_.tablesPage = 0;
    void dbLoadTables();
  };
}

/** The schema picker above the grep box (pg only): "All schemas" plus one option per schema
 *  on the current page, counts from the page the list is showing. A schema picked on an earlier
 *  page stays selectable even when this page does not carry it.
 *  docs/22 closeout B7: a non-pg connection carries NO picker — the select leaves the DOM,
 *  not hidden-with-options. A hidden native select still surfaces in automation accessibility
 *  trees as a live "Schema" button holding the previous pg connection's pick, which read as a
 *  redis page offering a schema dropdown; hidden options are state residue even unseen. */
function dbPaintSchemaOptions()       {
  const d = state.db;
  let sel = $                   ("dbSchema");
  const d_ = d ;
  if (!d_.conn || !dbIsPg()) {
    if (sel) sel.remove();
    return;
  }
  if (!sel) {
    // Coming back to pg after a non-pg connection removed it: re-create it in the sidebar,
    // right after the connection picker, wired like the mount path wires it.
    sel = el("select")                     ;
    sel.id = "dbSchema";
    sel.setAttribute("aria-label", "Schema");
    const conn = $("dbConn");
    if (conn && conn.parentNode) conn.parentNode.insertBefore(sel, conn.nextSibling);
    dbWireSchemaSelect(sel);
  }
  sel.hidden = false;
  const schemas           = [];
  d_.tables.forEach((t               )       => {
    if (schemas.indexOf(t.schema) < 0) schemas.push(t.schema);
  });
  const current = d_.schemaFilter || "";
  if (current && schemas.indexOf(current) < 0) schemas.push(current);
  schemas.sort();
  sel.innerHTML = "";
  const all = el("option", "", "All schemas")                     ;
  all.value = "";
  all.selected = current === "";
  sel.appendChild(all);
  schemas.forEach((s        )       => {
    const n = d?.tables.filter((t               )          => { return t.schema === s; }).length;
    const o = el("option", "", s + " (" + n + ")")                     ;
    o.value = s;
    o.selected = s === current;
    sel.appendChild(o);
  });
}

/** The selected connection's dialect word ("mysql" | "pg") — the DDL sheets build their
 *  type suggestions and titles on it (docs/22 W4.6). */
function dbDialectOf()         {
  const c = state.db?.conns.find((x                    )          => { return x.name === state.db?.conn; });
  return (c && c.dialect) || "mysql";
}

/** Every schema the loaded pages have shown, "public" always among them — the New table
 *  sheet's where-it-goes select (docs/22 W4.6; swiss-ui-design rule 6). */
function dbKnownSchemas()           {
  const d = state.db;
  const out           = [];
  d?.tables.forEach((t               )       => {
    if (t.schema && out.indexOf(t.schema) < 0) out.push(t.schema);
  });
  if (out.indexOf("public") < 0) out.unshift("public");
  out.sort();
  return out;
}

function dbOpenTable(t               )       {
  const d = state.db;
  const d_ = d ;
  if (t.name === d_.table && t.schema === d_.schema) return;
  if (!dbOkToDrop()) return;
  d_.table = t.name; d_.schema = t.schema;
  d_.offset = 0; d_.order = null; d_.dir = "asc"; d_.filters = [];
  d_.tab = "data"; d_.detail = null;
  d_.sqlResult = null;
  d_.sqlResults = null; d_.sqlTab = 0; // docs/22 W4.3: opening a table closes every result tab
  dbDropEdits();
  renderDbTables();
  void dbLoadData();
  // docs/22 W5.2: the detail rides along with every open — the FK columns' header arrows
  // read it, and the Structure tabs were going to ask for it on their first click anyway.
  void dbLoadDetail();
}

/** docs/22 W5.2: the FK jump's payload — one describe_table FK row plus the focused row's
 *  value become exactly the open-table state a typed filter would have built (the W1.5
 *  channel's shape), or nothing at all: col = NULL matches nothing, so a NULL has no jump.
 *  Pure. */
function dbFkJump(fk            , value         )                                                                           {
  if (!fk || value === null || value === undefined) return null;
  return {
    schema: fk.refSchema || null,
    table: fk.refTable,
    filters: [{ column: fk.refColumn, op: "eq", value: value                              }],
  };
}

/** The focused row's value of one column — the header arrow's "current value". Buffered
 *  inserts sit in front of the page's rows exactly as the grid draws them. undefined means
 *  nothing is focused; null IS a value (a NULL cell) — the caller tells them apart. Pure. */
function dbFocusedColumnValue(d                , column        )          {
  if (!d || !d.focus || !d.data) return undefined;
  /* Straight-line flow after the guard: d, d.focus and d.data are all narrowed here, so
   * the reads below need no assertions at all (docs/37 R2). */
  if (d.focus.r < d.inserts.length) return d.inserts[d.focus.r].values[column];
  const row = d.data.rows[d.focus.r - d.inserts.length];
  return row ? row[column] : undefined;
}

/** Open the referenced table with the filter preset (adminer's select-link, dbgate's
 *  openReferenceForm — both land on the target table already filtered to one row). */
function dbFkOpen(fk            , value         )       {
  const j = dbFkJump(fk, value);
  if (!j || !dbOkToDrop()) return;
  const d = state.db;
  const d_ = d ;
  d_.table = j.table; d_.schema = j.schema;
  d_.offset = 0; d_.order = null; d_.dir = "asc"; d_.filters = j.filters;
  d_.tab = "data"; d_.detail = null;
  d_.sqlResult = null; d_.sqlResults = null; d_.sqlTab = 0;
  dbDropEdits();
  renderDbTables();
  void dbLoadData();
  void dbLoadDetail(); // the target table's own FK arrows arrive with its detail
}

export { DB_HISTORY_KEY, DB_HISTORY_MAX, DB_PAGE_SIZES, dbClearSel, dbConnLabel, dbDialectOf, dbDropEdits, dbFilterMatches, dbFocusedColumnValue, dbFkJump, dbFkOpen, dbIsPg, dbKnownSchemas, dbLoadTables, dbOkToDrop, dbOpenTable, dbPending, dbPkKey, dbPkVals, dbResultKey, loadDbView, renderDbSide, renderDbTables, renderDbView };

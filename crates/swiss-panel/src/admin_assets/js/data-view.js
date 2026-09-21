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

                                                                                                                           
                                                                 
import { $, apiJson, dbReqGuard, el, iconNode, targetEl } from "./util.js";
import { fill, h } from "./h.js";
import { currentPageCount } from "./page-registry.js";
import { dbIsRedis, dbLoadKeys, dbRedisClick, dbRedisKeydown } from "./data-browsers.js";
import { dbFiltersChange, dbFiltersClick, dbFiltersInput, dbFiltersKeydown, dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbGridChange, dbGridClick, dbGridKeydown, dbLoadData, dbToolbarClick, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbBarClick, dbFavLoad, dbFavPush, dbFormatSql, dbHistoryLoad, dbHistoryRender, dbRunSql, renderDbBar } from "./data-sql.js";
import { dbActivityClick } from "./data-activity.js";
// The cycle data-view <-> data-structure is the same accepted shape as data-grid <->
// data-cell — both sides only call across it inside functions, never at module scope.
import { dbStructureClick } from "./data-structure.js";
// The pane's delegated listeners chain the form view's dispatchers too. data-form already
// reaches back into data-view for dbPkKey — the same accepted cycle shape as the
// data-structure edge above: both sides only call across it inside functions.
import { dbFormChange, dbFormClick, dbFormKeydown } from "./data-form.js";
import { openDbDdlSheet } from "./data-ddl.js";
import { dbSuggestHide, dbSuggestKeys, dbSuggestOnInput } from "./data-suggest.js";
import { popupMenu } from "./menu.js";
import { dbConn, dbIsMounted, dbSqlTab, dbTab, dbTabs, mountDbView } from "./db-state.js";
// The strip's policy module. The cycle is the same accepted shape as the data-structure edge
// below: data-tabs reaches back for renderDbTables, and both sides only call across it inside
// functions, never at module scope.
import { dbOpenTab, dbResetTabsForConn, dbTabsClick, dbTabsPending, dbTabPending, renderDbTabs } from "./data-tabs.js";
import { locale, tr, trn } from "./i18n.js";

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


/** What the ACTIVE tab is holding back — the number every in-page guard quotes, and the one
 *  the edit bar counts. The row grid's buffer and a key tab's typed-value buffer (docs/22 W3.3)
 *  are the same question asked of two kinds, so one counter answers both. */
function dbPending()         {
  return dbTabPending(dbTab());
}

/** The WHOLE strip's buffered writes (docs/42 D5). The page-leave guard asks this one, not
 *  dbPending(): before the tabs, a buffer could only exist on the object in front of the
 *  operator, and after them a background tab's edits would have walked out unmentioned. */
function dbPendingAll()         {
  return dbTabsPending(dbTabs());
}

/** Ask before an action would drop the ACTIVE tab's buffered edits; false when the user said no. */
function dbOkToDrop()          {
  return dbAskDrop(dbPending());
}

/** Ask before LEAVING the page would drop any tab's buffered edits (docs/42 D5) — once, with
 *  the total, so eight dirty tabs are eight confirms fewer than one guard per tab. */
function dbOkToLeave()          {
  return dbAskDrop(dbPendingAll());
}

function dbAskDrop(n        )          {
  return !n || confirm(tr("dataView.discardNothingWritten",
    { n: trn(n, "dataView.nUncommittedChanges.one", "dataView.nUncommittedChanges.other") }));
}

function dbDropEdits() {
  const t = dbTab();
  if (t.kind !== "table") return; // a key tab buffers under its own redisEdits (data-browsers)
  t.updates = {}; t.deletes = {}; t.inserts = [];
  dbClearSel(); // selection addresses rows of the OLD page — it never survives a reload
  t.sqlPreview = false;
}

/** Row selection is scoped to what is on screen: cleared whenever the grid identity changes. */
function dbClearSel()       {
  const t = dbTab();
  t.sel = {};
  t.selAnchor = -1;
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
  // The entry choke point. It used to be the place a freed record was rebuilt, and getting
  // that wrong killed every second entry halfway through renderDbView's wiring; db-state.ts
  // owns the record now and it is never absent, so all this still marks is that the view is
  // on screen — which the late callbacks below ask about by name.
  mountDbView();
  renderDbView();
  const j = await apiJson                                       ("/api/db");
  if (!j) return;
  const d = dbConn();
  d.conns = j.connections || [];
  const stillThere = d.conns.some((c                    )          => { return c.name === d.conn; });
  if (!stillThere) {
    d.conn = d.conns.length ? d.conns[0].name : null;
    d.tables = [];
    d.redis = null;
    // Every open object dies with the connection it belonged to (docs/42 T2): the strip is
    // replaced by one fresh placeholder of the new connection's family, which resets
    // table/schema/data, redisKey/Value/Edits and any standing query reply in one move.
    dbResetTabsForConn();
  }
  renderDbSide();
  // The skeleton above was drawn before /api/db answered, i.e. with no connection: its hint said
  // "No database MCP registered" and its console wore the SQL placeholder. With the connection
  // known, dress everything that depends on its KIND (redis / SQL) — a redis connection
  // entered first used to keep the SQL console and that hint until something else redrew them.
  dbSyncKind();
  // ...and the strip, for the same reason: the skeleton painted it with no connection known,
  // which is the one state it hides in. Without this it stayed hidden for the whole visit.
  renderDbTabs();
  renderDbToolbar(); renderDbFilters(); renderDbGrid();
  // A refresh re-fetches what is MISSING, not what is already on screen — reloading keys would
  // throw away the pages the user paged in with "More".
  if (d.conn && dbIsRedis()) { if (!d.redis) void dbLoadKeys(true); else renderDbTables(); }
  else if (d.conn) { if (!d.tables.length) void dbLoadTables(); else renderDbTables(); }
  else renderDbTables();
  const t = dbTab();
  if (t.kind === "table" && t.table && !t.data && !dbIsRedis()) void dbLoadData(true);
}

function renderDbView()       {
  const pane = $("pane");
  // db-host turns the pane into a full-bleed workspace body (layout "workspace", docs/13
  // D5 as revised): no pane padding, no measure — the explorer divides its own space.
  // views/data.js takes the class back off on unmount so no other page inherits it.
  pane.classList.add("db-host");
  pane.textContent = "";
  // docs/37 R5: the skeleton is a node tree, and the pane carries ONE delegated listener per
  // event type (property-assigned, never addEventListener — a repaint re-assigns the same
  // property instead of stacking). Every control below is addressed by id or a data-*
  // attribute; nothing is wired per render.
  const root = el("div", "db-root");
  fill(root,
    h("div", { class: "db-side" },
      h("select", { id: "dbConn", aria: { label: tr("dataView.connection") } }),
      h("select", { id: "dbSchema", aria: { label: tr("dataView.schema") }, hidden: true }),
      h("input", { id: "dbGrep", type: "search", placeholder: tr("dataView.filterTables"), aria: { label: tr("dataView.filterTables") } }),
      h("div", { class: "db-sortrow" },
        h("select", { id: "dbSort", aria: { label: tr("dataView.sort") } }),
        h("button", { class: "btn icon", id: "dbSortDir", type: "button", title: tr("dataView.sortDirection") })),
      // docs/22 W4.6: the table list's own header band — the list names itself and carries
      // one persistent dimmed + (the docs/20 §4 container-header glyph) for New table….
      h("div", { class: "db-list-head", id: "dbListHead", hidden: true },
        h("span", { class: "db-list-title" }, tr("dataView.tables")),
        h("button", { class: "btn icon grp-add", id: "dbNewTable", type: "button", aria: { label: tr("dataView.newTable") }, title: tr("dataView.newTable2") }, iconNode("plus"))),
      h("div", { class: "db-tables", id: "dbTables" }, h("div", { class: "db-hint" }, tr("dataView.loading"))),
      h("div", { class: "db-side-foot", id: "dbTablesPager" })),
    h("div", { class: "db-main" },
      // The object tabs (docs/42 T2). L3 resource navigation, page-body owned: the strip is
      // the top EDGE of the grid below it (swiss-ui-design §1.3), which is why it sits above
      // the head row rather than beside the seg pills.
      h("div", { class: "db-tabstrip", id: "dbTabStrip", role: "tablist" }),
      h("div", { class: "db-headrow" },
        h("div", { class: "db-head", id: "dbHead" }),
        // Pane-level actions live behind one ⋯ next to the head (docs/22 W3.2); the Activity
        // monitor is the first. Hidden until a SQL connection exists — redis has no sessions.
        h("button", { class: "btn icon", id: "dbMore", type: "button", title: tr("dataView.morePaneActions"), hidden: true }, iconNode("ellipsis"))),
      h("div", { class: "db-filters", id: "dbFilters" }),
      // The console is the sql tab's BODY now (docs/42 T2), not a block toggled over the
      // pane: renderDbGrid unhides it for a sql tab and hides it for every other kind.
      h("div", { class: "db-console", id: "dbConsole", hidden: true },
        h("div", { class: "db-sql-wrap" },
          h("pre", { class: "db-sql-hl db-sql-face", id: "dbSqlHl", aria: { hidden: "true" } }),
          h("textarea", {
            class: "db-sql-face", id: "dbSql", spellcheck: false,
            placeholder: tr("dataView.selectUpdateDeleteStatements"),
          })),
        h("div", { class: "db-console-row" },
          h("button", { class: "btn", id: "dbSqlRun" }, tr("dataView.run")),
          h("button", { class: "btn", id: "dbSqlExplain" }, tr("dataView.explain")),
          h("button", { class: "btn", id: "dbSqlFormat" }, tr("dataView.format")),
          h("select", { id: "dbSqlHistory", title: tr("dataView.queryHistory") }, h("option", { value: "" }, tr("dataView.history"))),
          h("button", { class: "btn icon", id: "dbSqlFav", type: "button", aria: { label: tr("dataView.saveFavorites") }, title: tr("dataView.saveConsoleTextFavorites") }, iconNode("star")),
          h("span", { class: "hint", id: "dbSqlHint" }, tr("dataView.statementsSplitCtrlEnter")))),
      h("div", { class: "db-grid-wrap", id: "dbGridWrap" }),
      h("div", { class: "db-bar", id: "dbBar", hidden: true })));
  pane.appendChild(root);
  pane.onclick = dbPaneClick;
  pane.oninput = dbPaneInput;
  pane.onchange = dbPaneChange;
  pane.onkeydown = dbPaneKeydown;
  // The view is REBUILT on every entry, but d.grep persists for the same table — seed the box
  // from state, or the list stays filtered by a term the (fresh, empty) input no longer shows.
  const db_ = dbConn();
  $                  ("dbGrep").value = db_.grep || "";
  const st = dbSqlTab();
  $                     ("dbSql").value = st ? st.sqlText : "";
  // The console keeps two direct hooks that delegation cannot express: onscroll never
  // bubbles (the highlight layer must mirror the textarea's scroll), and the suggest keydown
  // must run at the TARGET so it can consume the keys the open list owns before anything
  // else answers. Everything else — input, Ctrl+Enter — rides the pane's delegated listeners.
  $                     ("dbSql").onscroll = (e) => {
    const t = e.currentTarget                       ;
    const hl = $("dbSqlHl");
    if (hl) { hl.scrollTop = t.scrollTop; hl.scrollLeft = t.scrollLeft; }
    dbSuggestHide(); // the caret's point scrolled with the text; a list pinned to stale
    // coordinates would point at the wrong word. The next keystroke reopens it in place.
  };
  $                     ("dbSql").addEventListener("blur", dbSuggestHide);
  $                     ("dbSql").addEventListener("keydown", dbSuggestKeys);
  dbSqlPaint();
  dbSyncKind();
  dbHistoryLoad();
  dbFavLoad();
  dbHistoryRender();
  renderDbTabs();
  renderDbToolbar(); renderDbGrid(); renderDbBar();
}

/* --- #pane's four delegated listeners (docs/37 R5) ------------------------------------------------
   The pane this view owns answers every click/change/input/keydown in it. The chrome
   dispatcher below handles the sidebar and console; each section module exports its own
   dispatcher and the chain calls them in order — selectors are disjoint, and the first
   match wins. Behavior note (docs/37 §10.1): all state is resolved at EVENT time through
   dbConn()/dbTab(), never captured at render time. */

// The sidebar grep's debounce: one module-level timer, restarted per keystroke.
let dbGrepTimer                                           ;

function dbPaneClick(ev            )       {
  const t = targetEl(ev);
  if (!t) return;
  // Grid-internal clicks master stopped AT the node (data-grid.js: every selection checkbox
  // cb/cbAll/cb2/cbAll2 and the column grip carried a bare stopPropagation): connect.ts's
  // document listener closes any open Export/Explain/#dbMore menu over a click that reaches
  // it, and a selection click or a grip grab is not an "outside the menu" click. The
  // checkbox acts on change and the grip on mousedown, so stopping here is the whole click
  // behavior — no dispatcher follows, exactly as master's node handlers did nothing on click.
  if (t.closest(".db-selbox, .db-col-grip")) { ev.stopPropagation(); return; }
  if (dbTabsClick(t)) return;
  if (dbChromeClick(t, ev)) return;
  if (dbToolbarClick(t, ev)) return;
  if (dbGridClick(t)) return;
  if (dbBarClick(t)) return;
  if (dbFiltersClick(t)) return;
  if (dbFormClick(t)) return;
  if (dbStructureClick(t, ev)) return;
  if (dbRedisClick(t, ev)) return;
  if (dbActivityClick(t, ev)) return;
}

function dbPaneInput(ev       )       {
  const t = targetEl(ev);
  if (!t) return;
  if (dbChromeInput(t)) return;
  if (dbFiltersInput(t)) return;
}

function dbPaneChange(ev       )       {
  const t = targetEl(ev);
  if (!t) return;
  if (dbChromeChange(t)) return;
  if (dbGridChange(t, ev)) return;
  if (dbFiltersChange(t)) return;
  if (dbFormChange(t)) return;
}

function dbPaneKeydown(ev               )       {
  const t = targetEl(ev);
  if (!t) return;
  if (dbGridKeydown(t, ev)) return;
  if (dbChromeKeydown(t, ev)) return;
  if (dbFiltersKeydown(ev, t)) return;
  if (dbFormKeydown(t, ev)) return;
  if (dbRedisKeydown(t, ev)) return;
}

/** The sidebar + console half of the delegated click. */
function dbChromeClick(t         , ev            )          {
  if (t.closest("#dbSortDir")) {
    const d = dbConn();
    d.sortDir = d.sortDir === "asc" ? "desc" : "asc";
    dbPaintSort();
    d.tablesPage = 0;
    if (dbIsRedis()) renderDbTables();
    else void dbLoadTables();
    return true;
  }
  // wrapped: dbRunSql's first parameter is 'explain' — an event object is truthy, so a
  // plain Run click used to quietly run EXPLAIN (docs/22 W5.4 audit).
  if (t.closest("#dbSqlRun")) { void dbRunSql(false); return true; }
  const explainBtn = t.closest             ("#dbSqlExplain");
  if (explainBtn) {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without it the click that opens the menu also tears it down (same as the Export menu).
    ev.stopPropagation();
    popupMenu(explainBtn.getBoundingClientRect(), [
      { label: tr("dataView.explain"), fn: ()       => { void dbRunSql("plan"); } },
      { label: tr("dataView.explainAnalyze"), fn: ()       => { void dbRunSql("analyze"); } },
    ]);
    return true;
  }
  if (t.closest("#dbSqlFav")) {
    const st = dbSqlTab();
    if (st) dbFavPush(st.sqlText);
    return true;
  }
  if (t.closest("#dbSqlFormat")) {
    const st = dbSqlTab();
    if (!st || !st.sqlText.trim()) return true;
    st.sqlText = dbFormatSql(st.sqlText);
    const ta = $                     ("dbSql");
    if (ta) { ta.value = st.sqlText; dbSqlPaint(); ta.focus(); }
    return true;
  }
  if (t.closest("#dbNewTable")) {
    const d = dbConn();
    if (!d.conn || dbIsRedis()) return true;
    openDbDdlSheet("table", {
      dialect: dbDialectOf(),
      conn: d.conn,
      schema: dbIsPg() ? (d.schemaFilter || "public") : "",
      schemas: dbIsPg() ? dbKnownSchemas() : [],
    });
    return true;
  }
  const moreBtn = t.closest             ("#dbMore");
  if (moreBtn) {
    // stopPropagation: the document click closes popup menus — the opening click must not.
    ev.stopPropagation();
    // The monitor is an open object (docs/42 T2): the entry opens its tab, and the tab's own
    // × closes it — which is why there is no "Close activity" twin here any more.
    popupMenu(moreBtn.getBoundingClientRect(), [
      { label: tr("dataView.activity"), fn: ()       => { dbOpenTab({ kind: "activity" }); } },
    ]);
    return true;
  }
  // The redis key list: the row carries the key; the live selection decides the guard.
  const keyRow = t.closest             ("[data-rkey]");
  if (keyRow) {
    // A key is an object: it opens its own tab (docs/42 T2), so a second key no longer eats
    // the first one's typed-value buffer and no guard has to ask about it.
    dbOpenTab({ kind: "key", key: keyRow.dataset.rkey || "" });
    return true;
  }
  // A table row: re-find the row in the live list by name+schema and open it.
  const tblRow = t.closest             ("[data-tname]");
  if (tblRow) {
    const d = dbConn();
    const name = tblRow.dataset.tname ;
    const schema = tblRow.dataset.tschema || null;
    const row = d.tables.find((x               )          => { return x.name === name && (x.schema || null) === schema; });
    if (row) dbOpenTab({ kind: "table", table: row.name, schema: schema });
    return true;
  }
  if (t.closest("[data-keysmore]")) { void dbLoadKeys(false); return true; }
  const tpg = t.closest             ("[data-tpg]");
  if (tpg) {
    const d = dbConn();
    if (tpg.dataset.tpg === "prev") d.tablesPage--;
    else d.tablesPage++;
    void dbLoadTables();
    return true;
  }
  return false;
}

function dbChromeInput(t         )          {
  const grep = t.closest                  ("#dbGrep");
  if (grep) {
    const v = grep.value;
    clearTimeout(dbGrepTimer);
    dbGrepTimer = setTimeout(()       => {
      // A debounce pending across an unmount once threw here. The view is gone — the filter
      // belongs to no one, and writing it into the fresh record would be a lie either way.
      if (!dbIsMounted()) return;
      const d = dbConn();
      d.grep = v; d.tablesPage = 0;
      if (dbIsRedis()) void dbLoadKeys(true);
      else void dbLoadTables();
    }, 300);
    return true;
  }
  const ta = t.closest                     ("#dbSql");
  if (ta) {
    const st = dbSqlTab();
    if (st) st.sqlText = ta.value;
    dbSqlPaint();
    dbSuggestOnInput.call(ta);
    return true;
  }
  return false;
}

function dbChromeChange(t         )          {
  const connSel = t.closest                   ("#dbConn");
  if (connSel) {
    const db_ = dbConn();
    if (connSel.value === db_.conn) return true;
    // Every open object, not just the one in front: the switch is a page-wide drop, so it
    // asks with the whole strip's total (docs/42 D5).
    if (!dbOkToLeave()) { connSel.value = db_.conn || ""; return true; }
    const d = dbConn();
    d.conn = connSel.value;
    d.tables = []; d.tablesPage = 0;
    d.schemaFilter = ""; // a schema pick was made against the other connection's catalog
    d.redis = null;
    d.sort = "name"; d.sortDir = "asc"; // the new connection's kind may not have the chosen key
    // A sidebar search is table-list-scoped: carrying "tsys_" from one connection into the next
    // silently filters the new list down to nothing. Reset it and the box that shows it.
    d.grep = "";
    d.redisType = ""; // same reasoning: a type filter is chosen against a key list, not inherited
    const gb = $                  ("dbGrep");
    if (gb) gb.value = "";
    // The strip belongs to the connection being left — every open object closes with it, the
    // activity poll included (docs/42 T2).
    dbResetTabsForConn();
    const ta0 = $                     ("dbSql");
    if (ta0) ta0.value = "";
    dbSqlPaint();
    dbSyncKind();
    renderDbTabs();
    renderDbTables(); renderDbToolbar(); renderDbFilters(); renderDbGrid(); renderDbBar();
    // The page-bar count chip names the connection (views/data.js countText) and is otherwise
    // written only on navigation — the switch has to rewrite it here or the bar keeps naming
    // the connection this view just left behind.
    const chip = $("countChip");
    if (chip) chip.textContent = currentPageCount();
    if (dbIsRedis()) void dbLoadKeys(true);
    else void dbLoadTables();
    return true;
  }
  // The schema picker (pg only): a pick re-requests the table list inside that schema. The
  // select is removed and re-created by dbPaintSchemaOptions — delegation by id needs no
  // per-creation wiring, which is why dbWireSchemaSelect is gone (docs/37 R5).
  const schemaSel = t.closest                   ("#dbSchema");
  if (schemaSel) {
    const d = dbConn();
    if (schemaSel.value === d.schemaFilter) return true;
    d.schemaFilter = schemaSel.value;
    d.tablesPage = 0;
    void dbLoadTables();
    return true;
  }
  const sortSel = t.closest                   ("#dbSort");
  if (sortSel) {
    const d = dbConn();
    d.sort = sortSel.value;
    d.tablesPage = 0;
    if (dbIsRedis()) renderDbTables(); // keys sort in place over what has been scanned
    else void dbLoadTables();
    return true;
  }
  const hist = t.closest                   ("#dbSqlHistory");
  if (hist) {
    if (hist.value === "") return true;
    // docs/22 W5.4: "f"+i is a favorite, a plain index history — both land in the console.
    const fav = hist.value.charAt(0) === "f";
    const db_ = dbConn();
    const sql = fav ? db_.favorites?.[Number(hist.value.slice(1))]
      : db_.history?.[Number(hist.value)];
    hist.value = ""; // back to the label, so the same entry can be picked again
    if (sql == null) return true;
    const st = dbSqlTab();
    if (!st) return true;
    st.sqlText = sql;
    const ta = $                     ("dbSql");
    if (ta) { ta.value = sql; dbSqlPaint(); ta.focus(); }
    return true;
  }
  return false;
}

function dbChromeKeydown(t         , ev               )          {
  if (!t.closest("#dbSql")) return false;
  if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
    ev.preventDefault();
    void dbRunSql();
  }
  return true;
}

/* The list's sort row. SQL connections sort server-side (name/rows/size against the catalog);
 * redis sorts client-side over the keys SCAN has already handed over (key/TTL). One control,
 * one direction button; the option set repaints when the connection kind changes. */
function dbSortOptions()                             {
  return dbIsRedis()
    ? [{ v: "name", t: tr("dataView.sortKey") }, { v: "ttl", t: "TTL" }]
    : [{ v: "name", t: tr("dataView.sortName") }, { v: "rows", t: tr("dataView.sortRows") }, { v: "size", t: tr("dataView.sortSize") }];
}

function dbPaintSort()       {
  const d = dbConn();
  const sel = $                   ("dbSort"), dir = $("dbSortDir");
  if (!sel || !dir) return;
  const opts = dbSortOptions();
  if (!opts.some((o                          )          => { return o.v === d.sort; })) d.sort = opts[0].v; // kind switched
  sel.textContent = "";
  opts.forEach((o                          )       => {
    const op = el("option", "", o.t)                     ;
    op.value = o.v;
    op.selected = o.v === d.sort;
    sel.appendChild(op);
  });
  // The comparison is hoisted: a string literal in a ternary CONDITION reads as bare
  // copy to the ratchet scanner, while the arrows themselves are glyphs, not words.
  const asc = d.sortDir === "asc";
  dir.textContent = asc ? "\u2191" : "\u2193";
  dir.setAttribute("aria-label", asc ? tr("dataView.sortAscending") : tr("dataView.sortDescending"));
}

/** Redis keys arrive in SCAN (hash-slot) order; the list sorts what has been loaded. Key names
 *  compare naturally — numeric runs by value, case folded (t2 < t10, foo == FOO) — because the
 *  raw byte order is exactly the ASCII sort nobody asked for. TTL puts expiring keys first; no
 *  expiry (-1) reads as forever. */
function dbRedisCompare(a                  , b                  )         {
  const d = dbConn();
  if (d.sort === "ttl") {
    const ta         = a.ttl  < 0 ? Infinity : a.ttl ;
    const tb         = b.ttl  < 0 ? Infinity : b.ttl ;
    if (ta !== tb) return d.sortDir === "asc" ? ta - tb : tb - ta;
  }
  const c = String(a.key).localeCompare(String(b.key), undefined, { numeric: true, sensitivity: "base" });
  return d.sortDir === "asc" ? c : -c;
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
    grep.placeholder = tr("dataView.filterKeys"); grep.setAttribute("aria-label", tr("dataView.filterKeys"));
    grep.title = tr("dataView.filterKeysScanPattern");
    sql.placeholder = tr("dataView.redisConsolePlaceholder");
    explain.hidden = true;
    if (fmt) fmt.hidden = true; // docs/22 W5.4: SQL formatting has nothing to say about a command
    hint.textContent = tr("dataView.redisConsoleHint");
  } else {
    // The placeholder IS the grammar (docs/22 W1.6): comma AND, | OR, * wildcard.
    grep.placeholder = "a*, b|c"; grep.setAttribute("aria-label", tr("dataView.filterTables"));
    grep.title = tr("dataView.filterTablesGrammar");
    sql.placeholder = tr("dataView.selectUpdateDeleteStatements");
    explain.hidden = false;
    if (fmt) fmt.hidden = false;
    // docs/22 W4.3: the ; split answers one result tab per statement; the blank-line block
    // rule (W1.8) still decides what a single Run covers.
    hint.textContent = tr("dataView.blankLineStartsNew");
  }
  // The pane's ⋯ exists for the Activity page, which is a SQL-connection feature: a redis
  // connection (or none) hides the button rather than the menu hiding its one item. The
  // list band rides the same condition — redis keys are not tables, and there is nothing to
  // create without a connection.
  const more = $("dbMore");
  const d = dbConn();
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
  const d = dbConn();
  const sel = $                   ("dbConn");
  if (!sel) return;
  sel.textContent = "";
  if (!d.conns.length) {
    const none = el("option", "", tr("dataView.noDatabaseMcps"))                     ;
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
  d.conns.forEach((c                    )       => {
    const g = c.group || "default";
    if (!buckets[g]) { buckets[g] = []; order.push(g); }
    buckets[g].push(c);
  });
  const addOption = (parent             , c                    )       => {
    const o = el("option", "", dbConnLabel(c))                     ;
    o.value = c.name;
    o.selected = c.name === d.conn;
    parent.appendChild(o);
  };
  if (order.length < 2) {
    d.conns.forEach((c                    )       => { addOption(sel, c); });
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
  const d = dbConn();
  if (!d.conn) return;
  const box = $("dbTables");
  if (box) { box.textContent = ""; box.appendChild(el("div", "db-hint", tr("dataView.loading"))); }
  let q = "/api/db/" + encodeURIComponent(d.conn) + "/tables?page=" + d.tablesPage;
  if (d.grep) q += "&grep=" + encodeURIComponent(d.grep);
  if (d.schemaFilter) q += "&schema=" + encodeURIComponent(d.schemaFilter);
  if (d.sort) q += "&sort=" + encodeURIComponent(d.sort) + "&dir=" + encodeURIComponent(d.sortDir || "asc");
  const token = dbTablesReq.issue();
  const j = await apiJson                     (q);
  if (!dbTablesReq.accepts(token)) return; // superseded: a newer page/filter owns the list
  if (!j) { if ($("dbTables")) $("dbTables").textContent = ""; return; }
  d.tables = j.tables || [];
  d.tablesTotal = j.total || 0;
  d.tablesLimit = j.limit || 200;
  d.more = !!j.more;
  renderDbTables();
}

/** A Postgres connection (many schemas) vs everything else. The schema grouping and the
 *  schema picker exist only for it; MySQL is one database and redis has no schemas at all. */
function dbIsPg()          {
  const d = dbConn();
  const c = d.conns.find((x                    )          => { return x.name === d.conn; });
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
      if (!alt.includes("*")) return n.includes(alt);
      const re = "^" + alt.split("*").map((p        )         => {
        return p.replace(/[.+?^\[\]{}()\\\-|]/g, "\\$&");
      }).join(".*") + "$";
      return new RegExp(re).test(n);
    });
  });
}

function renderDbTables()       {
  const d = dbConn();
  const box = $("dbTables");
  if (!box) return;
  box.textContent = "";
  dbPaintSchemaOptions();
  if (!d.conn) {
    box.appendChild(el("div", "db-hint", tr("dataView.addMysqlPgMcpHint")));
    return;
  }
  if (dbIsRedis()) {
    const rr = d.redis;
    if (!rr || !rr.keys.length) {
      // docs/22 closeout B1: a failed scan is a FAILURE, not an empty keyspace — the toast
      // carries the server's own text; this row keeps the list from pretending otherwise.
      box.appendChild(el("div", "db-hint", d.redisError
        ? tr("dataView.scanFailedToastCarries")
        : d.grep ? tr("dataView.keysMatchQ", { q: d.grep }) : tr("dataView.keysScanReturnedNone")));
    }
    // The live key tab holds the selection: a redis connection's single tab is kind
    // "key" (docs/42 T1), and a fresh one with no key opened selects nothing.
    const kt = dbTab();
    const selKey = kt.kind === "key" ? kt.redisKey : null;
    const sortedKeys = (rr ? rr.keys : []).slice().sort(dbRedisCompare);
    sortedKeys.forEach((k                  )       => {
      const b = el("button", "db-table" + (k.key === selKey ? " sel" : ""));
      b.title = k.key; // the row truncates with an ellipsis; the full key is one hover away
      // docs/37 R5: the key itself is the address — the delegated click (dbChromeClick)
      // re-reads the live selection and the drop guard at event time.
      b.dataset.rkey = k.key;
      b.appendChild(el("div", "db-table-name", k.key));
      let meta = k.type;
      if (k.ttl  >= 0) meta += " · ttl " + k.ttl + "s";
      b.appendChild(el("div", "db-table-meta", meta));
      box.appendChild(b);
    });
    const foot2 = $("dbTablesPager");
    if (foot2) {
      foot2.textContent = "";
      const shown = rr ? rr.keys.length : 0;
      foot2.appendChild(el("span", "",
        rr && rr.total != null
          ? tr("dataView.keysShownOfTotal", { shown: shown.toLocaleString(locale()), total: Number(rr.total).toLocaleString(locale()) })
          : trn(shown, "dataView.nKeys.one", "dataView.nKeys.other", { n: shown.toLocaleString(locale()) })));
      if (rr && !rr.done) {
        const more = el("button", "btn", tr("dataView.more"));
        more.title = tr("dataView.continueScan");
        more.dataset.keysmore = "";
        foot2.appendChild(more);
      }
    }
    return;
  }
  if (!d.tables.length) {
    box.appendChild(el("div", "db-hint", d.grep ? tr("dataView.noTablesMatchQ", { q: d.grep }) : tr("dataView.noTables")));
  }
  // docs/22 W1.1: a Postgres catalog is many schemas, so the page renders grouped — the docs/20
  // §4 container vocabulary (band header, mixed-case name, tnum count, indented body behind the
  // guide line). MySQL is one database: the flat list, no headers, visually exactly as before.
  if (dbIsPg() && d.tables.length) {
    const schemas           = [];
    d.tables.forEach((t               )       => {
      if (!schemas.includes(t.schema)) schemas.push(t.schema);
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
    d.tables.forEach((t               )       => { box.appendChild(dbTableRow(t)); });
  }
  const foot = $("dbTablesPager");
  if (!foot) return;
  foot.textContent = "";
  const from = d.tablesTotal ? d.tablesPage * d.tablesLimit + 1 : 0;
  const to = d.tablesPage * d.tablesLimit + d.tables.length;
  foot.appendChild(el("span", "", tr("dataView.tablesRangeOfTotal", {
    from: from.toLocaleString(locale()), to: to.toLocaleString(locale()), total: d.tablesTotal.toLocaleString(locale()),
  })));
  // docs/37 R5: the pager rides the delegated click (dbChromeClick's [data-tpg]) — the
  // page counter it moves is read from dbConn() at event time, not from this render.
  const prev = el("button", "btn icon")                     ;
  prev.appendChild(iconNode("chevron-left"));
  prev.title = tr("dataView.prevTablesPage");
  prev.disabled = d.tablesPage === 0;
  prev.dataset.tpg = "prev";
  const next = el("button", "btn icon")                     ;
  next.appendChild(iconNode("chevron-right"));
  next.title = tr("dataView.nextTablesPage");
  next.disabled = !d.more;
  next.dataset.tpg = "next";
  foot.appendChild(prev);
  foot.appendChild(next);
}

/** One table row, shared by the flat list and every schema group. docs/37 R5: the name and
 *  schema are the address — the delegated click (dbChromeClick's [data-tname]) re-finds the
 *  row in the live list and hands it to dbOpenTab, whose dedupe and cap are re-read at event
 *  time. */
function dbTableRow(t               )              {
  const ct = dbTab();
  const sel = ct.kind === "table" && t.name === ct.table && t.schema === ct.schema;
  const b = el("button", "db-table" + (sel ? " sel" : ""));
  b.title = t.name;
  b.dataset.tname = t.name;
  b.dataset.tschema = t.schema || "";
  b.appendChild(el("div", "db-table-name", t.name));
  b.appendChild(el("div", "db-table-meta",
    t.type + (t.approxRows != null ? " · " + tr("dataView.approxRows", { n: Number(t.approxRows).toLocaleString(locale()) }) : "") +
    (t.size ? " · " + t.size : "")));
  return b;
}

/** The schema picker above the grep box (pg only): "All schemas" plus one option per schema
 *  on the current page, counts from the page the list is showing. A schema picked on an earlier
 *  page stays selectable even when this page does not carry it.
 *  docs/22 closeout B7: a non-pg connection carries NO picker — the select leaves the DOM,
 *  not hidden-with-options. A hidden native select still surfaces in automation accessibility
 *  trees as a live "Schema" button holding the previous pg connection's pick, which read as a
 *  redis page offering a schema dropdown; hidden options are state residue even unseen. */
function dbPaintSchemaOptions()       {
  const d = dbConn();
  let sel = $                   ("dbSchema");
  if (!d.conn || !dbIsPg()) {
    if (sel) sel.remove();
    return;
  }
  if (!sel) {
    // Coming back to pg after a non-pg connection removed it: re-create it in the sidebar,
    // right after the connection picker. docs/37 R5: no wiring to carry — the pane's
    // delegated change listener finds the select by id at event time.
    sel = el("select")                     ;
    sel.id = "dbSchema";
    sel.setAttribute("aria-label", tr("dataView.schema"));
    const conn = $("dbConn");
    if (conn && conn.parentNode) conn.parentNode.insertBefore(sel, conn.nextSibling);
  }
  sel.hidden = false;
  const schemas           = [];
  d.tables.forEach((t               )       => {
    if (!schemas.includes(t.schema)) schemas.push(t.schema);
  });
  const current = d.schemaFilter || "";
  if (current && !schemas.includes(current)) schemas.push(current);
  schemas.sort();
  sel.textContent = "";
  const all = el("option", "", tr("dataView.allSchemas"))                     ;
  all.value = "";
  all.selected = current === "";
  sel.appendChild(all);
  schemas.forEach((s        )       => {
    const n = d.tables.filter((t               )          => { return t.schema === s; }).length;
    const o = el("option", "", s + " (" + n + ")")                     ;
    o.value = s;
    o.selected = s === current;
    sel.appendChild(o);
  });
}

/** The selected connection's dialect word ("mysql" | "pg") — the DDL sheets build their
 *  type suggestions and titles on it (docs/22 W4.6). */
function dbDialectOf()         {
  const d = dbConn();
  const c = d.conns.find((x                    )          => { return x.name === d.conn; });
  return (c && c.dialect) || "mysql";
}

/** Every schema the loaded pages have shown, "public" always among them — the New table
 *  sheet's where-it-goes select (docs/22 W4.6; swiss-ui-design rule 6). */
function dbKnownSchemas()           {
  const d = dbConn();
  const out           = [];
  d.tables.forEach((t               )       => {
    if (t.schema && !out.includes(t.schema)) out.push(t.schema);
  });
  if (!out.includes("public")) out.unshift("public");
  out.sort();
  return out;
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
function dbFocusedColumnValue(ct            , column        )          {
  if (!ct.focus || !ct.data) return undefined;
  /* Straight-line flow after the guard: ct, ct.focus and ct.data are all narrowed here, so
   * the reads below need no assertions at all (docs/37 R2). */
  if (ct.focus.r < ct.inserts.length) return ct.inserts[ct.focus.r].values[column];
  const row = ct.data.rows[ct.focus.r - ct.inserts.length];
  return row ? row[column] : undefined;
}

/** Open the referenced table with the filter preset (adminer's select-link, dbgate's
 *  openReferenceForm — both land on the target table already filtered to one row).
 *
 *  docs/22 W5.2, finally right: the jump opens a NEW tab. It used to overwrite the tab the
 *  operator was reading — its filters, its page, its buffered edits all dropped to show them
 *  the one referenced row — which made the feature something people learned not to click. The
 *  source tab is untouched now and one click on the strip is the way back. */
function dbFkOpen(fk            , value         )       {
  const j = dbFkJump(fk, value);
  if (!j) return;
  dbOpenTab({ kind: "table", table: j.table, schema: j.schema, filters: j.filters });
}

export { DB_HISTORY_KEY, DB_HISTORY_MAX, DB_PAGE_SIZES, dbClearSel, dbConnLabel, dbDialectOf, dbDropEdits, dbFilterMatches, dbFocusedColumnValue, dbFkJump, dbFkOpen, dbIsPg, dbKnownSchemas, dbLoadTables, dbOkToDrop, dbOkToLeave, dbPaneChange, dbPaneClick, dbPaneInput, dbPaneKeydown, dbPending, dbPendingAll, dbPkKey, dbPkVals, dbResultKey, loadDbView, renderDbSide, renderDbTables, renderDbView };

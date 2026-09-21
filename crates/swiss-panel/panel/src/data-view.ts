import type { DbState } from "./db-state.js";
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

import type { ApiDbConnectionRow, ApiDbFkRow, ApiDbRedisKeyRow, ApiDbTableRow, ApiDbTablesResponse } from "./types/api.js";
import type { DbFilterTerm } from "./types/state.js";
import { $, apiJson, dbReqGuard, el, iconNode, targetEl } from "./util.js";
import { fill, h } from "./h.js";
import { currentPageCount } from "./page-registry.js";
import { dbIsRedis, dbLoadKeys, dbLoadRedisValue, dbRedisClick, dbRedisKeydown, dbRedisPendingCount } from "./data-browsers.js";
import { dbFiltersChange, dbFiltersClick, dbFiltersInput, dbFiltersKeydown, dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbGridChange, dbGridClick, dbGridKeydown, dbLoadData, dbToolbarClick, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbBarClick, dbFavLoad, dbFavPush, dbFormatSql, dbHistoryLoad, dbHistoryRender, dbRunSql, renderDbBar } from "./data-sql.js";
import { dbActivityClick, dbActivityPane, dbActivityPollStop } from "./data-activity.js";
// dbLoadDetail (the Structure tabs' loader) is needed at the table-open path now: the FK
// jump (docs/22 W5.2) reads the detail's foreignKeys. The cycle data-view <-> data-structure
// is the same accepted shape as data-grid <-> data-cell — both sides only call across it
// inside functions, never at module scope.
import { dbLoadDetail, dbStructureClick } from "./data-structure.js";
// The pane's delegated listeners chain the form view's dispatchers too. data-form already
// reaches back into data-view for dbPkKey — the same accepted cycle shape as the
// data-structure edge above: both sides only call across it inside functions.
import { dbFormChange, dbFormClick, dbFormKeydown } from "./data-form.js";
import { openDbDdlSheet } from "./data-ddl.js";
import { dbSuggestHide, dbSuggestKeys, dbSuggestOnInput } from "./data-suggest.js";
import { popupMenu } from "./menu.js";
import { dbIsMounted, dbView, mountDbView } from "./db-state.js";
import { tr, trn } from "./i18n.js";

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


function dbPending(): number {
  const d = dbView();
  return Object.keys(d.updates).length + Object.keys(d.deletes).length + d.inserts.length;
}

/** Ask before an action would drop buffered edits; false when the user said no. The redis
 *  value buffer counts too (docs/22 W3.3) — a key switch that ate buffered fields silently
 *  would break the same rule the row grid's guard exists for. */
function dbOkToDrop(): boolean {
  const n = dbPending() + dbRedisPendingCount();
  return !n || confirm(tr("dataView.discardNothingWritten",
    { n: trn(n, "dataView.nUncommittedChanges.one", "dataView.nUncommittedChanges.other") }));
}

function dbDropEdits() {
  const d = dbView();
  d.updates = {}; d.deletes = {}; d.inserts = [];
  dbClearSel(); // selection addresses rows of the OLD page — it never survives a reload
  d.sqlPreview = false;
}

/** Row selection is scoped to what is on screen: cleared whenever the grid identity changes. */
function dbClearSel(): void {
  const d = dbView();
  d.sel = {};
  d.selAnchor = -1;
}

function dbPkKey(pkCols: string[], row: Record<string, unknown>): string {
  return JSON.stringify(pkCols.map((c: string): unknown => { return row[c]; }));
}

function dbPkVals(pkCols: string[], row: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  pkCols.forEach((c: string): void => { out[c] = row[c]; });
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
  // The entry choke point. It used to be the place a freed record was rebuilt, and getting
  // that wrong killed every second entry halfway through renderDbView's wiring; db-state.ts
  // owns the record now and it is never absent, so all this still marks is that the view is
  // on screen — which the late callbacks below ask about by name.
  mountDbView();
  renderDbView();
  const j = await apiJson<{ connections: ApiDbConnectionRow[] }>("/api/db");
  if (!j) return;
  const d = dbView();
  d.conns = j.connections || [];
  const stillThere = d.conns.some((c: ApiDbConnectionRow): boolean => { return c.name === d.conn; });
  if (!stillThere) {
    d.conn = d.conns.length ? d.conns[0].name : null;
    d.table = null; d.schema = null; d.data = null; d.tables = [];
    d.redis = null; d.redisKey = null; d.redisValue = null;
    d.redisEdits = null;
    d.sqlResult = null;
    d.sqlResults = null; // docs/22 W4.3: the tab strip goes with the result it named
    d.sqlTab = 0;
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
  if (d.conn && dbIsRedis()) { if (!d.redis) void dbLoadKeys(true); else renderDbTables(); }
  else if (d.conn) { if (!d.tables.length) void dbLoadTables(); else renderDbTables(); }
  else renderDbTables();
  if (d.table && !d.data && !dbIsRedis()) void dbLoadData(true);
}

function renderDbView(): void {
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
      h("div", { class: "db-headrow" },
        h("div", { class: "db-head", id: "dbHead" }),
        // Pane-level actions live behind one ⋯ next to the head (docs/22 W3.2); the Activity
        // monitor is the first. Hidden until a SQL connection exists — redis has no sessions.
        h("button", { class: "btn icon", id: "dbMore", type: "button", title: tr("dataView.morePaneActions"), hidden: true }, iconNode("ellipsis"))),
      h("div", { class: "db-filters", id: "dbFilters" }),
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
  const db_ = dbView();
  $<HTMLInputElement>("dbGrep").value = db_.grep || "";
  $<HTMLTextAreaElement>("dbSql").value = db_.sqlText;
  // The console keeps two direct hooks that delegation cannot express: onscroll never
  // bubbles (the highlight layer must mirror the textarea's scroll), and the suggest keydown
  // must run at the TARGET so it can consume the keys the open list owns before anything
  // else answers. Everything else — input, Ctrl+Enter — rides the pane's delegated listeners.
  $<HTMLTextAreaElement>("dbSql").onscroll = (e) => {
    const t = e.currentTarget as HTMLTextAreaElement;
    const hl = $("dbSqlHl");
    if (hl) { hl.scrollTop = t.scrollTop; hl.scrollLeft = t.scrollLeft; }
    dbSuggestHide(); // the caret's point scrolled with the text; a list pinned to stale
    // coordinates would point at the wrong word. The next keystroke reopens it in place.
  };
  $<HTMLTextAreaElement>("dbSql").addEventListener("blur", dbSuggestHide);
  $<HTMLTextAreaElement>("dbSql").addEventListener("keydown", dbSuggestKeys);
  dbSqlPaint();
  dbSyncKind();
  dbHistoryLoad();
  dbFavLoad();
  dbHistoryRender();
  renderDbToolbar(); renderDbGrid(); renderDbBar();
  // The section page replaces the pane's content when (re)entered with it open — closing is
  // just another rebuild with the flag down, so nothing here needs a special restore path.
  if (db_.activity && db_.conn && !dbIsRedis()) dbActivityPane(dbActivityClose);
}

/* --- #pane's four delegated listeners (docs/37 R5) ------------------------------------------------
   The pane this view owns answers every click/change/input/keydown in it. The chrome
   dispatcher below handles the sidebar and console; each section module exports its own
   dispatcher and the chain calls them in order — selectors are disjoint, and the first
   match wins. Behavior note (docs/37 §10.1): all state is resolved at EVENT time through
   dbView(), never captured at render time. */

// The sidebar grep's debounce: one module-level timer, restarted per keystroke.
let dbGrepTimer: ReturnType<typeof setTimeout> | undefined;

function dbPaneClick(ev: MouseEvent): void {
  const t = targetEl(ev);
  if (!t) return;
  // Grid-internal clicks master stopped AT the node (data-grid.js: every selection checkbox
  // cb/cbAll/cb2/cbAll2 and the column grip carried a bare stopPropagation): connect.ts's
  // document listener closes any open Export/Explain/#dbMore menu over a click that reaches
  // it, and a selection click or a grip grab is not an "outside the menu" click. The
  // checkbox acts on change and the grip on mousedown, so stopping here is the whole click
  // behavior — no dispatcher follows, exactly as master's node handlers did nothing on click.
  if (t.closest(".db-selbox, .db-col-grip")) { ev.stopPropagation(); return; }
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

function dbPaneInput(ev: Event): void {
  const t = targetEl(ev);
  if (!t) return;
  if (dbChromeInput(t)) return;
  if (dbFiltersInput(t)) return;
}

function dbPaneChange(ev: Event): void {
  const t = targetEl(ev);
  if (!t) return;
  if (dbChromeChange(t)) return;
  if (dbGridChange(t, ev)) return;
  if (dbFiltersChange(t)) return;
  if (dbFormChange(t)) return;
}

function dbPaneKeydown(ev: KeyboardEvent): void {
  const t = targetEl(ev);
  if (!t) return;
  if (dbGridKeydown(t, ev)) return;
  if (dbChromeKeydown(t, ev)) return;
  if (dbFiltersKeydown(ev, t)) return;
  if (dbFormKeydown(t, ev)) return;
  if (dbRedisKeydown(t, ev)) return;
}

/** The sidebar + console half of the delegated click. */
function dbChromeClick(t: Element, ev: MouseEvent): boolean {
  if (t.closest("#dbSortDir")) {
    const d = dbView();
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
  const explainBtn = t.closest<HTMLElement>("#dbSqlExplain");
  if (explainBtn) {
    // stopPropagation: connect.js closes any open menu on clicks that reach document, and
    // without it the click that opens the menu also tears it down (same as the Export menu).
    ev.stopPropagation();
    popupMenu(explainBtn.getBoundingClientRect(), [
      { label: tr("dataView.explain"), fn: (): void => { void dbRunSql("plan"); } },
      { label: tr("dataView.explainAnalyze"), fn: (): void => { void dbRunSql("analyze"); } },
    ]);
    return true;
  }
  if (t.closest("#dbSqlFav")) { dbFavPush(dbView().sqlText); return true; }
  if (t.closest("#dbSqlFormat")) {
    const d = dbView();
    if (!d.sqlText || !d.sqlText.trim()) return true;
    d.sqlText = dbFormatSql(d.sqlText);
    const ta = $<HTMLTextAreaElement>("dbSql");
    if (ta) { ta.value = d.sqlText; dbSqlPaint(); ta.focus(); }
    return true;
  }
  if (t.closest("#dbNewTable")) {
    const d = dbView();
    if (!d.conn || dbIsRedis()) return true;
    openDbDdlSheet("table", {
      dialect: dbDialectOf(),
      conn: d.conn,
      schema: dbIsPg() ? (d.schemaFilter || "public") : "",
      schemas: dbIsPg() ? dbKnownSchemas() : [],
    });
    return true;
  }
  const moreBtn = t.closest<HTMLElement>("#dbMore");
  if (moreBtn) {
    // stopPropagation: the document click closes popup menus — the opening click must not.
    ev.stopPropagation();
    const d = dbView();
    popupMenu(moreBtn.getBoundingClientRect(), [
      { label: d.activity ? tr("dataView.closeActivity") : tr("dataView.activity"), fn: dbActivityToggle },
    ]);
    return true;
  }
  // The redis key list: the row carries the key; the live selection decides the guard.
  const keyRow = t.closest<HTMLElement>("[data-rkey]");
  if (keyRow) {
    const d = dbView();
    const key = keyRow.dataset.rkey!;
    // Switching keys drops the typed-value buffer (docs/22 W3.3): a different key cannot
    // adopt another key's fields, so the same guard the table switch uses asks first.
    if (key !== d.redisKey && dbOkToDrop()) void dbLoadRedisValue(key);
    return true;
  }
  // A table row: re-find the row in the live list by name+schema and open it.
  const tblRow = t.closest<HTMLElement>("[data-tname]");
  if (tblRow) {
    const d = dbView();
    const name = tblRow.dataset.tname!;
    const schema = tblRow.dataset.tschema || null;
    const row = d.tables.find((x: ApiDbTableRow): boolean => { return x.name === name && (x.schema || null) === schema; });
    if (row) dbOpenTable(row);
    return true;
  }
  if (t.closest("[data-keysmore]")) { void dbLoadKeys(false); return true; }
  const tpg = t.closest<HTMLElement>("[data-tpg]");
  if (tpg) {
    const d = dbView();
    if (tpg.dataset.tpg === "prev") d.tablesPage--;
    else d.tablesPage++;
    void dbLoadTables();
    return true;
  }
  return false;
}

function dbChromeInput(t: Element): boolean {
  const grep = t.closest<HTMLInputElement>("#dbGrep");
  if (grep) {
    const v = grep.value;
    clearTimeout(dbGrepTimer);
    dbGrepTimer = setTimeout((): void => {
      // A debounce pending across an unmount once threw here. The view is gone — the filter
      // belongs to no one, and writing it into the fresh record would be a lie either way.
      if (!dbIsMounted()) return;
      const d = dbView();
      d.grep = v; d.tablesPage = 0;
      if (dbIsRedis()) void dbLoadKeys(true);
      else void dbLoadTables();
    }, 300);
    return true;
  }
  const ta = t.closest<HTMLTextAreaElement>("#dbSql");
  if (ta) {
    dbView().sqlText = ta.value;
    dbSqlPaint();
    dbSuggestOnInput.call(ta);
    return true;
  }
  return false;
}

function dbChromeChange(t: Element): boolean {
  const connSel = t.closest<HTMLSelectElement>("#dbConn");
  if (connSel) {
    const db_ = dbView();
    if (connSel.value === db_.conn) return true;
    if (!dbOkToDrop()) { connSel.value = db_.conn || ""; return true; }
    // The Activity page belongs to ONE connection's server; the switch leaves it behind —
    // restore the normal pane (and stop its poll) before the state it reads changes.
    if (db_.activity) dbActivityClose();
    const d = dbView();
    d.conn = connSel.value; d.table = null; d.schema = null; d.data = null;
    d.tables = []; d.tablesPage = 0; d.order = null; d.sqlResult = null;
    d.sqlResults = null; d.sqlTab = 0; // docs/22 W4.3: no stale tabs across a connection switch
    d.schemaFilter = ""; // a schema pick was made against the other connection's catalog
    d.redis = null; d.redisKey = null; d.redisValue = null;
    d.redisEdits = null;
    d.filters = [];
    d.sort = "name"; d.sortDir = "asc"; // the new connection's kind may not have the chosen key
    // A sidebar search is table-list-scoped: carrying "tsys_" from one connection into the next
    // silently filters the new list down to nothing. Reset it and the box that shows it.
    d.grep = "";
    d.redisType = ""; // same reasoning: a type filter is chosen against a key list, not inherited
    const gb = $<HTMLInputElement>("dbGrep");
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
    return true;
  }
  // The schema picker (pg only): a pick re-requests the table list inside that schema. The
  // select is removed and re-created by dbPaintSchemaOptions — delegation by id needs no
  // per-creation wiring, which is why dbWireSchemaSelect is gone (docs/37 R5).
  const schemaSel = t.closest<HTMLSelectElement>("#dbSchema");
  if (schemaSel) {
    const d = dbView();
    if (schemaSel.value === d.schemaFilter) return true;
    d.schemaFilter = schemaSel.value;
    d.tablesPage = 0;
    void dbLoadTables();
    return true;
  }
  const sortSel = t.closest<HTMLSelectElement>("#dbSort");
  if (sortSel) {
    const d = dbView();
    d.sort = sortSel.value;
    d.tablesPage = 0;
    if (dbIsRedis()) renderDbTables(); // keys sort in place over what has been scanned
    else void dbLoadTables();
    return true;
  }
  const hist = t.closest<HTMLSelectElement>("#dbSqlHistory");
  if (hist) {
    if (hist.value === "") return true;
    // docs/22 W5.4: "f"+i is a favorite, a plain index history — both land in the console.
    const fav = hist.value.charAt(0) === "f";
    const db_ = dbView();
    const sql = fav ? db_.favorites?.[Number(hist.value.slice(1))]
      : db_.history?.[Number(hist.value)];
    hist.value = ""; // back to the label, so the same entry can be picked again
    if (sql == null) return true;
    db_.sqlText = sql;
    const ta = $<HTMLTextAreaElement>("dbSql");
    if (ta) { ta.value = sql; dbSqlPaint(); ta.focus(); }
    return true;
  }
  return false;
}

function dbChromeKeydown(t: Element, ev: KeyboardEvent): boolean {
  if (!t.closest("#dbSql")) return false;
  if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
    ev.preventDefault();
    void dbRunSql();
  }
  return true;
}

/* The Activity monitor (docs/22 W3.2): one flag drives it. Opening rebuilds the pane with the
   section in place; closing rebuilds it back — the same renderDbView that drew it. */
function dbActivityToggle(): void {
  const d = dbView();
  if (d.activity) { dbActivityClose(); return; }
  if (!d.conn || dbIsRedis()) return;
  d.activity = true;
  renderDbView();
}

function dbActivityClose(): void {
  const d = dbView();
  d.activity = false;
  d.activityRows = null;
  dbActivityPollStop();
  renderDbView();
}

/* The list's sort row. SQL connections sort server-side (name/rows/size against the catalog);
 * redis sorts client-side over the keys SCAN has already handed over (key/TTL). One control,
 * one direction button; the option set repaints when the connection kind changes. */
function dbSortOptions(): { v: string; t: string }[] {
  return dbIsRedis()
    ? [{ v: "name", t: tr("dataView.sortKey") }, { v: "ttl", t: "TTL" }]
    : [{ v: "name", t: tr("dataView.sortName") }, { v: "rows", t: tr("dataView.sortRows") }, { v: "size", t: tr("dataView.sortSize") }];
}

function dbPaintSort(): void {
  const d = dbView();
  const sel = $<HTMLSelectElement>("dbSort"), dir = $("dbSortDir");
  if (!sel || !dir) return;
  const opts = dbSortOptions();
  if (!opts.some((o: { v: string; t: string }): boolean => { return o.v === d.sort; })) d.sort = opts[0].v; // kind switched
  sel.textContent = "";
  opts.forEach((o: { v: string; t: string }): void => {
    const op = el("option", "", o.t) as HTMLOptionElement;
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
function dbRedisCompare(a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow): number {
  const d = dbView();
  if (d.sort === "ttl") {
    const ta: number = a.ttl! < 0 ? Infinity : a.ttl!;
    const tb: number = b.ttl! < 0 ? Infinity : b.ttl!;
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
function dbSyncKind(): void {
  const grep = $<HTMLInputElement>("dbGrep"), sql = $<HTMLTextAreaElement>("dbSql"), explain = $("dbSqlExplain"), hint = $("dbSqlHint");
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
  const d = dbView();
  if (more) more.hidden = !d.conn || dbIsRedis();
  const listHead = $("dbListHead");
  if (listHead) listHead.hidden = !d.conn || dbIsRedis();
}

/** The one label a connection goes by — the sidebar dropdown's option text. The page-bar
 *  count chip (views/data.js countText) shows the SAME string, so the bar and the control
 *  that changes it can never disagree; two builders is how the bar ended up saying "Data"
 *  twice while the dropdown said "shop-redis · redis". */
function dbConnLabel(c: ApiDbConnectionRow): string {
  return c.name + " · " + c.dialect;
}

function renderDbSide(): void {
  const d = dbView();
  const sel = $<HTMLSelectElement>("dbConn");
  if (!sel) return;
  sel.textContent = "";
  if (!d.conns.length) {
    const none = el("option", "", tr("dataView.noDatabaseMcps")) as HTMLOptionElement;
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
  const order: string[] = [];
  const buckets: Record<string, ApiDbConnectionRow[]> = {};
  d.conns.forEach((c: ApiDbConnectionRow): void => {
    const g = c.group || "default";
    if (!buckets[g]) { buckets[g] = []; order.push(g); }
    buckets[g].push(c);
  });
  const addOption = (parent: HTMLElement, c: ApiDbConnectionRow): void => {
    const o = el("option", "", dbConnLabel(c)) as HTMLOptionElement;
    o.value = c.name;
    o.selected = c.name === d.conn;
    parent.appendChild(o);
  };
  if (order.length < 2) {
    d.conns.forEach((c: ApiDbConnectionRow): void => { addOption(sel, c); });
    return;
  }
  order.forEach((g: string): void => {
    const og = el("optgroup") as HTMLOptGroupElement;
    og.label = g;
    buckets[g].forEach((c: ApiDbConnectionRow): void => { addOption(og, c); });
    sel.appendChild(og);
  });
}

/* --- lazy table list ---------------------------------------------------------------------------- */

// One /tables request chain: a slow answer for a page or filter the user just left must be
// dropped, or it would repopulate the sidebar with the stale page (docs/22 closeout audit).
const dbTablesReq = dbReqGuard();

async function dbLoadTables(): Promise<void> {
  const d = dbView();
  if (!d.conn) return;
  const box = $("dbTables");
  if (box) { box.textContent = ""; box.appendChild(el("div", "db-hint", tr("dataView.loading"))); }
  let q = "/api/db/" + encodeURIComponent(d.conn) + "/tables?page=" + d.tablesPage;
  if (d.grep) q += "&grep=" + encodeURIComponent(d.grep);
  if (d.schemaFilter) q += "&schema=" + encodeURIComponent(d.schemaFilter);
  if (d.sort) q += "&sort=" + encodeURIComponent(d.sort) + "&dir=" + encodeURIComponent(d.sortDir || "asc");
  const token = dbTablesReq.issue();
  const j = await apiJson<ApiDbTablesResponse>(q);
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
function dbIsPg(): boolean {
  const d = dbView();
  const c = d.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === d.conn; });
  return !!c && c.dialect === "pg";
}

/* The sidebar grep grammar (docs/22 W1.6): comma-separated terms AND together, "|" inside a
   term is OR, "*" is a wildcard, everything case-insensitive. A bare term with no "*" keeps
   the substring behaviour the list always had. Pure so the grammar can be pinned without a
   DOM; the server's table grep speaks the same language in SQL. */
function dbFilterMatches(tokens: string, name: unknown): boolean {
  const n = String(name).toLowerCase();
  const terms = String(tokens).toLowerCase().split(",")
    .map((s: string): string => { return s.trim(); }).filter(Boolean);
  if (!terms.length) return true;
  return terms.every((term: string): boolean => {
    return term.split("|").some((alt: string): boolean => {
      alt = alt.trim();
      if (!alt) return false;
      if (!alt.includes("*")) return n.includes(alt);
      const re = "^" + alt.split("*").map((p: string): string => {
        return p.replace(/[.+?^\[\]{}()\\\-|]/g, "\\$&");
      }).join(".*") + "$";
      return new RegExp(re).test(n);
    });
  });
}

function renderDbTables(): void {
  const d = dbView();
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
    const sortedKeys = (rr ? rr.keys : []).slice().sort(dbRedisCompare);
    sortedKeys.forEach((k: ApiDbRedisKeyRow): void => {
      const b = el("button", "db-table" + (k.key === d.redisKey ? " sel" : ""));
      b.title = k.key; // the row truncates with an ellipsis; the full key is one hover away
      // docs/37 R5: the key itself is the address — the delegated click (dbChromeClick)
      // re-reads the live selection and the drop guard at event time.
      b.dataset.rkey = k.key;
      b.appendChild(el("div", "db-table-name", k.key));
      let meta = k.type;
      if (k.ttl! >= 0) meta += " · ttl " + k.ttl + "s";
      b.appendChild(el("div", "db-table-meta", meta));
      box.appendChild(b);
    });
    const foot2 = $("dbTablesPager");
    if (foot2) {
      foot2.textContent = "";
      const shown = rr ? rr.keys.length : 0;
      foot2.appendChild(el("span", "",
        rr && rr.total != null
          ? tr("dataView.keysShownOfTotal", { shown: shown.toLocaleString(), total: Number(rr.total).toLocaleString() })
          : trn(shown, "dataView.nKeys.one", "dataView.nKeys.other", { n: shown.toLocaleString() })));
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
    const schemas: string[] = [];
    d.tables.forEach((t: ApiDbTableRow): void => {
      if (!schemas.includes(t.schema)) schemas.push(t.schema);
    });
    schemas.forEach((s: string): void => {
      const rows = d!.tables.filter((t: ApiDbTableRow): boolean => { return t.schema === s; });
      const g = el("div", "grp grp--side");
      const head = el("div", "grp-head");
      head.appendChild(el("span", "grp-toggle", s));
      head.appendChild(el("span", "grp-n", String(rows.length)));
      g.appendChild(head);
      const body = el("div", "grp-body");
      rows.forEach((t: ApiDbTableRow): void => { body.appendChild(dbTableRow(t)); });
      g.appendChild(body);
      box.appendChild(g);
    });
  } else {
    d.tables.forEach((t: ApiDbTableRow): void => { box.appendChild(dbTableRow(t)); });
  }
  const foot = $("dbTablesPager");
  if (!foot) return;
  foot.textContent = "";
  const from = d.tablesTotal ? d.tablesPage * d.tablesLimit + 1 : 0;
  const to = d.tablesPage * d.tablesLimit + d.tables.length;
  foot.appendChild(el("span", "", tr("dataView.tablesRangeOfTotal", {
    from: from.toLocaleString(), to: to.toLocaleString(), total: d.tablesTotal.toLocaleString(),
  })));
  // docs/37 R5: the pager rides the delegated click (dbChromeClick's [data-tpg]) — the
  // page counter it moves is read from dbView() at event time, not from this render.
  const prev = el("button", "btn icon") as HTMLButtonElement;
  prev.appendChild(iconNode("chevron-left"));
  prev.title = tr("dataView.prevTablesPage");
  prev.disabled = d.tablesPage === 0;
  prev.dataset.tpg = "prev";
  const next = el("button", "btn icon") as HTMLButtonElement;
  next.appendChild(iconNode("chevron-right"));
  next.title = tr("dataView.nextTablesPage");
  next.disabled = !d.more;
  next.dataset.tpg = "next";
  foot.appendChild(prev);
  foot.appendChild(next);
}

/** One table row, shared by the flat list and every schema group. docs/37 R5: the name and
 *  schema are the address — the delegated click (dbChromeClick's [data-tname]) re-finds the
 *  row in the live list and runs dbOpenTable, whose same-table short-circuit and drop guard
 *  still hold at event time. */
function dbTableRow(t: ApiDbTableRow): HTMLElement {
  const d = dbView();
  const b = el("button", "db-table" + (t.name === d.table && t.schema === d.schema ? " sel" : ""));
  b.title = t.name;
  b.dataset.tname = t.name;
  b.dataset.tschema = t.schema || "";
  b.appendChild(el("div", "db-table-name", t.name));
  b.appendChild(el("div", "db-table-meta",
    t.type + (t.approxRows != null ? " · " + tr("dataView.approxRows", { n: Number(t.approxRows).toLocaleString() }) : "") +
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
function dbPaintSchemaOptions(): void {
  const d = dbView();
  let sel = $<HTMLSelectElement>("dbSchema");
  if (!d.conn || !dbIsPg()) {
    if (sel) sel.remove();
    return;
  }
  if (!sel) {
    // Coming back to pg after a non-pg connection removed it: re-create it in the sidebar,
    // right after the connection picker. docs/37 R5: no wiring to carry — the pane's
    // delegated change listener finds the select by id at event time.
    sel = el("select") as HTMLSelectElement;
    sel.id = "dbSchema";
    sel.setAttribute("aria-label", tr("dataView.schema"));
    const conn = $("dbConn");
    if (conn && conn.parentNode) conn.parentNode.insertBefore(sel, conn.nextSibling);
  }
  sel.hidden = false;
  const schemas: string[] = [];
  d.tables.forEach((t: ApiDbTableRow): void => {
    if (!schemas.includes(t.schema)) schemas.push(t.schema);
  });
  const current = d.schemaFilter || "";
  if (current && !schemas.includes(current)) schemas.push(current);
  schemas.sort();
  sel.textContent = "";
  const all = el("option", "", tr("dataView.allSchemas")) as HTMLOptionElement;
  all.value = "";
  all.selected = current === "";
  sel.appendChild(all);
  schemas.forEach((s: string): void => {
    const n = d.tables.filter((t: ApiDbTableRow): boolean => { return t.schema === s; }).length;
    const o = el("option", "", s + " (" + n + ")") as HTMLOptionElement;
    o.value = s;
    o.selected = s === current;
    sel.appendChild(o);
  });
}

/** The selected connection's dialect word ("mysql" | "pg") — the DDL sheets build their
 *  type suggestions and titles on it (docs/22 W4.6). */
function dbDialectOf(): string {
  const d = dbView();
  const c = d.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === d.conn; });
  return (c && c.dialect) || "mysql";
}

/** Every schema the loaded pages have shown, "public" always among them — the New table
 *  sheet's where-it-goes select (docs/22 W4.6; swiss-ui-design rule 6). */
function dbKnownSchemas(): string[] {
  const d = dbView();
  const out: string[] = [];
  d.tables.forEach((t: ApiDbTableRow): void => {
    if (t.schema && !out.includes(t.schema)) out.push(t.schema);
  });
  if (!out.includes("public")) out.unshift("public");
  out.sort();
  return out;
}

/* The open-target the table list, the DDL sheet and the FK jump all hand in: a name plus
   its schema (null when the caller has none - the FK jump's same-schema case). */
function dbOpenTable(t: { name: string; schema: string | null }): void {
  const d = dbView();
  if (t.name === d.table && t.schema === d.schema) return;
  if (!dbOkToDrop()) return;
  d.table = t.name; d.schema = t.schema;
  d.offset = 0; d.order = null; d.dir = "asc"; d.filters = [];
  d.tab = "data"; d.detail = null;
  d.sqlResult = null;
  d.sqlResults = null; d.sqlTab = 0; // docs/22 W4.3: opening a table closes every result tab
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
function dbFocusedColumnValue(d: DbState, column: string): unknown {
  if (!d.focus || !d.data) return undefined;
  /* Straight-line flow after the guard: d, d.focus and d.data are all narrowed here, so
   * the reads below need no assertions at all (docs/37 R2). */
  if (d.focus.r < d.inserts.length) return d.inserts[d.focus.r].values[column];
  const row = d.data.rows[d.focus.r - d.inserts.length];
  return row ? row[column] : undefined;
}

/** Open the referenced table with the filter preset (adminer's select-link, dbgate's
 *  openReferenceForm — both land on the target table already filtered to one row). */
function dbFkOpen(fk: ApiDbFkRow, value: unknown): void {
  const j = dbFkJump(fk, value);
  if (!j || !dbOkToDrop()) return;
  const d = dbView();
  d.table = j.table; d.schema = j.schema;
  d.offset = 0; d.order = null; d.dir = "asc"; d.filters = j.filters;
  d.tab = "data"; d.detail = null;
  d.sqlResult = null; d.sqlResults = null; d.sqlTab = 0;
  dbDropEdits();
  renderDbTables();
  void dbLoadData();
  void dbLoadDetail(); // the target table's own FK arrows arrive with its detail
}

export { DB_HISTORY_KEY, DB_HISTORY_MAX, DB_PAGE_SIZES, dbClearSel, dbConnLabel, dbDialectOf, dbDropEdits, dbFilterMatches, dbFocusedColumnValue, dbFkJump, dbFkOpen, dbIsPg, dbKnownSchemas, dbLoadTables, dbOkToDrop, dbOpenTable, dbPaneChange, dbPaneClick, dbPaneInput, dbPaneKeydown, dbPending, dbPkKey, dbPkVals, dbResultKey, loadDbView, renderDbSide, renderDbTables, renderDbView };

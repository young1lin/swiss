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

import type { ApiDbConnectionRow, ApiDbDatabase, ApiDbDatabasesResponse, ApiDbFkRow, ApiDbRedisKeyRow, ApiDbTableRow, ApiDbTablesResponse } from "./types/api.js";
import type { DbConnState, DbFilterTerm, DbTableTab } from "./types/state.js";
import { $, apiJson, dbReqGuard, el, iconNode, targetEl } from "./util.js";
import { fill, h } from "./h.js";
import { currentPageCount } from "./page-registry.js";
import { dbIsRedis, dbLoadKeys, dbRedisClick, dbRedisKeydown } from "./data-browsers.js";
import { dbFiltersChange, dbFiltersClick, dbFiltersInput, dbFiltersKeydown, dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbGridChange, dbGridClick, dbGridKeydown, dbLoadData, dbToolbarClick, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbBarClick, dbFavLoad, dbHistoryLoad, dbHistoryRender, dbRunSql, renderDbBar } from "./data-sql.js";
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
import { loadCollapsed, mountGroup } from "./groups.js";
import type { GroupCfg, GroupSlice, MenuItem } from "./types/dom.js";
import { DB_TREE_SECTIONS, dbSectionOf, dbSectionSlices, redisNamespaceTree } from "./data-tree.js";
import type { DbTreeSection, RedisNsGroup } from "./data-tree.js";
import { dbConn, dbIsMounted, dbSqlTab, dbTab, dbTabs, mountDbView } from "./db-state.js";
// The strip's policy module. The cycle is the same accepted shape as the data-structure edge
// below: data-tabs reaches back for renderDbTables, and both sides only call across it inside
// functions, never at module scope.
import { dbOpenTab, dbResetTabsForConn, dbTabScope, dbTabsAuxClick, dbTabsClick, dbTabsPending, dbTabPending, renderDbTabs } from "./data-tabs.js";
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
function dbPending(): number {
  return dbTabPending(dbTab());
}

/** The connection menu's rows (docs/20 G5 as docs/43 M3 restated it): every connection
 *  under its group, a heading row where the optgroups were, first-appearance order over
 *  the flat list, one separator between groups. A single group stays flat (a heading
 *  around everything says nothing); a gateway that answers no group reads as one list.
 *  Pure — the row's fn hands the picked name back to the caller's pick. */
export function dbConnMenuItems(
  conns: ApiDbConnectionRow[], current: string, pick: (name: string) => void,
): MenuItem[] {
  const items: MenuItem[] = [];
  const order: string[] = [];
  const buckets: Record<string, ApiDbConnectionRow[]> = {};
  conns.forEach((c: ApiDbConnectionRow): void => {
    const g = c.group || "default";
    if (!buckets[g]) { buckets[g] = []; order.push(g); }
    buckets[g].push(c);
  });
  order.forEach((g: string, gi: number): void => {
    if (gi > 0 || order.length === 1) items.push({ sep: true });
    if (order.length > 1) items.push({ heading: true, label: g, fn: (): void => {} });
    buckets[g].forEach((c: ApiDbConnectionRow): void => {
      items.push({
        label: dbConnLabel(c),
        title: dbConnLabel(c),
        on: c.name === current,
        fn: (): void => { pick(c.name); },
      });
    });
  });
  return items;
}

/** The WHOLE strip's buffered writes (docs/42 D5). The page-leave guard asks this one, not
 *  dbPending(): before the tabs, a buffer could only exist on the object in front of the
 *  operator, and after them a background tab's edits would have walked out unmentioned. */
function dbPendingAll(): number {
  return dbTabsPending(dbTabs());
}

/** Ask before an action would drop the ACTIVE tab's buffered edits; false when the user said no. */
function dbOkToDrop(): boolean {
  return dbAskDrop(dbPending());
}

/** Ask before LEAVING the page would drop any tab's buffered edits (docs/42 D5) — once, with
 *  the total, so eight dirty tabs are eight confirms fewer than one guard per tab. */
function dbOkToLeave(): boolean {
  return dbAskDrop(dbPendingAll());
}

function dbAskDrop(n: number): boolean {
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
function dbClearSel(): void {
  const t = dbTab();
  t.sel = {};
  t.selAnchor = -1;
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
  const d = dbConn();
  d.conns = j.connections || [];
  const stillThere = d.conns.some((c: ApiDbConnectionRow): boolean => { return c.name === d.conn; });
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
      // docs/43 M3: the sidebar's top is TWO band-shaped rows — the connection row (status
      // dot, name, dialect chip, chevron; popupMenu keeps the docs/20 G5 groups as heading
      // rows) and, when the connection has a database axis, the database row (current name,
      // chevron; primary first with the rest behind a separator, system last, not-browsable
      // disabled with the server's reason one hover away). A select cannot carry any of
      // that; these two buttons replaced it.
      h("button", { class: "db-conn-row", id: "dbConnRow", type: "button", aria: { haspopup: "menu", label: tr("dataView.connection") } }),
      h("button", { class: "db-db-row", id: "dbDatabaseRow", type: "button", hidden: true, aria: { haspopup: "menu", label: tr("dataView.databaseRow") } }),
      // docs/43 M2: the sidebar's top is the connection row and ONE search box. The sort
      // select and its direction button moved into the Tables band's ellipsis (where they
      // act), and pg's schema dropdown died - a schema is a VISIBLE band in the tree now,
      // and a dropdown beside it would be two mechanisms saying one thing.
      h("input", { id: "dbGrep", type: "search", placeholder: tr("dataView.filterTables"), aria: { label: tr("dataView.filterTables") } }),
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
      // docs/43 M4: the flat action row is gone — Run is the toolbar's primary action,
      // Explain/Format/favorites/history ride the toolbar's overflow menu.
      h("div", { class: "db-console", id: "dbConsole", hidden: true },
        h("div", { class: "db-sql-wrap" },
          h("pre", { class: "db-sql-hl db-sql-face", id: "dbSqlHl", aria: { hidden: "true" } }),
          h("textarea", {
            class: "db-sql-face", id: "dbSql", spellcheck: false,
            placeholder: tr("dataView.selectUpdateDeleteStatements"),
          })),
        h("div", { class: "db-console-row" },
          h("span", { class: "hint", id: "dbSqlHint" }, tr("dataView.statementsSplitCtrlEnter")))),
      h("div", { class: "db-grid-wrap", id: "dbGridWrap" }),
      // docs/43 M4: the STATUS bar — pager, page size, elapsed, editability, connection —
      // a second line under the pane body, separate from .db-bar (the commit bar, whose
      // job is unchanged). Rendered by renderDbStatus on every grid repaint.
      h("div", { class: "db-status", id: "dbStatus" }),
      h("div", { class: "db-bar", id: "dbBar", hidden: true })));
  pane.appendChild(root);
  pane.onclick = dbPaneClick;
  pane.oninput = dbPaneInput;
  pane.onchange = dbPaneChange;
  pane.onkeydown = dbPaneKeydown;
  // auxclick is its own event type, property-assigned like its four siblings (docs/37 R5).
  pane.onauxclick = dbPaneAuxClick;
  // The view is REBUILT on every entry, but d.grep persists for the same table — seed the box
  // from state, or the list stays filtered by a term the (fresh, empty) input no longer shows.
  const db_ = dbConn();
  $<HTMLInputElement>("dbGrep").value = db_.grep || "";
  const st = dbSqlTab();
  $<HTMLTextAreaElement>("dbSql").value = st ? st.sqlText : "";
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
  if (dbTabsClick(t, ev)) return;
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

/** The middle-button half of the pointer contract (docs/43 M1 D8): a middle press fires
 *  click once for compatibility and auxclick for the truth, so the strip's middle close is
 *  answered HERE — acting on click as well would close two tabs for one press. */
function dbPaneAuxClick(ev: MouseEvent): void {
  if (ev.button !== 1) return; // 1 = middle; anything else belongs to click
  const t = targetEl(ev);
  if (!t) return;
  dbTabsAuxClick(t);
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
  // docs/43 M4: the console's flat row is gone, so the pane has no chrome-owned selects any
  // more — every change belongs to the grid (the status bar's page-size included) or a form.
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
  // wrapped: dbRunSql's first parameter is 'explain' — an event object is truthy, so a
  // plain Run click used to quietly run EXPLAIN (docs/22 W5.4 audit). docs/43 M4 moved
  // Run to the toolbar; the delegated id branch stays — the toolbar button carries it.
  if (t.closest("#dbSqlRun")) { void dbRunSql(false); return true; }
  // docs/43 M3: the connection row opens the connection menu — the docs/20 G5 groups as
  // heading rows, one separator between groups, the selected connection marked on.
  const connRow = t.closest<HTMLElement>("#dbConnRow");
  if (connRow) {
    const d = dbConn();
    if (!d.conns.length) return true;
    ev.stopPropagation();
    popupMenu(connRow.getBoundingClientRect(),
      dbConnMenuItems(d.conns, d.conn || "", (name: string): void => { void dbSwitchConn(name); }));
    return true;
  }
  // docs/43 M3: the database row opens the selector — primary first with its group heading,
  // the rest behind a separator, system last, not-browsable rows disabled with the server's
  // reason one hover away.
  const dbRowBtn = t.closest<HTMLElement>("#dbDatabaseRow");
  if (dbRowBtn) {
    const d = dbConn();
    if (!d.databases || !d.databases.length) return true;
    ev.stopPropagation();
    popupMenu(dbRowBtn.getBoundingClientRect(),
      dbDatabaseMenuItems(d.databases, dbCurrentDatabase(d), dbSwitchDatabase));
    return true;
  }
  const moreBtn = t.closest<HTMLElement>("#dbMore");
  if (moreBtn) {
    // stopPropagation: the document click closes popup menus — the opening click must not.
    ev.stopPropagation();
    // The monitor is an open object (docs/42 T2): the entry opens its tab, and the tab's own
    // × closes it — which is why there is no "Close activity" twin here any more.
    popupMenu(moreBtn.getBoundingClientRect(), [
      { label: tr("dataView.activity"), fn: (): void => { dbOpenTab({ kind: "activity" }); } },
    ]);
    return true;
  }
  // The redis key list: the row carries the key; the live selection decides the guard.
  const keyRow = t.closest<HTMLElement>("[data-rkey]");
  if (keyRow) {
    // A key is an object: it opens its own tab (docs/42 T2), so a second key no longer eats
    // the first one's typed-value buffer and no guard has to ask about it.
    dbOpenTab({ kind: "key", key: keyRow.dataset.rkey || "" });
    return true;
  }
  // A table row: re-find the row in the live list by name+schema and open it.
  const tblRow = t.closest<HTMLElement>("[data-tname]");
  if (tblRow) {
    const d = dbConn();
    const name = tblRow.dataset.tname!;
    const schema = tblRow.dataset.tschema || null;
    const row = d.tables.find((x: ApiDbTableRow): boolean => { return x.name === name && (x.schema || null) === schema; });
    if (row) dbOpenTab({ kind: "table", table: row.name, schema: schema });
    return true;
  }
  if (t.closest("[data-keysmore]")) { void dbLoadKeys(false); return true; }
  const tpg = t.closest<HTMLElement>("[data-tpg]");
  if (tpg) {
    const d = dbConn();
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
      const d = dbConn();
      d.grep = v; d.tablesPage = 0;
      if (dbIsRedis()) void dbLoadKeys(true);
      else void dbLoadTables();
    }, 300);
    return true;
  }
  const ta = t.closest<HTMLTextAreaElement>("#dbSql");
  if (ta) {
    const st = dbSqlTab();
    if (st) st.sqlText = ta.value;
    dbSqlPaint();
    dbSuggestOnInput.call(ta);
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

/* The list's sort row. SQL connections sort server-side (name/rows/size against the catalog);
 * redis sorts client-side over the keys SCAN has already handed over (key/TTL). One control,
 * one direction button; the option set repaints when the connection kind changes. */
function dbSortOptions(): { v: string; t: string }[] {
  return dbIsRedis()
    ? [{ v: "name", t: tr("dataView.sortKey") }, { v: "ttl", t: "TTL" }]
    : [{ v: "name", t: tr("dataView.sortName") }, { v: "rows", t: tr("dataView.sortRows") }, { v: "size", t: tr("dataView.sortSize") }];
}

/** Redis keys arrive in SCAN (hash-slot) order; the list sorts what has been loaded. Key names
 *  compare naturally — numeric runs by value, case folded (t2 < t10, foo == FOO) — because the
 *  raw byte order is exactly the ASCII sort nobody asked for. TTL puts expiring keys first; no
 *  expiry (-1) reads as forever. */
function dbRedisCompare(a: ApiDbRedisKeyRow, b: ApiDbRedisKeyRow): number {
  const d = dbConn();
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
  // connection (or none) hides the button rather than the menu hiding its one item.
  const more = $("dbMore");
  const d = dbConn();
  if (more) more.hidden = !d.conn || dbIsRedis();
}

/** The one label a connection goes by — the sidebar dropdown's option text. The page-bar
 *  count chip (views/data.js countText) shows the SAME string, so the bar and the control
 *  that changes it can never disagree; two builders is how the bar ended up saying "Data"
 *  twice while the dropdown said "shop-redis · redis". */
function dbConnLabel(c: ApiDbConnectionRow): string {
  return c.name + " · " + c.dialect;
}

/* --- docs/43 M3: the two sidebar rows' menus and the two switches ----------------------------- */

// One /databases request in flight (renderDbSide can fire the lazy fetch from a busy loop).
let dbDatabasesInFlight = false;

/** docs/43 M3: the database IN EFFECT — the picked one, or the catalog's primary when
 *  nothing has been picked yet ("" means the configured default, which the primary row
 *  stands for). The row's name, the menu's on-mark and the switch's own no-op test all
 *  read THIS, so the three can never disagree. Pure. */
function dbCurrentDatabase(d: DbConnState): string {
  if (d.database) return d.database;
  const list = d.databases || [];
  const p = list.find((x: ApiDbDatabase): boolean => x.primary);
  return p ? p.name : "";
}

/** Switch CONNECTIONS (docs/42 D5, now shared by the row menu): every open object closes,
 *  the database catalog of the connection being left is dropped, and the first page loads
 *  for the new connection's CONFIGURED database. */
async function dbSwitchConn(name: string): Promise<void> {
  const d = dbConn();
  if (!name || name === d.conn) return;
  // Every open object, not just the one in front: the switch is a page-wide drop, so it
  // asks with the whole strip's total.
  if (!dbOkToLeave()) return;
  d.conn = name;
  d.tables = []; d.tablesPage = 0;
  d.schemaFilter = ""; // a schema pick was made against the other connection's catalog
  d.redis = null;
  d.database = "";  // docs/43 M3: the new connection starts on its CONFIGURED database
  d.databases = null; // and its catalog is not the one just left behind
  d.sort = "name"; d.sortDir = "asc"; // the new connection's kind may not have the chosen key
  d.grep = "";        // a sidebar search is table-list-scoped, never inherited
  d.redisType = "";   // same reasoning: a type filter belongs to the key list it chose
  const gb = $<HTMLInputElement>("dbGrep");
  if (gb) gb.value = "";
  dbResetTabsForConn();
  const ta0 = $<HTMLTextAreaElement>("dbSql");
  if (ta0) ta0.value = "";
  dbSqlPaint();
  dbSyncKind();
  renderDbSide();
  renderDbTabs();
  renderDbTables(); renderDbToolbar(); renderDbFilters(); renderDbGrid(); renderDbBar();
  const chip = $("countChip");
  if (chip) chip.textContent = currentPageCount();
  if (dbIsRedis()) void dbLoadKeys(true);
  else { void dbLoadTables(); void dbLoadDatabases(); }
}

function renderDbSide(): void {
  const d = dbConn();
  const row = $("dbConnRow");
  const dbRow = $("dbDatabaseRow");
  if (!row) return;
  row.textContent = "";
  if (!d.conns.length) {
    row.classList.add("off");
    row.appendChild(el("span", "db-row-name", tr("dataView.noDatabaseMcps")));
    if (dbRow) dbRow.hidden = true;
    return;
  }
  row.classList.remove("off");
  // The connection row itself: status dot + name + dialect chip + chevron. The dot is the
  // connection's state in one glance (stopped = hollow text-colored, everything else =
  // the accent); the chip names the dialect so the row no longer needs the label's host.
  const cur = d.conns.find((c: ApiDbConnectionRow): boolean => { return c.name === d.conn; });
  const dot = el("span", "db-dot" + (cur && cur.state === "stopped" ? " off" : ""));
  row.appendChild(dot);
  row.appendChild(el("span", "db-row-name", cur ? cur.name : tr("dataView.pickConnection")));
  if (cur) row.appendChild(el("span", "db-chip", cur.dialect));
  const chev = iconNode("chevron-down");
  chev.setAttribute("class", "ic db-row-chev");
  row.appendChild(chev);
  // docs/20 G5: the connection menu keeps the groups — heading rows where the optgroups
  // were, first-appearance order over the flat list, one separator between groups. A single
  // group stays flat (a heading around everything says nothing); no group reads as one list.
  // docs/43 M3: the database row appears only when the catalog answered with entries; the
  // catalog itself is fetched once per connection (dbLoadDatabases), null = not yet asked.
  if (dbRow) {
    const names = d.databases || [];
    const show = names.length > 0;
    dbRow.hidden = !show;
    if (show) {
      dbRow.textContent = "";
      const active = dbCurrentDatabase(d);
      const dbIc = iconNode("database");
      dbIc.setAttribute("class", "ic db-row-ic");
      dbRow.appendChild(dbIc);
      dbRow.appendChild(el("span", "db-row-name", active));
      const chev2 = iconNode("chevron-down");
      chev2.setAttribute("class", "ic db-row-chev");
      dbRow.appendChild(chev2);
    }
  }
  // docs/43 M3: the catalog is fetched once per connection — null means not asked yet, and
  // the in-flight flag keeps a busy render loop from asking twice.
  if (d.conn && d.databases === null && !dbDatabasesInFlight) void dbLoadDatabases();
}

/** docs/43 M3: the database selector's ordering, a pure function so the acceptance suite
 *  can pin it - primary first, the rest by name, system databases last (they stay listed:
 *  hiding them is not the panel's call). */
function dbSortDatabases(list: ApiDbDatabase[]): ApiDbDatabase[] {
  return list.slice().sort((a: ApiDbDatabase, b: ApiDbDatabase): number => {
    if (a.primary !== b.primary) return a.primary ? -1 : 1;
    if (a.system !== b.system) return a.system ? 1 : -1;
    return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
  });
}

/** docs/43 M3: the database selector's menu rows, built as DATA so the suite can pin the
 *  contract (primary first; browsable:false rows disabled with the server's own reason as
 *  title; the selected database marks on and cannot re-pick) without a live menu. */
function dbDatabaseMenuItems(
  list: ApiDbDatabase[],
  selected: string,
  pick: (name: string) => void,
): MenuItem[] {
  const items: MenuItem[] = [];
  const sorted = dbSortDatabases(list);
  sorted.forEach((x: ApiDbDatabase, i: number): void => {
    if (x.primary && i === 0) {
      items.push({ heading: true, label: tr("dataView.dbPrimaryGroup"), fn: (): void => {} });
    } else if (!x.primary && (i === 0 || sorted[i - 1].primary)) {
      items.push({ sep: true });
      items.push({ heading: true, label: tr("dataView.dbOtherGroup"), fn: (): void => {} });
    } else if (x.system && (i === 0 || !sorted[i - 1].system)) {
      items.push({ sep: true });
      items.push({ heading: true, label: tr("dataView.dbSystemGroup"), fn: (): void => {} });
    }
    items.push({
      label: x.name + (x.tables != null ? " - " + Number(x.tables).toLocaleString(locale()) : ""),
      title: x.browsable ? "" : (x.reason || ""),
      disabled: !x.browsable || x.name === selected,
      on: x.name === selected,
      fn: (): void => { pick(x.name); },
    });
  });
  return items;
}

/** docs/43 M3 4.3.3: switching DATABASES is the light version of switching connections -
 *  the same whole-strip confirmation (an orders tab that now points at another database's
 *  orders table is worse than a closed tab), the same resets, but the connection and its
 *  catalog stay. */
function dbSwitchDatabase(name: string): void {
  const d = dbConn();
  if (!name) return;
  // The no-op test reads the database IN EFFECT (dbCurrentDatabase): picking the primary
  // while nothing has been picked is "stay", not a strip-wide drop for nothing.
  if (name === dbCurrentDatabase(d)) return;
  if (!dbOkToLeave()) return;
  d.database = name;
  d.tables = []; d.tablesPage = 0;
  d.grep = "";
  d.sort = "name"; d.sortDir = "asc";
  const gb = $<HTMLInputElement>("dbGrep");
  if (gb) gb.value = "";
  dbResetTabsForConn();
  renderDbSide();
  renderDbTabs();
  renderDbTables(); renderDbToolbar(); renderDbFilters(); renderDbGrid(); renderDbBar();
  void dbLoadTables();
}

/** docs/43 M3: fetch the connection's database catalog once — null means not asked yet,
 *  and an EMPTY array is a real answer (the dialect has no database axis; the row stays
 *  hidden). The lazy fetch rides the first renderDbSide after a connection switch; the
 *  selector itself then reads the cache. */
async function dbLoadDatabases(): Promise<void> {
  const d = dbConn();
  if (!d.conn || d.databases) return;
  dbDatabasesInFlight = true;
  try {
    const j = await apiJson<ApiDbDatabasesResponse>("/api/db/" + encodeURIComponent(d.conn) + "/databases");
    if (!j) { d.databases = []; return; } // the toast already spoke; an empty axis hides the row
    d.databases = j.databases || [];
  } finally {
    dbDatabasesInFlight = false;
  }
  renderDbSide();
}

/* --- lazy table list ---------------------------------------------------------------------------- */

// One /tables request chain: a slow answer for a page or filter the user just left must be
// dropped, or it would repopulate the sidebar with the stale page (docs/22 closeout audit).
const dbTablesReq = dbReqGuard();

async function dbLoadTables(): Promise<void> {
  const d = dbConn();
  if (!d.conn) return;
  const box = $("dbTables");
  if (box) { box.textContent = ""; box.appendChild(el("div", "db-hint", tr("dataView.loading"))); }
  let q = "/api/db/" + encodeURIComponent(d.conn) + "/tables?page=" + d.tablesPage;
  if (d.grep) q += "&grep=" + encodeURIComponent(d.grep);
  // docs/43 M3: on MySQL the schema parameter NAMES THE DATABASE — absent keeps the
  // configured one byte-identical; a chosen database rides here (the server whitelists it
  // before any SQL is built). pg keeps its in-database schema semantics, untouched.
  if (d.schemaFilter) q += "&schema=" + encodeURIComponent(d.schemaFilter);
  else if (d.database) q += "&schema=" + encodeURIComponent(d.database);
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
  const d = dbConn();
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

/* The sidebar's list, which is a TREE now (docs/43 M2): SQL connections cut into Tables /
   Views / Routines bands (a pg catalog nests them under schema bands), a redis keyspace
   folded along ":" with single-child chains compressed. Every band is mountGroup - the one
   container-head shape swiss-ui-design allows - at side density; the collapse state rides the
   component's own localStorage (scope "dbtree", plus ".sch" for pg's schema bands and ".ns"
   for redis namespaces). Nothing here drags: the grouping is DERIVED from the catalog, not
   named by the operator. */
function renderDbTables(): void {
  const d = dbConn();
  const box = $("dbTables");
  if (!box) return;
  box.textContent = "";
  if (!d.conn) {
    box.appendChild(el("div", "db-hint", tr("dataView.addMysqlPgMcpHint")));
    return;
  }
  if (dbIsRedis()) {
    const rr = d.redis;
    if (!rr || !rr.keys.length) {
      // docs/22 closeout B1: a failed scan is a FAILURE, not an empty keyspace — the toast
      // carries the server's own text; this row keeps the list from pretending otherwise.
      // docs/43 M2 walk: SCAN MATCH may return a hits-empty page while the keyspace walk
      // continues (done=false) — that is not "no keys match", it is "not there YET"; the
      // footer's More keeps scanning.
      box.appendChild(el("div", "db-hint", d.redisError
        ? tr("dataView.scanFailedToastCarries")
        : rr && !rr.done
          ? tr("dataView.scanNoHitsYet")
          : d.grep ? tr("dataView.keysMatchQ", { q: d.grep }) : tr("dataView.keysScanReturnedNone")));
    }
    // The live key tab holds the selection: a redis connection's single tab is kind
    // "key" (docs/42 T1), and a fresh one with no key opened selects nothing.
    const kt = dbTab();
    const selKey = kt.kind === "key" ? kt.redisKey : null;
    // ONE collapse map per scope for the whole pass (docs/43 M2): mountGroup mutates the map
    // it is handed and saves it whole, so a per-band load would let the second toggle clobber
    // the first.
    const nsCollapsed = loadCollapsed("dbtree.ns");
    const tree = redisNamespaceTree(rr ? rr.keys : [], dbRedisCompare);
    tree.forEach((g: RedisNsGroup): void => {
      if (!g.ns) {
        // Keys with no ":" are the keyspace's own rows (docs/43 M2 acceptance): they sit at
        // the root, not under a band that pretends a namespace exists.
        g.keys.forEach((k: ApiDbRedisKeyRow): void => { box.appendChild(dbKeyRow(k, k.key, selKey)); });
        return;
      }
      // The leaf promise at the root too: a namespace holding exactly one key and nothing
      // else IS that row (docs/43 M2 #4) - no folder around a single file.
      if (!g.children.length && g.keys.length === 1) {
        box.appendChild(dbKeyRow(g.keys[0], g.keys[0].key, selKey));
        return;
      }
      box.appendChild(dbRedisBand(g, "", selKey, nsCollapsed, !!d.grep));
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
  // docs/43 M2: the SQL tree. A pg catalog nests its type bands under one band per schema
  // (mockup B); MySQL is one database, so its three type bands sit at the root. Both shapes
  // are the same component at the same density - a schema band's "rows" are the type bands.
  if (dbIsPg() && d.tables.length) {
    // One collapse map per scope for the whole pass (see the redis half above for why).
    const secCollapsed = loadCollapsed("dbtree");
    const schCfg: GroupCfg<GroupSlice<ApiDbTableRow>> = {
      scope: "dbtree.sch", density: "side",
      names: [], collapsed: loadCollapsed("dbtree.sch"), noun: "schema",
      draggable: false, filtered: !!d.grep,
      reload: renderDbTables, render: renderDbTables,
      rowsById: () => { return []; },
      groupOfRow: (s: GroupSlice<ApiDbTableRow>): string => { return s.rows.length ? s.rows[0].schema : ""; },
      rowNode: (s: GroupSlice<ApiDbTableRow>): HTMLElement => { return dbSectionBand(s, secCollapsed); },
      countOf: (s: { rows: unknown[] }): number => {
        return s.rows.reduce((n: number, sec: unknown): number => {
          const sl = sec as GroupSlice<ApiDbTableRow>;
          return n + (sl && sl.rows ? sl.rows.length : 0);
        }, 0);
      },
    };
    const schemas: string[] = [];
    d.tables.forEach((t: ApiDbTableRow): void => {
      if (!schemas.includes(t.schema)) schemas.push(t.schema);
    });
    schemas.forEach((s: string): void => {
      const rows = dbSectionSlices(d.tables.filter((t: ApiDbTableRow): boolean => { return t.schema === s; }));
      box.appendChild(mountGroup(schCfg, { name: s, rows: rows }));
    });
  } else if (d.tables.length) {
    const secCollapsed = loadCollapsed("dbtree");
    dbSectionSlices(d.tables).forEach((sec: { name: DbTreeSection; rows: ApiDbTableRow[] }): void => {
      box.appendChild(dbSectionBand(sec, secCollapsed));
    });
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

/** One table row, one LINE (docs/43 M2): the name left, the approximate row count right in
 *  --text-3 tnum caption - the two-line row's "table · ~9,309 rows · 7.0 MB" meta moved into
 *  the title, one hover away, where nothing truncates the name to show it. docs/37 R5: the
 *  name and schema are the address — the delegated click (dbChromeClick's [data-tname])
 *  re-finds the row in the live list and hands it to dbOpenTab, whose dedupe and cap are
 *  re-read at event time. */
function dbTableRow(t: ApiDbTableRow, scope: string | null): HTMLElement {
  const ct = dbTab();
  const sel = ct.kind === "table" && t.name === ct.table && t.schema === ct.schema;
  const b = el("button", "db-table" + (sel ? " sel" : ""));
  b.title = t.schema ? t.schema + "." + t.name + " · " : "";
  b.title += t.type + (t.approxRows != null
    ? " · " + tr("dataView.approxRows", { n: Number(t.approxRows).toLocaleString(locale()) }) : "")
    + (t.size ? " · " + t.size : "");
  b.dataset.tname = t.name;
  b.dataset.tschema = t.schema || "";
  // docs/43 M3 D3's other shoe: a table in a database OTHER than the scope carries its
  // db.table qualifier in the row (browsing a secondary database must never read as the
  // configured one's tables); inside the scope the name stands alone, as it always did.
  const qualified = t.schema && scope && t.schema !== scope;
  b.appendChild(el("span", "db-table-name", qualified ? t.schema + "." + t.name : t.name));
  // The trailing slot numbers what the row holds. Zero reads as 0 - an empty table is a fact
  // worth scanning for, not a blank.
  b.appendChild(el("span", "db-table-meta",
    t.approxRows != null ? Number(t.approxRows).toLocaleString(locale()) : ""));
  return b;
}

/** One redis key row, one line (docs/43 M2): the name left (the REMAINDER under its band's
 *  prefix - the band already said the prefix; a leaf row says the whole key), its type and
 *  expiry right. The full key stays the row's title and its address ([data-rkey]). */
function dbKeyRow(k: ApiDbRedisKeyRow, label: string, selKey: string | null): HTMLElement {
  const b = el("button", "db-table" + (k.key === selKey ? " sel" : ""));
  let meta = k.type;
  if (k.ttl! >= 0) meta += " · ttl " + k.ttl + "s";
  b.title = k.key + " · " + meta;
  b.dataset.rkey = k.key;
  b.appendChild(el("span", "db-table-name", label));
  b.appendChild(el("span", "db-table-meta", k.type + (k.ttl! >= 0 ? " · " + k.ttl + "s" : "")));
  return b;
}

/** One Tables / Views / Routines band (docs/43 M2) — the tree's leaf container, shared by
 *  MySQL (at the root) and pg (nested inside its schema band). The band owns the list's sort
 *  (its ellipsis, mockup B) and — on the Tables section only — the New table +. */
function dbSectionBand(sec: { name: string; rows: ApiDbTableRow[] },
  secCollapsed: Record<string, boolean>): HTMLElement {
  const d = dbConn();
  const cfg: GroupCfg<ApiDbTableRow> = {
    scope: "dbtree", density: "side",
    names: DB_TREE_SECTIONS.slice(), collapsed: secCollapsed, noun: "table",
    draggable: false, filtered: !!d.grep,
    label: dbSectionLabel,
    onAdd: dbNewTableFlow, canAdd: (g: string): boolean => { return g === "tables"; },
    addTitle: (): string => { return tr("dataView.newTable2"); },
    moreItems: (g: string): MenuItem[] | null => { return g === "tables" ? dbSortMenu() : null; },
    moreTitle: (): string => { return tr("dataView.sort"); },
    emptyText: dbSectionEmpty,
    reload: renderDbTables, render: renderDbTables,
    rowsById: (): ApiDbTableRow[] => { return d.tables; },
    groupOfRow: (t: ApiDbTableRow): DbTreeSection => { return dbSectionOf(t); },
    rowNode: (t: ApiDbTableRow): HTMLElement => dbTableRow(t, dbTabScope(d, dbIsPg())),
  };
  return mountGroup(cfg, sec);
}

/** The band label over a stable section id (docs/38: identity keys the fold state, copy is
 *  a lookup, so the tree never mistranslates between renders - and the keys stay LITERAL,
 *  one branch per section, because the dictionary's orphan scanner reads source, not
 *  concatenations). */
function dbSectionLabel(sec: string): string {
  if (sec === "tables") return tr("dataTree.tables");
  if (sec === "views") return tr("dataTree.views");
  return tr("dataTree.routines");
}

/** What an empty section says: the fact it is missing, not a drop target it cannot honor
 *  (docs/43 M2 - nothing can be dropped into a band the catalog owns). */
function dbSectionEmpty(sec: string): string {
  if (sec === "tables") return tr("dataTree.no.tables");
  if (sec === "views") return tr("dataTree.no.views");
  return tr("dataTree.no.routines");
}

/** New table… — the Tables band's +, the same flow the old list header's button opened
 *  (docs/22 W4.6). Where it lands follows the picked schema the same way. */
function dbNewTableFlow(): void {
  const d = dbConn();
  if (!d.conn || dbIsRedis()) return;
  openDbDdlSheet("table", {
    dialect: dbDialectOf(),
    conn: d.conn,
    schema: dbIsPg() ? (d.schemaFilter || "public") : "",
    schemas: dbIsPg() ? dbKnownSchemas() : [],
  });
}

/** The Tables band's ellipsis (docs/43 M2 #6): the list's sort key and direction, moved off
 *  the sidebar's top row and put where they act. SQL re-requests the list (server-side sort
 *  against the catalog); redis re-sorts what SCAN has handed over, client-side. */
function dbSortMenu(): MenuItem[] {
  const d = dbConn();
  const items: MenuItem[] = dbSortOptions().map((o: { v: string; t: string }): MenuItem => {
    return { label: o.t, pick: true, on: d.sort === o.v, fn: (): void => { dbSetSort(o.v, null); } };
  });
  items.push({ sep: true });
  items.push({ label: tr("dataView.sortAscending"), pick: true, on: d.sortDir !== "desc", fn: (): void => { dbSetSort(null, "asc"); } });
  items.push({ label: tr("dataView.sortDescending"), pick: true, on: d.sortDir === "desc", fn: (): void => { dbSetSort(null, "desc"); } });
  return items;
}

/** Apply a sort choice from the band menu (docs/43 M2): key or direction or both; a null
 *  argument keeps what is set. Choosing what is already set is a repaint, not a request. */
function dbSetSort(key: string | null, dir: string | null): void {
  const d = dbConn();
  if (dir) d.sortDir = dir;
  if (key) d.sort = key;
  d.tablesPage = 0;
  if (dbIsRedis()) renderDbTables();
  else void dbLoadTables();
}

/** One redis namespace band (docs/43 M2): mountGroup over the node's children (as bands)
 *  and its direct keys (as rows), children first. A namespace holding exactly one key and
 *  nothing else IS that row — no folder around a single file (the tree compressed the chain
 *  on the way in; this is the rendering half of that promise). */
function dbRedisBand(g: RedisNsGroup, parentNs: string, selKey: string | null,
  nsCollapsed: Record<string, boolean>, filtered: boolean): HTMLElement {
  type Row = RedisNsGroup | ApiDbRedisKeyRow;
  const isGroup = (r: Row): r is RedisNsGroup => { return (r as RedisNsGroup).ns !== undefined; };
  const prefix = parentNs ? parentNs + ":" : "";
  const rows: Row[] = ([] as Row[]).concat(g.children, g.keys);
  const cfg: GroupCfg<Row> = {
    scope: "dbtree.ns", density: "side",
    names: [], collapsed: nsCollapsed, noun: "key",
    draggable: false, filtered,
    // The one-key leaf never reaches a band of its own (its parent renders it as the row),
    // so every band that exists holds at least two things worth sorting.
    moreItems: (): MenuItem[] | null => { return dbSortMenu(); },
    moreTitle: (): string => { return tr("dataView.sort"); },
    reload: renderDbTables, render: renderDbTables,
    rowsById: (): Row[] => { return []; },
    groupOfRow: (r: Row): string => { return isGroup(r) ? r.ns : r.key; },
    rowNode: (r: Row): HTMLElement => {
      if (isGroup(r)) {
        if (!r.children.length && r.keys.length === 1) return dbKeyRow(r.keys[0], r.keys[0].key, selKey);
        return dbRedisBand(r, g.ns, selKey, nsCollapsed, filtered);
      }
      return dbKeyRow(r, r.key.slice(prefix.length), selKey);
    },
  };
  return mountGroup(cfg, { name: g.ns, rows: rows });
}

/** The selected connection's dialect word ("mysql" | "pg") — the DDL sheets build their
 *  type suggestions and titles on it (docs/22 W4.6). */
function dbDialectOf(): string {
  const d = dbConn();
  const c = d.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === d.conn; });
  return (c && c.dialect) || "mysql";
}

/** Every schema the loaded pages have shown, "public" always among them — the New table
 *  sheet's where-it-goes select (docs/22 W4.6; swiss-ui-design rule 6). */
function dbKnownSchemas(): string[] {
  const d = dbConn();
  const out: string[] = [];
  d.tables.forEach((t: ApiDbTableRow): void => {
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
function dbFocusedColumnValue(ct: DbTableTab, column: string): unknown {
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
function dbFkOpen(fk: ApiDbFkRow, value: unknown): void {
  const j = dbFkJump(fk, value);
  if (!j) return;
  dbOpenTab({ kind: "table", table: j.table, schema: j.schema, filters: j.filters });
}

export { DB_HISTORY_KEY, DB_HISTORY_MAX, DB_PAGE_SIZES, dbClearSel, dbConnLabel, dbCurrentDatabase, dbDatabaseMenuItems, dbDialectOf, dbDropEdits, dbFilterMatches, dbFocusedColumnValue, dbFkJump, dbFkOpen, dbIsPg, dbKnownSchemas, dbLoadTables, dbOkToDrop, dbOkToLeave, dbPaneChange, dbPaneClick, dbPaneInput, dbPaneKeydown, dbPending, dbPendingAll, dbPkKey, dbPkVals, dbResultKey, dbSectionLabel, dbSortDatabases, dbSortMenu, dbSwitchConn, dbSwitchDatabase, loadDbView, renderDbSide, renderDbTables, renderDbView };
// dbSectionEmpty stays module-private: the acceptance suite reaches it through the band's
// emptyText hook, which is the only contract it has.

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
import type { DbConnState, DbFilterTerm, DbTab, DbTableTab } from "./types/state.js";
import { $, apiJson, dbReqGuard, el, iconNode, targetEl, typeTagNode } from "./util.js";
import { fill, h } from "./h.js";
import { currentPageCount } from "./page-registry.js";
import { dbIsRedis, dbLoadKeys, dbRedisClick, dbRedisKeydown, dbRefreshKeyspace, dbRefreshRedisValue } from "./data-browsers.js";
import { dbFiltersChange, dbFiltersClick, dbFiltersInput, dbFiltersKeydown, dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { dbGridChange, dbGridClick, dbGridKeydown, dbLoadData, dbToolbarClick, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbBarClick, dbFavLoad, dbHistoryLoad, dbHistoryRender, dbRunSql, renderDbBar } from "./data-sql.js";
import { dbActivityClick, dbActivityPollStop } from "./data-activity.js";
// The cycle data-view <-> data-structure is the same accepted shape as data-grid <->
// data-cell — both sides only call across it inside functions, never at module scope.
import { dbStructureClick } from "./data-structure.js";
// The pane's delegated listeners chain the form view's dispatchers too. data-form already
// reaches back into data-view for dbPkKey — the same accepted cycle shape as the
// data-structure edge above: both sides only call across it inside functions.
import { dbFormChange, dbFormClick, dbFormKeydown } from "./data-form.js";
import { openDbDdlSheet } from "./data-ddl.js";
import { dbSuggestHide, dbSuggestKeys, dbSuggestOnInput } from "./data-suggest.js";
import { loadCollapsed, mountGroup } from "./groups.js";
import type { GroupCfg, GroupSlice, MenuItem } from "./types/dom.js";
import { DB_TREE_SECTIONS, dbSectionOf, dbSectionSlices, redisNamespaceTree, redisTypeGlyph } from "./data-tree.js";
import type { DbTreeSection, RedisNsGroup } from "./data-tree.js";
import {
  dbConn, dbForgetParked, dbIsMounted, dbParkedTabs, dbReplaceConn, dbResumeView, dbSqlTab, dbSwapConn, dbTab, dbTabs, mountDbView,
} from "./db-state.js";
// The strip's policy module. The cycle is the same accepted shape as the data-structure edge
// below: data-tabs reaches back for renderDbTables, and both sides only call across it inside
// functions, never at module scope.
import {
  dbAfterTabSwitch, dbLastTableTab, dbOpenTab, dbResetTabsForConn, dbTabScope, dbTabsAuxClick, dbTabsClick, dbTabsContext,
  dbTabsPending, dbTabPending, renderDbTabs,
} from "./data-tabs.js";
import { locale, tr, trn } from "./i18n.js";
import { btn, moreBtn } from "./ui/button.js";
import { hint } from "./ui/form.js";
import { popupMenu } from "./ui/menu.js";
import { tag } from "./ui/status.js";

/* ================================================================================================
   Data view — a DBeaver-style browser over the mysql/pg MCPs.
   Left: a lazy, greppable, paged table list (never the whole instance at once). Right: one
   bounded page of rows (10/20/50/100/200/500, default 50 — the server's BROWSE_DEFAULT_PAGE)
   with click-to-sort headers.
   Edits are buffered client-side — an amber bar counts them — and nothing reaches the database
   until Commit posts the whole buffer as ONE transaction; Discard drops it without a query.
   Same rendering contract as everywhere else: the 6s poll never rebuilds this view.
   ================================================================================================ */

const DB_PAGE_SIZES = [10, 20, 50, 100, 200, 500];
const DB_HISTORY_KEY = "swiss.dbSqlHistory";
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
        // label is the NAME alone now - the drawer (this builder's only reader since the
        // menus became drawers) paints the dialect as a MARK beside it, and a menu label
        // that spelled both was a text-only menu's habit. title keeps the full "name ·
        // dialect" for the tooltip, which is where the host belongs anyway.
        label: c.name,
        mark: c.dialect,
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
  // docs/47 D4: every session's strip, the parked ones too - a write parked on the MySQL
  // session while Redis is in front is exactly what an unguarded tab close would lose.
  return dbParkedTabs().reduce((n: number, strip: DbTab[]): number => n + dbTabsPending(strip), dbTabsPending(dbTabs()));
}

/** Does a session hold work the park must never evict (docs/47 D7)? */
function dbSessionPinned(tabs: DbTab[]): boolean {
  return dbTabsPending(tabs) > 0;
}

/** The confirmed page leave (docs/47 D3): every session's buffered writes go and the sessions
 *  stay. canLeave() has just said the writes would be discarded, and once the page is gone
 *  nothing would count a parked one - beforeunload asks only the page in front. */
function dbDropAllEdits(): void {
  for (const strip of [dbTabs(), ...dbParkedTabs()]) {
    for (const t of strip) {
      if (t.kind === "table") { t.updates = {}; t.deletes = {}; t.inserts = []; t.sqlPreview = false; }
      if (t.kind === "key") t.redisEdits = null;
    }
  }
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
  // docs/47 D3: a page leave parked the session; the mount takes it back, so the page returns
  // on the connection it left with everything that connection held.
  dbResumeView();
  renderDbView();
  const j = await apiJson<{ connections: ApiDbConnectionRow[] }>("/api/db");
  // A leave while the list was on its way: this pane belongs to another page now.
  if (!j || !dbIsMounted()) return;
  const d = dbConn();
  d.conns = j.connections || [];
  const listed = (name: string): boolean => d.conns.some((c: ApiDbConnectionRow): boolean => c.name === name);
  dbForgetParked(listed); // a connection that left the registry takes its parked session with it
  if (!d.conn || !listed(d.conn)) {
    // The connection on screen is gone, or there never was one: go to the registry's first,
    // resuming its parked session if it has one. The one that left is not parked - nothing
    // can be read or committed through it any more.
    if (!dbReplaceConn(d.conns.length ? d.conns[0].name : null)) dbResetTabsForConn();
  }
  // The skeleton above was drawn before /api/db answered: dbShowSession dresses everything that
  // depends on the connection's KIND and repaints the strip, which hides with no connection.
  dbShowSession();
}

/** Paint the live session whole, then fetch only what it lacks (docs/47 D5). What the session
 *  holds is on screen at once; the catalog and the page in front are then re-read QUIETLY - no
 *  Loading, nothing cleared, a repaint only when the answer moved. A Redis key walk is re-walked
 *  as deep as More took it, so no page is lost (D6, revised); a console is never re-run. */
function dbShowSession(): void {
  const d = dbConn();
  const gb = $<HTMLInputElement>("dbGrep");
  if (gb) gb.value = d.grep;
  dbSyncKind();
  renderDbSide();
  // The strip, the list and the pane - and the page, detail or value the object in front lacks,
  // plus the Activity poll when a monitor is in front (D8).
  dbAfterTabSwitch(true);
  const chip = $("countChip");
  if (chip) chip.textContent = currentPageCount();
  if (!d.conn) return;
  if (dbIsRedis()) dbRefreshKeyspace();
  else {
    void dbLoadTables(d.tables.length > 0);
    if (!d.databases) void dbLoadDatabases();
  }
  const t = dbTab();
  if (t.kind === "table" && t.table && t.data) void dbLoadData(true, true, true);
  if (t.kind === "key" && t.redisKey && t.redisValue && !t.redisEdits) void dbRefreshRedisValue(t.redisKey);
}

function renderDbView(): void {
  const pane = $("pane");
  // "full" turns the pane into the library's full-bleed workspace body (ui.css .pane.full;
  // docs/13 D5 as revised): no padding, no measure - the explorer divides its own space.
  // db-host is this page's own hook for its child rules. views/data.js takes both back off
  // on unmount so no other page inherits them.
  pane.classList.add("db-host", "full");
  pane.textContent = "";
  // docs/37 R5: the skeleton is a node tree, and the pane carries ONE delegated listener per
  // event type (property-assigned, never addEventListener — a repaint re-assigns the same
  // property instead of stacking). Every control below is addressed by id or a data-*
  // attribute; nothing is wired per render.
  const root = el("div", "db-root");
  fill(root,
    h("div", { class: "db-side" },
      // docs/43 M3: the sidebar's top is TWO band-shaped rows — the connection row (status
      // dot, name, dialect mark, chevron) and, when the connection has a database axis, the
      // database row (current name, chevron). Each opens a DRAWER under itself (the owner's
      // ask: the picker slides down inside the sidebar instead of floating over it) — the
      // rows and their drawers are siblings so the drawer pushes the tree, it never covers it.
      h("button", { class: "db-conn-row", id: "dbConnRow", type: "button", aria: { expanded: "false", controls: "dbConnDrawer", label: tr("dataView.connection") } }),
      h("div", { class: "db-drawer", id: "dbConnDrawer" }, h("div", { class: "db-drawer-in" })),
      h("button", { class: "db-db-row", id: "dbDatabaseRow", type: "button", hidden: true, aria: { expanded: "false", controls: "dbDbDrawer", label: tr("dataView.databaseRow") } }),
      h("div", { class: "db-drawer", id: "dbDbDrawer" }, h("div", { class: "db-drawer-in" })),
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
        // monitor is the first. Shown only on a SQL connection (redis has no sessions) and only
        // while the open object has no ⋯ of its own - renderDbToolbar decides (docs/46 P7).
        moreBtn(tr("dataView.morePaneActions"), { id: "dbMore", hidden: true })),
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
          hint(tr("dataView.statementsSplitCtrlEnter"), { id: "dbSqlHint" }))),
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
  // So is contextmenu — the strip's card menu (rename / close left / close right) rides it.
  pane.oncontextmenu = dbPaneContext;
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
  if (dbRedisClick(t)) return;
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

/** The pane's right-click: the strip answers first (the card menu — rename, close left,
 *  close right); everything else keeps the browser's own menu, the grid's cell menus
 *  included (those are per-node handlers that run before delegation). */
function dbPaneContext(ev: MouseEvent): void {
  const t = targetEl(ev);
  if (!t) return;
  dbTabsContext(t, ev);
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
  // Escape closes the open drawer first — it is the lightest thing on the screen (the
  // sheet's own Esc handling sits behind this return).
  if (ev.key === "Escape" && dbDrawer) { dbDrawer = ""; renderDbSide(); return; }
  if (dbGridKeydown(t, ev)) return;
  if (dbChromeKeydown(t, ev)) return;
  if (dbFiltersKeydown(ev, t)) return;
  if (dbFormKeydown(t, ev)) return;
  if (dbRedisKeydown(t, ev)) return;
}

/** The sidebar + console half of the delegated click. */
function dbChromeClick(t: Element, ev: MouseEvent): boolean {
  // A drawer closes on any click that is neither inside it nor on its own row — the rows
  // below toggle. Deliberately NOT returning true: the same click still does its own work
  // (opening a table also dismisses the picker).
  if (dbDrawer && !t.closest(".db-drawer") && !t.closest("#dbConnRow") && !t.closest("#dbDatabaseRow")) {
    dbDrawer = "";
    renderDbSide();
  }
  // wrapped: dbRunSql's first parameter is 'explain' — an event object is truthy, so a
  // plain Run click used to quietly run EXPLAIN (docs/22 W5.4 audit). docs/43 M4 moved
  // Run to the toolbar; the delegated id branch stays — the toolbar button carries it.
  if (t.closest("#dbSqlRun")) { void dbRunSql(false); return true; }
  // The connection row toggles its drawer (the owner's ask: the picker slides down inside
  // the sidebar, pushing the tree, instead of floating over it).
  const connRow = t.closest<HTMLElement>("#dbConnRow");
  if (connRow) {
    const d = dbConn();
    if (!d.conns.length) return true;
    ev.stopPropagation();
    dbDrawer = dbDrawer === "conn" ? "" : "conn";
    renderDbSide();
    return true;
  }
  // The database row toggles its own drawer the same way.
  const dbRowBtn = t.closest<HTMLElement>("#dbDatabaseRow");
  if (dbRowBtn) {
    const d = dbConn();
    if (!d.databases || !d.databases.length) return true;
    ev.stopPropagation();
    dbDrawer = dbDrawer === "db" ? "" : "db";
    renderDbSide();
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
    dbOpenTab({ kind: "key", key: keyRow.dataset.rkey || "", type: keyRow.dataset.rtype });
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
  // docs/43 addendum: a section's note row. treemore widens the window by the batch the
  // note itself advertised (dataset.next); treeless drops the memory and the window snaps
  // back to the initial cap. The key carries schema + section so a pg catalog widens one
  // schema's Tables band, not every schema's.
  const tm = t.closest<HTMLElement>("[data-treemore]");
  if (tm && tm.dataset.treemore) {
    const d = dbConn();
    d.treeShown[tm.dataset.treemore] = Number(tm.dataset.next || 0);
    renderDbTables();
    return true;
  }
  const tl = t.closest<HTMLElement>("[data-treeless]");
  if (tl && tl.dataset.treeless) {
    const d = dbConn();
    delete d.treeShown[tl.dataset.treeless];
    renderDbTables();
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
      d.grep = v;
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
  // Only the skeleton's own ids. This used to demand #dbSqlExplain too, and docs/43 M4 took the
  // console's flat row (Explain, Format) into the toolbar's overflow: the early return then fired
  // on every mount and switch, so the pane's ⋯ never showed and a Redis key list's search said
  // "Filter tables" (found on the docs/46 P7 walk). Explain and Format are overflow rows now,
  // and dbMoreItemsForSql leaves them out for a Redis command.
  const grep = $<HTMLInputElement>("dbGrep"), sql = $<HTMLTextAreaElement>("dbSql"), sqlHint = $("dbSqlHint");
  if (!grep || !sql || !sqlHint) return;
  if (dbIsRedis()) {
    grep.placeholder = tr("dataView.filterKeys"); grep.setAttribute("aria-label", tr("dataView.filterKeys"));
    grep.title = tr("dataView.filterKeysScanPattern");
    sql.placeholder = tr("dataView.redisConsolePlaceholder");
    sqlHint.textContent = tr("dataView.redisConsoleHint");
  } else {
    // The placeholder IS the grammar (docs/22 W1.6): comma AND, | OR, * wildcard.
    grep.placeholder = "a*, b|c"; grep.setAttribute("aria-label", tr("dataView.filterTables"));
    grep.title = tr("dataView.filterTablesGrammar");
    sql.placeholder = tr("dataView.selectUpdateDeleteStatements");
    // docs/22 W4.3: the ; split answers one result tab per statement; the blank-line block
    // rule (W1.8) still decides what a single Run covers.
    sqlHint.textContent = tr("dataView.blankLineStartsNew");
  }
  // The pane's ⋯ (#dbMore) is renderDbToolbar's to show: it depends on the open object too,
  // and every caller of this repaints the toolbar right after.
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

/** Switch CONNECTIONS (docs/47 D1, D2): the session on screen parks whole - its strip, its
 *  buffered writes, its catalog, its search and sort - and the target's parked session comes
 *  back; a connection seen for the first time starts at its family's placeholder on its
 *  CONFIGURED database. Nothing is dropped, so nothing is asked. */
async function dbSwitchConn(name: string): Promise<void> {
  const d = dbConn();
  if (!name || name === d.conn) return;
  // The rows-per-page is the operator's word, not the connection's: a fresh session starts at it.
  const last = dbLastTableTab();
  const size = last ? last.pageSize : 0;
  dbActivityPollStop(); // a parked monitor stops asking about sessions nobody is looking at (D8)
  if (!dbSwapConn(name, dbSessionPinned)) {
    dbResetTabsForConn();
    const t = dbTab();
    if (size && t.kind === "table") t.pageSize = size;
  }
  dbShowSession();
}

/* The owner's drawer ask (post-M5): the connection and database pickers slide open under
 * their rows instead of floating over the sidebar as popup menus. One drawer at a time —
 * two stacked pickers in a ~240px sidebar say nothing either one couldn't say alone. */
let dbDrawer: "" | "conn" | "db" = "";

/** Paint one drawer from the same items a menu would have shown (headings stay headings,
 * seps stay hairlines, on marks the current pick). Closed is COLLAPSED, not hidden: the
 * collapse is a grid-rows transition, and hidden would snap it. inert takes the closed
 * drawer out of the tab order and the a11y tree while its rows still exist to animate. */
function dbDrawerFill(host: HTMLElement | null, items: MenuItem[], open: boolean): void {
  if (!host) return;
  host.classList.toggle("open", open);
  if (open) host.removeAttribute("inert");
  else host.setAttribute("inert", "");
  const inner = host.firstElementChild as HTMLElement | null;
  if (!inner) return;
  inner.textContent = "";
  items.forEach((it: MenuItem): void => {
    if (it.sep) { inner.appendChild(h("hr")); return; }
    if (it.heading) { inner.appendChild(h("div", { class: "db-drawer-head" }, it.label)); return; }
    // The current pick is a tick in its own column (docs/46 §3.7, rule 15), the menu's .pick
    // idiom: blue text said "selected" in the colour links and actions use. Every row keeps the
    // column, so the names line up whether or not their row is the current one.
    const b = h("button", {
      class: "db-drow" + (it.on ? " on" : ""), type: "button",
      disabled: !!it.disabled, title: it.title || undefined,
    },
    h("span", { class: "db-drow-tick" }, it.on ? iconNode("check") : null),
    it.mark ? typeTagNode(it.mark) : null,
    h("span", { class: "db-drow-name" }, it.label),
    it.meta != null ? h("span", { class: "db-drow-meta" }, it.meta) : null);
    // Direct onclick, not a data address: the row IS rebuilt by every renderDbSide, so a
    // delegated address would save nothing. The pick closes the drawer first — the
    // switch's own repaint then draws the new connection's sidebar with no drawer open.
    b.onclick = (ev: MouseEvent): void => { ev.stopPropagation(); dbDrawer = ""; it.fn(); };
    inner.appendChild(b);
  });
}

function renderDbSide(): void {
  const d = dbConn();
  const row = $("dbConnRow");
  const dbRow = $("dbDatabaseRow");
  if (!row) return;
  row.textContent = "";
  if (!d.conns.length) {
    row.classList.add("off");
    row.setAttribute("aria-expanded", "false");
    row.appendChild(el("span", "db-row-name", tr("dataView.noDatabaseMcps")));
    if (dbRow) { dbRow.hidden = true; dbRow.setAttribute("aria-expanded", "false"); }
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
  // The dialect follows the MCP sidebar's launch-tag vocabulary (docs/29): a mapped dialect
  // paints its brand glyph, bare (the word rides the aria-label); anything outside the
  // whitelist is the library's mono tag (docs/46 §3.7) - a mark and a word are different
  // things, and the same tag should never be a mark in one sidebar and a word in the other.
  if (cur) {
    const mark = typeTagNode(cur.dialect);
    if (typeof mark === "string") row.appendChild(tag(mark, { mono: true }));
    else {
      (mark as SVGElement).classList.add("db-row-mark");
      row.appendChild(mark as SVGElement);
    }
  }
  const chev = iconNode("chevron-down");
  chev.setAttribute("class", "ic db-row-chev");
  row.appendChild(chev);
  row.setAttribute("aria-expanded", dbDrawer === "conn" ? "true" : "false");
  row.classList.toggle("engaged", dbDrawer === "conn");
  // The connection drawer: same items the old menu showed (docs/20 G5 groups), rendered
  // as rows that push the tree below instead of floating over it.
  dbDrawerFill($("dbConnDrawer"), dbConnMenuItems(d.conns, d.conn || "",
    (name: string): void => { dbDrawer = ""; void dbSwitchConn(name); }), dbDrawer === "conn");
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
      dbRow.setAttribute("aria-expanded", dbDrawer === "db" ? "true" : "false");
      dbRow.classList.toggle("engaged", dbDrawer === "db");
      // The database drawer: primary first with its heading, system last, not-browsable
      // rows disabled with the server's reason one hover away.
      dbDrawerFill($("dbDbDrawer"), dbDatabaseMenuItems(d.databases || [], dbCurrentDatabase(d),
        (name: string): void => { dbDrawer = ""; dbSwitchDatabase(name); }), dbDrawer === "db");
    } else {
      dbRow.setAttribute("aria-expanded", "false");
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
      // The table count rides in meta - the drawer paints it dim at the row's end, where
      // a scan compares names without numbers interleaved.
      label: x.name,
      meta: x.tables != null ? Number(x.tables).toLocaleString(locale()) : undefined,
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
  d.tables = []; d.treeShown = {};
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
 *  selector itself then reads the cache. `quiet` re-reads a catalog already held (a redis
 *  keyspace's per-database key counts move with every write): a failure keeps it, and the
 *  sidebar repaints only when the answer moved. */
async function dbLoadDatabases(quiet?: boolean): Promise<void> {
  const d = dbConn();
  if (!d.conn || (d.databases && !quiet) || dbDatabasesInFlight) return;
  dbDatabasesInFlight = true;
  try {
    const j = await apiJson<ApiDbDatabasesResponse>("/api/db/" + encodeURIComponent(d.conn) + "/databases");
    if (!j) { if (!d.databases) d.databases = []; return; } // the toast already spoke; an empty axis hides the row
    const next = j.databases || [];
    if (d.databases && JSON.stringify(next) === JSON.stringify(d.databases)) return;
    d.databases = next;
  } finally {
    dbDatabasesInFlight = false;
  }
  renderDbSide();
}

/* --- lazy table list ---------------------------------------------------------------------------- */

// One /tables request chain: a slow answer for a page or filter the user just left must be
// dropped, or it would repopulate the sidebar with the stale page (docs/22 closeout audit).
const dbTablesReq = dbReqGuard();

async function dbLoadTables(quiet?: boolean): Promise<void> {
  const d = dbConn();
  if (!d.conn) return;
  const box = $("dbTables");
  // docs/47 D5: a quiet re-read keeps the list on screen until the answer is in.
  if (box && !quiet) { box.textContent = ""; box.appendChild(el("div", "db-hint", tr("dataView.loading"))); }
  let q = "/api/db/" + encodeURIComponent(d.conn) + "/tables?limit=" + DB_TREE_FETCH_LIMIT;
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
  // The session may have parked while this was on its way: the answer is still its catalog,
  // but the list on screen is another connection's and must not be touched.
  const shown = dbConn() === d;
  if (!j) { if (shown && !quiet && $("dbTables")) $("dbTables").textContent = ""; return; }
  const next = j.tables || [];
  // The same catalog: the tree stays exactly as painted (its scroll, its open bands).
  if (quiet && d.tablesTotal === (j.total || 0) && d.more === !!j.more &&
    JSON.stringify(next) === JSON.stringify(d.tables)) return;
  d.tables = next;
  d.tablesTotal = j.total || 0;
  d.more = !!j.more;
  if (shown) renderDbTables();
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
        foot2.appendChild(btn(tr("dataView.more"), { title: tr("dataView.continueScan"), data: { keysmore: "" } }));
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
      rowNode: (s: GroupSlice<ApiDbTableRow>): HTMLElement => {
        return dbSectionBand(s as { name: DbTreeSection; rows: ApiDbTableRow[] }, secCollapsed);
      },
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
  // docs/43 addendum: the pager died with whole-catalog fetches. The footer now states the
  // one fact that can still be true - the tree holds everything, or the fetch cap clipped it
  // and grep (server-side, same grammar) is the way past. Browsing is not paging anymore.
  if (d.tablesTotal) {
    if (d.more) {
      foot.appendChild(el("span", "", tr("dataView.treeTruncated", {
        n: d.tables.length.toLocaleString(locale()),
      })));
    } else {
      foot.appendChild(el("span", "", tr("dataView.treeTotal", {
        n: d.tablesTotal.toLocaleString(locale()),
      })));
    }
  }
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

/** One redis key row, one line (docs/43 M2): the type's glyph, the name (the REMAINDER under its
 *  band's prefix - the band already said the prefix; a leaf row says the whole key), its type
 *  and expiry right. The full key stays the row's title and its address ([data-rkey]). */
function dbKeyRow(k: ApiDbRedisKeyRow, label: string, selKey: string | null): HTMLElement {
  const b = el("button", "db-table db-key-row" + (k.key === selKey ? " sel" : ""));
  b.appendChild(iconNode(redisTypeGlyph(k.type)));
  let meta = k.type;
  if (k.ttl! >= 0) meta += " · ttl " + k.ttl + "s";
  b.title = k.key + " · " + meta;
  b.dataset.rkey = k.key;
  b.dataset.rtype = k.type; // handed to the tab it opens, so its card wears the same glyph
  b.appendChild(el("span", "db-table-name", label));
  b.appendChild(el("span", "db-table-meta", k.type + (k.ttl! >= 0 ? " · " + k.ttl + "s" : "")));
  return b;
}

/** The one-shot catalog fetch asks for this many rows (docs/43 addendum). 2,000 covers a
 *  thousand-table database with margin to spare; the server clamps hand-written queries at
 *  5,000, and anything bigger is grep territory - the footer says so when it happens. */
const DB_TREE_FETCH_LIMIT = 2000;
/** Rows painted per section band before the note row takes over (docs/43 addendum). The
 *  cap is a DOM budget, not a data limit: the band's badge still numbers the full list. */
const DB_TREE_ROW_CAP = 200;
/** Rows one "show more" click appends (docs/43 addendum). Incremental on purpose: the note
 *  stays "show N more" until the END of the list, and only then offers "show fewer" - the
 *  owner's ask, verbatim: page your way down, and say fewer when there is nothing left. */
const DB_TREE_ROW_STEP = 500;

/** One band's render budget (docs/43 addendum). A search renders every MATCH - the server
 *  already filtered, and a match the operator typed must not hide behind an expander. The
 *  window memory is keyed "schema/section" so a pg catalog widens one schema's Tables band
 *  without dragging every other schema's along. */
function dbTreeCap(sec: { name: DbTreeSection; rows: ApiDbTableRow[] }):
  { keep: number; note: HTMLElement | null } {
  const d = dbConn();
  const total = sec.rows.length;
  if (!total || d.grep || total <= DB_TREE_ROW_CAP) return { keep: total, note: null };
  const key = (sec.rows[0].schema || "") + "/" + sec.name;
  const shown = Math.min(d.treeShown[key] || DB_TREE_ROW_CAP, total);
  if (shown >= total) {
    const less = el("button", "db-tree-cap") as HTMLButtonElement;
    less.type = "button";
    less.dataset.treeless = key;
    less.appendChild(el("span", "db-table-name", tr("dataView.treeShowFewer")));
    return { keep: total, note: less };
  }
  const next = Math.min(shown + DB_TREE_ROW_STEP, total);
  const more = el("button", "db-tree-cap") as HTMLButtonElement;
  more.type = "button";
  more.dataset.treemore = key;
  // The handler trusts this instead of recomputing from state: the note is rebuilt by every
  // render, so the count it carries is always the one the operator was shown.
  more.dataset.next = String(next);
  more.appendChild(el("span", "db-table-name", tr("dataView.treeShowAll", {
    n: (next - shown).toLocaleString(locale()),
  })));
  return { keep: shown, note: more };
}

/** One Tables / Views / Routines band (docs/43 M2) — the tree's leaf container, shared by
 *  MySQL (at the root) and pg (nested inside its schema band). The band owns the list's sort
 *  (its ellipsis, mockup B) and — on the Tables section only — the New table +. */
function dbSectionBand(sec: { name: DbTreeSection; rows: ApiDbTableRow[] },
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
    capRows: (rows: ApiDbTableRow[]): { keep: number; note: HTMLElement | null } => {
      return dbTreeCap({ name: sec.name, rows });
    },
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

export { DB_HISTORY_KEY, DB_HISTORY_MAX, DB_PAGE_SIZES, dbClearSel, dbConnLabel, dbCurrentDatabase, dbDatabaseMenuItems, dbDialectOf, dbDropAllEdits, dbDropEdits, dbFilterMatches, dbFocusedColumnValue, dbFkJump, dbFkOpen, dbIsPg, dbKnownSchemas, dbLoadDatabases, dbLoadTables, dbOkToDrop, dbOkToLeave, dbPaneChange, dbPaneClick, dbPaneInput, dbPaneKeydown, dbPending, dbPendingAll, dbPkKey, dbPkVals, dbResultKey, dbSectionLabel, dbSessionPinned, dbSortDatabases, dbSortMenu, dbSwitchConn, dbSwitchDatabase, loadDbView, renderDbSide, renderDbTables, renderDbView };
// dbSectionEmpty stays module-private: the acceptance suite reaches it through the band's
// emptyText hook, which is the only contract it has.

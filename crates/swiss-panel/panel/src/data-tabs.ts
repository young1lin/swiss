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

import { visitObject } from "./data-objects.js";
import { paintContext } from "./data-agent.js";
import type { DbConnState, DbFilterTerm, DbSqlTab, DbTab, DbTableTab, DbTabSpec } from "./types/state.js";
import type { MenuItem } from "./types/dom.js";
import type { ApiDbDatabase } from "./types/api.js";
import { $, iconNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import { dbActiveIndex, dbConn, dbIsMounted, dbOnSqlTextChanged, dbParkedNames, dbParkedTabs, dbResetTabs, dbSetActive, dbSqlTab, dbTab, dbTabs, freshTab } from "./db-state.js";
import { dbClosedAdd, dbClosedAdopt, dbClosedById, dbClosedClear, dbClosedDrop, dbClosedEmpty, dbClosedFor, dbClosedLocalFor, dbClosedLocalForget, dbClosedLocalOnly, dbClosedRemember, dbClosedTake } from "./data-closed.js";
import type { DbClosedTab } from "./data-closed.js";
import {
  dbClosedEntry, dbConsoleAdopt, dbConsoleClearClosed, dbConsoleFlush, dbConsoleForget, dbConsoleOnSettled, dbConsolePut, dbConsoleRebase,
  dbConsoleRecord, dbConsoleStage, dbConsoleStoreList, dbConsoleStoreOn, dbConsoleSyncState,
} from "./data-console-store.js";
import type { ConsoleRecord } from "./data-console-sync.js";
import { dbIsRedis, dbLoadRedisValue } from "./data-browsers.js";
import { dbIsMongo, mongoAfterTabSwitch, mongoCurrentDb } from "./data-mongo.js";
import { dbActivityPollStop, dbActivityStart } from "./data-activity.js";
import { dbPubsubStart, dbPubsubStop } from "./data-pubsub.js";
import { dbRestoreData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbSqlPaint, renderDbFilters } from "./data-filters.js";
import { renderDbBar } from "./data-sql.js";
import { dbLoadDetail } from "./data-structure.js";
import { dbIsPg, renderDbTables } from "./data-view.js";
import { dbKeyShown, redisTypeGlyph } from "./data-tree.js";
import { tr, trn } from "./i18n.js";
import { popupMenu } from "./ui/menu.js";
import { heldDot } from "./ui/status.js";
import { objTab } from "./ui/tab.js";
import { dayKey, dayLabel, timeLabel } from "./ui/timeline.js";
import { locale } from "./i18n.js";

/* ================================================================================================
   The object tab strip (SPEC §data.tabs).

   The Data page used to hold exactly one object: opening a table, jumping through a foreign key
   or running a query all overwrote whatever was in front of the operator, filters and buffered
   edits included. This module is the strip that lets it hold several — a table, a second table,
   a console, the activity monitor — and the policy that keeps the set small: a small
   cap, a least-recently-used eviction that never touches a tab holding work, and a close
   that asks before it drops buffered writes.

   Where the pieces live: db-state.ts owns the array and the active index (and nothing else about
   them), this file owns open/close/activate/evict and the strip's markup, and every renderer
   keeps reading the ACTIVE tab through dbTab() exactly as it did when there was only one.

   Two shapes are worth naming before reading further:

   - The PLACEHOLDER. The strip never holds zero tabs, because a null active tab would put a
     branch in every renderer. What it holds instead, before the first open and after the last
     close, is a fresh table (or key) tab pointing at nothing. dbTabVisible() keeps it off the
     strip and dbOpenTab() drops it as soon as a real object arrives, so it costs the operator
     nothing and the empty state (renderDbGrid's emptyNode) is what they actually see.
   - The LRU stamp. `touched` is bumped on birth and on every activation, and eviction reads
     only that. It is the whole memory story of D3/D4: a capped set of tabs, each holding at most one page
     of rows, and a background tab drops even that.
   ================================================================================================ */

/** SPEC §data.tabs: how many objects the page holds at once. The protection is not the number —
 *  it is that eviction only takes CLEAN background tabs and a strip where everything holds
 *  work refuses the open outright. Eight was the value set when the strip could not scroll
 *  and had no overflow menu; SPEC §data.tabs pins both exits, so the cap can follow the
 *  operator's workload instead: twelve cards at floor width fit a 1440px window, and
 *  twelve pages of rows is the memory budget the cap exists to defend. */

const DB_TAB_MAX = 12;

/** The placeholder a connection's strip falls back to: the object its family browses — a
 *  redis key, a MongoDB collection, a table everywhere else. */
function dbHomeTab(): DbTab {
  if (dbIsRedis()) return freshTab("key");
  return dbIsMongo() ? freshTab("coll") : freshTab("table");
}

/** A redis or MongoDB console runs commands, not SQL; the card and the + button say which. */
function dbConsoleSpeaksCommands(): boolean {
  return dbIsRedis() || dbIsMongo();
}

/* --- the server-owned console (SPEC §data.agent Tabs) -------------------------------------------- */

/** Stage one open console's current text and name for the store. Every edit path calls this;
 *  the store's debounce coalesces a burst of keystrokes into one write. A no-op while the
 *  gateway has no store, so the console keeps working from the page's own snapshot. */
export function dbConsoleTouch(tab: DbSqlTab): void {
  const conn = dbConn().conn;
  if (!conn || tab.analysisId || !dbConsoleStoreOn(conn)) return;
  dbConsoleStage(conn, tab, dbTabTitle(tab));
}

// Every dbSetSqlText edit (typing, formatting, an agent frame) reaches the store through here;
// the store's debounce coalesces the burst. Registered once, at module load.
dbOnSqlTextChanged((tab) => { dbConsoleTouch(tab); });
// A save finishes after the keystroke that staged it. The strip has to learn the refusal
// then, or the red dot waits for some later, unrelated repaint.
dbConsoleOnSettled((conn) => { if (dbConn().conn === conn) renderDbTabs(); });

/** A server record as a strip tab: identity, text, agent slot and lifetime, never reformatted -
 *  the bytes stored are the reader's. */
function dbConsoleTabOf(record: ConsoleRecord): DbSqlTab {
  const t = freshTab("sql");
  t.id = record.id;
  t.sqlText = record.sql;
  t.agent = record.agent === true;
  if (record.agentTab) t.agentTab = record.agentTab;
  const fallback = tr(dbConsoleSpeaksCommands() ? "dataTabs.command" : "dataTabs.sql");
  if (record.tab && record.tab !== fallback) t.custom = record.tab;
  t.createdAt = record.createdAt || null;
  t.updatedAt = record.updatedAt || null;
  t.revision = record.revision;
  return t;
}

/** Put the open records the page does not already have back on the strip, oldest first, and
 *  bring the newest in front. True when the strip changed. */
function dbAdoptConsoles(records: ConsoleRecord[]): boolean {
  const tabs = dbTabs();
  const open = new Set(tabs.filter((t: DbTab): boolean => t.kind === "sql").map((t) => (t as DbSqlTab).id));
  const add = records.filter((r) => r.open && !open.has(r.id))
    .sort((a, b) => (a.createdAt || 0) - (b.createdAt || 0) || a.id.localeCompare(b.id))
    .map(dbConsoleTabOf);
  if (!add.length) return false;
  const next: DbTab[] = tabs.filter(dbTabVisible).concat(add);
  dbResetTabs(next, next.length - 1);
  return true;
}

/** Where this connection's own legacy drafts are imported from, once: localStorage entries and
 *  open consoles born before the store answered. Create-only - a record already on the server
 *  always wins, and a refused import keeps the local copy for the next visit. */
async function dbConsoleMigrate(conn: string, records: ConsoleRecord[]): Promise<void> {
  const known = new Set(records.map((r) => r.id));
  const migrated: string[] = [];
  for (const e of dbClosedLocalFor(conn)) {
    if (known.has(e.id)) { migrated.push(e.id); continue; }
    const result = await dbConsolePut(conn, {
      id: e.id, tab: e.custom || "", agent: e.agent === true, agentTab: e.agentTab || null,
      open: false, sql: e.text, revision: 0, createdAt: e.createdAt || 0, updatedAt: e.updatedAt || 0, closedAt: e.at,
    }, "create");
    if (result.ok) { dbClosedAdd(conn, dbClosedEntry(conn, result.console)); migrated.push(e.id); }
    else if (result.conflict) { migrated.push(e.id); continue; } // another page imported it first
    else { toast(tr("dataTabs.consoleMigrateFailed"), true); break; }
  }
  if (!dbClosedLocalForget(conn, migrated)) toast(tr("dataTabs.storageFull"), true);
  for (const t of dbTabs()) {
    if (t.kind !== "sql" || t.analysisId || t.revision || known.has(t.id)) continue;
    const result = await dbConsolePut(conn, dbConsoleRecord(conn, t, dbTabTitle(t)), "create");
    if (!result.ok) {
      if (result.conflict) dbConsoleRebase(conn, { ...dbConsoleRecord(conn, t, dbTabTitle(t)), revision: 0 });
      continue;
    }
    t.revision = result.console.revision;
    t.createdAt = result.console.createdAt;
    t.updatedAt = result.console.updatedAt;
  }
}

/** Enter a connection: read the store once, restore its open consoles, fill Recently closed and
 *  import this browser's older drafts. A gateway without the store leaves the page exactly on
 *  its local snapshot. */
const dbConsoleLoaded = new Set<string>();
const dbConsoleLoading = new Set<string>();
export async function dbConsolesLoad(conn: string): Promise<void> {
  if (!conn || dbConsoleLoaded.has(conn) || dbConsoleLoading.has(conn)) return;
  dbConsoleLoading.add(conn);
  try {
    const records = await dbConsoleStoreList(conn);
    if (!records) { dbClosedLocalOnly(conn); return; }
    if (dbConn().conn !== conn) return;
    dbClosedAdopt(conn, records.filter((r) => !r.open).map((r) => dbClosedEntry(conn, r)));
    for (const t of dbTabs()) {
      if (t.kind !== "sql") continue;
      const record = records.find((r) => r.id === t.id && r.open);
      if (!record) continue;
      t.revision = record.revision;
      if (record.createdAt) t.createdAt = record.createdAt;
      if (record.updatedAt) t.updatedAt = record.updatedAt;
    }
    const adopted = dbAdoptConsoles(records);
    await dbConsoleMigrate(conn, records);
    if (dbConn().conn !== conn) return;
    dbConsoleLoaded.add(conn);
    if (adopted) dbAfterTabSwitch();
    else { renderDbTabs(); renderDbTables(); }
  } finally {
    dbConsoleLoading.delete(conn);
  }
}

/* --- pure: what a tab is, and what it is worth ---------------------------------------------------- */

/** A tab that has never been pointed at an object — the fresh literal the view falls back to so
 *  dbTab() is never null. It is not on the strip and it does not count against the cap. A sql or
 *  activity tab is never a placeholder: opening one IS the act. Pure. */
function dbTabPlaceholder(t: DbTab): boolean {
  if (t.kind === "table") return !t.table;
  if (t.kind === "key") return !t.redisKey;
  if (t.kind === "coll") return !t.coll;
  return false;
}

/** Everything on the strip the operator can see and close. Pure. */
function dbTabVisible(t: DbTab): boolean {
  return !dbTabPlaceholder(t);
}

/** What one tab is holding back from the database: the row grid's buffered updates, deletes and
 *  inserts, or a key tab's buffered field edits (SPEC §data.redis). This is the number the close
 *  confirm and the page-leave guard both quote, so it counts writes and nothing else. Pure. */
function dbTabPending(t: DbTab): number {
  if (t.kind === "table") {
    return Object.keys(t.updates).length + Object.keys(t.deletes).length + t.inserts.length;
  }
  if (t.kind === "key") {
    const b = t.redisEdits;
    return b ? Object.keys(b.updates).length + Object.keys(b.deletes).length + b.inserts.length : 0;
  }
  return 0;
}

/** The whole strip's buffered writes — what `canLeave()` reports in ONE question (SPEC §data.tabs).
 *  The old guard asked the single record, so a buffer parked on a background tab walked out
 *  silently. Pure. */
function dbTabsPending(tabs: DbTab[]): number {
  return tabs.reduce((n: number, t: DbTab): number => { return n + dbTabPending(t); }, 0);
}

/** May the cap drop this tab to make room? Buffered writes never may — that is D3's promise.
 *  Neither may a console: its ID can already be in use even before any text is entered. Pure. */
function dbTabEvictable(t: DbTab): boolean {
  if (dbTabPending(t)) return false;
  if (t.kind === "sql" && !t.analysisId) return false;
  return true;
}

/** Which tab makes room for the next one, or null when none may go (SPEC §data.tabs). The victim is
 *  the least recently touched tab that is neither the active one nor holding work. Returning
 *  null is not a failure — it is the refusal dbOpenTab turns into a message, because dropping
 *  an edit to open a table is never the right trade. Pure. */
function dbEvictTarget(tabs: DbTab[], activeIndex: number): number | null {
  let best = -1;
  tabs.forEach((t: DbTab, i: number): void => {
    if (i === activeIndex || !dbTabEvictable(t)) return;
    if (best < 0 || t.touched < tabs[best].touched) best = i;
  });
  return best < 0 ? null : best;
}

/** Same filter set, term for term — the tail of a table tab's identity. Pure. */
function dbFiltersSame(a: DbFilterTerm[], b: DbFilterTerm[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((t: DbFilterTerm, i: number): boolean => {
    return t.column === b[i].column && t.op === b[i].op && t.value === b[i].value;
  });
}

/** Is this open tab the object the spec asks for? The dedupe rule, and the reason the FK jump
 *  and the sidebar click behave differently on the same table (SPEC §data.tabs):
 *
 *  - a plain open ("show me orders") lands on whatever tab already holds orders, filters and
 *    buffered edits included — a second `orders` tab would only confuse the strip;
 *  - a jump carries its own filter, so it matches only a tab already showing exactly that.
 *    Anything else and it would rewrite the filter the operator is reading, which is the
 *    regression this whole tier exists to end (SPEC §data.grid).
 *
 *  A console, an activity monitor and a pub/sub page have no address: there is one of each per
 *  connection, so a second open activates the first. Pure. */
function dbTabMatches(t: DbTab, spec: DbTabSpec): boolean {
  if (t.kind !== spec.kind) return false;
  if (t.kind === "table" && spec.kind === "table") {
    if (t.table !== spec.table || t.schema !== spec.schema) return false;
    return !spec.filters || dbFiltersSame(t.filters, spec.filters);
  }
  if (t.kind === "key" && spec.kind === "key") return t.redisKey === spec.key;
  if (t.kind === "coll" && spec.kind === "coll") return t.db === spec.db && t.coll === spec.coll;
  return true;
}

/** The name on the tab: the operator's own rename first, then the qualified table, the
 * key, or the word for the kind. Pure. */
function dbTabTitle(t: DbTab): string {
  // A rename is a label the operator chose; it wins everywhere the derived title shows
  // (card, overflow, confirms) but addresses nothing — identity lives on the fields below.
  if (t.custom) return t.custom;
  if (t.kind === "table") return (t.schema ? t.schema + "." : "") + (t.table || tr("dataTabs.untitled"));
  if (t.kind === "key") return t.redisKey != null ? dbKeyShown(t.redisKey) : tr("dataTabs.untitled");
  // A collection of the database being browsed reads bare; one left open from another database
  // keeps its db. qualifier, the same rule a MySQL table outside the primary follows.
  if (t.kind === "coll") {
    if (!t.coll) return tr("dataTabs.untitled");
    return (t.db && t.db !== mongoCurrentDb() ? t.db + "." : "") + t.coll;
  }
  // A console opened on a redis connection runs redis commands, not SQL — the box's own
  // placeholder has always said so (SET k v · GET k …), and a card reading "SQL" over it was the
  // one label on the strip that named something the tab cannot do.
  if (t.kind === "sql") return tr(dbConsoleSpeaksCommands() ? "dataTabs.command" : "dataTabs.sql");
  if (t.kind === "pubsub") return tr("dataTabs.pubsub");
  return tr("dataTabs.activity");
}

/** The leading type glyph (SPEC §data.tabs): one per kind, so the strip is readable before
 *  a single name is. A key's card wears its type's glyph - the one its sidebar row leads with:
 *  the type the row handed over at open, then the landed value's (the truth, should the key
 *  have been recreated as another type); the redis mark when neither is known. Pure. */
function dbTabGlyph(t: DbTab): string {
  if (t.kind === "table") return "table";
  if (t.kind === "key") {
    const type = t.redisValue ? t.redisValue.type : t.redisKeyType;
    return type ? redisTypeGlyph(type) : "redis";
  }
  if (t.kind === "coll") return "braces";
  if (t.kind === "sql") return "terminal";
  if (t.kind === "pubsub") return "activity";
  return "clock";
}

/** The scope a card's schema qualifier is redundant against (SPEC §data.tabs, revised by
 *  SPEC §data.databases): on MySQL the scope is the CONFIGURED primary database, not whichever
 *  database is being browsed — a secondary database's cards carry their db.table qualifier
 *  everywhere (row, card, overflow menu), the primary's never do. A pg connection spans
 *  schemas: the scope is the schema the operator picked, public by default. Null when
 *  nothing names one — then every qualifier stays. Pure. */
export function dbTabScope(c: DbConnState, pg: boolean): string | null {
  if (pg) return c.schemaFilter || "public";
  const primary = (c.databases || []).find((x: ApiDbDatabase): boolean => x.primary);
  if (primary) return primary.name;
  return c.tables.length ? c.tables[0].schema : null;
}

/** The name the card SHOWS: the object's own name inside the current scope, qualified only
 *  when the qualifier tells two cards apart (SPEC §data.tabs). On MySQL the old title spent
 *  13 of a card's 210px on an `acme_app_dev.` prefix that carried nothing, and the table
 *  name — the part that actually distinguishes cards — was the part being truncated. The
 *  full name stays one hover away (the card's title) and in every confirm, which must name
 *  the object precisely. Pure. */
function dbTabCardTitle(t: DbTab, scope: string | null): string {
  const full = dbTabTitle(t);
  // A rename is already the operator's shortest name for the card — no scope trimming.
  if (t.custom) return full;
  if (t.kind !== "table" || !t.schema || !scope) return full;
  if (t.schema === scope) return t.table || tr("dataTabs.untitled");
  return full;
}

/* --- the strip ------------------------------------------------------------------------------------ */

/** The rename input a card's name span becomes while dbTabRenaming points at it. Handlers
 *  are property-assigned after h() builds the node (h strips function-valued props): the
 *  input is rebuilt with every strip paint, so direct wiring costs nothing, and its keys
 *  must stop before the strip's own handlers — Enter commits, Escape cancels, blur
 *  commits (the click that stole focus was a decision to move on). */
function dbTabRenameInput(t: DbTab, scope: string | null): HTMLInputElement {
  const inp = h("input", {
    class: "db-tab-rename", type: "text", value: dbTabCardTitle(t, scope),
    aria: { label: tr("dataTabs.rename") },
  });
  inp.onclick = (ev: MouseEvent): void => { ev.stopPropagation(); };
  inp.onkeydown = (ev: KeyboardEvent): void => {
    if (ev.key === "Enter") {
      ev.preventDefault(); ev.stopPropagation();
      dbTabRenameCommit((ev.currentTarget as HTMLInputElement).value);
    } else if (ev.key === "Escape") {
      ev.preventDefault(); ev.stopPropagation();
      dbTabRenaming = null; renderDbTabs();
    }
  };
  inp.onblur = (ev: Event): void => { dbTabRenameCommit((ev.currentTarget as HTMLInputElement).value); };
  return inp;
}

/** Paint the strip. Card tabs (SPEC §data.tabs): a leading type glyph, the name, a trailing
 *  ×, the whole row sitting on --sidebar with the active card lifted to --bg and joined to the
 *  grid below it. Every control is an address answered by #pane's delegated click (SPEC §panel.toolchain) —
 *  the index it carries is re-read against live state when the click lands. */
function renderDbTabs(): void {
  const strip = $("dbTabStrip");
  if (!strip) return;
  // No connection, no objects: the strip would be a band holding one "+ SQL" for a console
  // that has nothing to run against.
  strip.hidden = !dbConn().conn;
  // Every repaint replaces the card the keyboard was standing on, and the browser answers a
  // destroyed activeElement by falling back to <body> — so one arrow key moved the strip and
  // the next one went nowhere, because the handler only listens while focus is ON the strip
  // (found live on 19998 during a walk of SPEC §data.tabs: the tab's own fetch repaints on arrival,
  // a second after the move). Focus is put back only when it was here to begin with: a repaint
  // while the user types in a filter box or the grid must never pull them out of it.
  const hadFocus = strip.contains(document.activeElement);
  const tabs = dbTabs();
  const active = dbActiveIndex();
  // The qualifier a card can drop (SPEC §data.tabs): read once per paint, shared by the cards
  // and the overflow menu so the two never disagree about an object's name.
  const scope = dbTabScope(dbConn(), dbIsPg());
  const kids = tabs.map((t: DbTab, i: number) => {
    if (!dbTabVisible(t)) return null;
    const n = dbTabPending(t);
    const filters = t.kind === "table" ? t.filters.length : 0;
    // A refused console save is state the reader must see: the local draft is intact, but the
    // server has a newer record until they resolve it from the card's own menu.
    const conflict = t.kind === "sql" && dbConsoleSyncState(dbConn().conn, t.id) === "failed";
    // The library's object tab (SPEC §panel.pages): the shape Terminal's sessions wear too. It carries
    // the roving tabindex and the close; the rename (right-click) swaps the name for an input
    // for as long as dbTabRenaming points here (built by dbTabRenameInput - handlers are
    // property-assigned there, h() strips function-valued props by design).
    return objTab({
      name: i === dbTabRenaming ? dbTabRenameInput(t, scope) : dbTabCardTitle(t, scope),
      selected: i === active,
      icon: dbTabGlyph(t),
      title: dbTabTitle(t),
      data: { dbtab: String(i) },
      count: filters ? { n: filters, title: trn(filters, "dataTabs.nFilters.one", "dataTabs.nFilters.other") } : null,
      mark: conflict
        ? heldDot(tr("dataTabs.consoleConflict"), "err")
        : n ? heldDot(trn(n, "dataTabs.nBufferedChanges.one", "dataTabs.nBufferedChanges.other")) : null,
      close: { label: tr("dataTabs.closeTab"), data: { dbtabx: String(i) } },
    });
  });
  // SPEC §data.tabs: the strip is two zones — a scrolling run of cards and a PINNED end.
  // The exits used to sit inside the scroll, so a full strip scrolled its own "+" out of
  // reach (the owner's screenshot): opening a console became a hunt for an offscreen button.
  // The cards scroll; the overflow button and the "+" never do.
  const scroller = h("div", { class: "db-tabstrip-scroll" }, kids);
  // A vertical wheel over a horizontal strip is the first gesture every trackpad user tries;
  // take it as the horizontal scroll it means, but only when there is somewhere to scroll to
  // (otherwise the page's own scroll answers, as it should). Property-assigned on a node this
  // repaint just built — the next rebuild replaces the node, so nothing stacks.
  scroller.onwheel = (ev: WheelEvent): void => {
    if (!ev.deltaY || scroller.scrollWidth <= scroller.clientWidth) return;
    ev.preventDefault();
    scroller.scrollLeft += ev.deltaY;
  };
  const shown = tabs.filter(dbTabVisible).length;
  fill(strip, scroller,
    h("div", { class: "db-tabstrip-end" },
      // The overflow menu is the one place the whole set stays visible once the strip scrolls
      // (SPEC §data.tabs): every open object, then the consoles closed on this connection and
      // the browser's bulk-close idioms. Nothing open and nothing closed → nothing to list → the
      // button is not offered.
      h("button", {
        class: "db-tab-more", type: "button", hidden: !shown && !dbClosedFor(dbConn().conn).length,
        title: tr("dataTabs.openObjects"), aria: { haspopup: "menu" }, data: { dbtabmenu: "" },
      }, iconNode("chevron-down")),
      // The console opens from the strip's own end: it is an object like the rest, and the one
      // gesture that ADDS to the strip belongs on it (rule 19).
      h("button", {
        class: "db-tab-add", type: "button",
        title: tr(dbConsoleSpeaksCommands() ? "dataTabs.newCommandConsoleTitle" : "dataTabs.newConsoleTitle"),
        data: { dbtabadd: "sql" },
      }, iconNode("plus"), h("span", null, tr(dbConsoleSpeaksCommands() ? "dataTabs.command" : "dataTabs.sql")))));
  if (hadFocus) dbFocusActiveTab();
  dbScrollActiveTab();
}

/** Bring the active card into the visible run after every paint (SPEC §data.tabs): the
 *  keyboard walk, Ctrl+Tab and the overflow menu can all land on a card the strip has
 *  scrolled out of sight, and a "current" tab nobody can see is not current. Guarded — the
 *  vitest DOM has no layout and may not implement scrollIntoView at all (house rule 5);
 *  "nearest" never moves a card that is already on screen. */
function dbScrollActiveTab(): void {
  const strip = $("dbTabStrip");
  const card = strip ? strip.querySelector<HTMLElement>(".otab.sel") : null;
  if (card && typeof card.scrollIntoView === "function") {
    card.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
}

/** #pane's delegated click for the strip (SPEC §panel.toolchain). The × is checked before the card so a
 *  close never reads as an activate. ev is the live event when the caller has it: opening
 *  the overflow menu must stop the document click that would close it again. */
function dbTabsClick(t: Element, ev?: MouseEvent): boolean {
  const x = t.closest<HTMLElement>("[data-dbtabx]");
  if (x) { void dbCloseTab(Number(x.dataset.dbtabx)); return true; }
  const menuBtn = t.closest<HTMLElement>("[data-dbtabmenu]");
  if (menuBtn) {
    if (ev) ev.stopPropagation();
    popupMenu(menuBtn.getBoundingClientRect(), dbTabsMenuItems(), { filter: tr("dataTabs.findClosed") });
    return true;
  }
  const add = t.closest<HTMLElement>("[data-dbtabadd]");
  if (add) { dbOpenTabForce({ kind: "sql" }); return true; }
  const card = t.closest<HTMLElement>("[data-dbtab]");
  if (card) { dbActivateTab(Number(card.dataset.dbtab)); return true; }
  return false;
}

/** The middle-button close (SPEC §data.tabs): the browser-tab idiom, on the ×'s exact path —
 *  the dirty-tab confirm included. #pane answers auxclick (a middle press fires BOTH click
 *  and auxclick, so only auxclick may act) and hands the target here. */
function dbTabsAuxClick(t: Element): boolean {
  const x = t.closest<HTMLElement>("[data-dbtabx]");
  if (x) { void dbCloseTab(Number(x.dataset.dbtabx)); return true; }
  const card = t.closest<HTMLElement>("[data-dbtab]");
  if (card) { void dbCloseTab(Number(card.dataset.dbtab)); return true; }
  return false;
}

/* The card being renamed, or null (the right-click's Rename): while set, that card's name
 * span is an input instead. Not state that survives a connection switch — a repaint from
 * dbResetTabs clears it, which is the right collapse for "the strip under me changed". */
let dbTabRenaming: number | null = null;

/** Begin the rename: repaint the strip with the card's name as a focused, selected input. */
function dbTabRenameStart(i: number): void {
  dbTabRenaming = i;
  renderDbTabs();
  const strip = $("dbTabStrip");
  const input = strip ? strip.querySelector<HTMLInputElement>(".db-tab-rename") : null;
  if (input && typeof input.focus === "function") { input.focus(); input.select(); }
}

/** Commit the rename onto the tab (empty cancels — a blank card says nothing). */
function dbTabRenameCommit(value: string): void {
  const i = dbTabRenaming;
  dbTabRenaming = null;
  const v = value.trim();
  if (i == null) return;
  const t = dbTabs()[i];
  if (t && t.custom !== (v || undefined)) {
    t.custom = v || undefined;
    if (t.kind === "sql") { t.updatedAt = Date.now(); dbConsoleTouch(t); }
  }
  renderDbTabs();
}

/** Resolve a refused console save, never silently: either take the record the server holds now,
 *  or keep the reader's draft and write it over that record on purpose. */
async function dbConsoleResolve(id: string, choice: "server" | "draft"): Promise<void> {
  const conn = dbConn().conn;
  if (!conn) return;
  const records = await dbConsoleStoreList(conn, true);
  if (dbConn().conn !== conn) return;
  const record = records ? records.find((r) => r.id === id) || null : null;
  const at = dbTabs().findIndex((t: DbTab): boolean => t.kind === "sql" && t.id === id);
  if (at < 0) return;
  const t = dbTabs()[at] as DbSqlTab;
  if (choice === "server") {
    if (!record) { toast(tr("dataTabs.idMissing"), true); return; }
    if (t.sqlText !== record.sql && !confirm(tr("dataTabs.reloadConsoleConfirm"))) return;
    t.sqlText = record.sql;
    if (record.createdAt) t.createdAt = record.createdAt;
    if (record.updatedAt) t.updatedAt = record.updatedAt;
    t.revision = record.revision;
    dbConsoleAdopt(conn, record);
    if (dbSqlTab() === t) {
      const ta = $<HTMLTextAreaElement>("dbSql");
      if (ta) ta.value = t.sqlText;
      dbSqlPaint();
    }
    renderDbTabs();
    toast(tr("dataTabs.consoleReloaded"));
    return;
  }
  // The draft wins: catch up to the record's revision, or to 0 when it is gone so the write
  // creates a record rather than failing forever.
  const base = record || { ...dbConsoleRecord(conn, t, dbTabTitle(t)), revision: 0 };
  dbConsoleRebase(conn, base);
  const ok = await dbConsoleFlush(conn);
  renderDbTabs();
  toast(tr(ok ? "dataTabs.consoleDraftSaved" : "dataTabs.consoleSaveFailed"), !ok);
}

/** #pane's delegated contextmenu for the strip: the card's own menu (the owner's ask —
 *  rename, close to the left, close to the right; the browser-tab trio). Left and right
 * are read from the CLICKED card, not the active one — a right-click does not activate,
 * and "the tabs left of the one I pointed at" is what the operator means by it. */
function dbTabsContext(t: Element, ev: MouseEvent): boolean {
  const card = t.closest<HTMLElement>("[data-dbtab]");
  if (!card) return false;
  // While the card is a rename input, the browser's own text menu is the right one.
  if (t.closest(".db-tab-rename")) return false;
  ev.preventDefault();
  ev.stopPropagation();
  const i = Number(card.dataset.dbtab);
  const tabs = dbTabs();
  const target = tabs[i];
  const index = (): number => dbTabs().indexOf(target);
  const hasLeft = tabs.slice(0, i).some(dbTabVisible);
  const hasRight = tabs.slice(i + 1).some(dbTabVisible);
  const items: MenuItem[] = [
    { label: tr("dataTabs.rename"), fn: (): void => { if (index() >= 0) dbTabRenameStart(index()); } },
  ];
  if (target && target.kind === "sql") {
    const id = target.id;
    items.push({ label: tr("dataTabs.copyId"), fn: (): void => { void navigator.clipboard.writeText(id).catch(() => toast(tr("dataTabs.copyFailed"), true)); } });
  }
  if (target && target.kind === "sql" && dbConsoleSyncState(dbConn().conn, target.id) === "failed") {
    const id = target.id;
    items.push(
      { sep: true },
      { label: tr("dataTabs.reloadConsoleServer"), fn: (): void => { void dbConsoleResolve(id, "server"); } },
      { label: tr("dataTabs.keepConsoleDraft"), fn: (): void => { void dbConsoleResolve(id, "draft"); } },
    );
  }
  items.push(
    { sep: true },
    { label: tr("dataTabs.closeLeft"), disabled: !hasLeft, fn: (): void => { if (index() >= 0) void dbCloseToLeft(index()); } },
    { label: tr("dataTabs.closeRight"), disabled: !hasRight, fn: (): void => { if (index() >= 0) void dbCloseToRight(index()); } },
  );
  popupMenu(card.getBoundingClientRect(), items);
  return true;
}

/** The overflow menu's rows (SPEC §data.tabs): one per open object, in strip order, the
 *  current one ticked — the strip's whole set in one place that never scrolls away. The type
 *  glyph and the dirty dot ride the row so the menu also answers "which of these is holding
 *  my work" without opening anything. Behind a separator sit Recently closed (the consoles
 *  closed on this connection, a row opening their list) and the bulk closes; the destructive
 *  one is last and red (rule 4). */
function dbTabsMenuItems(): MenuItem[] {
  const tabs = dbTabs();
  const active = dbActiveIndex();
  const scope = dbTabScope(dbConn(), dbIsPg());
  const items: MenuItem[] = [];
  tabs.forEach((t: DbTab, i: number): void => {
    if (!dbTabVisible(t)) return;
    items.push({
      label: dbTabCardTitle(t, scope),
      title: dbTabTitle(t) + (t.kind === "sql" ? " · " + t.id + "\n" + t.sqlText.slice(0, 400) : ""),
      pick: true, on: i === active,
      icon: dbTabGlyph(t), dot: !!dbTabPending(t),
      fn: (): void => { const now = dbTabs().indexOf(t); if (now >= 0) dbActivateTab(now); },
    });
  });
  const closed = dbClosedFor(dbConn().conn);
  const recent: MenuItem | null = closed.length ? {
    label: tr("dataTabs.recentlyClosed"), title: trn(closed.length, "dataTabs.nClosed.one", "dataTabs.nClosed.other"), affordance: "chevron-right",
    fn: (): void => {
      const at = document.querySelector<HTMLElement>("#dbTabStrip [data-dbtabmenu]");
      const rows = dbClosedMenuItems();
      if (at && rows.length) popupMenu(at.getBoundingClientRect(), rows, rows.length > 9 ? { filter: tr("dataTabs.findClosed") } : undefined);
    },
  } : null;
  if (!items.length) return recent ? [recent] : items;
  items.push({ sep: true });
  if (recent) items.push(recent);
  if (tabs.filter(dbTabVisible).length > 1) {
    items.push({ label: tr("dataTabs.closeOthers"), fn: dbCloseOthers });
  }
  if (tabs.slice(active + 1).some(dbTabVisible)) {
    items.push({ label: tr("dataTabs.closeRight"), fn: (): void => { dbCloseToRight(active); } });
  }
  items.push({ label: tr("dataTabs.closeAll"), danger: true, fn: dbCloseAllTabs });
  return items;
}

/** A closed console's row: its name and its first line, the text itself one hover away. */
function dbClosedLabel(e: DbClosedTab): string {
  const line = (e.text.split("\n").find((l: string): boolean => !!l.trim()) || "").trim();
  const name = e.custom || tr(dbConsoleSpeaksCommands() ? "dataTabs.command" : "dataTabs.sql");
  return name + " · " + (line.length > 48 ? line.slice(0, 47) + "…" : line);
}

/** Forget the connection's closed history. The server deletes first on a store-backed
 *  connection, so a failure keeps the list instead of hiding entries that would come back on
 *  the next visit; the local list is cleared only after its own write succeeded. */
async function dbClearClosedHistory(): Promise<void> {
  const conn = dbConn().conn;
  if (!conn) return;
  if (dbConsoleStoreOn(conn)) {
    if (!(await dbConsoleClearClosed(conn))) return;
    dbClosedEmpty(conn);
  } else if (!dbClosedClear(conn)) {
    toast(tr("dataTabs.storageFull"), true);
    return;
  }
  renderDbTabs();
}

/** Recently closed's list (SPEC §data.tabs): the connection's closed consoles, newest first, each
 *  brought back by a click; Clear forgets them. */
function dbClosedMenuItems(): MenuItem[] {
  const closed = dbClosedFor(dbConn().conn);
  if (!closed.length) return [];
  const rows: MenuItem[] = [];
  let day = "";
  const exact = (n: number | null | undefined): string => n && Number.isFinite(n) && n > 0 && n <= 8.64e15 ? new Date(n).toLocaleString(locale()) : tr("ui.timeUnknown");
  for (const e of closed) {
    const at = e.updatedAt && Number.isFinite(e.updatedAt) && e.updatedAt > 0 && e.updatedAt <= 8.64e15 ? e.updatedAt : NaN;
    const group = Number.isFinite(at) ? dayKey(at) : "unknown";
    if (day !== group) { rows.push({ label: dayLabel(at, Date.now()), heading: true, fn: () => {} }); day = group; }
    rows.push({
      label: dbClosedLabel(e) + " · " + (Number.isFinite(at) ? timeLabel(at) : tr("ui.timeUnknown")),
      title: tr("dataTabs.lifetime", { id: e.id, created: exact(e.createdAt), updated: exact(e.updatedAt), closed: exact(e.at) }) + "\n" + e.text.slice(0, 400), icon: "terminal",
      fn: (): void => { if (dbClosedById(dbConn().conn, e.id)) void dbRestoreClosed(e.id); },
    });
  }
  rows.push({ sep: true });
  rows.push({ label: tr("dataTabs.clearClosed"), danger: true, fn: (): void => { void dbClearClosedHistory(); } });
  return rows;
}

/** Open a closed console as a tab from the page's own copy - the frame path, where a restore
 *  must land before the statement that named it arrives. `revision` keeps the store's address
 *  so the next save compares against the record instead of recreating it. */
function dbOpenClosedCached(e: DbClosedTab, revision: number = e.revision || 0): DbSqlTab | null {
  dbOpenTabForce({ kind: "sql" });
  const t = dbSqlTab();
  if (!t) return null;
  t.id = e.id;
  t.sqlText = e.text;
  t.revision = revision;
  t.createdAt = e.createdAt ?? null;
  t.updatedAt = e.updatedAt ?? null;
  if (e.custom) t.custom = e.custom;
  const name = e.agentTab || "";
  if (e.agent && !dbTabs().some((x: DbTab): boolean => x !== t && x.kind === "sql" && x.agent === true && (x.agentTab || "") === name)) {
    t.agent = true;
    if (name) t.agentTab = name;
  }
  // The tab was built blank and seeded the box before its text was known: put the rescued text
  // back in front now, or the console would show an empty editor for a real draft.
  if (dbSqlTab() === t) {
    const ta = $<HTMLTextAreaElement>("dbSql");
    if (ta) ta.value = t.sqlText;
    dbSqlPaint();
  }
  renderDbTabs();
  return t;
}

/** Open a closed console and take it off Recently closed. A frame has to land before the
 *  statement that named it, so this stays synchronous: a store-backed list drops the cached
 *  row (the edit that follows saves it open), and the local list removes the entry. A refused
 *  local write leaves the row where it was and opens nothing. */
function dbClaimClosed(e: DbClosedTab): DbSqlTab | null {
  if (dbConsoleStoreOn(e.conn)) dbClosedDrop(e.conn, e.id);
  else if (!dbClosedTake(e.id, e.conn)) { toast(tr("dataTabs.storageFull"), true); return null; }
  return dbOpenClosedCached(e);
}

/** Bring a closed console back as a new tab in front: its text and its name, and - while no open
 *  tab is the agent's of that name - the agent's again, so its next run lands there. It keeps
 *  the id it was filed under, which is the tab's own: a tool holding that id still addresses
 *  this console after the round trip (data-closed.ts).
 *
 *  On a store-backed connection the server record is read fresh first: reopening a stale copy
 *  and then saving it would overwrite a newer edit another page made. A record removed
 *  meanwhile is gone, and the page says so instead of reopening a dead id. */
async function dbRestoreClosed(id: string): Promise<void> {
  const live = dbTabs().findIndex(t => t.kind === "sql" && t.id === id);
  if (live >= 0) { dbActivateTab(live); return; }
  const conn = dbConn().conn;
  if (!conn) return;
  if (dbConsoleStoreOn(conn)) {
    const records = await dbConsoleStoreList(conn, true);
    if (dbConn().conn !== conn) return;
    const record = records ? records.find((r) => r.id === id) || null : null;
    if (!record) {
      dbClosedDrop(conn, id);
      renderDbTabs();
      toast(tr("dataTabs.idMissing"), true);
      return;
    }
    const result = await dbConsolePut(conn, { ...record, open: true, closedAt: null }, "update");
    if (dbConn().conn !== conn) return;
    if (!result.ok) { toast(result.error || tr("dataTabs.consoleSaveFailed"), true); return; }
    dbClosedDrop(conn, id);
    if (!dbOpenClosedCached(dbClosedEntry(conn, result.console), result.console.revision)) return;
    dbAfterTabSwitch();
    return;
  }
  const e = dbClosedById(conn, id);
  if (!e) { renderDbTabs(); return; }
  if (!dbClosedTake(id, conn)) { toast(tr("dataTabs.storageFull"), true); return; }
  if (!dbOpenClosedCached(e)) return;
  dbAfterTabSwitch();
}

/** File the consoles among these tabs in the browser (the connection has no server store). */
function dbFileClosedNow(gone: DbTab[]): boolean {
  const conn = dbConn().conn;
  if (!conn) return true;
  const sqlTabs = gone.filter((t: DbTab): t is DbSqlTab => t.kind === "sql" && !t.analysisId);
  if (!sqlTabs.length) return true;
  const saved = dbClosedRemember(sqlTabs.map((tab: DbSqlTab) => ({ conn, tab })));
  if (!saved) toast(tr("dataTabs.storageFull"), true);
  return saved;
}

/** File the consoles among these tabs on a store-backed connection. The close is a
 *  compare-and-swap first: a refused write keeps the tab open rather than closing work the
 *  server never received. */
async function dbFileClosedStored(gone: DbTab[]): Promise<boolean> {
  const conn = dbConn().conn;
  if (!conn) return true;
  const sqlTabs = gone.filter((t: DbTab): t is DbSqlTab => t.kind === "sql" && !t.analysisId);
  if (!sqlTabs.length) return true;
  for (const tab of sqlTabs) {
    const record = dbConsoleRecord(conn, tab, dbTabTitle(tab));
    const result = await dbConsolePut(conn, { ...record, open: false }, tab.revision ? "update" : "create");
    if (!result.ok) { toast(result.error || tr("dataTabs.consoleSaveFailed"), true); return false; }
    dbClosedAdd(conn, dbClosedEntry(conn, result.console));
    dbConsoleForget(conn, tab.id);
  }
  return true;
}

/* A page reload or a closed browser tab takes every console with it: each one with text, the
   parked sessions' too, is filed as closed on its way out, so the next visit brings it back.
   A store-backed console is not filed closed - it stays open on the server and the next visit
   restores the strip it left; pending edits are flushed, best effort, before the page goes. */
function dbFileClosedOnLeave(): boolean {
  const gone: { conn: string; tab: DbSqlTab }[] = [];
  const live = dbConn().conn;
  if (live) {
    if (dbConsoleStoreOn(live)) void dbConsoleFlush(live, true);
    else for (const t of dbTabs()) if (t.kind === "sql") gone.push({ conn: live, tab: t });
  }
  const names = dbParkedNames();
  dbParkedTabs().forEach((strip: DbTab[], i: number): void => {
    if (dbConsoleStoreOn(names[i])) { void dbConsoleFlush(names[i], true); return; }
    for (const t of strip) if (t.kind === "sql") gone.push({ conn: names[i], tab: t });
  });
  return gone.length ? dbClosedRemember(gone) : true;
}
// A node test has no window, or a stand-in (globalThis) with no events to listen to.
if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
  window.addEventListener("pagehide", () => { dbFileClosedOnLeave(); });
  window.addEventListener("beforeunload", (ev) => { if (!dbFileClosedOnLeave()) { ev.preventDefault(); ev.returnValue = ""; } });
}

/** Close a set of tabs by index, asking ONCE when any of them holds work (SPEC §data.tabs).
 *  A single × names the one object it drops; a bulk close asks the one question the operator
 *  can actually answer — "N buffered changes would go" — and a refusal keeps every tab
 *  exactly where it was. */
function dbCloseBatch(ix: number[]): void {
  const tabs = dbTabs();
  const targets = ix.filter((i: number): boolean => { return i >= 0 && i < tabs.length; });
  if (!targets.length) return;
  const pending = targets.reduce((n: number, i: number): number => { return n + dbTabPending(tabs[i]); }, 0);
  if (pending && !confirm(tr("dataTabs.closeDiscardsBatch", {
    n: trn(pending, "dataTabs.nBufferedChanges.one", "dataTabs.nBufferedChanges.other"),
  }))) return;
  const gone = targets.map((i: number): DbTab => tabs[i]);
  const proceed = (): void => {
    targets.forEach((i: number): void => {
      if (tabs[i].kind === "activity") dbActivityPollStop();
      if (tabs[i].kind === "pubsub") dbPubsubStop();
    });
    const closing = new Set(targets);
    const live = dbTab();
    const next = tabs.filter((_: DbTab, j: number): boolean => { return !closing.has(j); });
    if (!next.length) next.push(dbHomeTab());
    const at = next.indexOf(live);
    dbResetTabs(next, at >= 0 ? at : 0);
    dbAfterTabSwitch();
  };
  const conn = dbConn().conn;
  if (conn && dbConsoleStoreOn(conn)) { void dbFileClosedStored(gone).then((ok) => { if (ok) proceed(); }); return; }
  if (dbFileClosedNow(gone)) proceed();
}

/** "Close others" — everything but the active tab. */
function dbCloseOthers(): void {
  const active = dbActiveIndex();
  const ix: number[] = [];
  dbTabs().forEach((t: DbTab, i: number): void => {
    if (i !== active && dbTabVisible(t)) ix.push(i);
  });
  void dbCloseBatch(ix);
}

/** "Close to the left" of the PIVOT card (the right-clicked one; the overflow menu
 *  passes the active one) — the browser-tab trio's missing third. */
function dbCloseToLeft(pivot: number): void {
  const ix: number[] = [];
  dbTabs().forEach((t: DbTab, i: number): void => {
    if (i < pivot && dbTabVisible(t)) ix.push(i);
  });
  void dbCloseBatch(ix);
}

/** "Close to the right" of the pivot card — the tabs the strip shows after it. */
function dbCloseToRight(pivot: number): void {
  const ix: number[] = [];
  dbTabs().forEach((t: DbTab, i: number): void => {
    if (i > pivot && dbTabVisible(t)) ix.push(i);
  });
  void dbCloseBatch(ix);
}

/** "Close all" — back to the placeholder, i.e. the pane's empty state. */
function dbCloseAllTabs(): void {
  const ix: number[] = [];
  dbTabs().forEach((t: DbTab, i: number): void => {
    if (dbTabVisible(t)) ix.push(i);
  });
  void dbCloseBatch(ix);
}

/* Ctrl+Tab / Ctrl+Shift+Tab cycle the strip (rule 19: the frequent gesture must
   not need discovering). Document-level and capture-phase, because the console textarea would
   otherwise swallow a Tab before the pane's delegated listener saw it; the dbIsMounted() guard is
   what keeps a Data shortcut from firing on any other page. Note for the reader: Chrome reserves
   Ctrl+Tab for its own tab bar, so in an ordinary browser tab the browser acts as well — the
   strip's × and the cards themselves are the gesture that always works. */
document.addEventListener("keydown", (ev: KeyboardEvent): void => {
  if (!dbIsMounted()) return;
  if (ev.key === "Tab" && ev.ctrlKey && !ev.altKey && !ev.metaKey) {
    ev.preventDefault();
    ev.stopPropagation();
    dbCycleTab(ev.shiftKey);
    return;
  }
  // The arrows work only once focus is ON the strip — a bare ArrowLeft anywhere else in the
  // view belongs to the grid's cell cursor.
  if (ev.key !== "ArrowLeft" && ev.key !== "ArrowRight") return;
  if (ev.ctrlKey || ev.altKey || ev.metaKey) return;
  const on = ev.target instanceof Element && ev.target.closest(".db-tabstrip");
  if (!on) return;
  ev.preventDefault();
  // The move repaints, and the repaint is what puts the keyboard back on the card that took the
  // place — including the repaint the new tab's own fetch triggers a second later.
  dbCycleTab(ev.key === "ArrowLeft");
}, true);

/** Put the keyboard back on the active card after a repaint replaced the node it was on. */
function dbFocusActiveTab(): void {
  const strip = $("dbTabStrip");
  if (!strip) return;
  const card = strip.querySelector<HTMLElement>(".otab.sel");
  if (card) card.focus();
}

/* --- open / close / activate ----------------------------------------------------------------------- */

/** The most recently touched table tab — where a new table tab inherits its page size from. The
 *  rows-per-page pick is the operator's density, not one table's property, and it survived every
 *  table switch when there was a single record. */
function dbLastTableTab(): DbTableTab | null {
  let best: DbTableTab | null = null;
  dbTabs().forEach((t: DbTab): void => {
    if (t.kind !== "table") return;
    if (!best || t.touched > best.touched) best = t;
  });
  return best;
}

/** A fresh tab for one spec. The literals themselves come from freshTab (db-state.ts owns the
 *  shapes); this only writes the address the spec names. */
function dbBuildTab(spec: DbTabSpec): DbTab {
  if (spec.kind === "table") {
    const t = freshTab("table");
    t.table = spec.table;
    t.schema = spec.schema;
    if (spec.filters) t.filters = spec.filters;
    const last = dbLastTableTab();
    if (last) t.pageSize = last.pageSize;
    return t;
  }
  if (spec.kind === "key") {
    const t = freshTab("key");
    t.redisKey = spec.key;
    t.redisKeyType = spec.type || null;
    return t;
  }
  if (spec.kind === "coll") {
    const t = freshTab("coll");
    t.db = spec.db;
    t.coll = spec.coll;
    return t;
  }
  if (spec.kind === "sql") return freshTab("sql");
  if (spec.kind === "pubsub") return freshTab("pubsub");
  return freshTab("activity");
}

/** The one way an object reaches the pane (SPEC §data.tabs — dbOpenTable and dbFkOpen were the same
 *  ritual written twice). Already open → activated, never duplicated. Strip full → the least
 *  recently used tab with nothing in it makes room; if every tab holds work, the open is REFUSED
 *  and says so, because silently dropping an operator's edits to show them a table is not a
 *  trade the page gets to make. */
function dbOpenTab(spec: DbTabSpec): void {
  const tabs = dbTabs();
  const at = tabs.findIndex((t: DbTab): boolean => { return dbTabMatches(t, spec); });
  if (at >= 0) { dbActivateTab(at); return; }
  const live = dbTab();
  // The placeholder is not an object: the first real open takes its slot rather than sitting
  // beside it. Its index is dropped with it, so the active index is recomputed below.
  const next = tabs.filter(dbTabVisible);
  if (next.length >= DB_TAB_MAX) {
    const victim = dbEvictTarget(next, next.indexOf(live));
    if (victim === null) {
      toast(tr("dataTabs.fullCloseOne", { n: DB_TAB_MAX }), true);
      return;
    }
    // Not silent (SPEC §data.tabs): a clean card vanishing without a word reads as "the page
    // ate my tab". Say which one made room — and that it held nothing unsaved.
    toast(tr("dataTabs.evictedForRoom", { name: dbTabTitle(next[victim]) }));
    next.splice(victim, 1);
  }
  dbLeaveTab(live);
  next.push(dbBuildTab(spec));
  dbResetTabs(next, next.length - 1);
  dbAfterTabSwitch();
}

/** The strip's own "+" (the owner's rule): one press, one NEW console — no dedupe, no
 *  refusal. The dedupe above is right for objects ("show me orders" lands on orders), but
 *  a console has no address, so for the + it read as "the button is broken": pressing it
 *  with a console open just activated the old one. The cap still helps when it can (a
 *  clean tab is evicted with the usual toast), but when every tab holds work the + goes
 *  OVER instead of refusing — the operator explicitly asked for this tab, and DB_TAB_MAX
 *  defends a memory budget, not the right to say no to a direct request. */
function dbOpenTabForce(spec: DbTabSpec): void {
  const tabs = dbTabs();
  const live = dbTab();
  const next = tabs.filter(dbTabVisible);
  if (next.length >= DB_TAB_MAX) {
    const victim = dbEvictTarget(next, next.indexOf(live));
    if (victim !== null) {
      toast(tr("dataTabs.evictedForRoom", { name: dbTabTitle(next[victim]) }));
      next.splice(victim, 1);
    }
  }
  dbLeaveTab(live);
  next.push(dbBuildTab(spec));
  dbResetTabs(next, next.length - 1);
  dbAfterTabSwitch();
  // A new console exists on the server from birth: its id can already be handed to a tool,
  // and a blank console is work the reader may type into after a reload.
  const made = dbSqlTab();
  if (made) dbConsoleTouch(made);
}

/** Drop every tab open on one table — what DROP TABLE leaves behind (SPEC §data,
 *  now that the pane is a strip). Nothing of the dropped table may linger, and a BACKGROUND tab
 *  is exactly where it would: renderDbTables refreshes only the left list. No confirm on the
 *  buffered writes it discards — they address rows that no longer exist, so the only question a
 *  prompt could ask is whether to keep writing to a table that is gone. */
function dbDropTableTabs(table: string | null, schema: string | null): void {
  const tabs = dbTabs();
  const live = dbTab();
  const next = tabs.filter((t: DbTab): boolean => {
    return !(t.kind === "table" && t.table === table && t.schema === schema);
  });
  if (next.length === tabs.length) return;
  if (!next.length) next.push(dbHomeTab());
  const at = next.indexOf(live);
  dbResetTabs(next, at >= 0 ? at : Math.min(dbActiveIndex(), next.length - 1));
  // The pane is only re-seeded when the object in front of the user changed; a drop of some
  // background table repaints the strip and nothing else.
  if (at >= 0) renderDbTabs(); else dbAfterTabSwitch();
}

/** Bring the i'th tab to the pane. `byKeyboard` says the move came from the strip's own arrows
 *  or Ctrl+Tab, which is the one case where the console must NOT take the caret: the walk has to
 *  be able to continue past it. */
function dbActivateTab(i: number, byKeyboard?: boolean): void {
  if (i === dbActiveIndex()) { renderDbTabs(); return; }
  dbLeaveTab(dbTab());
  dbSetActive(i);
  dbAfterTabSwitch(byKeyboard);
}

/** Close it. A tab holding buffered writes asks first (SPEC §data.tabs) and a refusal keeps it
 *  exactly where it was. The active tab's place goes to its right neighbour, or its left when
 *  there is no right; closing the last one leaves the placeholder, so the pane falls back to the
 *  empty state instead of to nothing. */
function dbCloseTab(i: number): void {
  const tabs = dbTabs();
  const t = tabs[i];
  if (!t) return; // a stale render's address — the strip moved under the click
  const n = dbTabPending(t);
  if (n && !confirm(tr("dataTabs.closeDiscards", {
    name: dbTabTitle(t),
    n: trn(n, "dataTabs.nBufferedChanges.one", "dataTabs.nBufferedChanges.other"),
  }))) return;
  const proceed = (): void => {
    if (t.kind === "activity") dbActivityPollStop();
    if (t.kind === "pubsub") dbPubsubStop();
    const active = dbActiveIndex();
    const next = tabs.filter((_: DbTab, j: number): boolean => { return j !== i; });
    let idx = active;
    if (i === active) idx = Math.min(i, next.length - 1);
    else if (i < active) idx = active - 1;
    if (!next.length) next.push(dbHomeTab());
    dbResetTabs(next, Math.max(0, idx));
    dbAfterTabSwitch();
  };
  const conn = dbConn().conn;
  if (conn && dbConsoleStoreOn(conn)) { void dbFileClosedStored([t]).then((ok) => { if (ok) proceed(); }); return; }
  if (dbFileClosedNow([t])) proceed();
}

/** Ctrl+Tab's move: forward through the strip, wrapping. */
function dbCycleTab(back: boolean): void {
  const n = dbTabs().length;
  if (n < 2) return;
  dbActivateTab(((dbActiveIndex() + (back ? -1 : 1)) % n + n) % n, true);
}

/** Every open object belongs to the connection it was opened under: the switch closes the whole
 *  strip and starts the new connection at its own family's placeholder (a redis connection
 *  browses keys, a MongoDB one collections, everything else tables). The page size carries
 *  across, the same preference the single record never reset either. */
function dbResetTabsForConn(): boolean | Promise<boolean> {
  const closing = dbTabs();
  const finish = (): boolean => {
    const keep = dbLastTableTab();
    dbActivityPollStop();
    dbPubsubStop();
    const next = dbHomeTab();
    if (next.kind === "table" && keep) next.pageSize = keep.pageSize;
    dbResetTabs([next], 0);
    return true;
  };
  // A database switch drops the strip: its consoles are filed as closed, to bring back.
  const conn = dbConn().conn;
  if (conn && dbConsoleStoreOn(conn)) return dbFileClosedStored(closing).then((ok) => ok ? finish() : false);
  if (!dbFileClosedNow(closing)) return false;
  return finish();
}

/** SPEC §data.tabs: a tab going to the background keeps what is cheap — its filters, its offset, its
 *  order, its buffered edits — and drops what is expensive, the page of rows. Coming back
 *  re-fetches from exactly those coordinates, which is why a filtered, paged, half-edited table
 *  looks untouched on return. */
function dbLeaveTab(t: DbTab): void {
  if (t.kind === "table" && t.table) t.data = null;
  if (t.kind === "coll" && t.coll) t.docs = null;
  if (t.kind === "activity") dbActivityPollStop();
  // A pub/sub page in the background holds no subscription (SPEC §data.pubsub).
  if (t.kind === "pubsub") dbPubsubStop();
}

/** Everything the pane owes a new active tab: the console's text, the chrome, the grid, and the
 *  fetch for whatever the tab dropped while it was in the background. */
function dbAfterTabSwitch(byKeyboard?: boolean): void {
  const t = dbTab();
  // The console is the sql tab's body now (SPEC §data.tabs), so the textarea is seeded from the tab
  // being shown — otherwise the previous console's text sits in the new one's box.
  const ta = $<HTMLTextAreaElement>("dbSql");
  if (ta) ta.value = t.kind === "sql" ? t.sqlText : "";
  dbSqlPaint();
  const connection = dbConn().conn;
  if (t.kind === "table" && t.table && connection) visitObject(connection, dbConn().database, { name: t.table, schema: t.schema });
  renderDbTabs();
  renderDbTables();
  renderDbToolbar();
  renderDbFilters();
  renderDbGrid();
  renderDbBar();
  paintContext();
  if (t.kind === "table" && t.table) {
    if (!t.data) void dbRestoreData();
    if (!t.detail) void dbLoadDetail();
  }
  if (t.kind === "key" && t.redisKey && !t.redisValue) void dbLoadRedisValue(t.redisKey);
  if (t.kind === "coll") mongoAfterTabSwitch(t);
  // The 5s poll follows "is that tab active" (SPEC §data.tabs): a backgrounded monitor stops
  // asking the server about sessions nobody is looking at.
  if (t.kind === "activity") dbActivityStart();
  if (t.kind === "pubsub") dbPubsubStart();
  // Landing on a console by pointer means "I am going to type here"; landing on it mid-walk
  // does not. Taking the caret on the keyboard path turned the arrows into a one-way trip:
  // the next ArrowLeft moved the textarea's caret instead of the strip (found live, T2 walk).
  if (t.kind === "sql" && !t.analysisId && ta && !byKeyboard) ta.focus();
}

export {
  DB_TAB_MAX, dbActivateTab, dbAfterTabSwitch, dbClosedMenuItems, dbCloseAllTabs, dbCloseBatch, dbCloseToLeft, dbCloseOthers,
  dbCloseTab, dbCloseToRight, dbCycleTab, dbDropTableTabs, dbEvictTarget, dbFiltersSame, dbLastTableTab,
  dbClaimClosed, dbOpenClosedCached, dbOpenTab, dbOpenTabForce, dbResetTabsForConn, dbRestoreClosed, dbTabCardTitle, dbTabGlyph, dbTabMatches, dbTabPending,
  dbTabPlaceholder, dbTabsAuxClick, dbTabsClick, dbTabsContext, dbTabsMenuItems, dbTabsPending,
  dbTabTitle, dbTabVisible, dbTabEvictable, renderDbTabs,
};

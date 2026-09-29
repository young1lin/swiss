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

                                                                                                
                                               
                                                    
import { $, iconNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import { dbActiveIndex, dbConn, dbIsMounted, dbResetTabs, dbSetActive, dbTab, dbTabs, freshTab } from "./db-state.js";
import { dbIsRedis, dbLoadRedisValue } from "./data-browsers.js";
import { dbActivityPollStop, dbActivityStart } from "./data-activity.js";
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

/* ================================================================================================
   The object tab strip (docs/42 T2).

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

/** docs/42 D3: how many objects the page holds at once. The protection is not the number —
 *  it is that eviction only takes CLEAN background tabs and a strip where everything holds
 *  work refuses the open outright. Eight was the value set when the strip could not scroll
 *  and had no overflow menu; docs/43 M1 D6 pins both exits, so the cap can follow the
 *  operator's workload instead: twelve cards at floor width fit a 1440px window, and
 *  twelve pages of rows is the memory budget the cap exists to defend. */

const DB_TAB_MAX = 12;

/* --- pure: what a tab is, and what it is worth ---------------------------------------------------- */

/** A tab that has never been pointed at an object — the fresh literal the view falls back to so
 *  dbTab() is never null. It is not on the strip and it does not count against the cap. A sql or
 *  activity tab is never a placeholder: opening one IS the act. Pure. */
function dbTabPlaceholder(t       )          {
  if (t.kind === "table") return !t.table;
  if (t.kind === "key") return !t.redisKey;
  return false;
}

/** Everything on the strip the operator can see and close. Pure. */
function dbTabVisible(t       )          {
  return !dbTabPlaceholder(t);
}

/** What one tab is holding back from the database: the row grid's buffered updates, deletes and
 *  inserts, or a key tab's buffered field edits (docs/22 W3.3). This is the number the close
 *  confirm and the page-leave guard both quote, so it counts writes and nothing else. Pure. */
function dbTabPending(t       )         {
  if (t.kind === "table") {
    return Object.keys(t.updates).length + Object.keys(t.deletes).length + t.inserts.length;
  }
  if (t.kind === "key") {
    const b = t.redisEdits;
    return b ? Object.keys(b.updates).length + Object.keys(b.deletes).length + b.inserts.length : 0;
  }
  return 0;
}

/** The whole strip's buffered writes — what `canLeave()` reports in ONE question (docs/42 D5).
 *  The old guard asked the single record, so a buffer parked on a background tab walked out
 *  silently. Pure. */
function dbTabsPending(tabs         )         {
  return tabs.reduce((n        , t       )         => { return n + dbTabPending(t); }, 0);
}

/** May the cap drop this tab to make room? Buffered writes never may — that is D3's promise.
 *  Neither may a console with text in it: an unrun query the operator is still composing is
 *  work they would have to type again, and it is far cheaper to keep than a page of rows. Pure. */
function dbTabEvictable(t       )          {
  if (dbTabPending(t)) return false;
  if (t.kind === "sql" && t.sqlText.trim()) return false;
  return true;
}

/** Which tab makes room for the next one, or null when none may go (docs/42 D3). The victim is
 *  the least recently touched tab that is neither the active one nor holding work. Returning
 *  null is not a failure — it is the refusal dbOpenTab turns into a message, because dropping
 *  an edit to open a table is never the right trade. Pure. */
function dbEvictTarget(tabs         , activeIndex        )                {
  let best = -1;
  tabs.forEach((t       , i        )       => {
    if (i === activeIndex || !dbTabEvictable(t)) return;
    if (best < 0 || t.touched < tabs[best].touched) best = i;
  });
  return best < 0 ? null : best;
}

/** Same filter set, term for term — the tail of a table tab's identity. Pure. */
function dbFiltersSame(a                , b                )          {
  if (a.length !== b.length) return false;
  return a.every((t              , i        )          => {
    return t.column === b[i].column && t.op === b[i].op && t.value === b[i].value;
  });
}

/** Is this open tab the object the spec asks for? The dedupe rule, and the reason the FK jump
 *  and the sidebar click behave differently on the same table (docs/42 D6):
 *
 *  - a plain open ("show me orders") lands on whatever tab already holds orders, filters and
 *    buffered edits included — a second `orders` tab would only confuse the strip;
 *  - a jump carries its own filter, so it matches only a tab already showing exactly that.
 *    Anything else and it would rewrite the filter the operator is reading, which is the
 *    docs/22 W5.2 regression this whole tier exists to end.
 *
 *  A console and an activity monitor have no address: there is one of each per connection, so a
 *  second open activates the first. Pure. */
function dbTabMatches(t       , spec           )          {
  if (t.kind !== spec.kind) return false;
  if (t.kind === "table" && spec.kind === "table") {
    if (t.table !== spec.table || t.schema !== spec.schema) return false;
    return !spec.filters || dbFiltersSame(t.filters, spec.filters);
  }
  if (t.kind === "key" && spec.kind === "key") return t.redisKey === spec.key;
  return true;
}

/** The name on the tab: the operator's own rename first, then the qualified table, the
 * key, or the word for the kind. Pure. */
function dbTabTitle(t       )         {
  // A rename is a label the operator chose; it wins everywhere the derived title shows
  // (card, overflow, confirms) but addresses nothing — identity lives on the fields below.
  if (t.custom) return t.custom;
  if (t.kind === "table") return (t.schema ? t.schema + "." : "") + (t.table || tr("dataTabs.untitled"));
  if (t.kind === "key") return t.redisKey != null ? dbKeyShown(t.redisKey) : tr("dataTabs.untitled");
  // A console opened on a redis connection runs redis commands, not SQL — the box's own
  // placeholder has always said so (SET k v · GET k …), and a card reading "SQL" over it was the
  // one label on the strip that named something the tab cannot do.
  if (t.kind === "sql") return tr(dbIsRedis() ? "dataTabs.command" : "dataTabs.sql");
  return tr("dataTabs.activity");
}

/** The leading type glyph (swiss-ui-design §1.3): one per kind, so the strip is readable before
 *  a single name is. A key's card wears its type's glyph - the one its sidebar row leads with:
 *  the type the row handed over at open, then the landed value's (the truth, should the key
 *  have been recreated as another type); the redis mark when neither is known. Pure. */
function dbTabGlyph(t       )         {
  if (t.kind === "table") return "table";
  if (t.kind === "key") {
    const type = t.redisValue ? t.redisValue.type : t.redisKeyType;
    return type ? redisTypeGlyph(type) : "redis";
  }
  if (t.kind === "sql") return "terminal";
  return "clock";
}

/** The scope a card's schema qualifier is redundant against (docs/43 M1 D3, revised by
 *  docs/43 M3): on MySQL the scope is the CONFIGURED primary database, not whichever
 *  database is being browsed — a secondary database's cards carry their db.table qualifier
 *  everywhere (row, card, overflow menu), the primary's never do. A pg connection spans
 *  schemas: the scope is the schema the operator picked, public by default. Null when
 *  nothing names one — then every qualifier stays. Pure. */
export function dbTabScope(c             , pg         )                {
  if (pg) return c.schemaFilter || "public";
  const primary = (c.databases || []).find((x               )          => x.primary);
  if (primary) return primary.name;
  return c.tables.length ? c.tables[0].schema : null;
}

/** The name the card SHOWS: the object's own name inside the current scope, qualified only
 *  when the qualifier tells two cards apart (docs/43 M1 D3). On MySQL the old title spent
 *  13 of a card's 210px on an `acme_app_dev.` prefix that carried nothing, and the table
 *  name — the part that actually distinguishes cards — was the part being truncated. The
 *  full name stays one hover away (the card's title) and in every confirm, which must name
 *  the object precisely. Pure. */
function dbTabCardTitle(t       , scope               )         {
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
function dbTabRenameInput(t       , scope               )                   {
  const inp = h("input", {
    class: "db-tab-rename", type: "text", value: dbTabCardTitle(t, scope),
    aria: { label: tr("dataTabs.rename") },
  });
  inp.onclick = (ev            )       => { ev.stopPropagation(); };
  inp.onkeydown = (ev               )       => {
    if (ev.key === "Enter") {
      ev.preventDefault(); ev.stopPropagation();
      dbTabRenameCommit((ev.currentTarget                    ).value);
    } else if (ev.key === "Escape") {
      ev.preventDefault(); ev.stopPropagation();
      dbTabRenaming = null; renderDbTabs();
    }
  };
  inp.onblur = (ev       )       => { dbTabRenameCommit((ev.currentTarget                    ).value); };
  return inp;
}

/** Paint the strip. Card tabs (swiss-ui-design §1.3): a leading type glyph, the name, a trailing
 *  ×, the whole row sitting on --sidebar with the active card lifted to --bg and joined to the
 *  grid below it. Every control is an address answered by #pane's delegated click (docs/37 R5) —
 *  the index it carries is re-read against live state when the click lands. */
function renderDbTabs()       {
  const strip = $("dbTabStrip");
  if (!strip) return;
  // No connection, no objects: the strip would be a band holding one "+ SQL" for a console
  // that has nothing to run against.
  strip.hidden = !dbConn().conn;
  // Every repaint replaces the card the keyboard was standing on, and the browser answers a
  // destroyed activeElement by falling back to <body> — so one arrow key moved the strip and
  // the next one went nowhere, because the handler only listens while focus is ON the strip
  // (found live on 19998 during the docs/42 T2 walk: the tab's own fetch repaints on arrival,
  // a second after the move). Focus is put back only when it was here to begin with: a repaint
  // while the user types in a filter box or the grid must never pull them out of it.
  const hadFocus = strip.contains(document.activeElement);
  const tabs = dbTabs();
  const active = dbActiveIndex();
  // The qualifier a card can drop (docs/43 M1 D3): read once per paint, shared by the cards
  // and the overflow menu so the two never disagree about an object's name.
  const scope = dbTabScope(dbConn(), dbIsPg());
  const kids = tabs.map((t       , i        ) => {
    if (!dbTabVisible(t)) return null;
    const n = dbTabPending(t);
    const filters = t.kind === "table" ? t.filters.length : 0;
    // The library's object tab (docs/46 P8): the shape Terminal's sessions wear too. It carries
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
      mark: n ? heldDot(trn(n, "dataTabs.nBufferedChanges.one", "dataTabs.nBufferedChanges.other")) : null,
      close: { label: tr("dataTabs.closeTab"), data: { dbtabx: String(i) } },
    });
  });
  // docs/43 M1 D1: the strip is two zones — a scrolling run of cards and a PINNED end.
  // The exits used to sit inside the scroll, so a full strip scrolled its own "+" out of
  // reach (the owner's screenshot): opening a console became a hunt for an offscreen button.
  // The cards scroll; the overflow button and the "+" never do.
  const scroller = h("div", { class: "db-tabstrip-scroll" }, kids);
  // A vertical wheel over a horizontal strip is the first gesture every trackpad user tries;
  // take it as the horizontal scroll it means, but only when there is somewhere to scroll to
  // (otherwise the page's own scroll answers, as it should). Property-assigned on a node this
  // repaint just built — the next rebuild replaces the node, so nothing stacks.
  scroller.onwheel = (ev            )       => {
    if (!ev.deltaY || scroller.scrollWidth <= scroller.clientWidth) return;
    ev.preventDefault();
    scroller.scrollLeft += ev.deltaY;
  };
  const shown = tabs.filter(dbTabVisible).length;
  fill(strip, scroller,
    h("div", { class: "db-tabstrip-end" },
      // The overflow menu is the one place the whole set stays visible once the strip scrolls
      // (docs/43 M1 D4): every open object, then the browser's bulk-close idioms. Nothing is
      // open yet → nothing to list → the button is not offered.
      h("button", {
        class: "db-tab-more", type: "button", hidden: !shown,
        title: tr("dataTabs.openObjects"), aria: { haspopup: "menu" }, data: { dbtabmenu: "" },
      }, iconNode("chevron-down")),
      // The console opens from the strip's own end: it is an object like the rest, and the one
      // gesture that ADDS to the strip belongs on it (swiss-ui-design rule 19).
      h("button", {
        class: "db-tab-add", type: "button",
        title: tr(dbIsRedis() ? "dataTabs.newCommandConsoleTitle" : "dataTabs.newConsoleTitle"),
        data: { dbtabadd: "sql" },
      }, iconNode("plus"), h("span", null, tr(dbIsRedis() ? "dataTabs.command" : "dataTabs.sql")))));
  if (hadFocus) dbFocusActiveTab();
  dbScrollActiveTab();
}

/** Bring the active card into the visible run after every paint (docs/43 M1 D2): the
 *  keyboard walk, Ctrl+Tab and the overflow menu can all land on a card the strip has
 *  scrolled out of sight, and a "current" tab nobody can see is not current. Guarded — the
 *  vitest DOM has no layout and may not implement scrollIntoView at all (house rule 5);
 *  "nearest" never moves a card that is already on screen. */
function dbScrollActiveTab()       {
  const strip = $("dbTabStrip");
  const card = strip ? strip.querySelector             (".otab.sel") : null;
  if (card && typeof card.scrollIntoView === "function") {
    card.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
}

/** #pane's delegated click for the strip (docs/37 R5). The × is checked before the card so a
 *  close never reads as an activate. ev is the live event when the caller has it: opening
 *  the overflow menu must stop the document click that would close it again. */
function dbTabsClick(t         , ev             )          {
  const x = t.closest             ("[data-dbtabx]");
  if (x) { dbCloseTab(Number(x.dataset.dbtabx)); return true; }
  const menuBtn = t.closest             ("[data-dbtabmenu]");
  if (menuBtn) {
    if (ev) ev.stopPropagation();
    popupMenu(menuBtn.getBoundingClientRect(), dbTabsMenuItems());
    return true;
  }
  const add = t.closest             ("[data-dbtabadd]");
  if (add) { dbOpenTabForce({ kind: "sql" }); return true; }
  const card = t.closest             ("[data-dbtab]");
  if (card) { dbActivateTab(Number(card.dataset.dbtab)); return true; }
  return false;
}

/** The middle-button close (docs/43 M1 D8): the browser-tab idiom, on the ×'s exact path —
 *  the dirty-tab confirm included. #pane answers auxclick (a middle press fires BOTH click
 *  and auxclick, so only auxclick may act) and hands the target here. */
function dbTabsAuxClick(t         )          {
  const x = t.closest             ("[data-dbtabx]");
  if (x) { dbCloseTab(Number(x.dataset.dbtabx)); return true; }
  const card = t.closest             ("[data-dbtab]");
  if (card) { dbCloseTab(Number(card.dataset.dbtab)); return true; }
  return false;
}

/* The card being renamed, or null (the right-click's Rename): while set, that card's name
 * span is an input instead. Not state that survives a connection switch — a repaint from
 * dbResetTabs clears it, which is the right collapse for "the strip under me changed". */
let dbTabRenaming                = null;

/** Begin the rename: repaint the strip with the card's name as a focused, selected input. */
function dbTabRenameStart(i        )       {
  dbTabRenaming = i;
  renderDbTabs();
  const strip = $("dbTabStrip");
  const input = strip ? strip.querySelector                  (".db-tab-rename") : null;
  if (input && typeof input.focus === "function") { input.focus(); input.select(); }
}

/** Commit the rename onto the tab (empty cancels — a blank card says nothing). */
function dbTabRenameCommit(value        )       {
  const i = dbTabRenaming;
  dbTabRenaming = null;
  const v = value.trim();
  if (i == null) return;
  const t = dbTabs()[i];
  if (t) t.custom = v || undefined;
  renderDbTabs();
}

/** #pane's delegated contextmenu for the strip: the card's own menu (the owner's ask —
 *  rename, close to the left, close to the right; the browser-tab trio). Left and right
 * are read from the CLICKED card, not the active one — a right-click does not activate,
 * and "the tabs left of the one I pointed at" is what the operator means by it. */
function dbTabsContext(t         , ev            )          {
  const card = t.closest             ("[data-dbtab]");
  if (!card) return false;
  // While the card is a rename input, the browser's own text menu is the right one.
  if (t.closest(".db-tab-rename")) return false;
  ev.preventDefault();
  ev.stopPropagation();
  const i = Number(card.dataset.dbtab);
  const tabs = dbTabs();
  const hasLeft = tabs.slice(0, i).some(dbTabVisible);
  const hasRight = tabs.slice(i + 1).some(dbTabVisible);
  popupMenu(card.getBoundingClientRect(), [
    { label: tr("dataTabs.rename"), fn: ()       => { dbTabRenameStart(i); } },
    { sep: true },
    { label: tr("dataTabs.closeLeft"), disabled: !hasLeft, fn: ()       => { dbCloseToLeft(i); } },
    { label: tr("dataTabs.closeRight"), disabled: !hasRight, fn: ()       => { dbCloseToRight(i); } },
  ]);
  return true;
}

/** The overflow menu's rows (docs/43 M1 D4): one per open object, in strip order, the
 *  current one ticked — the strip's whole set in one place that never scrolls away. The type
 *  glyph and the dirty dot ride the row so the menu also answers "which of these is holding
 *  my work" without opening anything. Behind a separator sit the bulk closes; the
 *  destructive one is last and red (swiss-ui-design rule 4). */
function dbTabsMenuItems()             {
  const tabs = dbTabs();
  const active = dbActiveIndex();
  const scope = dbTabScope(dbConn(), dbIsPg());
  const items             = [];
  tabs.forEach((t       , i        )       => {
    if (!dbTabVisible(t)) return;
    items.push({
      label: dbTabCardTitle(t, scope),
      title: dbTabTitle(t),
      pick: true, on: i === active,
      icon: dbTabGlyph(t), dot: !!dbTabPending(t),
      fn: ()       => { dbActivateTab(i); },
    });
  });
  if (!items.length) return items;
  items.push({ sep: true });
  if (tabs.filter(dbTabVisible).length > 1) {
    items.push({ label: tr("dataTabs.closeOthers"), fn: dbCloseOthers });
  }
  if (tabs.slice(active + 1).some(dbTabVisible)) {
    items.push({ label: tr("dataTabs.closeRight"), fn: ()       => { dbCloseToRight(active); } });
  }
  items.push({ label: tr("dataTabs.closeAll"), danger: true, fn: dbCloseAllTabs });
  return items;
}

/** Close a set of tabs by index, asking ONCE when any of them holds work (docs/43 M1 D4).
 *  A single × names the one object it drops; a bulk close asks the one question the operator
 *  can actually answer — "N buffered changes would go" — and a refusal keeps every tab
 *  exactly where it was. */
function dbCloseBatch(ix          )       {
  const tabs = dbTabs();
  const targets = ix.filter((i        )          => { return i >= 0 && i < tabs.length; });
  if (!targets.length) return;
  const pending = targets.reduce((n        , i        )         => { return n + dbTabPending(tabs[i]); }, 0);
  if (pending && !confirm(tr("dataTabs.closeDiscardsBatch", {
    n: trn(pending, "dataTabs.nBufferedChanges.one", "dataTabs.nBufferedChanges.other"),
  }))) return;
  targets.forEach((i        )       => {
    if (tabs[i].kind === "activity") dbActivityPollStop();
  });
  const gone = new Set(targets);
  const live = dbTab();
  const next = tabs.filter((_       , j        )          => { return !gone.has(j); });
  if (!next.length) next.push(freshTab(dbIsRedis() ? "key" : "table"));
  const at = next.indexOf(live);
  dbResetTabs(next, at >= 0 ? at : 0);
  dbAfterTabSwitch();
}

/** "Close others" — everything but the active tab. */
function dbCloseOthers()       {
  const active = dbActiveIndex();
  const ix           = [];
  dbTabs().forEach((t       , i        )       => {
    if (i !== active && dbTabVisible(t)) ix.push(i);
  });
  dbCloseBatch(ix);
}

/** "Close to the left" of the PIVOT card (the right-clicked one; the overflow menu
 *  passes the active one) — the browser-tab trio's missing third. */
function dbCloseToLeft(pivot        )       {
  const ix           = [];
  dbTabs().forEach((t       , i        )       => {
    if (i < pivot && dbTabVisible(t)) ix.push(i);
  });
  dbCloseBatch(ix);
}

/** "Close to the right" of the pivot card — the tabs the strip shows after it. */
function dbCloseToRight(pivot        )       {
  const ix           = [];
  dbTabs().forEach((t       , i        )       => {
    if (i > pivot && dbTabVisible(t)) ix.push(i);
  });
  dbCloseBatch(ix);
}

/** "Close all" — back to the placeholder, i.e. the pane's empty state. */
function dbCloseAllTabs()       {
  const ix           = [];
  dbTabs().forEach((t       , i        )       => {
    if (dbTabVisible(t)) ix.push(i);
  });
  dbCloseBatch(ix);
}

/* Ctrl+Tab / Ctrl+Shift+Tab cycle the strip (swiss-ui-design rule 19: the frequent gesture must
   not need discovering). Document-level and capture-phase, because the console textarea would
   otherwise swallow a Tab before the pane's delegated listener saw it; the dbIsMounted() guard is
   what keeps a Data shortcut from firing on any other page. Note for the reader: Chrome reserves
   Ctrl+Tab for its own tab bar, so in an ordinary browser tab the browser acts as well — the
   strip's × and the cards themselves are the gesture that always works. */
document.addEventListener("keydown", (ev               )       => {
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
function dbFocusActiveTab()       {
  const strip = $("dbTabStrip");
  if (!strip) return;
  const card = strip.querySelector             (".otab.sel");
  if (card) card.focus();
}

/* --- open / close / activate ----------------------------------------------------------------------- */

/** The most recently touched table tab — where a new table tab inherits its page size from. The
 *  rows-per-page pick is the operator's density, not one table's property, and it survived every
 *  table switch when there was a single record. */
function dbLastTableTab()                    {
  let best                    = null;
  dbTabs().forEach((t       )       => {
    if (t.kind !== "table") return;
    if (!best || t.touched > best.touched) best = t;
  });
  return best;
}

/** A fresh tab for one spec. The literals themselves come from freshTab (db-state.ts owns the
 *  shapes); this only writes the address the spec names. */
function dbBuildTab(spec           )        {
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
  if (spec.kind === "sql") return freshTab("sql");
  return freshTab("activity");
}

/** The one way an object reaches the pane (docs/42 D6 — dbOpenTable and dbFkOpen were the same
 *  ritual written twice). Already open → activated, never duplicated. Strip full → the least
 *  recently used tab with nothing in it makes room; if every tab holds work, the open is REFUSED
 *  and says so, because silently dropping an operator's edits to show them a table is not a
 *  trade the page gets to make. */
function dbOpenTab(spec           )       {
  const tabs = dbTabs();
  const at = tabs.findIndex((t       )          => { return dbTabMatches(t, spec); });
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
    // Not silent (docs/43 M1 D5): a clean card vanishing without a word reads as "the page
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
function dbOpenTabForce(spec           )       {
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
}

/** Drop every tab open on one table — what DROP TABLE leaves behind (docs/22 closeout audit,
 *  now that the pane is a strip). Nothing of the dropped table may linger, and a BACKGROUND tab
 *  is exactly where it would: renderDbTables refreshes only the left list. No confirm on the
 *  buffered writes it discards — they address rows that no longer exist, so the only question a
 *  prompt could ask is whether to keep writing to a table that is gone. */
function dbDropTableTabs(table               , schema               )       {
  const tabs = dbTabs();
  const live = dbTab();
  const next = tabs.filter((t       )          => {
    return !(t.kind === "table" && t.table === table && t.schema === schema);
  });
  if (next.length === tabs.length) return;
  if (!next.length) next.push(freshTab(dbIsRedis() ? "key" : "table"));
  const at = next.indexOf(live);
  dbResetTabs(next, at >= 0 ? at : Math.min(dbActiveIndex(), next.length - 1));
  // The pane is only re-seeded when the object in front of the user changed; a drop of some
  // background table repaints the strip and nothing else.
  if (at >= 0) renderDbTabs(); else dbAfterTabSwitch();
}

/** Bring the i'th tab to the pane. `byKeyboard` says the move came from the strip's own arrows
 *  or Ctrl+Tab, which is the one case where the console must NOT take the caret: the walk has to
 *  be able to continue past it. */
function dbActivateTab(i        , byKeyboard          )       {
  if (i === dbActiveIndex()) { renderDbTabs(); return; }
  dbLeaveTab(dbTab());
  dbSetActive(i);
  dbAfterTabSwitch(byKeyboard);
}

/** Close it. A tab holding buffered writes asks first (docs/42 D5) and a refusal keeps it
 *  exactly where it was. The active tab's place goes to its right neighbour, or its left when
 *  there is no right; closing the last one leaves the placeholder, so the pane falls back to the
 *  empty state instead of to nothing. */
function dbCloseTab(i        )       {
  const tabs = dbTabs();
  const t = tabs[i];
  if (!t) return; // a stale render's address — the strip moved under the click
  const n = dbTabPending(t);
  if (n && !confirm(tr("dataTabs.closeDiscards", {
    name: dbTabTitle(t),
    n: trn(n, "dataTabs.nBufferedChanges.one", "dataTabs.nBufferedChanges.other"),
  }))) return;
  if (t.kind === "activity") dbActivityPollStop();
  const active = dbActiveIndex();
  const next = tabs.filter((_       , j        )          => { return j !== i; });
  let idx = active;
  if (i === active) idx = Math.min(i, next.length - 1);
  else if (i < active) idx = active - 1;
  if (!next.length) next.push(freshTab(dbIsRedis() ? "key" : "table"));
  dbResetTabs(next, Math.max(0, idx));
  dbAfterTabSwitch();
}

/** Ctrl+Tab's move: forward through the strip, wrapping. */
function dbCycleTab(back         )       {
  const n = dbTabs().length;
  if (n < 2) return;
  dbActivateTab(((dbActiveIndex() + (back ? -1 : 1)) % n + n) % n, true);
}

/** Every open object belongs to the connection it was opened under: the switch closes the whole
 *  strip and starts the new connection at its own family's placeholder (a redis connection
 *  browses keys, everything else tables). The page size carries across, the same preference the
 *  single record never reset either. */
function dbResetTabsForConn()       {
  const keep = dbLastTableTab();
  dbActivityPollStop();
  const next = freshTab(dbIsRedis() ? "key" : "table");
  if (next.kind === "table" && keep) next.pageSize = keep.pageSize;
  dbResetTabs([next], 0);
}

/** docs/42 D4: a tab going to the background keeps what is cheap — its filters, its offset, its
 *  order, its buffered edits — and drops what is expensive, the page of rows. Coming back
 *  re-fetches from exactly those coordinates, which is why a filtered, paged, half-edited table
 *  looks untouched on return. */
function dbLeaveTab(t       )       {
  if (t.kind === "table" && t.table) t.data = null;
  if (t.kind === "activity") dbActivityPollStop();
}

/** Everything the pane owes a new active tab: the console's text, the chrome, the grid, and the
 *  fetch for whatever the tab dropped while it was in the background. */
function dbAfterTabSwitch(byKeyboard          )       {
  const t = dbTab();
  // The console is the sql tab's body now (docs/42 T2), so the textarea is seeded from the tab
  // being shown — otherwise the previous console's text sits in the new one's box.
  const ta = $                     ("dbSql");
  if (ta) ta.value = t.kind === "sql" ? t.sqlText : "";
  dbSqlPaint();
  renderDbTabs();
  renderDbTables();
  renderDbToolbar();
  renderDbFilters();
  renderDbGrid();
  renderDbBar();
  if (t.kind === "table" && t.table) {
    if (!t.data) void dbRestoreData();
    if (!t.detail) void dbLoadDetail();
  }
  if (t.kind === "key" && t.redisKey && !t.redisValue) void dbLoadRedisValue(t.redisKey);
  // The 5s poll follows "is that tab active" (docs/42 T2 item 6): a backgrounded monitor stops
  // asking the server about sessions nobody is looking at.
  if (t.kind === "activity") dbActivityStart();
  // Landing on a console by pointer means "I am going to type here"; landing on it mid-walk
  // does not. Taking the caret on the keyboard path turned the arrows into a one-way trip:
  // the next ArrowLeft moved the textarea's caret instead of the strip (found live, T2 walk).
  if (t.kind === "sql" && ta && !byKeyboard) ta.focus();
}

export {
  DB_TAB_MAX, dbActivateTab, dbAfterTabSwitch, dbCloseAllTabs, dbCloseBatch, dbCloseToLeft, dbCloseOthers,
  dbCloseTab, dbCloseToRight, dbCycleTab, dbDropTableTabs, dbEvictTarget, dbFiltersSame, dbLastTableTab,
  dbOpenTab, dbOpenTabForce, dbResetTabsForConn, dbTabCardTitle, dbTabGlyph, dbTabMatches, dbTabPending,
  dbTabPlaceholder, dbTabsAuxClick, dbTabsClick, dbTabsContext, dbTabsMenuItems, dbTabsPending,
  dbTabTitle, dbTabVisible, dbTabEvictable, renderDbTabs,
};

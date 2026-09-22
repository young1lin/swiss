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
// @vitest-environment happy-dom

/* The object tab strip (docs/42 T2). The page used to hold ONE object: opening a table,
   jumping a foreign key or running a query overwrote whatever was in front of the operator,
   filters and buffered edits with it. These pins hold the four promises the strip makes —
   dedupe, a cap that never evicts work, a close that asks, and a page-leave guard that counts
   the WHOLE strip — and they run in two halves: the policy is pure functions over a tab array,
   the wiring drives the real dbOpenTab/dbCloseTab against a real DOM. */

import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  dbActiveIndex, dbConn as dbConnState, dbTab, dbTabs, freshTab, mountDbView, unmountDbView,
} from "../src/db-state.js";
import { dbConn, dbPage } from "./db-fixtures.js";
import type { DbKeyTab, DbSqlTab, DbTab, DbTableTab } from "../src/types/state.js";

const here = dirname(fileURLToPath(import.meta.url));

/* The served shell's ids plus the Data view's own, which renderDbView builds at runtime: the
   renderers all read their host through $(), so a missing id would silently skip the paint
   this suite is asserting on. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  const own = ["dbTabStrip", "dbTables", "dbTablesPager", "dbHead", "dbMore", "dbFilters",
    "dbConsole", "dbSql", "dbSqlHl", "dbToolbar", "dbGridWrap", "dbBar", "dbNewTable"];
  return Array.from(new Set([...ids, ...own])).map((id) => '<div id="' + id + '"></div>').join("");
}

/* Every request answers the same empty page: this suite asks WHETHER a load went out and with
   which address, never what came back. */
const urls: string[] = [];
const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, {
  fetch: (url: unknown) => {
    urls.push(String(url));
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(dbPage()) });
  },
});
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});

/* The shell BEFORE the import: several modules in the Data view's graph wire a shell control
   at evaluation time (add-sheet.ts's $("addBtn").onclick), so an empty body kills the import. */
document.body.innerHTML = shellSkeleton();
const tabs = await import("../src/data-tabs.js");
const page = await import("../src/views/data.js");
const sql = await import("../src/data-sql.js");
const filters = await import("../src/data-filters.js");

/** A table tab pointing at a real table — what the strip actually holds. */
function tableTab(table: string, schema: string | null = null): DbTableTab {
  const t = freshTab("table");
  t.table = table;
  t.schema = schema;
  return t;
}
/** One buffered update, which is all "dirty" means to the cap and the close confirm. */
function dirty(t: DbTableTab): DbTableTab {
  t.updates = { "1": { pk: { id: 1 }, changes: { a: "x" } } };
  return t;
}
/** The active tab, narrowed — every open in this suite opens a table. */
function liveTable(): DbTableTab {
  const t = dbTabs()[dbActiveIndex()];
  if (t.kind !== "table") throw new Error("the active tab is not a table tab");
  return t;
}
function tableAt(i: number): DbTableTab {
  const t = dbTabs()[i];
  if (t.kind !== "table") throw new Error("tab " + i + " is not a table tab");
  return t;
}

beforeEach(() => {
  urls.length = 0;
  document.body.innerHTML = shellSkeleton();
  unmountDbView();
  mountDbView();
  Object.assign(dbConnState(), { conns: [dbConn("c", "mysql")], conn: "c" });
  vi.restoreAllMocks();
});

/* --- the policy, as pure functions ---------------------------------------------------------- */

describe("what a tab is worth (docs/42 D3)", () => {
  it("a tab pointing at nothing is the placeholder: off the strip, off the cap", () => {
    expect(tabs.dbTabPlaceholder(freshTab("table"))).toBe(true);
    expect(tabs.dbTabPlaceholder(freshTab("key"))).toBe(true);
    // Opening a console or the monitor IS the act — neither is ever a placeholder.
    expect(tabs.dbTabPlaceholder(freshTab("sql"))).toBe(false);
    expect(tabs.dbTabPlaceholder(freshTab("activity"))).toBe(false);
    expect(tabs.dbTabVisible(tableTab("t"))).toBe(true);
  });

  it("pending counts writes and nothing else — per tab and over the whole strip", () => {
    const t = tableTab("t");
    t.updates = { a: { pk: { id: 1 }, changes: {} }, b: { pk: { id: 2 }, changes: {} } };
    t.deletes = { c: { id: 3 } };
    t.inserts = [{ values: {} }];
    expect(tabs.dbTabPending(t)).toBe(4);
    const k = freshTab("key");
    k.redisKey = "k";
    k.redisEdits = { key: "k", type: "hash", updates: { f: "1" }, deletes: {}, inserts: [] };
    expect(tabs.dbTabPending(k)).toBe(1);
    // A console's text is work, but it is not a WRITE: the number the guards quote is what is
    // being held back from the database.
    const s = freshTab("sql");
    s.sqlText = "delete from t";
    expect(tabs.dbTabPending(s)).toBe(0);
    expect(tabs.dbTabsPending([t, k, s])).toBe(5);
  });

  it("the cap never evicts work: buffered writes and an unrun query both pin a tab", () => {
    expect(tabs.dbTabEvictable(tableTab("t"))).toBe(true);
    expect(tabs.dbTabEvictable(dirty(tableTab("t")))).toBe(false);
    const s = freshTab("sql");
    expect(tabs.dbTabEvictable(s)).toBe(true);
    s.sqlText = "  \n ";
    expect(tabs.dbTabEvictable(s), "whitespace is not a query").toBe(true);
    s.sqlText = "select 1";
    expect(tabs.dbTabEvictable(s), "a query still being composed would have to be retyped").toBe(false);
  });

  it("the victim is the least recently touched tab that is neither active nor holding work", () => {
    const a = tableTab("a"); const b = tableTab("b"); const c = tableTab("c");
    a.touched = 1; b.touched = 2; c.touched = 3;
    expect(tabs.dbEvictTarget([a, b, c], 2)).toBe(0);
    // The active tab is never the victim, however old its stamp.
    expect(tabs.dbEvictTarget([a, b, c], 0)).toBe(1);
    // ...and neither is a dirty one: the next oldest goes instead.
    dirty(a);
    expect(tabs.dbEvictTarget([a, b, c], 2)).toBe(1);
    dirty(b);
    expect(tabs.dbEvictTarget([a, b, c], 2), "nothing may go").toBeNull();
  });

  it("identity: a plain open matches any tab on the table, a jump only its own filter set", () => {
    const t = tableTab("orders");
    t.filters = [{ column: "id", op: "=", value: "7" }];
    expect(tabs.dbTabMatches(t, { kind: "table", table: "orders", schema: null })).toBe(true);
    expect(tabs.dbTabMatches(t, { kind: "table", table: "orders", schema: "s" })).toBe(false);
    expect(tabs.dbTabMatches(t, {
      kind: "table", table: "orders", schema: null, filters: [{ column: "id", op: "=", value: "7" }],
    })).toBe(true);
    expect(tabs.dbTabMatches(t, {
      kind: "table", table: "orders", schema: null, filters: [{ column: "id", op: "=", value: "8" }],
    }), "a jump to another row is another object").toBe(false);
    // One console and one monitor per connection: a second open activates the first.
    expect(tabs.dbTabMatches(freshTab("sql"), { kind: "sql" })).toBe(true);
    expect(tabs.dbTabMatches(freshTab("activity"), { kind: "activity" })).toBe(true);
    expect(tabs.dbTabMatches(freshTab("sql"), { kind: "activity" })).toBe(false);
  });

  it("the name on the card is the object's own", () => {
    expect(tabs.dbTabTitle(tableTab("orders", "shop"))).toBe("shop.orders");
    expect(tabs.dbTabTitle(tableTab("orders"))).toBe("orders");
    const k = freshTab("key"); k.redisKey = "session:1";
    expect(tabs.dbTabTitle(k)).toBe("session:1");
    expect(tabs.dbTabGlyph(tableTab("t"))).toBe("table");
    expect(tabs.dbTabGlyph(k)).toBe("redis");
    expect(tabs.dbTabGlyph(freshTab("sql"))).toBe("terminal");
    expect(tabs.dbTabGlyph(freshTab("activity"))).toBe("clock");
  });
});

/* --- open, close, activate -------------------------------------------------------------------- */

describe("opening an object (docs/42 D3, D6)", () => {
  it("the same table twice is one tab, activated the second time", () => {
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbOpenTab({ kind: "sql" });
    expect(dbTabs().length).toBe(2);
    expect(dbActiveIndex()).toBe(1);
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    expect(dbTabs().length, "no second card for one table").toBe(2);
    expect(dbActiveIndex(), "the open one comes forward").toBe(0);
  });

  it("the first real object takes the placeholder's slot", () => {
    expect(dbTabs().length).toBe(1);
    expect(tabs.dbTabPlaceholder(dbTab())).toBe(true);
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    expect(dbTabs().length, "the placeholder is not a card to sit beside").toBe(1);
    expect(liveTable().table).toBe("t1");
  });

  it("a thirteenth table evicts the least recently used one — and never a dirty one", () => {
    for (let i = 1; i <= 12; i++) tabs.dbOpenTab({ kind: "table", table: "t" + i, schema: null });
    expect(dbTabs().length).toBe(12);
    // t1 is the oldest, so it would be the victim — unless it is holding work, which is what
    // D3 promises: the cap defends memory, never at the price of an edit.
    dirty(tableAt(0));
    tabs.dbOpenTab({ kind: "table", table: "t13", schema: null });
    const names = dbTabs().map((t: DbTab): string | null => { return t.kind === "table" ? t.table : t.kind; });
    expect(dbTabs().length, "the cap holds").toBe(12);
    expect(names, "t2 went, t1 stayed with its edit").toEqual(
      ["t1", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10", "t11", "t12", "t13"]);
  });

  it("twelve tabs all holding work: the open is refused and says so", () => {
    for (let i = 1; i <= 12; i++) {
      tabs.dbOpenTab({ kind: "table", table: "t" + i, schema: null });
      dirty(liveTable());
    }
    const before = dbTabs().slice();
    tabs.dbOpenTab({ kind: "table", table: "t13", schema: null });
    expect(dbTabs(), "nothing was dropped to make room").toEqual(before);
    expect(document.body.textContent, "the refusal is on screen").toContain("close one first");
  });

  it("the strip's + always adds: force open neither dedupes nor refuses (the owner's rule)", () => {
    tabs.dbOpenTab({ kind: "sql" });
    tabs.dbOpenTabForce({ kind: "sql" });
    expect(dbTabs().length, "one + press, one NEW console — no dedupe on kind").toBe(2);
    expect(dbActiveIndex()).toBe(1);
    for (let i = 1; i <= 10; i++) tabs.dbOpenTabForce({ kind: "sql" });
    expect(dbTabs().length, "twelve now, none holding work").toBe(12);
    // Every console holding unrun text: nothing is evictable, and the + still opens — over
    // the cap, because the operator asked for THIS tab (the owner: "one +, one tab, always").
    dbTabs().forEach((t: DbTab): void => { if (t.kind === "sql") t.sqlText = "select 1"; });
    tabs.dbOpenTabForce({ kind: "sql" });
    expect(dbTabs().length, "over the cap rather than refused").toBe(13);
  });

  it("a rename is the card's name everywhere, and empty cancels back to the derived title", () => {
    tabs.dbOpenTab({ kind: "sql" });
    const con = dbTab();
    con.custom = "nightly totals";
    expect(tabs.dbTabTitle(con)).toBe("nightly totals");
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "acme_app_dev" });
    const tt = dbTabs()[dbTabs().length - 1];
    tt.custom = "my orders";
    expect(tabs.dbTabCardTitle(tt, "acme_app_dev"), "scope trimming never touches a custom name").toBe("my orders");
    tt.custom = "";
    expect(tabs.dbTabCardTitle(tt, "acme_app_dev"), "empty falls back to the derived title").toBe("orders");
  });

  it("close to the left and right of the CLICKED card (the right-click trio)", () => {
    for (let i = 1; i <= 4; i++) tabs.dbOpenTab({ kind: "table", table: "t" + i, schema: null });
    const names = (): (string | null)[] => dbTabs().map((t: DbTab): string | null => {
      return t.kind === "table" ? t.table : t.kind;
    });
    tabs.dbCloseToRight(1);
    expect(names(), "t3 and t4 went, t1 stayed").toEqual(["t1", "t2"]);
    tabs.dbCloseToLeft(1);
    expect(names(), "now t1 goes too").toEqual(["t2"]);
  });

  it("the card's right-click menu: rename, close left, close right — with the clicked card as pivot", () => {
    for (let i = 1; i <= 3; i++) tabs.dbOpenTab({ kind: "table", table: "t" + i, schema: null });
    // This suite mounts state, not the view (loadDbView is what property-assigns
    // pane.oncontextmenu), so the dispatcher dbTabsContext is driven the way the pane
    // hands it a target — the pane wiring itself is walked live on 19998.
    const card = document.querySelector('[data-dbtab="1"]');
    expect(card, "the strip painted the cards").toBeTruthy();
    tabs.dbTabsContext(card as Element, new MouseEvent("contextmenu", { cancelable: true }));
    const menu = document.getElementById("menu");
    expect(menu, "the card menu opened").toBeTruthy();
    const labels = [...(menu as HTMLElement).querySelectorAll("button")].map((b) => b.textContent);
    expect(labels).toContain("Rename");
    expect(labels).toContain("Close to the left");
    expect(labels).toContain("Close to the right");
    // The first card has nothing to its left: that row is disabled, not missing.
    const first = document.querySelector('[data-dbtab="0"]');
    expect(first, "the first card is on the strip").toBeTruthy();
    tabs.dbTabsContext(first as Element, new MouseEvent("contextmenu", { cancelable: true }));
    const left = [...document.querySelectorAll("#menu button")].find((b) => b.textContent === "Close to the left");
    expect((left as HTMLButtonElement).disabled, "no left neighbors: the row refuses").toBe(true);
  });

  it("a jump opens a NEW tab and leaves the source exactly as it was (docs/22 W5.2)", () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: null });
    const src = tableAt(0);
    src.filters = [{ column: "state", op: "=", value: "paid" }];
    src.offset = 100;
    dirty(src);
    tabs.dbOpenTab({
      kind: "table", table: "customers", schema: null, filters: [{ column: "id", op: "=", value: "7" }],
    });
    expect(dbTabs().length).toBe(2);
    const opened = liveTable();
    expect(opened.table).toBe("customers");
    expect(opened.filters).toEqual([{ column: "id", op: "=", value: "7" }]);
    expect(src.filters, "the source's own filter is untouched").toEqual([{ column: "state", op: "=", value: "paid" }]);
    expect(src.offset, "and its page").toBe(100);
    expect(Object.keys(src.updates).length, "and its buffered edit").toBe(1);
  });

  it("a backgrounded table drops its page, and re-fetches by its own address when it returns (D4)", () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: null });
    const src = tableAt(0);
    src.filters = [{ column: "state", op: "=", value: "paid" }];
    src.offset = 100;
    src.data = dbPage({ table: "orders" });
    tabs.dbOpenTab({ kind: "sql" });
    expect(src.data, "a page nobody is looking at is not worth the memory").toBeNull();
    urls.length = 0;
    tabs.dbActivateTab(0);
    const q = urls.find((u: string): boolean => { return u.includes("/data?"); });
    expect(q, "coming back re-fetches").toBeTruthy();
    expect(q).toContain("offset=100");
    expect(q).toContain("table=orders");
    expect(q, "under the filter it was left with").toContain(encodeURIComponent(JSON.stringify(src.filters)));
  });

  it("and comes back with its buffered writes intact, dot and all", async () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: null });
    const src = tableAt(0);
    src.data = dbPage({ table: "orders" });
    dirty(src);
    tabs.dbOpenTab({ kind: "sql" });
    tabs.dbActivateTab(0);
    await new Promise((r) => { setTimeout(r, 0); }); // let the return fetch answer
    expect(Object.keys(src.updates).length, "the return fetch re-reads rows, it does not re-baseline").toBe(1);
    expect(src.data, "and the page it dropped is back").toBeTruthy();
    expect(
      document.querySelector("#dbTabStrip .db-tab.sel .db-tab-dot"),
      "so the card still says this object holds work",
    ).toBeTruthy();
  });

  it("cycling onto a console leaves the keyboard on the strip; clicking it hands over the caret", () => {
    // The console grabs the caret so a click means "type here". On the keyboard path that made
    // the arrows a one-way trip: the walk went INTO the box and the next arrow moved a caret.
    const box = document.createElement("textarea");
    box.id = "dbSql";
    document.getElementById("dbSql")!.replaceWith(box);
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbOpenTab({ kind: "sql" });
    tabs.dbActivateTab(0);
    document.querySelector<HTMLElement>("#dbTabStrip .db-tab.sel")!.focus();
    tabs.dbCycleTab(false); // the arrows / Ctrl+Tab
    expect(dbTab().kind, "the console is in front").toBe("sql");
    expect(
      document.activeElement && (document.activeElement as HTMLElement).closest(".db-tab.sel"),
      "and the keyboard is still on its card, free to walk on",
    ).toBeTruthy();
    tabs.dbActivateTab(0);
    tabs.dbActivateTab(1); // the pointer path
    expect(document.activeElement, "a click on the console is a request to type in it").toBe(box);
  });

  it("on a redis connection the console card says what it runs", () => {
    // The console box on a redis connection takes SET/GET/DEL, not SQL. A card reading "SQL"
    // over that box was the one label on the strip naming something the tab cannot do.
    Object.assign(dbConnState(), { conns: [dbConn("r", "redis")], conn: "r" });
    tabs.dbOpenTab({ kind: "sql" });
    tabs.renderDbTabs();
    const strip = document.getElementById("dbTabStrip")!;
    expect(strip.querySelector(".db-tab-name")!.textContent).toBe("Command");
    expect(strip.querySelector("[data-dbtabadd]")!.textContent).toContain("Command");
    Object.assign(dbConnState(), { conns: [dbConn("c", "mysql")], conn: "c" });
    tabs.renderDbTabs();
    expect(strip.querySelector(".db-tab-name")!.textContent, "and SQL where it is SQL").toBe("SQL");
  });

  it("a repaint keeps the keyboard on the strip, and keeps its hands off everything else", () => {
    // Found live on 19998: the arrow keys only answer while focus is on the strip, and every
    // repaint destroys the focused card — so the tab's own fetch, landing a second after the
    // move, dropped focus to <body> and the NEXT arrow key did nothing.
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbOpenTab({ kind: "table", table: "t2", schema: null });
    const card = document.querySelector<HTMLElement>("#dbTabStrip .db-tab.sel");
    card!.focus();
    tabs.renderDbTabs();
    expect(
      document.activeElement && (document.activeElement as HTMLElement).closest(".db-tab.sel"),
      "the keyboard is back on the active card, not on <body>",
    ).toBeTruthy();
    const box = document.createElement("input");
    document.body.appendChild(box);
    box.focus();
    tabs.renderDbTabs();
    expect(document.activeElement, "a repaint never pulls the user out of a box they are typing in").toBe(box);
  });

  it("Ctrl+Tab cycles the strip and wraps", () => {
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbOpenTab({ kind: "table", table: "t2", schema: null });
    tabs.dbOpenTab({ kind: "sql" });
    expect(dbActiveIndex()).toBe(2);
    tabs.dbCycleTab(false);
    expect(dbActiveIndex(), "past the end is the first").toBe(0);
    tabs.dbCycleTab(true);
    expect(dbActiveIndex(), "and backwards past the start is the last").toBe(2);
  });
});

describe("closing an object (docs/42 D5)", () => {
  it("a tab holding writes asks first, and a refusal keeps it where it was", () => {
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbOpenTab({ kind: "table", table: "t2", schema: null });
    dirty(tableAt(0));
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    tabs.dbCloseTab(0);
    expect(ask, "the question names the object and the count").toHaveBeenCalled();
    expect(String(ask.mock.calls[0][0])).toContain("t1");
    expect(dbTabs().length, "no was an answer").toBe(2);
    ask.mockReturnValue(true);
    tabs.dbCloseTab(0);
    expect(dbTabs().length).toBe(1);
  });

  it("a clean tab closes without a question; the place goes to the right neighbour", () => {
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    for (const t of ["t1", "t2", "t3"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tabs.dbActivateTab(1);
    tabs.dbCloseTab(1);
    expect(ask).not.toHaveBeenCalled();
    expect(liveTable().table, "the right neighbour takes the place").toBe("t3");
    expect(dbActiveIndex()).toBe(1);
  });

  it("closing the last object leaves the placeholder, never an empty strip", () => {
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbCloseTab(0);
    expect(dbTabs().length).toBe(1);
    expect(tabs.dbTabPlaceholder(dbTab()), "what is left is the empty state, not a null tab").toBe(true);
  });
});

/* --- the page-leave guards -------------------------------------------------------------------- */

describe("leaving the page asks once, for the whole strip (docs/42 D5)", () => {
  it("a buffer parked on a BACKGROUND tab still stops the page", () => {
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.dbOpenTab({ kind: "sql" });
    expect(page.hasPendingChanges()).toBe(false);
    dirty(tableAt(0));
    expect(page.hasPendingChanges(), "the old guard asked the ACTIVE object and let this walk out").toBe(true);
  });

  it("the question carries the sum over every tab, and is asked once", () => {
    for (const t of ["t1", "t2"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tableAt(0).updates = { a: { pk: { id: 1 }, changes: {} }, b: { pk: { id: 2 }, changes: {} } };
    tableAt(1).deletes = { c: { id: 3 } };
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    expect(page.canLeave()).toBe(true);
    expect(ask, "one question, not one per tab").toHaveBeenCalledTimes(1);
    expect(String(ask.mock.calls[0][0]), "3 changes across the strip").toContain("3");
  });

  it("a clean strip walks out without a question", () => {
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    expect(page.canLeave()).toBe(true);
    expect(ask).not.toHaveBeenCalled();
  });
});

/* --- the strip itself ------------------------------------------------------------------------- */

describe("the strip's markup (swiss-ui-design §1.3)", () => {
  it("one card per visible object: glyph, name, close — and the placeholder is not on it", () => {
    tabs.renderDbTabs();
    const strip = document.getElementById("dbTabStrip");
    if (!strip) throw new Error("no strip");
    expect(strip.querySelectorAll(".db-tab").length, "the placeholder is not an object").toBe(0);
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: null });
    tabs.dbOpenTab({ kind: "sql" });
    tabs.renderDbTabs();
    const cards = Array.from(strip.querySelectorAll(".db-tab"));
    expect(cards.length).toBe(2);
    expect(cards[0].querySelector("use")?.getAttribute("href")).toBe("#i-table");
    expect(cards[0].querySelector(".db-tab-name")?.textContent).toBe("orders");
    expect(cards[0].getAttribute("data-dbtab")).toBe("0");
    expect(cards[0].querySelector("[data-dbtabx]"), "every card carries its own close").not.toBeNull();
    expect(cards[1].className, "the active card is the lifted one").toContain("sel");
    expect(cards[1].getAttribute("aria-selected")).toBe("true");
    // Roving tabindex: one Tab stop for the strip, the arrows move inside it.
    expect((cards[1] as HTMLElement).tabIndex).toBe(0);
    expect((cards[0] as HTMLElement).tabIndex).toBe(-1);
    expect(strip.querySelector("[data-dbtabadd]"), "the console opens from the strip's end").not.toBeNull();
  });

  it("the card says what the object is holding: the filter count and the dirty dot", () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: null });
    const t = tableAt(0);
    t.filters = [{ column: "a", op: "=", value: "1" }, { column: "b", op: "=", value: "2" }];
    dirty(t);
    tabs.renderDbTabs();
    const card = document.querySelector(".db-tab");
    if (!card) throw new Error("no card");
    expect(card.querySelector(".db-tab-n")?.textContent, "this tab shows a subset").toBe("2");
    expect(card.querySelector(".db-tab-dot"), "and is holding writes").not.toBeNull();
    expect(card.className).toContain("dirty");
  });

  it("the dot and the count are repainted with the readouts they mirror", () => {
    // Found on the 19998 walk: buffering an edit painted the pending BAR and left the card
    // clean, because nothing in the edit path repainted the strip. The card states the same
    // two facts as the bar and the filter row, so it is repainted with each of them.
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: null });
    const t = tableAt(0);
    dirty(t);
    sql.renderDbBar();
    expect(document.querySelector(".db-tab-dot"), "the dot arrives with the pending bar").not.toBeNull();
    t.updates = {};
    sql.renderDbBar();
    expect(document.querySelector(".db-tab-dot"), "and leaves when the bar does").toBeNull();
    t.filters = [{ column: "a", op: "=", value: "1" }];
    filters.renderDbFilters();
    expect(document.querySelector(".db-tab-n")?.textContent, "the count arrives with the filter row").toBe("1");
  });

  it("no connection, no strip", () => {
    dbConnState().conn = null;
    tabs.renderDbTabs();
    expect((document.getElementById("dbTabStrip") as HTMLElement).hidden).toBe(true);
  });

  it("the delegated click answers close before activate — a × is never an activate", () => {
    for (const t of ["t1", "t2"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tabs.renderDbTabs();
    const cards = Array.from(document.querySelectorAll(".db-tab"));
    expect(tabs.dbTabsClick(cards[0])).toBe(true);
    expect(dbActiveIndex(), "the card activates its own index").toBe(0);
    const x = cards[1].querySelector("[data-dbtabx]");
    if (!x) throw new Error("no close button");
    expect(tabs.dbTabsClick(x)).toBe(true);
    expect(dbTabs().length, "the × closed it instead of activating it").toBe(1);
    expect(tabs.dbTabsClick(document.body), "anything else is not the strip's").toBe(false);
  });
});

/* --- the connection switch --------------------------------------------------------------------- */

describe("switching connection empties the strip (docs/42 T2)", () => {
  it("every open object belonged to the connection it was opened on", () => {
    for (const t of ["t1", "t2"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    // The density carries over from the most recently used table tab — the operator's last
    // word on rows-per-page, not the oldest tab's.
    liveTable().pageSize = 200;
    tabs.dbResetTabsForConn();
    expect(dbTabs().length).toBe(1);
    expect(tabs.dbTabPlaceholder(dbTab()), "the new connection starts empty").toBe(true);
    expect(liveTable().pageSize, "the operator's row density is theirs, not the table's").toBe(200);
  });

  it("a redis connection starts on a key tab, not a table tab", () => {
    Object.assign(dbConnState(), { conns: [dbConn("r", "redis")], conn: "r" });
    tabs.dbResetTabsForConn();
    expect(dbTab().kind).toBe("key");
  });
});

/* --- the strip's exits (docs/43 M1) --------------------------------------------------------- */

describe("a full strip still has exits (docs/43 M1)", () => {
  it("the cap is twelve — the exits, not the number, are the protection", () => {
    expect(tabs.DB_TAB_MAX).toBe(12);
  });

  it("eviction says which tab it closed — a card that vanishes silently reads as a bug", () => {
    for (let i = 1; i <= 12; i++) tabs.dbOpenTab({ kind: "table", table: "t" + i, schema: null });
    // t2 is the oldest clean background tab (t1 is dirtied below), so it is the victim.
    dirty(tableAt(0));
    tabs.dbOpenTab({ kind: "table", table: "t13", schema: null });
    const toastEl = document.getElementById("toast");
    expect(toastEl!.textContent, "the toast names the victim").toContain("t2");
    expect(toastEl!.textContent, "and says it had nothing unsaved").toContain("no unsaved changes");
  });

  it("the card's name drops the redundant qualifier; the title keeps the full name (D3)", () => {
    // MySQL: the list browses one database, so that database is the scope and a same-database
    // card spends its 210px on the table name alone.
    const mysqlScope = tabs.dbTabScope({ ...dbConnState(), tables: [
      { schema: "acme_app_dev", name: "a" }, { schema: "acme_app_dev", name: "b" },
    ] }, false);
    expect(mysqlScope).toBe("acme_app_dev");
    expect(tabs.dbTabCardTitle(tableTab("orders", "acme_app_dev"), mysqlScope)).toBe("orders");
    // A schema outside the scope (an FK jump across databases) keeps its qualifier.
    expect(tabs.dbTabCardTitle(tableTab("orders", "other_db"), mysqlScope)).toBe("other_db.orders");
    // pg: the picked schema is the scope, public by default.
    expect(tabs.dbTabScope({ ...dbConnState(), schemaFilter: "sales" }, true)).toBe("sales");
    expect(tabs.dbTabScope({ ...dbConnState(), schemaFilter: "" }, true)).toBe("public");
    // No list to read a scope from: every qualifier stays.
    expect(tabs.dbTabScope({ ...dbConnState(), tables: [] }, false)).toBeNull();
    // The strip paints the short name and titles the full one.
    Object.assign(dbConnState(), { tables: [{ schema: "acme_app_dev", name: "orders" }] });
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "acme_app_dev" });
    tabs.renderDbTabs();
    const card = document.querySelector<HTMLElement>("#dbTabStrip .db-tab")!;
    expect(card.querySelector(".db-tab-name")!.textContent).toBe("orders");
    expect(card.title).toBe("acme_app_dev.orders");
  });

  it("the overflow menu lists the whole set, ticks the current, offers the bulk closes (D4)", () => {
    for (const t of ["t1", "t2", "t3"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tabs.dbActivateTab(1); // t2 in front
    dirty(tableAt(2));
    const items = tabs.dbTabsMenuItems();
    // Three object rows in strip order, then sep, then the bulk closes (Close all last: danger).
    type ActionItem = Extract<typeof items[number], { label: string }>;
    const ofLabel = (i: typeof items[number]): i is ActionItem => { return "label" in i; };
    const objects = items.filter((i) => { return ofLabel(i) && !!i.pick; }) as ActionItem[];
    expect(objects.map((i) => { return i.label; })).toEqual(["t1", "t2", "t3"]);
    expect(objects[1].on, "the current object is the ticked one").toBe(true);
    expect(objects[0].on).toBeFalsy();
    expect(objects[2].dot, "the dirty tab carries its dot").toBe(true);
    const labels = items.filter(ofLabel).map((i) => { return i.label; });
    expect(labels).toContain("Close others");
    expect(labels).toContain("Close to the right");
    expect(labels.at(-1)).toBe("Close all");
    // Nothing open: no rows, and no bulk closes either — the menu is not offered at all.
    // (t3 above is dirty, so this close goes through the batch confirm.)
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    tabs.dbCloseAllTabs();
    expect(tabs.dbTabsMenuItems()).toEqual([]);
  });

  it("Close others leaves only the current tab; Close to the right cuts only what follows (D4)", () => {
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    for (const t of ["t1", "t2", "t3", "t4"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tabs.dbActivateTab(1); // t2
    tabs.dbCloseToRight(1); // the pivot is the card the menu was opened on
    expect(ask).not.toHaveBeenCalled(); // everything to the right was clean
    expect(dbTabs().map((t) => { return t.kind === "table" ? t.table : t.kind; })).toEqual(["t1", "t2"]);
    tabs.dbCloseOthers();
    expect(dbTabs().map((t) => { return t.kind === "table" ? t.table : t.kind; })).toEqual(["t2"]);
    expect(liveTable().table).toBe("t2");
  });

  it("a bulk close that would drop buffered writes asks once — and a refusal keeps every tab", () => {
    for (const t of ["t1", "t2", "t3"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tabs.dbActivateTab(0);
    dirty(tableAt(1));
    dirty(tableAt(2));
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    tabs.dbCloseAllTabs();
    expect(ask, "one question for the whole batch").toHaveBeenCalledTimes(1);
    expect(String(ask.mock.calls[0][0]), "naming the total across the batch").toContain("2");
    expect(dbTabs().length, "no was an answer for every tab at once").toBe(3);
  });

  it("the middle button closes through the ×'s path — dirty asks first (D8)", () => {
    const ask = vi.spyOn(globalThis, "confirm").mockReturnValue(false);
    for (const t of ["t1", "t2"]) tabs.dbOpenTab({ kind: "table", table: t, schema: null });
    tabs.renderDbTabs();
    const cards = Array.from(document.querySelectorAll("#dbTabStrip .db-tab"));
    dirty(tableAt(0));
    expect(tabs.dbTabsAuxClick(cards[0])).toBe(true);
    expect(ask, "a dirty card asks before a middle close takes it").toHaveBeenCalled();
    expect(dbTabs().length).toBe(2);
    ask.mockReturnValue(true);
    tabs.dbTabsAuxClick(cards[0]);
    expect(dbTabs().length, "answered, the middle close is the ×").toBe(1);
    expect(tabs.dbTabsAuxClick(document.body)).toBe(false);
  });

  it("the markup: cards scroll, the exits are pinned outside the run (D1)", () => {
    tabs.renderDbTabs();
    const strip = document.getElementById("dbTabStrip")!;
    expect(strip.querySelector(".db-tabstrip-scroll"), "the card run has its own scroller").not.toBeNull();
    const end = strip.querySelector(".db-tabstrip-end")!;
    expect(end).not.toBeNull();
    // Nothing is open: no overflow seat — there is nothing to list.
    expect((end.querySelector("[data-dbtabmenu]") as HTMLElement).hidden).toBe(true);
    tabs.dbOpenTab({ kind: "table", table: "t1", schema: null });
    tabs.renderDbTabs();
    const end2 = document.querySelector("#dbTabStrip .db-tabstrip-end")!;
    expect((end2.querySelector("[data-dbtabmenu]") as HTMLElement).hidden).toBe(false);
    expect(strip.querySelector(".db-tabstrip-scroll .db-tab"), "the cards live inside the run").not.toBeNull();
    expect(end2.querySelector("[data-dbtabadd]"), "the + lives in the pinned end").not.toBeNull();
  });
});

/* A type-level pin: the union is the split's safety net, so the three record shapes stay
   assignable to DbTab and nothing else. It never runs — tsc is the assertion. */
function _shapes(a: DbTableTab, b: DbSqlTab, c: DbKeyTab): DbTab[] {
  return [a, b, c];
}
void _shapes;

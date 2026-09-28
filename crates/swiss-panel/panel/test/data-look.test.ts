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

/* docs/46 P7 (§3.7): Data wears the library's look. Each case pins one item of the page's
   acceptance list against the real renderers on a real DOM: the status line stops repeating
   the head, the connection row's dialect is a mark or a tag (never a bordered chip), the
   drawers tick the current pick instead of painting it blue, the two segmented strips are
   seg(), every context menu is the library's floating menu, and the grid's CSS draws no
   vertical lines, grey comments and row controls that wait for the row. */

import { afterAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbConn as dbConnState, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";
import { tr } from "../src/i18n.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";
import { sheet } from "./styles.js";
import type { DbSqlTab, DbTableTab } from "../src/types/state.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, {
  fetch: () => Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(dbPage()) }),
});
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});

document.body.innerHTML = shellSkeleton();
const view = await import("../src/data-view.js");
const grid = await import("../src/data-grid.js");
const csv = await import("../src/data-csv.js");
const edit = await import("../src/data-edit.js");
const activity = await import("../src/data-activity.js");
const browsers = await import("../src/data-browsers.js");

/** An open table with one row, the way a loaded /data page leaves it. */
function openTable(editable: boolean, editNote?: string): DbTableTab {
  const t = freshTab("table");
  t.table = "customers";
  t.schema = "demo_shop";
  t.pane = "data";
  t.data = dbPage({
    schema: "demo_shop", table: "customers", total: 1, editable, editNote,
    columns: [dbCol("id", { isPrimaryKey: true, dataType: "int" }), dbCol("name", { comment: "Display name" })],
    rows: [{ id: 1, name: "Ada Example" }],
    primaryKey: ["id"],
  });
  dbTabs().length = 0;
  dbTabs().push(t);
  return t;
}

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  document.getElementById("menu")?.remove();
  unmountDbView();
  mountDbView();
  Object.assign(dbConnState(), { conns: [dbConn("demo-shop", "mysql", { label: "demo_shop @ db:3306" })], conn: "demo-shop", databases: [] });
  view.renderDbView();
});

describe("the status line leaves editability to the head (docs/46 §3.7)", () => {
  it("an editable table says so once, in the head", () => {
    openTable(true);
    grid.renderDbToolbar();
    grid.renderDbGrid();
    const said = tr("dataGrid.editableChangesBufferUntil");
    expect(document.querySelector("#dbHead .db-meta")!.textContent).toContain(said);
    expect(document.getElementById("dbStatus")!.textContent).not.toContain(said);
    // What the status line is for stays: the pager, the page size, the connection.
    expect(document.querySelectorAll("#dbStatus [data-pg]").length).toBe(2);
    expect(document.querySelector("#dbStatus [data-tb='pagesize']")).not.toBeNull();
    expect(document.querySelector("#dbStatus .db-status-conn")!.textContent).toBe("demo_shop @ db:3306");
  });

  it("a read-only table's reason is the head's too, not a second copy in the status line", () => {
    const why = "Read-only: other_db is not this connection's configured database (demo_shop).";
    openTable(false, why);
    grid.renderDbToolbar();
    grid.renderDbGrid();
    expect(document.querySelector("#dbHead .db-meta")!.textContent).toContain(why);
    expect(document.getElementById("dbStatus")!.textContent).not.toContain(why);
  });
});

describe("the connection row's dialect (docs/46 §3.7: .db-chip becomes tag())", () => {
  it("a mapped dialect is its glyph, bare; a word dialect is a mono tag", () => {
    view.renderDbSide();
    const row = document.getElementById("dbConnRow")!;
    expect(row.querySelector(".db-chip"), "no bordered chip").toBeNull();
    const mark = row.querySelector("svg.db-row-mark");
    expect(mark, "the mysql glyph").not.toBeNull();
    expect(mark!.getAttribute("aria-label")).toBe("mysql");
    Object.assign(dbConnState(), { conns: [dbConn("demo-shop", "sqlite")] });
    view.renderDbSide();
    const word = document.querySelector("#dbConnRow .tag.mono");
    expect(word, "a word dialect is the library's tag").not.toBeNull();
    expect(word!.textContent).toBe("sqlite");
  });
});

describe("the drawers tick the current pick (docs/46 §3.7, rule 15)", () => {
  it("every drawer row has a tick column; only the current connection's holds the check", () => {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis"), dbConn("demo-shop", "mysql")] });
    view.renderDbSide();
    (document.getElementById("dbConnRow") as HTMLButtonElement).click();
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#dbConnDrawer .db-drow"));
    expect(rows.length).toBe(2);
    rows.forEach((r) => { expect(r.firstElementChild!.classList.contains("db-drow-tick")).toBe(true); });
    const ticked = rows.filter((r) => !!r.querySelector(".db-drow-tick svg"));
    expect(ticked.length).toBe(1);
    expect(ticked[0].querySelector(".db-drow-name")!.textContent).toBe("demo-shop");
  });
});

describe("the segmented strips are seg() (docs/46 §3.7: .db-tabs goes)", () => {
  it("the table tab's Data / Form / Structure / DDL", () => {
    openTable(true);
    grid.renderDbToolbar();
    expect(document.querySelector(".db-tabs")).toBeNull();
    const s = document.querySelector("#dbHead .seg[role='tablist']")!;
    expect(s).not.toBeNull();
    expect(Array.from(s.querySelectorAll("button")).map((b) => b.dataset.dtab)).toEqual(["data", "form", "structure", "ddl"]);
    expect(s.querySelector("button[aria-selected='true']")!.getAttribute("data-dtab")).toBe("data");
  });

  it("the Structure tab's Columns / Indexes / Foreign keys", () => {
    const t = openTable(true);
    t.pane = "columns";
    t.detail = { schema: "demo_shop", table: "customers", columns: t.data!.columns, primaryKey: ["id"], indexes: [], foreignKeys: [], ddl: "" } as unknown as DbTableTab["detail"];
    grid.renderDbGrid();
    expect(document.querySelector(".db-tabs")).toBeNull();
    const sub = document.querySelector("#dbGridWrap .db-struct-sub .seg")!;
    expect(Array.from(sub.querySelectorAll("button")).map((b) => b.dataset.dtab)).toEqual(["columns", "indexes", "fks"]);
  });

  it("a script's result tabs", () => {
    const t = freshTab("sql") as DbSqlTab;
    const r = { columns: ["n"], rows: [{ n: 1 }], rowCount: 1 };
    t.sqlResults = [r, { ...r, tabLabel: "second" }];
    t.sqlResult = r;
    t.resultTab = 0;
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    expect(document.querySelector(".db-tabs")).toBeNull();
    const s = document.querySelector("#dbHead .seg")!;
    expect(Array.from(s.querySelectorAll("button")).map((b) => b.dataset.rtab)).toEqual(["0", "1"]);
  });
});

describe("every Data context menu is the library's floating menu (docs/46 §3.7: .ctx-menu goes)", () => {
  const at = (): MouseEvent => new MouseEvent("contextmenu", { clientX: 40, clientY: 40, cancelable: true });

  it("a table cell's menu", () => {
    openTable(true);
    csv.dbCellMenu(at(), { id: 1, name: "Ada Example" }, "1", "name", null);
    expect(document.querySelector(".ctx-menu")).toBeNull();
    const m = document.getElementById("menu")!;
    expect(m.classList.contains("float")).toBe(true);
    expect(m.getAttribute("role")).toBe("menu");
    const labels = Array.from(m.querySelectorAll("button")).map((b) => b.textContent);
    expect(labels[0]).toBe(tr("logs.copyValue"));
    // The no-rows-checked line stays a shown-but-refused row.
    const hint = Array.from(m.querySelectorAll<HTMLButtonElement>("button")).find((b) => b.textContent === tr("dataCsv.noRowsChecked"))!;
    expect(hint.disabled).toBe(true);
  });

  it("a query result cell's menu", () => {
    const t = freshTab("sql") as DbSqlTab;
    const r = { columns: ["n"], rows: [{ n: 1 }], rowCount: 1 };
    t.sqlResults = [r];
    t.sqlResult = r;
    dbTabs().length = 0;
    dbTabs().push(t);
    csv.dbResultCellMenu(at(), r.rows[0], "n");
    expect(document.querySelector(".ctx-menu")).toBeNull();
    expect(document.querySelector("#menu.menu.float button")!.textContent).toBe(tr("logs.copyValue"));
  });

  it("the Table menu", () => {
    openTable(true);
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    edit.dbTableMenu(anchor);
    expect(document.querySelector(".ctx-menu")).toBeNull();
    const m = document.querySelector("#menu.menu.float")!;
    expect(Array.from(m.querySelectorAll("button")).map((b) => b.textContent)).toContain(tr("dataEdit.renameTable"));
    expect(m.querySelector("hr"), "the generate group and the DDL group are separated").not.toBeNull();
  });
});

describe("the activity monitor's own session is a tag (docs/46 §3.7: .db-keytype goes)", () => {
  it("the panel's row carries tag(), not a hand-drawn pill", () => {
    const t = freshTab("activity");
    t.activityRows = [{ pid: 7, user: "root", state: "", wait: "", seconds: 1, query: "SELECT 1", own: true }];
    dbTabs().length = 0;
    dbTabs().push(t);
    activity.dbActivityRender();
    expect(document.querySelector(".db-keytype")).toBeNull();
    const own = document.querySelector("#dbGridWrap tr.db-act-own .tag")!;
    expect(own.textContent).toBe(tr("dataActivity.thisPanel"));
    expect(own.getAttribute("title")).toBe(tr("dataActivity.thisPanelTip"));
  });
});

/* Found on the P7 walk (19996, a throwaway MySQL): the pane's ⋯ - the Activity monitor's only
   door - never showed on a SQL connection, and a Redis key list's search still said "Filter
   tables". dbSyncKind bailed out on a missing #dbSqlExplain: docs/43 M4 folded the console's
   flat row (Explain, Format) into the toolbar's overflow and dropped those ids, so the early
   return has fired on every mount and switch since. The same fold lost the rule that a Redis
   command has no plan and no SQL formatting: its overflow offered both. */
describe("the connection's kind reaches the page (docs/43 M4 regression)", () => {
  it("a SQL connection shows the pane's ⋯ and the table grammar", () => {
    const more = document.getElementById("dbMore")!;
    expect(more.hidden, "the Activity door is open").toBe(false);
    expect((document.getElementById("dbGrep") as HTMLInputElement).placeholder).toBe("a*, b|c");
    expect(document.getElementById("dbSqlHint")!.textContent).toBe(tr("dataView.blankLineStartsNew"));
  });

  it("a Redis connection hides it and speaks of keys and commands", () => {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    view.renderDbView();
    expect(document.getElementById("dbMore")!.hidden).toBe(true);
    expect((document.getElementById("dbGrep") as HTMLInputElement).placeholder).toBe(tr("dataView.filterKeys"));
    expect(document.getElementById("dbSqlHint")!.textContent).toBe(tr("dataView.redisConsoleHint"));
  });

  it("a Redis command tab's overflow offers no plan and no SQL formatting", () => {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("sql") as DbSqlTab;
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    (document.querySelector("#dbHead [aria-haspopup='menu']") as HTMLButtonElement).click();
    const labels = Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent);
    expect(labels).not.toContain(tr("dataView.explain"));
    expect(labels).not.toContain(tr("dataView.explainAnalyze"));
    expect(labels).not.toContain(tr("dataView.format"));
    expect(labels).toContain(tr("dataView.saveFavorites"));
  });

  /* Restoring the pane's ⋯ put it beside the open object's own ⋯: two identical glyphs, one
     hairline apart, opening different menus. One ⋯ per head - the object's carries Activity…
     last, and the pane's stands in only while no object has one. */
  it("one ⋯ per head: an open table's ⋯ carries Activity… and the pane's steps aside", () => {
    openTable(true);
    grid.renderDbToolbar();
    expect(document.getElementById("dbMore")!.hidden).toBe(true);
    expect(document.querySelectorAll(".db-headrow [aria-haspopup='menu']:not([hidden])").length).toBe(1);
    (document.querySelector("#dbHead [aria-haspopup='menu']") as HTMLButtonElement).click();
    const labels = Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent);
    expect(labels[labels.length - 1]).toBe(tr("dataView.activity"));
  });

  it("with nothing open, the pane's ⋯ is the Activity monitor's door", () => {
    dbTabs().length = 0;
    dbTabs().push(freshTab("table"));
    grid.renderDbToolbar();
    expect(document.getElementById("dbMore")!.hidden).toBe(false);
  });

  it("a SQL console's overflow keeps them", () => {
    const t = freshTab("sql") as DbSqlTab;
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    (document.querySelector("#dbHead [aria-haspopup='menu']") as HTMLButtonElement).click();
    const labels = Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent);
    expect(labels).toContain(tr("dataView.explain"));
    expect(labels).toContain(tr("dataView.format"));
  });
});

/* Found on the P7 walk: a Redis hash drew "+ Field" and ⋯ twice - in the head (docs/43 M4's
   one primary + one overflow) and again on the value view's own meta line, which predates the
   fold. The meta line's "+ Field" carried no data-radd address, so a real click on it did
   nothing at all; the head's buffered the row. The value view keeps its facts, the head keeps
   the actions. */
describe("a Redis key's actions live in the head once (found on the P7 walk)", () => {
  it("the value view's meta line carries facts, no buttons", () => {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("key");
    t.redisKey = "user:1";
    t.redisValue = { key: "user:1", type: "hash", ttl: -1, value: { name: "Ada Example", tier: "pro" }, length: 2 };
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    grid.renderDbGrid();
    const meta = document.querySelector("#dbGridWrap .db-detail-meta")!;
    expect(meta.textContent).toContain("user:1 · hash");
    expect(meta.querySelectorAll("button:not(.db-ttl)").length, "no second + Field or ⋯").toBe(0);
    expect(document.querySelectorAll("#dbHead [data-radd]").length).toBe(1);
    expect(document.querySelectorAll("#dbHead [aria-haspopup='menu']").length).toBe(1);
  });
});

/* Also found on the P7 walk. The typed value table claims the row grid's vocabulary, and the SQL
   grid's buffered insert carries its own remove (data-irm). The Redis one passed "not deletable"
   and drew an empty control cell, so the row was a 13px blank strip whose only way back was
   Discard-all - the data-rins branch of the dispatcher could never fire. And its field and value
   headers wore the row-control class to say "no sort", which gave every column the control's
   58px and stretched the control column to a third of the table. */
describe("a Redis value's table speaks the row grid's words (found on the P7 walk)", () => {
  function openKey(key: string, type: string, value: unknown, inserts: Record<string, unknown>[]): void {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("key");
    t.redisKey = key;
    t.redisValue = { key, type, ttl: -1, value, length: 2 };
    t.redisEdits = { key, type, updates: {}, deletes: {}, inserts };
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbGrid();
  }

  it("a buffered insert can be removed on its own, even from a list", () => {
    openKey("queue:mail", "list", ["a", "b"], [{}]);
    const ctl = document.querySelector<HTMLElement>("#dbGridWrap tr.db-ins [data-rins='0']");
    expect(ctl, "the insert row's remove").not.toBeNull();
    expect(ctl!.title).toBe(tr("dataGrid.removeBufferedInsert"));
  });

  it("the remove drops that insert and no other", () => {
    openKey("user:1", "hash", { name: "Ada Example" }, [{ field: "a" }, { field: "b" }]);
    const ctl = document.querySelector<HTMLElement>("#dbGridWrap tr.db-ins [data-rins='0']")!;
    expect(browsers.dbRedisClick(ctl)).toBe(true);
    expect((dbTabs()[0] as { redisEdits?: { inserts: unknown[] } }).redisEdits!.inserts).toEqual([{ field: "b" }]);
    expect(document.querySelectorAll("#dbGridWrap tr.db-ins").length).toBe(1);
  });

  it("only the control column is a control column", () => {
    openKey("user:1", "hash", { name: "Ada Example" }, []);
    const ths = Array.from(document.querySelectorAll("#dbGridWrap thead th"));
    expect(ths.length).toBe(3);
    expect(ths.map((th) => th.classList.contains("db-rowctl"))).toEqual([true, false, false]);
    expect(ths.slice(1).every((th) => th.classList.contains("db-nosort")), "field and value do not sort").toBe(true);
    expect(sheet("views.css")).toContain(".db-grid thead th:is(.db-rowctl, .db-nosort) { cursor: default; }");
  });
});

/* Found on the docs/46 P7-2 walk: the cell editor's head said "PK" and printed the whole row.
   The buffered row's address carries every original value on purpose (docs/22 W4.2: the other
   columns are the optimistic lock), but on a keyed table the key is its key columns. */
describe("the cell editor's head names the row by its key (found on the P7-2 walk)", () => {
  it("title is the column, the sub is table · PK with the key columns only", async () => {
    const cell = await import("../src/data-cell.js");
    openTable(true);
    cell.dbOpenCellEditor("update", "[1]", -1, "name", { pk: { id: 1, name: "Ada Example" }, orig: "Ada Example" });
    const head = document.querySelector("#sheet .sheet-head")!;
    expect(document.getElementById("sheet")!.hidden).toBe(false);
    expect(head.querySelector("h2")!.textContent).toBe("name");
    expect(head.querySelector(".sheet-sub")!.textContent).toBe("demo_shop.customers" + tr("dataCell.pkPk", { pk: "{\"id\":1}" }));
  });
});

/* docs/46 P7-2: renaming a key is the library's one-field sheet. The hand-built one closed
   before RENAME ran, so a refusal lost what was typed, and an empty name was a silent cancel. */
describe("a Redis key's rename is the library's one-field sheet (docs/46 P7-2)", () => {
  const menuItem = (label: string): HTMLButtonElement =>
    Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === label)!;
  const flush = (): Promise<void> => new Promise((r) => { setTimeout(r, 0); });

  it("opens on the key, refuses an empty name inline, keeps the typed name when redis refuses", async () => {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("key");
    t.redisKey = "user:1";
    t.redisValue = { key: "user:1", type: "hash", ttl: -1, value: { name: "Ada Example" }, length: 1 };
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    document.querySelector<HTMLButtonElement>("#dbHead [aria-haspopup='menu']")!.click();
    menuItem(tr("dataBrowsers.renameEllipsis")).click();

    const host = document.getElementById("sheet")!;
    expect(host.hidden, "the sheet is on screen").toBe(false);
    expect(host.querySelector(".sheet-head h2")!.textContent).toBe(tr("dataBrowsers.renameKey"));
    const input = document.getElementById("g-name") as HTMLInputElement;
    expect(input.value).toBe("user:1");
    const save = document.getElementById("g-save") as HTMLButtonElement;
    expect(save.textContent).toBe(tr("dataBrowsers.rename2"));

    input.value = "  ";
    save.click();
    await flush();
    expect(host.hidden).toBe(false);
    expect(document.getElementById("g-err")!.hidden).toBe(false);
    expect(document.getElementById("g-err")!.textContent).toBe(tr("ui.nameRequired"));

    const ok = globalThis.fetch;
    let sent = "";
    Object.assign(globalThis, {
      fetch: (_u: string, init?: { body?: string }) => {
        sent = init?.body ?? "";
        return Promise.resolve({ ok: false, status: 400, json: () => Promise.resolve({ error: "ERR no such key" }) });
      },
    });
    try {
      input.value = "user:2";
      save.click();
      await flush();
      await flush();
      expect(JSON.parse(sent)).toEqual({ command: "RENAME user:1 user:2" });
      expect(host.hidden, "a refused rename keeps the sheet").toBe(false);
      expect(input.value).toBe("user:2");
    } finally {
      Object.assign(globalThis, { fetch: ok });
    }
  });
});

/* The owner's report (2026-09-28), on a grid whose `id` header read `id 🔑 bigint`: "my id is
   short - the type should go on its own line, and a comment on one more." A column was as wide
   as its name and its type side by side; stacked, it is as wide as the longer of the two. */
describe("a column header stacks its name, its type and its comment", () => {
  const lines = (th: Element): string[] => Array.from(th.querySelectorAll(":scope > div")).map((d) => d.className);

  it("the name line keeps the name and its marks; the type and the comment each take a line under it", () => {
    openTable(true);
    grid.renderDbGrid();
    const [id, name] = Array.from(document.querySelectorAll("#dbGridWrap thead th.db-col"));
    expect(lines(id)).toEqual(["db-col-main", "db-col-type", "db-col-comment"]);
    expect(id.querySelector(".db-col-main .db-key"), "the PK mark rides the name").not.toBeNull();
    expect(id.querySelector(".db-col-main .db-col-type"), "no type beside the name").toBeNull();
    expect(id.querySelector(":scope > .db-col-type")!.textContent).toBe("int");
    // The comment-less column keeps an empty line, so the header row stays one height.
    expect(id.querySelector(":scope > .db-col-comment")!.textContent).toBe("");
    expect(lines(name)).toEqual(["db-col-main", "db-col-type", "db-col-comment"]);
    expect(name.querySelector(":scope > .db-col-type")!.textContent).toBe("text");
    expect(name.querySelector(":scope > .db-col-comment")!.textContent).toBe("Display name");
  });

  it("a table with no comments draws two lines, name and type", () => {
    const t = openTable(true);
    t.data!.columns.forEach((c) => { c.comment = null; });
    grid.renderDbGrid();
    const ths = Array.from(document.querySelectorAll("#dbGridWrap thead th.db-col"));
    expect(ths.map(lines)).toEqual([["db-col-main", "db-col-type"], ["db-col-main", "db-col-type"]]);
  });
});

/* The owner, on the key head's ⋯ (2026-09-28): "there should be somewhere to set the TTL
   here, this is nowhere near complete." It offered one nested item (Rename or delete this
   key) and the console. The acts a key has now sit in it directly, TTL among them. */
describe("a Redis key's ⋯ offers every act on the key, one press away", () => {
  function openKeyHead(): void {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("key");
    t.redisKey = "user:1";
    t.redisValue = { key: "user:1", type: "hash", ttl: -1, value: { name: "Ada Example" }, length: 1 };
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    grid.renderDbGrid();
    const more = document.querySelector<HTMLElement>("#dbHead [aria-haspopup='menu']")!;
    more.click();
  }
  const labels = (): string[] =>
    Array.from(document.querySelectorAll("#menu button")).map((b) => b.textContent || "");

  it("names the TTL, the rename and the delete without a submenu", () => {
    openKeyHead();
    expect(labels()).toEqual(["Set the TTL…", "Rename…", "Delete…", "Command"]);
    expect(labels().some((l) => /Rename or delete/.test(l)), "no nested door").toBe(false);
  });

  it("the TTL item opens the sheet on the key's own expiry", () => {
    openKeyHead();
    Array.from(document.querySelectorAll<HTMLElement>("#menu button")).find((b) => /Set the TTL/.test(b.textContent || ""))!.click();
    const head = document.querySelector("#sheet .sheet-head")!;
    expect(document.getElementById("sheet")!.hidden).toBe(false);
    expect(head.querySelector("h2")!.textContent).toBe("Set the TTL");
    expect(head.textContent).toContain("user:1");
    const input = document.querySelector<HTMLInputElement>("#sheet input")!;
    expect(input.value, "no expiry seeds an empty field").toBe("");
  });
});

describe("the grid's look in views.css (docs/46 §3.7, U7)", () => {
  const css = sheet("views.css");
  const rule = (sel: string): string => {
    const i = css.indexOf("\n    " + sel + " {");
    expect(i, sel + " has a rule").toBeGreaterThan(-1);
    return css.slice(i, css.indexOf("}", i));
  };

  /* The header's comment used to be --text-3, a step lighter than prose, on the reasoning that
     small print under a header should recede. Once the type took a line of its own - also
     --text-3 - the two greys stacked and read as one block (the owner, 2026-09-28: "this
     Comment colour is not right"). The comment is prose, so it takes prose's --text-2 and the
     type keeps the lighter grey; the two lines are told apart by weight of colour, not only
     by typeface. */
  it("a column comment is prose grey, a step darker than the type above it", () => {
    expect(rule(".db-col-comment")).toContain("color: var(--text-2)");
    expect(rule(".db-col-type")).toContain("color: var(--text-3)");
    expect(rule(".db-tip .t-comment")).toContain("color: var(--text-2)");
    expect(rule(".db-cell.db-det-comment")).toContain("color: var(--text-2)");
    expect(css).not.toMatch(/deliberate exception/);
  });

  it("a header's type line is a caption under the name, not an inset beside it", () => {
    const type = rule(".db-col-type");
    expect(type).not.toContain("margin-left");
    expect(type).toContain("font-weight: var(--w-body)");
    expect(type).toContain("text-overflow: ellipsis");
    // An empty div has no height, so a comment-less header was a line shorter and, centred,
    // drew its name half a line below its neighbours' (found on the 2026-09-28 walk).
    expect(rule(".db-col-comment:empty::before")).toContain('content: "\\a0"');
  });

  it("the grid draws no vertical lines and its rows are 4px taller", () => {
    expect(rule(".db-grid th, .db-grid td")).not.toContain("border-right");
    expect(rule(".db-grid td")).toContain("padding-top: 6px");
  });

  it("a row's checkbox and delete wait for the row: hover, focus inside, or checked", () => {
    const hide = rule(".db-grid tbody tr:not(:hover, :focus-within, .db-sel, .db-del, .db-ins) > td.db-rowctl > :is(.db-selbox, .db-act)");
    expect(hide).toContain("opacity: 0");
  });

  it("the selected table is the source-list selection: bar and tint, not blue text", () => {
    const sel = rule(".db-table.sel");
    expect(sel).toContain("box-shadow: inset 2px 0 0 var(--accent)");
    expect(rule(".db-table.sel .db-table-name")).not.toContain("var(--accent)");
    expect(css).not.toMatch(/\.db-drow\.on[^{]*\{[^}]*--accent/);
  });

  it("the replaced hand-drawn shapes are gone", () => {
    for (const cls of [".db-tabs", ".ctx-menu", ".db-chip", ".db-keytype"]) {
      expect(css.includes(cls + " ") || css.includes(cls + "{") || css.includes(cls + ":") || css.includes(cls + ","), cls).toBe(false);
    }
  });
});

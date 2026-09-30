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

/* SPEC §panel.pages (§3.7): Data wears the library's look. Each case pins one item of the page's
   acceptance list against the real renderers on a real DOM: the status line stops repeating
   the head, the connection row's dialect is a mark or a tag (never a bordered chip), the
   drawers tick the current pick instead of painting it blue, the two segmented strips are
   seg(), every context menu is the library's floating menu, and the grid's CSS draws no
   vertical lines, grey comments and row controls that wait for the row. */

import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbConn as dbConnState, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";
import { tr } from "../src/i18n.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";
import { sheet } from "./styles.js";
import type { DbKeyTab, DbSqlTab, DbTableTab } from "../src/types/state.js";

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

describe("the status line leaves editability to the head (SPEC §panel.pages)", () => {
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

describe("the connection row's dialect (SPEC §panel.pages: .db-chip becomes tag())", () => {
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

describe("the drawers tick the current pick (SPEC §panel.pages, rule 15)", () => {
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

describe("the segmented strips are seg() (SPEC §panel.pages: .db-tabs goes)", () => {
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

describe("every Data context menu is the library's floating menu (SPEC §panel.pages: .ctx-menu goes)", () => {
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

describe("the activity monitor's own session is a tag (SPEC §panel.pages: .db-keytype goes)", () => {
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
   tables". dbSyncKind bailed out on a missing #dbSqlExplain: SPEC §data.tabs folded the console's
   flat row (Explain, Format) into the toolbar's overflow and dropped those ids, so the early
   return has fired on every mount and switch since. The same fold lost the rule that a Redis
   command has no plan and no SQL formatting: its overflow offered both. */
describe("the connection's kind reaches the page (SPEC §data.tabs, a regression)", () => {
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

/* Found on the P7 walk: a Redis hash drew "+ Field" and ⋯ twice - in the head (SPEC §data.tabs's
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

/* Found on a walk of SPEC §panel.pages: the cell editor's head said "PK" and printed the whole row.
   The buffered row's address carries every original value on purpose (SPEC §data.edits: the other
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

/* SPEC §panel.pages: renaming a key is the library's one-field sheet. The hand-built one closed
   before RENAME ran, so a refusal lost what was typed, and an empty name was a silent cancel. */
describe("a Redis key's rename is the library's one-field sheet (SPEC §panel.pages)", () => {
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
      // The exact arguments through the pipeline route, and RENAMENX - never a line to split.
      expect(JSON.parse(sent)).toEqual({ commands: [["RENAMENX", "user:1", "user:2"]] });
      expect(host.hidden, "a refused rename keeps the sheet").toBe(false);
      expect(input.value).toBe("user:2");
    } finally {
      Object.assign(globalThis, { fetch: ok });
    }
  });
});

/* 2026-09-29, "各个极端情况，你都需要考虑清楚": a key's own acts - Delete, Rename, the TTL - built
   a console LINE ("DEL " + key) that the gateway splits like a shell. A key named `my key` was
   two arguments: Delete removed the keys `my` and `key` and left `my key` standing. Every act
   below sends the key as ONE argument, and each case names a key that line-building broke. */
describe("a key's own acts send the key's exact bytes (2026-09-29)", () => {
  const menuItem = (label: string): HTMLButtonElement =>
    Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === label)!;
  const flush = (): Promise<void> => new Promise((r) => { setTimeout(r, 0); });
  const sent: { url: string; body: unknown }[] = [];
  let reply: (url: string) => { ok: boolean; body: unknown } = () => ({ ok: true, body: { replies: [1] } });
  const realFetch = globalThis.fetch;
  const realConfirm = globalThis.confirm;

  beforeEach(() => {
    sent.length = 0;
    reply = () => ({ ok: true, body: { replies: [1] } });
    Object.assign(globalThis, {
      fetch: (url: string, init?: { body?: string }) => {
        sent.push({ url: String(url), body: init && init.body ? JSON.parse(init.body) : null });
        const r = reply(String(url));
        return Promise.resolve({ ok: r.ok, status: r.ok ? 200 : 400, json: () => Promise.resolve(r.body) });
      },
      confirm: () => true,
    });
  });
  afterAll(() => { Object.assign(globalThis, { fetch: realFetch, confirm: realConfirm }); });

  const openKey = (key: string): void => {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("key");
    t.redisKey = key;
    t.redisValue = { key, type: "string", ttl: -1, value: "v", length: 1 };
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    document.querySelector<HTMLButtonElement>("#dbHead [aria-haspopup='menu']")!.click();
  };
  const pipelined = (): unknown[] => sent.filter((x) => x.url.endsWith("/redis-pipeline")).map((x) => (x.body as { commands: unknown }).commands);

  const NAMES = ["my key", "back\\slash", "it's", 'say "hi"', "tab\there", "用户 1", "-dash-first", "$HOME"];

  for (const name of NAMES) {
    it("Delete removes exactly " + JSON.stringify(name) + " - one argument, never a line", async () => {
      openKey(name);
      menuItem(tr("dataBrowsers.delete")).click();
      await flush(); await flush();
      expect(pipelined()[0]).toEqual([["DEL", name]]);
      expect(sent.some((x) => x.url.endsWith("/command")), "nothing went through the line splitter").toBe(false);
    });
  }

  it("the key \"\" heads its own view - its head read \"Keys\", as if nothing were open", () => {
    openKey("");
    expect(document.querySelector("#dbHead .db-title")!.textContent).toBe('""');
    openKey(" x");
    expect(document.querySelector("#dbHead .db-title")!.textContent, "edge blanks show").toBe('" x"');
  });

  it("a key whose name is not UTF-8 shows as redis-cli prints it, and a click opens nothing", () => {
    // Spring's JDK serializer writes names like this. The walk sends redis-cli's printing of
    // the bytes and marks it; that text is not the key, so nothing may be built from it.
    const bin = '"\\xac\\xed\\x00\\x05t\\x00\\x04user"';
    Object.assign(dbConnState(), {
      conns: [dbConn("demo-cache", "redis")], conn: "demo-cache",
      redis: { keys: [{ key: bin, type: "string", ttl: -1, binary: true }, { key: "plain", type: "string", ttl: -1 }], cursor: "0", done: true, total: 2 },
    });
    dbTabs().length = 0;
    dbTabs().push(freshTab("sql"));
    const keyTabs = (): number => dbTabs().filter((t) => t.kind === "key").length;
    view.renderDbTables();
    const rows = Array.from(document.querySelectorAll<HTMLElement>("#dbTables .db-key-row"));
    const b = rows.find((r) => r.dataset.rkey === bin)!;
    expect(b.querySelector(".db-table-name")!.textContent).toBe(bin);
    expect(b.querySelector(".db-table-meta")!.textContent).toBe(tr("dataView.binaryKeyMeta"));
    b.click();
    expect(keyTabs(), "no tab over a name that addresses nothing").toBe(0);
    expect(document.getElementById("toast")!.textContent).toBe(tr("dataView.binaryKeyTitle"));
    rows.find((r) => r.dataset.rkey === "plain")!.click();
    expect(keyTabs(), "while a UTF-8 row still opens its tab").toBe(1);
  });

  it("Delete says so when the key was already gone (DEL answered 0)", async () => {
    reply = (url) => url.endsWith("/redis-pipeline") ? { ok: true, body: { replies: [0] } } : { ok: true, body: { keys: [], cursor: "0", done: true } };
    openKey("gone:1");
    menuItem(tr("dataBrowsers.delete")).click();
    await flush(); await flush();
    expect(document.getElementById("toast")!.textContent).toBe(tr("dataBrowsers.keyAlreadyGone", { key: "gone:1" }));
  });

  it("Rename onto a name that EXISTS is refused inline - RENAMENX answered 0, and no RENAME follows", async () => {
    reply = () => ({ ok: true, body: { replies: [0] } });
    openKey("my key");
    menuItem(tr("dataBrowsers.renameEllipsis")).click();
    const input = document.getElementById("g-name") as HTMLInputElement;
    input.value = "taken key";
    (document.getElementById("g-save") as HTMLButtonElement).click();
    await flush(); await flush();
    expect(pipelined()).toEqual([[["RENAMENX", "my key", "taken key"]]]);
    expect(document.getElementById("sheet")!.hidden, "the sheet stays, with the typed name").toBe(false);
    expect(document.getElementById("g-err")!.textContent).toBe(tr("dataBrowsers.renameTargetExists", { to: "taken key" }));
  });

  it("the TTL goes as its own argument on a spaced key, and 0 - which would delete the key - is refused inline", async () => {
    openKey("my key");
    menuItem(tr("dataBrowsers.setTtlEllipsis")).click();
    const input = document.getElementById("g-name") as HTMLInputElement;
    input.value = "0";
    (document.getElementById("g-save") as HTMLButtonElement).click();
    await flush();
    expect(sent, "0 never leaves the panel").toHaveLength(0);
    expect(document.getElementById("g-err")!.textContent).toBe(tr("dataBrowsers.ttlZeroDeletes"));
    input.value = "60";
    (document.getElementById("g-save") as HTMLButtonElement).click();
    await flush(); await flush();
    expect(pipelined()[0]).toEqual([["EXPIRE", "my key", "60"]]);
  });

  // The TTL readout edits in place through its own little input - a second door to the same
  // act, and it had neither rule: it built "EXPIRE " + key as a console line, and 0 went.
  const editTtlInPlace = (typed: string): void => {
    const b = document.querySelector<HTMLElement>("#dbHead [data-rttl]")!;
    browsers.dbRedisClick(b);
    const input = document.querySelector<HTMLInputElement>("input.db-ttl-in")!;
    input.value = typed;
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  };

  it("the TTL readout's own editor refuses 0 too - it used to delete the key on the spot", async () => {
    openKey("my key");
    editTtlInPlace("0");
    await flush(); await flush();
    expect(pipelined(), "0 never leaves the panel").toEqual([]);
    expect(sent.some((x) => x.url.endsWith("/command")), "and nothing through the line splitter").toBe(false);
    expect(document.getElementById("toast")!.textContent).toBe(tr("dataBrowsers.ttlZeroDeletes"));
  });

  it("the TTL readout's own editor sends a spaced key as ONE argument", async () => {
    openKey("my key");
    editTtlInPlace("60");
    await flush(); await flush();
    expect(pipelined()[0]).toEqual([["EXPIRE", "my key", "60"]]);
    expect(sent.some((x) => x.url.endsWith("/command")), "nothing through the line splitter").toBe(false);
  });

  it("an EXPIRE that answers 0 says the key is gone - not that a TTL was set on it", async () => {
    reply = (url) => url.endsWith("/redis-pipeline") ? { ok: true, body: { replies: [0] } } : { ok: true, body: {} };
    openKey("gone:ttl");
    menuItem(tr("dataBrowsers.setTtlEllipsis")).click();
    (document.getElementById("g-name") as HTMLInputElement).value = "60";
    (document.getElementById("g-save") as HTMLButtonElement).click();
    await flush(); await flush();
    expect(pipelined()[0]).toEqual([["EXPIRE", "gone:ttl", "60"]]);
    expect(document.getElementById("toast")!.textContent).toBe(tr("dataBrowsers.keyAlreadyGone", { key: "gone:ttl" }));
  });

  it("Rename keeps a name's blanks: an unchanged ' x' is a cancel, and ' y ' goes exactly as typed", async () => {
    openKey(" x");
    menuItem(tr("dataBrowsers.renameEllipsis")).click();
    expect((document.getElementById("g-name") as HTMLInputElement).value).toBe(" x");
    (document.getElementById("g-save") as HTMLButtonElement).click();
    await flush(); await flush();
    expect(pipelined(), "a trim made this RENAMENX ' x' 'x' - a rename nobody asked for").toEqual([]);
    expect(document.getElementById("sheet")!.hidden, "unchanged is a cancel").toBe(true);
    openKey(" x");
    menuItem(tr("dataBrowsers.renameEllipsis")).click();
    (document.getElementById("g-name") as HTMLInputElement).value = " y ";
    (document.getElementById("g-save") as HTMLButtonElement).click();
    await flush(); await flush();
    expect(pipelined()[0]).toEqual([["RENAMENX", " x", " y "]]);
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

describe("the grid's look in views.css (SPEC §panel.pages, §panel.design)", () => {
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
/* The owner, 2026-09-28: "不要在这里显示刷新时间，而是应该在右上角，并且实时（获取一次时间就行了，页面上计算）显示这个 Key
   剩余时间是多少". A TTL printed where a fetch left it is the age of the fetch, not the life
   of the key: the sidebar row said "95s" for as long as the list stood, and so did the meta line
   and the status bar. One readout now - top-right in the head, beside the key's ⋯ - counting down
   in the page from the moment its value was read. */
describe("a key's TTL is one live countdown, top-right (2026-09-28)", () => {
  function openKey(ttl: number, readAt: number): void {
    Object.assign(dbConnState(), { conns: [dbConn("demo-cache", "redis")], conn: "demo-cache" });
    const t = freshTab("key") as DbKeyTab;
    t.redisKey = "test223";
    t.redisValue = { key: "test223", type: "string", ttl, value: "11" };
    t.redisValueAt = readAt;
    dbTabs().length = 0;
    dbTabs().push(t);
    grid.renderDbToolbar();
    grid.renderDbGrid();
  }

  it("the head ends with the countdown and the ⋯, and the countdown still edits in place", () => {
    openKey(91, Date.now());
    const ctl = Array.from(document.querySelectorAll<HTMLElement>("#dbHead .db-head-ctl > *"));
    const ttl = ctl[ctl.length - 2];
    expect(ttl.dataset.rttl, "the same data-rttl control it always was").toBe("");
    expect(ttl.textContent).toBe("1:31");
    expect(ctl[ctl.length - 1].getAttribute("aria-haspopup"), "the ⋯ stays last").toBe("menu");
  });

  it("a repaint counts from the read, not from the number the read saw", () => {
    openKey(91, Date.now() - 60_000);
    expect(document.querySelector("#dbHead [data-rttl]")!.textContent).toBe("31s");
  });

  it("the second hand is the page's own: no fetch, no repaint", () => {
    vi.useFakeTimers();
    try {
      openKey(91, Date.now());
      const before = document.querySelector("#dbHead [data-rttl]")!;
      vi.advanceTimersByTime(3000);
      const after = document.querySelector("#dbHead [data-rttl]")!;
      expect(after, "the same node, rewritten").toBe(before);
      expect(after.textContent).toBe("1:28");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a key with no expiry says so and starts no clock", () => {
    openKey(-1, Date.now());
    const b = document.querySelector("#dbHead [data-rttl]")!;
    expect(b.textContent).toBe(tr("dataBrowsers.noExpiry"));
    expect(b.getAttribute("data-ttlend"), "nothing to count down").toBeNull();
  });

  it("nowhere else prints a TTL that cannot move", () => {
    openKey(91, Date.now());
    expect(document.querySelector("#dbGridWrap .db-detail-meta")!.textContent).toBe("test223 · string");
    expect(document.getElementById("dbStatus")!.textContent).not.toContain("ttl");
    Object.assign(dbConnState(), {
      conns: [dbConn("demo-cache", "redis")], conn: "demo-cache",
      redis: { keys: [{ key: "test223", type: "string", ttl: 95 }], cursor: "0", done: true, total: 1 },
    });
    view.renderDbTables();
    const row = document.querySelector("#dbTables .db-key-row")!;
    expect(row.querySelector(".db-table-meta")!.textContent, "the type alone").toBe("string");
    expect((row as HTMLElement).title).toBe("test223 · string");
  });

  it("the label is seconds under a minute and a clock above it", () => {
    expect(browsers.dbTtlLabel(45)).toBe("45s");
    expect(browsers.dbTtlLabel(91)).toBe("1:31");
    expect(browsers.dbTtlLabel(7500)).toBe("2:05:00");
    expect(browsers.dbTtlLabel(0)).toBe(tr("dataBrowsers.ttlExpired"));
  });

  it("the edges of the label: the minute, the hour, the day, and past zero", () => {
    expect(browsers.dbTtlLabel(1)).toBe("1s");
    expect(browsers.dbTtlLabel(59), "the last second before the clock").toBe("59s");
    expect(browsers.dbTtlLabel(60), "the first minute reads as one").toBe("1:00");
    expect(browsers.dbTtlLabel(61)).toBe("1:01");
    expect(browsers.dbTtlLabel(599)).toBe("9:59");
    expect(browsers.dbTtlLabel(3599), "the last second before the hour").toBe("59:59");
    expect(browsers.dbTtlLabel(3600)).toBe("1:00:00");
    expect(browsers.dbTtlLabel(3661)).toBe("1:01:01");
    expect(browsers.dbTtlLabel(86400), "a day, which redis will happily hold").toBe("24:00:00");
    expect(browsers.dbTtlLabel(1000000)).toBe("277:46:40");
    // Zero and anything below it is one word - a key past its expiry is not "-3s".
    expect(browsers.dbTtlLabel(0)).toBe(tr("dataBrowsers.ttlExpired"));
    expect(browsers.dbTtlLabel(-1)).toBe(tr("dataBrowsers.ttlExpired"));
    expect(browsers.dbTtlLabel(-9999)).toBe(tr("dataBrowsers.ttlExpired"));
  });

  it("a countdown that runs out says so, and the ticker stops when nothing is counting", () => {
    vi.useFakeTimers();
    try {
      openKey(3, Date.now());
      expect(document.querySelector("#dbHead [data-rttl]")!.textContent).toBe("3s");
      vi.advanceTimersByTime(3000);
      expect(document.querySelector("#dbHead [data-rttl]")!.textContent).toBe(tr("dataBrowsers.ttlExpired"));
      vi.advanceTimersByTime(10000);
      expect(document.querySelector("#dbHead [data-rttl]")!.textContent, "it stays expired, never negative")
        .toBe(tr("dataBrowsers.ttlExpired"));
      // The key tab goes: with no countdown on screen the ticker has nothing to do, and the
      // next tick is its last. A timer per opened key for the rest of the session is a leak.
      const t = freshTab("sql") as DbSqlTab;
      dbTabs().length = 0;
      dbTabs().push(t);
      grid.renderDbToolbar();
      grid.renderDbGrid();
      vi.advanceTimersByTime(1000);
      expect(vi.getTimerCount(), "one tick to notice, then none").toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a key read long ago is already expired, and one read in the future is not negative", () => {
    // A tab resumed from a background page: the read is older than the TTL it carried.
    openKey(30, Date.now() - 120_000);
    expect(document.querySelector("#dbHead [data-rttl]")!.textContent).toBe(tr("dataBrowsers.ttlExpired"));
    // A clock that jumped backwards (a laptop waking, an NTP step) must not print a bigger
    // number than the key ever had: the readout is capped by what was read.
    openKey(30, Date.now() + 5_000);
    expect(document.querySelector("#dbHead [data-rttl]")!.textContent, "capped by what was read").toBe("30s");
  });
});

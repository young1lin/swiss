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

/* The console's tabs as the Data agent sees them (SPEC §data.agent Tabs). A console tab used to
   be reachable only by the name the agent gave it, which died with the tab's close; these pins
   hold the id that outlives it — listed, restored, and addressed statement by statement — and
   the snapshot every send carries, since the page is the only place the tabs exist. */

import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbActiveIndex, dbConn as dbConnState, dbTab, dbTabs, mountDbView, unmountDbView } from "../src/db-state.js";
import type { DbSqlTab } from "../src/types/state.js";
import { dbConn, dbPage } from "./db-fixtures.js";

const here = dirname(fileURLToPath(import.meta.url));

/** The shell ids the Data view's renderers read through $(), plus its own runtime ones. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  const own = ["dbTabStrip", "dbTables", "dbTablesPager", "dbHead", "dbMore", "dbFilters",
    "dbConsole", "dbSql", "dbSqlHl", "dbToolbar", "dbGridWrap", "dbBar", "dbNewTable"];
  return Array.from(new Set([...ids, ...own])).map((id) => '<div id="' + id + '"></div>').join("");
}

const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, { fetch: (): Promise<unknown> => Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(dbPage()) }) });
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});
vi.stubGlobal("confirm", () => true);

/* The shell before the import: modules in the Data view's graph wire controls at evaluation. */
document.body.innerHTML = shellSkeleton();
const tabs = await import("../src/data-tabs.js");
const sql = await import("../src/data-sql.js");
const closed = await import("../src/data-closed.js");
const grid = await import("../src/data-grid.js");
const view = await import("../src/data-view.js");

/** A new console in front holding this text — what the agent's stage/run leaves behind. */
function consoleTab(text: string, custom?: string): DbSqlTab {
  tabs.dbOpenTabForce({ kind: "sql" });
  const t = dbTab() as DbSqlTab;
  t.sqlText = text;
  if (custom) t.custom = custom;
  return t;
}

const live = (): DbSqlTab => dbTab() as DbSqlTab;

describe("formatting the SQL editor", () => {
  it("coalesces the browser's intermediate input events into one final repaint", () => {
    consoleTab("select id, name from users where id = 1;");
    const old = document.querySelector("#dbSql")!;
    const input = document.createElement("textarea");
    input.id = "dbSql";
    input.value = live().sqlText;
    old.replaceWith(input);
    input.addEventListener("input", view.dbPaneInput);
    const hl = document.querySelector<HTMLElement>("#dbSqlHl")!;
    const paint = vi.spyOn(hl, "textContent", "set");
    const original = Object.getOwnPropertyDescriptor(document, "execCommand");
    Object.defineProperty(document, "execCommand", { configurable: true, value: (_cmd: string, _ui: boolean, value: string): boolean => {
      for (const partial of ["select", "select id", value]) {
        input.value = partial;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }
      return true;
    } });
    try {
      grid.dbSqlFormatNow();
      expect(input.value).toBe("select id,\n       name\nfrom users\nwhere id = 1;");
      expect(live().sqlText).toBe(input.value);
      expect(hl.textContent).toBe(input.value + "\n");
      expect(paint).toHaveBeenCalledTimes(1);
    } finally {
      if (original) Object.defineProperty(document, "execCommand", original);
      else Reflect.deleteProperty(document, "execCommand");
    }
  });
});

beforeEach(() => {
  localStorage.clear();
  document.body.innerHTML = shellSkeleton();
  unmountDbView();
  mountDbView();
  Object.assign(dbConnState(), { conns: [dbConn("c", "mysql")], conn: "c" });
  vi.restoreAllMocks();
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

describe("the console's statements (SPEC §data.agent Tabs)", () => {
  it("one tab is one text, and a statement is a blank-line block: its index, its text, and the name its first line comments it with", () => {
    const text = "-- Revenue\nSELECT 1;\n\nSELECT 2\nFROM t\n\n# Cleanup\nDELETE FROM t\n";
    expect(sql.dbConsoleStatements(text)).toEqual([
      { index: 0, sql: "-- Revenue\nSELECT 1;", title: "Revenue" },
      { index: 1, sql: "SELECT 2\nFROM t" },
      { index: 2, sql: "# Cleanup\nDELETE FROM t", title: "Cleanup" },
    ]);
    // A console with no blank line at all is one statement; an empty one holds none.
    expect(sql.dbConsoleStatements("SELECT 1")).toEqual([{ index: 0, sql: "SELECT 1" }]);
    expect(sql.dbConsoleStatements("  \n\n ")).toEqual([]);
  });

  it("an edit rewrites exactly one block and leaves the rest byte for byte; a delete takes its blank lines too", () => {
    const text = "SELECT 1;\n\nSELECT 2;\n\nSELECT 3;";
    expect(sql.dbConsoleReplace(text, 1, "SELECT 20;")).toBe("SELECT 1;\n\nSELECT 20;\n\nSELECT 3;");
    expect(sql.dbConsoleDelete(text, 1)).toBe("SELECT 1;\n\nSELECT 3;");
    expect(sql.dbConsoleDelete(text, 0)).toBe("SELECT 2;\n\nSELECT 3;");
    expect(sql.dbConsoleDelete(text, 2)).toBe("SELECT 1;\n\nSELECT 2;");
    // Deleting the only statement leaves an empty tab, never a closed one.
    expect(sql.dbConsoleDelete("SELECT 1;", 0)).toBe("");
    // An index past the end is refused, not clamped onto another statement.
    expect(sql.dbConsoleReplace(text, 3, "x")).toBeNull();
    expect(sql.dbConsoleDelete(text, 3)).toBeNull();
  });
});

describe("a console tab's id (SPEC §data.agent Tabs)", () => {
  it("keeps a closed draft closed when removing it from storage fails", () => {
    const tab = consoleTab("SELECT 1", "Manual");
    tabs.dbCloseAllTabs();
    const before = dbTabs().length;
    const storage = vi.spyOn(Storage.prototype, "removeItem").mockImplementation(() => { throw new Error("storage unavailable"); });
    tabs.dbRestoreClosed(tab.id);
    expect(closed.dbClosedById("c", tab.id)?.text).toBe("SELECT 1");
    expect(dbTabs()).toHaveLength(before);
    expect(dbTabs().some(t => t.kind === "sql" && t.id === tab.id)).toBe(false);
    expect(closed.dbClosedClear("c")).toBe(false);
    expect(closed.dbClosedById("c", tab.id)?.text).toBe("SELECT 1");
    storage.mockRestore();
  });

  it("keeps identical consoles distinct and restores their original lifetime timestamps", () => {
    vi.spyOn(Date, "now").mockReturnValue(1000);
    const a = consoleTab("SELECT 1", "Same");
    vi.spyOn(Date, "now").mockReturnValue(2000);
    const b = consoleTab("SELECT 1", "Same");
    tabs.dbCloseAllTabs();
    expect(closed.dbClosedFor("c").map(e => e.id).sort()).toEqual([a.id, b.id].sort());
    vi.spyOn(Date, "now").mockReturnValue(3000);
    tabs.dbRestoreClosed(a.id);
    expect(live().id).toBe(a.id);
    expect(live().createdAt).toBe(1000);
    expect(live().updatedAt).toBe(1000);
    sql.dbConsoleEdit(a.id, "replace", 0, "SELECT 2");
    expect(live().updatedAt).toBe(3000);
    tabs.dbCloseTab(dbActiveIndex());
    expect(closed.dbClosedById("c", a.id)?.createdAt).toBe(1000);
    expect(closed.dbClosedById("c", a.id)?.updatedAt).toBe(3000);
  });

  it("does not place an id-addressed statement into a different console when the id is missing", () => {
    const a = consoleTab("SELECT 1", "Same");
    sql.dbAgentStatement("SELECT 2", null, new Set(), undefined, "Same", "missing");
    expect(dbTabs()).toHaveLength(1);
    expect(a.sqlText).toBe("SELECT 1");
  });
  it("is minted at birth, survives the close into Recently closed, and the snapshot lists open and closed tabs alike", () => {
    const first = consoleTab("SELECT 1;\n\nSELECT 2;", "Report");
    const id = first.id;
    expect(id).toBeTruthy();
    expect(sql.dbConsoleTabs()).toEqual([
      { id, tab: "Report", open: true, agent: false, statements: [{ sql: "SELECT 1;" }, { sql: "SELECT 2;" }] },
    ]);
    tabs.dbCloseTab(dbActiveIndex());
    expect(dbTabs().filter(tabs.dbTabVisible)).toEqual([]);
    // Closed, still listed - under the same id, with the statements it held.
    expect(sql.dbConsoleTabs()).toEqual([
      { id, tab: "Report", open: false, agent: false, statements: [{ sql: "SELECT 1;" }, { sql: "SELECT 2;" }] },
    ]);
    expect(closed.dbClosedFor("c")[0].id).toBe(id);
  });

  it("refuses to close without evicting older consoles at the count and size limits", () => {
    for (const entries of [
      Array.from({ length: 200 }, (_, i) => ({ id: "old-" + i, conn: "c", text: "SELECT " + i, at: i + 1 })),
      [{ id: "large", conn: "c", text: "x".repeat(999900), at: 1 }],
    ]) {
      localStorage.setItem("swiss.dbClosedTabs", JSON.stringify(entries));
      const before = localStorage.getItem("swiss.dbClosedTabs");
      const t = consoleTab("SELECT 1", "New");
      tabs.dbCloseTab(dbActiveIndex());
      expect(dbTabs()).toContain(t);
      expect(localStorage.getItem("swiss.dbClosedTabs")).toBe(before);
    }
  });

  it("never exposes an analysis tab as a console or files it as closed SQL", () => {
    const t = consoleTab("SELECT 1", "Analysis");
    t.analysisId = "analysis-1";
    expect(sql.dbConsoleTabs()).toEqual([]);
    expect(sql.dbConsoleEdit(t.id, "replace", 0, "SELECT 2")).toBe(false);
    expect(sql.dbAgentOpenConsole({ tabId: t.id })).toBe(false);
    expect(t.sqlText).toBe("SELECT 1");
    tabs.dbCloseTab(dbActiveIndex());
    expect(closed.dbClosedById("c", t.id)).toBeNull();
  });

  it("keeps oversized console text out of the bounded agent snapshot without pretending it is empty", () => {
    const t = consoleTab("x".repeat(20001), "Long");
    expect(sql.dbConsoleTabs()[0]).toMatchObject({ id: t.id, statements: [], statementsOmitted: true });
    expect(t.sqlText).toHaveLength(20001);
  });

  it("keeps a whole page of large consoles under the request budget, marking exactly what it left out", () => {
    const statement = "SELECT '" + "x".repeat(19_900) + "';";
    for (let i = 0; i < 46; i++) consoleTab(statement + "\n\n" + statement, "T" + i);
    const rows = sql.dbConsoleTabs();
    const bytes = new TextEncoder().encode(JSON.stringify(rows)).length;
    expect(bytes, "the gateway refuses a body over 2 MiB").toBeLessThan(1_900_000);
    expect(rows.some((row) => row.statementsOmitted), "the tail is omitted, not silently truncated").toBe(true);
    for (const row of rows) {
      if (row.statementsOmitted) expect(row.statements).toEqual([]);
      else expect(row.statements.length).toBe(2);
    }
  }, 20_000);

  it("restores a blank console by its stable id", () => {
    const t = consoleTab("", "");
    const id = t.id;
    tabs.dbCloseTab(dbActiveIndex());
    expect(closed.dbClosedById("c", id)?.id).toBe(id);
    expect(sql.dbAgentOpenConsole({ tabId: id, format: false })).toBe(true);
    expect(live().id).toBe(id);
  });

  it("sends the canonical agent slot for closed consoles", () => {
    const t = consoleTab("SELECT 1", "Agent");
    t.agent = true;
    tabs.dbCloseTab(dbActiveIndex());
    expect(sql.dbConsoleTabs()[0]).toMatchObject({ id: t.id, open: false, agent: true, agentTab: "" });
  });

  it("open_console's tabId brings a closed tab back under its own id, text and name - and an unknown id changes nothing", () => {
    const t = consoleTab("SELECT 1;\n\nSELECT 2;", "Report");
    const id = t.id;
    tabs.dbCloseTab(dbActiveIndex());
    sql.dbAgentConsoleId(id, "Report");
    const back = live();
    expect([back.kind, back.id, back.sqlText, back.custom, back.agent, back.agentTab])
      .toEqual(["sql", id, "SELECT 1;\n\nSELECT 2;", "Report", true, "Report"]);
    expect(document.querySelector<HTMLTextAreaElement>("#dbSql")!.value).toBe("SELECT 1;\n\nSELECT 2;");
    // An id the page does not have is ignored, with the reason on the console.
    const before = dbTabs().length;
    sql.dbAgentConsoleId("no-such-tab", "Report");
    expect(dbTabs().length).toBe(before);
    expect(console.warn).toHaveBeenCalled();
  });

  it("edit and delete address one statement of an open tab and repaint the console in front", () => {
    const t = consoleTab("SELECT 1;\n\nSELECT 2;", "Report");
    expect(sql.dbConsoleEdit(t.id, "replace", 0, "SELECT 10;", "SELECT 1;")).toBe(true);
    expect(live().sqlText).toBe("SELECT 10;\n\nSELECT 2;");
    expect(document.querySelector<HTMLTextAreaElement>("#dbSql")!.value).toBe("SELECT 10;\n\nSELECT 2;");
    expect(sql.dbConsoleEdit(t.id, "delete", 1, "", "SELECT 2;")).toBe(true);
    expect(live().sqlText).toBe("SELECT 10;");
    // Past the end: refused, nothing rewritten.
    expect(sql.dbConsoleEdit(t.id, "delete", 4)).toBe(false);
    expect(live().sqlText).toBe("SELECT 10;");
    expect(sql.dbConsoleEdit("no-such-tab", "delete", 0)).toBe(false);
  });

  it("a frame delivered twice cannot hit the wrong statement: the same replace is a no-op, a repeated delete is refused", () => {
    const t = consoleTab("SELECT 1;\n\nSELECT 2;", "Report");
    const frame = { tabId: t.id, index: 0, sql: "SELECT 10;", expect: "SELECT 1;" };
    sql.dbAgentConsoleEdit({ kind: "consoleEdit", op: "replace", ...frame });
    expect(live().sqlText).toBe("SELECT 10;\n\nSELECT 2;");
    sql.dbAgentConsoleEdit({ kind: "consoleEdit", op: "replace", ...frame });
    expect(live().sqlText, "the repeat finds its own text and writes nothing").toBe("SELECT 10;\n\nSELECT 2;");
    sql.dbAgentConsoleEdit({ kind: "consoleEdit", op: "delete", tabId: t.id, index: 0, expect: "SELECT 10;" });
    expect(live().sqlText).toBe("SELECT 2;");
    // The same delete again: index 0 now holds another statement, so it is ignored rather than dropped.
    sql.dbAgentConsoleEdit({ kind: "consoleEdit", op: "delete", tabId: t.id, index: 0, expect: "SELECT 10;" });
    expect(live().sqlText, "the statement that moved into index 0 is not deleted by a repeat").toBe("SELECT 2;");
    expect(console.warn).toHaveBeenCalled();
  });

  it("edit and delete reach a tab that is closed: its filed text is rewritten, and it stays closed", () => {
    const t = consoleTab("SELECT 1;\n\nSELECT 2;", "Report");
    const id = t.id;
    tabs.dbCloseTab(dbActiveIndex());
    expect(sql.dbConsoleEdit(id, "replace", 1, "SELECT 20;")).toBe(true);
    expect(sql.dbConsoleEdit(id, "delete", 0)).toBe(true);
    expect(sql.dbConsoleTabs()).toEqual([{ id, tab: "Report", open: false, agent: false, statements: [{ sql: "SELECT 20;" }] }]);
    expect(dbTabs().filter(tabs.dbTabVisible), "an edit never opens a closed tab").toEqual([]);
  });

  it("an entry filed before ids existed gets one on the next read, and keeps it", () => {
    localStorage.setItem("swiss.dbClosedTabs", JSON.stringify([{ conn: "c", text: "SELECT 9;", custom: "Old", at: 1 }]));
    const [row] = sql.dbConsoleTabs();
    expect(row.tab).toBe("Old");
    expect(row.id).toBeTruthy();
    const stored: { id?: string }[] = JSON.parse(localStorage.getItem("swiss.dbClosedTabs") || "[]");
    expect(stored[0].id, "written back, so the id a tool read once still addresses it").toBe(row.id);
    // A page reload reads the same id back from storage.
    expect(sql.dbConsoleTabs()[0].id).toBe(row.id);
  });
});

describe("a statement's way to its tab (SPEC §data.agent Tabs)", () => {
  it("two consoles sharing a name are told apart by id: the statement lands in the one the agent opened", () => {
    const first = consoleTab("SELECT 1;", "SQL");
    const second = consoleTab("SELECT 2;", "SQL");
    first.agent = true;
    first.agentTab = "SQL";
    second.agent = true;
    second.agentTab = "SQL";
    sql.dbAgentConsoleId(second.id, "SQL");
    sql.dbAgentStatement("SELECT 3;", null, new Set(), undefined, "SQL", second.id);
    expect(second.sqlText).toBe("SELECT 2;\n\nSELECT 3;");
    expect(first.sqlText, "the console of the same name that was not opened is untouched").toBe("SELECT 1;");
    expect(dbTabs().filter(tabs.dbTabVisible).length, "no third console was opened for the statement").toBe(2);
  });

  it("preserves a renamed default Agent slot when opened by id", () => {
    const t = consoleTab("SELECT 1", "Renamed");
    t.agent = true;
    sql.dbAgentConsoleId(t.id, "Renamed");
    expect(sql.dbConsoleTabs()[0].agentTab).toBe("");
    sql.dbAgentStatement("SELECT 2", null, new Set());
    expect(dbTabs().filter(tabs.dbTabVisible)).toHaveLength(1);
    expect(t.sqlText).toContain("SELECT 2");
  });

  it("a tab the reader renamed answers to its id, and to the title the rename left it", () => {
    const t = consoleTab("SELECT 1;", "Original");
    t.agent = true;
    t.agentTab = "Original";
    const id = t.id;
    // The reader renames the card: that is the tab's title, not the agent's own name for it.
    t.custom = "Renamed";
    sql.dbAgentConsoleId(id, "Renamed");
    sql.dbAgentStatement("SELECT 2;", null, new Set(), undefined, "Renamed", id);
    expect(t.sqlText, "the statement reached the renamed tab, not a new one of that name").toBe("SELECT 1;\n\nSELECT 2;");
    expect(t.agentTab).toBe("Original");
    expect(dbTabs().filter(tabs.dbTabVisible).length).toBe(1);
    // A call naming only the card's new title still finds it: the title is the name it has now.
    sql.dbAgentStatement("SELECT 3;", null, new Set(), undefined, "Renamed");
    expect(t.sqlText).toBe("SELECT 1;\n\nSELECT 2;\n\nSELECT 3;");
    expect(dbTabs().filter(tabs.dbTabVisible).length).toBe(1);
  });

  it("a console brought back by name keeps the id it was filed under, so an edit by that id still lands", () => {
    const t = consoleTab("SELECT 1;\n\nSELECT 2;", "Report");
    t.agent = true;
    t.agentTab = "Report";
    const id = t.id;
    tabs.dbCloseTab(dbActiveIndex());
    sql.dbAgentConsole("Report");
    const back = live();
    expect([back.id, back.sqlText, back.agentTab]).toEqual([id, "SELECT 1;\n\nSELECT 2;", "Report"]);
    // The server still holds the tab under the id it filed: an edit addressed by it must reach
    // this console, not be ignored as one the page no longer has.
    expect(sql.dbConsoleEdit(id, "replace", 0, "SELECT 10;", "SELECT 1;")).toBe(true);
    expect(live().sqlText).toBe("SELECT 10;\n\nSELECT 2;");
  });
});


it("formats an opened SQL console by default, preserves raw text only for false, and never executes", () => {
  const raw = "select id,name from users where id=7";
  expect(sql.dbAgentOpenConsole({ create: true, tabId: "generated-1", tab: "Report", sql: raw })).toBe(true);
  expect(live().id).toBe("generated-1");
  expect(live().sqlText).toContain("\nfrom users");
  expect(live().sqlResult).toBeNull();
  expect(sql.dbAgentOpenConsole({ create: true, tabId: "generated-2", tab: "Report", sql: raw, format: false })).toBe(true);
  expect(live().sqlText).toBe(raw);
  const before = live().sqlText;
  expect(sql.dbAgentOpenConsole({ tabId: "generated-2", format: "false" })).toBe(false);
  expect(live().sqlText).toBe(before);
  expect(sql.dbAgentOpenConsole({ tabId: "generated-1" })).toBe(true);
  expect(live().id).toBe("generated-1");
  expect(sql.dbConsoleEdit("generated-1", "replace", 0, "select 9", raw)).toBe(true);
  expect(live().sqlText).toBe("select 9");
});

it("keeps a manual console's exact bytes when open_console only brings it to the front by id", () => {
  const original = "select  1\n\n  -- keep  spacing\nselect 2 ;";
  const t = consoleTab(original, "Manual");
  const id = t.id;
  tabs.dbCloseTab(dbActiveIndex());
  expect(sql.dbAgentOpenConsole({ tabId: id })).toBe(true);
  expect(live().sqlText).toBe(original);
});

it("keeps every byte of a manual draft when an Agent statement is appended by ID", () => {
  const original = "  SELECT 1;\n\n-- trailing note\n \t\n";
  const tab = consoleTab(original, "Manual");
  sql.dbAgentStatement("SELECT 2;", null, new Set(), undefined, undefined, tab.id, false);
  expect(tab.sqlText).toBe(original + "\n\nSELECT 2;");
});

it("formats Agent staged, executed and replaced SQL by default without rewriting earlier drafts", () => {
  const raw = "select id,name from orders where id=1";
  const first = consoleTab("-- keep my draft\nselect 0;", "Report");
  sql.dbAgentStatement(raw, null, new Set(), undefined, "Report", first.id);
  const formatted = sql.dbFormatSql(raw + ";", { dialect: "mysql" });
  expect(first.sqlText).toBe("-- keep my draft\nselect 0;\n\n" + formatted);
  const second = consoleTab("");
  sql.dbAgentStatement(raw, { error: "read failed" }, new Set(), undefined, undefined, second.id);
  expect(second.sqlText).toBe(formatted);
  sql.dbAgentConsoleEdit({ tabId: second.id, op: "replace", index: 0, expect: raw + ";", sql: "select id from orders where id=2" });
  expect(second.sqlText).toBe(sql.dbFormatSql("select id from orders where id=2", { dialect: "mysql" }));
});

it("formats closed PostgreSQL replacements and honors explicit raw staging and edits", () => {
  dbConnState().conns[0].dialect = "pg";
  const raw = "select id,name from orders where id=1";
  const first = consoleTab(raw + ";");
  tabs.dbCloseTab(dbActiveIndex());
  sql.dbAgentConsoleEdit({ tabId: first.id, op: "replace", index: 0, expect: raw + ";", sql: "select name from orders where id=2" });
  expect(closed.dbClosedById("c", first.id)?.text).toBe(sql.dbFormatSql("select name from orders where id=2", { dialect: "pg" }));
  const second = consoleTab("");
  sql.dbAgentStatement(raw, null, new Set(), undefined, undefined, second.id, false);
  expect(second.sqlText).toBe(raw + ";");
  const next = "select id,name from orders where id=3";
  sql.dbAgentConsoleEdit({ tabId: second.id, op: "replace", index: 0, expect: raw + ";", sql: next, format: false });
  expect(second.sqlText).toBe(next);
});

it("refuses a close when storage cannot save it and preserves both identity and text", () => {
  const t = consoleTab("SELECT 'keep this work';", "Unsaved");
  const original = localStorage;
  vi.stubGlobal("localStorage", new Proxy(original, { get(target, key) { return key === "setItem" ? (): never => { throw new DOMException("full", "QuotaExceededError"); } : Reflect.get(target, key); } }));
  tabs.dbCloseTab(dbActiveIndex());
  expect(dbTabs()).toContain(t);
  expect(live().id).toBe(t.id);
  expect(live().sqlText).toBe("SELECT 'keep this work';");
  vi.stubGlobal("localStorage", original);
});


it("closed edits and removal stay scoped when connections carry the same supplied id", () => {
  const first = consoleTab("SELECT 1", "One"); first.id = "shared-id";
  expect(closed.dbClosedRemember([{ conn: "other", tab: first }, { conn: "c", tab: { ...first, sqlText: "SELECT 2" } }])).toBe(true);
  expect(closed.dbClosedSetText(first.id, "SELECT 3", "c")).toBe(true);
  expect(closed.dbClosedById("other", first.id)?.text).toBe("SELECT 1");
  expect(closed.dbClosedTake(first.id, "c")?.text).toBe("SELECT 3");
  expect(closed.dbClosedById("other", first.id)?.text).toBe("SELECT 1");
});

it("failed legacy migration keeps duplicate legacy entries distinct and their ids stable for the session", () => {
  localStorage.setItem("swiss.dbClosedTabs", JSON.stringify([{ conn: "c", text: "SELECT 1", at: 1 }, { conn: "c", text: "SELECT 1", at: 1 }]));
  const original = localStorage;
  vi.stubGlobal("localStorage", new Proxy(original, { get(target, key) { return key === "setItem" ? (): never => { throw new DOMException("full", "QuotaExceededError"); } : Reflect.get(target, key); } }));
  const before = closed.dbClosedFor("c").map(e => e.id);
  expect(new Set(before).size).toBe(2);
  expect(closed.dbClosedFor("c").map(e => e.id)).toEqual(before);
  vi.stubGlobal("localStorage", original);
});

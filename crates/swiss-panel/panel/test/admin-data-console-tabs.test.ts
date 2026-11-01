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

/** A new console in front holding this text — what the agent's stage/run leaves behind. */
function consoleTab(text: string, custom?: string): DbSqlTab {
  tabs.dbOpenTabForce({ kind: "sql" });
  const t = dbTab() as DbSqlTab;
  t.sqlText = text;
  if (custom) t.custom = custom;
  return t;
}

const live = (): DbSqlTab => dbTab() as DbSqlTab;

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

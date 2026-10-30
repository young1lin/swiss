// @vitest-environment happy-dom
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

/* SPEC §data.console, §data.redis-console: what one run covers. The owner's ask was "run several
   statements at once": a run covered only the caret's blank-line block, and a redis console sent
   its whole box as ONE command, so "SET a 1" over "GET a" reached redis as a SET with two stray
   words. Now a selection runs exactly itself, Run all (Ctrl+Shift+Enter, Alt+X) the whole
   console, every statement - on redis every line - its own request and result tab, and the first
   failure stops the batch in a tab that says where. These drive the real view against a real
   DOM with a routed fetch. */
import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbTab, unmountDbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";
import type { DbSqlTab } from "../src/types/state.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* The routed server: every /query and /command body in the order it came, and a statement
   holding "boom" refused the way the server refuses one. */
const sent: string[] = [];
function answer(url: string, init?: { body?: unknown }): { ok: boolean; status: number; body: unknown } {
  const p = new URL(url, "http://x").pathname;
  const body = JSON.parse(String(init?.body || "{}")) as { sql?: string; command?: string };
  if (p === "/api/db") return { ok: true, status: 200, body: { connections: [dbConn("shop", "mysql"), dbConn("cache", "redis")] } };
  if (p.endsWith("/tables")) return { ok: true, status: 200, body: { tables: [], total: 0, page: 0, limit: 5000, more: false } };
  if (p.endsWith("/databases")) return { ok: true, status: 200, body: { primary: null, current: null, databases: [] } };
  if (p.endsWith("/keys")) return { ok: true, status: 200, body: { keys: [], cursor: "0", done: true, total: 0 } };
  if (p.endsWith("/query") || p.endsWith("/command")) {
    const text = body.sql ?? body.command ?? "";
    sent.push(text);
    if (text.includes("boom")) return { ok: false, status: 400, body: { error: "syntax error near boom" } };
    if (p.endsWith("/command")) return { ok: true, status: 200, body: { reply: "OK", elapsedMs: 1 } };
    return { ok: true, status: 200, body: { columns: ["n"], rows: [{ n: 1 }], rowCount: 1, elapsedMs: 1 } };
  }
  return { ok: true, status: 200, body: {} };
}
const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, {
  fetch: (url: unknown, init?: { body?: unknown }) => {
    const a = answer(String(url), init);
    return Promise.resolve({ ok: a.ok, status: a.status, json: () => Promise.resolve(a.body) });
  },
});
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});

Object.assign(window, { confirm: () => true });
document.body.innerHTML = shellSkeleton();
const page = await import("../src/views/data.js");
const view = await import("../src/data-view.js");
const tabs = await import("../src/data-tabs.js");
const sql = await import("../src/data-sql.js");

const flush = async (): Promise<void> => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); };

/** Open the console on `conn` with `text` in it, the caret (or a selection) where asked. */
async function console_(conn: string, text: string, sel?: [number, number]): Promise<DbSqlTab> {
  await view.dbSwitchConn(conn);
  await flush();
  tabs.dbOpenTab({ kind: "sql" });
  await flush();
  const st = dbTab();
  if (st.kind !== "sql") throw new Error("not a console tab");
  st.sqlText = text;
  const ta = document.getElementById("dbSql") as HTMLTextAreaElement;
  ta.value = text;
  ta.setSelectionRange(sel ? sel[0] : text.length, sel ? sel[1] : text.length);
  sent.length = 0;
  return st;
}
const labels = (st: DbSqlTab): string[] => (st.sqlResults || []).map((r) => r.tabLabel ?? "");

beforeEach(async () => {
  page.unmount();
  unmountDbView();
  sent.length = 0;
  try { localStorage.clear(); } catch (e) { /* none here */ }
  vi.restoreAllMocks();
  await page.mount();
  await flush();
});

describe("the text a run covers (SPEC §data.console)", () => {
  it("pure: the selection, else the caret's block; all is the whole text", () => {
    const t = "SELECT 1\n\nSELECT 2;\nSELECT 3\n\nSELECT 4";
    expect(sql.dbRunText(t, 12, 12, false)).toBe("SELECT 2;\nSELECT 3");
    expect(sql.dbRunText(t, 10, 19, false)).toBe("SELECT 2;");
    expect(sql.dbRunText(t, 12, 12, true)).toBe(t);
    expect(sql.dbRunText(t, null, null, false), "no caret: the last block").toBe("SELECT 4");
    expect(sql.dbConsoleBlocks(t)).toEqual(["SELECT 1", "SELECT 2;\nSELECT 3", "SELECT 4"]);
    expect(sql.dbConsoleBlocks("\n \n\t\n")).toEqual([]);
  });

  it("pure: a redis script is one command a line, a quoted newline staying in its argument", () => {
    expect(sql.dbSplitCommands("SET a 1\nGET a\n\n  \nDEL a")).toEqual(["SET a 1", "GET a", "DEL a"]);
    expect(sql.dbSplitCommands("SET k \"x\ny\" EX 10\nGET k")).toEqual(["SET k \"x\ny\" EX 10", "GET k"]);
    expect(sql.dbSplitCommands("SET k 'a\nb'\nSET j \"q\\\"\nr\"")).toEqual(["SET k 'a\nb'", "SET j \"q\\\"\nr\""]);
    expect(sql.dbCommandTabLabel("get user:1")).toBe("GET user:1");
    expect(sql.dbCommandTabLabel("PING")).toBe("PING");
    expect(sql.dbCommandTabLabel("echo \"hello swiss\" x"), "a quoted argument is one").toBe("ECHO \"hello swiss\"");
    expect(sql.dbCommandTabLabel("SET 'a b' 1")).toBe("SET 'a b'");
    expect(sql.dbCommandTabLabel("SET " + "k".repeat(40) + " v")).toHaveLength(28);
  });

  it("Run all sends every statement of every block, one request and one tab each", async () => {
    const st = await console_("shop", "SELECT 1; SELECT 2\n\nSELECT 3\n\nUPDATE t SET a = 1", [0, 0]);
    (document.getElementById("dbSqlRunAll") as HTMLElement).click();
    await flush();
    expect(sent).toEqual(["SELECT 1", "SELECT 2", "SELECT 3", "UPDATE t SET a = 1"]);
    expect(labels(st)).toEqual(["SELECT · 1", "SELECT · 1", "SELECT · 1", "UPDATE t · 1"]);
    expect(st.resultTab).toBe(0);
  });

  it("Run and Ctrl+Enter take the selection when there is one, else the caret's block; Ctrl+Shift+Enter and Alt+X take everything", async () => {
    const text = "SELECT 1\n\nSELECT 2; SELECT 3";
    await console_("shop", text, [10, 18]);
    (document.getElementById("dbSqlRun") as HTMLElement).click();
    await flush();
    expect(sent, "the selection, exactly").toEqual(["SELECT 2"]);
    const ta = document.getElementById("dbSql") as HTMLTextAreaElement;
    const key = (o: KeyboardEventInit): void => { ta.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...o })); };
    sent.length = 0;
    ta.setSelectionRange(3, 3);
    key({ key: "Enter", ctrlKey: true });
    await flush();
    expect(sent, "the caret's block").toEqual(["SELECT 1"]);
    sent.length = 0;
    key({ key: "Enter", ctrlKey: true, shiftKey: true });
    await flush();
    expect(sent).toEqual(["SELECT 1", "SELECT 2", "SELECT 3"]);
    sent.length = 0;
    key({ key: "x", altKey: true });
    await flush();
    expect(sent).toEqual(["SELECT 1", "SELECT 2", "SELECT 3"]);
  });

  it("the first failure stops the batch in a tab of its own, in front, saying how many did not run", async () => {
    const st = await console_("shop", "SELECT 1; SELECT boom; SELECT 3; SELECT 4");
    await sql.dbRunSql(false, true);
    await flush();
    expect(sent).toEqual(["SELECT 1", "SELECT boom"]);
    expect(labels(st)).toEqual(["SELECT · 1", "SELECT · failed"]);
    expect(st.resultTab).toBe(1);
    const failed = st.sqlResults?.[1];
    expect(failed?.rows).toEqual([{ error: "syntax error near boom", statement: "SELECT boom" }]);
    expect(failed?.note).toBe("2 after it not run");
  });

  it("a redis console sends each line as its own command, a tab per command; one command keeps the single reply", async () => {
    const st = await console_("cache", "SET a 1\nGET a\n\nSET b \"x\ny\"");
    await sql.dbRunSql(false, true);
    await flush();
    expect(sent).toEqual(["SET a 1", "GET a", "SET b \"x\ny\""]);
    expect(labels(st)).toEqual(["SET a", "GET a", "SET b"]);
    sent.length = 0;
    const ta = document.getElementById("dbSql") as HTMLTextAreaElement;
    ta.setSelectionRange(0, 7);
    await sql.dbRunSql();
    await flush();
    expect(sent).toEqual(["SET a 1"]);
    expect(st.sqlResults).toHaveLength(1);
    expect(st.sqlResult?.columns).toEqual(["reply"]);
    expect(st.sqlResult?.tabLabel).toBeUndefined();
  });

  /* The owner's ask after the first cut: results that fold down to the bottom, and an editor that
     grows and folds. The results' bar - their tabs, named by table, and their facts - is the dock's
     handle under the console; it stays when they fold, and a run or a tab brings them back. */
  it("the results fold down to their bar under the console; a tab or a run brings them back", async () => {
    const st = await console_("shop", "SELECT * FROM orders; SELECT 2");
    await sql.dbRunSql(false, true);
    await flush();
    expect(labels(st)).toEqual(["orders · 1", "SELECT · 1"]);
    const bar = document.getElementById("dbRbar") as HTMLElement;
    expect(Array.from(bar.querySelectorAll<HTMLElement>("[data-rtab]")).map((b) => b.title)).toEqual(["SELECT * FROM orders", "SELECT 2"]);
    const dockEl = (): HTMLElement => document.querySelector<HTMLElement>(".db-main > .dock")!;
    expect(Array.from(dockEl().children).map((c) => c.id || c.className)).toEqual(["dbConsole", "dock-head", "dbGridWrap"]);
    expect(bar.parentElement!.className, "the bar is the handle under the console").toBe("dock-head");
    const hide = (): void => { dockEl().querySelector<HTMLElement>(".dock-tools > button[aria-label='Hide Results']")!.click(); };
    hide();
    expect(dockEl().dataset.fold).toBe("bottom");
    expect(bar.hidden, "the bar stays").toBe(false);
    (bar.querySelector("[data-rtab='1']") as HTMLElement).click();
    expect(dockEl().dataset.fold).toBeUndefined();
    expect(st.resultTab).toBe(1);
    hide();
    expect(dockEl().dataset.fold).toBe("bottom");
    await sql.dbRunSql(false, true);
    await flush();
    expect(dockEl().dataset.fold, "a run answers into results in sight").toBeUndefined();
  });

  /* The owner's ask: "with several results the strip cannot show them all" - the tabs wrapped onto
     two lines and the last hid under the facts. Each stays on one line in a strip that scrolls
     sideways (the wheel too), and the list beside it names every result. */
  it("a long script's strip scrolls sideways, the wheel too, and the list beside it shows any result", async () => {
    const st = await console_("shop", "SELECT * FROM orders; SELECT 2; SELECT 3");
    await sql.dbRunSql(false, true);
    await flush();
    expect(labels(st)).toEqual(["orders · 1", "SELECT · 1", "SELECT · 1"]);
    const bar = document.getElementById("dbRbar") as HTMLElement;
    const strip = bar.querySelector<HTMLElement>(".db-rtabs")!;
    expect(strip.nextElementSibling?.matches("button[data-rlist]"), "the list sits after the strip").toBe(true);
    const css = readFileSync(join(here, "../../src/admin_assets/styles/ui.css"), "utf8");
    expect(css, "a pill's label stays on one line").toMatch(/\.seg button \{[^}]*white-space: nowrap;/);
    // A strip wider than its room turns the wheel sideways; one that fits leaves the wheel to the page.
    Object.defineProperty(strip, "scrollWidth", { configurable: true, value: 900 });
    Object.defineProperty(strip, "clientWidth", { configurable: true, value: 300 });
    const wheel = new WheelEvent("wheel", { deltaY: 120, bubbles: true, cancelable: true });
    strip.dispatchEvent(wheel);
    expect(wheel.defaultPrevented).toBe(true);
    expect(strip.scrollLeft).toBe(120);
    Object.defineProperty(strip, "scrollWidth", { configurable: true, value: 300 });
    const fits = new WheelEvent("wheel", { deltaY: 120, bubbles: true, cancelable: true });
    strip.dispatchEvent(fits);
    expect(fits.defaultPrevented).toBe(false);
    // The list: every result by its tab's name, the one showing checked; a pick shows it.
    bar.querySelector<HTMLButtonElement>("[data-rlist]")!.click();
    const rows = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button"));
    expect(rows.map((b) => [b.textContent, b.title, b.classList.contains("on")])).toEqual([
      ["orders · 1", "SELECT * FROM orders", true], ["SELECT · 1", "SELECT 2", false], ["SELECT · 1", "SELECT 3", false],
    ]);
    // The find box matches a statement too, one hover away on its row.
    const find = document.querySelector<HTMLInputElement>("#menu input.menu-filter")!;
    expect(find.placeholder).toBe("Find a result");
    find.value = "select 3";
    find.dispatchEvent(new Event("input"));
    expect(rows.filter((b) => !b.hidden).map((b) => b.title)).toEqual(["SELECT 3"]);
    rows[2].click();
    expect(st.resultTab).toBe(2);
    expect(st.sqlResult).toBe(st.sqlResults?.[2]);
    expect(document.querySelector("#dbRbar [data-rtab='2']")?.getAttribute("aria-selected")).toBe("true");
  });
});

/* The owner's ask: "SQL formatting should have a shortcut, and look like IDEA's Data Grip". */
describe("Format's keys (SPEC §data.console)", () => {
  const ta = (): HTMLTextAreaElement => document.getElementById("dbSql") as HTMLTextAreaElement;
  // happy-dom reads AltGraph as any Alt; a browser says it only for the AltGr key itself.
  const key = (o: KeyboardEventInit, altGraph = false): void => {
    const ev = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...o });
    const own = ev.getModifierState.bind(ev);
    Object.defineProperty(ev, "getModifierState", { value: (k: string): boolean => (k === "AltGraph" ? altGraph : own(k)) });
    ta().dispatchEvent(ev);
  };

  it("Ctrl+Alt+L formats the whole console, the caret staying on its word", async () => {
    const st = await console_("shop", "select a, b from t where x = 1", [10, 10]);
    key({ key: "l", code: "KeyL", ctrlKey: true, altKey: true });
    expect(ta().value).toBe("select a,\n       b\nfrom t\nwhere x = 1");
    expect(st.sqlText).toBe(ta().value);
    expect(ta().value.slice(ta().selectionStart, ta().selectionStart + 6), "the caret stood before b").toBe("b\nfrom");
    expect(sent, "formatting runs nothing").toEqual([]);
  });

  it("Shift+Alt+F does the same; a selection formats alone at its column, and stays selected", async () => {
    const text = "-- keep   this\nselect 1;\n\n    select a, b from t";
    await console_("shop", text, [text.indexOf("    select") + 4, text.length]);
    key({ key: "F", code: "KeyF", shiftKey: true, altKey: true });
    expect(ta().value).toBe("-- keep   this\nselect 1;\n\n    select a,\n           b\n    from t");
    expect(ta().value.slice(ta().selectionStart, ta().selectionEnd)).toBe("select a,\n           b\n    from t");
  });

  it("a redis console has nothing to format; Ctrl+L and AltGr+L stay the browser's", async () => {
    await console_("cache", "set   a 1");
    key({ key: "l", code: "KeyL", ctrlKey: true, altKey: true });
    expect(ta().value).toBe("set   a 1");
    await console_("shop", "select a, b from t");
    key({ key: "l", code: "KeyL", ctrlKey: true });
    key({ key: "ł", code: "KeyL", ctrlKey: true, altKey: true }, true);
    expect(ta().value).toBe("select a, b from t");
  });

  it("pure: the caret keeps its place among the words", async () => {
    const grid = await import("../src/data-grid.js");
    expect(grid.dbCaretAfterFormat("select a,b", 9, "select a,\n       b"), "before a word: still before it").toBe(17);
    expect(grid.dbCaretAfterFormat("select a, b", 10, "select a,\n       b")).toBe(17);
    expect(grid.dbCaretAfterFormat("select a, b", 9, "select a,\n       b"), "after a word: still after it").toBe(9);
    expect(grid.dbCaretAfterFormat("select a", 3, "select a"), "inside a word").toBe(3);
    expect(grid.dbCaretAfterFormat("select a", 0, "select a")).toBe(0);
  });
});

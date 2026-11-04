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

/* SPEC §data.console, §data.grid, §panel.ui: a wide answer. The owner's ask: a table of dozens or
   a hundred columns answered a SELECT the panel could only show by scrolling right and right
   again - a result's columns could not be resized, every one took up to 340 px, and the last
   columns were out of reach. A result's header now carries the table's grip (drag, double-click
   to fit) and keeps its widths per connection and column list, the cap is 240 px, and a finder
   on the results bar (and a table's filter row) lists the columns under a find box and jumps to the
   one picked - the library's popupMenu grew that find box. These drive the real module and the
   real view against a real DOM with a routed fetch. */
import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbConn as dbConnState, dbTab, unmountDbView } from "../src/db-state.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";
import {
  DB_COL_MAX, DB_COL_MIN, dbColFit, dbColGrip, dbGridTrack, dbResultColsSig, dbResultWidthSave, dbResultWidths,
} from "../src/data-columns.js";
import { closeMenu, popupMenu } from "../src/ui/menu.js";
import { sheet } from "./styles.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* The routed server: "SELECT wide" answers thirty columns, anything else one. */
const WIDE = Array.from({ length: 30 }, (_, i) => "col_" + i);
function answer(url: string, init?: { body?: unknown }): { ok: boolean; status: number; body: unknown } {
  const p = new URL(url, "http://x").pathname;
  const body = JSON.parse(String(init?.body || "{}")) as { sql?: string };
  if (p === "/api/db") return { ok: true, status: 200, body: { connections: [dbConn("shop", "mysql")] } };
  if (p.endsWith("/tables")) return { ok: true, status: 200, body: { tables: [], total: 0, page: 0, limit: 5000, more: false } };
  if (p.endsWith("/databases")) return { ok: true, status: 200, body: { primary: null, current: null, databases: [] } };
  if (p.endsWith("/query")) {
    const cols = (body.sql ?? "").includes("wide") ? WIDE : ["n"];
    const row = Object.fromEntries(cols.map((c) => [c, "v" + c]));
    return { ok: true, status: 200, body: { columns: cols, rows: [row, row], rowCount: 2, elapsedMs: 1 } };
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
const grid = await import("../src/data-grid.js");
const filters = await import("../src/data-filters.js");

const flush = async (): Promise<void> => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); };

/** Run `text` in a console on shop and let the answer paint. */
async function run(text: string): Promise<void> {
  await view.dbSwitchConn("shop");
  await flush();
  if (dbTab().kind !== "sql") tabs.dbOpenTab({ kind: "sql" });
  await flush();
  const st = dbTab();
  if (st.kind !== "sql") throw new Error("not a console tab");
  st.sqlText = text;
  const ta = document.getElementById("dbSql") as HTMLTextAreaElement;
  ta.value = text;
  ta.setSelectionRange(text.length, text.length);
  (document.getElementById("dbSqlRun") as HTMLElement).click();
  await flush();
}

const menu = (): HTMLElement | null => document.getElementById("menu");
const shown = (): string[] => Array.from(menu()?.querySelectorAll<HTMLButtonElement>("button") ?? [])
  .filter((b) => !b.hidden).map((b) => (b.textContent || "").trim());
function typeInto(box: HTMLInputElement, text: string): void {
  box.value = text;
  box.dispatchEvent(new Event("input"));
}
function key(target: HTMLElement, k: string): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true }));
}
const headers = (): HTMLElement[] => Array.from(document.querySelectorAll<HTMLElement>("#dbGridWrap thead th.db-col"));

beforeEach(async () => {
  closeMenu();
  page.unmount();
  unmountDbView();
  try { localStorage.clear(); } catch (e) { /* none here */ }
  vi.restoreAllMocks();
  await page.mount();
  await flush();
});

describe("a result's widths, filed by connection and column list", () => {
  it("the key is the connection and the columns, in order", () => {
    const a = dbResultColsSig("shop", ["id", "name"]);
    expect(dbResultColsSig("shop", ["id", "name"])).toBe(a);
    expect(dbResultColsSig("crm", ["id", "name"])).not.toBe(a);
    expect(dbResultColsSig("shop", ["name", "id"])).not.toBe(a);
    expect(dbResultColsSig("shop", ["id", "name", "x"])).not.toBe(a);
    expect(a.length, "a hundred columns still key short").toBeLessThan(10);
  });

  it("a width saved reads back; a corrupt store or a width outside the clamp is none", () => {
    dbResultWidthSave("s1", "a", 180);
    dbResultWidthSave("s1", "b", 64);
    dbResultWidthSave("s2", "a", 300);
    expect(dbResultWidths("s1")).toEqual({ a: 180, b: 64 });
    expect(dbResultWidths("s2")).toEqual({ a: 300 });
    expect(dbResultWidths("nope")).toEqual({});
    localStorage.setItem("swiss.dbResultCols", JSON.stringify({ s3: { w: { a: 5, b: DB_COL_MAX + 1, c: "x", d: 99.6 }, at: 1 } }));
    expect(dbResultWidths("s3")).toEqual({ d: 100 });
    localStorage.setItem("swiss.dbResultCols", "{broken");
    expect(dbResultWidths("s3")).toEqual({});
    dbResultWidthSave("s4", "a", 90);
    expect(dbResultWidths("s4"), "a broken store is started again, not a broken grid").toEqual({ a: 90 });
  });

  it("fifty results are kept, the least recently resized going first", () => {
    let t = 1000;
    vi.spyOn(Date, "now").mockImplementation(() => ++t);
    for (let i = 0; i < 50; i++) dbResultWidthSave("r" + i, "a", 100);
    dbResultWidthSave("r0", "b", 120); // touched again: the newest now
    dbResultWidthSave("r50", "a", 100);
    dbResultWidthSave("r51", "a", 100);
    const kept = Object.keys(JSON.parse(localStorage.getItem("swiss.dbResultCols") || "{}") as object);
    expect(kept).toHaveLength(50);
    expect(kept).not.toContain("r1");
    expect(kept).not.toContain("r2");
    expect(kept).toContain("r0");
    expect(kept).toContain("r51");
    expect(dbResultWidths("r0")).toEqual({ a: 100, b: 120 });
  });
});

describe("the header's grip: drag to size, double-click to fit", () => {
  /** A three-row column on its own table, its grip saving into `saved`. */
  function column(): { th: HTMLElement; cells: HTMLElement[]; grip: HTMLElement; saved: number[] } {
    const table = document.createElement("table");
    table.innerHTML = "<thead><tr><th>x</th><th>name</th></tr></thead><tbody>"
      + "<tr><td>1</td><td>a</td></tr><tr><td>2</td><td>bb</td></tr><tr><td>3</td><td>ccc</td></tr></tbody>";
    document.body.appendChild(table);
    const th = table.querySelectorAll<HTMLElement>("th")[1];
    const saved: number[] = [];
    const grip = dbColGrip(th, (w) => { saved.push(w); });
    th.appendChild(grip);
    const cells = [th, ...Array.from(table.querySelectorAll<HTMLElement>("tbody td:nth-child(2)"))];
    return { th, cells, grip, saved };
  }

  it("a drag sizes every cell of the column live and saves once, on release", () => {
    const { cells, grip, saved } = column();
    expect(grip.className).toBe("db-col-grip");
    grip.dispatchEvent(new MouseEvent("mousedown", { clientX: 100, bubbles: true }));
    document.dispatchEvent(new MouseEvent("mousemove", { clientX: 260 }));
    expect(cells.map((c) => c.style.width)).toEqual(["160px", "160px", "160px", "160px"]);
    expect(cells[2].style.maxWidth, "max-width too: a width alone is a hint nowrap outvotes").toBe("160px");
    expect(saved, "nothing saved mid-drag").toEqual([]);
    document.dispatchEvent(new MouseEvent("mousemove", { clientX: 5000 }));
    document.dispatchEvent(new MouseEvent("mouseup"));
    expect(saved).toEqual([DB_COL_MAX]);
    document.dispatchEvent(new MouseEvent("mousemove", { clientX: 100 }));
    expect(cells[0].style.width, "released: the pointer no longer drives it").toBe(DB_COL_MAX + "px");
  });

  it("a press without a move saves nothing", () => {
    const { grip, saved } = column();
    grip.dispatchEvent(new MouseEvent("mousedown", { clientX: 100, bubbles: true }));
    document.dispatchEvent(new MouseEvent("mouseup"));
    expect(saved).toEqual([]);
  });

  it("a double-click fits the widest cell, the header's included, clamped", () => {
    const { cells, grip, saved } = column();
    const widths = [70, 40, 212.4, 90];
    cells.forEach((c, i) => { Object.defineProperty(c, "scrollWidth", { configurable: true, get: () => widths[i] }); });
    cells.forEach((c) => { c.style.width = "60px"; c.style.maxWidth = "60px"; });
    grip.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(saved).toEqual([214]);
    expect(cells.map((c) => c.style.maxWidth)).toEqual(["214px", "214px", "214px", "214px"]);
    widths.fill(9000);
    expect(dbColFit(cells)).toBe(DB_COL_MAX);
    widths.fill(3);
    expect(dbColFit(cells)).toBe(DB_COL_MIN);
  });
});

describe("popupMenu's find box (SPEC §panel.ui)", () => {
  const items = (picked: string[]) => ["order_id", "customer_name", "created_at", "updated_at"].map((label) => ({
    label, fn: () => { picked.push(label); },
  }));

  it("takes the focus, narrows the rows as it is typed into, and says when nothing matches", () => {
    popupMenu({ left: 10, top: 10, bottom: 30 }, items([]), { filter: "Find a column" });
    const box = menu()?.querySelector<HTMLInputElement>("input.menu-filter");
    if (!box) throw new Error("a find box");
    expect(menu()?.classList.contains("find")).toBe(true);
    expect(box.placeholder).toBe("Find a column");
    expect(document.activeElement).toBe(box);
    expect(menu()?.querySelector(".menu-rows")?.children).toHaveLength(4);
    typeInto(box, "AT");
    expect(shown(), "case does not matter").toEqual(["created_at", "updated_at"]);
    const none = menu()?.querySelector<HTMLElement>(":scope > .menu-head");
    expect(none?.hidden).toBe(true);
    typeInto(box, "zzz");
    expect(shown()).toEqual([]);
    expect(none?.hidden).toBe(false);
    expect(none?.textContent).toBe("Nothing matches");
    typeInto(box, "");
    expect(shown()).toHaveLength(4);
  });

  it("the arrows walk the box and the rows left; Enter runs the first; a letter on a row goes to the box", () => {
    const picked: string[] = [];
    popupMenu({ left: 10, top: 10, bottom: 30 }, items(picked), { filter: "Find" });
    const box = menu()?.querySelector<HTMLInputElement>("input.menu-filter");
    if (!box) throw new Error("a find box");
    typeInto(box, "_at");
    key(box, "ArrowDown");
    expect(document.activeElement?.textContent).toBe("created_at");
    key(document.activeElement as HTMLElement, "ArrowDown");
    expect(document.activeElement?.textContent).toBe("updated_at");
    key(document.activeElement as HTMLElement, "ArrowDown");
    expect(document.activeElement, "past the last row: round to the box").toBe(box);
    key(box, "ArrowUp");
    expect(document.activeElement?.textContent).toBe("updated_at");
    key(document.activeElement as HTMLElement, "u");
    expect(document.activeElement, "typing on a row keeps typing into the box").toBe(box);
    typeInto(box, "upd");
    key(box, "Enter");
    expect(picked).toEqual(["updated_at"]);
    expect(menu(), "a choice closes the menu").toBeNull();
  });

  it("Enter with nothing left runs nothing; a click into the box stays in the menu", () => {
    const picked: string[] = [];
    popupMenu({ left: 10, top: 10, bottom: 30 }, items(picked), { filter: "Find" });
    const box = menu()?.querySelector<HTMLInputElement>("input.menu-filter");
    if (!box) throw new Error("a find box");
    const outside = vi.fn();
    document.addEventListener("click", outside);
    box.click();
    document.removeEventListener("click", outside);
    expect(outside, "the document's click closes open menus: it must not hear this one").not.toHaveBeenCalled();
    typeInto(box, "zzz");
    key(box, "Enter");
    expect(picked).toEqual([]);
    expect(menu()).not.toBeNull();
    key(box, "Escape");
    expect(menu()).toBeNull();
  });

  it("without a filter the menu is what it was: no box, the first row focused", () => {
    popupMenu({ left: 10, top: 10, bottom: 30 }, items([]));
    expect(menu()?.querySelector("input")).toBeNull();
    expect(menu()?.classList.contains("find")).toBe(false);
    expect(document.activeElement?.textContent).toBe("order_id");
  });

  it("the box and the scrolling rows are the library's (ui.css)", () => {
    const ui = sheet("ui.css");
    expect(ui).toMatch(/\.menu-filter \{[^}]*margin-bottom/);
    expect(ui).toMatch(/\.menu-rows \{[^}]*min-height:[^}]*overflow-y: auto/);
  });
});

describe("a wide result on the real console (SPEC §data.console)", () => {
  it("every column has a grip, none sorts, and the cap is 240 px", async () => {
    await run("SELECT wide");
    const ths = headers();
    expect(ths).toHaveLength(30);
    expect(ths.every((th) => th.querySelector(".db-col-grip"))).toBe(true);
    expect(ths.every((th) => th.classList.contains("db-nosort"))).toBe(true);
    expect(ths.map((th) => th.dataset.ci)).toEqual(WIDE.map((_, i) => String(i)));
    expect(sheet("views.css")).toMatch(/\.db-grid th, \.db-grid td \{[^}]*max-width: 240px/);
  });

  it("a dragged width is kept for the same columns on the same connection", async () => {
    await run("SELECT wide");
    const grip = headers()[3].querySelector<HTMLElement>(".db-col-grip");
    if (!grip) throw new Error("a grip");
    grip.dispatchEvent(new MouseEvent("mousedown", { clientX: 100, bubbles: true }));
    document.dispatchEvent(new MouseEvent("mousemove", { clientX: 230 }));
    document.dispatchEvent(new MouseEvent("mouseup"));
    expect(dbResultWidths(dbResultColsSig("shop", WIDE))).toEqual({ col_3: 130 });
    await run("SELECT wide -- again");
    const th = headers()[3];
    expect(th.style.width).toBe("130px");
    const cells = Array.from(document.querySelectorAll<HTMLElement>("#dbGridWrap tbody tr > td:nth-child(5)"));
    expect(cells).toHaveLength(2);
    expect(cells.every((td) => td.style.maxWidth === "130px"), "the body cells too").toBe(true);
    expect(headers()[4].style.width, "only the column dragged").toBe("");
  });

  it("the results bar's finder lists the columns, narrows them and jumps to the one picked", async () => {
    await run("SELECT wide");
    const find = document.querySelector<HTMLButtonElement>("#dbRbar .btn.icon[aria-label=\"Find a column\"]");
    if (!find) throw new Error("the finder button on the results bar");
    find.click();
    expect(menu()?.classList.contains("find")).toBe(true);
    expect(shown()).toEqual(WIDE);
    const box = menu()?.querySelector<HTMLInputElement>("input.menu-filter");
    if (!box) throw new Error("a find box");
    typeInto(box, "col_27");
    expect(shown()).toEqual(["col_27"]);
    key(box, "Enter");
    expect(menu()).toBeNull();
    const hit = headers().filter((th) => th.classList.contains("db-hit"));
    expect(hit.map((th) => th.dataset.ci)).toEqual(["27"]);
    expect(sheet("views.css")).toMatch(/th\.db-hit \{[^}]*border-bottom-color: var\(--accent\); box-shadow: inset 0 -1px var\(--accent\)/);
  });

  it("a one-column answer has no finder", async () => {
    await run("SELECT 1");
    expect(headers()).toHaveLength(1);
    expect(document.querySelector("#dbRbar .btn.icon")).toBeNull();
  });
});

describe("a table tab's finder, on its filter row (SPEC §data.grid)", () => {
  it("lists every column, the hidden one marked; picking it shows it again and goes to it", async () => {
    await view.dbSwitchConn("shop");
    await flush();
    const c = dbConnState();
    c.gridCfg = { widths: {}, hidden: ["name"] };
    const tab = dbTab();
    if (tab.kind !== "table") throw new Error("a fresh connection opens on a table tab");
    tab.table = "t"; tab.schema = null;
    tab.data = dbPage({
      table: "t", columns: [dbCol("id"), dbCol("name"), dbCol("email")],
      rows: [{ id: 1, name: "a", email: "x" }], total: 1, primaryKey: ["id"], editable: false,
    });
    tab.pane = "data"; tab.loading = false;
    grid.renderDbToolbar();
    grid.renderDbGrid();
    filters.renderDbFilters();
    expect(headers().map((th) => th.dataset.sort)).toEqual(["id", "email"]);
    const find = document.querySelector<HTMLButtonElement>('#dbFilters .btn.icon[aria-label="Find a column"]');
    if (!find) throw new Error("the finder button on the filter row");
    expect(find.querySelector("use")?.getAttribute("href")).toBe("#i-search");
    // connect.ts closes menus at document: the opener must stop this very click there.
    document.addEventListener("click", closeMenu);
    try { find.click(); } finally { document.removeEventListener("click", closeMenu); }
    expect(menu()?.classList.contains("find")).toBe(true);
    expect(shown()).toEqual(["id", "name", "email"]);
    const hiddenRow = Array.from(menu()?.querySelectorAll<HTMLButtonElement>("button") ?? []).find((b) => b.textContent === "name");
    expect(hiddenRow?.querySelector("use")?.getAttribute("href")).toBe("#i-eye-off");
    expect(hiddenRow?.title).toBe("Hidden · choose it to show it again");
    hiddenRow?.click();
    expect(c.gridCfg.hidden).toEqual([]);
    expect(JSON.parse(localStorage.getItem("swiss.dbGrid.shop_t") || "{}")).toMatchObject({ hidden: [] });
    expect(headers().map((th) => th.dataset.sort)).toEqual(["id", "name", "email"]);
    expect(headers().filter((th) => th.classList.contains("db-hit")).map((th) => th.dataset.sort)).toEqual(["name"]);
  });

  it("keeps the finder out of the table's overflow menu and hides it for one column", async () => {
    await view.dbSwitchConn("shop");
    await flush();
    const tab = dbTab();
    if (tab.kind !== "table") throw new Error("a table tab");
    tab.table = "t"; tab.pane = "data"; tab.loading = false;
    tab.data = dbPage({ columns: [dbCol("id"), dbCol("name")] });
    grid.renderDbToolbar();
    filters.renderDbFilters();
    expect(document.querySelector('#dbFilters [aria-label="Find a column"]')).not.toBeNull();
    const more = document.querySelector<HTMLButtonElement>("#dbHead .btn.icon.ghost[aria-haspopup=menu]");
    if (!more) throw new Error("the table's overflow menu");
    more.click();
    expect(shown()).not.toContain("Find a column");
    closeMenu();
    tab.data.columns = [dbCol("id")];
    filters.renderDbFilters();
    expect(document.querySelector('#dbFilters [aria-label="Find a column"]')).toBeNull();
    tab.data.columns.push(dbCol("name"));
    tab.pane = "form";
    filters.renderDbFilters();
    expect(document.querySelector('#dbFilters [aria-label="Find a column"]')).toBeNull();
  });
});

describe("the grid's vertical track starts below its header (SPEC §data.grid)", () => {
  it("insets only the vertical track by the measured header", () => {
    expect(sheet("views.css")).toMatch(/\.db-grid-wrap::-webkit-scrollbar-track:vertical \{[^}]*margin-top: var\(--db-head-h, 0px\)/);
  });

  it("measures result and table headers after repaint, including types and comments", async () => {
    let height = 27;
    vi.spyOn(HTMLTableSectionElement.prototype, "getBoundingClientRect").mockImplementation(() => new DOMRect(0, 0, 400, height));
    await run("SELECT wide");
    const wrap = document.getElementById("dbGridWrap");
    expect(wrap?.style.getPropertyValue("--db-head-h")).toBe("27px");
    await view.dbSwitchConn("shop");
    await flush();
    tabs.dbOpenTab({ kind: "table", table: "", schema: null });
    await flush();
    const tab = dbTab();
    if (tab.kind !== "table") throw new Error("a table tab");
    tab.table = "t";
    tab.data = dbPage({ columns: [dbCol("id"), dbCol("name", { comment: "A comment" })] });
    tab.pane = "data"; tab.loading = false;
    height = 63;
    grid.renderDbGrid();
    await flush();
    expect(document.querySelector("#dbGridWrap .db-col-comment")).not.toBeNull();
    expect(document.getElementById("dbGridWrap")?.style.getPropertyValue("--db-head-h")).toBe("63px");
  });

  it("clears the offset for Activity, empty and nested content, then measures a row grid again", async () => {
    vi.spyOn(HTMLTableSectionElement.prototype, "getBoundingClientRect").mockImplementation(() => new DOMRect(0, 0, 400, 27));
    await run("SELECT wide");
    const wrap = document.getElementById("dbGridWrap");
    if (!wrap) throw new Error("the grid wrap");
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("27px");
    const table = wrap.querySelector("table");
    if (!table) throw new Error("the result grid");
    // Activity uses a top-level db-grid too, but has no selectable row headers.
    const activity = document.createElement("table");
    activity.className = "db-grid";
    activity.innerHTML = "<thead><tr><th>PID</th><th>Query</th></tr></thead><tbody></tbody>";
    wrap.replaceChildren(activity);
    await flush();
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("0px");
    const nested = document.createElement("div");
    nested.appendChild(table);
    wrap.replaceChildren(nested);
    await flush();
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("0px");
    wrap.replaceChildren(document.createElement("div"), table);
    await flush();
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("0px");
    wrap.replaceChildren(table);
    await flush();
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("27px");
    wrap.replaceChildren();
    await flush();
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("0px");
  });

  it("follows header resizes and releases both observers when the page leaves", async () => {
    let height = 27;
    vi.spyOn(HTMLTableSectionElement.prototype, "getBoundingClientRect").mockImplementation(() => new DOMRect(0, 0, 400, height));
    await run("SELECT wide");
    const wrap = document.getElementById("dbGridWrap");
    if (!wrap) throw new Error("the grid wrap");
    const observed = new Set<Element>();
    let resize = (): void => {};
    vi.spyOn(globalThis, "ResizeObserver").mockImplementation(class implements ResizeObserver {
      constructor(cb: ResizeObserverCallback) { resize = (): void => { cb([], this); }; }
      observe(target: Element): void { observed.add(target); }
      unobserve(target: Element): void { observed.delete(target); }
      disconnect(): void { observed.clear(); }
    });
    dbGridTrack(wrap);
    expect([...observed]).toEqual([wrap.querySelector("thead")]);
    height = 49.5;
    resize();
    expect(wrap.style.getPropertyValue("--db-head-h")).toBe("50px");
    page.unmount();
    expect(observed.size).toBe(0);
    wrap.replaceChildren();
    await flush();
    expect(wrap.style.getPropertyValue("--db-head-h"), "the old wrap is no longer watched").toBe("50px");
  });
});

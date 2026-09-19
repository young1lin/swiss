/*
 * Copyright 2026 The swiss authors
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

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dbView } from "../src/db-state.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), sharpened where the
   focus-ring behavior needs it: classList REALLY mutates className, setAttribute records
   attrs, querySelector walks the children tree (tag / .class / [attr="v"] chains), and an
   innerHTML assignment CLEARS the children — so a full-grid rebuild is observable as the
   death of a marker node that survived every ring move. */
type Stub = Record<string, any> & { children: Stub[]; attrs: Record<string, string> };
const matches = (n: Stub, sel: string): boolean => {
  const m = /^([a-z]+)((?:\.[A-Za-z0-9_-]+)*)((?:\[[a-z-]+="[^"]*"\])*)$/i.exec(sel);
  if (!m) return false;
  if (String(n.tag).toLowerCase() !== m[1].toLowerCase()) return false;
  const cls = String(n.className).split(/\s+/);
  for (const c of m[2].slice(1).split(".")) if (c && !cls.includes(c)) return false;
  const re = /\[([a-z-]+)="([^"]*)"\]/g;
  let a: RegExpExecArray | null;
  while ((a = re.exec(m[3]))) if (n.attrs[a[1]] !== a[2]) return false;
  return true;
};
const queryAll = (root: Stub, sel: string, out: Stub[] = []): Stub[] => {
  for (const c of root.children || []) {
    if (matches(c, sel)) out.push(c);
    queryAll(c, sel, out);
  }
  return out;
};
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], attrs: {}, style: {}, dataset: {}, hidden: false, disabled: false,
    checked: false, value: "", textContent: "", className: "", id: "", title: "", rows: 0,
    type: "", selectionStart: 0, scrollHeight: 22, scrollTop: 0, scrollLeft: 0,
    setAttribute(k: string, v: string) { n.attrs[k] = String(v); if (k === "id") n.id = String(v); },
    getAttribute(k: string) { return Object.prototype.hasOwnProperty.call(n.attrs, k) ? n.attrs[k] : null; },
    removeAttribute(k: string) { delete n.attrs[k]; },
    classList: {
      add(...cs: string[]) {
        const s = new Set(String(n.className).split(/\s+/).filter(Boolean));
        cs.forEach((c) => s.add(c));
        n.className = [...s].join(" ");
      },
      remove(...cs: string[]) {
        const s = new Set(String(n.className).split(/\s+/).filter(Boolean));
        cs.forEach((c) => s.delete(c));
        n.className = [...s].join(" ");
      },
      toggle(c: string) { (String(n.className).split(/\s+/).includes(c)) ? n.classList.remove(c) : n.classList.add(c); },
      contains: (c: string) => String(n.className).split(/\s+/).includes(c),
    },
    appendChild(c: Stub) { (c as any).remove?.(); n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: (sel: string) => queryAll(n, sel)[0] ?? null,
    querySelectorAll: (sel: string) => queryAll(n, sel),
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", {
    get: () => "",
    set: () => { n.children = []; }, // a real innerHTML="" wipes the subtree — the test's oracle
  });
  return n;
};
const byId: Record<string, Stub> = {};
const doc: any = {
  documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
  activeElement: null,
  createElement: (t: string) => el(t), createTextNode: (s: string) => ({ text: s }),
  getElementById: (id: string) => (byId[id] ||= el()),
  querySelector: (sel: string) => (byId.dbGridWrap ? queryAll(byId.dbGridWrap, sel)[0] ?? null : null),
  querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};
Object.assign(globalThis, {
  document: doc,
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const grid = await import(pathToFileURL(join(here, "data-grid.js")).href) as {
  renderDbGrid: () => void;
  dbFocusCell: (r: number, c: number) => void;
};

/** A two-row editable grid, rendered once into a wrap the test holds. */
function renderTwoRowGrid(): { d: Record<string, any>; wrap: Stub } {
  const d = dbView();
  d.conns = [dbConn("c", "mysql")];
  d.conn = "c"; d.table = "t"; d.schema = null;
  d.data = dbPage({
    table: "t", columns: [dbCol("id"), dbCol("name")],
    rows: [{ id: 1, name: "a" }, { id: 2, name: "b" }],
    total: 2, primaryKey: ["id"], editable: true,
  });
  d.tab = "data"; d.sqlResult = null; d.sqlBusy = false; d.loading = false;
  d.gridCfg = { widths: {}, hidden: [] }; d.focus = null; d.conflict = null; d.detail = null;
  d.updates = {}; d.deletes = {}; d.inserts = []; d.sel = {};
  byId.dbGridWrap = el();
  grid.renderDbGrid();
  // Register the real #dbKbd the render just built, so $("dbKbd") finds it like on a page
  // (input.db-kbd — the row checkboxes are inputs too).
  const kbd = queryAll(byId.dbGridWrap, "input.db-kbd")[0];
  if (kbd) byId.dbKbd = kbd;
  return { d, wrap: byId.dbGridWrap };
}

describe("the grid's keyboard focus ring (docs/22 closeout audit P0-B)", () => {
  it("a cell mousedown moves the ring on the LIVE grid — no innerHTML rebuild, so the click/dblclick that follows still lands", () => {
    // Real-input regression (CDP: a mousedown that rebuilt the table left only
    // mousedown+mouseup in the stream — no click, no dblclick — so double-click editing
    // could never open). The oracle here is a marker node the panel never adds: it survives
    // every ring move and dies on any full wipe.
    const { d, wrap } = renderTwoRowGrid();
    const marker = el();
    wrap.appendChild(marker);
    const td = queryAll(wrap, 'td[data-r="1"][data-c="0"]')[0];
    expect(td, "the data cell carries its data-r/data-c address").toBeTruthy();
    td.onmousedown({ preventDefault: () => {} });
    expect(d.focus, "the focus cell moved to the clicked row").toEqual({ r: 1, c: 0 });
    expect(queryAll(wrap, "td").includes(marker as any)).toBe(false); // sanity: marker is not a td
    expect(wrap.children.includes(marker), "no rebuild happened — the marker survived").toBe(true);
    expect(td.classList.contains("db-focus"), "the ring landed on the very cell that was clicked").toBe(true);
  });

  it("an arrow key moves the ring the same way — the table is not rebuilt mid-navigation", () => {
    const { d, wrap } = renderTwoRowGrid();
    const marker = el();
    wrap.appendChild(marker);
    const tdRow1 = queryAll(wrap, 'td[data-r="1"][data-c="0"]')[0];
    tdRow1.onmousedown({ preventDefault: () => {} });
    const kbd = byId.dbKbd;
    kbd.onkeydown({ key: "ArrowUp", preventDefault: () => {}, ctrlKey: false, metaKey: false, altKey: false });
    expect(d.focus).toEqual({ r: 0, c: 0 });
    expect(wrap.children.includes(marker), "no rebuild on navigation").toBe(true);
    const tdRow0 = queryAll(wrap, 'td[data-r="0"][data-c="0"]')[0];
    expect(tdRow0.classList.contains("db-focus"), "the ring moved to the row above").toBe(true);
    expect(tdRow1.classList.contains("db-focus"), "and left the row it came from").toBe(false);
  });

  it("Esc drops the ring without a rebuild either", () => {
    const { d, wrap } = renderTwoRowGrid();
    const marker = el();
    wrap.appendChild(marker);
    queryAll(wrap, 'td[data-r="0"][data-c="0"]')[0].onmousedown({ preventDefault: () => {} });
    const kbd = byId.dbKbd;
    kbd.onkeydown({ key: "Escape", currentTarget: kbd, preventDefault: () => {}, ctrlKey: false, metaKey: false, altKey: false }); // docs/37 M4: Esc reads e.currentTarget for its blur
    expect(d.focus).toBeNull();
    expect(queryAll(wrap, "td.db-focus").length, "the ring is gone").toBe(0);
    expect(wrap.children.includes(marker), "no rebuild on Esc").toBe(true);
  });
});

describe("a cell mousedown keeps the keyboard layer alive (docs/22 closeout audit P0-A)", () => {
  it("the mousedown preventDefaults — the default focus move to <body> would land AFTER dbFocusCell focused #dbKbd and every key would go nowhere", () => {
    const { wrap } = renderTwoRowGrid();
    for (const sel of ['td[data-r="0"][data-c="0"]', 'td[data-r="1"][data-c="1"]']) {
      const td = queryAll(wrap, sel)[0];
      expect(td, `${sel} exists`).toBeTruthy();
      let prevented = false;
      td.onmousedown({ preventDefault: () => { prevented = true; } });
      expect(prevented, `${sel} mousedown preventDefaults`).toBe(true);
    }
  });

  it("a buffered-insert cell preventDefaults the same way", () => {
    const { d, wrap } = renderTwoRowGrid();
    d.data.editable = true;
    d.inserts.push({ values: {} });
    grid.renderDbGrid();
    const kbd = queryAll(byId.dbGridWrap, "input.db-kbd")[0];
    if (kbd) byId.dbKbd = kbd;
    // The insert row sits in front of the page's rows: grid row 0, and it grew the grid.
    const td = queryAll(byId.dbGridWrap, 'td[data-r="0"][data-c="0"]')[0];
    expect(td, "the insert cell exists").toBeTruthy();
    let prevented = false;
    td.onmousedown({ preventDefault: () => { prevented = true; } });
    expect(prevented, "insert cell mousedown preventDefaults").toBe(true);
    expect(d.focus.r, "the focus moved to the insert row").toBe(0);
    expect(d.focus.c).toBe(0);
  });

  it("the insert row's context menu offers Edit in dialog… like a data row (docs/22 closeout B2)", () => {
    // dbCellMenu's 5th parameter is the editInDialog callback; the insert row's call passed a
    // stray null before it, so the callback landed in an unread 6th slot and the item never
    // rendered — the keyboard path could open the dialog but the menu could not.
    const { d } = renderTwoRowGrid();
    d.data.editable = true;
    d.inserts.push({ values: {} });
    grid.renderDbGrid();
    const td = queryAll(byId.dbGridWrap, 'td[data-r="0"][data-c="0"]')[0];
    expect(td, "the insert cell exists").toBeTruthy();
    expect(typeof td.oncontextmenu, "the insert cell carries the menu wiring").toBe("function");
    td.oncontextmenu({ preventDefault: () => {} });
    const menuBtns = queryAll(doc.body, "button");
    const labels = menuBtns.map((b: any) => String(b.textContent));
    expect(labels.some((t: string) => t.includes("Edit in dialog")), "the menu offers the dialog path").toBe(true);
    const item = menuBtns.find((b: any) => String(b.textContent).includes("Edit in dialog"))!;
    expect(() => item.onclick(), "clicking it opens the dialog without throwing").not.toThrow();
  });
});

describe("the header hover card's 260ms timer (docs/22 closeout audit)", () => {
  const scheduled: { fn: () => void; ms: number }[] = [];
  const cleared: unknown[] = [];
  let origSet: any, origClear: any;
  beforeAll(() => {
    origSet = globalThis.setTimeout;
    origClear = globalThis.clearTimeout;
    (globalThis as any).setTimeout = ((fn: () => void, ms: number) => {
      const id = { fn, ms } as any;
      scheduled.push(id);
      return id;
    }) as any;
    (globalThis as any).clearTimeout = ((id: unknown) => { cleared.push(id); }) as any;
  });
  afterAll(() => {
    (globalThis as any).setTimeout = origSet;
    (globalThis as any).clearTimeout = origClear;
  });

  it("a grid rebuild cancels a pending hover card — the timer must not fire into a grid it no longer points at", () => {
    const { wrap } = renderTwoRowGrid();
    const th = queryAll(wrap, "th").find((n) => typeof n.onmouseenter === "function")!;
    expect(th, "a column header carries the hover wiring").toBeTruthy();
    th.onmouseenter(); // schedules the 260ms show
    expect(scheduled.length).toBeGreaterThan(0);
    const pending = scheduled[scheduled.length - 1];
    expect(pending.ms).toBe(260);
    grid.renderDbGrid(); // the rebuild calls dbTipHide — which must cancel the pending show
    expect(cleared.includes(pending), "the pending show timer was cleared").toBe(true);
  });
});

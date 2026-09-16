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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), with child-tracking
   nodes so a test can walk the filter row's selects. confirm defaults to REFUSAL: the audit
   case is a user with buffered edits saying "no" to the discard gate. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, checked: false,
    value: "", textContent: "", className: "", id: "", title: "", type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    setAttribute() {}, getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => { n.children = []; } });
  return n;
};
const byId: Record<string, Stub> = {};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    createElement: (t: string) => el(t), createTextNode: (s: string) => ({ text: s }),
    getElementById: (id: string) => (byId[id] ||= el()),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener: () => {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => false, // the user refuses to discard their buffered edits
  alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js");
const filters = await import(pathToFileURL(join(here, "data-filters.js")).href) as {
  renderDbFilters: () => void;
};
const csv = await import(pathToFileURL(join(here, "data-csv.js")).href) as {
  dbPushCellFilter: (column: string, op: string, value: string) => void;
};
const util = await import(pathToFileURL(join(here, "util.js")).href) as {
  state: { db: Record<string, any> };
};

function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}

/** One filter row ("a" = "1") plus a buffered update, so the discard gate asks. */
function renderFilterRow(): { d: Record<string, any>; selects: Stub[] } {
  const d = util.state.db;
  d.conns = [{ name: "c", dialect: "mysql" }];
  d.conn = "c"; d.table = "t"; d.tab = "data"; d.sqlResult = null;
  d.data = { table: "t", columns: [{ name: "a" }, { name: "b" }], rows: [], total: 0, primaryKey: ["a"], editable: false };
  d.filters = [{ column: "a", op: "eq", value: "1" }];
  d.updates = { k1: { pk: { a: 1 }, changes: { a: "buffered" } } };
  d.deletes = {}; d.inserts = []; d.sel = {}; d.gridCfg = { widths: {}, hidden: [] };
  byId.dbFilters = el();
  filters.renderDbFilters();
  const selects = find(byId.dbFilters, (n) => n.tag === "select");
  expect(selects.length).toBeGreaterThanOrEqual(2);
  return { d, selects: selects.slice(0, 2) }; // [column, operator]
}

describe("a refused discard leaves the filter row exactly as it was (docs/22 closeout audit)", () => {
  it("changing the column select and refusing does not keep the new column", () => {
    const { d, selects } = renderFilterRow();
    const [cs] = selects;
    cs.value = "b";
    cs.onchange.call(cs);
    expect(d.filters[0].column, "the row keeps its column").toBe("a");
  });

  it("picking a valueless operator and refusing does not keep the new op", () => {
    const { d, selects } = renderFilterRow();
    const os = selects[1];
    os.value = "isNull";
    os.onchange.call(os); // isNull applies at once — through the discard gate
    expect(d.filters[0].op, "the row keeps its operator").toBe("eq");
  });

  it("the row's remove button stays after a refused discard (docs/22 closeout B6)", () => {
    const { d } = renderFilterRow();
    const rm = find(byId.dbFilters, (n) => n.tag === "button" && n.title === "Remove this filter")[0];
    expect(rm, "the remove button is wired").toBeTruthy();
    rm.onclick();
    expect(d.filters.length, "the refused discard kept the row").toBe(1);
    expect(d.filters[0].column).toBe("a");
  });

  it("a pushed cell filter is taken back on a refused discard (docs/22 closeout B6)", () => {
    const { d } = renderFilterRow();
    d.filters = []; // nothing on screen — the push is the only movement
    csv.dbPushCellFilter("a", "eq", "x");
    expect(d.filters.length, "the refused discard took the pushed row back").toBe(0);
  });
});

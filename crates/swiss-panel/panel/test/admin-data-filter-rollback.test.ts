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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dbConn as dbConnState, dbTab } from "../src/db-state.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), with child-tracking
   nodes so a test can walk the filter row's selects. confirm defaults to REFUSAL: the audit
   case is a user with buffered edits saying "no" to the discard gate.
   docs/37 R5: the rows are built with h()/fill() and carry data-fi/data-fk/data-frm addresses
   instead of per-render handlers, so the stub extends the Node stub (h() instanceof-checks
   children), closest self-matches the attribute selectors the dispatchers climb, and the
   tests fire the exported dispatchers the pane's delegated listeners would call. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
/* Self-match the selector units the filter dispatchers use: "#id", "[data-x]",
   '[data-x="v"]' — a comma list matches when any unit does. */
function selfMatches(n: Stub, selector: string): boolean {
  return selector.split(",").some((unit) => {
    const u = unit.trim();
    if (u.charAt(0) === "#") return n.id === u.slice(1);
    const m = u.match(/^\[data-([\w-]+)(?:="([^"]*)")?\]$/);
    if (!m) return false;
    return typeof n.dataset[m[1]] !== "undefined" && (m[2] === undefined || n.dataset[m[1]] === m[2]);
  });
}
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, checked: false,
    value: "", textContent: "", className: "", id: "", title: "", type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false,
    closest(sel: string) { return selfMatches(n, sel) ? n : null; },
    setAttribute(k: string, v: string) { if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => { n.children = []; } });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
    createDocumentFragment: () => el("#document-fragment"),
    createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
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

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const filters = await import(pathToFileURL(join(here, "data-filters.js")).href) as {
  renderDbFilters: () => void;
  dbFiltersChange: (t: Stub) => boolean;
  dbFiltersClick: (t: Stub) => boolean;
};
const csv = await import(pathToFileURL(join(here, "data-csv.js")).href) as {
  dbPushCellFilter: (column: string, op: string, value: string) => void;
};

function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}

/** One filter row ("a" = "1") plus a buffered update, so the discard gate asks. */
function renderFilterRow(): { d: Record<string, any>; selects: Stub[] } {
  const c = dbConnState();
  c.conns = [dbConn("c", "mysql")];
  c.conn = "c"; c.sqlResult = null; c.gridCfg = { widths: {}, hidden: [] };
  // The filter rows are the open table tab's (docs/42 T1); the callers read only tab
  // fields through d, so d IS the narrowed tab.
  const tab = dbTab();
  if (tab.kind !== "table") throw new Error("fresh state must hold a table tab");
  const d = tab as unknown as Record<string, any>;
  d.table = "t"; d.pane = "data";
  d.data = dbPage({ table: "t", columns: [dbCol("a"), dbCol("b")], primaryKey: ["a"] });
  d.filters = [{ column: "a", op: "eq", value: "1" }];
  d.updates = { k1: { pk: { a: 1 }, changes: { a: "buffered" } } };
  d.deletes = {}; d.inserts = []; d.sel = {};
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
    // docs/37 R5: the pane's delegated change listener would resolve this select by its
    // data-fi/data-fk address — the dispatcher is the pane's answer, fired directly.
    filters.dbFiltersChange(cs);
    expect(d.filters[0].column, "the row keeps its column").toBe("a");
  });

  it("picking a valueless operator and refusing does not keep the new op", () => {
    const { d, selects } = renderFilterRow();
    const os = selects[1];
    os.value = "isNull";
    filters.dbFiltersChange(os); // isNull applies at once — through the discard gate
    expect(d.filters[0].op, "the row keeps its operator").toBe("eq");
  });

  it("the row's remove button stays after a refused discard (docs/22 closeout B6)", () => {
    const { d } = renderFilterRow();
    const rm = find(byId.dbFilters, (n) => n.tag === "button" && n.title === "Remove this filter")[0];
    expect(rm, "the remove button carries its data-frm address").toBeTruthy();
    filters.dbFiltersClick(rm); // the pane's delegated click dispatcher
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

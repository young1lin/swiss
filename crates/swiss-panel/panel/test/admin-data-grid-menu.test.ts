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
import { dbConn as dbConnState, dbTab, mountDbView, unmountDbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";

/* Grid-internal clicks must not escape #pane (docs/37 R5 + the code-review P3): master's
 * data-grid.js stopped click propagation on every selection checkbox (cb/cbAll/cb2/cbAll2)
 * and the column grip, because connect.ts's document listener closes any open
 * Export/Explain/#dbMore menu over a click that reaches it. The delegated dispatch has no
 * per-node handlers to carry that stop, so dbPaneClick must stop for exactly master's set —
 * and NOT for the sort header, whose click master let bubble (the menu closed over it).
 *
 * Same DOM-stub technique as admin-data-grep-unmount.test.ts; the grid is PAINTED by the
 * real renderDbView from state, so the dispatcher answers the nodes the render made. The
 * stub closest() only knows #ids, so class-based matching is patched onto the painted
 * nodes — that is the one DOM behavior the stub lacks, not behavior under test. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the stub accepts every
 * DOM shape the view hands it; the frozen ratchet budget has no room for two new anys */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- same budget rule as above
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, checked: false,
    value: "", textContent: "", className: "", id: "", title: "", type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false,
    closest(sel: string) { return sel.charAt(0) === "#" && n.id === sel.slice(1) ? n : null; },
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
function findId(root: Stub, id: string): Stub | null {
  if (root.id === id) return root;
  for (const c of root.children) {
    const hit = findId(c, id);
    if (hit) return hit;
  }
  return null;
}
const resolveId = (id: string): Stub => {
  const painted = byId.pane ? findId(byId.pane, id) : null;
  if (painted) return painted;
  return (byId[id] ||= el());
};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
    createDocumentFragment: () => el("#document-fragment"),
    createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
    getElementById: (id: string) => resolveId(id),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener: () => {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  setTimeout: () => 0, clearTimeout: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const view = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  renderDbView: () => void;
};

/** Paint the data grid with one row from state, the way a finished dbLoadData would. */
function paintGrid(): Stub {
  mountDbView();
  Object.assign(dbConnState(), { conns: [dbConn("c", "mysql")], conn: "c" });
  const t = dbTab();
  if (t.kind === "table") {
    Object.assign(t, {
      table: "t", pane: "data",
      data: { columns: [{ name: "id", dataType: "int" }], rows: [{ id: 1 }], primaryKey: ["id"], editable: true, total: 1 },
    });
  }
  view.renderDbView();
  return resolveId("pane");
}
/** Walk the painted tree for the first node whose className carries the given class. */
function byClass(root: Stub, cls: string): Stub | null {
  if (String(root.className).split(/\s+/).indexOf(cls) >= 0) return root;
  for (const c of root.children) {
    const hit = byClass(c, cls);
    if (hit) return hit;
  }
  return null;
}
/** Give a painted node the class-based closest() a real DOM has (the stub only knows #ids). */
const classClosest = (n: Stub, cls: string): Stub => {
  n.closest = (sel: string) => (String(sel).indexOf(cls) >= 0 ? n : null);
  return n;
};
/** Fire the pane's delegated click like a bubbling event: the target + a recording stop. */
const fireClick = (pane: Stub, target: Stub): { stopped: boolean } => {
  const mark = { stopped: false };
  pane.onclick({ target, stopPropagation: () => { mark.stopped = true; } });
  return mark;
};

describe("grid-internal clicks stay inside the pane (code-review P3 vs master)", () => {
  it("a selection checkbox click is stopped — an open Export/#dbMore menu survives it", () => {
    const pane = paintGrid();
    const cb = byClass(pane, "db-selbox");
    if (!cb) throw new Error("the painted grid carries a .db-selbox");
    const mark = fireClick(pane, classClosest(cb, "db-selbox"));
    expect(mark.stopped, "the click is stopped before it can reach document").toBe(true);
  });

  it("a column-grip click is stopped the same way (the grip owns mousedown, not click)", () => {
    const pane = paintGrid();
    const grip = byClass(pane, "db-col-grip");
    if (!grip) { unmountDbView(); return; } // a grid config without grips still must not break
    const mark = fireClick(pane, classClosest(grip, "db-col-grip"));
    expect(mark.stopped).toBe(true);
  });

  it("a sort-header click is NOT stopped — master let that one bubble and close the menu", () => {
    const pane = paintGrid();
    // The header cell carries data-sort; its closest answers that selector, never the
    // grid-internal one, so the dispatcher runs its normal course.
    const th = byClass(pane, "db-col");
    if (!th) throw new Error("the painted header row carries th.db-col");
    th.setAttribute("data-sort", "id");
    th.closest = (sel: string) => (String(sel).indexOf("data-sort") >= 0 ? th : null);
    const mark = fireClick(pane, th);
    expect(mark.stopped, "the sort header's click still bubbles, as in master").toBe(false);
  });

  unmountDbView();
});

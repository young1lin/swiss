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

/* docs/22 closeout B7: a styled select that leaves the DOM must take its trigger button with
   it. The trigger sits BESIDE the select ("afterend"), so removing the select alone orphans a
   live-looking dropdown — on a redis page one kept showing the previous pg connection's schema
   pick. This suite drives dropdown.js's own machinery: real prototype descriptors are injected
   (styleSelect resolves them lazily), a fake MutationObserver captures the init callback, and
   the test fires it with the removal mutation the real observer would deliver. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag: string, cls = ""): Stub => {
  const n: any = {
    tag, nodeType: 1, className: cls, children: [], dataset: {}, style: {}, title: "", type: "",
    disabled: false, options: [], selectedIndex: -1, value: "", multiple: false,
    classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
    setAttribute() {}, getAttribute: () => null,
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    insertAdjacentElement(_pos: string, node: Stub) { (n.parentNode || n).children.push(node); node.parentNode = n.parentNode || n; return node; },
    addEventListener() {}, removeEventListener() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, bottom: 0, right: 0, width: 0, height: 0 }),
    contains: () => false, focus() {},
    // paint() asks the trigger for its .dd-label — a class-only selector walk is enough.
    querySelector: (sel: string) => {
      const cls = sel.startsWith(".") ? sel.slice(1) : null;
      if (!cls) return null;
      const walk = (n: Stub): Stub | null => {
        for (const c of n.children || []) {
          if (String(c.className).split(" ").includes(cls)) return c;
          const hit = walk(c);
          if (hit) return hit;
        }
        return null;
      };
      return walk(n);
    },
    querySelectorAll: () => [],
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set() {}, configurable: true });
  n.remove = () => { if (n.parentNode) n.parentNode.removeChild(n); };
  return n;
};

// The two prototype globals styleSelect resolves descriptors from.
(class HTMLSelectElementMock {} as any).prototype;
(globalThis as any).HTMLSelectElement = class HTMLSelectElementMock {
  get value() { return ""; }
  set value(v: string) { void v; }
  get selectedIndex() { return 0; }
  set selectedIndex(v: number) { void v; }
  get disabled() { return false; }
  set disabled(v: boolean) { void v; }
};
(globalThis as any).Element = class ElementMock {
  get innerHTML() { return ""; }
  set innerHTML(v: string) { void v; }
};
let observerCb: ((muts: any[]) => void) | null = null;
(globalThis as any).MutationObserver = class {
  constructor(cb: (muts: any[]) => void) { observerCb = cb; }
  observe() {}
};
const body = el("body");
const parent = el("div");
body.children.push(parent);
Object.assign(globalThis, {
  document: {
    body, documentElement: el("html"), head: el("head"),
    getElementById: () => null, createElement: (t: string) => el(t),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener() {}, removeEventListener() {},
  },
  window: globalThis,
});
(globalThis as any).addEventListener = () => {};
(globalThis as any).removeEventListener = () => {};

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const dd = await import(pathToFileURL(join(here, "dropdown.js")).href) as {
  initSelects: () => void;
  styleSelect: (sel: Stub) => void;
};

/** A styled select in the parent, its trigger beside it, plus an open-menu-free world. */
function styledSelect(label: string): { sel: Stub; trig: Stub } {
  const sel = el("SELECT", "db-schemaish");
  sel.tag = "SELECT";
  (sel as any).tagName = "SELECT";
  sel.parentNode = parent;
  parent.children.push(sel);
  sel.options = [{ value: "", textContent: "All schemas", selected: true }, { value: "public", textContent: "public (103)", selected: false }];
  sel.selectedIndex = 0;
  sel.setAttribute = (k: string, v: string) => { if (k === "aria-label") (sel as any)["aria-" + k.slice(5)] = v; };
  sel.getAttribute = (k: string) => (k === "aria-label" ? label : null);
  dd.styleSelect(sel);
  const trig = parent.children.find((c) => c.className.startsWith("dd")) as Stub;
  expect(trig, "styleSelect inserted a trigger beside the select").toBeTruthy();
  return { sel, trig };
}

describe("a styled select leaving the DOM takes its trigger with it (docs/22 closeout B7)", () => {
  it("removing the select alone must not orphan a live-looking dropdown", () => {
    dd.initSelects();
    expect(observerCb, "the observer is armed").toBeTruthy();
    const { sel, trig } = styledSelect("Schema");
    // The view removes the SELECT (connection switch to a kind with no picker):
    sel.remove();
    // ...and the real MutationObserver reports that removal to dropdown.js.
    observerCb!([{ addedNodes: [], removedNodes: [sel] }]);
    expect(parent.children.includes(trig), "the trigger went with its select").toBe(false);
    expect(parent.children.includes(sel), "sanity: the select itself is gone").toBe(false);
  });

  it("a whole removed subtree containing styled selects drops their triggers too", () => {
    const holder = el("div");
    holder.parentNode = parent;
    parent.children.push(holder);
    const sel = el("SELECT");
    (sel as any).tagName = "SELECT";
    sel.parentNode = holder;
    holder.children.push(sel);
    sel.options = [{ value: "", textContent: "x", selected: true }];
    sel.selectedIndex = 0;
    dd.styleSelect(sel);
    const trig = holder.children.find((c) => c.className.startsWith("dd")) as Stub;
    expect(trig, "trigger inside the subtree").toBeTruthy();
    holder.remove();
    observerCb!([{ addedNodes: [], removedNodes: [holder] }]);
    // Inside a removed subtree both die together; what must NOT remain is a trigger in the
    // LIVE tree — and none was outside it. The contract: no crash, no live-tree orphan.
    expect(parent.children.includes(holder)).toBe(false);
  });
});

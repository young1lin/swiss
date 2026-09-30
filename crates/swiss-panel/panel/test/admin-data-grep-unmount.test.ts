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
import { dbConn as dbConnState, dbIsMounted, mountDbView, unmountDbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), with RECORDING
   timers so the 300ms grep debounce can be fired deterministically — the bug is what its
   callback does after the view unmounted, not 300ms later.
   SPEC §panel.toolchain: the skeleton is built with h()/fill() and the grep box carries no per-render
   wiring — #pane owns one delegated input listener — so the fake DOM extends the Node stub
   (h() instanceof-checks children) and the test fires the pane dispatcher like a real
   event: the target is the painted #dbGrep node, resolved by walking the pane tree. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
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
/* The painted tree owns the ids the view reads back: resolve #dbGrep (and #pane) from the
   pane's built children first, so the dispatcher's target is the node the render made. */
function findId(root: Stub, id: string): Stub | null {
  if (root.id === id) return root;
  for (const c of root.children) {
    const hit = findId(c, id);
    if (hit) return hit;
  }
  return null;
}
const resolveId = (id: string): Stub => {
  const root = byId.pane;
  const painted = root ? findId(root, id) : null;
  if (painted) return painted;
  return (byId[id] ||= el());
};
const timers: { fn: () => void; ms: number }[] = [];
const origSet = globalThis.setTimeout;
(globalThis as any).setTimeout = ((fn: () => void, ms: number) => {
  timers.push({ fn, ms });
  return timers.length as any;
}) as any;
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
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const view = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  renderDbView: () => void;
};
describe("the table-list grep debounce vs an unmounted view (SPEC §data)", () => {
  it("the 300ms callback firing after the view unmounted is a silent no-op, not a TypeError", () => {
    mountDbView();
    view.renderDbView();
    expect(dbIsMounted(), "the view is mounted").toBe(true);
    dbConnState().conns = [dbConn("c", "mysql")];
    dbConnState().conn = "c";
    const pane = resolveId("pane");
    const grep = resolveId("dbGrep");
    expect(pane.oninput, "the pane carries the delegated input listener").toBeTruthy();
    grep.value = "abc";
    pane.oninput({ target: grep }); // SPEC §panel.toolchain: one delegated listener; the target carries the box
    const fired = timers.filter((t) => t.ms === 300);
    expect(fired.length, "the input scheduled its 300ms debounce").toBeGreaterThan(0);
    const cb = fired[fired.length - 1].fn;
    // The unmount resets the record and clears the mounted flag; the pending debounce must
    // survive that without throwing AND without writing a filter nobody can see.
    unmountDbView();
    expect(() => cb(), "the late callback is a no-op").not.toThrow();
    expect(dbConnState().grep, "and it wrote nothing into the reset record").toBe("");
  });

  it("with the view still mounted the same callback still applies the grep", () => {
    mountDbView();
    view.renderDbView();
    dbConnState().conns = [dbConn("c", "mysql")];
    dbConnState().conn = "c";
    const pane = resolveId("pane");
    const grep = resolveId("dbGrep");
    grep.value = "xyz";
    pane.oninput({ target: grep });
    const cb = timers[timers.length - 1].fn;
    expect(() => cb()).not.toThrow();
    expect(dbConnState().grep, "the debounce still applies the grep while mounted").toBe("xyz");
  });
});

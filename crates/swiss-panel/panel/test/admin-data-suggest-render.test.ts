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
import { dbConn, mountDbView, unmountDbView } from "../src/db-state.js";

/* DOM-stub technique as admin-data-redis-celledit.test.ts: the suggest list renders through
   util's el()/$, so the globals must exist before the module graph evaluates. fetch is
   stubbed with the completion route's reply shape. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, id: "", className: "", title: "", value: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    addEventListener() {}, removeEventListener() {}, focus() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 10, height: 10 }),
    querySelector: () => null, querySelectorAll: () => [],
    setAttribute() {}, getAttribute: () => "",
  };
  // setting .id registers the node, so $(id) finds mounted boxes the way a real DOM would
  Object.defineProperty(n, "id", { get: () => n._id, set: (v: string) => { n._id = v; byId[v] = n; } });
  return n;
};
const byId: Record<string, Stub> = {};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), hidden: false,
    createElement: (t: string) => el(t), createTextNode: (s: string) => ({ text: s }),
    getElementById: (id: string) => (byId[id] ||= el()),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener: () => {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  fetch: async () => ({ ok: true, json: async () => ({ items: [{ label: "users", kind: "table", detail: "table" }] }) }),
});

const mod = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-suggest.ts")).href
) as { dbSuggestOnInput: () => void; dbSuggestHide: () => void };

describe("the suggest list render (SPEC §data.completion)", () => {
  it("a reply draws its rows — not an empty positioned box", async () => {
    // Regression, caught live: render called dbSuggestHide() first, which clears the item
    // list, then drew the list — every box reached the screen with zero rows.
    unmountDbView();
    mountDbView();
    Object.assign(dbConn(), { conn: "pg", conns: [{ name: "pg", dialect: "pg" }], tables: ["users"] });
    const wrap = el();
    byId.dbSuggest = undefined as unknown as Stub;
    const ta: any = el("textarea");
    ta.closest = () => ({ className: "db-sql-wrap", ...wrap, appendChild: wrap.appendChild, removeChild: wrap.removeChild, clientHeight: 400 });
    ta.value = "SELECT * FROM us";
    ta.selectionStart = 15;
    mod.dbSuggestOnInput.call(ta);
    await new Promise(r => setTimeout(r, 400)); // debounce 150ms + stubbed fetch
    const box = byId.dbSuggest;
    expect(box, "the box mounted").toBeTruthy();
    expect(box.children.length, "the reply's rows are in it").toBe(1);
    mod.dbSuggestHide();
  });
});

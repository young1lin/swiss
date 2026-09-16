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

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts): browser ES modules
   need the globals stubbed before they will evaluate under Node. getElementById keeps ONE
   stub per id, so a test can hold the same node the wiring will look up later — the editor
   lands in $("dbGridWrap") exactly as on a real page. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
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
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => {} });
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
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const mod = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-browsers.js")).href
) as { dbRenderRedisValue: (wrap: Stub) => void };
const util = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "util.js")).href
) as { state: { db: Record<string, any> } };

function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}

describe("the typed hash table's inline editor (docs/22 W3.3)", () => {
  it("buffers an edited existing value as an update, not as insert #0", () => {
    // Regression, caught live on 19998: the existing-row dblclick once passed insertIdx 0,
    // and save() read 0 >= 0 as "this is an insert row" — b.inserts[0] was undefined, so
    // the edit silently vanished and Commit had nothing to post.
    const wrap = el();
    byId.dbGridWrap = wrap;
    util.state.db = { conn: "r", conns: [{ name: "r", dialect: "redis" }], redisKey: "h", redisValue: { key: "h", type: "hash", value: { f1: "one" }, length: 1 }, redisEdits: null };
    mod.dbRenderRedisValue(wrap);
    const valueTd = find(wrap, (n) => typeof n.ondblclick === "function" && n.textContent === "one")[0];
    expect(valueTd, "the value cell carries a dblclick").toBeTruthy();
    valueTd.ondblclick();
    const editor = find(wrap, (n) => n.className === "db-inline-edit")[0];
    expect(editor, "the inline editor opened").toBeTruthy();
    editor.value = "ONE";
    editor.onkeydown({ key: "Enter", preventDefault: () => {}, stopPropagation: () => {} });
    expect(util.state.db.redisEdits.updates, "the edit landed in updates").toEqual({ f1: "ONE" });
    expect(util.state.db.redisEdits.inserts, "no phantom insert row").toEqual([]);
  });
});

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
import { dbConn, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";

/* SPEC §data.tabs: the CSV import sheet's body and foot were nested INSIDE sheet-head (a
 *  missing paren on the h2 line), so .sheet-head's band rules laid out the entire dialog on
 *  every dialect — the "broken import styling" on both mysql and pg. This pins the sheet
 *  idiom's shape: head, body, foot are SIBLINGS under .sheet. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) {
      // Real-DOM semantics: appending a fragment moves its children and empties it.
      if (c.tag === "#document-fragment") { c.children.forEach((k: Stub) => { n.children.push(k); }); c.children = []; return c; }
      n.children.push(c); return c;
    },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    setAttribute(k: string, v: string) { if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => {} });
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
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const mod = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-csv.ts")).href
) as { dbOpenImport: () => void };

function openOnTable(): void {
  byId.sheet = el();
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), { conn: "m", conns: [{ name: "m", dialect: "mysql" }] });
  const t = freshTab("table");
  dbTabs()[0] = t;
  t.table = "events";
  t.schema = null;
  t.data = {
    table: "events", schema: null,
    columns: [{ name: "id", type: "int" }, { name: "name", type: "text" }],
    rows: [], primaryKey: ["id"], editable: true,
  } as any;
  mod.dbOpenImport();
}

describe("the CSV import sheet's structure (SPEC §data.tabs)", () => {
  it("head, body and foot are siblings under .sheet — the whole dialog is not laid out by the head band", () => {
    openOnTable();
    expect(byId.sheet.hidden, "the sheet opened").toBe(false);
    const sheet = byId.sheet.children[0];
    expect(sheet.className).toBe("sheet");
    const classes = sheet.children.map((c: Stub) => c.className);
    expect(classes).toEqual(["sheet-head", "sheet-body", "sheet-foot"]);
  });

  it("the head carries only its title; the mode switch and inputs live in the body", () => {
    openOnTable();
    const sheet = byId.sheet.children[0];
    const head = sheet.children[0];
    expect(head.children).toHaveLength(1);
    expect(head.children[0].tag).toBe("h2");
    const body = sheet.children[1];
    const ids: string[] = [];
    const walk = (n: Stub): void => { if (n.id) ids.push(n.id); (n.children || []).forEach(walk); };
    walk(body);
    expect(ids).toContain("dbImpMode");
    expect(ids).toContain("dbImpFile");
    expect(ids).toContain("dbImpText");
  });
});

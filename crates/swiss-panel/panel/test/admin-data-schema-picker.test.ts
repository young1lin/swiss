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
import { dbConn as dbConnState } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";

/* SPEC §data kept a pg-only schema PICKER above the grep box; SPEC §data.tabs kills it
 *  for every kind: a schema is a VISIBLE band in the tree now, and a dropdown beside it
 *  would be two mechanisms saying one thing. The suite keeps the B7 concern (nothing
 *  schema-shaped may linger for a non-pg connection, where it read as a redis page offering
 *  a schema dropdown) and strengthens it: the select exists for NO connection at all. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, value: "",
    className: "", id: "", title: "", type: "", selectedIndex: -1,
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false,
    closest(sel: string) { return sel.charAt(0) === "#" && n.id === sel.slice(1) ? n : null; },
    setAttribute(k: string, v: string) { if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {}, dispatchEvent: () => true,
    focus() {}, blur() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
    querySelector: () => null, querySelectorAll: () => [],
    insertBefore(c: Stub) { n.children.push(c); return c; },
  };
  Object.defineProperty(n, "textContent", {
    get: (): string => (n as any)._text ?? "",
    set: (v: string) => { if (v === "") n.children = []; (n as any)._text = v; },
  });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    // Auto-create keeps module-graph import-time lookups alive; dbSchema never auto-creates —
    // a leftover picker would have to come from the renderer, which is the whole subject.
    getElementById: (id: string) => (id === "dbSchema" ? byId.dbSchema : (byId[id] ||= el())),
    createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
    createDocumentFragment: () => el("#document-fragment"),
    createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
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
  fetch: () => new Promise(() => {}),
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const view = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  renderDbTables: () => void;
};

/** Mount-like sidebar: the connection select and the tree's containers, nothing else. */
function sidebar(): void {
  const d = dbConnState();
  d.conns = [
    dbConn("pgc", "pg"),
    dbConn("myc", "mysql"),
    dbConn("rc", "redis"),
  ];
  d.conn = "pgc";
  d.tables = []; d.treeShown = {}; d.tablesTotal = 0; d.more = false; d.grep = "";
  d.schemaFilter = ""; d.sort = "name"; d.sortDir = "asc";
  d.redis = null;
  d.gridCfg = { widths: {}, hidden: [] };
  byId.dbConn = el("select");
  byId.dbTables = el("div");
  byId.dbTablesPager = el("div");
}

describe("no connection kind carries a schema select (SPEC §data, superseded by SPEC §data.tabs)", () => {
  it("a redis connection renders no schema select — hidden-with-options reads as a dropdown", () => {
    sidebar();
    dbConnState().conn = "rc";
    view.renderDbTables();
    expect(byId.dbSchema, "the select never exists on redis").toBeUndefined();
  });

  it("a mysql connection renders none either — one database, one visible tree", () => {
    sidebar();
    dbConnState().conn = "myc";
    view.renderDbTables();
    expect(byId.dbSchema, "the select never exists on mysql").toBeUndefined();
  });

  it("a pg connection renders none either — a schema is a BAND now, not a dropdown pick", () => {
    sidebar();
    const d = dbConnState();
    d.tables = [{ schema: "public", name: "t1" }, { schema: "app", name: "t2" }];
    view.renderDbTables();
    expect(byId.dbSchema, "the picker is gone for pg too").toBeUndefined();
    // ...and the information it held is on screen instead: one band per schema.
    const text = (n: Stub): string => (n.textContent || "") + n.children.map(text).join("");
    const bands = byId.dbTables.children.map((c: Stub) => text(c));
    expect(bands.join("|"), "schema bands replace the picker").toContain("public");
    expect(bands.join("|"), "every schema gets its band").toContain("app");
  });
});

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
import { dbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";

/* docs/22 closeout B7: the schema picker belongs to ONE connection kind. A redis or mysql
   connection must not carry it at all — a hidden native select still shows up in automation
   accessibility trees as a live "Schema" button holding the PREVIOUS pg pick, which read as
   a redis page offering a schema dropdown. Same DOM-stub technique as the other panel suites. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, value: "",
    textContent: "", className: "", id: "", title: "", type: "", selectedIndex: -1,
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    setAttribute() {}, getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {}, dispatchEvent: () => true,
    focus() {}, blur() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
    querySelector: () => null, querySelectorAll: () => [],
    insertBefore(c: Stub) { n.children.push(c); return c; },
  };
  Object.defineProperty(n, "innerHTML", {
    get: () => "", set: (v: string) => { if (v === "") n.children = []; },
  });
  return n;
};
const byId: Record<string, Stub> = {};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    // Auto-create keeps module-graph import-time lookups ($("addBtn")…) alive, but dbSchema
    // must NOT auto-create: this suite's whole subject is that element's presence and absence.
    getElementById: (id: string) => (id === "dbSchema" ? byId.dbSchema : (byId[id] ||= el())),
    createElement: (t: string) => el(t), createTextNode: (s: string) => ({ text: s }),
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

/** Mount-like sidebar: a #dbConn select and the skeleton's #dbSchema select, whose remove()
 *  does what the real DOM does — the element is GONE from the document afterwards. */
function sidebar(): void {
  const d = dbView();
  d.conns = [
    dbConn("pgc", "pg"),
    dbConn("myc", "mysql"),
    dbConn("rc", "redis"),
  ];
  d.conn = "pgc"; d.table = null; d.schema = null; d.data = null;
  d.tables = []; d.tablesPage = 0; d.tablesTotal = 0; d.more = false; d.grep = "";
  d.schemaFilter = ""; d.sort = "name"; d.sortDir = "asc";
  d.redis = null; d.redisKey = null; d.filters = [];
  d.gridCfg = { widths: {}, hidden: [] }; d.sel = {};
  byId.dbConn = el("select");
  // The sidebar container: a re-created #dbSchema is inserted after the connection picker,
  // and an id'd child landing here becomes visible to getElementById — the real DOM contract.
  const side = el("div");
  (byId.dbConn as any).parentNode = side;
  (side as any).insertBefore = (c: Stub) => { side.children.push(c); if (c.id) byId[c.id] = c; return c; };
  byId.dbSchema = el("select");
  byId.dbSchema.remove = () => { delete byId.dbSchema; };
  byId.dbTables = el("div");
  byId.dbTablesPager = el("div");
}

describe("the schema picker follows the connection kind (docs/22 closeout B7)", () => {
  it("a redis connection carries no schema select at all — hidden-with-options reads as a dropdown", () => {
    sidebar();
    dbView().conn = "rc";
    view.renderDbTables();
    expect(byId.dbSchema, "the select leaves the DOM on redis").toBeUndefined();
  });

  it("a mysql connection carries none either — one database, no picker (docs/22 W1.1)", () => {
    sidebar();
    dbView().conn = "myc";
    view.renderDbTables();
    expect(byId.dbSchema, "the select leaves the DOM on mysql").toBeUndefined();
  });

  it("a pg connection keeps the picker, unhidden, with its options filled", () => {
    sidebar();
    const d = dbView();
    d.tables = [{ schema: "public", name: "t1" }, { schema: "app", name: "t2" }];
    view.renderDbTables();
    const sel = byId.dbSchema;
    expect(sel, "the picker stays for pg").toBeTruthy();
    expect(sel.hidden, "and is shown").toBe(false);
    expect(sel.children.length, "All schemas + public + app").toBe(3);
  });

  it("pg -> redis -> pg round-trip: the picker comes back legal and wired, with no other connection's pick", () => {
    sidebar();
    const d = dbView();
    d.tables = [{ schema: "public", name: "t1" }];
    view.renderDbTables();
    d.schemaFilter = "public"; // a pick made against the pg catalog
    d.conn = "rc"; // the switch: connection kind changes under the picker
    view.renderDbTables();
    expect(byId.dbSchema, "redis dropped it").toBeUndefined();
    d.conn = "pgc"; // back on pg the picker must return, selectable and empty of residue
    view.renderDbTables();
    const sel = byId.dbSchema;
    expect(sel, "the picker is re-created for pg").toBeTruthy();
    expect(typeof sel.onchange, "it is wired again").toBe("function");
  });
});

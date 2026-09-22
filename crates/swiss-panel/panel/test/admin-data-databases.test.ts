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

/* docs/43 M3: the database selector's contract. The catalog order, the disabled rows, and
 * the two switches (database, connection) are the whole feature's panel half — a selector
 * that listed system databases first, or let a not-browsable row be picked, would be worse
 * than no selector at all. Pinned here as data, without a live menu. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) {
      if (c.tag === "#document-fragment") { c.children.forEach((k: Stub) => { k.parentNode = n; n.children.push(k); }); c.children = []; return c; }
      c.parentNode = n; n.children.push(c); return c;
    },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() { const p = n.parentNode as Stub | undefined; if (p) p.children = p.children.filter((x: Stub) => x !== n); },
    contains: () => false, closest: () => null,
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
    getElementById: (id: string) => byId[id] || (byId[id] = el()),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener() {},
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
const mod = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  dbSortDatabases: (list: any[]) => any[];
  dbDatabaseMenuItems: (list: any[], selected: string, pick: (n: string) => void) => any[];
  dbSwitchDatabase: (name: string) => void;
};

// The live mysql catalog's shape, shrunk to what the contract reads (docs/43 M3 4.2).
const CATALOG = [
  { name: "acme_app_dev", primary: true, browsable: true, system: false, tables: 1712 },
  { name: "sys", primary: false, browsable: true, system: true, tables: 101 },
  { name: "acme_app_uat", primary: false, browsable: true, system: false, tables: 9 },
  { name: "information_schema", primary: false, browsable: false, system: true,
    reason: "not a database on this connection: information_schema" },
  { name: "zdata", primary: false, browsable: true, system: false, tables: 4 },
];

function mountMysql(): void {
  byId.dbConnRow = el("button");
  byId.dbDatabaseRow = el("button");
  byId.dbGridWrap = el();
  byId.dbTables = el();
  byId.dbSql = el("textarea");
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), {
    conn: "mysql", conns: [{ name: "mysql", dialect: "mysql", state: "running", group: "" }],
    databases: CATALOG.slice(),
  });
  const t = freshTab("table");
  t.table = "orders"; t.schema = "acme_app_dev";
  dbTabs().length = 0;
  dbTabs().push(t, freshTab("sql"));
}

describe("the database selector's order (docs/43 M3 4.3.2)", () => {
  it("primary first, the rest by name, system last - whatever the server sent", () => {
    const out = mod.dbSortDatabases(CATALOG).map((x) => x.name);
    expect(out).toEqual(["acme_app_dev", "acme_app_uat", "zdata", "information_schema", "sys"]);
  });
});

describe("the selector's menu rows (docs/43 M3 4.3.2)", () => {
  it("a not-browsable row is disabled and carries the server's reason as its title", () => {
    const rows = mod.dbDatabaseMenuItems(CATALOG, "acme_app_dev", () => {});
    const info = rows.find((r) => r.label && r.label.startsWith("information_schema"));
    expect(info, "the row is listed, not hidden").toBeTruthy();
    expect(info.disabled, "it cannot be picked").toBe(true);
    expect(info.title, "the title is the server's reason, verbatim").toBe(
      "not a database on this connection: information_schema");
  });
  it("the selected database marks on and cannot re-pick; a browsable row is enabled", () => {
    const rows = mod.dbDatabaseMenuItems(CATALOG, "acme_app_dev", () => {});
    // The drawer reshape: label is the bare NAME, the table count rides in meta (painted
    // dim at the row's end) instead of being interleaved into the label.
    const primary = rows.find((r) => r.label === "acme_app_dev");
    expect(primary.on).toBe(true);
    expect(primary.disabled, "the row already in effect does not re-fire the switch").toBe(true);
    expect(primary.meta, "the count is meta now, not part of the name").toBe("1,712");
    const uat = rows.find((r) => r.label === "acme_app_uat");
    expect(uat.on).toBe(false);
    expect(uat.disabled).toBe(false);
  });
  it("the rows come with the group headings: primary, others, system", () => {
    const heads = mod.dbDatabaseMenuItems(CATALOG, "", () => {})
      .filter((r) => r.heading).map((r) => r.label);
    expect(heads.length).toBe(3);
    expect(heads[0]).toBe("Primary");
    expect(heads[2]).toBe("System");
  });
});

describe("switching databases (docs/43 M3 4.3.3)", () => {
  it("the selected database lands in state, the strip drops to the placeholder, filters reset", () => {
    mountMysql();
    const d = dbConn();
    d.grep = "tsys_";
    d.sort = "rows"; d.sortDir = "desc";
    mod.dbSwitchDatabase("acme_app_uat");
    expect(d.database, "the switch records the chosen database").toBe("acme_app_uat");
    expect(d.grep, "a table-list search never crosses databases").toBe("");
    expect(d.sort).toBe("name");
    expect(d.sortDir).toBe("asc");
    expect(d.tables, "the old database's page does not survive the switch").toEqual([]);
    expect(d.tablesPage).toBe(0);
    const tabs = dbTabs();
    expect(tabs.length, "the strip closes with the connection - placeholder only").toBe(1);
    expect(tabs[0].kind).toBe("table");
    expect((tabs[0] as { table: string | null }).table, "the placeholder carries no table").toBe(null);
  });
  it("picking the database already in effect changes nothing", () => {
    mountMysql();
    const d = dbConn();
    d.database = "acme_app_uat";
    d.tables = [{ schema: "acme_app_uat", name: "orders" }];
    mod.dbSwitchDatabase("acme_app_uat");
    expect(d.tables.length, "the loaded page stays").toBe(1);
    expect(dbTabs().length, "open tabs stay").toBe(2);
  });
  it("nothing picked yet: re-picking the PRIMARY is a stay, not a strip-wide drop", () => {
    mountMysql();
    const d = dbConn();
    d.database = ""; // the configured default — the primary row stands for it
    d.tables = [{ schema: "acme_app_dev", name: "orders" }];
    mod.dbSwitchDatabase("acme_app_dev");
    expect(d.database, "still nothing picked").toBe("");
    expect(d.tables.length, "the loaded page stays").toBe(1);
    expect(dbTabs().length, "open tabs stay").toBe(2);
  });
});

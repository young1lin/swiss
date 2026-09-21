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

/* docs/43 M4 = docs/42 T4: the toolbar draws ONLY the active tab's controls — one primary
 * action plus an overflow — and the pager/page-size live on a separate status line. The
 * hard gate is the .btn budget: at most ONE non-icon .btn per toolbar, in every tab kind
 * (swiss-ui-design rule 4, as a machine check instead of a screenshot). */
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
    setAttribute(k: string, v: string) { if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; if (k === "class") n.className = v; },
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
const grid = await import(pathToFileURL(join(here, "data-grid.js")).href) as {
  renderDbToolbar: () => void;
  renderDbStatus: () => void;
};
const structure = await import(pathToFileURL(join(here, "data-structure.js")).href) as {
  DB_TABS: { id: string }[];
  dbPaneToTab: (p: string) => string;
  dbTabToPane: (t: string) => string;
};

function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}

function mount(): void {
  byId.dbHead = el("div");
  byId.dbStatus = el("div");
  byId.dbGridWrap = el("div");
  byId.dbConsole = el("div");
  byId.dbBar = el("div");
  byId.dbFilters = el("div");
  byId.dbSql = el("textarea");
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), {
    conn: "mysql",
    conns: [{ name: "mysql", dialect: "mysql", state: "running", group: "", label: "mysql" }],
    databases: [{ name: "acme_app_dev", primary: true, browsable: true, system: false }],
  });
}

/** The toolbar's non-icon .btn count — the M4 gate. */
function flatBtns(): Stub[] {
  const head = byId.dbHead;
  return find(head, (n) => n.tag === "button"
    && String(n.className).includes("btn")
    && !String(n.className).includes("icon"));
}

const PAGE = (editable: boolean, editNote?: string): Record<string, unknown> => ({
  schema: "acme_app_dev", table: "orders", total: 550968, rows: new Array(50).fill({}),
  cols: [], primaryKey: ["id"], editable, editNote, nextPage: true,
});

function openTable(editable: boolean, editNote?: string): void {
  mount();
  const t = freshTab("table");
  t.table = "orders"; t.schema = "acme_app_dev";
  t.data = PAGE(editable, editNote) as never;
  dbTabs().length = 0;
  dbTabs().push(t);
  grid.renderDbToolbar();
  grid.renderDbStatus();
}

describe("the toolbar draws only the active tab's controls (docs/43 M4)", () => {
  it("a table tab: at most ONE non-icon .btn (the row buffer), plus the overflow", () => {
    openTable(true);
    const flat = flatBtns();
    expect(flat.length, "the primary action is the only flat button").toBeLessThanOrEqual(1);
    expect(byId.dbHead.children.some((c: Stub) => find(c, (n) => n.dataset && n.dataset.tb === "addrow").length > 0)
      || find(byId.dbHead, (n) => n.dataset && n.dataset.tb === "addrow").length > 0).toBe(true);
  });
  it("a read-only table tab (a foreign database) has NO flat button at all", () => {
    openTable(false, "Read-only: ops_dev is not this connection's configured database (acme_app_dev).");
    expect(flatBtns().length).toBe(0);
  });
  it("an sql tab: no pager, no row buffer — Run is the one flat button", () => {
    mount();
    const s = freshTab("sql");
    s.sqlText = "select 1";
    dbTabs().length = 0;
    dbTabs().push(s);
    grid.renderDbToolbar();
    const flat = flatBtns();
    expect(flat.length).toBeLessThanOrEqual(1);
    const head = find(byId.dbHead, (n) => n.dataset && (n.dataset.pg || n.dataset.tb === "addrow"));
    expect(head.length, "no pager and no +Row on an sql toolbar").toBe(0);
  });
  it("a redis key tab: the primary action follows the value's type; string has none", () => {
    mount();
    Object.assign(dbConn(), {
      conn: "redis",
      conns: [{ name: "redis", dialect: "redis", state: "running", group: "", label: "redis" }],
    });
    const k = freshTab("key");
    k.redisKey = "h";
    k.redisValue = { key: "h", type: "hash", value: {}, length: 1, ttl: -1 } as never;
    dbTabs().length = 0;
    dbTabs().push(k);
    grid.renderDbToolbar();
    const adds = find(byId.dbHead, (n) => n.dataset && n.dataset.radd === "");
    expect(adds.length, "a hash gets its + Field action").toBe(1);
    (k.redisValue as unknown as { type: string }).type = "string";
    k.redisValue = { key: "s", type: "string", value: "x", length: 1, ttl: -1 } as never;
    // The stub's textContent is a plain property (setting it does not detach children the
    // way the real DOM does), so the repaint's wipe is spelled out here.
    byId.dbHead.children.length = 0;
    grid.renderDbToolbar();
    expect(find(byId.dbHead, (n) => n.dataset && n.dataset.radd === "").length,
      "a string has no primary action").toBe(0);
  });
  it("an activity tab: Refresh is the one flat button", () => {
    mount();
    const a = freshTab("activity");
    dbTabs().length = 0;
    dbTabs().push(a);
    grid.renderDbToolbar();
    expect(flatBtns().length).toBeLessThanOrEqual(1);
    expect(find(byId.dbHead, (n) => n.dataset && n.dataset.actrefresh === "").length).toBe(1);
  });
});

describe("the six panes fold into four tabs (docs/43 M4)", () => {
  it("DB_TABS is exactly Data / Form / Structure / DDL", () => {
    expect(structure.DB_TABS.length).toBe(4);
    expect(structure.DB_TABS.map((x) => x.id)).toEqual(["data", "form", "structure", "ddl"]);
  });
  it("the three catalog panes all stand for the Structure tab; it opens on Columns", () => {
    expect(structure.dbPaneToTab("columns")).toBe("structure");
    expect(structure.dbPaneToTab("indexes")).toBe("structure");
    expect(structure.dbPaneToTab("fks")).toBe("structure");
    expect(structure.dbPaneToTab("data")).toBe("data");
    expect(structure.dbTabToPane("structure")).toBe("columns");
  });
});

describe("the status line (docs/43 M4)", () => {
  it("says the server's own read-only sentence when editable is false", () => {
    openTable(false, "Read-only: ops_dev is not this connection's configured database (acme_app_dev).");
    const note = find(byId.dbStatus, (n) => String(n.className).includes("db-status-note"))
      .map((n) => n.textContent).join(" ");
    expect(note).toContain("Read-only: ops_dev is not this connection's configured database (acme_app_dev).");
  });
  it("carries the pager and the connection name on an editable table", () => {
    openTable(true);
    const pager = find(byId.dbStatus, (n) => n.dataset && (n.dataset.pg === "prev" || n.dataset.pg === "next"));
    expect(pager.length).toBe(2);
    const size = find(byId.dbStatus, (n) => n.dataset && n.dataset.tb === "pagesize");
    expect(size.length).toBe(1);
    const conn = find(byId.dbStatus, (n) => String(n.className).includes("db-status-conn"))
      .map((n) => n.textContent).join(" ");
    expect(conn).toContain("mysql");
  });
});

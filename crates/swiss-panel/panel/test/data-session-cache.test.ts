// @vitest-environment happy-dom
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

/* docs/47: one parked session per connection. The owner's report was "I clicked MySQL, then
   Redis, then back, and everything MySQL had was gone": the switch rebuilt the connection
   record and the object strip from nothing, and a page leave did the same. These pins hold
   the promises the park makes - the session comes back whole and at once, a buffered write
   is never dropped by a switch, the guards count every session, and the return fetch is
   quiet: it repaints only when the server's answer moved. They drive the real view against
   a real DOM with a routed fetch. */
import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbActiveIndex, dbConn as rec, dbIsMounted, dbTab, dbTabs, unmountDbView } from "../src/db-state.js";
import { dbConn, dbCol, dbPage } from "./db-fixtures.js";
import type { DbTableTab } from "../src/types/state.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* The routed server. `conns` is what /api/db lists; `rows` is the orders table as the server
   holds it right now. */
let conns = [dbConn("shop", "mysql"), dbConn("cache", "redis"), dbConn("audit", "mysql")];
let rows: Record<string, unknown>[] = [{ id: 1, total: 10 }, { id: 2, total: 20 }];
/* The redis keyspace of "cache", as SCAN pages (the cursor is the page index), and whether the
   walk on screen had been dropped whenever the server was asked for keys. */
let keyPages: string[][] = [["k1"]];
const walkDropped: boolean[] = [];
const urls: string[] = [];
function answer(url: string, init?: { body?: unknown }): unknown {
  const u = new URL(url, "http://x");
  const p = u.pathname;
  if (p === "/api/db") return { connections: conns };
  if (p.endsWith("/tables")) {
    const tables = [{ schema: "demo", name: "orders" }, { schema: "demo", name: "customers" }];
    return { tables, total: tables.length, page: 0, limit: 5000, more: false };
  }
  if (p.endsWith("/databases")) {
    if (!p.includes("/cache/")) return { primary: null, current: null, databases: [] };
    const tables = keyPages.flat().length;
    return { primary: "0", current: "0", databases: [{ name: "0", primary: true, browsable: true, system: false, tables, reason: null }] };
  }
  if (p.endsWith("/command")) {
    const cmd = String(JSON.parse(String(init?.body || "{}")).command || "").split(/\s+/);
    if (cmd[0].toUpperCase() === "SET") keyPages[keyPages.length - 1].push(cmd[1]);
    return { reply: "OK", elapsedMs: 0 };
  }
  if (p.endsWith("/data")) {
    return dbPage({
      schema: "demo", table: u.searchParams.get("table") || "t",
      columns: [dbCol("id", { isPrimaryKey: true }), dbCol("total")],
      rows: rows.map((r) => ({ ...r })), total: rows.length, primaryKey: ["id"], editable: true,
    });
  }
  if (p.endsWith("/schema")) return { schema: "demo", table: u.searchParams.get("table"), columns: [], indexes: [], foreignKeys: [] };
  if (p.endsWith("/keys")) {
    walkDropped.push(rec().redis === null);
    const at = Number(u.searchParams.get("cursor") || "0");
    const last = at >= keyPages.length - 1;
    return {
      keys: (keyPages[at] || []).map((key) => ({ key, type: "string", ttl: -1 })),
      cursor: last ? "0" : String(at + 1), done: last, total: keyPages.flat().length,
    };
  }
  if (p.endsWith("/key")) return { key: u.searchParams.get("key"), type: "string", ttl: -1, value: "v" };
  return {};
}
const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, {
  fetch: (url: unknown, init?: { body?: unknown }) => {
    urls.push(String(url));
    const body = answer(String(url), init);
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(body) });
  },
});
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});

// happy-dom has no dialogs; the guards under test ask through confirm().
Object.assign(window, { confirm: () => true });
document.body.innerHTML = shellSkeleton();
const page = await import("../src/views/data.js");
const view = await import("../src/data-view.js");
const tabs = await import("../src/data-tabs.js");
const state = await import("../src/db-state.js") as Record<string, unknown>;
const browsers = await import("../src/data-browsers.js");
const sql = await import("../src/data-sql.js");

const flush = async (): Promise<void> => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); };
const front = (): DbTableTab => { const t = dbTab(); if (t.kind !== "table") throw new Error("not a table tab"); return t; };
function dirty(t: DbTableTab): void { t.updates = { "1": { pk: { id: 1 }, changes: { total: 99 } } }; }

beforeEach(async () => {
  page.unmount();
  unmountDbView(); // the full reset: the park goes too, so every case starts from nothing
  conns = [dbConn("shop", "mysql"), dbConn("cache", "redis"), dbConn("audit", "mysql")];
  rows = [{ id: 1, total: 10 }, { id: 2, total: 20 }];
  keyPages = [["k1"]];
  walkDropped.length = 0;
  urls.length = 0;
  vi.restoreAllMocks();
  await page.mount();
  await flush();
});

describe("switching away and back brings the session back whole (docs/47 D1, D2)", () => {
  it("the strip, the front tab, its coordinates, its buffered writes and the catalog all return", async () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "demo" });
    await flush();
    tabs.dbOpenTab({ kind: "table", table: "customers", schema: "demo" });
    await flush();
    tabs.dbActivateTab(0);
    await flush();
    front().filters = [{ column: "total", op: ">", value: "5" }];
    dirty(front());
    const ask = vi.spyOn(window, "confirm").mockReturnValue(true);

    await view.dbSwitchConn("cache");
    await flush();
    expect(rec().conn).toBe("cache");
    expect(dbTab().kind, "redis starts on its own family's tab").toBe("key");

    await view.dbSwitchConn("shop");
    expect(dbTabs().map((t) => (t.kind === "table" ? t.table : t.kind))).toEqual(["orders", "customers"]);
    expect(dbActiveIndex()).toBe(0);
    expect(front().filters).toEqual([{ column: "total", op: ">", value: "5" }]);
    expect(Object.keys(front().updates), "the buffered write rode the park").toEqual(["1"]);
    expect(rec().tables.map((t) => t.name), "the catalog is there before any fetch").toEqual(["orders", "customers"]);
    expect(ask, "nothing was dropped, so nothing was asked").not.toHaveBeenCalled();
  });

  it("a connection seen for the first time starts empty; the row density and the history carry", async () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "demo" });
    await flush();
    front().pageSize = 200;
    rec().history = ["select 1"];
    await view.dbSwitchConn("audit");
    await flush();
    expect(dbTabs().length).toBe(1);
    expect(front().table, "a fresh placeholder").toBeNull();
    expect(front().pageSize).toBe(200);
    expect(rec().history).toEqual(["select 1"]);
    expect(rec().conns.map((c) => c.name), "the registry list is the page's, not the session's").toEqual(["shop", "cache", "audit"]);
  });
});

describe("the guards count every session (docs/47 D3, D4)", () => {
  it("a write parked on another connection still counts; a page leave drops writes, keeps the rest", async () => {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "demo" });
    await flush();
    dirty(front());
    await view.dbSwitchConn("cache");
    await flush();
    expect(page.hasPendingChanges(), "beforeunload must hear about the parked write").toBe(true);
    expect(view.dbPendingAll()).toBe(1);

    page.unmount();
    expect(dbIsMounted()).toBe(false);
    expect(page.hasPendingChanges()).toBe(false);
    await page.mount();
    await flush();
    expect(rec().conn, "the page comes back on the connection it left").toBe("cache");
    await view.dbSwitchConn("shop");
    expect(front().table, "the session itself survived the leave").toBe("orders");
    expect(Object.keys(front().updates), "the confirmed leave dropped the write").toEqual([]);
  });

  it("a connection that left the registry takes its parked session with it", async () => {
    await view.dbSwitchConn("cache");
    await flush();
    const parked = state.dbParkedNames as () => string[];
    expect(parked()).toContain("shop");
    conns = [dbConn("cache", "redis")];
    await page.refresh();
    await flush();
    expect(parked()).not.toContain("shop");
  });
});

describe("the park is capped, and a session holding writes is never the one to go (docs/47 D7)", () => {
  it("the least recently used clean session goes; a dirty one stays", async () => {
    conns = Array.from({ length: 11 }, (_, i) => dbConn("c" + i, "mysql"));
    await page.refresh();
    await flush();
    await view.dbSwitchConn("c0");
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "demo" });
    await flush();
    dirty(front());
    for (let i = 1; i <= 10; i++) { await view.dbSwitchConn("c" + i); await flush(); }
    const parked = (state.dbParkedNames as () => string[])();
    expect(parked.length).toBeLessThanOrEqual(state.DB_PARKED_MAX as number);
    expect(parked, "the dirty session is kept past the cap").toContain("c0");
    expect(parked, "the oldest clean one went").not.toContain("c1");
  });
});

describe("the return is quiet and incremental (docs/47 D5)", () => {
  async function parkOrders(): Promise<void> {
    tabs.dbOpenTab({ kind: "table", table: "orders", schema: "demo" });
    await flush();
    await view.dbSwitchConn("cache");
    await flush();
    urls.length = 0;
  }

  it("the rows are on screen at once, with no Loading, and the page is re-read at the same coordinates", async () => {
    await parkOrders();
    await view.dbSwitchConn("shop");
    expect(front().loading, "no spinner over rows we already hold").toBe(false);
    expect(front().data?.rows.length).toBe(2);
    expect(document.querySelector("#dbTables .db-hint")?.textContent ?? "", "the tree is not blanked to Loading").not.toMatch(/Loading/);
    await flush();
    expect(urls.some((u) => u.includes("/data?table=orders")), "the page was re-read").toBe(true);
    expect(urls.some((u) => u.includes("/tables")), "and the catalog").toBe(true);
  });

  it("an unchanged answer repaints nothing; a changed one lands", async () => {
    await parkOrders();
    await view.dbSwitchConn("shop");
    const cell = document.querySelector("#dbGridWrap tbody tr");
    expect(cell, "the grid painted from the cache").toBeTruthy();
    await flush();
    expect(cell!.isConnected, "same answer: the painted rows were left alone").toBe(true);

    await view.dbSwitchConn("cache");
    await flush();
    rows = [{ id: 1, total: 11 }, { id: 2, total: 20 }];
    await view.dbSwitchConn("shop");
    await flush();
    expect(front().data?.rows[0].total, "the moved row came back from the server").toBe(11);
  });
});

/* The owner's report: on an empty Redis, "SET test 1" never showed up - not after the refresh
   button, not after leaving the page and coming back. D6 had kept the key walk as paged so More's
   pages would not be lost, and nothing else re-walked it: the list froze at its first answer. */
describe("a Redis key list is re-read, never frozen (docs/47 D6, revised)", () => {
  const keys = (): string[] => (rec().redis?.keys || []).map((k) => k.key);

  it("the refresh re-walks quietly: a key SET elsewhere shows up, and the walk is never dropped", async () => {
    await view.dbSwitchConn("cache");
    await flush();
    expect(keys()).toEqual(["k1"]);
    keyPages = [["k1", "test"]];
    walkDropped.length = 0;
    await page.refresh();
    await flush();
    expect(keys(), "the key SET since the last walk").toEqual(["k1", "test"]);
    expect(document.getElementById("dbTables")?.textContent).toContain("test");
    expect(walkDropped, "the list on screen stayed while the re-walk was out").toEqual([false]);
  });

  it("a return to the page re-walks as deep as More went, and the open key stays open", async () => {
    keyPages = [["a"], ["b"]];
    await view.dbSwitchConn("cache");
    await flush();
    expect(keys()).toEqual(["a"]);
    await browsers.dbLoadKeys(false); // More
    expect(keys()).toEqual(["a", "b"]);
    await browsers.dbLoadRedisValue("a");
    keyPages = [["a"], ["b", "c"]];
    page.unmount();
    await page.mount();
    await flush();
    expect(keys(), "both pages were re-read, not just the first").toEqual(["a", "b", "c"]);
    const t = dbTab();
    expect(t.kind === "key" ? t.redisKey : null).toBe("a");
  });

  it("a console command re-walks the keys and re-reads the database catalog", async () => {
    await view.dbSwitchConn("cache");
    await flush();
    expect(rec().databases?.[0]?.tables).toBe(1);
    tabs.dbOpenTab({ kind: "sql" });
    await flush();
    const st = dbTab();
    if (st.kind !== "sql") throw new Error("not a console tab");
    st.sqlText = "SET test 1";
    await sql.dbRunSql();
    await flush();
    expect(keys()).toEqual(["k1", "test"]);
    expect(rec().databases?.[0]?.tables, "the key count in the database drawer moved too").toBe(2);
  });
});

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

/* SPEC §data.history, §data.console: the owner found a statement run in a PostgreSQL schema listed
   under a MySQL database - the console history was one list per browser. It is the server's now,
   filed by the database a statement reached, and these drive the real view against a routed
   server that files the same way: the ⋯ lists only this database's statements, the History sheet
   pages, searches, clears and loads them back. The second ask rides here too: a MySQL or
   PostgreSQL statement without a LIMIT goes with the console's row limit, 500 unless the operator
   picked another. */
import { afterAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbTab, unmountDbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";
import type { ApiDbHistoryRun } from "../src/types/api.js";

const here = dirname(fileURLToPath(import.meta.url));

function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* The routed server: each connection name reaches a database, two names one of them; a run is
   filed under that database, as history_api.rs files it. */
const TARGETS: Record<string, string> = {
  shop: "shop @ db1:3306", "shop-ro": "shop @ db1:3306", pgdb: "app @ db2:5432", cache: "cache @ localhost:6379 db 0",
};
let seq = 0;
const filed: Record<string, ApiDbHistoryRun[]> = {};
const reads: string[] = [];
const deletes: string[] = [];
const bodies: Array<{ sql?: string; limit?: number }> = [];
function file(conn: string, stmt: string, over: Partial<ApiDbHistoryRun> = {}): void {
  const target = TARGETS[conn];
  (filed[target] = filed[target] || []).push({ id: ++seq, ts: Date.now(), conn, dialect: "mysql", stmt, ok: true, ms: 2, ...over });
}
function answer(url: string, init?: { body?: unknown; method?: string }): { ok: boolean; status: number; body: unknown } {
  const u = new URL(url, "http://x");
  const p = u.pathname;
  const body = JSON.parse(String(init?.body || "{}")) as { sql?: string; command?: string; limit?: number };
  if (p === "/api/db") {
    return { ok: true, status: 200, body: { connections: [dbConn("shop", "mysql"), dbConn("pgdb", "pg"), dbConn("cache", "redis")] } };
  }
  if (p.endsWith("/tables")) return { ok: true, status: 200, body: { tables: [], total: 0, page: 0, limit: 5000, more: false } };
  if (p.endsWith("/databases")) return { ok: true, status: 200, body: { primary: null, current: null, databases: [] } };
  if (p.endsWith("/keys")) return { ok: true, status: 200, body: { keys: [], cursor: "0", done: true, total: 0 } };
  if (p.endsWith("/history")) {
    const target = TARGETS[decodeURIComponent(p.split("/")[3])];
    if (init?.method === "DELETE") {
      deletes.push(p);
      const cleared = (filed[target] || []).length;
      filed[target] = [];
      return { ok: true, status: 200, body: { target, cleared } };
    }
    reads.push(p + u.search);
    const q = (u.searchParams.get("q") || "").toLowerCase();
    const before = Number(u.searchParams.get("before") || Infinity);
    const limit = Number(u.searchParams.get("limit") || 100);
    const runs = (filed[target] || [])
      .filter((r) => r.id < before && (!q || r.stmt.toLowerCase().includes(q)))
      .sort((a, b) => b.id - a.id);
    return { ok: true, status: 200, body: { target, runs: runs.slice(0, limit), more: runs.length > limit } };
  }
  if (p.endsWith("/query") || p.endsWith("/command")) {
    const text = body.sql ?? body.command ?? "";
    bodies.push({ sql: body.sql, limit: body.limit });
    file(decodeURIComponent(p.split("/")[3]), text);
    if (p.endsWith("/command")) return { ok: true, status: 200, body: { reply: "PONG", elapsedMs: 1 } };
    if (text.includes("big")) {
      return { ok: true, status: 200, body: { columns: ["n"], rows: [{ n: 1 }], rowCount: 500, limitApplied: 500, elapsedMs: 3 } };
    }
    return { ok: true, status: 200, body: { columns: ["n"], rows: [{ n: 1 }], rowCount: 1, elapsedMs: 1 } };
  }
  return { ok: true, status: 200, body: {} };
}
const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
Object.assign(globalThis, {
  fetch: (url: unknown, init?: { body?: unknown; method?: string }) => {
    const a = answer(String(url), init);
    return Promise.resolve({ ok: a.ok, status: a.status, json: () => Promise.resolve(a.body) });
  },
});
afterAll(() => {
  if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
  else delete (globalThis as Record<string, unknown>).fetch;
});

Object.assign(window, { confirm: () => true });
document.body.innerHTML = shellSkeleton();
const page = await import("../src/views/data.js");
const view = await import("../src/data-view.js");
const tabs = await import("../src/data-tabs.js");
const sql = await import("../src/data-sql.js");
const hist = await import("../src/data-history.js");

const flush = async (): Promise<void> => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); };

/** Open the console on `conn` with `text` in it. */
async function console_(conn: string, text: string): Promise<void> {
  await view.dbSwitchConn(conn);
  await flush();
  tabs.dbOpenTab({ kind: "sql" });
  await flush();
  const st = dbTab();
  if (st.kind !== "sql") throw new Error("not a console tab");
  st.sqlText = text;
  const ta = document.getElementById("dbSql") as HTMLTextAreaElement;
  ta.value = text;
  ta.setSelectionRange(text.length, text.length);
}
const sheetEl = (): HTMLElement => document.getElementById("sheet") as HTMLElement;
const titles = (): string[] => Array.from(sheetEl().querySelectorAll(".fl-title")).map((n) => n.textContent || "");
const run = (id: number, stmt: string): ApiDbHistoryRun => ({ id, ts: 0, conn: "shop", dialect: "mysql", stmt, ok: true });

beforeEach(async () => {
  page.unmount();
  unmountDbView();
  for (const k of Object.keys(filed)) delete filed[k];
  reads.length = 0;
  deletes.length = 0;
  bodies.length = 0;
  try { localStorage.clear(); } catch (e) { /* none here */ }
  vi.restoreAllMocks();
  await page.mount();
  await flush();
});

describe("the console history is the server's, per database (SPEC §data.history)", () => {
  it("pure: the ⋯ keeps a statement once, at its newest place", () => {
    expect(hist.dbHistoryDistinct([run(5, "SELECT 2"), run(4, " SELECT 1 "), run(3, "SELECT 2"), run(2, ""), run(1, "SELECT 3")], 8))
      .toEqual(["SELECT 2", "SELECT 1", "SELECT 3"]);
    expect(hist.dbHistoryDistinct([run(2, "a"), run(1, "b")], 1)).toEqual(["a"]);
  });

  it("the ⋯ lists what ran on this database, never what ran on another", async () => {
    file("pgdb", "SELECT * FROM pg_stat_activity");
    file("shop", "SELECT * FROM orders");
    await console_("shop", "");
    expect(hist.dbHistoryRecent()).toEqual(["SELECT * FROM orders"]);
    await console_("pgdb", "");
    expect(hist.dbHistoryRecent()).toEqual(["SELECT * FROM pg_stat_activity"]);
    expect(reads.map((r) => r.split("?")[0])).toEqual(["/api/db/shop/history", "/api/db/pgdb/history"]);
  });

  it("a run reads the list again: what just ran is on top", async () => {
    file("shop", "SELECT 1");
    await console_("shop", "SELECT * FROM customers");
    await sql.dbRunSql();
    await flush();
    expect(hist.dbHistoryRecent()).toEqual(["SELECT * FROM customers", "SELECT 1"]);
  });

  it("the sheet: one line a run, newest first, a failure's words under it, the name that ran it when it was another", async () => {
    file("shop", "SELECT 1");
    file("shop-ro", "SELECT * FROM orders", { rows: 10 });
    file("shop", "UPDATE orders SET x = 1", { ok: false, error: "denied" });
    await console_("shop", "");
    await hist.dbHistoryOpen();
    await flush();
    expect(sheetEl().hidden).toBe(false);
    expect(sheetEl().querySelector(".sheet-sub")?.textContent).toBe("shop @ db1:3306");
    expect(titles()).toEqual(["UPDATE orders SET x = 1", "SELECT * FROM orders", "SELECT 1"]);
    expect(sheetEl().querySelector(".fl-detail.bad")?.textContent).toBe("denied");
    const summaries = Array.from(sheetEl().querySelectorAll(".fl-summary")).map((n) => n.textContent);
    expect(summaries[1]).toBe("10 rows · 2 ms · via shop-ro");
    expect(summaries[2], "the console's own name goes unsaid").toBe("2 ms");
  });

  it("a search asks the server; a run opens to its statement and goes back into the console", async () => {
    file("shop", "SELECT 1");
    file("shop", "DELETE FROM carts");
    await console_("shop", "");
    await hist.dbHistoryOpen();
    await flush();
    const box = sheetEl().querySelector("input.filter") as HTMLInputElement;
    box.value = "select";
    box.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((r) => setTimeout(r, 300));
    await flush();
    expect(reads[reads.length - 1]).toContain("q=select");
    expect(titles()).toEqual(["SELECT 1"]);
    (sheetEl().querySelector(".fl-sum") as HTMLElement).click();
    expect(sheetEl().querySelector(".fl-step.open .fl-io")?.textContent).toContain("SELECT 1");
    (sheetEl().querySelector("[data-hload]") as HTMLElement).click();
    await flush();
    expect(sheetEl().hidden).toBe(true);
    const st = dbTab();
    expect(st.kind === "sql" ? st.sqlText : null).toBe("SELECT 1");
  });

  it("Load earlier asks for the page before the oldest shown", async () => {
    for (let i = 0; i < 120; i++) file("shop", "SELECT " + i);
    await console_("shop", "");
    await hist.dbHistoryOpen();
    await flush();
    expect(titles()).toHaveLength(100);
    const earlier = Array.from(sheetEl().querySelectorAll("button")).find((b) => b.textContent === "Load earlier") as HTMLButtonElement;
    expect(earlier.hidden).toBe(false);
    earlier.click();
    await flush();
    expect(reads[reads.length - 1]).toMatch(/before=\d+/);
    expect(titles()).toHaveLength(120);
    expect(titles()[119]).toBe("SELECT 0");
    expect(earlier.hidden).toBe(true);
  });

  it("Clear forgets this database's runs and no other's", async () => {
    file("shop", "SELECT 1");
    file("pgdb", "SELECT 2");
    await console_("shop", "");
    await hist.dbHistoryOpen();
    await flush();
    (Array.from(sheetEl().querySelectorAll("button")).find((b) => b.textContent === "Clear history") as HTMLElement).click();
    await flush();
    expect(deletes).toEqual(["/api/db/shop/history"]);
    expect(filed["app @ db2:5432"]).toHaveLength(1);
    expect(sheetEl().textContent).toContain("Nothing ran here yet");
  });
});

describe("the console's row limit (SPEC §data.console)", () => {
  it("a statement goes with 500 unless the operator picked another; the pick is remembered", async () => {
    await console_("shop", "SELECT * FROM user");
    await sql.dbRunSql();
    await flush();
    expect(bodies[bodies.length - 1]).toEqual({ sql: "SELECT * FROM user", limit: 500 });
    const pick = document.querySelector('[data-tb="rowlimit"]') as HTMLSelectElement;
    expect(pick.value).toBe("500");
    expect(Array.from(pick.options).map((o) => o.value)).toEqual(["100", "500", "1000", "2000", "5000", "10000"]);
    pick.value = "1000";
    pick.dispatchEvent(new Event("change", { bubbles: true }));
    await sql.dbRunSql();
    await flush();
    expect(bodies[bodies.length - 1]?.limit).toBe(1000);
    expect(localStorage.getItem("swiss.dbRowLimit")).toBe("1000");
    expect((document.querySelector('[data-tb="rowlimit"]') as HTMLSelectElement).value).toBe("1000");
  });

  it("a cut by the row limit is said on the results bar", async () => {
    await console_("pgdb", "SELECT * FROM big");
    await sql.dbRunSql();
    await flush();
    expect(document.getElementById("dbRbar")?.textContent).toContain("500 rows · cut at the row limit 500");
  });

  it("a redis console has no row limit to pick", async () => {
    await console_("cache", "PING");
    expect(document.querySelector('[data-tb="rowlimit"]')).toBeNull();
  });
});

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
import { dbConn as dbConnState, dbResetTabs, dbTab, dbTabs, freshTab } from "../src/db-state.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), plus a fetch stub
   whose responses the TEST resolves by hand — that is the only way to make a slow response
   arrive after a newer one under Node.
   SPEC §panel.toolchain: the renders build with h()/fill(), so nodes extend a Node stub (h()
   instanceof-checks children), textContent="" wipes like the DOM's, and document carries
   the NS/fragment factories. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, checked: false,
    value: "", className: "", id: "", title: "", rows: 0, type: "",
    selectionStart: 0, scrollHeight: 22, scrollTop: 0, scrollLeft: 0,
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
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => {} });
  Object.defineProperty(n, "textContent", {
    get: (): string => (n as any)._text ?? "",
    set: (v: string) => { if (v === "") n.children = []; (n as any)._text = v; },
  });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};

// The fetch stub: every call records its URL/body and parks until the test resolves it.
const requests: { url: string; body: any }[] = [];
const parked: { resolve: (j: any) => void; fail: (j: any) => void }[] = [];
globalThis.fetch = ((url: any, opts: any) => {
  requests.push({ url: String(url), body: opts && opts.body ? JSON.parse(opts.body) : null });
  return new Promise((res) => {
    parked.push({
      resolve: (j: any) => res({ ok: true, status: 200, json: async () => j }),
      fail: (j: any) => res({ ok: false, status: 400, json: async () => j }),
    });
  });
}) as any;

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

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const grid = await import(pathToFileURL(join(here, "data-grid.js")).href) as {
  dbLoadData: (keepOffset?: boolean, keepEdits?: boolean) => Promise<void>;
  dbRestoreData: () => Promise<void>;
};
const browsers = await import(pathToFileURL(join(here, "data-browsers.js")).href) as {
  dbLoadRedisValue: (key: string) => Promise<void>;
  dbRedisCommit: () => Promise<void>;
  dbLoadKeys: (reset?: boolean) => Promise<void>;
};
const edit = await import(pathToFileURL(join(here, "data-edit.js")).href) as {
  dbRunDdl: (op: string, to?: string) => Promise<void>;
};
const sql = await import(pathToFileURL(join(here, "data-sql.js")).href) as {
  dbCommit: () => Promise<void>;
};

/** Let every parked promise chain (apiJson, renders) run to its next await. */
const tick = () => new Promise((r) => setTimeout(r, 0));
/** Answer parked request `index` (default the oldest) with `body`. */
const answer = async (body: any, index = 0) => {
  parked.splice(index, 1)[0].resolve(body);
  await tick();
};
/** Fail parked request `index` with a 400 body — the apiJson error path. */
const fail = async (body: any, index = 0) => {
  parked.splice(index, 1)[0].fail(body);
  await tick();
};

/* SPEC §data.tabs: the one record is two halves — the connection's and the open object's.
   freshTab() supplies each tab's defaults, so only the connection fields and the fixture's
   own base line (two conns, mysql selected) are named. t is the ACTIVE tab (as a fresh
   mount leaves it); k is the key tab the redis tests install, the way selecting a redis
   connection swaps the tab kind in the view. */
function freshDb(): { c: Record<string, any>; t: Record<string, any>; k: Record<string, any>; installKey(): void } {
  const c = dbConnState() as unknown as Record<string, any>;
  c.conns = [dbConn("c", "mysql"), dbConn("r", "redis")];
  c.conn = "c"; c.redis = null; c.gridCfg = { widths: {}, hidden: [] };
  const tt = freshTab("table");
  const kk = freshTab("key");
  // The whole strip, not dbTabs()[0]: a test that opened a second tab must not leak it into
  // the next one (SPEC §data.tabs).
  dbResetTabs([tt], 0);
  return {
    c: c,
    t: tt as unknown as Record<string, any>,
    k: kk as unknown as Record<string, any>,
    installKey: () => { dbResetTabs([kk], 0); },
  };
}

describe("db loader response races (SPEC §data)", () => {
  it("a slow /data answer for the previous table never overwrites the newer table's page", async () => {
    const { t } = freshDb();
    t.table = "t1"; t.schema = "s1";
    const p1 = grid.dbLoadData(true);
    t.table = "t2"; // the user opens another table while t1's page is still in flight
    const p2 = grid.dbLoadData(true);
    await answer({ table: "t2", schema: "s2", columns: [dbCol("id")], rows: [{ id: 2 }], total: 1, primaryKey: ["id"], editable: false }, 1); // the newer request answers first
    await answer({ table: "t1", schema: "s1", columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: false }); // the stale one lands last
    await Promise.all([p1, p2]);
    expect(t.data.table, "the newer table's page stays").toBe("t2");
    expect(t.schema, "the old table never rewrites the tab's schema").toBe("s2");
  });

  it("a slow /key answer for the previous key never overwrites the newer key's value", async () => {
    const { c, k, installKey } = freshDb();
    c.conn = "r";
    installKey(); // a redis connection browses keys, not tables
    const p1 = browsers.dbLoadRedisValue("a");
    const p2 = browsers.dbLoadRedisValue("b"); // the user clicks another key mid-flight
    await answer({ key: "b", type: "string", value: "B" }, 1); // the newer key answers first
    await answer({ key: "a", type: "hash", value: { x: "1" }, length: 1 }); // the stale one lands last
    await Promise.all([p1, p2]);
    expect(k.redisKey).toBe("b");
    expect(k.redisValue.key, "the newer key's value stays").toBe("b");
  });
});

describe("the grid's pager after the set shrinks under it (SPEC §data)", () => {
  it("a reload that lands past the end backs off one page and re-fetches — no empty page reading 51-50 of 50", async () => {
    // A Commit that deletes the last page's rows (or a filter that shrinks the set) leaves
    // t.offset past the end: the reload returns zero rows and the footer used to paint an
    // empty page with "51–50 of 50". The honest answer is the previous page, re-fetched.
    const { t } = freshDb();
    t.table = "t";
    t.offset = 50; // page 2 of a 50-row set that has since shrunk to 1 row
    const before = requests.length;
    const p = grid.dbLoadData(true);
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [], total: 1, primaryKey: ["id"], editable: false });
    await p;
    expect(requests.length - before, "the loader re-fetched the previous page").toBe(2);
    expect(t.offset).toBe(0);
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: false });
    expect(t.data.rows.length, "the backed-off page shows its row").toBe(1);
  });

  it("an empty page at offset 0 stays — an empty (or fully filtered) table is the truth", async () => {    const { t } = freshDb();
    t.table = "t";
    t.offset = 0;
    const before = requests.length;
    const p = grid.dbLoadData(true);
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [], total: 0, primaryKey: ["id"], editable: false });
    await p;
    expect(requests.length - before, "no second fetch for a genuinely empty first page").toBe(1);
    expect(t.data.rows).toEqual([]);
  });
});

describe("who may drop a tab's buffered writes (SPEC §data.tabs)", () => {
  it("an explicit reload is a fresh baseline: the buffer goes", async () => {
    const { t } = freshDb();
    t.table = "t";
    t.updates = { "1": { pk: { id: 1 }, changes: { a: "x" } } };
    const p = grid.dbLoadData(true);
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: true });
    await p;
    expect(Object.keys(t.updates).length, "Refresh says so in its own tooltip (SPEC §data)").toBe(0);
  });

  it("the return fetch of a backgrounded tab is NOT a reload: the buffer stays", async () => {
    // Found live on 19998 during a walk of SPEC §data.tabs: switching to another tab and back ran the
    // same loader, so the writes the strip was still counting on that card vanished without a
    // word. D4 re-reads the page the tab dropped; it does not re-baseline the tab.
    const { t } = freshDb();
    t.table = "t";
    t.updates = { "1": { pk: { id: 1 }, changes: { a: "x" } } };
    const p = grid.dbRestoreData();
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: true });
    await p;
    expect(Object.keys(t.updates).length, "what the user typed is still buffered").toBe(1);
    expect(t.data.rows.length, "and the page it dropped is back").toBe(1);
  });

  it("a short page under a restore backs off WITHOUT taking the buffer with it", async () => {
    // The back-off is a second dbLoadData call: it must carry the restore's promise, or the
    // fix above would hold only for tabs that happen to land on a full page.
    const { t } = freshDb();
    t.table = "t";
    t.offset = 50;
    t.updates = { "1": { pk: { id: 1 }, changes: { a: "x" } } };
    const p = grid.dbRestoreData();
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [], total: 1, primaryKey: ["id"], editable: true });
    await answer({ table: "t", schema: null, columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: true });
    await p;
    expect(t.offset, "it backed off one page").toBe(0);
    expect(Object.keys(t.updates).length, "and kept the writes").toBe(1);
  });
});

describe("a redis commit that deletes the key's last field (SPEC §data)", () => {
  it("re-reads the key and refreshes the key list — the sidebar must not offer a key that is gone", async () => {
    // Deleting a hash's last field deletes the KEY itself. The commit path never awaited
    // the value re-read, so it checked a null redisValue, the "type none" branch never
    // ran, and the sidebar kept listing a key that no longer exists.
    const { c, k, installKey } = freshDb();
    c.conn = "r";
    installKey(); // the redis value view and its buffer live on the key tab
    k.redisKey = "h";
    k.redisValue = { key: "h", type: "hash", value: { only: "field" }, length: 1 };
    k.redisEdits = { key: "h", type: "hash", updates: {}, deletes: { only: 1 }, inserts: [] };
    const before = requests.length;
    const p = browsers.dbRedisCommit();
    await tick();
    await answer({ results: [{ ok: 1 }] }); // the pipeline ran HDEL
    await answer({ key: "h", type: "none" }); // the re-read: the key is gone
    await p;
    const keysReq = requests.slice(before).find((r) => r.url.includes("/keys"));
    expect(keysReq, "the key list was refreshed").toBeTruthy();
    if (parked.length) await answer({ keys: [], cursor: "0", done: true, total: 0 }); // drain
  });
});

describe("the sidebar grep reaches redis as a substring (SPEC §data.tabs, found on a walk)", () => {
  it("a bare word wraps in wildcards; a grep that already shapes its own wildcard rides through", async () => {
    // SCAN MATCH is exact-shape while the SQL side greps substrings - a bare "i18n"
    // used to answer "no keys" on a keyspace that plainly holds i18n_strings.
    const { c } = freshDb();
    c.conn = "r";
    c.grep = "i18n";
    const before = requests.length;
    const p = browsers.dbLoadKeys(true);
    await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    expect(requests.slice(before).find((r) => r.url.includes("/keys"))!.url)
      .toContain("pattern=*i18n*");

    c.grep = "K_*";
    const before2 = requests.length;
    const p2 = browsers.dbLoadKeys(true);
    await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p2;
    expect(requests.slice(before2).find((r) => r.url.includes("/keys"))!.url)
      .toContain("pattern=K_*");
  });
});

describe("the redis key list's More button (SPEC §data)", () => {
  it("a second More while a SCAN page is in flight is a no-op — the same cursor must not append its page twice", async () => {
    // A double click on More fired two dbLoadKeys with the SAME c.redis.cursor; both
    // answers concatenated their page onto the key list, duplicating every key in it.
    const { c } = freshDb();
    c.conn = "r";
    // SPEC §data.tabs: the tree renderer folds d.redis.keys by .key, so the fixture carries the
    // wire shape (ApiDbRedisKeyRow), not bare strings.
    c.redis = { keys: [{ key: "a", type: "string", ttl: -1 }], cursor: "42", done: false, total: 10 };
    const before = requests.length;
    const p1 = browsers.dbLoadKeys(false);
    const p2 = browsers.dbLoadKeys(false); // the double click
    expect(requests.length - before, "only one SCAN page is requested").toBe(1);
    await answer({ keys: [{ key: "b", type: "string", ttl: -1 }], cursor: "0", done: true, total: 10 });
    await Promise.all([p1, p2]);
    expect(c.redis.keys, "the page arrives once").toEqual([{ key: "a", type: "string", ttl: -1 }, { key: "b", type: "string", ttl: -1 }]);
  });
});

describe("DROP leaves no trace of the table on the right pane (SPEC §data)", () => {
  /* A wrap that keeps what it is given: the no-table empty state is a fill()/emptyNode APPEND
     (SPEC §panel.toolchain), so the assertions walk the collected text of the tree. */
  function gridWrap(): Stub {
    const wrap: any = {
      style: {}, children: [], textContent: "",
      appendChild(c: any) { wrap.children.push(c); return c; }, removeChild: (c: any) => c, remove: () => {},
      addEventListener: () => {}, removeEventListener: () => {},
      querySelector: () => null, querySelectorAll: () => [], contains: () => false,
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
    };
    byId.dbGridWrap = wrap;
    return wrap as Stub;
  }
  const textOf = (n: any, out: string[] = []): string[] => {
    if (!n) return out;
    if (typeof n.textContent === "string" && n.textContent) out.push(n.textContent);
    for (const c of n.children || []) textOf(c, out);
    return out;
  };
  const page = (table: string) => {
    return dbPage({ table: table, schema: "s", columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"] });
  };

  it("closes the dropped table's own tab — the pane falls back to the empty state", async () => {
    // The drop branch used to null table/data/detail but leave d.schema and the view state
    // behind, and the pane itself unpainted — renderDbTables refreshes only the LEFT list.
    // Under SPEC §data.tabs the whole tab goes instead: an object that no longer exists has no
    // business holding a card on the strip.
    const { t } = freshDb();
    t.table = "t"; t.schema = "s"; t.data = page("t");
    const wrap = gridWrap();
    const p = edit.dbRunDdl("drop");
    await tick();
    await answer({ ran: "DROP TABLE s.t" });   // the ddl ran
    await answer({ tables: [], total: 0, more: false }); // dbLoadTables refreshes the list
    await p;
    expect(dbTabs().length, "the strip keeps its placeholder, never zero tabs").toBe(1);
    const left = dbTab();
    expect(left.kind).toBe("table");
    expect(left === (t as unknown as object), "the dropped table's tab is not the one left").toBe(false);
    expect(left.kind === "table" && left.table, "the placeholder names no table").toBeNull();
    expect(left.kind === "table" && left.schema, "the dropped table's schema is gone").toBeNull();
    expect(textOf(wrap).some((x) => x.includes("Select a table")), "the right pane shows its empty state").toBe(true);
  });

  it("a background tab on the same table goes with it; the console's own tab stays", async () => {
    // Two tabs can hold one table — an FK jump opens the target under its own filter beside
    // the plain tab (SPEC §data.grid). The DROP must take both, and must take nothing else: a
    // console reply belongs to the console object, not to the table that was dropped.
    const { t } = freshDb();
    t.table = "t"; t.schema = "s"; t.data = page("t");
    const jumped = freshTab("table");
    jumped.table = "t"; jumped.schema = "s"; jumped.data = page("t");
    jumped.filters = [{ column: "id", op: "=", value: "1" }];
    const console_ = freshTab("sql");
    console_.sqlText = "select 1";
    console_.sqlResult = { columns: ["reply"], rows: [{ reply: "x" }], rowCount: 1 };
    dbResetTabs([t as any, jumped, console_], 0);
    gridWrap();
    const p = edit.dbRunDdl("drop");
    await tick();
    await answer({ ran: "DROP TABLE s.t" });
    await answer({ tables: [], total: 0, more: false });
    await p;
    expect(dbTabs().some((x) => x.kind === "table"), "no tab is still open on the dropped table").toBe(false);
    expect(dbTabs(), "the console object is untouched").toEqual([console_]);
    expect(console_.sqlResult, "its reply is not the dropped table's").not.toBeNull();
  });
});

describe("Commit keeps the grid where the user was looking (SPEC §data)", () => {
  it("a successful commit reloads the page but restores scrollTop — the edited row stays in view", async () => {
    // The reload after Commit rebuilds the grid; overflow-anchor is OFF (SPEC §data.grid), so the
    // pane snaps to the top and the row the user just committed scrolls out of sight.
    const { t } = freshDb();
    t.table = "t";
    t.data = { table: "t", columns: [dbCol("id")], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: true };
    t.updates = { "[1]": { pk: { id: 1 }, changes: { id: 2 } } };
    // A wrap that behaves like the real one: a repaint (the textContent wipe that opens
    // fill(), SPEC §panel.toolchain) RESETS scroll to the top.
    let paintedText = "";
    const wrap: any = {
      style: {}, children: [], scrollTop: 0, scrollLeft: 0,
      get textContent() { return paintedText; },
      set textContent(v: string) { paintedText = v; if (v === "") wrap.scrollTop = 0; },
      appendChild: (c: any) => c, removeChild: (c: any) => c, remove: () => {},
      addEventListener: () => {}, removeEventListener: () => {},
      querySelector: () => null, querySelectorAll: () => [], contains: () => false,
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
    };
    byId.dbGridWrap = wrap;
    wrap.scrollTop = 220; // the user is scrolled down at the row they edited
    const p = sql.dbCommit();
    await tick();
    await answer({ results: [{ affected: 1 }] }); // the edits transaction
    await answer({ rows: [{ id: 2 }], total: 1, primaryKey: ["id"], editable: true, columns: [dbCol("id")] }); // the reload
    await p;
    expect(wrap.scrollTop, "the pane is back where the user was").toBe(220);
  });
});

describe("a failed scan does not leave the key list pretending (SPEC §data)", () => {
  it("paints the failure in the list — not 'no keys' as if the keyspace were empty", async () => {
    // The redis 5.0.5 case: the server refuses the scan and the toast scrolls away, but the
    // list must not say "No keys match" — that reads as an empty keyspace, not a failure.
    const { c } = freshDb();
    c.conn = "r";
    c.grep = "zz";
    byId.dbTables = el();
    const p = browsers.dbLoadKeys(true);
    await tick();
    await fail({ error: "ERR Unknown option TYPE" });
    await p;
    const hints: string[] = [];
    const walk = (n: any) => {
      if (!n) return;
      if (String(n.className).includes("db-hint")) hints.push(String(n.textContent));
      for (const c of n.children || []) walk(c);
    };
    walk(byId.dbTables);
    expect(hints.length, "a hint row is painted").toBeGreaterThanOrEqual(1);
    expect(hints[0].toLowerCase(), "the hint names the failure").toContain("failed");
    expect(hints[0], "and not the empty-keyspace wording").not.toContain("No keys");
  });

  it("a stale More answer landing after a reset is dropped — its old-cursor page never splices in (SPEC §data)", async () => {
    // The suspected page-loss window: a slow More answer arriving after a grep/type reset
    // concatenated its page onto the FRESH walk and dragged the cursor BACKWARD, so the
    // walk re-scanned old ground and later pages shifted under the user.
    const { c } = freshDb();
    c.conn = "r";
    c.redis = { keys: [{ key: "a", type: "string", ttl: -1 }], cursor: "42", done: false, total: 10 };
    const more = browsers.dbLoadKeys(false); // the More page, parked at index 0
    await tick();
    const reset = browsers.dbLoadKeys(true); // a grep/type reset walks from cursor 0, index 1
    await tick();
    // The RESET answers first — the fresh page owns the list; the stale More lands LAST.
    await answer({ keys: [{ key: "fresh", type: "string", ttl: -1 }], cursor: "7", done: false, total: 10 }, 1);
    await answer({ keys: [{ key: "stale", type: "string", ttl: -1 }], cursor: "99", done: false, total: 10 }, 0);
    await Promise.all([more, reset]);
    expect(c.redis.cursor, "the fresh walk keeps its cursor").toBe("7");
    expect(c.redis.keys.map(function (row: any) { return row.key; }), "the stale page never spliced in").toEqual(["fresh"]);
  });

  it("a redis key scan never carries a schema param — even one left over from a pg connection (SPEC §data)", async () => {
    // Defense-in-depth: redis has no schema concept, and a residual c.schemaFilter (a pick
    // made against a pg catalog) must not ride the /keys request even if a future refactor
    // of the switch path forgets to clear it.
    const { c } = freshDb();
    c.conn = "r";
    c.schemaFilter = "public"; // residue by hypothesis
    const p = browsers.dbLoadKeys(true);
    await tick();
    await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    const url = requests[requests.length - 1].url;
    expect(url, "the scan URL is schema-free").not.toContain("schema=");
  });
});

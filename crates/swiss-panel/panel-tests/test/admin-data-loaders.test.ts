import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), plus a fetch stub
   whose responses the TEST resolves by hand — that is the only way to make a slow response
   arrive after a newer one under Node. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, checked: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
    selectionStart: 0, scrollHeight: 22, scrollTop: 0, scrollLeft: 0,
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

const here = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js");
const grid = await import(pathToFileURL(join(here, "data-grid.js")).href) as {
  dbLoadData: (keepOffset?: boolean) => Promise<void>;
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
const util = await import(pathToFileURL(join(here, "util.js")).href) as {
  state: { db: Record<string, any> };
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

function freshDb(): Record<string, any> {
  const d = util.state.db;
  d.conns = [{ name: "c", dialect: "mysql" }, { name: "r", dialect: "redis" }];
  d.conn = "c"; d.table = null; d.schema = null; d.data = null; d.detail = null;
  d.filters = []; d.offset = 0; d.pageSize = 50; d.order = null; d.dir = "asc";
  d.tab = "data"; d.sqlResult = null; d.sqlResults = null; d.sqlTab = 0; d.sqlBusy = false;
  d.redis = null; d.redisKey = null; d.redisValue = null; d.redisEdits = null;
  d.loading = false; d.detailBusy = false; d.conflict = null; d.focus = null;
  d.updates = {}; d.deletes = {}; d.inserts = []; d.sel = {}; d.gridCfg = { widths: {}, hidden: [] };
  return d;
}

describe("db loader response races (docs/22 closeout audit)", () => {
  it("a slow /data answer for the previous table never overwrites the newer table's page", async () => {
    const d = freshDb();
    d.table = "t1"; d.schema = "s1";
    const p1 = grid.dbLoadData(true);
    d.table = "t2"; // the user opens another table while t1's page is still in flight
    const p2 = grid.dbLoadData(true);
    await answer({ table: "t2", schema: "s2", columns: [{ name: "id" }], rows: [{ id: 2 }], total: 1, primaryKey: ["id"], editable: false }, 1); // the newer request answers first
    await answer({ table: "t1", schema: "s1", columns: [{ name: "id" }], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: false }); // the stale one lands last
    await Promise.all([p1, p2]);
    expect(d.data.table, "the newer table's page stays").toBe("t2");
    expect(d.schema, "the old table never rewrites d.schema").toBe("s2");
  });

  it("a slow /key answer for the previous key never overwrites the newer key's value", async () => {
    const d = freshDb();
    d.conn = "r";
    const p1 = browsers.dbLoadRedisValue("a");
    const p2 = browsers.dbLoadRedisValue("b"); // the user clicks another key mid-flight
    await answer({ key: "b", type: "string", value: "B" }, 1); // the newer key answers first
    await answer({ key: "a", type: "hash", value: { x: "1" }, length: 1 }); // the stale one lands last
    await Promise.all([p1, p2]);
    expect(d.redisKey).toBe("b");
    expect(d.redisValue.key, "the newer key's value stays").toBe("b");
  });
});

describe("the grid's pager after the set shrinks under it (docs/22 closeout audit)", () => {
  it("a reload that lands past the end backs off one page and re-fetches — no empty page reading 51-50 of 50", async () => {
    // A Commit that deletes the last page's rows (or a filter that shrinks the set) leaves
    // d.offset past the end: the reload returns zero rows and the footer used to paint an
    // empty page with "51–50 of 50". The honest answer is the previous page, re-fetched.
    const d = freshDb();
    d.table = "t";
    d.offset = 50; // page 2 of a 50-row set that has since shrunk to 1 row
    const before = requests.length;
    const p = grid.dbLoadData(true);
    await answer({ table: "t", schema: null, columns: [{ name: "id" }], rows: [], total: 1, primaryKey: ["id"], editable: false });
    await p;
    expect(requests.length - before, "the loader re-fetched the previous page").toBe(2);
    expect(d.offset).toBe(0);
    await answer({ table: "t", schema: null, columns: [{ name: "id" }], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: false });
    expect(d.data.rows.length, "the backed-off page shows its row").toBe(1);
  });

  it("an empty page at offset 0 stays — an empty (or fully filtered) table is the truth", async () => {    const d = freshDb();
    d.table = "t";
    d.offset = 0;
    const before = requests.length;
    const p = grid.dbLoadData(true);
    await answer({ table: "t", schema: null, columns: [{ name: "id" }], rows: [], total: 0, primaryKey: ["id"], editable: false });
    await p;
    expect(requests.length - before, "no second fetch for a genuinely empty first page").toBe(1);
    expect(d.data.rows).toEqual([]);
  });
});

describe("a redis commit that deletes the key's last field (docs/22 closeout audit)", () => {
  it("re-reads the key and refreshes the key list — the sidebar must not offer a key that is gone", async () => {
    // Deleting a hash's last field deletes the KEY itself. The commit path never awaited
    // the value re-read, so it checked a null redisValue, the "type none" branch never
    // ran, and the sidebar kept listing a key that no longer exists.
    const d = freshDb();
    d.conn = "r";
    d.redisKey = "h";
    d.redisValue = { key: "h", type: "hash", value: { only: "field" }, length: 1 };
    d.redisEdits = { key: "h", type: "hash", updates: {}, deletes: { only: 1 }, inserts: [] };
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

describe("the redis key list's More button (docs/22 closeout audit)", () => {
  it("a second More while a SCAN page is in flight is a no-op — the same cursor must not append its page twice", async () => {
    // A double click on More fired two dbLoadKeys with the SAME d.redis.cursor; both
    // answers concatenated their page onto the key list, duplicating every key in it.
    const d = freshDb();
    d.conn = "r";
    d.redis = { keys: ["a"], cursor: "42", done: false, total: 10 };
    const before = requests.length;
    const p1 = browsers.dbLoadKeys(false);
    const p2 = browsers.dbLoadKeys(false); // the double click
    expect(requests.length - before, "only one SCAN page is requested").toBe(1);
    await answer({ keys: ["b"], cursor: "0", done: true, total: 10 });
    await Promise.all([p1, p2]);
    expect(d.redis.keys, "the page arrives once").toEqual(["a", "b"]);
  });
});

describe("DROP leaves no trace of the table on the right pane (docs/22 closeout audit)", () => {
  it("clears the schema, the result tabs and the pane itself — the dropped table cannot linger", async () => {
    // The drop branch nulled table/data/detail but left d.schema, the open result tabs and
    // the right pane's DOM untouched — renderDbTables refreshes only the LEFT list, so the
    // dropped table's page stayed on screen with no live table behind it.
    const d = freshDb();
    d.table = "t"; d.schema = "s";
    d.data = { table: "t", schema: "s", columns: [{ name: "id" }], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: false };
    d.sqlResult = { columns: ["reply"], rows: [{ reply: "x" }], rowCount: 1 };
    // A wrap that records its innerHTML paint: the no-table empty state is an innerHTML
    // ASSIGNMENT, so a children-walk can never see it.
    let painted = "";
    const wrap: any = {
      style: {}, children: [],
      get innerHTML() { return painted; },
      set innerHTML(v: string) { painted = v; },
      appendChild: (c: any) => c, removeChild: (c: any) => c, remove: () => {},
      addEventListener: () => {}, removeEventListener: () => {},
      querySelector: () => null, querySelectorAll: () => [], contains: () => false,
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
    };
    byId.dbGridWrap = wrap;
    const p = edit.dbRunDdl("drop");
    await tick();
    await answer({ ran: "DROP TABLE s.t" }); // the ddl ran
    await answer({ tables: [], total: 0, more: false }); // dbLoadTables refreshes the list
    await p;
    expect(d.table).toBeNull();
    expect(d.schema, "the dropped table's schema is gone").toBeNull();
    expect(d.sqlResult, "no result tab survives the drop").toBeNull();
    expect(painted.includes("Select a table"), "the right pane shows its empty state").toBe(true);
  });
});

describe("Commit keeps the grid where the user was looking (docs/22 closeout B4)", () => {
  it("a successful commit reloads the page but restores scrollTop — the edited row stays in view", async () => {
    // The reload after Commit rebuilds the grid; overflow-anchor is OFF (docs/22 W2.2), so the
    // pane snaps to the top and the row the user just committed scrolls out of sight.
    const d = freshDb();
    d.table = "t";
    d.data = { table: "t", columns: [{ name: "id" }], rows: [{ id: 1 }], total: 1, primaryKey: ["id"], editable: true };
    d.updates = { "[1]": { pk: { id: 1 }, changes: { id: 2 } } };
    // A wrap that behaves like the real one: an innerHTML repaint RESETS scroll to the top.
    let painted = "";
    const wrap: any = {
      style: {}, children: [], scrollTop: 0, scrollLeft: 0,
      get innerHTML() { return painted; },
      set innerHTML(v: string) { painted = v; wrap.scrollTop = 0; },
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
    await answer({ rows: [{ id: 2 }], total: 1, primaryKey: ["id"], editable: true, columns: [{ name: "id" }] }); // the reload
    await p;
    expect(wrap.scrollTop, "the pane is back where the user was").toBe(220);
  });
});

describe("a failed scan does not leave the key list pretending (docs/22 closeout B1)", () => {
  it("paints the failure in the list — not 'no keys' as if the keyspace were empty", async () => {
    // The redis 5.0.5 case: the server refuses the scan and the toast scrolls away, but the
    // list must not say "No keys match" — that reads as an empty keyspace, not a failure.
    const d = freshDb();
    d.conn = "r";
    d.grep = "zz";
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

  it("a stale More answer landing after a reset is dropped — its old-cursor page never splices in (docs/22 closeout B3)", async () => {
    // The suspected page-loss window: a slow More answer arriving after a grep/type reset
    // concatenated its page onto the FRESH walk and dragged the cursor BACKWARD, so the
    // walk re-scanned old ground and later pages shifted under the user.
    const d = freshDb();
    d.conn = "r";
    d.redis = { keys: [{ key: "a", type: "string", ttl: -1 }], cursor: "42", done: false, total: 10 };
    const more = browsers.dbLoadKeys(false); // the More page, parked at index 0
    await tick();
    const reset = browsers.dbLoadKeys(true); // a grep/type reset walks from cursor 0, index 1
    await tick();
    // The RESET answers first — the fresh page owns the list; the stale More lands LAST.
    await answer({ keys: [{ key: "fresh", type: "string", ttl: -1 }], cursor: "7", done: false, total: 10 }, 1);
    await answer({ keys: [{ key: "stale", type: "string", ttl: -1 }], cursor: "99", done: false, total: 10 }, 0);
    await Promise.all([more, reset]);
    expect(d.redis.cursor, "the fresh walk keeps its cursor").toBe("7");
    expect(d.redis.keys.map(function (k: any) { return k.key; }), "the stale page never spliced in").toEqual(["fresh"]);
  });

  it("a redis key scan never carries a schema param — even one left over from a pg connection (docs/22 closeout B7)", async () => {
    // Defense-in-depth: redis has no schema concept, and a residual d.schemaFilter (a pick
    // made against a pg catalog) must not ride the /keys request even if a future refactor
    // of the switch path forgets to clear it.
    const d = freshDb();
    d.conn = "r";
    d.schemaFilter = "public"; // residue by hypothesis
    const p = browsers.dbLoadKeys(true);
    await tick();
    await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    const url = requests[requests.length - 1].url;
    expect(url, "the scan URL is schema-free").not.toContain("schema=");
  });
});

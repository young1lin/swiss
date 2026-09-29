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

import { beforeEach, describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { readFileSync } from "node:fs";
import { dbConn as dbConnState } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";
import { REDIS_NS_MAX_DEPTH, dbSectionOf, dbSectionSlices, redisNamespaceTree, redisTypeGlyph } from "../src/data-tree.js";

/* The Data sidebar's tree (docs/43 M2). The pure half (data-tree.ts) runs directly; the
 *  rendering half runs over the same DOM-stub technique as every panel suite, because the
 *  tree is built from mountGroup bands whose shape IS the acceptance: sections keep their
 *  slots at zero, the + and the ellipsis live on the Tables band alone, rows are one line,
 *  and the old chrome (#dbSchema select, .db-sortrow, the dbListHead band) is gone. */
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
    setAttribute(k: string, v: string) { (n.attrs ||= {})[k] = v; if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
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
    getElementById: (id: string) => (byId[id] ||= el()),
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
  confirm: (m: string) => { confirms.asked.push(String(m)); return confirms.answer; },
  alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
  // Recording and PARKED, the admin-data-loaders technique: a test that wants an answer
  // hands one over with answer(); everything else waits for ever, as before.
  fetch: (url: any, init?: any) => recordingFetch(url, init),
});

/** The suite's fetch. Named, because a test that swaps in its own must put THIS one back -
    restoring the bare parked-forever lambda instead cost an afternoon: every later test saw
    its requests vanish, while passing in isolation. */
function recordingFetch(url: unknown, init?: { body?: string }): Promise<unknown> {
  requests.push({ url: String(url), body: init && init.body });
  return new Promise((res) => {
    parked.push((j: unknown, ok = true) => res({ ok, status: ok ? 200 : 400, json: async () => j }));
  });
}

/* What the panel asked for, and what has not answered yet. Module-level, so a test that
   parks a request it never answers would hand the next one a ghost: both are cleared per
   test in the suites that use them. */
const requests: { url: string; body?: string }[] = [];
const parked: ((j: unknown, ok?: boolean) => void)[] = [];
const confirms = { answer: true, asked: [] as string[] };
const tick = (): Promise<void> => new Promise((r) => { setTimeout(r, 0); });
const answer = async (body: unknown, ok = true): Promise<void> => {
  const next = parked.shift();
  if (next) next(body, ok);
  await tick();
};

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const view = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  renderDbTables: () => void;
  dbLoadTables: () => Promise<void>;
  dbSectionLabel: (sec: string) => string;
  dbSortMenu: () => Array<{ label?: string; pick?: boolean; on?: boolean; sep?: boolean }>;
  keysUnder: (g: { ns: string; keys: { key: string }[]; children: unknown[] }) => string[];
  dbRedisDeleteKeys: (ns: string, keys: string[]) => Promise<void>;
  dbRedisDelBatches: (keys: string[]) => string[][][];
};

/** The whole text under a stub node, depth-first — the cheap walker every band assertion
 *  here reduces to. */
function text(n: Stub): string {
  return (n.textContent || "") + n.children.map(text).join("");
}
/** A band's BUTTONS by class: grp-add is the +, grp-more the ellipsis. */
function btn(n: Stub, cls: string): Stub[] {
  const out: Stub[] = [];
  const walk = (x: Stub): void => {
    if (x.className && typeof x.className === "string" && x.className.includes(cls) && x.tag === "button") out.push(x);
    (x.children || []).forEach(walk);
  };
  walk(n);
  return out;
}

function mount(kind: "mysql" | "pg" | "redis"): void {
  const d = dbConnState();
  d.conns = [dbConn("c1", kind)];
  d.conn = "c1";
  d.tables = []; d.treeShown = {}; d.tablesTotal = 0; d.more = false; d.grep = "";
  d.schemaFilter = ""; d.sort = "name"; d.sortDir = "asc";
  d.redis = null; d.redisError = false;
  d.gridCfg = { widths: {}, hidden: [] };
  byId.dbConn = el("select");
  byId.dbTables = el("div");
  byId.dbTablesPager = el("div");
}

describe("the SQL sections cut by type (docs/43 M2 #1)", () => {
  it("a missing type is a table, not a mystery section — servers that answer no type still land somewhere named", () => {
    expect(dbSectionOf({ schema: "s", name: "t" })).toBe("tables");
  });

  it("table-like kinds land under Tables; view-like kinds under Views; anything else under Routines", () => {
    expect(dbSectionOf({ schema: "s", name: "t", type: "table" })).toBe("tables");
    expect(dbSectionOf({ schema: "s", name: "p", type: "partitioned table" })).toBe("tables");
    expect(dbSectionOf({ schema: "s", name: "f", type: "foreign table" })).toBe("tables");
    expect(dbSectionOf({ schema: "s", name: "v", type: "view" })).toBe("views");
    expect(dbSectionOf({ schema: "s", name: "m", type: "matview" })).toBe("views");
    expect(dbSectionOf({ schema: "s", name: "r", type: "procedure" })).toBe("routines");
    expect(dbSectionOf({ schema: "s", name: "x", type: "function" })).toBe("routines");
  });

  it("every section keeps its slot at count 0 — the tree's shape never jumps between connections", () => {
    const slices = dbSectionSlices([{ schema: "s", name: "only", type: "table" }]);
    expect(slices.map((s) => s.name)).toEqual(["tables", "views", "routines"]);
    expect(slices[0].rows).toHaveLength(1);
    expect(slices[1].rows).toHaveLength(0);
    expect(slices[2].rows).toHaveLength(0);
  });
});

describe("the redis type glyphs (2026-09-28)", () => {
  const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");
  const TYPES = ["string", "hash", "list", "set", "zset", "stream"];

  it("gives each of the six core types its own glyph, and every glyph is in the sprite", () => {
    const glyphs = TYPES.map(redisTypeGlyph);
    expect(new Set(glyphs).size, glyphs.join(", ")).toBe(TYPES.length);
    for (const g of glyphs) expect(shell, `symbol i-${g}`).toContain(`<symbol id="i-${g}"`);
  });

  it("a module type (ReJSON-RL, vectorset) or none falls back to the plain key mark", () => {
    expect(redisTypeGlyph("ReJSON-RL")).toBe("key");
    expect(redisTypeGlyph("vectorset")).toBe("key");
    expect(redisTypeGlyph("none")).toBe("key");
    expect(shell).toContain('<symbol id="i-key"');
  });
});

describe("the redis keyspace folds along ':' (docs/43 M2 #4)", () => {
  it("an empty keyspace answers []", () => {
    expect(redisNamespaceTree([])).toEqual([]);
  });

  it("keys without a ':' sit at the root as the keyspace's own rows, not under a phantom namespace", () => {
    const tree = redisNamespaceTree([{ key: "bare", type: "string", ttl: -1 }]);
    expect(tree).toHaveLength(1);
    expect(tree[0].ns).toBe("");
    expect(tree[0].keys.map((k) => k.key)).toEqual(["bare"]);
    expect(tree[0].children).toEqual([]);
  });

  it("a shared prefix becomes one band; deeper keys nest under it", () => {
    const tree = redisNamespaceTree([
      { key: "a:b:c", type: "string", ttl: -1 },
      { key: "a:b:d", type: "hash", ttl: -1 },
      { key: "a:e", type: "string", ttl: -1 },
    ]);
    expect(tree).toHaveLength(1); // everything lives under "a"
    expect(tree[0].ns).toBe("a");
    expect(tree[0].keys.map((k) => k.key)).toEqual(["a:e"]);
    expect(tree[0].children).toHaveLength(1);
    expect(tree[0].children[0].ns).toBe("a:b");
    expect(tree[0].children[0].keys.map((k) => k.key)).toEqual(["a:b:c", "a:b:d"]);
  });

  it("a single-child chain compresses to one node — no folder around a single file", () => {
    const tree = redisNamespaceTree([
      { key: "stream:orders:1", type: "string", ttl: -1 },
      { key: "stream:orders:2", type: "string", ttl: -1 },
    ]);
    expect(tree).toHaveLength(1);
    expect(tree[0].ns, "the chain folds to its deepest useful label").toBe("stream:orders");
    expect(tree[0].keys.map((k) => k.key).sort()).toEqual(["stream:orders:1", "stream:orders:2"]);
    expect(tree[0].children).toEqual([]);
  });

  it("children sort by name with numbers compared by value (t2 < t10), keys by the caller's comparator", () => {
    const tree = redisNamespaceTree([
      { key: "t10:x", type: "string", ttl: -1 },
      { key: "t2:x", type: "string", ttl: -1 },
      { key: "a:x", type: "string", ttl: -1 },
    ]);
    expect(tree.map((g) => g.ns)).toEqual(["a", "t2", "t10"]);
  });

  it("a namespace holding exactly one key and nothing else stays one node — the rendering's leaf promise starts here", () => {
    const tree = redisNamespaceTree([{ key: "lonely:only", type: "string", ttl: -1 }]);
    expect(tree).toHaveLength(1);
    // The node keeps its own namespace name and holds the key directly; "renders as one
    // bare row showing the FULL key" is the renderer's promise, pinned in the DOM section.
    expect(tree[0].ns).toBe("lonely");
    expect(tree[0].keys.map((k) => k.key)).toEqual(["lonely:only"]);
    expect(tree[0].children).toHaveLength(0);
  });
});

describe("the rendered tree (docs/43 M2, DOM stubs)", () => {
  it("mysql: three section bands in mockup order, rows one line with the count right, + and ellipsis on Tables alone", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = [
      { schema: "iq", name: "t1", type: "table", approxRows: 9 },
      { schema: "iq", name: "v1", type: "view" },
      { schema: "iq", name: "r1", type: "procedure" },
    ];
    view.renderDbTables();
    const bands = byId.dbTables.children;
    expect(bands, "tables, views, routines — one band each").toHaveLength(3);
    const heads = bands.map((b: Stub) => text(b.children[0]));
    expect(heads[0]).toContain("Tables");
    expect(heads[1]).toContain("Views");
    expect(heads[2]).toContain("Routines");
    // The + and the ellipsis are Tables-band-only affordances (docs/43 M2 #6): the others
    // offer nothing to add and nothing to sort.
    expect(btn(bands[0], "grp-add")).toHaveLength(1);
    expect(btn(bands[0], "grp-more")).toHaveLength(1);
    expect(btn(bands[1], "grp-add")).toHaveLength(0);
    expect(btn(bands[1], "grp-more")).toHaveLength(0);
    expect(btn(bands[2], "grp-add")).toHaveLength(0);
    expect(btn(bands[2], "grp-more")).toHaveLength(0);
    // A row is one line: the name and the numeric meta sit as siblings, no second line.
    const row = bands[0].children[1].children[0];
    expect(row.className).toContain("db-table");
    expect(row.children.map((c: Stub) => c.className).join(",")).toContain("db-table-name");
    expect(row.children.map((c: Stub) => c.className).join(",")).toContain("db-table-meta");
    expect(text(row)).toContain("t1");
    expect(text(row)).toContain("9");
    // The full address stays the row's title (type · rows moved there from the second line).
    expect(String(row.title)).toContain("iq.t1");
  });

  it("the empty Views band SAYS what it is missing instead of offering a drop target", () => {
    mount("mysql");
    dbConnState().tables = [{ schema: "iq", name: "t1", type: "table" }];
    view.renderDbTables();
    const viewsBand = byId.dbTables.children[1];
    expect(text(viewsBand)).toContain("No views");
  });

  it("the sidebar's top row is gone: no #dbSchema select, no .db-sortrow, no dbListHead (docs/43 M2 #6)", () => {
    mount("mysql");
    dbConnState().tables = [{ schema: "iq", name: "t1", type: "table" }];
    view.renderDbTables();
    expect(byId.dbSchema).toBeUndefined();
    expect(byId.dbSort).toBeUndefined();
    expect(byId.dbSortDir).toBeUndefined();
    expect(byId.dbListHead).toBeUndefined();
  });

  it("the Tables band's ellipsis carries the list's sort: keys with the current one ticked, then direction", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = [{ schema: "iq", name: "t1", type: "table" }];
    d.sort = "rows"; d.sortDir = "asc";
    view.renderDbTables();
    const items = view.dbSortMenu().filter((i) => i.label);
    const names = items.map((i) => i.label);
    expect(names).toContain("Name");
    expect(names).toContain("Rows");
    expect(names).toContain("Size");
    expect(names).toContain("Sort ascending");
    expect(names).toContain("Sort descending");
    const rowsItem = items.find((i) => i.label === "Rows")!;
    expect(rowsItem.on, "the current sort key is ticked").toBe(true);
    const asc = items.find((i) => i.label === "Sort ascending")!;
    expect(asc.on, "the current direction is ticked").toBe(true);
  });

  it("pg: one schema band over nested type bands, the schema's count summing its rows (docs/43 M2 #3)", () => {
    mount("pg");
    const d = dbConnState();
    d.tables = [
      { schema: "public", name: "t1", type: "table" },
      { schema: "public", name: "t2", type: "table" },
      { schema: "public", name: "v1", type: "view" },
      { schema: "app", name: "t9", type: "table" },
    ];
    view.renderDbTables();
    const schemas = byId.dbTables.children;
    expect(schemas).toHaveLength(2);
    expect(text(schemas[0].children[0])).toContain("public");
    expect(text(schemas[0].children[0]), "the band numbers ROWS, not inner bands").toContain("3");
    expect(text(schemas[1].children[0])).toContain("app");
    expect(text(schemas[1].children[0])).toContain("1");
    // Inside public: the three type bands again (tables 2, views 1, routines 0).
    const inner = schemas[0].children[1].children;
    expect(inner).toHaveLength(3);
    expect(text(inner[0])).toContain("Tables");
    expect(inner[0].children[1].children).toHaveLength(2);
    expect(text(inner[1])).toContain("Views");
    expect(inner[1].children[1].children).toHaveLength(1);
  });

  it("redis: bands for namespaces, a one-key namespace IS its row, bare keys at the root (docs/43 M2 #4)", () => {
    mount("redis");
    const d = dbConnState();
    d.redis = {
      keys: [
        { key: "sess:a", type: "string", ttl: 30 },
        { key: "sess:b", type: "hash", ttl: -1 },
        { key: "lonely:only", type: "string", ttl: -1 },
        { key: "bare", type: "set", ttl: -1 },
      ],
      cursor: "0", done: true, total: 4,
    };
    view.renderDbTables();
    const tops = byId.dbTables.children;
    // bare root key first (the tree's root group), then the sess band, then the leaf row.
    const all = tops.map((b: Stub) => text(b)).join("||");
    expect(all).toContain("bare");
    expect(all).toContain("sess");
    expect(all, "a one-key namespace renders as its whole key, no folder").toContain("lonely:only");
    // The leaf and the bare key are plain rows (.db-table), the namespace is a band.
    expect(tops.some((b: Stub) => text(b).startsWith("sess")), "sess is a band with its count").toBe(true);
  });


  /* The owner's report (2026-09-29), looking at a band named `market` whose every row read
     `market:fast`, `market:ticks`: "它们的子层级不应该是 fast 和 ticks 吗？怎么还要显示全部信息，
     完整的key在旁边显示不行？" The band already said the prefix; the row says what it adds. */
  it("redis: a row under a band is named by what it ADDS to that band, with the full key beside it", () => {
    mount("redis");
    const d = dbConnState();
    d.redis = {
      keys: [
        { key: "market:fast", type: "stream", ttl: -1 },
        { key: "market:ticks", type: "stream", ttl: -1 },
        { key: "demo:events", type: "stream", ttl: -1 },
      ],
      cursor: "0", done: true, total: 3,
    };
    view.renderDbTables();
    const rows = btn(byId.dbTables, "db-table").filter((b) => b.dataset.rkey);
    const byKey = Object.fromEntries(rows.map((b) => [b.dataset.rkey, b]));
    const nameOf = (b: Stub): string =>
      b.children.find((c: Stub) => (c.className || "").indexOf("db-table-name") >= 0)!.textContent;
    const metaOf = (b: Stub): string =>
      b.children.find((c: Stub) => (c.className || "").indexOf("db-table-meta") >= 0)!.textContent;
    expect(nameOf(byKey["market:fast"]), "under the market band, the row is `fast`").toBe("fast");
    expect(nameOf(byKey["market:ticks"])).toBe("ticks");
    expect(metaOf(byKey["market:fast"]), "the whole key is beside it").toBe("market:fast");
    // A one-key namespace is its own row at the ROOT, so there the name IS the whole key and
    // the meta goes back to saying the type - nothing would be gained by printing it twice.
    expect(nameOf(byKey["demo:events"])).toBe("demo:events");
    expect(metaOf(byKey["demo:events"])).toBe("stream");
    // Either way the title carries both, and the row still opens the full key.
    expect(String(byKey["market:fast"].title)).toContain("market:fast");
    expect(String(byKey["market:fast"].title)).toContain("stream");
  });

  it("redis: a nested band is named by what IT adds, while its collapse key stays the full path", () => {
    mount("redis");
    const d = dbConnState();
    d.redis = {
      keys: [
        { key: "app:cache:users", type: "hash", ttl: -1 },
        { key: "app:cache:posts", type: "hash", ttl: -1 },
        { key: "app:queue:in", type: "list", ttl: -1 },
        { key: "app:queue:out", type: "list", ttl: -1 },
      ],
      cursor: "0", done: true, total: 4,
    };
    view.renderDbTables();
    const heads = btn(byId.dbTables, "grp-toggle").map((h: Stub) => text(h));
    expect(heads.some((t: string) => t.indexOf("app") >= 0), "the outer band is app").toBe(true);
    // The inner bands say `cache` and `queue`, not `app:cache` and `app:queue`.
    expect(heads.some((t: string) => t.indexOf("cache") >= 0 && t.indexOf("app:cache") < 0)).toBe(true);
    expect(heads.some((t: string) => t.indexOf("queue") >= 0 && t.indexOf("app:queue") < 0)).toBe(true);
    const rows = btn(byId.dbTables, "db-table").filter((b) => b.dataset.rkey);
    const byKey = Object.fromEntries(rows.map((b) => [b.dataset.rkey, b]));
    const nameOf = (b: Stub): string =>
      b.children.find((c: Stub) => (c.className || "").indexOf("db-table-name") >= 0)!.textContent;
    expect(nameOf(byKey["app:cache:users"]), "two levels down, the row is one segment").toBe("users");
    expect(nameOf(byKey["app:queue:in"])).toBe("in");
  });

  /* The owner's report (2026-09-28): "string" and "stream" side by side in the meta read alike.
     Each row now LEADS with its type's glyph - monochrome, like every descriptive mark (design
     rule 2) - and the word stays in the meta. */
  it("redis: a key row leads with its type's glyph, and string and stream differ at a glance", () => {
    mount("redis");
    const d = dbConnState();
    d.redis = {
      keys: [{ key: "test", type: "string", ttl: -1 }, { key: "ticks", type: "stream", ttl: -1 }],
      cursor: "0", done: true, total: 2,
    };
    view.renderDbTables();
    const rows = btn(byId.dbTables, "db-table").filter((b) => b.dataset.rkey);
    const glyph = (b: Stub): string => {
      const svg = b.children[0];
      expect(svg.tag, "the glyph comes first").toBe("svg");
      return svg.children[0].attrs.href;
    };
    const byKey = Object.fromEntries(rows.map((b) => [b.dataset.rkey, b]));
    expect(glyph(byKey.test)).toBe("#i-" + redisTypeGlyph("string"));
    expect(glyph(byKey.ticks)).toBe("#i-" + redisTypeGlyph("stream"));
    expect(glyph(byKey.test)).not.toBe(glyph(byKey.ticks));
    expect(text(byKey.ticks), "the type word stays in the meta").toContain("stream");
    expect(byKey.ticks.dataset.rtype, "the row hands its type to the tab it opens").toBe("stream");
    // .db-key is the grid's amber primary-key marker: the row must not wear it.
    expect(byKey.test.className.split(" ")).not.toContain("db-key");
  });

  it("a grep makes every band expand - filtered results must not hide inside collapsed bands", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = [{ schema: "iq", name: "t1", type: "table" }];
    d.grep = "t1";
    view.renderDbTables();
    const band = byId.dbTables.children[0];
    // With filtered: true, mountGroup paints the body open regardless of stored collapse.
    expect(band.children[1].children).toHaveLength(1);
  });
});

/* docs/43 addendum — the 200-row wall falls. The tree fetches the WHOLE catalog in one
 * shot; each section paints a render-capped window with a note row that expands it, and a
 * search paints every match. The pager is gone: browsing is not paging. */
describe("the thousand-table catalog (docs/43 addendum)", () => {
  /** .db-table rows under the box, excluding the note row (which also says db-tree-cap). */
  const rows = (): Stub[] => {
    const out: Stub[] = [];
    const walk = (x: Stub): void => {
      if (x.tag === "button" && String(x.className).includes("db-table")
        && !String(x.className).includes("db-tree-cap")) out.push(x);
      (x.children || []).forEach(walk);
    };
    walk(byId.dbTables);
    return out;
  };
  const note = (): Stub | undefined => {
    const out: Stub[] = [];
    const walk = (x: Stub): void => {
      if (x.tag === "button" && String(x.className).includes("db-tree-cap")) out.push(x);
      (x.children || []).forEach(walk);
    };
    walk(byId.dbTables);
    return out[0];
  };
  const tables = (n: number, schema: string): Array<{ schema: string; name: string; type: string }> =>
    Array.from({ length: n }, (_: unknown, i: number) => ({ schema, name: "t" + i, type: "table" }));

  it("205 tables paint 200 rows plus a note offering the remaining 5; the badge still says 205", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = tables(205, "iq");
    d.tablesTotal = 205;
    view.renderDbTables();
    expect(rows(), "the render cap is a DOM budget").toHaveLength(200);
    const n = note();
    expect(n, "the note row sits under the last row").toBeTruthy();
    expect(n!.dataset.treemore).toBe("iq/tables");
    expect(text(n!)).toContain("5");
    expect(text(byId.dbTables.children[0].children[0]), "the band's count numbers the full list").toContain("205");
  });

  it("show more pages DOWN in batches and only says Show fewer at the end (the owner's ask)", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = tables(1200, "iq");
    d.tablesTotal = 1200;
    view.renderDbTables();
    // Window one: the cap, offering one batch (500) - not the whole remaining list.
    expect(rows()).toHaveLength(200);
    let n = note()!;
    expect(n.dataset.treemore).toBe("iq/tables");
    expect(n.dataset.next).toBe("700");
    expect(text(n)).toContain("500");
    // The handler's move: trust the batch the note advertised. Window two: another batch.
    d.treeShown["iq/tables"] = Number(n.dataset.next);
    view.renderDbTables();
    expect(rows()).toHaveLength(700);
    n = note()!;
    expect(n.dataset.next).toBe("1200");
    // Still "show more" while anything remains - the end has not been reached.
    expect(n.dataset.treemore).toBe("iq/tables");
    // The last batch lands exactly on the end; only now does the note offer Show fewer.
    d.treeShown["iq/tables"] = Number(n.dataset.next);
    view.renderDbTables();
    expect(rows()).toHaveLength(1200);
    expect(note()!.dataset.treeless).toBe("iq/tables");
    // Collapsing drops the memory entirely - the window snaps back to the initial cap.
    delete d.treeShown["iq/tables"];
    view.renderDbTables();
    expect(rows()).toHaveLength(200);
    expect(note()!.dataset.treemore).toBe("iq/tables");
  });

  it("a search paints every match - nothing an operator typed hides behind the expander", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = tables(205, "iq");
    d.tablesTotal = 205;
    d.grep = "t";
    view.renderDbTables();
    expect(rows(), "grep overrides the cap").toHaveLength(205);
    expect(note()).toBeUndefined();
  });

  it("pg: the show-all memory is per schema - expanding public's Tables leaves app's alone", () => {
    mount("pg");
    const d = dbConnState();
    d.tables = [...tables(205, "public"), ...tables(205, "app")];
    d.tablesTotal = 410;
    view.renderDbTables();
    d.treeShown["public/tables"] = 205;
    view.renderDbTables();
    const notes: Stub[] = [];
    const walk = (x: Stub): void => {
      if (x.tag === "button" && String(x.className).includes("db-tree-cap")) notes.push(x);
      (x.children || []).forEach(walk);
    };
    walk(byId.dbTables);
    expect(notes, "one note per schema band, keys distinct").toHaveLength(2);
    expect(notes.some((x: Stub) => x.dataset.treeless === "public/tables"), "public is expanded").toBe(true);
    expect(notes.some((x: Stub) => x.dataset.treemore === "app/tables"), "app is still capped").toBe(true);
    expect(rows()).toHaveLength(405); // 205 + 200
  });

  it("the footer states the total — and says when the one-shot fetch clipped the catalog", () => {
    mount("mysql");
    const d = dbConnState();
    d.tables = tables(3, "iq");
    d.tablesTotal = 3; d.more = false;
    view.renderDbTables();
    expect(text(byId.dbTablesPager)).toContain("3");
    expect(text(byId.dbTablesPager)).not.toContain("first");
    d.more = true;
    view.renderDbTables();
    expect(text(byId.dbTablesPager)).toContain("first");
  });

  it("the list is ONE fetch for the whole catalog: limit=2000 rides the query, no page param", async () => {
    mount("mysql");
    const urls: string[] = [];
    (globalThis as unknown as { fetch: unknown }).fetch = (u: unknown):
      Promise<{ ok: boolean; status: number; json: () => Promise<unknown> }> => {
      urls.push(String(u));
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({ tables: [], total: 0, more: false }) });
    };
    await view.dbLoadTables();
    (globalThis as unknown as { fetch: unknown }).fetch = recordingFetch;
    expect(urls[0]).toContain("/tables?limit=2000");
    expect(urls[0], "browsing is not paging anymore").not.toContain("page=");
  });
});

/* The owner (2026-09-29): "你这个折叠的，也没有批量删除功能". A namespace band is the one place
   where "these keys" is already a set the operator pointed at. */
describe("a namespace band deletes its keys together (2026-09-29)", () => {
  beforeEach(() => { requests.length = 0; parked.length = 0; confirms.asked.length = 0; confirms.answer = true; });

  const keysFor = (): { key: string; type: string; ttl: number }[] => [
    { key: "market:fast", type: "stream", ttl: -1 },
    { key: "market:ticks", type: "stream", ttl: -1 },
    { key: "market:deep:one", type: "string", ttl: -1 },
    { key: "other:x", type: "string", ttl: -1 },
  ];

  it("counts every key under the band, its descendants included", () => {
    const tree = redisNamespaceTree(keysFor());
    const market = tree.find((g) => g.ns === "market")!;
    expect(view.keysUnder(market)).toEqual(["market:fast", "market:ticks", "market:deep:one"]);
  });

  it("sends ONE DEL with every key - one command is what makes it atomic", async () => {
    mount("redis");
    const d = dbConnState();
    d.redis = { keys: keysFor(), cursor: "0", done: true, total: 4 };
    view.renderDbTables();
    const p = view.dbRedisDeleteKeys("market", ["market:fast", "market:ticks", "market:deep:one"]);
    await tick();
    await tick(); // the confirm is synchronous; the request is a microtask behind it
    expect(requests.map((r) => r.url)).toEqual(["/api/db/c1/redis-pipeline"]);
    expect(JSON.parse(String(requests[0].body)).commands)
      .toEqual([["DEL", "market:fast", "market:ticks", "market:deep:one"]]);
    // The delete answers, and the keyspace re-walk it kicks off is answered too - a promise
    // left parked here would be the next test's ghost.
    for (let i = 0; i < 4; i++) await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    expect(parked, "nothing left in flight").toHaveLength(0);
  });

  it("asks before it deletes, and a refusal sends nothing", async () => {
    mount("redis");
    dbConnState().redis = { keys: keysFor(), cursor: "0", done: true, total: 4 };
    confirms.answer = false;
    await view.dbRedisDeleteKeys("market", ["market:fast"]);
    expect(requests, "a no is a no - nothing left the panel").toHaveLength(0);
    expect(confirms.asked[0], "the question names the namespace and the count").toContain("market");
    expect(confirms.asked[0]).toContain("1");
    confirms.answer = true;
  });

  it("says the walk is unfinished when it is, because then the count is not the namespace", async () => {
    mount("redis");
    dbConnState().redis = { keys: keysFor(), cursor: "17", done: false, total: 4 };
    confirms.answer = false;
    await view.dbRedisDeleteKeys("market", ["market:fast", "market:ticks"]);
    expect(confirms.asked[0]).toContain("has not finished");
    confirms.answer = true;
  });
});

/* The owner (2026-09-29): "每个极端情况，你都考虑到了吗？例如很多 key 都是同样的前缀的，还有其他
   的" - and then "各个极端情况，你都需要考虑清楚". A key name is whatever bytes somebody chose, so
   the tree meets names no fixture of ours would: a leading ":", "::" runs, a trailing ":", the
   names JavaScript's own objects answer to, an empty name, thousands under one prefix. Each
   case below is one of those, run through the builder AND the rendered rows. */
describe("the extreme keyspaces (2026-09-29)", () => {
  beforeEach(() => { requests.length = 0; parked.length = 0; confirms.asked.length = 0; confirms.answer = true; });

  const row = (key: string, type = "string"): { key: string; type: string; ttl: number } => ({ key, type, ttl: -1 });
  const nameOf = (b: Stub): string =>
    b.children.find((c: Stub) => (c.className || "").indexOf("db-table-name") >= 0)!.textContent;
  const metaOf = (b: Stub): string =>
    b.children.find((c: Stub) => (c.className || "").indexOf("db-table-meta") >= 0)!.textContent;
  const paint = (keys: { key: string; type: string; ttl: number }[]): Record<string, Stub> => {
    mount("redis");
    dbConnState().redis = { keys, cursor: "0", done: true, total: keys.length };
    view.renderDbTables();
    const rows = btn(byId.dbTables, "db-table").filter((b) => b.dataset.rkey !== undefined);
    return Object.fromEntries(rows.map((b) => [b.dataset.rkey, b]));
  };
  const heads = (): string[] => btn(byId.dbTables, "grp-toggle").map((h: Stub) => text(h));

  it("keys that START with ':' fold under a band of their own - the builder used to recurse for ever on them", () => {
    const keys = [row(":lead"), row(":lead:x"), row("::deep"), row("::deep2"), row("plain")];
    // The old builder took ns "" for the root AND for the empty first segment: a stack
    // overflow, and the whole Data page with it.
    const tree = redisNamespaceTree(keys);
    expect(tree[0].prefix, "the root's own keys come first").toBe("");
    expect(tree[0].keys.map((k) => k.key)).toEqual(["plain"]);
    const colon = tree.find((g) => g.prefix === ":")!;
    expect(colon, "one band for everything under ':'").toBeTruthy();
    expect(view.keysUnder(colon).sort()).toEqual([":lead", ":lead:x", "::deep", "::deep2"].sort());
    const byKey = paint(keys);
    expect(nameOf(byKey["plain"])).toBe("plain");
    // `:lead:x` is the only key under `:lead`, so the leaf promise makes it the row itself,
    // named by what it adds to the ':' band - the same rule that shows `b:c:d` under `a`.
    expect(nameOf(byKey[":lead:x"])).toBe("lead:x");
    expect(nameOf(byKey[":lead"])).toBe("lead");
    expect(nameOf(byKey["::deep"]), "inside the '::' band").toBe("deep");
    // Every row has a visible name - an empty segment never paints as a blank line.
    for (const k of keys) expect(nameOf(byKey[k.key]), k.key).not.toBe("");
    expect(heads().every((h) => h.trim() !== "" && !/^\d+$/.test(h.trim())), "no band head without words: " + heads().join(" | ")).toBe(true);
  });

  it("a ':' run ('a::b') makes a band whose segment is empty - it shows its whole prefix instead of nothing", () => {
    const byKey = paint([row("a::b"), row("a::c"), row("a:x")]);
    expect(heads().some((h) => h.indexOf("a::") >= 0), "the empty segment's band: " + heads().join(" | ")).toBe(true);
    expect(nameOf(byKey["a::b"])).toBe("b");
    expect(nameOf(byKey["a:x"])).toBe("x");
  });

  it("a key ending in ':' beside keys under that namespace is named by the whole key, not by nothing", () => {
    const byKey = paint([row("foo:"), row("foo:bar"), row("foo:baz")]);
    expect(nameOf(byKey["foo:"])).toBe("foo:");
    expect(metaOf(byKey["foo:"]), "the name is the whole key, so the slot says the type").toBe("string");
    expect(nameOf(byKey["foo:bar"])).toBe("bar");
  });

  it("a key that is ALSO a namespace sits beside that namespace's band, told apart by its full key", () => {
    const byKey = paint([row("a:b", "hash"), row("a:b:c"), row("a:b:d"), row("a:z")]);
    expect(nameOf(byKey["a:b"])).toBe("b");
    expect(metaOf(byKey["a:b"]), "the slot says which b").toBe("a:b");
    expect(nameOf(byKey["a:b:c"])).toBe("c");
    const a = redisNamespaceTree([row("a:b"), row("a:b:c"), row("a:b:d"), row("a:z")]).find((g) => g.ns === "a")!;
    expect(view.keysUnder(a).sort(), "deleting `a` takes the key a:b and everything under it").toEqual(["a:b", "a:b:c", "a:b:d", "a:z"]);
  });

  it("names JavaScript objects answer to - constructor, __proto__, toString - are plain namespaces", () => {
    const keys = [row("constructor:a"), row("constructor:b"), row("__proto__:x"), row("__proto__:y"),
      row("toString:1"), row("hasOwnProperty:1"), row("hasOwnProperty:2"), row("valueOf")];
    // A plain object answered bySeg["constructor"] with Object's constructor: `.push` is not
    // a function, and the sidebar threw.
    const tree = redisNamespaceTree(keys);
    expect(tree.map((g) => g.ns).sort()).toEqual(["", "__proto__", "constructor", "hasOwnProperty", "toString"].sort());
    const byKey = paint(keys);
    expect(nameOf(byKey["constructor:a"])).toBe("a");
    expect(nameOf(byKey["__proto__:y"])).toBe("y");
    expect(nameOf(byKey["valueOf"])).toBe("valueOf");
  });

  it("a band named constructor or __proto__ folds and unfolds like any other (the fold map has no prototype)", async () => {
    const { loadCollapsed } = await import("../src/groups.js");
    const m = loadCollapsed("extreme-test");
    expect(m["constructor"], "nothing inherited reads as folded").toBeUndefined();
    expect(m["toString"]).toBeUndefined();
    m["__proto__"] = true;
    m["constructor"] = true;
    expect(Object.keys(m).sort(), "both are ordinary entries").toEqual(["__proto__", "constructor"]);
    delete m["constructor"];
    expect(m["constructor"]).toBeUndefined();
    expect(JSON.parse(JSON.stringify(m))).toEqual(JSON.parse('{"__proto__": true}'));
  });

  it("an empty key name (SET \"\" v is legal) still paints as a row with something on it", () => {
    const byKey = paint([row(""), row("x")]);
    expect(nameOf(byKey[""]), "the empty string's own spelling").toBe('""');
    expect(metaOf(byKey[""])).toBe("string");
  });

  it("names a page would fold or hide print quoted and escaped - ' x' and 'x ' are two rows, not two x's", () => {
    // Seen live on 19998 (2026-09-29): HTML folds blanks, so ` x` and `x ` both painted as `x`,
    // and `line\nbreak` read as a key named `line break`.
    const names = [" x", "x ", "x", "line\nbreak", "line break", "tab\there", "a\u200bb", "ab", "rtl\u202eevil", "my key", "用户"];
    const byKey = paint(names.map((n) => row(n)));
    const shown = names.map((n) => nameOf(byKey[n]));
    expect(shown).toEqual(['" x"', '"x "', "x", '"line\\nbreak"', "line break", '"tab\\there"',
      '"a\\u200bb"', "ab", '"rtl\\u202eevil"', "my key", "用户"]);
    expect(new Set(shown).size, "every name tells itself apart").toBe(names.length);
    // Only the painting changes: the row still opens the key's own bytes.
    expect(byKey[" x"].dataset.rkey).toBe(" x");
  });

  it("a segment with edge blanks is shown quoted in its band and in its row", () => {
    const byKey = paint([row("ns: lead:a"), row("ns: lead:b"), row("ns:tail :c")]);
    expect(heads().some((h) => h.indexOf('" lead"') >= 0), heads().join(" | ")).toBe(true);
    expect(metaOf(byKey["ns:tail :c"]), "the whole key in the right slot").toBe("ns:tail :c");
  });

  it("5,000 keys under ONE prefix: one band, 5,000 rows each named by its own segment, one count to delete", () => {
    const keys = Array.from({ length: 5000 }, (_, i) => row("session:" + i));
    const t0 = Date.now();
    const byKey = paint(keys);
    const ms = Date.now() - t0;
    expect(Object.keys(byKey)).toHaveLength(5000);
    expect(nameOf(byKey["session:4999"])).toBe("4999");
    expect(metaOf(byKey["session:4999"])).toBe("session:4999");
    expect(heads(), "one band, not 5,000").toHaveLength(1);
    expect(ms, "the tree builds and paints in well under a second").toBeLessThan(1500);
    const g = redisNamespaceTree(keys)[0];
    expect(view.keysUnder(g)).toHaveLength(5000);
    // Numeric order within the band: session:2 before session:10, as a person counts.
    const order = g.keys.map((k) => k.key);
    expect(order.indexOf("session:2")).toBeLessThan(order.indexOf("session:10"));
  });

  it("a twelve-deep chain folds into one band named by the whole run it adds", () => {
    const chain = "d1:d2:d3:d4:d5:d6:d7:d8:d9:d10:d11:d12";
    const byKey = paint([row("d1:x"), row(chain + ":leaf"), row(chain + ":leaf2")]);
    expect(heads().some((h) => h.indexOf("d2:d3:d4:d5:d6:d7:d8:d9:d10:d11:d12") >= 0), heads().join(" | ")).toBe(true);
    expect(nameOf(byKey[chain + ":leaf"])).toBe("leaf");
    expect(nameOf(byKey["d1:x"])).toBe("x");
  });

  // How deep the tree goes, counting every band - a fold is one band however long its run.
  const depthOf = (gs: { children: unknown[] }[]): number => {
    let deepest = 0;
    let level: { children: unknown[] }[] = gs;
    while (level.length) {
      deepest++;
      level = level.flatMap((g) => g.children as { children: unknown[] }[]);
    }
    return deepest;
  };

  it("a staircase 5,000 levels deep builds, paints and deletes - it overflowed the stack", () => {
    // `a:x`, `a:a:x`, `a:a:a:x` ... every level holds a key, so nothing folds and the builder
    // went one call deeper per level until the stack ran out, taking the Data page with it.
    const keys = Array.from({ length: 5000 }, (_, i) => row("a:".repeat(i + 1) + "x"));
    const tree = redisNamespaceTree(keys);
    expect(depthOf(tree)).toBeLessThanOrEqual(REDIS_NS_MAX_DEPTH + 1);
    const a = tree.find((g) => g.prefix === "a:")!;
    expect(view.keysUnder(a).length, "every key is still in the tree, once").toBe(5000);
    expect(new Set(view.keysUnder(a)).size).toBe(5000);
    const byKey = paint(keys);
    // The deepest band keeps the rest of each key as its row's name.
    const deep = "a:".repeat(4000) + "x";
    expect(nameOf(byKey[deep])).toBe(deep.slice(REDIS_NS_MAX_DEPTH * 2));
    expect(nameOf(byKey["a:x"])).toBe("x");
  });

  it("two keys sharing a run of 100,000 colons fold into bands and paint", () => {
    const run = "b:".repeat(100000);
    const byKey = paint([row(run + "x"), row(run + "y"), row("plain")]);
    expect(nameOf(byKey[run + "x"]).endsWith("b:x")).toBe(true);
    expect(nameOf(byKey[run + "y"]).endsWith("b:y")).toBe(true);
    expect(nameOf(byKey["plain"])).toBe("plain");
  });

  it("a band over 200,000 walked keys lists them all - a spread of that many arguments overflowed", () => {
    // `app:config` keeps `app` a band of its own, so its count gathers a 200,000-key child.
    const keys = Array.from({ length: 200000 }, (_, i) => row("app:session:" + i)).concat([row("app:config")]);
    const tree = redisNamespaceTree(keys);
    const app = tree.find((g) => g.prefix === "app:")!;
    expect(app.children.length, "the child band is there to gather").toBe(1);
    const under = view.keysUnder(app);
    expect(under.length).toBe(200001);
    expect(under).toContain("app:session:199999");
    expect(view.dbRedisDelBatches(under).flat().length, "and the delete chunks them all").toBe(201);
  });

  it("a name that is not UTF-8 is never folded into a band - its ':' belongs to redis-cli's printing", () => {
    // Seen live on 19998 (2026-09-29): two Spring-serialized names folded into a band at the
    // ':' inside their printing, and a row read `one"`. Such a name stays at the root, whole.
    const bin = { ...row('"\\xac\\xed\\x00\\x05t\\x00\\x0bsession:one"'), binary: true };
    const tree = redisNamespaceTree([bin, row("session:two"), row("session:three")]);
    expect(tree[0].prefix, "the root's own rows").toBe("");
    expect(tree[0].keys.map((k) => k.key)).toEqual([bin.key]);
    const byKey = paint([bin, row("session:two"), row("session:three")]);
    expect(nameOf(byKey[bin.key])).toBe(bin.key);
    expect(metaOf(byKey[bin.key])).toContain("UTF-8");
    expect(byKey[bin.key].dataset.rbinary, "marked for the click to refuse").toBe("");
  });

  it("and a band never hands one to its DEL, whatever puts it there", () => {
    const band = { ns: "ns", prefix: "ns:", children: [], keys: [row("ns:a"), { ...row("ns:b"), binary: true }, row("ns:c")] };
    expect(view.keysUnder(band)).toEqual(["ns:a", "ns:c"]);
  });

  it("unicode, emoji, spaces and backslashes in names slice cleanly at the ':'", () => {
    const byKey = paint([row("用户:1"), row("用户:2"), row("emoji:😀"), row("emoji:🎉"),
      row("with space:a b"), row("with space:c"), row("back\\slash:a"), row("back\\slash:b")]);
    expect(nameOf(byKey["用户:2"])).toBe("2");
    expect(nameOf(byKey["emoji:🎉"])).toBe("🎉");
    expect(nameOf(byKey["with space:a b"])).toBe("a b");
    expect(nameOf(byKey["back\\slash:b"])).toBe("b");
  });

  it("SCAN handing a key back twice (it promises each key AT LEAST once) lists it once", async () => {
    const browsers = await import("../src/data-browsers.js");
    mount("redis");
    const p = browsers.dbLoadKeys(true);
    await tick();
    await answer({ keys: [row("dup:a"), row("dup:a"), row("dup:b")], cursor: "9", done: false, total: 3 });
    await p;
    const p2 = browsers.dbLoadKeys(false);
    await tick();
    // The next page repeats a key from the first - a rehash mid-walk does exactly this.
    await answer({ keys: [row("dup:b"), row("dup:c")], cursor: "0", done: true, total: 3 });
    await p2;
    expect(dbConnState().redis!.keys.map((k) => k.key)).toEqual(["dup:a", "dup:b", "dup:c"]);
  });
});

describe("a batch delete at the edges (2026-09-29)", () => {
  beforeEach(() => { requests.length = 0; parked.length = 0; confirms.asked.length = 0; confirms.answer = true; });
  const row = (key: string): { key: string; type: string; ttl: number } => ({ key, type: "string", ttl: -1 });
  const bodies = (): string[][][] => requests
    .filter((r) => r.url.endsWith("/redis-pipeline"))
    .map((r) => JSON.parse(String(r.body)).commands);

  it("cuts DEL at 1,000 keys a command, exactly at the boundary", () => {
    const keys = (n: number): string[] => Array.from({ length: n }, (_, i) => "k:" + i);
    expect(view.dbRedisDelBatches([])).toEqual([]);
    const one = view.dbRedisDelBatches(keys(1000));
    expect(one).toHaveLength(1);
    expect(one[0].map((c) => c.length - 1), "1,000 keys: one DEL").toEqual([1000]);
    expect(view.dbRedisDelBatches(keys(1001))[0].map((c) => c.length - 1), "1,001: two").toEqual([1000, 1]);
    const five = view.dbRedisDelBatches(keys(5000));
    expect(five, "5,000 short keys still fit ONE request").toHaveLength(1);
    expect(five[0].map((c) => c.length - 1)).toEqual([1000, 1000, 1000, 1000, 1000]);
    expect(five[0].every((c) => c[0] === "DEL")).toBe(true);
  });

  it("keeps each request under the gateway's 2 MiB body limit when the names are long", () => {
    const long = Array.from({ length: 3000 }, (_, i) => "k:" + i + ":" + "x".repeat(1000));
    const batches = view.dbRedisDelBatches(long);
    expect(batches.length, "3 MB of names cannot be one request").toBeGreaterThan(1);
    for (const req of batches) {
      const bytes = new TextEncoder().encode(JSON.stringify({ commands: req })).length;
      expect(bytes, "every request under the 2 MiB BODY_LIMIT").toBeLessThan(2 * 1024 * 1024);
    }
    expect(batches.flat().flatMap((c) => c.slice(1)), "every key once, in order").toEqual(long);
    // One key bigger than any budget still goes - alone - rather than being silently kept.
    const huge = "h:" + "y".repeat(1600000);
    const lone = view.dbRedisDelBatches(["a:1", huge, "a:2"]);
    expect(lone.flat().flatMap((c) => c.slice(1))).toEqual(["a:1", huge, "a:2"]);
    expect(lone.flat().find((c) => c.includes(huge))!.length, "the giant rides alone").toBe(2);
  });

  it("5,000 keys: one confirm naming the count and the five DEL commands, one request, a toast from redis' own count", async () => {
    mount("redis");
    const keys = Array.from({ length: 5000 }, (_, i) => "session:" + i);
    dbConnState().redis = { keys: keys.map(row), cursor: "0", done: true, total: 5000 };
    const p = view.dbRedisDeleteKeys("session", keys);
    await tick(); await tick();
    expect(confirms.asked[0]).toContain("5,000");
    expect(confirms.asked[0], "it says the delete is not one step").toContain("5 DEL commands");
    expect(bodies()).toHaveLength(1);
    expect(bodies()[0]).toHaveLength(5);
    await answer({ replies: [1000, 1000, 1000, 1000, 1000] });
    for (let i = 0; i < 3; i++) await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    expect(byId.toast.textContent).toBe("Deleted 5,000 keys under session");
  });

  it("reports what REDIS deleted: keys someone else removed first are not claimed", async () => {
    mount("redis");
    dbConnState().redis = { keys: [row("m:a"), row("m:b"), row("m:c")], cursor: "0", done: true, total: 3 };
    const p = view.dbRedisDeleteKeys("m", ["m:a", "m:b", "m:c"]);
    await tick(); await tick();
    await answer({ replies: [2] });
    for (let i = 0; i < 3; i++) await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    expect(byId.toast.textContent).toBe("Deleted 2 keys under m - 1 were already gone");
  });

  it("the same key twice (SCAN duplicates) is sent once and counted once", async () => {
    mount("redis");
    dbConnState().redis = { keys: [row("m:a"), row("m:b")], cursor: "0", done: true, total: 2 };
    const p = view.dbRedisDeleteKeys("m", ["m:a", "m:b", "m:a"]);
    await tick(); await tick();
    expect(confirms.asked[0]).toContain("all 2 keys");
    expect(bodies()[0]).toEqual([["DEL", "m:a", "m:b"]]);
    await answer({ replies: [2] });
    for (let i = 0; i < 3; i++) await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
  });

  it("under a filter the question says only the SHOWN keys go - the namespace may hold more", async () => {
    mount("redis");
    const d = dbConnState();
    d.grep = "*error*";
    d.redis = { keys: [row("log:error:1"), row("log:error:2")], cursor: "0", done: true, total: 2 };
    confirms.answer = false;
    await view.dbRedisDeleteKeys("log", ["log:error:1", "log:error:2"]);
    expect(confirms.asked[0]).toContain("Only the keys the filter shows are deleted");
    expect(confirms.asked[0]).not.toContain("all 2");
    expect(requests).toHaveLength(0);
    d.grep = "";
  });

  it("a request that fails midway: what went is reported with the server's reason, and the tree is walked again", async () => {
    mount("redis");
    const long = Array.from({ length: 2000 }, (_, i) => "big:" + i + ":" + "z".repeat(1000));
    dbConnState().redis = { keys: long.map(row), cursor: "0", done: true, total: 2000 };
    // A tab open on a key of the FIRST request, and one on a key of the second.
    const tabs = (await import("../src/db-state.js")).dbTabs();
    const first = { kind: "key", redisKey: long[0], redisValue: { type: "string" }, redisEdits: null, redisStreamRows: null };
    const second = { kind: "key", redisKey: long[1999], redisValue: { type: "string" }, redisEdits: null, redisStreamRows: null };
    tabs.push(first as never, second as never);
    const batches = view.dbRedisDelBatches(long);
    expect(batches.length).toBeGreaterThan(1);
    const p = view.dbRedisDeleteKeys("big", long);
    await tick(); await tick();
    const firstCount = batches[0].reduce((a, c) => a + c.length - 1, 0);
    await answer({ replies: batches[0].map((c) => c.length - 1) });
    await tick();
    await answer({ error: "ERR the server went away" }, false);
    for (let i = 0; i < 3; i++) await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
    expect(byId.toast.textContent).toContain("Deleted " + firstCount.toLocaleString("en") + " of 2,000 keys under big");
    expect(byId.toast.textContent, "the reason survives").toContain("ERR the server went away");
    expect(first.redisKey, "its key went: the tab lets go").toBeNull();
    expect(second.redisKey, "its key did not: the tab stays").toBe(long[1999]);
    expect(requests.some((r) => r.url.includes("/keys?")), "the tree is walked again").toBe(true);
    tabs.splice(tabs.indexOf(first as never), 1);
    tabs.splice(tabs.indexOf(second as never), 1);
  });

  it("a second Delete while the first is still in flight sends nothing more", async () => {
    mount("redis");
    dbConnState().redis = { keys: [row("q:a"), row("q:b")], cursor: "0", done: true, total: 2 };
    const p = view.dbRedisDeleteKeys("q", ["q:a", "q:b"]);
    await tick(); await tick();
    await view.dbRedisDeleteKeys("q", ["q:a", "q:b"]);
    expect(confirms.asked, "one question").toHaveLength(1);
    expect(bodies(), "one request").toHaveLength(1);
    await answer({ replies: [2] });
    for (let i = 0; i < 3; i++) await answer({ keys: [], cursor: "0", done: true, total: 0 });
    await p;
  });
});

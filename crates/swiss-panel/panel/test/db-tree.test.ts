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
import { readFileSync } from "node:fs";
import { dbConn as dbConnState } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";
import { dbSectionOf, dbSectionSlices, redisNamespaceTree, redisTypeGlyph } from "../src/data-tree.js";

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
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
  fetch: () => new Promise(() => {}),
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const view = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  renderDbTables: () => void;
  dbLoadTables: () => Promise<void>;
  dbSectionLabel: (sec: string) => string;
  dbSortMenu: () => Array<{ label?: string; pick?: boolean; on?: boolean; sep?: boolean }>;
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
    (globalThis as unknown as { fetch: unknown }).fetch = () => new Promise(() => {});
    expect(urls[0]).toContain("/tables?limit=2000");
    expect(urls[0], "browsing is not paging anymore").not.toContain("page=");
  });
});

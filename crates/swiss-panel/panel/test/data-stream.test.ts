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
import type { ApiDbRedisValue, ApiDbStreamWindow } from "../src/types/api.js";

/* The stream view's tests: the pure merge/cap/columns folds (docs/45 S2), plus the DOM
 * walk - Load-earlier against a parked fetch, the docs/43 M3 cellmenu technique. */
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
  // renderDbGrid wipes the pane with `wrap.textContent = ""` (a real DOM clear); the
  // stub honors it the same way, or every repaint APPENDS and the walk reads a stale table.
  let tc = "";
  Object.defineProperty(n, "textContent", {
    get: () => { return tc; },
    set: (v: string) => { tc = v == null ? "" : v; if (tc === "") n.children = []; },
  });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};
const docStub: any = {
  documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
  activeElement: null,
  createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
  createDocumentFragment: () => el("#document-fragment"),
  createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
  getElementById: (id: string) => {
    const walk = (n: Stub): Stub | undefined => {
      if (n.id === id) return n;
      for (const c of n.children || []) { const hit = walk(c); if (hit) return hit; }
      return undefined;
    };
    return walk(docStub.body) || byId[id] || (byId[id] = el());
  },
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};
// The fetch stub: every call records its URL and parks until the test answers it -
// the admin-data-loaders technique, so the DOM walk drives the real request chain.
const requests: { url: string }[] = [];
const parked: { resolve: (j: any) => void; fail: (j: any) => void }[] = [];
globalThis.fetch = ((url: any) => {
  requests.push({ url: String(url) });
  return new Promise((res) => {
    parked.push({
      resolve: (j: any) => res({ ok: true, status: 200, json: async () => j }),
      fail: (j: any) => res({ ok: false, status: 400, json: async () => j }),
    });
  });
}) as any;
Object.assign(globalThis, {
  document: docStub,
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});
Object.defineProperty(globalThis, "navigator", { value: {}, configurable: true });

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const stream = await import(pathToFileURL(join(here, "data-stream.ts")).href) as {
  streamMerge: (rows: StreamEntry[], page: StreamEntry[]) => StreamEntry[];
  streamCap: (rows: StreamEntry[], cap: number, keep: "newest" | "oldest") => StreamEntry[];
  streamColumns: (rows: StreamEntry[]) => string[];
};
const browsers = await import(pathToFileURL(join(here, "data-browsers.ts")).href) as {
  dbRenderRedisValue: (wrap: Stub) => void;
};
interface StreamEntry { id: string; ts: string | null; fields: Record<string, string> }
const streamMerge = stream.streamMerge;
const streamCap = stream.streamCap;
const streamColumns = stream.streamColumns;

const entry = (id: string, fields: Record<string, string>): StreamEntry => {
  return { id: id, ts: null, fields: fields };
};

// docs/45 S2 fix: the merge SORTS both directions into newest-first order — the first
// cut prepended the page unconditionally and stood a Before page on its head.
describe("streamMerge", () => {
  it("lands an older page BELOW the cached window, in order", () => {
    const rows = [entry("5-0", { a: "1" }), entry("4-0", { a: "2" })];
    const page = [entry("3-0", { a: "3" }), entry("2-0", { a: "4" })];
    expect(streamMerge(rows, page).map((e: StreamEntry) => e.id)).toEqual(["5-0", "4-0", "3-0", "2-0"]);
  });

  it("lands a NEWER page above the cache - the Follow shape merges by the same rule", () => {
    const rows = [entry("4-0", { a: "1" }), entry("3-0", { a: "2" })];
    expect(streamMerge(rows, [entry("5-0", { a: "0" })]).map((e: StreamEntry) => e.id))
      .toEqual(["5-0", "4-0", "3-0"]);
  });

  it("sorts a page that arrives in the wrong internal order", () => {
    // The server promises newest-first pages, but the merge's order is structural:
    // it must not TRUST the promise, it must re-derive the order from the ids.
    const rows = [entry("5-0", { a: "1" })];
    const page = [entry("1-0", { a: "9" }), entry("9-0", { a: "0" }), entry("3-0", { a: "5" })];
    expect(streamMerge(rows, page).map((e: StreamEntry) => e.id)).toEqual(["9-0", "5-0", "3-0", "1-0"]);
  });

  it("never repeats a row at a seam overlap", () => {
    const rows = [entry("5-0", { a: "1" }), entry("4-0", { a: "2" })];
    const page = [entry("4-0", { a: "2" }), entry("3-0", { a: "3" })];
    expect(streamMerge(rows, page).map((e: StreamEntry) => e.id)).toEqual(["5-0", "4-0", "3-0"]);
  });

  it("orders seq past 65536 correctly - the ms*65536+seq fuse would collide here", () => {
    // Fused keys: 1000*65536+65536 === 1001*65536+0 — the first cut ranked
    // "1000-65536" and "1001-0" EQUAL. The tuple compare cannot.
    const rows = [entry("1001-0", { a: "1" })];
    const page = [entry("1000-65536", { a: "2" }), entry("1000-65535", { a: "3" })];
    expect(streamMerge(rows, page).map((e: StreamEntry) => e.id))
      .toEqual(["1001-0", "1000-65536", "1000-65535"]);
  });

  it("merges the walk's empty last page as a no-op", () => {
    const rows = [entry("5-0", { a: "1" })];
    expect(streamMerge(rows, [])).toEqual(rows);
  });
});

// docs/45 §2.2: one cap, two directions - a walk of older pages drops from the TOP so
// the page it just fetched survives; a live-edge follow drops from the BOTTOM.
describe("streamCap", () => {
  it("keep newest: the head survives, the oldest tail falls out", () => {
    // Fixtures arrive newest-first - the cache's own order; the cap never sorts.
    const rows = [6, 5, 4, 3, 2, 1].map((n: number) => entry(n + "-0", {}));
    expect(streamCap(rows, 4, "newest").map((e: StreamEntry) => e.id))
      .toEqual(["6-0", "5-0", "4-0", "3-0"]);
  });

  it("keep oldest: the TAIL survives, the top falls out - spec §2.2's walk direction", () => {
    const rows = [6, 5, 4, 3, 2, 1].map((n: number) => entry(n + "-0", {}));
    expect(streamCap(rows, 4, "oldest").map((e: StreamEntry) => e.id))
      .toEqual(["4-0", "3-0", "2-0", "1-0"]);
  });

  it("at or under the cap both directions answer the same rows untouched", () => {
    const rows = [entry("2-0", {}), entry("1-0", {})];
    expect(streamCap(rows, 2, "newest")).toBe(rows);
    expect(streamCap(rows, 2, "oldest")).toBe(rows);
    expect(streamCap(rows, 0, "newest")).toBe(rows); // a junk cap trims nothing
  });

  it("a 500-row window plus a 500-row older page keeps the OLDER half - the walk's own shape", () => {
    const window500 = Array.from({ length: 500 }, (_v: unknown, i: number) => entry("1000-" + i, {}));
    const page500 = Array.from({ length: 500 }, (_v: unknown, i: number) => entry("999-" + i, {}));
    const merged = streamCap(streamMerge(window500, page500), 500, "oldest");
    expect(merged.length).toBe(500);
    expect(merged[0].id).toBe("999-499"); // the page's newest row is now the table's top
    expect(merged[499].id).toBe("999-0"); // the page's oldest SURVIVED: the cursor moved
  });
});

// docs/45 S2/D3: the columns derivation — the field union in first-seen-scanning-
// newest-first order, the same algorithm the server runs per window, re-run over the
// merged rows so an older page's new field lands at the tail.
describe("streamColumns", () => {
  it("unions ragged fields newest-first, never alphabetized", () => {
    // The ragged seed's five entries, newest first: {a,c,d} / {c} / {b,c} / {a,b} / {a}.
    const rows = [
      entry("4-0", { a: "1", c: "3", d: "4" }),
      entry("3-0", { c: "5" }),
      entry("2-0", { b: "6", c: "7" }),
      entry("1-0", { a: "8", b: "9" }),
      entry("0-0", { a: "0" }),
    ];
    expect(streamColumns(rows)).toEqual(["a", "c", "d", "b"]);
  });

  it("keeps the first window's order and appends what an older page introduces", () => {
    const rows = [entry("9-0", { sym: "AAA", px: "1" }), entry("8-0", { qty: "2", sym: "BBB" })];
    expect(streamColumns(rows)).toEqual(["sym", "px", "qty"]);
  });

  it("answers [] for no rows and skips entries with no fields", () => {
    expect(streamColumns([])).toEqual([]);
    expect(streamColumns([entry("1-0", {})])).toEqual([]);
  });
});

/* --- the DOM walk (docs/45 S2 fix) ------------------------------------------------------------- */

function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}
function text(node: Stub): string {
  return (node.textContent || "") + (node.children || []).map(text).join("");
}
const tick = () => new Promise((r) => setTimeout(r, 0));
const answer = async (body: any): Promise<void> => {
  parked.splice(0, 1)[0].resolve(body);
  await tick();
};

function mountStream(win: Partial<ApiDbStreamWindow>): Stub {
  const wrap = el();
  byId.dbGridWrap = wrap;
  byId.sheet = el();
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), { conn: "r", conns: [{ name: "r", dialect: "redis" }] });
  const k = freshTab("key");
  dbTabs()[0] = k;
  k.redisKey = "s";
  k.redisValue = win as ApiDbRedisValue;
  browsers.dbRenderRedisValue(wrap);
  return wrap;
}
/** The value body's row ids, newest first - the first cell of every tbody row. */
const rowIds = (): string[] =>
  find(byId.dbGridWrap, (n) => n.tag === "tbody")[0].children
    .map((r: Stub) => (r.children[0] as Stub).textContent);
const loadEarlierBtn = (): Stub =>
  find(byId.dbGridWrap, (n) => n.tag === "button" && text(n).indexOf("Load earlier") >= 0)[0];
const pageOf = (ms: number): StreamEntry[] =>
  Array.from({ length: 500 }, (_v: unknown, i: number) => entry(ms + "-" + (499 - i), { a: String(i) }));

describe("the stream value view's Load-earlier walk (docs/45 S2 fix)", () => {
  it("mounts newest-first: the FIRST row is the stream's lastId, headers id/time then first-seen fields", () => {
    const wrap = mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("9-1", { a: "1" }), entry("9-0", { b: "2", a: "3" })],
      columns: ["a", "b"], more: true, firstId: "9-0", lastId: "9-1",
    });
    const ids = rowIds();
    expect(ids).toEqual(["9-1", "9-0"]); // newest row on top - NOT firstId
    const headers = find(wrap, (n) => n.tag === "thead")[0].children[0].children
      .map((th: Stub) => th.textContent);
    expect(headers).toEqual(["id", "time", "a", "b"]); // field union, first-seen order
  });

  it("walks three older pages past the 500-row cap: cursor strictly older each click, never stuck", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2000,
      entries: pageOf(1000), columns: ["a"], more: true, firstId: "1000-0", lastId: "1000-499",
    });
    let oldest = "1000-0";
    for (const ms of [999, 998, 997]) {
      loadEarlierBtn().onclick();
      await answer({ entries: pageOf(ms), columns: ["a"], more: true, firstId: null, lastId: null });
      // The request asked for the page BEFORE the then-oldest row - a stuck cursor
      // would re-send the same before= forever (asserted after the answer so a
      // failure here cannot leak a half-flight request into the next test).
      expect(requests[requests.length - 1].url).toContain("before=" + encodeURIComponent(oldest));
      const ids = rowIds();
      expect(ids.length, "the cap holds at 500 rows").toBe(500);
      expect(ids[0]).toBe(ms + "-499"); // the fetched page is the table now
      expect(ids[499], "the fetched page's oldest row SURVIVED the cap").toBe(ms + "-0");
      for (let i = 1; i < ids.length; i++) {
        expect(newer(ids[i - 1], ids[i]), "rows stay strictly newest-first: " + ids[i - 1] + " then " + ids[i]).toBe(true);
      }
      expect(newer(oldest, ids[499]), "the cursor moved strictly older").toBe(true);
      oldest = ids[499];
    }
    expect(requests.length).toBe(3);
  });

  it("a page that answers more=false ends the walk and paints the beginning-of-stream line", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("5-0", { a: "1" }), entry("4-0", { a: "2" })],
      columns: ["a"], more: true, firstId: "4-0", lastId: "5-0",
    });
    loadEarlierBtn().onclick();
    await answer({ entries: [entry("3-0", { a: "3" })], columns: ["a"], more: false, firstId: null, lastId: null });
    expect(rowIds()).toEqual(["5-0", "4-0", "3-0"]);
    expect(loadEarlierBtn(), "the walk's end takes the button with it").toBeUndefined();
    expect(text(byId.dbGridWrap)).toContain("Beginning of the stream");
  });
});
/* Newest-first compare for assertions - the same tuple math the view runs, kept
   independent here so a broken comparator cannot pass by agreeing with itself. */
const newer = (a: string, b: string): boolean => {
  const [ams, aseq] = a.split("-");
  const [bms, bseq] = b.split("-");
  if (ams !== bms) return Number(ams) > Number(bms);
  return Number(aseq) > Number(bseq);
};

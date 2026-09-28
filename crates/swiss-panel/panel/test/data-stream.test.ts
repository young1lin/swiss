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

import { describe, it, expect, beforeEach } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dbConn, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";
import type { ApiDbRedisValue, ApiDbStreamGroupRow, ApiDbStreamWindow } from "../src/types/api.js";

/* The stream view's tests: the pure merge/cap/columns folds (docs/45 S2), plus the DOM
 * walk - Load-earlier against a parked fetch, the docs/43 M3 cellmenu technique. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, attrs: {} as Record<string, string>, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) {
      if (c.tag === "#document-fragment") { c.children.forEach((k: Stub) => { k.parentNode = n; n.children.push(k); }); c.children = []; return c; }
      c.parentNode = n; n.children.push(c); return c;
    },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() { const p = n.parentNode as Stub | undefined; if (p) p.children = p.children.filter((x: Stub) => x !== n); },
    contains: () => false, closest: () => null,
    setAttribute(k: string, v: string) { n.attrs[k] = v; if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: (k: string) => n.attrs[k] ?? "", removeAttribute(k: string) { delete n.attrs[k]; },
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith(x: Stub) {
      const p = n.parentNode as Stub | undefined;
      if (!p) return;
      const i = p.children.indexOf(n);
      if (i >= 0) { p.children[i] = x; x.parentNode = p; n.parentNode = undefined; }
    },
    insertAdjacentHTML() {},
  };
  // renderDbGrid wipes the pane with `wrap.textContent = ""` (a real DOM clear); the
  // stub honors it the same way, or every repaint APPENDS and the walk reads a stale table.
  // Any assignment replaces the children, as the DOM does - a label rewritten in place
  // (the groups button's count) must not read as old text plus new.
  let tc = "";
  Object.defineProperty(n, "textContent", {
    get: () => { return tc; },
    set: (v: string) => { tc = v == null ? "" : v; n.children = []; },
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
  streamPool: (pending: StreamEntry[], page: StreamEntry[], cap: number) => { pending: StreamEntry[]; dropped: boolean };
  streamColumns: (rows: StreamEntry[]) => string[];
  streamRateText: (dLen: number, dtMs: number) => string;
  isTopPinned: (scrollTop: number, rowHeight: number) => boolean;
  dbStreamTick: () => Promise<void>;
};
const browsers = await import(pathToFileURL(join(here, "data-browsers.ts")).href) as {
  dbRenderRedisValue: (wrap: Stub) => void;
};
interface StreamEntry { id: string; ts: string | null; fields: Record<string, string> }
const streamMerge = stream.streamMerge;
const streamCap = stream.streamCap;
const streamPool = stream.streamPool;
const streamColumns = stream.streamColumns;
const streamRateText = stream.streamRateText;

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

// docs/45 S3 follow-up: the holdback pool behind the pill is bounded by the same cap
// as the table — overflow keeps the newest half and reports the drop (a hole in the
// middle is the gap bar's own semantic, whoever made the hole).
describe("streamPool", () => {
  it("under the cap: the page merges in and nothing drops", () => {
    const r = streamPool([entry("3-0", {}), entry("2-0", {})], [entry("4-0", {})], 500);
    expect(r.pending.map((e: StreamEntry) => e.id)).toEqual(["4-0", "3-0", "2-0"]);
    expect(r.dropped).toBe(false);
  });

  it("exactly at the cap: nothing drops", () => {
    const r = streamPool([entry("2-0", {}), entry("1-0", {})], [entry("3-0", {})], 3);
    expect(r.pending.map((e: StreamEntry) => e.id)).toEqual(["3-0", "2-0", "1-0"]);
    expect(r.dropped).toBe(false);
  });

  it("over the cap: the OLDEST falls out, the drop reports itself", () => {
    const r = streamPool(
      [entry("5-0", {}), entry("4-0", {}), entry("3-0", {})],
      [entry("7-0", {}), entry("6-0", {})],
      4,
    );
    expect(r.pending.map((e: StreamEntry) => e.id)).toEqual(["7-0", "6-0", "5-0", "4-0"]);
    expect(r.dropped).toBe(true);
  });

  it("an empty page is a no-op on content and flag", () => {
    const r = streamPool([entry("2-0", {})], [], 500);
    expect(r.pending.map((e: StreamEntry) => e.id)).toEqual(["2-0"]);
    expect(r.dropped).toBe(false);
  });
});

// docs/45 S2/D3: the columns derivation — the field union in first-seen-scanning-
// newest-first order, the same algorithm the server runs per window, re-run over the
// merged rows so an older page's new field lands at the tail.
// docs/45 §2.3: the rate readout is a pure function over two samples of XLEN, and the
// decision it carries is the negative one - a stream that SHRANK between ticks (XTRIM,
// XDEL) reads as a real negative number. Clamping to 0 would hide exactly the signal an
// operator watching a feed is looking for ("where did my entries go"), so the sign stays.
describe("streamRateText (docs/45 §2.3)", () => {
  it("no elapsed time is no sample: junk reads as empty, never a fabricated spike", () => {
    expect(streamRateText(120, 0)).toBe("");        // the first tick: no baseline yet
    expect(streamRateText(120, -1000)).toBe("");    // a clock that went backwards
    expect(streamRateText(120, NaN)).toBe("");
    expect(streamRateText(120, Infinity)).toBe("");
    expect(streamRateText(NaN, 1000)).toBe("");     // a length that never arrived
  });

  it("a flat stream reads zero - a real sample of nothing happening", () => {
    expect(streamRateText(0, 1000)).toBe("0");
    expect(streamRateText(0, 250)).toBe("0");
  });

  it("entries per second, one decimal", () => {
    expect(streamRateText(50, 1000)).toBe("50");    // the seed generator's own rate
    expect(streamRateText(5, 2000)).toBe("2.5");    // a longer window divides down
    expect(streamRateText(1, 300)).toBe("3.3");     // 3.333... rounds to one decimal
    expect(streamRateText(1, 60000)).toBe("0");     // a minute for one entry rounds to 0
  });

  it("a shrinking stream reads NEGATIVE, not zero (the S4 decision)", () => {
    // XTRIM MAXLEN cut 120 entries between two one-second ticks: the readout says so.
    expect(streamRateText(-120, 1000)).toBe("-120");
    expect(streamRateText(-1, 2000)).toBe("-0.5");
  });
});

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

// docs/45 §2.3: the edge rule. Junk counts as pinned — the edge must never be missed
// on a technicality; only a provably-scrolled view counts as history.
describe("isTopPinned", () => {
  it("scrollTop 0 is the edge", () => {
    expect(stream.isTopPinned(0, 24)).toBe(true);
  });
  it("within one row of the top still rides the edge", () => {
    expect(stream.isTopPinned(24, 24)).toBe(true); // the first row is still under the eye
  });
  it("past one row is history", () => {
    expect(stream.isTopPinned(25, 24)).toBe(false);
    expect(stream.isTopPinned(240, 24)).toBe(false);
  });
  it("junk counts as pinned: negative offset, unreadable row height", () => {
    expect(stream.isTopPinned(-5, 24)).toBe(true); // rubber-band overscroll is not reading
    expect(stream.isTopPinned(500, 0)).toBe(true); // a row that cannot be measured
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
/* requests/parked are MODULE-level arrays and the Follow timer is module state: a
   test that parks a request it never answers - or leaves Follow on - would hand the
   next test a queue that already has a ghost in it, and its first answer() would
   resolve the wrong promise. Every test starts from empty queues and a stopped
   Follow: the previous test's view may still be mounted, so pausing is a click on
   ITS button, only when the label says it is running. */
beforeEach(() => {
  requests.length = 0;
  parked.length = 0;
  const w = byId.dbGridWrap;
  if (!w) return; // nothing mounted yet - nothing to stop
  const running = find(w, (n: Stub) => n.dataset && n.dataset.stream === "follow" && n.attrs["aria-checked"] === "true")[0];
  if (running) running.onclick(); // a Follow left on stops HERE, not in the next test
});

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
const rowIds = (): string[] => {
  const tb = find(byId.dbGridWrap, (n) => n.tag === "tbody")[0]; // no table: the empty view
  return (tb ? tb.children : []).map((r: Stub) => (r.children[0] as Stub).textContent);
};
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

/* --- the Follow edge (docs/45 S3) ------------------------------------------------------------- */

/* 2026-09-28: Follow is an on/off switch, not a button whose word flips between Follow and
   Pause - the owner asked for the state to be the control ("用 UI 来替换文字"). */
const followBtn = (): Stub =>
  find(byId.dbGridWrap, (n) => n.dataset && n.dataset.stream === "follow")[0];
const following = (): boolean => followBtn().attrs["aria-checked"] === "true";
// docs/46 P7: library buttons, addressed by their data hook (the classes had no rule of their own).
const pill = (): Stub => find(byId.dbGridWrap, (n) => n.dataset && n.dataset.stream === "pill")[0];
const gapBar = (): Stub => find(byId.dbGridWrap, (n) => n.dataset && n.dataset.stream === "gap")[0];
const barText = (): string => {
  const bars = find(byId.dbGridWrap, (n) => (n.className || "").toString().split(" ").indexOf("db-detail-meta") >= 0);
  return bars.map(text).join("");
};
/* Point the scroller stub at a scroll position and a measurable row height; the tick
   reads both off #dbGridWrap the way the real DOM would answer. */
const setScroll = (top: number, rowH: number): void => {
  const w = byId.dbGridWrap as any;
  w.scrollTop = top;
  w.querySelector = rowH > 0 ? () => ({ offsetHeight: rowH }) : () => null;
};
const answerErr = async (): Promise<void> => {
  parked.splice(0, 1)[0].fail({});
  await tick();
};
const followOn = (): void => { followBtn().onclick(); }; // renderDbGrid repainted; state is on

describe("the stream Follow edge (docs/45 S3)", () => {
  it("an empty stream mounts the Follow bar too, and its tick polls after=0-0", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 0,
      entries: [], columns: [], more: false, firstId: null, lastId: null,
    });
    expect(rowIds()).toEqual([]); // no table - but the bar is there (empty hint too)
    expect(text(byId.dbGridWrap)).toContain("This stream is empty");
    expect(followBtn(), "an empty stream is still a followable one").toBeDefined();
    followOn();
    const p = stream.dbStreamTick();
    await answer({ entries: [entry("1-0", { a: "1" })], columns: ["a"], more: false, firstId: null, lastId: null, length: 1 });
    await p;
    expect(requests[requests.length - 1].url).toContain("after=0-0"); // the stream's very beginning
    expect(rowIds()).toEqual(["1-0"]); // the first append ever lands, no re-key needed
  });

  it("a tick error stops Follow: the toggle resets and the bar says why", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    expect(following(), "off until it is switched on").toBe(false);
    followOn();
    expect(following(), "the switch itself is the state").toBe(true);
    const n0 = requests.length;
    const p = stream.dbStreamTick();
    await answerErr();
    await p;
    expect(following()).toBe(false); // the switch reset - no zombie polling
    expect(barText()).toContain("Follow stopped");
    expect(requests.length).toBe(n0 + 1); // its one poll, nothing retried after the stop
  });

  it("pinned: a live-edge page inserts at the top, the pill never appears", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 3,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(0, 24);
    followOn();
    const p = stream.dbStreamTick();
    expect(requests[requests.length - 1].url).toContain("after=9-0");
    await answer({ entries: [entry("10-0", { a: "0" })], columns: ["a"], more: false, firstId: null, lastId: null, length: 3 });
    await p;
    expect(rowIds()).toEqual(["10-0", "9-0", "8-0"]); // the edge moved with the stream
    expect(pill().hidden).toBe(true); // pinned readers need no pill
  });

  it("not pinned: the table never moves - the pill counts, the click flushes at the top", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 4,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(500, 24); // deep in history, one measured row
    followOn();
    const p = stream.dbStreamTick();
    await answer({ entries: [entry("10-0", { a: "0" }), entry("11-0", { a: "x" })], columns: ["a"], more: false, firstId: null, lastId: null, length: 4 });
    await p;
    expect(rowIds()).toEqual(["9-0", "8-0"]); // NOTHING inserted under the reader
    expect(pill().hidden).toBe(false);
    expect(text(pill()).trim()).toBe("2 new entries"); // fix-plan #14: the arrow is the i-arrow-up sprite, not copy
    expect(gapBar().hidden).toBe(true); // an untruncated page is no gap
    setScroll(500, 24); // still reading; the flush itself returns to the top
    pill().onclick();
    expect(rowIds()).toEqual(["11-0", "10-0", "9-0", "8-0"]); // the pool rode the edge
    expect(pill().hidden).toBe(true);
    expect((byId.dbGridWrap as any).scrollTop).toBe(0);
  });

  it("not pinned, stream running hot: the pool holds at the cap, the pill says 500+, the flush lands at most 500", async () => {
    // docs/45 S3 follow-up: 7 ticks x 100 entries while the operator reads history —
    // the pool must never grow past the row cap, and what falls out is a hole in the
    // middle (the gap bar's own semantic), not a silent loss.
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 702,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(500, 24); // deep in history: every tick pools behind the pill
    followOn();
    const tab = dbTabs()[0];
    if (tab.kind !== "key") throw new Error("expected the key tab");
    const hundred = (ms: number): StreamEntry[] =>
      Array.from({ length: 100 }, (_v: unknown, i: number) => entry(ms + "-" + (99 - i), { a: String(i) }));
    for (let ms = 10; ms <= 16; ms++) {
      const p = stream.dbStreamTick();
      parked.splice(0, 1)[0].resolve({ entries: hundred(ms), columns: ["a"], more: false, firstId: null, lastId: null, length: 702 });
      await tick();
      if (parked.length) {
        // seven consecutive ticks must cross an every-fifth one: its consumer-group
        // follow-up parks behind the page and the tick cannot finish without it.
        parked.splice(0, 1)[0].resolve({ groups: [] });
        await tick();
      }
      await p;
      expect((tab.redisStreamPending || []).length, "tick " + ms).toBeLessThanOrEqual(500);
    }
    expect((tab.redisStreamPending || []).length, "700 arrived, 500 held, 200 dropped").toBe(500);
    expect(tab.redisStreamPendingDropped).toBe(true);
    expect(tab.redisStreamGap, "the drop is a hole in the middle — same flag as a truncated page").toBe(true);
    expect(gapBar().hidden).toBe(false);
    expect(text(pill()).trim()).toBe("500+ new entries"); // the count stopped moving, the copy says so (the arrow is the sprite)
    pill().onclick();
    const ids = rowIds();
    expect(ids.length, "pool 500 + 2 held rows, capped newest-first").toBe(500);
    expect(ids[0]).toBe("16-99");
    expect(ids[499]).toBe("12-0"); // the newest 500 of the 700 that arrived: 16/15/14/13 whole, 12 to its floor
    expect(pill().hidden).toBe(true);
  });

  it("a truncated page (more=true) paints the skipped-middle bar; jump-latest reopens with no cursor", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 402,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(0, 24);
    followOn();
    const p = stream.dbStreamTick();
    await answer({ entries: [entry("10-0", { a: "0" })], columns: ["a"], more: true, firstId: null, lastId: null, length: 402 });
    await p;
    const gap = gapBar();
    expect(gap.hidden).toBe(false);
    expect(text(gap)).toContain("skipped");
    expect(text(gap)).toContain("Jump to latest");
    // The hole does not heal by itself: a later untruncated tick fetched nothing new,
    // and the missing middle is still missing - the bar stays until the operator jumps.
    const p2 = stream.dbStreamTick();
    await answer({ entries: [], columns: ["a"], more: false, firstId: null, lastId: null, length: 402 });
    await p2;
    expect(gapBar().hidden).toBe(false);
    gap.onclick();
    await answer({ entries: [entry("12-0", { a: "z" }), entry("11-0", { a: "y" })], columns: ["a"], more: false, firstId: "11-0", lastId: "12-0", length: 402 });
    await tick();
    const jumpUrl = requests[requests.length - 1].url;
    expect(jumpUrl).not.toContain("after="); // the latest window: no cursor at all
    expect(jumpUrl).not.toContain("before=");
    expect(rowIds()).toEqual(["12-0", "11-0"]); // the rows were REPLACED, not spliced
    expect(gapBar().hidden).toBe(true); // the lie about contiguity is gone
  });

  it("a jump voids the tick in flight: the late answer pools nothing, pending stays empty", async () => {
    // docs/45 S3 follow-up: the jump reopens the window, so a tick still in flight
    // speaks for rows that no longer exist. Its answer must be dropped on arrival —
    // not pooled behind a pill whose splice target the jump just replaced.
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 402,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(500, 24); // deep in history: a late tick would pool behind the pill
    followOn();
    const tab = dbTabs()[0];
    if (tab.kind !== "key") throw new Error("expected the key tab");
    // Light the gap bar first (a truncated page), so the jump has its button.
    const warm = stream.dbStreamTick();
    await answer({ entries: [entry("10-0", { a: "0" })], columns: ["a"], more: true, firstId: null, lastId: null, length: 402 });
    await warm;
    expect(gapBar().hidden).toBe(false);
    expect((tab.redisStreamPending || []).length).toBe(1); // the pooled warm-up page
    // Now race: a tick parks unanswered, the jump issues past it and lands first.
    // Requests are taken BY ARRIVAL as named handles, so "the tick was issued first"
    // is something the sequence proves, not an index guess.
    const p = stream.dbStreamTick();
    const tickReq = parked.splice(0, 1)[0]; // the tick's page, hanging by name
    gapBar().onclick();
    const jumpReq = parked.splice(0, 1)[0]; // the jump's window, arrived after it
    expect(parked.length).toBe(0); // both spoken for; no third request exists
    jumpReq.resolve({ entries: [entry("13-0", { a: "z" }), entry("12-0", { a: "y" })], columns: ["a"], more: false, firstId: "12-0", lastId: "13-0", length: 402 });
    await tick();
    expect(rowIds()).toEqual(["13-0", "12-0"]); // the jump's window won
    tickReq.resolve({ entries: [entry("11-0", { a: "x" })], columns: ["a"], more: false, firstId: null, lastId: null, length: 402 });
    await p;
    // The tick was superseded: its page pooled NOTHING — pending is the empty pool
    // the jump left, not [11-0] spliced onto it.
    expect((tab.redisStreamPending || []).length).toBe(0);
    expect(rowIds()).toEqual(["13-0", "12-0"]); // and the table never heard of it
    expect(gapBar().hidden).toBe(true);
    expect(following()).toBe(true); // the voided tick stopped nothing
  });

  it("hidden: the tick fetches nothing; the next visible tick catches up", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(0, 24);
    followOn();
    const n0 = requests.length;
    docStub.hidden = true;
    await stream.dbStreamTick(); // returns before the fetch - zero requests while hidden
    expect(requests.length).toBe(n0);
    docStub.hidden = false;
    const p = stream.dbStreamTick();
    await answer({ entries: [entry("10-0", { a: "0" })], columns: ["a"], more: false, firstId: null, lastId: null, length: 3 });
    await p;
    expect(rowIds()).toEqual(["10-0", "9-0", "8-0"]); // caught up in one tick
  });

  /* Two tests in a row, each turning Follow on: the first LEAVES it on and drains
     its queue itself; the second must start from nothing. This is the proof the
     beforeEach isolation (empty queues, Follow stopped) actually holds. */
  it("isolation, first of a pair: a Follow left ON still drains every request it parked", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: false, firstId: "8-0", lastId: "9-0",
    });
    followOn();
    const p = stream.dbStreamTick();
    await answer({ entries: [], columns: ["a"], more: false, firstId: null, lastId: null, length: 2 });
    await p;
    expect(parked.length).toBe(0); // this test consumed what it issued
    // Follow is deliberately LEFT ON for the next test.
  });

  it("isolation, second of the pair: starts with empty queues even after a Follow was left on", async () => {
    expect(requests.length, "beforeEach emptied the shared request queue").toBe(0);
    expect(parked.length, "beforeEach emptied the shared parked queue").toBe(0);
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("7-0", { a: "1" }), entry("6-0", { a: "2" })], columns: ["a"], more: false, firstId: "6-0", lastId: "7-0",
    });
    followOn();
    expect(requests.length, "turning Follow on issues nothing by itself").toBe(0);
    expect(following()).toBe(true);
  });

  /* The owner's report: with Follow on, the interval picker snapped shut every second - each
     pinned tick ran renderDbGrid, which rebuilt the whole pane, bar and picker included. A
     tick is new ROWS: it replaces the table and nothing else. */
  it("a pinned tick replaces the table only - the Follow bar and its interval picker stay the same nodes", async () => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: true, firstId: "8-0", lastId: "9-0",
    });
    setScroll(0, 24);
    followOn();
    const bar = (): Stub => find(byId.dbGridWrap, (n) => (n.className || "").toString().split(" ").indexOf("db-detail-meta") >= 0)[0];
    const picker = (): Stub => find(byId.dbGridWrap, (n) => n.tag === "select")[0];
    const table = (): Stub => find(byId.dbGridWrap, (n) => n.tag === "table")[0];
    const bar0 = bar();
    const picker0 = picker();
    const p = stream.dbStreamTick();
    await answer({ entries: [entry("10-0", { a: "0" })], columns: ["a"], more: false, firstId: null, lastId: null, length: 3 });
    await p;
    expect(rowIds(), "the new row is on screen").toEqual(["10-0", "9-0", "8-0"]);
    expect(bar(), "the bar was not rebuilt").toBe(bar0);
    expect(picker(), "the interval picker was not rebuilt").toBe(picker0);

    // A tick with nothing new touches no node at all.
    const table0 = table();
    const p2 = stream.dbStreamTick();
    await answer({ entries: [], columns: [], more: false, firstId: null, lastId: null, length: 3 });
    await p2;
    expect(table(), "an empty tick leaves the table alone").toBe(table0);
    expect(bar()).toBe(bar0);
  });
});

/* --- the consumer-group fold (docs/45 §2.4) ------------------------------------------------------ */

const groupsBtn = (): Stub =>
  find(byId.dbGridWrap, (n) => n.tag === "button" && text(n).indexOf("Consumer groups") === 0)[0];
/* The fold's table is the one whose first header cell is "group" - the stream table's
   own headers are id/time/fields, so this tells the two tables apart. */
const groupsTable = (): Stub | undefined =>
  find(byId.dbGridWrap, (n) => n.tag === "table")
    .find((tb: Stub) => tb.children[0] && tb.children[0].children[0]
      && text(tb.children[0].children[0].children[0]) === "group");
const groupsRows = (tb: Stub): string[][] =>
  (tb.children[1] ? tb.children[1].children : []).map((r: Stub) => r.children.map((c: Stub) => c.textContent));

describe("the consumer-group fold (docs/45 §2.4)", () => {
  const groupRow = (over: Partial<ApiDbStreamGroupRow>): ApiDbStreamGroupRow =>
    Object.assign({ name: "feed", consumers: 1, pending: 7, lag: 9993, "last-delivered-id": "1700000000600-0" }, over);
  const mountTwo = (): void => {
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 2,
      entries: [entry("9-0", { a: "1" }), entry("8-0", { a: "2" })], columns: ["a"], more: false, firstId: "8-0", lastId: "9-0",
    });
  };
  const theTab = (): Stub & Record<string, unknown> => {
    const tab = dbTabs()[0];
    if (tab.kind !== "key") throw new Error("expected the key tab");
    return tab as unknown as Stub & Record<string, unknown>;
  };

  it("closed by default; the button opens the fold, and closes it again", () => {
    mountTwo();
    const tab = theTab();
    expect(groupsTable(), "closed by default").toBeUndefined();
    expect(text(groupsBtn())).toBe("Consumer groups"); // no groups loaded yet: no count
    (tab.redisStreamGroups as unknown) = [groupRow({})];
    groupsBtn().onclick(); // open — the click repaints, label included
    const tb = groupsTable();
    expect(tb, "the fold's table appeared").toBeDefined();
    expect(groupsRows(tb as Stub)).toEqual([["feed", "1", "7", "9993", "1700000000600-0"]]);
    expect(text(groupsBtn())).toBe("Consumer groups (1)"); // the label carries the count
    groupsBtn().onclick(); // close
    expect(groupsTable()).toBeUndefined();
  });

  it("opening the fold reads the groups itself - Follow off, nothing pre-loaded", async () => {
    // The 2026-09-23 walk found this on 19997: with Follow off the fold only ever showed
    // its empty state, because the groups answer arrived on Follow's fifth tick and
    // nowhere else. A reader who opened the fold on a stream WITH a group was told it had
    // none - the empty state asserting a fact no request had checked.
    mountTwo();
    expect(requests.length, "mounting a stream asks for no groups").toBe(0);
    groupsBtn().onclick(); // open
    expect(requests.length, "opening the fold pays its own one command").toBe(1);
    expect(requests[0].url).toContain("/stream/groups");
    expect(text(byId.dbGridWrap), "before the answer it says reading, not none")
      .toContain("Reading consumer groups");
    parked.splice(0, 1)[0].resolve({ groups: [groupRow({})] });
    await tick();
    expect(groupsRows(groupsTable() as Stub)).toEqual([["feed", "1", "7", "9993", "1700000000600-0"]]);
    expect(text(groupsBtn())).toBe("Consumer groups (1)");
  });

  it("a server that answers with no groups still reads as none, not as loading", async () => {
    mountTwo();
    groupsBtn().onclick();
    parked.splice(0, 1)[0].resolve({ groups: [] });
    await tick();
    expect(text(byId.dbGridWrap)).toContain("No consumer groups on this stream.");
    expect(groupsRows(groupsTable() as Stub)).toEqual([]);
  });

  it("closing and reopening the fold refreshes it; a closed fold costs nothing", async () => {
    mountTwo();
    groupsBtn().onclick();
    parked.splice(0, 1)[0].resolve({ groups: [groupRow({})] });
    await tick();
    groupsBtn().onclick(); // close
    expect(groupsTable(), "closed").toBeUndefined();
    const closed = requests.length;
    expect(closed, "closing asks for nothing").toBe(1);
    groupsBtn().onclick(); // reopen
    expect(requests.length - closed, "reopening re-reads - pending moves while you are away").toBe(1);
    parked.splice(0, 1)[0].resolve({ groups: [groupRow({ pending: 3 })] });
    await tick();
    expect(groupsRows(groupsTable() as Stub)[0][2], "the fold shows the fresher pending").toBe("3");
  });

  it("lag null renders as an em dash — the honest pre-7.0 answer, not a zero", () => {
    mountTwo();
    const tab = theTab();
    (tab.redisStreamGroups as unknown) = [groupRow({ lag: null, "last-delivered-id": null })];
    groupsBtn().onclick();
    expect(groupsRows(groupsTable() as Stub)).toEqual([["feed", "1", "7", "—", ""]]);
  });

  it("an empty groups list keeps the fold but shows its empty state", () => {
    mountTwo();
    const tab = theTab();
    (tab.redisStreamGroups as unknown) = [];
    groupsBtn().onclick();
    const tb = groupsTable();
    expect(tb, "the header row survives an empty list").toBeDefined();
    expect(groupsRows(tb as Stub)).toEqual([]);
    expect(text(byId.dbGridWrap)).toContain("No consumer groups on this stream.");
    expect(text(groupsBtn())).toBe("Consumer groups (0)");
  });

  it("the fifth tick polls /stream/groups; the first four do not", async () => {
    mountTwo();
    setScroll(0, 24); // pinned: every tick's repaint also repaints the fold's label
    followOn();
    for (let i = 1; i <= 5; i++) {
      const before = requests.length;
      const p = stream.dbStreamTick();
      parked.splice(0, 1)[0].resolve({ entries: [], columns: ["a"], more: false, firstId: null, lastId: null, length: 2 });
      await tick();
      if (i === 5) {
        expect(requests.length - before, "the fifth tick adds the groups poll on top of its page").toBe(2);
        expect(requests[requests.length - 1].url).toContain("/stream/groups");
        parked.splice(0, 1)[0].resolve({ groups: [groupRow({})] });
        await tick();
      } else {
        expect(requests.length - before, "an ordinary tick is its page alone").toBe(1);
        expect(requests[requests.length - 1].url).not.toContain("/stream/groups");
      }
      await p;
    }
    const tab = theTab();
    expect((tab.redisStreamGroups as { length: number } | null)?.length).toBe(1);
    expect(text(groupsBtn())).toBe("Consumer groups (1)"); // the pinned repaint carried the count
    groupsBtn().onclick();
    expect(groupsRows(groupsTable() as Stub)).toEqual([["feed", "1", "7", "9993", "1700000000600-0"]]);
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

/* --- reading a fast stream (docs/49) ------------------------------------------------------------ */

/* The feed that made this necessary: 9 symbols x 10 entries a second is 90 rows a second,
   and a 500-row table turns over in under six seconds. Filtering, the summary strip and
   the hold are the three things that make that readable. */
const streamSummary = (stream as unknown as {
  streamSummary: (rows: StreamEntry[], field: string) => { value: string; n: number; rate: string }[];
}).streamSummary;
const streamAutoField = (stream as unknown as {
  streamAutoField: (rows: StreamEntry[], cols: string[]) => string | null;
}).streamAutoField;
const dbStreamQuote = (stream as unknown as { dbStreamQuote: (v: string) => string }).dbStreamQuote;

describe("docs/49 S2.3 - the summary strip's folds", () => {
  const tickRows = (n: number, syms: string[]): StreamEntry[] =>
    Array.from({ length: n }, (_v: unknown, i: number) =>
      entry(String(1000000 - i * 100) + "-0", { symbol: syms[i % syms.length], price: String(100 + i) }));

  it("counts each value of the field and rates it over the held window's own span", () => {
    // Ten rows one tenth of a second apart: 0.9 s of span, so a value seen 5 times
    // reads 5.6/s. One denominator for every value, so the rates add up to the stream's.
    const rows = tickRows(10, ["A", "B"]);
    expect(streamSummary(rows, "symbol")).toEqual([
      { value: "A", n: 5, rate: "5.6" },
      { value: "B", n: 5, rate: "5.6" },
    ]);
  });

  it("puts the busiest first and breaks a tie by value, so the strip does not reshuffle per tick", () => {
    const rows = [
      entry("9-0", { symbol: "B" }), entry("8-0", { symbol: "A" }),
      entry("7-0", { symbol: "C" }), entry("6-0", { symbol: "C" }),
    ];
    expect(streamSummary(rows, "symbol").map((r) => r.value)).toEqual(["C", "A", "B"]);
  });

  it("reports no rate when there is no span, and skips rows without the field", () => {
    // One row has no span; a count over zero seconds is infinity, and infinity is
    // not a readout. A row missing the field is not a row with an empty value.
    expect(streamSummary([entry("9-0", { symbol: "A" })], "symbol")).toEqual([{ value: "A", n: 1, rate: "" }]);
    const mixed = [entry("9-0", { symbol: "A" }), entry("8-0", { other: "x" })];
    expect(streamSummary(mixed, "symbol")).toEqual([{ value: "A", n: 1, rate: "1000" }]);
    expect(streamSummary([], "symbol")).toEqual([]);
  });

  it("opens on the first column that repeats itself, never on one value per row", () => {
    const rows = tickRows(20, ["A", "B", "C"]);
    // `price` is one value per row - grouping by it would draw 20 chips nobody reads.
    expect(streamAutoField(rows, ["price", "symbol"])).toBe("symbol");
    expect(streamAutoField(rows, ["price"])).toBe(null);
    expect(streamAutoField(rows.slice(0, 3), ["symbol"]), "too few rows to tell").toBe(null);
    const oneValue = Array.from({ length: 8 }, (_v, i) => entry(String(9 - i) + "-0", { symbol: "A" }));
    expect(streamAutoField(oneValue, ["symbol"]), "one value is not a grouping").toBe(null);
  });

  it("quotes a chip's value only when the filter line would otherwise split it", () => {
    expect(dbStreamQuote("NVDA")).toBe("NVDA");
    expect(dbStreamQuote("600519.SH")).toBe("600519.SH");
    expect(dbStreamQuote("two words")).toBe("\"two words\"");
    expect(dbStreamQuote("say \"hi\"")).toBe("\"say \\\"hi\\\"\"");
  });
});

describe("docs/49 - the reading bar on a fast stream", () => {
  const filterBox = (): Stub => find(byId.dbGridWrap, (n) => n.tag === "input")[0];
  const chips = (): Stub[] => find(byId.dbGridWrap, (n) => n.dataset && n.dataset.schip !== undefined);
  const groupSel = (): Stub => find(byId.dbGridWrap, (n) => n.tag === "select" && n.className === "db-stream-by")[0];
  const theTab = (): Stub & Record<string, unknown> => {
    const tab = dbTabs()[0];
    if (tab.kind !== "key") throw new Error("expected the key tab");
    return tab as unknown as Stub & Record<string, unknown>;
  };
  const typeFilter = (line: string): Promise<void> => {
    const box = filterBox();
    box.value = line;
    box.onkeydown({ key: "Enter", preventDefault: () => {} }); // Enter applies now, no debounce
    return tick() as Promise<void>;
  };
  const fast = (): StreamEntry[] =>
    Array.from({ length: 9 }, (_v: unknown, i: number) =>
      entry(String(1000000 - i * 10) + "-0", { symbol: i % 3 === 0 ? "NVDA" : "AAPL", price: String(100 + i) }));
  const mountFast = (): Stub => mountStream({
    key: "s", type: "stream", ttl: -1, length: 90000,
    entries: fast(), columns: ["symbol", "price"], more: true,
    firstId: "1-0", lastId: "1000000-0",
  });

  it("sends the filter with the window, replaces the rows, and says what the walk cost", async () => {
    mountFast();
    expect(chips().length, "the strip opens on the repeating column").toBe(2);
    const p = typeFilter("symbol=NVDA");
    await tick();
    expect(requests[requests.length - 1].url).toContain("match=" + encodeURIComponent("symbol=NVDA"));
    await answer({
      entries: [entry("1000000-0", { symbol: "NVDA", price: "100" })],
      columns: ["symbol", "price"], more: true, firstId: null, lastId: null, length: 90000,
      scanned: 20000, scannedFrom: "1000000-0", scannedTo: "500000-0",
    });
    await p;
    expect(rowIds(), "the rows that answered the OLD question are gone").toEqual(["1000000-0"]);
    expect(barText()).toContain("1 kept of the newest 20000 examined");
    expect(theTab().redisStreamScanTo).toBe("500000-0");
  });

  it("the follow tick under a filter polls from the newest id EXAMINED, not the newest kept", async () => {
    mountFast();
    const p0 = typeFilter("symbol=NVDA");
    await tick();
    await answer({
      entries: [entry("900000-0", { symbol: "NVDA" })], columns: ["symbol"], more: false,
      firstId: null, lastId: null, length: 90000,
      scanned: 500, scannedFrom: "1000000-0", scannedTo: "900000-0",
    });
    await p0;
    followOn();
    const p = stream.dbStreamTick();
    await answer({
      entries: [], columns: [], more: false, firstId: null, lastId: null, length: 90100,
      scanned: 90, scannedFrom: "1000100-0", scannedTo: "1000010-0",
    });
    await p;
    const url = requests[requests.length - 1].url;
    expect(url, "the cursor is the examined edge - not the kept row, which would re-read every tick")
      .toContain("after=" + encodeURIComponent("1000000-0"));
    expect(url).toContain("match=");
    // A tick that matched nothing still moves the cursor on: that is the whole point.
    const p2 = stream.dbStreamTick();
    await answer({ entries: [], columns: [], more: false, firstId: null, lastId: null, length: 90200 });
    await p2;
    expect(requests[requests.length - 1].url).toContain("after=" + encodeURIComponent("1000100-0"));
  });

  it("Load earlier under a filter resumes where the walk STOPPED looking", async () => {
    mountFast();
    const p0 = typeFilter("symbol=NVDA");
    await tick();
    await answer({
      entries: [entry("900000-0", { symbol: "NVDA" })], columns: ["symbol"], more: true,
      firstId: null, lastId: null, length: 90000,
      scanned: 20000, scannedFrom: "1000000-0", scannedTo: "800000-0",
    });
    await p0;
    loadEarlierBtn().onclick();
    await tick();
    expect(requests[requests.length - 1].url).toContain("before=" + encodeURIComponent("800000-0"));
    await answer({
      entries: [entry("700000-0", { symbol: "NVDA" })], columns: ["symbol"], more: true,
      firstId: null, lastId: null, scanned: 20000, scannedFrom: "800000-0", scannedTo: "600000-0",
    });
    expect(barText(), "the hits line counts the WHOLE walk back, not this page")
      .toContain("2 kept of the newest 40000 examined");
    expect(theTab().redisStreamScanTo).toBe("600000-0");
  });

  it("a chip filters to its own value, and a filter that matches nothing keeps its box", async () => {
    mountFast();
    const aapl = chips().find((c: Stub) => c.dataset.schip === "AAPL");
    expect(aapl, "the strip drew a chip for the value this test clicks").toBeDefined();
    const p = (aapl as Stub).onclick();
    await tick();
    expect(requests[requests.length - 1].url).toContain("match=" + encodeURIComponent("symbol=AAPL"));
    await answer({
      entries: [], columns: [], more: false, firstId: null, lastId: null, length: 90000,
      scanned: 20000, scannedFrom: "1000000-0", scannedTo: "500000-0",
    });
    await p;
    expect(rowIds()).toEqual([]);
    expect(text(byId.dbGridWrap)).toContain("Nothing in the entries examined matches");
    expect(filterBox(), "the one control that can undo the emptiness stays").toBeDefined();
    expect(filterBox().value, "and it keeps the line, so it can be corrected").toBe("symbol=AAPL");
  });

  it("an automatic pick stays picked while its column is still there", async () => {
    // Found on the 19998 walk: filtering to one symbol leaves that column ONE value,
    // which stops looking like a dimension, and the strip jumped to `price` - a
    // different question answered by the same strip, exactly when the operator had
    // just narrowed the first one.
    mountFast();
    expect(chips().map((c: Stub) => c.dataset.schip)).toEqual(["AAPL", "NVDA"]);
    const p = typeFilter("symbol=NVDA");
    await tick();
    await answer({
      entries: fast().filter((e: StreamEntry) => e.fields.symbol === "NVDA"),
      columns: ["symbol", "price"], more: true, firstId: null, lastId: null, length: 90000,
      scanned: 500, scannedFrom: "1000000-0", scannedTo: "900000-0",
    });
    await p;
    expect(chips().map((c: Stub) => c.dataset.schip), "still symbol, now one value")
      .toEqual(["NVDA"]);
  });

  it("the group-by picker overrides the automatic choice, and off means off", () => {
    mountFast();
    expect(chips().map((c: Stub) => c.dataset.schip)).toEqual(["AAPL", "NVDA"]);
    const sel = groupSel();
    sel.value = "";
    sel.onchange();
    expect(chips().length, "off is a deliberate choice and outlasts the auto pick").toBe(0);
    sel.value = "price";
    sel.onchange();
    expect(chips().length, "grouping by a measurement is the operator's call to make").toBe(9);
  });

  it("the strip is bounded: the busiest dozen, then a count of the rest", () => {
    // A field with two dozen values would push the table off the screen the strip is
    // there to summarise. The chips are already sorted busiest-first, so the tail is
    // the part worth counting rather than drawing.
    mountStream({
      key: "s", type: "stream", ttl: -1, length: 40,
      entries: Array.from({ length: 40 }, (_v: unknown, i: number) =>
        entry(String(1000000 - i * 10) + "-0", { box: "v" + String(i % 20) })),
      columns: ["box"], more: false, firstId: null, lastId: null,
    });
    const painted = chips();
    expect(painted.length).toBe(12);
    expect(text(byId.dbGridWrap)).toContain("+8 more");
  });

  it("the pointer resting on the table holds the live edge, and moving away resumes it", async () => {
    mountFast();
    followOn();
    setScroll(0, 22); // pinned at the edge: without the hold these rows would insert
    const table = find(byId.dbGridWrap, (n: Stub) => n.tag === "table")[0];
    table.onpointerenter();
    const p = stream.dbStreamTick();
    await answer({
      entries: [entry("1000010-0", { symbol: "NVDA" })], columns: ["symbol"], more: false,
      firstId: null, lastId: null, length: 90001,
    });
    await p;
    expect(rowIds()[0], "the row under the reader's eyes did not move").toBe("1000000-0");
    expect(barText()).toContain("held while you read");
    expect(text(pill())).toContain("1 new entry");
    table.onpointerleave();
    const p2 = stream.dbStreamTick();
    await answer({
      entries: [entry("1000020-0", { symbol: "AAPL" })], columns: ["symbol"], more: false,
      firstId: null, lastId: null, length: 90002,
    });
    await p2;
    expect(rowIds()[0], "the pooled page rides the edge with the next tick").toBe("1000020-0");
    expect(rowIds()[1]).toBe("1000010-0");
  });
});

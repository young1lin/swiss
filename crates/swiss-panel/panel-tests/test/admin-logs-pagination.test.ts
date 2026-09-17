import { describe, it, expect, beforeAll, beforeEach } from "vitest";

/* docs/32 B1 — the Logs pager becomes a transaction. The suite drives the REAL module graph
   (wireTabBody -> callsPageStep -> loadCalls -> renderCallsOnly) under the FakeNode micro-DOM,
   with a fetch queue the test resolves by hand — the only way to make a stale response land
   after a newer one under Node. Assertions never trust a bare logsBody string: they check
   state, issued requests, painted tabbody HTML and the patched pager chrome. */

class FakeNode {
  tag: string;
  attrs: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  textContent = "";
  id = "";
  type = "button";
  disabled = false;
  value = "";
  hidden = false;
  checked = false;
  title = "";
  style: Record<string, string> = {};
  children: FakeNode[] = [];
  onclick: ((ev?: { detail?: number; stopPropagation?(): void; preventDefault?(): void; key?: string }) => void) | null = null;
  oninput: (() => void) | null = null;
  onkeydown: ((ev: { key: string; preventDefault(): void }) => void) | null = null;
  onchange: (() => void) | null = null;
  selectionStart = 0;
  selectionEnd = 0;
  scrollTop = 0;
  removed = false;
  replacedBy: FakeNode | null = null;
  focused = 0;
  inserted: Array<[string, string]> = [];
  paints: string[] = [];
  private _html = "";
  private _rect = { top: 0, left: 0, bottom: 22, right: 40, width: 40, height: 22 };
  constructor(tag: string) { this.tag = tag.toUpperCase(); }
  get innerHTML(): string { return this._html; }
  set innerHTML(v: string) { this._html = v; this.paints.push(v); }
  getBoundingClientRect() { return this._rect; }
  setRect(top: number) { this._rect = { top, left: 0, bottom: top + 22, right: 40, width: 40, height: 22 }; }
  setAttribute(k: string, v: string) { this.attrs[k] = v; if (k === "id") this.id = v; }
  getAttribute(k: string) { return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null; }
  removeAttribute(k: string) { delete this.attrs[k]; }
  querySelector(): FakeNode | null { return null; }
  querySelectorAll(): FakeNode[] { return []; }
  contains(): boolean { return false; }
  closest(): FakeNode | null { return null; }
  appendChild(n: FakeNode) { this.children.push(n); return n; }
  removeChild(n: FakeNode) { this.children = this.children.filter((c) => c !== n); return n; }
  remove() { this.removed = true; }
  replaceWith(n: FakeNode) { this.replacedBy = n; }
  insertAdjacentHTML(pos: string, html: string) { this.inserted.push([pos, html]); }
  addEventListener() {} removeEventListener() {}
  dispatchEvent() { return true; }
  focus() { this.focused++; doc.activeElement = this; }
  blur() { doc.activeElement = null; }
  select() {}
  click() { if (this.onclick) this.onclick({ detail: 1 }); }
  setSelectionRange(a: number, b: number) { this.selectionStart = a; this.selectionEnd = b; }
}

const byId = new Map<string, FakeNode>();
const doc = {
  getElementById(id: string) {
    if (!byId.has(id)) byId.set(id, new FakeNode("div"));
    return byId.get(id)!;
  },
  createElement: (t: string) => new FakeNode(t),
  createTextNode: (s: string) => ({ text: s }),
  querySelector: (): FakeNode | null => null,
  querySelectorAll: (): FakeNode[] => [],
  body: new FakeNode("body"),
  documentElement: new FakeNode("html"),
  head: new FakeNode("head"),
  addEventListener() {}, removeEventListener() {},
  activeElement: null as FakeNode | null,
  hidden: false,
  visibilityState: "visible",
};

/* The fetch queue: every call parks until the test resolves it (ok / HTTP error / network
   rejection), so response ORDER is the test's to choose. */
const requests: Array<{ url: string; method?: string }> = [];
const parked: Array<{ ok(body: any): void; http(body: any, status?: number): void; net(): void }> = [];

let detail: typeof import("../../src/admin_assets/js/detail.js");
let logs: typeof import("../../src/admin_assets/js/logs.js");
let util: typeof import("../../src/admin_assets/js/util.js");
let rh: typeof import("../../src/admin_assets/js/run-history.js");

beforeAll(async () => {
  (globalThis as unknown as Record<string, unknown>).document = doc;
  (globalThis as unknown as Record<string, unknown>).window = { addEventListener() {}, innerWidth: 1280, innerHeight: 800 };
  (globalThis as unknown as Record<string, unknown>).localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  (globalThis as unknown as Record<string, unknown>).location = { origin: "http://127.0.0.1:19998", reload() {} };
  (globalThis as unknown as Record<string, unknown>).confirm = () => false;
  (globalThis as unknown as Record<string, unknown>).alert = () => {};
  (globalThis as unknown as Record<string, unknown>).prompt = () => "";
  (globalThis as unknown as Record<string, unknown>).fetch = ((url: any, opts: any) => {
    requests.push({ url: String(url), method: opts && opts.method });
    return new Promise((res, rej) => {
      parked.push({
        ok: (body: any) => res({ ok: true, status: 200, json: async () => body }),
        http: (body: any, status = 500) => res({ ok: false, status, json: async () => body }),
        net: () => rej(new TypeError("fetch failed")),
      });
    });
  }) as any;
  detail = await import("../../src/admin_assets/js/detail.js");
  logs = await import("../../src/admin_assets/js/logs.js");
  util = await import("../../src/admin_assets/js/util.js");
  rh = await import("../../src/admin_assets/js/run-history.js");
});

const tick = () => new Promise((r) => setTimeout(r, 0));
const ok = async (body: any, index = 0) => { parked.splice(index, 1)[0].ok(body); await tick(); };
const http = async (body: any, index = 0, status = 500) => { parked.splice(index, 1)[0].http(body, status); await tick(); };
const net = async (index = 0) => { parked.splice(index, 1)[0].net(); await tick(); };

function callRow(seq: number) {
  return { seq, at: "2026-09-17T10:00:00.000Z", via: "panel", client: "panel", ms: 5, chars: 100, ok: true, tool: "get_" + seq, args: "a" + seq, output: "o" + seq, preview: false };
}
function rowsOf(seqs: number[]) { return seqs.map(callRow); }
const PAGE0 = rowsOf(Array.from({ length: 20 }, (_, i) => 100 - i)); // seq 100..81
const PAGE1 = rowsOf(Array.from({ length: 20 }, (_, i) => 80 - i));  // seq 80..61

function fakeDetail(name = "redis") {
  const kd = { items: [], nextCursor: undefined, total: undefined, loaded: false, loading: false, error: null, cursors: [""], pageSize: 50 };
  return {
    name, tab: "logs", config: { type: "http", url: "http://x" }, source: "managed",
    editing: false, editMode: null, editType: null, editVals: null, revisions: null,
    tools: { ...kd }, resources: { ...kd }, prompts: { ...kd },
    run: { tool: null, result: null, running: false, hist: null, histTool: null, histLoading: false, histOpen: false, histSelSeq: null, histFull: {}, histQ: "" },
    calls: null, stderr: "", callsOpen: {}, callsPage: 0, callsMore: false, callsFull: {}, callsQ: "",
    // docs/32 B1 fields, seeded the way openDetail builds them — the generation counter must
    // start at a number, or every response would look stale to it.
    callsPendingPage: null, callsError: "", callsErrStatus: "", callsRetryTarget: null, callsRequest: 0,
  } as any;
}

/** Mount the Logs tab of a detail: paint it once through the real renderCallsOnly (which also
 *  wires the pager through the real wireTabBody), and hand back the tabbody node. */
function mountLogs(d: any, opts: { rows?: any[]; more?: boolean; q?: string } = {}) {
  d.calls = opts.rows ?? PAGE0;
  d.callsMore = opts.more ?? true;
  d.callsQ = opts.q ?? "";
  util.state.detail = d;
  util.state.selected = d.name;
  util.state.mcps = [];
  byId.set("pane", new FakeNode("main"));
  const tb = new FakeNode("div");
  byId.set("tabbody", tb);
  rh.renderCallsOnly();
  return tb;
}

/** Click Older through the REAL wiring (mouse: detail > 0). */
function clickOlder() { byId.get("clNext")!.onclick!({ detail: 1 }); }
function clickNewer() { byId.get("clPrev")!.onclick!({ detail: 1 }); }

beforeEach(() => {
  byId.clear();
  doc.activeElement = null;
  requests.length = 0;
  parked.length = 0;
  util.state.detail = null;
  util.state.selected = null;
  util.state.mcps = [];
  util.state.groups = ["default"];
  util.state.menuOpen = false;
});

describe("docs/32 B1: a page switch is a transaction", () => {
  it("pending keeps the committed page, its rows and the scroll container — no repaint to a shell", async () => {
    const d = fakeDetail();
    const tb = mountLogs(d, { q: "GET" });
    expect(tb.paints.length).toBe(1);
    const painted = tb.innerHTML;
    expect(painted).toContain('data-seq="100"'); // the committed rows are on screen
    clickOlder();
    expect(requests.length).toBe(1);
    expect(requests[0].url).toContain("/api/mcps/redis/calls?page=1&q=GET");
    expect(d.callsPage, "the committed page is untouched while pending").toBe(0);
    expect(d.callsPendingPage).toBe(1);
    expect(d.calls, "the rows are not cleared to a loading shell").toEqual(PAGE0);
    expect(tb.paints.length, "pending patches in place — the tabbody is not repainted").toBe(1);
    expect(tb.innerHTML).toContain('data-seq="100"');
  });

  it("pending marks the region busy and disables both directions while the number stays the committed page", () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    expect(byId.get("clPrev")!.disabled).toBe(true);
    expect(byId.get("clNext")!.disabled).toBe(true);
    expect(byId.get("clStatus")!.innerHTML).toContain("Page 1");
    expect(byId.get("clStatus")!.innerHTML).toContain("Loading…");
    expect(byId.get("callsRegion")!.attrs["aria-busy"]).toBe("true");
  });

  it("success commits page, rows, more and stderr together", async () => {
    const d = fakeDetail();
    const tb = mountLogs(d);
    clickOlder();
    await ok({ calls: PAGE1, more: false, stderr: "" }, 0);
    expect(d.callsPage).toBe(1);
    expect(d.callsPendingPage).toBeNull();
    expect(d.calls).toEqual(PAGE1);
    expect(d.callsMore).toBe(false);
    expect(tb.paints.length, "the commit repaints once").toBe(2);
    expect(tb.innerHTML).toContain('data-seq="80"');
    expect(tb.innerHTML).toContain(">Page 2<");
    expect(tb.innerHTML).toContain('aria-busy="false"');
    expect(tb.innerHTML).not.toMatch(/id="clPrev"[^>]*disabled/);
    expect(tb.innerHTML).toMatch(/id="clNext"[^>]*disabled/); // last page: Older is done
  });

  it("failure commits nothing: the old page stays and the error arrives with Retry, in place", async () => {
    const d = fakeDetail();
    const tb = mountLogs(d);
    clickOlder();
    await http({ error: "boom" }, 0, 502);
    expect(d.callsPage).toBe(0);
    expect(d.callsPendingPage).toBeNull();
    expect(d.calls, "the committed rows stay").toEqual(PAGE0);
    expect(d.callsError).toBe("Could not load calls.");
    expect(d.callsRetryTarget).toBe(1);
    expect(tb.paints.length, "the failure also patches in place").toBe(1);
    const inserts = byId.get("clPager")!.inserted;
    expect(inserts.length).toBe(1);
    expect(inserts[0][0]).toBe("afterend");
    expect(inserts[0][1]).toContain("Could not load calls.");
    expect(inserts[0][1]).toContain(">Retry</button>");
    expect(byId.get("clStatus")!.innerHTML).toContain("Page 1");
    expect(byId.get("clPrev")!.disabled, "page 0: Newer stays boundary-disabled").toBe(true);
    expect(byId.get("clNext")!.disabled).toBe(false);
    expect(byId.get("callsRegion")!.attrs["aria-busy"]).toBe("false");
  });

  it("Retry re-requests the same target once, and success clears the error", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    await http({ error: "boom" }, 0, 500);
    byId.get("clRetry")!.onclick!({ detail: 1 });
    byId.get("clRetry")!.onclick!({ detail: 1 }); // double activation while pending
    expect(requests.length, "a double Retry fires one request").toBe(2);
    expect(requests[1].url).toContain("page=1");
    expect(d.callsPendingPage).toBe(1);
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(d.callsPage).toBe(1);
    expect(d.callsError).toBe("");
    expect(byId.get("clErr")!.removed).toBe(true);
  });

  it("a stale response never commits over a newer needle", async () => {
    const d = fakeDetail();
    const qRows = rowsOf([42]);
    mountLogs(d);
    clickOlder(); // request A: page 1 of the old needle
    // The user types a new needle — docs/31's path: q set, back to page 0, request B issued.
    d.callsQ = "GET";
    d.callsPage = 0;
    d.callsPendingPage = null;
    d.calls = null;
    detail.loadCalls(d.name); // request B parks (the test resolves it below — never await)
    expect(requests.length).toBe(2);
    await ok({ calls: qRows, more: false, stderr: "" }, 1); // the newer request answers first
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);  // the stale page lands last
    expect(d.callsPage).toBe(0);
    expect(d.calls).toEqual(qRows);
  });

  it("a first open with no rows still paints the loading shell — an empty page must not read as success", () => {
    const d = fakeDetail();
    d.calls = null;
    util.state.detail = d;
    byId.set("pane", new FakeNode("main"));
    const tb = new FakeNode("div");
    byId.set("tabbody", tb);
    rh.renderCallsOnly();
    expect(tb.innerHTML).toContain("Loading calls…");
  });

  it("the pager markup carries the docs/32 semantics (nav role, live status)", () => {
    const html = logs.logsBody({ ...fakeDetail(), calls: PAGE0, callsMore: true });
    expect(html).toContain('role="navigation"');
    expect(html).toContain('aria-label="Call log pages"');
    expect(html).toMatch(/id="clStatus"[^>]*aria-live="polite"/);
  });
});

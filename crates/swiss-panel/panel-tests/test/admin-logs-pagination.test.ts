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

/* docs/32 B4: the confirm behind Clear logs — recorded, and its answer the test's to choose. */
let confirmAnswer = false;
const confirmTexts: string[] = [];

let detail: typeof import("../../src/admin_assets/js/detail.js");
let logs: typeof import("../../src/admin_assets/js/logs.js");
let util: typeof import("../../src/admin_assets/js/util.js");
let rh: typeof import("../../src/admin_assets/js/run-history.js");

beforeAll(async () => {
  (globalThis as unknown as Record<string, unknown>).document = doc;
  (globalThis as unknown as Record<string, unknown>).window = { addEventListener() {}, innerWidth: 1280, innerHeight: 800 };
  (globalThis as unknown as Record<string, unknown>).localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  (globalThis as unknown as Record<string, unknown>).location = { origin: "http://127.0.0.1:19998", reload() {} };
  (globalThis as unknown as Record<string, unknown>).confirm = (msg: unknown) => {
    confirmTexts.push(String(msg));
    return confirmAnswer;
  };
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
/** One sidebar row — the real loadList closes the detail of an MCP missing from /api/mcps,
 *  so any list answer that must keep the detail open carries it. */
const ROW = { name: "redis", lifecycle: "started", state: "up", type: "http", source: "managed", group: "default", description: "" };
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
    callsPendingPage: null, callsError: "", callsErrStatus: "", callsRetryTarget: null,
    callsRetryDir: null, callsSwitch: null, callsRequest: 0, callsActive: 0,
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
  doc.body = new FakeNode("body");
  confirmAnswer = false;
  confirmTexts.length = 0;
  requests.length = 0;
  parked.length = 0;
  util.state.detail = null;
  util.state.selected = null;
  util.state.mcps = [];
  util.state.groups = ["default"];
  util.state.menuOpen = false;
});

describe("docs/32 B2: the pager is the scroll anchor, focus follows the action", () => {
  it("pane.scrollTop compensates exactly the pager's viewport drift", async () => {
    const d = fakeDetail();
    mountLogs(d);
    const paneNode = byId.get("pane")!;
    paneNode.scrollTop = 1200;
    // The geometry seam (admin-data-loaders' scroll-regression idiom): the pager's rect is the
    // test's to move between the pre-click read and the post-commit read.
    const pager = new FakeNode("div");
    pager.setRect(500);
    byId.set("clPager", pager);
    clickOlder();
    pager.setRect(380); // the new page's pager sits 120px higher at the same scroll
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(paneNode.scrollTop, "scroll moves by the pager's drift — not to an absolute").toBe(1080);
  });

  it("a keyboard switch returns focus to the same-direction button", async () => {
    const d = fakeDetail();
    mountLogs(d);
    byId.get("clNext")!.onclick!({ detail: 0 }); // Enter/Space on a focused button
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(byId.get("clNext")!.focused).toBe(1);
    expect(byId.get("clPrev")!.focused).toBe(0);
    expect(byId.get("callsQ")!.focused, "the search box is never stolen").toBe(0);
  });

  it("focus falls to the other direction at a boundary page", async () => {
    const d = fakeDetail();
    mountLogs(d);
    byId.get("clNext")!.onclick!({ detail: 0 });
    await ok({ calls: PAGE1, more: false, stderr: "" }, 0); // Older is done on this page
    expect(byId.get("clNext")!.focused, "the done direction yields focus").toBe(0);
    expect(byId.get("clPrev")!.focused).toBe(1);
  });

  it("a mouse switch moves no focus at all", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(byId.get("clNext")!.focused + byId.get("clPrev")!.focused + byId.get("callsQ")!.focused).toBe(0);
  });

  it("the docs/31 input contract survives a switch repaint: a focused search box keeps node, focus and caret", async () => {
    const d = fakeDetail();
    mountLogs(d);
    const q = byId.get("callsQ")!;
    q.focus();
    q.setSelectionRange(2, 2);
    clickOlder(); // the click blurs the input in a real browser; the repaint must still not eat it
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(byId.get("callsQ")!.focused, "focus is restored after the repaint").toBeGreaterThanOrEqual(1);
  });
});

describe("docs/32 B3: history pages hold still, errors are honest", () => {
  it("the MCP poll refreshes calls on page 0 and stays off every older page", async () => {
    const d = fakeDetail();
    mountLogs(d); // page 0, committed
    const mcps = await import("../../src/admin_assets/js/views/mcps.js");
    // The list answer must still contain the row — the real loadList closes the detail of an
    // MCP that vanished from the list, which is not what this test is about.
    const list = { mcps: [{ ...ROW }], groups: ["default"] };
    const p1 = mcps.poll();
    await ok(list, 0); // the list half of the poll parks, then resolves
    expect(requests.some((r) => r.url.includes("/calls?")), "page 0 is polled").toBe(true);
    await ok({ calls: PAGE0, more: true, stderr: "" }, 0); // the calls half — poll can finish
    await p1;
    // Back to a committed older page: reading position, not a live view.
    const d2 = fakeDetail();
    mountLogs(d2);
    d2.callsPage = 2;
    requests.length = 0;
    parked.length = 0;
    const p2 = mcps.poll();
    await ok(list, 0);
    await p2;
    expect(requests.some((r) => r.url.includes("/calls?")), "an offset page is never polled").toBe(false);
  });

  it("a poll request itself refuses an offset page or a pending switch", () => {
    const d = fakeDetail();
    mountLogs(d);
    d.callsPage = 3;
    detail.loadCalls(d.name, true);
    expect(requests.length).toBe(0);
    d.callsPage = 0;
    clickOlder();
    requests.length = 0;
    detail.loadCalls(d.name, true);
    expect(requests.length, "no poll while a switch is mid-transaction").toBe(0);
  });

  it("the poll never races a foreground load in flight — a stolen generation would swallow the failure", async () => {
    const d = fakeDetail();
    mountLogs(d, { rows: PAGE0.slice(0, 3), more: false }); // single page, no pager
    detail.showTab("logs"); // the re-entry foreground load parks, unresolved
    expect(requests.filter((r) => r.url.includes("/calls?")).length).toBe(1);
    const mcps = await import("../../src/admin_assets/js/views/mcps.js");
    const list = { mcps: [{ ...ROW }], groups: ["default"] };
    const p = mcps.poll();
    await ok(list, 1); // the list half (parked behind the re-entry load) resolves; the calls half must not have fired at all
    await p;
    expect(
      requests.filter((r) => r.url.includes("/calls?")).length,
      "no poll load while a foreground load is in flight",
    ).toBe(1);
    await net(0); // the foreground load dies — its failure is still the newest word
    expect(d.callsError).toBe("Could not load calls.");
    expect(d.calls, "the committed rows stay").toEqual(PAGE0.slice(0, 3));
    expect(
      byId.get("callsRegion")!.inserted.filter(([, html]) => html.includes("clErr")).length,
      "the strip lands — the poll never stole the generation",
    ).toBe(1);
  });

  it("a poll failure is silent: rows stay, no error block appears", async () => {
    const d = fakeDetail();
    mountLogs(d);
    byId.set("clPager", new FakeNode("div")); // the painted pager, present and untouched
    detail.loadCalls(d.name, true);
    await http({ error: "down" }, 0, 500);
    expect(d.callsError).toBe("");
    expect(d.calls).toEqual(PAGE0);
    expect(byId.get("clPager")!.inserted.length).toBe(0);
  });

  it("a foreground failure of the very first load replaces the spinner, not with rows", async () => {
    const d = fakeDetail();
    d.calls = null;
    util.state.detail = d;
    byId.set("pane", new FakeNode("main"));
    const tb = new FakeNode("div");
    byId.set("tabbody", tb);
    rh.renderCallsOnly();
    expect(tb.innerHTML).toContain("Loading calls…");
    detail.loadCalls(d.name); // foreground (showTab path)
    await http({ error: "nope" }, 0, 503);
    expect(d.callsError).toBe("Could not load calls.");
    expect(tb.innerHTML, "the error replaces the eternal spinner").toContain("Could not load calls.");
    expect(tb.innerHTML).toContain(">Retry</button>");
    expect(tb.innerHTML).not.toContain("Loading calls…");
  });

  it("a fetch rejection lands in the same visible error", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    await net(0);
    expect(d.callsPage).toBe(0);
    expect(d.calls).toEqual(PAGE0);
    expect(d.callsError).toBe("Could not load calls.");
    expect(byId.get("callsRegion")!.inserted.length).toBe(1);
  });

  it("a new needle supersedes a pending switch: the debounce clears it and returns to page 0", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    expect(d.callsPendingPage).toBe(1);
    const q = byId.get("callsQ")!;
    q.value = "GET";
    q.oninput!();
    await new Promise((r) => setTimeout(r, 340)); // the docs/31 debounce
    expect(d.callsPendingPage).toBeNull();
    expect(d.callsPage).toBe(0);
    const last = requests[requests.length - 1];
    expect(last.url).toContain("page=0");
    expect(last.url).toContain("q=GET");
    await ok({ calls: rowsOf([7]), more: false, stderr: "" }, 0); // drain the parked search
  });

  it("Escape clears the needle at once, through the same reset", async () => {
    const d = fakeDetail();
    mountLogs(d, { q: "GET" });
    clickOlder();
    const q = byId.get("callsQ")!;
    q.value = "GET";
    q.onkeydown!({ key: "Escape", preventDefault() {} });
    expect(d.callsQ).toBe("");
    expect(d.callsPendingPage).toBeNull();
    expect(d.callsPage).toBe(0);
    expect(requests[requests.length - 1].url).not.toContain("q=");
    await ok({ calls: PAGE0, more: true, stderr: "" }, 0); // drain
  });

  it("re-entering the Logs tab while a switch is pending fires no duplicate request", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    detail.showTab("logs");
    expect(requests.length).toBe(1);
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0); // drain
  });
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
    const inserts = byId.get("callsRegion")!.inserted;
    expect(inserts.length).toBe(1);
    expect(inserts[0][0]).toBe("beforeend");
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

describe("docs/32 B4: the toolbar ellipsis, Clear logs behind a confirm", () => {
  /** The popup menu popupMenu appends to <body> — its last child. */
  function lastMenu() {
    const body = (globalThis as unknown as { document: typeof doc }).document.body;
    return body.children[body.children.length - 1];
  }

  it("the toolbar carries the ellipsis menu button, not a standing Clear", () => {
    const d = fakeDetail();
    const tb = mountLogs(d);
    expect(tb.innerHTML).toContain('id="clMenu"');
    expect(tb.innerHTML).toContain('aria-label="More log actions"');
    expect(tb.innerHTML).not.toContain("callsClear");
    expect(tb.innerHTML).not.toContain("Clear logs", "the destructive verb is not painted inline");
  });

  it("the menu opens with Clear logs… as its danger item", () => {
    const d = fakeDetail();
    mountLogs(d);
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    const menu = lastMenu();
    const items = menu.children.filter((c) => c.tag === "BUTTON");
    expect(items.map((c) => c.textContent)).toEqual(["Clear logs…"]);
    expect(items[0].className).toContain("danger");
  });

  it("a cancelled confirm fires no request at all", async () => {
    const d = fakeDetail();
    mountLogs(d);
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    lastMenu().children.find((c) => c.textContent === "Clear logs…")!.onclick!({ stopPropagation() {} });
    await new Promise((r) => setTimeout(r, 0));
    expect(requests.length).toBe(0);
  });

  it("confirm names both costs, and only then the DELETE runs and the log resets to a clean page 0", async () => {
    const d = fakeDetail();
    const tb = mountLogs(d);
    d.callsOpen = { 100: true };
    d.callsFull = { 100: "full" };
    clickOlder(); // a switch in flight — Clear must still win and reset it
    await http({ error: "boom" }, 0, 500); // failed switch leaves an error + Retry target
    confirmAnswer = true;
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    lastMenu().children.find((c) => c.textContent === "Clear logs…")!.onclick!({ stopPropagation() {} });
    expect(confirmTexts).toEqual([
      "Clear all recorded tool calls for \u201Credis\u201D? This removes the call history and stored full replies. The MCP configuration is not changed.",
    ]);
    await new Promise((r) => setTimeout(r, 0));
    const del = requests.find((r) => r.method === "DELETE");
    expect(del && del.url).toContain("/api/mcps/redis/calls");
    await ok({}, 0);
    expect(d.calls).toEqual([]);
    expect(d.callsPage).toBe(0);
    expect(d.callsMore).toBe(false);
    expect(d.callsOpen).toEqual({});
    expect(d.callsFull).toEqual({});
    expect(d.callsError).toBe("");
    expect(d.callsPendingPage).toBeNull();
    expect(byId.get("toast")!.textContent).toBe("Call log cleared");
    expect(tb.innerHTML).toContain("No calls yet");
  });
});

describe("docs/32 review fixes: the clear transaction and the error strip's edges", () => {
  /** The popup menu popupMenu appends to <body> — its last child. */
  function lastMenu() {
    const body = (globalThis as unknown as { document: typeof doc }).document.body;
    return body.children[body.children.length - 1];
  }

  it("a response parked before Clear lands after the DELETE and must not repaint the cleared log", async () => {
    const d = fakeDetail();
    const tb = mountLogs(d);
    clickOlder(); // page-1 switch in flight — its GET parks first
    confirmAnswer = true;
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    lastMenu().children.find((c) => c.textContent === "Clear logs…")!.onclick!({ stopPropagation() {} });
    await new Promise((r) => setTimeout(r, 0));
    // parked order: the switch's GET first, the DELETE behind it — the DELETE answers now.
    await ok({}, 1);
    expect(d.calls).toEqual([]);
    expect(tb.innerHTML).toContain("No calls yet");
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0); // the pre-clear switch lands LAST
    expect(d.calls, "a stale answer never repaints a cleared log").toEqual([]);
    expect(d.callsPage).toBe(0);
    expect(d.callsPendingPage).toBeNull();
    expect(tb.innerHTML).toContain("No calls yet");
  });

  it("a foreground failure with committed rows and no pager (single page) still shows the strip in the region", async () => {
    const d = fakeDetail();
    mountLogs(d, { rows: PAGE0.slice(0, 3), more: false });
    expect(byId.get("clPager")).toBeUndefined(); // a single-page log paints no pager
    detail.showTab("logs"); // re-entry reload — a foreground load over committed rows
    expect(requests.length).toBe(1);
    await http({ error: "boom" }, 0, 500);
    expect(d.callsError).toBe("Could not load calls.");
    expect(d.calls, "the committed rows stay").toEqual(PAGE0.slice(0, 3));
    const inserts = byId.get("callsRegion")!.inserted;
    expect(inserts.filter(([, html]) => html.includes("clErr")).length, "the strip lands inside the region").toBe(1);
    expect(inserts[inserts.length - 1][1]).toContain(">Retry</button>");
  });

  it("the patch-wired Retry honors the keyboard contract too: focus returns on commit", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    await http({ error: "boom" }, 0, 500);
    byId.get("clRetry")!.onclick!({ detail: 0 }); // Enter on the focused Retry
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(byId.get("clNext")!.focused, "a keyboard Retry gets its focus back").toBe(1);
    expect(byId.get("clPrev")!.focused).toBe(0);
  });

  it("two consecutive failures never stack a second error strip", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    await http({ error: "boom" }, 0, 500);
    byId.get("clRetry")!.onclick!({ detail: 1 });
    await http({ error: "boom" }, 0, 500);
    const strips = byId.get("callsRegion")!.inserted.filter(([, html]) => html.includes('id="clErr"'));
    expect(strips.length, "one strip per failure, never stacked").toBe(2);
    expect(byId.get("callsRegion")!.inserted.length).toBe(2);
  });

  it("the ellipsis toggles its menu closed like the house overflow button", () => {
    const d = fakeDetail();
    mountLogs(d);
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    expect(util.state.menuOpen).toBe(true);
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    expect(util.state.menuOpen, "a second click dismisses, not reopens").toBe(false);
    byId.get("clMenu")!.onclick!({ detail: 1, stopPropagation() {} });
    expect(util.state.menuOpen, "a third click opens again").toBe(true);
  });
});

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

import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { setMcpDetail, setMcpGroups, setMcpRows, setSelectedMcp } from "../src/mcp-state.js";
import { menuOpen, setMenuOpen } from "../src/ui/menu.js";

/* docs/32 B1 — the Logs pager becomes a transaction. The suite drives the REAL module graph
   (wireTabBody -> callsPageStep -> loadCalls -> renderCallsOnly) under the FakeNode micro-DOM,
   with a fetch queue the test resolves by hand — the only way to make a stale response land
   after a newer one under Node. Assertions never trust a bare logsBody string: they check
   state, issued requests, painted tabbody HTML and the patched pager chrome.
   It stays on the micro-DOM on purpose (docs/46 P7-3, where the other three menu suites moved
   to happy-dom): its assertions count paints, error strips and focus calls on FakeNode, which a
   real DOM does not record. */

/* h()/frag() branch on the global Node class (docs/37 R5); under this micro-DOM it is this
   stub, so a FakeNode IS a Node to the builder. */
const NodeStub = class {};
class FakeNode extends NodeStub {
  tag: string;
  attrs: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  _text = "";
  // Set directly, or read back from the children the way a real DOM does: a text child is
  // { text } (createTextNode below) and an element child answers for itself. h() builds a menu
  // row's word as a text child (docs/46 P7-3), so the menu assertions read it through here.
  get textContent(): string {
    return this._text || this.children.map((c) => {
      const t = (c as unknown as { text?: string }).text;
      return t != null ? t : c.textContent;
    }).join("");
  }
  set textContent(v: string) {
    if (v === "") { this.children = []; this._html = ""; this._text = ""; this.paints.push(""); return; }
    this._text = v; this.children = [{ text: v } as unknown as FakeNode];
  }
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
  constructor(tag: string) { super(); this.tag = tag.toUpperCase(); }
  static fragment(): FakeNode { const f = new FakeNode("#document-fragment"); f.tag = "#DOCUMENT-FRAGMENT"; return f; }
  get innerHTML(): string { return this._html || serialize(this); }
  set innerHTML(v: string) { this._html = v; this.children = []; this.paints.push(v); }
  /* docs/37 R5: the real renderers now paint with fill() — textContent wipe + appendChild —
   *  instead of assigning innerHTML. The wipe marks a paint (same count semantics: one paint
   *  per repaint, none for an in-place chrome patch), and innerHTML READS serialise the live
   *  children so the substring assertions keep their meaning on the built tree. */
  get liveText(): string { return serialize(this); }
  getBoundingClientRect() { return this._rect; }
  setRect(top: number) { this._rect = { top, left: 0, bottom: top + 22, right: 40, width: 40, height: 22 }; }
  setAttribute(k: string, v: string) { this.attrs[k] = v; if (k === "id") this.id = v; }
  getAttribute(k: string) { return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null; }
  removeAttribute(k: string) { delete this.attrs[k]; }
  querySelector(): FakeNode | null { return null; }
  querySelectorAll(): FakeNode[] { return []; }
  contains(): boolean { return false; }
  // The dispatchers climb to the button (t.closest("#clMenu")) because a real pointer click
  // lands on the glyph, not the button. The suite's tree carries no parent links, so closest
  // self-matches by id - the part this suite's clicks exercise.
  closest(sel: string): FakeNode | null {
    return sel.charAt(0) === "#" && this.id === sel.slice(1) ? this : null;
  }
  appendChild(n: FakeNode) {
    if (n.tag === "#DOCUMENT-FRAGMENT") { n.children.forEach((c) => { this.children.push(c); }); return n; }
    this.children.push(n);
    this._html = ""; // built nodes, not a parsed string — reads serialise instead
    return n;
  }
  append(...nodes: FakeNode[]) {
    nodes.forEach((n) => {
      this.appendChild(n);
      // The error strip moved from insertAdjacentHTML to append (docs/37 R5); record it the
      // same way so the transaction assertions keep counting strips.
      this.inserted.push(["beforeend", serialize(n)]);
    });
  }
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
function serialize(node: FakeNode): string {
  const raw = node as unknown as { text?: string };
  if (!node.tag || raw.text != null) return String(raw.text ?? "");
  const attrs = Object.keys(node.attrs).map((k) => { return " " + k + "=\"" + node.attrs[k] + "\""; }).join("");
  const id = node.id ? " id=\"" + node.id + "\"" : "";
  const role = (node as unknown as { role?: string }).role ? " role=\"" + (node as unknown as { role?: string }).role + "\"" : "";
  const cls = node.className ? " class=\"" + node.className + "\"" : "";
  const dis = (node as unknown as { disabled?: boolean }).disabled ? " disabled" : "";
  const val = (node as unknown as { value?: string }).value ? " value=\"" + (node as unknown as { value?: string }).value + "\"" : "";
  const inner = node.children.map((c) => { return serialize(c); }).join("");
  return "<" + node.tag.toLowerCase() + id + attrs + role + cls + dis + val + ">" + inner + "</" + node.tag.toLowerCase() + ">";
}
/* docs/37 R5: nothing walks the fresh tab body assigning handlers any more, so controls are
   * no longer registered by id as a side effect of wiring. Painted nodes resolve from the tree
   * the paint built (the real DOM's own answer), the byId cache first like before. */
let paintedTb: FakeNode | null = null;
function findInTree(node: FakeNode, id: string): FakeNode | null {
  const kids = (node as unknown as { children?: FakeNode[] }).children || [];
  if (node.id === id) return node;
  for (const c of kids) {
    const hit = findInTree(c, id);
    if (hit) return hit;
  }
  return null;
}
const doc = {
  getElementById(id: string) {
    if (!byId.has(id)) {
      const painted = paintedTb ? findInTree(paintedTb, id) : null;
      if (painted) byId.set(id, painted);
      else { byId.set(id, new FakeNode("div")); byId.get(id)!.id = id; }
    }
    return byId.get(id)!;
  },
  createDocumentFragment: () => FakeNode.fragment(),
  createElementNS: () => new FakeNode("svg"),
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

let detail: typeof import("../src/detail.js");
let logs: typeof import("../src/logs.js");
let util: typeof import("../src/util.js");
let rh: typeof import("../src/run-history.js");

beforeAll(async () => {
  (globalThis as unknown as Record<string, unknown>).Node = NodeStub;
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
  detail = await import("../src/detail.js");
  logs = await import("../src/logs.js");
  util = await import("../src/util.js");
  rh = await import("../src/run-history.js");
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
  setMcpDetail(d);
  setSelectedMcp(d.name);
  setMcpRows([]);
  byId.set("pane", new FakeNode("main"));
  const tb = new FakeNode("div");
  byId.set("tabbody", tb);
  rh.renderCallsOnly();
  paintedTb = tb;
  return tb;
}

/** Route a click on a tab-body control through the REAL delegated dispatcher (docs/37 R5):
 *  #pane's listener answers it - the per-node onclick wiring retired with wireTabBody. */
function fireTab(idOrNode: string | FakeNode, ev: Record<string, unknown> = { detail: 1 }): void {
  const what = typeof idOrNode === "string" ? doc.getElementById(idOrNode) : idOrNode;
  rh.paneTabClick({ target: what, stopPropagation() {}, ...ev } as unknown as MouseEvent);
}
function fireInput(what: FakeNode): void {
  rh.paneTabInput({ target: what } as unknown as Event);
}
function fireKey(what: FakeNode, key: string): void {
  rh.paneTabKeydown({ target: what, key, preventDefault() {} } as unknown as KeyboardEvent);
}

/** Click Older through the REAL wiring (mouse: detail > 0). */
function clickOlder() { fireTab("clNext"); }
function clickNewer() { fireTab("clPrev"); }

beforeEach(() => {
  byId.clear();
  doc.activeElement = null;
  doc.body = new FakeNode("body");
  confirmAnswer = false;
  confirmTexts.length = 0;
  requests.length = 0;
  parked.length = 0;
  setMcpDetail(null);
  setSelectedMcp(null);
  setMcpRows([]);
  setMcpGroups(["default"]);
  setMenuOpen(false);
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
    fireTab("clNext", { detail: 0 }); // Enter/Space on a focused button
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(byId.get("clNext")!.focused).toBe(1);
    expect(byId.get("clPrev")!.focused).toBe(0);
    expect(byId.get("callsQ")!.focused, "the search box is never stolen").toBe(0);
  });

  it("focus falls to the other direction at a boundary page", async () => {
    const d = fakeDetail();
    mountLogs(d);
    fireTab("clNext", { detail: 0 });
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
    const mcps = await import("../src/views/mcps.js");
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
    const mcps = await import("../src/views/mcps.js");
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
    setMcpDetail(d);
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
    fireInput(q);
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
    fireKey(q, "Escape");
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
    expect(painted).toContain('data-callseq="100"'); // the committed rows are on screen
    clickOlder();
    expect(requests.length).toBe(1);
    expect(requests[0].url).toContain("/api/mcps/redis/calls?page=1&q=GET");
    expect(d.callsPage, "the committed page is untouched while pending").toBe(0);
    expect(d.callsPendingPage).toBe(1);
    expect(d.calls, "the rows are not cleared to a loading shell").toEqual(PAGE0);
    expect(tb.paints.length, "pending patches in place — the tabbody is not repainted").toBe(1);
    expect(tb.innerHTML).toContain('data-callseq="100"');
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
    expect(tb.innerHTML).toContain('data-callseq="80"');
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
    fireTab("clRetry");
    fireTab("clRetry"); // double activation while pending
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
    setMcpDetail(d);
    byId.set("pane", new FakeNode("main"));
    const tb = new FakeNode("div");
    byId.set("tabbody", tb);
    rh.renderCallsOnly();
    expect(tb.innerHTML).toContain("Loading calls…");
  });

  it("the pager markup carries the docs/32 semantics (nav role, live status)", () => {
    const host = new FakeNode("div");
    host.appendChild(logs.logsBodyNode({ ...fakeDetail(), calls: PAGE0, callsMore: true }) as unknown as FakeNode);
    const html = host.innerHTML;
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
    (expect(tb.innerHTML).not.toContain as (expected: unknown, message?: string) => void)("Clear logs", "the destructive verb is not painted inline");
  });

  it("the menu opens with Clear logs… as its danger item", () => {
    const d = fakeDetail();
    mountLogs(d);
    fireTab("clMenu");
    const menu = lastMenu();
    const items = menu.children.filter((c) => c.tag === "BUTTON");
    expect(items.map((c) => c.textContent)).toEqual(["Clear logs…"]);
    expect(items[0].className).toContain("danger");
  });

  it("a cancelled confirm fires no request at all", async () => {
    const d = fakeDetail();
    mountLogs(d);
    fireTab("clMenu");
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
    fireTab("clMenu");
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
    fireTab("clMenu");
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
    fireTab("clRetry", { detail: 0 }); // Enter on the focused Retry
    await ok({ calls: PAGE1, more: true, stderr: "" }, 0);
    expect(byId.get("clNext")!.focused, "a keyboard Retry gets its focus back").toBe(1);
    expect(byId.get("clPrev")!.focused).toBe(0);
  });

  it("two consecutive failures never stack a second error strip", async () => {
    const d = fakeDetail();
    mountLogs(d);
    clickOlder();
    await http({ error: "boom" }, 0, 500);
    fireTab("clRetry");
    await http({ error: "boom" }, 0, 500);
    const strips = byId.get("callsRegion")!.inserted.filter(([, html]) => html.includes('id="clErr"'));
    expect(strips.length, "one strip per failure, never stacked").toBe(2);
    expect(byId.get("callsRegion")!.inserted.length).toBe(2);
  });

  it("the ellipsis toggles its menu closed like the house overflow button", () => {
    const d = fakeDetail();
    mountLogs(d);
    fireTab("clMenu");
    expect(menuOpen()).toBe(true);
    fireTab("clMenu");
    expect(menuOpen(), "a second click dismisses, not reopens").toBe(false);
    fireTab("clMenu");
    expect(menuOpen(), "a third click opens again").toBe(true);
  });
});

/* docs/33 C3 — the operator opens a call to read its result. A reply the page clipped to its
   2 KB preview is fetched whole on the open itself, through the same REAL dispatcher a pointer
   click and Enter/Space reach; the Show full result button is only the retry. */
describe("docs/33 C3: opening a clipped call fetches its full reply", () => {
  function clipped(seq: number) {
    return { ...callRow(seq), chars: 5000, output: '{"rows":[', preview: true };
  }
  /** A target inside one call's summary button — the dispatcher climbs to .tl-sum, then to
   *  the row's [data-callseq] (docs/46 P2-2: only the summary toggles, never the open body). */
  function summaryOf(seq: number) {
    const item = { dataset: { callseq: String(seq) } };
    const sum = { closest: (sel: string) => (sel === "[data-callseq]" ? item : null) };
    return { id: "", closest: (sel: string) => (sel === ".tl-sum" ? sum : null) };
  }
  /** A target inside the open body of the same row: it is under [data-callseq] but not .tl-sum. */
  function bodyOf(seq: number) {
    return { id: "", closest: (sel: string) => (sel === "[data-callseq]" ? { dataset: { callseq: String(seq) } } : null) };
  }
  function openRow(seq: number): void {
    rh.paneTabClick({ target: summaryOf(seq), stopPropagation() {}, detail: 1 } as unknown as MouseEvent);
  }
  const fetchesOf = (seq: number) => requests.filter((r) => r.url.endsWith("/api/mcps/redis/calls/" + seq)).length;

  it("a click that opens a clipped row asks for the whole reply, once", async () => {
    const d = fakeDetail();
    mountLogs(d, { rows: [clipped(300), callRow(299)], more: false });
    openRow(300);
    expect(d.callsOpen[300]).toBe(true);
    expect(fetchesOf(300), "the open itself fetches — no Show full result click").toBe(1);
    openRow(300); // close
    openRow(300); // reopen while the first fetch is still out
    expect(fetchesOf(300), "an open/close/open does not stack a second request").toBe(1);
    await ok({ call: { seq: 300, output: '{"rows":[1,2,3]}' } }, 0);
    expect(d.callsFull[300]).toBe('{"rows":[1,2,3]}');
    openRow(300);
    openRow(300);
    expect(fetchesOf(300), "a reply already here is never fetched again").toBe(1);
  });

  it("a reply the page carried whole is never fetched", () => {
    const d = fakeDetail();
    mountLogs(d, { rows: [clipped(310), callRow(309)], more: false });
    openRow(309);
    expect(d.callsOpen[309]).toBe(true);
    expect(requests.some((r) => r.url.includes("/calls/")), "no per-call request for an unclipped row").toBe(false);
  });

  it("the summary is a real button, so Enter and Space are its click; the keydown path stays out", async () => {
    const d = fakeDetail();
    const tb = mountLogs(d, { rows: [clipped(320)], more: false });
    expect(tb.innerHTML, "the row's summary is a <button>").toMatch(/<button[^>]*class="tl-sum"/);
    rh.paneTabKeydown({ target: summaryOf(320), key: "Enter", preventDefault() {} } as unknown as KeyboardEvent);
    expect(d.callsOpen[320], "a second, keydown toggle would undo the button's own click").toBeUndefined();
    rh.paneTabClick({ target: summaryOf(320), stopPropagation() {}, detail: 0 } as unknown as MouseEvent);
    expect(d.callsOpen[320]).toBe(true);
    expect(fetchesOf(320)).toBe(1);
    await ok({ call: { seq: 320, output: "{}" } }, 0);
  });

  it("a click inside an open body does not close the row", () => {
    const d = fakeDetail();
    mountLogs(d, { rows: [callRow(325)], more: false });
    d.callsOpen[325] = true;
    rh.paneTabClick({ target: bodyOf(325), stopPropagation() {}, detail: 1 } as unknown as MouseEvent);
    expect(d.callsOpen[325]).toBe(true);
  });

  it("identical consecutive calls are one row; opening it and a newer twin keeps it open", () => {
    const d = fakeDetail();
    const twin = (seq: number) => ({ ...callRow(seq), tool: "ping", args: "{}", output: "pong" });
    const tb = mountLogs(d, { rows: [twin(352), twin(351), callRow(350)], more: false });
    const html = tb.innerHTML;
    expect(html).toContain('data-callseq="352"');
    expect(html, "the older twin folds under the newer one").not.toContain('data-callseq="351"');
    expect(html).toContain("×2");
    openRow(352);
    expect(d.callsOpen[352]).toBe(true);
    const runs = logs.callRunOf(d, 352)!.map((c: { seq: number }) => c.seq);
    expect(runs).toEqual([352, 351]);
    // A poll brings a third twin to the top: the run's first seq changes, the row stays open.
    d.calls = [twin(353), twin(352), twin(351), callRow(350)];
    const again = new FakeNode("div");
    again.appendChild(logs.logsBodyNode(d) as unknown as FakeNode);
    expect(again.innerHTML).toMatch(/class="tl-item open"[^>]*data-tl-id="353"|data-tl-id="353"[^>]*class="tl-item open"/);
    openRow(353); // closing forgets every call in the run
    expect([d.callsOpen[353], d.callsOpen[352], d.callsOpen[351]]).toEqual([false, false, false]);
  });

  it("closing a row fetches nothing", () => {
    const d = fakeDetail();
    mountLogs(d, { rows: [clipped(330)], more: false });
    d.callsOpen[330] = true;
    openRow(330); // this click closes it
    expect(d.callsOpen[330]).toBe(false);
    expect(fetchesOf(330)).toBe(0);
  });

  it("a pruned body is recorded, so the row stops offering a fetch it cannot serve", async () => {
    const d = fakeDetail();
    d.callsGone = {};
    mountLogs(d, { rows: [clipped(340)], more: false });
    openRow(340);
    await ok({ call: { seq: 340, bodyGone: true, output: '{"rows":[' } }, 0);
    expect(d.callsGone[340]).toBe(true);
    expect(d.callsFull[340], "the preview is not passed off as the whole reply").toBeUndefined();
    openRow(340);
    openRow(340);
    expect(fetchesOf(340), "a known-pruned body is not asked for again").toBe(1);
  });
});

/* docs/33 C3 — a block's Copy, Copy raw and Show all, through the REAL delegated dispatcher.
   Both copies read the call row, not the painted DOM, so a block cut at its line cap still
   copies everything; Show all is state, so a poll repaint keeps the block whole. */
describe("docs/33 C3: the block buttons dispatch", () => {
  /** A target inside one button — the dispatcher climbs to its data attribute. */
  function btn(attr: string, key: string) {
    const dataKey = attr.replace(/^data-/, "");
    return { id: "", closest: (sel: string) => (sel === "[" + attr + "]" ? { dataset: { [dataKey]: key } } : null) };
  }
  const click = (target: unknown) => { rh.paneTabClick({ target, stopPropagation() {}, detail: 1 } as unknown as MouseEvent); };
  const WIRE = JSON.stringify(JSON.stringify({ hits: [{ title: "a" }] }));

  it("Copy writes the decoded reply as formatted JSON; Copy raw, behind the block's ⋯, the stored text", async () => {
    const wrote: string[] = [];
    vi.stubGlobal("navigator", { clipboard: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } } });
    const d = fakeDetail();
    mountLogs(d, { rows: [{ ...callRow(400), output: WIRE }], more: false });
    click(btn("data-copy", "out:400"));
    await tick();
    const more = { ...btn("data-blkmore", "out:400"), getBoundingClientRect: () => ({ top: 0, left: 0, bottom: 22, right: 24, width: 24, height: 22 }) };
    const blkMore = more.closest("[data-blkmore]") as unknown as Record<string, unknown>;
    blkMore.getBoundingClientRect = more.getBoundingClientRect;
    click({ id: "", closest: (sel: string) => (sel === "[data-blkmore]" ? blkMore : null) });
    expect(menuOpen(), "the block's ⋯ opens a menu").toBe(true);
    const body = (globalThis as unknown as { document: { body: FakeNode } }).document.body;
    const menu = body.children[body.children.length - 1];
    const item = menu.children[0];
    expect(item.textContent).toBe("Copy raw");
    item.onclick!({ stopPropagation() {} });
    await tick();
    expect(wrote[0]).toBe('{\n  "hits": [\n    {\n      "title": "a"\n    }\n  ]\n}');
    expect(wrote[1]).toBe(WIRE);
    expect(d.callsOpen[400], "a copy never toggles the row").toBeUndefined();
    vi.unstubAllGlobals();
  });

  it("Show all records the block, so a repaint keeps it whole", () => {
    const d = fakeDetail();
    mountLogs(d, { rows: [callRow(410)], more: false });
    click(btn("data-showall", "out:410"));
    expect(d.callsAll["out:410"]).toBe(true);
    expect(d.callsOpen[410], "Show all never toggles the row").toBeUndefined();
  });
});

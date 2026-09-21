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
import { menuIsOpen, setMenuOpen } from "../src/ui-state.js";
import { mcpDetail, mcpRows, resetMcpState, setMcpDetail, setMcpGroups, setMcpRows, setSelectedMcp } from "../src/mcp-state.js";

/* h()/frag() route children through `instanceof Node`, so the stub must exist BEFORE the
   FakeNode class below evaluates, and FakeNode must be one of its instances. */
const NodeStub = class {};
(globalThis as unknown as Record<string, unknown>).Node = NodeStub;

/* def revisions (docs/28 D1): the config tab's Replace flow and the rollback shelf. The
   rendering assertions go through the real renderPane markup; the confirm-gated restore is
   driven through a fetch queue — cancelled confirm must not fire a request at all. */

/* docs/37 R5: the tab bodies are built as nodes and ride a serialization bridge into the
   pane's string paint; the FakeNode's innerHTML READ serialises its children so the config-
   tab assertions keep seeing the built tree. */
function serialize(node: FakeNode): string {
  const raw = node as unknown as { text?: string };
  if (!node.tag || raw.text != null) return String(raw.text ?? "");
  const attrs = Object.keys(node.attrs).map((k) => { return " " + k + "=\"" + node.attrs[k] + "\""; }).join("");
  const id = node.id ? " id=\"" + node.id + "\"" : "";
  const cls = node.className ? " class=\"" + node.className + "\"" : "";
  const dis = node.disabled ? " disabled" : "";
  const val = node.value ? " value=\"" + node.value + "\"" : "";
  const inner = node.children.map((c) => { return serialize(c); }).join("");
  return "<" + node.tag.toLowerCase() + id + attrs + cls + dis + val + ">" + inner + "</" + node.tag.toLowerCase() + ">";
}
class FakeNode extends NodeStub {
  tag: string;
  attrs: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  textContent = "";
  _html = "";
  get innerHTML(): string { return this._html || serialize(this); }
  set innerHTML(v: string) { this._html = v; this.children = []; }
  id = "";
  type = "button";
  checked = false;
  value = "";
  hidden = false;
  disabled = false;
  style: Record<string, string> = {};
  children: FakeNode[] = [];
  onclick: ((ev?: unknown) => void) | null = null;
  constructor(tag: string) {
    super();
    this.tag = tag.toUpperCase();
  }
  getBoundingClientRect() { return { left: 0, top: 0, bottom: 0, width: 10, height: 10 }; }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
  }
  getAttribute(k: string) {
    return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null;
  }
  querySelector() { return null; }
  querySelectorAll() { return [] as FakeNode[]; }
  contains() { return false; }
  appendChild(n: FakeNode) { this.children.push(n); return n; }
  append(...nodes: FakeNode[]) { nodes.forEach((n) => { this.appendChild(n); }); }
  removeChild(n: FakeNode) { return n; }
  remove() {}
  addEventListener() {}
  select() {}
}

const byId = new Map<string, FakeNode>();
const doc = {
  getElementById(id: string) {
    if (!byId.has(id)) byId.set(id, new FakeNode("div"));
    return byId.get(id)!;
  },
  createElement: (t: string) => new FakeNode(t),
  createElementNS: () => new FakeNode("svg"),
  createTextNode: (s: string) => ({ text: s }) as unknown as FakeNode,
  createDocumentFragment: () => new FakeNode("#document-fragment"),
  // patchDetailHead probes the pane's dot/sub-text through the document; everything else in
  // these modules expects null here. Answer only that probe.
  querySelector: (sel: string) => (String(sel).indexOf("pane-sub") >= 0 ? new FakeNode("span") : null),
  querySelectorAll: () => [] as FakeNode[],
  body: new FakeNode("body"),
  addEventListener() {},
  activeElement: null as FakeNode | null,
};

const confirmMock = vi.fn();
const calls: Array<{ url: string; init?: RequestInit }> = [];

let pane: typeof import("../src/pane.js");
let detail: typeof import("../src/detail.js");
let sidebar: typeof import("../src/sidebar.js");
let util: typeof import("../src/util.js");

beforeAll(async () => {
  (globalThis as unknown as Record<string, unknown>).document = doc;
  (globalThis as unknown as Record<string, unknown>).window = { addEventListener() {}, innerWidth: 1280, innerHeight: 800 };
  (globalThis as unknown as Record<string, unknown>).confirm = confirmMock;
  (globalThis as unknown as Record<string, unknown>).fetch = vi.fn(async (url: string, init?: RequestInit) => {
    calls.push({ url: String(url), init });
    return { ok: true, status: 200, json: async () => ({ revisions: [], parked: true }) } as never;
  });
  pane = await import("../src/pane.js");
  detail = await import("../src/detail.js");
  sidebar = await import("../src/sidebar.js");
  util = await import("../src/util.js");
});

function freshState(overrides: { mcps?: unknown[]; groups?: string[]; selected?: string | null; detail?: unknown } = {}) {
  resetMcpState();
  setMcpGroups(overrides.groups ?? ["default"]);
  if (overrides.mcps) setMcpRows(overrides.mcps as never);
  if (overrides.selected !== undefined) setSelectedMcp(overrides.selected);
  if (overrides.detail !== undefined) setMcpDetail(overrides.detail as never);
  setMenuOpen(false);
}

function fakeDetail(name: string, config: Record<string, unknown> | null) {
  const kd = { items: [], nextCursor: undefined, total: undefined, loaded: false, loading: false, error: null, cursors: [""], pageSize: 50 };
  return {
    name, tab: "config", config, source: "managed", editing: false, editMode: null, editType: null, editVals: null,
    revisions: null,
    tools: { ...kd }, resources: { ...kd }, prompts: { ...kd },
    run: { tool: null, result: null, running: false, hist: null, histTool: null, histLoading: false, histOpen: false, histSelSeq: null, histFull: {}, histQ: "" },
    calls: null, stderr: "", callsOpen: {}, callsLoading: false, callsPage: 0, callsMore: false, callsFull: {},
  } as never;
}

const ROW = { name: "m", lifecycle: "started", state: "up", type: "echo", tag: "echo", source: "managed", description: "", group: "default" };

describe("config tab: the replace flow and the rollback shelf (docs/28 D1)", () => {
  beforeEach(() => { byId.clear(); freshState(); calls.length = 0; confirmMock.mockReset(); });

  it("renders Replace definition… beside Edit, and the parked revisions under the config rows", () => {
    const d = fakeDetail("m", { type: "echo" });
    (d as unknown as Record<string, unknown>).revisions = [
      { index: 0, at: 1789467260822, note: "proc version", type: "proc" },
    ];
    freshState({ mcps: [ROW] as never, detail: d });
    pane.renderPane();
    const html = byId.get("pane")!.innerHTML;
    expect(html).toContain('id="c-replace"');
    expect(html).toContain("Replace definition");
    expect(html).toContain("Saved revisions (1)");
    expect(html).toContain("proc version");
    expect(html).toContain('data-restore="0"');
    expect(html).toContain('data-revdel="0"');
  });

  it("an empty shelf says so instead of nothing", () => {
    const d = fakeDetail("m", { type: "echo" });
    (d as unknown as Record<string, unknown>).revisions = [];
    freshState({ mcps: [ROW] as never, detail: d });
    pane.renderPane();
    expect(byId.get("pane")!.innerHTML).toContain("None yet");
  });

  it("startReplace opens the same form with a note field and the Replace verb", () => {
    const d = fakeDetail("m", { type: "echo" });
    freshState({ mcps: [ROW] as never, detail: d });
    detail.startReplace();
    expect((mcpDetail() as unknown as Record<string, unknown>).editMode).toBe("replace");
    const html = byId.get("pane")!.innerHTML;
    expect(html).toContain('id="e-note"');
    expect(html).toContain("Replace definition</button>");
  });

  it("restore asks first, and a cancelled confirm fires no request", async () => {
    const d = fakeDetail("m", { type: "echo" });
    freshState({ mcps: [ROW] as never, detail: d });
    confirmMock.mockReturnValue(false);
    await detail.restoreRevision(0);
    expect(calls.length).toBe(0);
    confirmMock.mockReturnValue(true);
    await detail.restoreRevision(0);
    // The restore POST leads; the meta/revisions/list refreshes follow it.
    expect(calls[0].url).toContain("/api/mcps/m/revisions/0/restore");
    expect((calls[0].init as { method?: string }).method).toBe("POST");
    expect(calls.some((c) => c.url.includes("/api/mcps/m/revisions"))).toBe(true);
  });
});

describe("docs/28 D2: the verb is disable, the word is disabled", () => {
  beforeEach(() => { byId.clear(); freshState(); calls.length = 0; });

  it("the detail header's primary button says Disable on a started MCP and Enable on a stopped one", () => {
    freshState({ mcps: [{ ...ROW }] as never, detail: fakeDetail("m", { type: "echo" }) });
    const primary = new FakeNode("button");
    byId.set("primaryBtn", primary);
    // patchDetailHead reads the dot and the sub-text through querySelector before it touches
    // the button — the micro-DOM returns null, so hand it a pane node that answers.
    const paneNode = new FakeNode("div");
    (paneNode as unknown as { querySelector: () => FakeNode }).querySelector = () => new FakeNode("span");
    byId.set("pane", paneNode);
    pane.patchDetailHead();
    expect(primary.textContent).toBe("Disable");
    (mcpRows()[0] as unknown as Record<string, unknown>).lifecycle = "stopped";
    pane.patchDetailHead();
    expect(primary.textContent).toBe("Enable");
  });

  it("the header subtitle and the overflow menu use the operator's word", () => {
    const stopped = { ...ROW, lifecycle: "stopped", state: "stopped" };
    expect(pane.headSubtitle(stopped as never)).toContain("disabled");
    const menu = pane.menuNode(stopped as never);
    expect(menu.innerHTML).toContain(">Enable</button>");
    expect(menu.innerHTML).toContain('data-act="start"');
    const startedMenu = pane.menuNode({ ...ROW } as never);
    expect(startedMenu.innerHTML).toContain(">Disable</button>");
    expect(startedMenu.innerHTML).toContain('data-act="stop"');
  });
});

describe("docs/28 D3: the row's right-click menu", () => {
  beforeEach(() => { byId.clear(); freshState(); calls.length = 0; confirmMock.mockReset(); });

  /** The menu popup as the last child of <body> — popupMenu appends it there. */
  function lastMenu() {
    const body = (document as unknown as { body: FakeNode }).body;
    return body.children[body.children.length - 1];
  }

  it("a started row offers Rename, Disable, Delete — and Disable fires the stop verb", async () => {
    freshState({ mcps: [{ ...ROW }] as never });
    sidebar.rowMenu("m", { left: 10, top: 10, bottom: 10 });
    const menu = lastMenu();
    const labels = menu.children.filter((c) => c.tag === "BUTTON").map((c) => c.textContent);
    expect(labels).toEqual(["Rename…", "Disable", "Delete"]);
    const disable = menu.children.find((c) => c.textContent === "Disable")!;
    disable.onclick!({ stopPropagation() {} });
    await new Promise((r) => setTimeout(r, 0));
    expect(calls[0].url).toContain("/api/mcps/m/stop");
    expect((calls[0].init as { method?: string }).method).toBe("POST");
  });

  it("a stopped row offers Enable, and it fires the start verb", async () => {
    freshState({ mcps: [{ ...ROW, lifecycle: "stopped" }] as never });
    sidebar.rowMenu("m", { left: 10, top: 10, bottom: 10 });
    const menu = lastMenu();
    const enable = menu.children.find((c) => c.textContent === "Enable")!;
    enable.onclick!({ stopPropagation() {} });
    await new Promise((r) => setTimeout(r, 0));
    expect(calls[0].url).toContain("/api/mcps/m/start");
  });
});


/*
 * Copyright 2026 The swiss authors
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

import { describe, it, expect, beforeAll, beforeEach, afterEach, vi } from "vitest";

/* OAuth authorize (docs/24): the form fields + the bool-to-string translation (fields.js),
   the pane header button (pane.js renderPane), and the one-click flow (detail.js authorizeMcp)
   — POST plants the flow, the consent URL opens in a real browser window once, polling ends on
   approved with the tools count in the pane note.

   No DOM library in this repo, so this suite drives the real module graph (detail.js ->
   pane.js -> add-sheet.js -> ...) under the same kind of micro-DOM admin-row-menu uses: an
   auto-creating getElementById (modules like add-sheet.js wire #addBtn at import time), a
   fetch queue, and fake timers for the 3s poll. */

class FakeNode {
  tag: string;
  attrs: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  textContent = "";
  innerHTML = "";
  id = "";
  type = "button";
  checked = false;
  value = "";
  hidden = false;
  disabled = false;
  style: Record<string, string> = {};
  onclick: ((ev?: unknown) => void) | null = null;
  constructor(tag: string) {
    this.tag = tag.toUpperCase();
  }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
  }
  getAttribute(k: string) {
    return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null;
  }
  querySelector() { return null; }
  querySelectorAll() { return []; }
  contains() { return false; }
  appendChild(n: FakeNode) { return n; }
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
  querySelector: () => null,
  querySelectorAll: () => [] as FakeNode[],
  body: new FakeNode("body"),
  addEventListener() {},
  activeElement: null as FakeNode | null,
};
const windowOpen = vi.fn();

let fields: typeof import("../src/fields.js");
let pane: typeof import("../src/pane.js");
let detail: typeof import("../src/detail.js");
let util: typeof import("../src/util.js");

beforeAll(async () => {
  (globalThis as unknown as Record<string, unknown>).document = doc;
  (globalThis as unknown as Record<string, unknown>).window = { open: windowOpen, addEventListener() {} };
  fields = await import("../src/fields.js");
  util = await import("../src/util.js");
  pane = await import("../src/pane.js");
  detail = await import("../src/detail.js");
});

function freshState(overrides: Record<string, unknown> = {}) {
  util.state.mcps = [];
  util.state.groups = ["default"];
  util.state.selected = null;
  util.state.detail = null;
  util.state.busy = {};
  util.state.lastAction = {};
  util.state.menuOpen = false;
  Object.assign(util.state, overrides);
}

/** A detail object shaped the way openDetail builds it, trimmed to what renderPane reads. */
function fakeDetail(name: string, config: Record<string, unknown> | null) {
  const kd = { items: [], nextCursor: undefined, total: undefined, loaded: false, loading: false, error: null, cursors: [""], pageSize: 50 };
  return {
    name, tab: "tools", config, source: "managed", editing: false, editType: null, editVals: null,
    oauth: undefined, oauthBusy: false,
    tools: { ...kd }, resources: { ...kd }, prompts: { ...kd },
    run: { tool: null, result: null, running: false, hist: null, histTool: null, histLoading: false, histOpen: false, histSelSeq: null, histFull: {}, histQ: "" },
    calls: null, stderr: "", callsOpen: {}, callsLoading: false, callsPage: 0, callsMore: false, callsFull: {},
  } as never;
}

/** One list row the way /api/mcps answers it (the oauth field only on oauth MCPs). */
function row(oauth?: string) {
  return { name: "fig", lifecycle: "started", state: "unknown", type: "http", tag: "http", source: "managed", description: "", group: "default", ...(oauth ? { oauth } : {}) };
}

describe("oauth form fields (docs/24 D1)", () => {
  it("the http type carries an auth checkbox and an optional client name", () => {
    const keys = fields.TYPE_FIELDS.http.map((f) => f.k);
    expect(keys).toContain("auth");
    expect(keys).toContain("oauthClientName");
    const auth = fields.TYPE_FIELDS.http.find((f) => f.k === "auth")!;
    expect(auth.bool).toBe(true);
    expect(auth.def).toBe(false);
  });

  it("translateOauth maps the checkbox to the def string both ways", () => {
    expect(fields.translateOauth({ auth: true } as never)).toEqual({ auth: "oauth" });
    // Unchecked REMOVES the key — that is how OAuth is switched back off server-side.
    expect(fields.translateOauth({ auth: false, url: "https://mcp.figma.com/mcp" } as never)).toEqual({ url: "https://mcp.figma.com/mcp" });
    // An empty client name never travels; a chosen one does.
    expect(fields.translateOauth({ auth: true, oauthClientName: "" } as never)).toEqual({ auth: "oauth" });
    expect(fields.translateOauth({ auth: true, oauthClientName: "Codex" } as never)).toEqual({ auth: "oauth", oauthClientName: "Codex" });
  });

  it("readFields reads the checkbox the translation then turns into the def string", () => {
    byId.set("f-auth", Object.assign(new FakeNode("input"), { checked: true }));
    const read = fields.readFields("http", "f-");
    expect(read.auth).toBe(true);
    expect(fields.translateOauth(read).auth).toBe("oauth");
  });
});

describe("the figma type (docs/24 rev): one field, everything implied", () => {
  it("the figma form carries no url, no auth box — the type decides those", () => {
    const keys = fields.TYPE_FIELDS.figma.map((f) => f.k);
    expect(keys).toContain("description");
    expect(keys).not.toContain("url");
    expect(keys).not.toContain("auth");
    expect(fields.TYPE_LABELS.figma).toContain("figma");
    // No Test button: a keyless handshake is always 401 for an OAuth remote.
    expect(fields.TESTABLE_TYPES).not.toContain("figma");
  });

  it("a figma detail renders the Authorize button with no auth key on the config", () => {
    byId.clear(); freshState();
    freshState({ mcps: [{ ...row(), name: "fig2", tag: "figma", oauth: "needs-auth" }] as never, detail: fakeDetail("fig2", { type: "figma" }) });
    pane.renderPane();
    expect(byId.get("pane")!.innerHTML).toContain('id="oauthBtn"');
    expect(byId.get("pane")!.innerHTML).toContain("Authorize");
  });

  it("runConnTest answers the honest message for a figma MCP without firing a request", async () => {
    byId.clear(); freshState();
    freshState({ detail: { ...fakeDetail("fig2", { type: "figma" }), editing: true, editType: "figma" } as never });
    let fired = 0;
    (globalThis as unknown as Record<string, unknown>).fetch = async () => { fired++; return { ok: true, status: 200, json: async () => ({}) } as never; };
    await detail.runConnTest("e-");
    expect(fired).toBe(0);
    expect(doc.getElementById("e-test-out").textContent).toContain("OAuth remote");
  });
});

describe("both submit paths run the translation through the real modules (docs/24 D1)", () => {
  // The browser-only failure class this suite guards: an identifier used but never imported
  // (translateOauth) links fine in vitest's transform and passes every import-graph walk —
  // only RUNNING the call site bites. So both submit paths are driven end to end here.

  let addSheet: typeof import("../src/add-sheet.js");

  beforeAll(async () => {
    addSheet = await import("../src/add-sheet.js");
    (globalThis as unknown as Record<string, unknown>).localStorage = {
      getItem: () => null, setItem() {}, removeItem() {},
    };
  });

  it("submitAdd posts auth oauth for a checked box and drops the empty client name", async () => {
    byId.clear(); freshState();
    // doc.getElementById auto-creates; byId.get does not, so seed the form through doc.
    Object.assign(doc.getElementById("a-name"), { value: "figma" });
    Object.assign(doc.getElementById("a-type"), { value: "http" });
    Object.assign(doc.getElementById("a-url"), { value: "https://mcp.figma.com/mcp" });
    Object.assign(doc.getElementById("a-auth"), { checked: true });
    Object.assign(doc.getElementById("a-start"), { checked: true });
    doc.getElementById("g-sel").value = "learn";
    const bodies: Array<[string, unknown]> = [];
    (globalThis as unknown as Record<string, unknown>).fetch = async (path: string, opts: Record<string, unknown>) => {
      const p = String(path);
      let body: unknown = null;
      if (typeof opts.body === "string") { body = JSON.parse(opts.body); bodies.push([p, body]); }
      if (p === "/api/mcps" && opts.method === "POST") return { ok: true, status: 200, json: async () => ({ lifecycle: "stopped" }) } as never;
      if (p.startsWith("/api/groups/")) return { ok: true, status: 200, json: async () => ({}) } as never;
      if (p === "/api/mcps") return { ok: true, status: 200, json: async () => ({ mcps: [row("needs-auth")], groups: ["learn"] }) } as never;
      if (p.endsWith("/details")) return { ok: true, status: 200, json: async () => ({ config: { type: "http", auth: "oauth" }, oauth: "needs-auth", tunnels: [] }) } as never;
      return { ok: true, status: 200, json: async () => ({}) } as never;
    };
    await addSheet.submitAdd();
    const post = bodies.find(([p, b]) => p === "/api/mcps" && (b as Record<string, unknown>).name === "figma");
    expect(post).toBeTruthy();
    const sent = post![1] as Record<string, unknown>;
    expect(sent.auth).toBe("oauth");
    expect("oauthClientName" in sent).toBe(false);
  });

  it("saveEdit PUTs auth oauth through the edit form", async () => {
    byId.clear(); freshState();
    util.state.detail = { ...fakeDetail("fig", { type: "http", auth: "oauth" }), editing: true, editType: "http" } as never;
    Object.assign(doc.getElementById("e-url"), { value: "https://mcp.figma.com/mcp" });
    Object.assign(doc.getElementById("e-auth"), { checked: true });
    let put: Record<string, unknown> | null = null;
    (globalThis as unknown as Record<string, unknown>).fetch = async (path: string, opts: Record<string, unknown>) => {
      const p = String(path);
      if (opts.method === "PUT" && p === "/api/mcps/fig") {
        put = JSON.parse(opts.body as string);
        return { ok: true, status: 200, json: async () => ({}) } as never;
      }
      if (p === "/api/mcps") return { ok: true, status: 200, json: async () => ({ mcps: [row("authorized")], groups: ["default"] }) } as never;
      if (p.endsWith("/details")) return { ok: true, status: 200, json: async () => ({ config: { type: "http", auth: "oauth" }, oauth: "authorized", tunnels: [] }) } as never;
      return { ok: true, status: 200, json: async () => ({}) } as never;
    };
    await detail.saveEdit();
    expect(put).toBeTruthy();
    expect(put!.auth).toBe("oauth");
  });
});

describe("the pane header authorize button (docs/24 D5)", () => {
  beforeEach(() => { byId.clear(); freshState(); });

  it("renders Authorize for an oauth MCP that has no stored grant", () => {
    freshState({ mcps: [row("needs-auth")] as never, detail: fakeDetail("fig", { type: "http", auth: "oauth" }) });
    pane.renderPane();
    expect(byId.get("pane")!.innerHTML).toContain('id="oauthBtn"');
    expect(byId.get("pane")!.innerHTML).toContain("Authorize");
    // The subtitle carries the badge state the server reported.
    expect(byId.get("pane")!.innerHTML).toContain("oauth: needs-auth");
  });

  it("renders Reauthorize once a grant is stored", () => {
    freshState({ mcps: [row("authorized")] as never, detail: fakeDetail("fig", { type: "http", auth: "oauth" }) });
    pane.renderPane();
    expect(byId.get("pane")!.innerHTML).toContain("Reauthorize");
  });

  it("a plain http MCP gets no button", () => {
    freshState({ mcps: [row()] as never, detail: fakeDetail("fig", { type: "http", url: "https://x.test/mcp" }) });
    pane.renderPane();
    expect(byId.get("pane")!.innerHTML).not.toContain("oauthBtn");
  });

  it("the button click starts the flow (the POST leaves with the MCP's name)", async () => {
    freshState({ mcps: [row("needs-auth")] as never, detail: fakeDetail("fig", { type: "http", auth: "oauth" }) });
    const calls: Array<[string, Record<string, unknown>]> = [];
    (globalThis as unknown as Record<string, unknown>).fetch = async (path: string, opts: Record<string, unknown>) => {
      calls.push([String(path), opts]);
      return { ok: true, status: 200, json: async () => ({ status: "starting" }) } as never;
    };
    pane.renderPane();
    byId.get("oauthBtn")!.onclick!();
    // The POST fires before the first poll; the flow path is name-keyed. Real timers here, so
    // two microtask/setTimeout rounds are enough for the mocked fetch to settle.
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    expect(calls.some(([p, o]) => p === "/api/mcps/fig/authorize" && o.method === "POST")).toBe(true);
  });
});

describe("the authorize flow (docs/24 D5)", () => {
  beforeEach(() => {
    byId.clear();
    freshState();
    windowOpen.mockClear();
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it("opens the consent page once, then reports approved with the tools count", async () => {
    freshState({ mcps: [row("needs-auth")] as never, detail: fakeDetail("fig", { type: "http", auth: "oauth" }) });
    const queue = [
      { status: 202, body: { flowId: "f1", status: "starting" } },                       // POST authorize
      { status: 200, body: { status: "starting", flowId: "f1" } },                       // poll 1: still preparing
      { status: 200, body: { status: "authorization_required", flowId: "f1", authorizationUrl: "https://as.test/authorize?client_id=x" } },
      { status: 200, body: { status: "authorization_required", flowId: "f1", authorizationUrl: "https://as.test/authorize?client_id=x" } }, // again: must NOT reopen
      { status: 200, body: { status: "approved", flowId: "f1", tools: 26 } },
      { status: 200, body: { mcps: [row("authorized")], groups: ["default"] } },          // loadList
      { status: 200, body: { config: { type: "http", auth: "oauth" }, oauth: "authorized", tunnels: [] } }, // loadMeta
    ];
    const seen: string[] = [];
    (globalThis as unknown as Record<string, unknown>).fetch = async (path: string) => {
      const next = queue.shift()!;
      seen.push(String(path));
      return { ok: true, status: next.status, json: async () => next.body } as never;
    };
    const d = util.state.detail as unknown as { oauthBusy: boolean; name: string };
    const done = detail.authorizeMcp("fig");
    // Drive the 3s polls: each advance settles one poll round.
    for (let i = 0; i < 6 && !(await Promise.race([done.then(() => true), Promise.resolve(false)])); i++) {
      await vi.advanceTimersByTimeAsync(3000);
    }
    await done;
    expect(windowOpen).toHaveBeenCalledTimes(1);
    expect(windowOpen).toHaveBeenCalledWith("https://as.test/authorize?client_id=x", "_blank");
    expect(util.state.lastAction.fig.msg).toContain("authorized · 26 tools");
    expect(d.oauthBusy).toBe(false);
    expect(seen[0]).toBe("/api/mcps/fig/authorize");
  });

  it("a flow error lands in the pane note and clears busy", async () => {
    freshState({ mcps: [row("needs-auth")] as never, detail: fakeDetail("fig", { type: "http", auth: "oauth" }) });
    const queue = [
      { status: 202, body: { flowId: "f1", status: "starting" } },
      { status: 200, body: { status: "error", flowId: "f1", error: "oauth registration failed: name not allowed" } },
      { status: 200, body: { mcps: [row("needs-auth")], groups: ["default"] } }, // loadList on error
    ];
    (globalThis as unknown as Record<string, unknown>).fetch = async () => {
      const next = queue.shift()!;
      return { ok: true, status: next.status, json: async () => next.body } as never;
    };
    const d = util.state.detail as unknown as { oauthBusy: boolean };
    const done = detail.authorizeMcp("fig");
    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(3000);
      try { await done; break; } catch { /* pending */ }
    }
    await done;
    expect(util.state.lastAction.fig.err).toBe(true);
    expect(util.state.lastAction.fig.msg).toContain("name not allowed");
    expect(d.oauthBusy).toBe(false);
  });
});

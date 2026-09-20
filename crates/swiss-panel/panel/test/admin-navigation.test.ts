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

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* The adaptive shell (docs/13 D5, redrawn): a plugin RAIL for global navigation and a
   compact page SWITCHER ("MCP / Servers" + menu) for page navigation. No bundler and no
   browser in the suite, so this drives the real page-registry under a hand-rolled DOM:
   fake elements that record what paintNavigation writes, and a fetch stub answering
   /api/plugins the way the plugin-aware gateway (full inventory) or an older gateway
   (404) would. What is asserted is the generated markup and the hidden state — the exact
   contract the browser renders. */

/* docs/37 R5: the rail/context bar builders construct NODES now, so the fake element is
 * a kid-carrying node with a serializing innerHTML getter. Attribute order in the output
 * (class, id, title, then the bag attrs in insertion order) is chosen so the historical
 * string assertions below keep reading the way they always did. */
class NodeStub {}
class FakeNode extends NodeStub {
  tag: string;
  className = "";
  id = "";
  title = "";
  hidden = false;
  disabled = false;
  text = "";
  onclick: unknown = null;
  attrs: Record<string, string> = {};
  kids: unknown[] = [];
  constructor(tag: string) { super(); this.tag = tag; }
  setAttribute(k: string, v: string) { this.attrs[k] = v; }
  get textContent(): string { return this.kids.length ? this.kids.map(textOf).join("") : this.text; }
  set textContent(v: string) { this.kids = []; this.text = v; }
  appendChild(n: unknown): unknown {
    // A fragment splices; a finished node lands whole (the DOM contract fill() relies on).
    const f = n as { kids?: unknown[] };
    if (f && Array.isArray(f.kids) && (n as { tag: string }).tag === "#document-fragment") this.kids.push(...f.kids);
    else this.kids.push(n);
    return n;
  }
  get innerHTML(): string { return this.kids.map(markupOf).join(""); }
}
const textOf = (n: unknown): string => {
  const node = n as FakeNode;
  if (node && typeof node.tag === "string") return node.textContent;
  return String(n);
};
const markupOf = (n: unknown): string => {
  const node = n as FakeNode;
  if (!node || typeof node.tag !== "string" || node.tag === "#text") return textOf(node);
  let attrStr = "";
  if (node.className) attrStr += ' class="' + node.className + '"';
  if (node.id) attrStr += ' id="' + node.id + '"';
  if (node.title) attrStr += ' title="' + node.title + '"';
  for (const k of Object.keys(node.attrs)) attrStr += " " + k + '="' + node.attrs[k] + '"';
  if (node.disabled) attrStr += " disabled";
  return "<" + node.tag + attrStr + ">" + node.kids.map(markupOf).join("") + "</" + node.tag + ">";
};
const fakeEl = (): FakeNode => new FakeNode("div");

const els = new Map<string, FakeNode>();
/* Map-backed so the last-page memory round-trips (docs/39 S4): navigatePage writes the
 * store, the seat click reads it back. Every other consumer sees an empty store, exactly
 * what the old always-null stub gave them. */
const memoryStore = new Map<string, string>();
let responder: (path: string) => Promise<{ status: number; ok: boolean; json: () => Promise<unknown> }>;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let registry: any; // the whole page-registry surface, reached loosely: this suite pokes internals
let setCurrentView: (id: string) => void;
let currentView: () => string;

const jsonResponse = (status: number, body?: unknown) => ({
  status,
  ok: status < 400,
  json: async () => body,
});

/* Shaped like the Rust gateway's /api/plugins: pages[] carries the pluginId grouping keys
   on, plugins[] carries the label the rail seat shows, layout carries the adaptive-shell
   framing (absent on older gateways — the panel must fall back to sidebar). */
const inventory = {
  plugins: [
    { id: "mcp", label: "MCP", enabled: true, state: "running" },
    { id: "tunnels", label: "Tunnels", enabled: true, state: "running" },
    { id: "jobs", label: "Jobs", enabled: true, state: "running" },
    { id: "terminal", label: "Terminal", enabled: true, state: "running" },
  ],
  pages: [
    { id: "mcps", pluginId: "mcp", label: "Servers", order: 10, sidebar: true, layout: "resource", path: "#mcps", entry: "/admin/js/views/mcps.js" },
    { id: "traffic", pluginId: "mcp", label: "Traffic", order: 20, layout: "page", path: "#traffic", entry: "/admin/js/views/traffic.js" },
    { id: "tokens", pluginId: "mcp", label: "Token", order: 30, layout: "page", path: "#tokens", entry: "/admin/js/views/tokens.js" },
    { id: "tunnels", pluginId: "tunnels", label: "SSH Connections", order: 30, layout: "page", path: "#tunnels", entry: "/admin/js/views/tunnels.js" },
    { id: "tunnel-forwards", pluginId: "tunnels", label: "Port Forwards", order: 35, layout: "page", path: "#tunnel-forwards", entry: "/admin/js/views/tunnel-forwards.js" },
    { id: "jobs", pluginId: "jobs", label: "Jobs", order: 50, layout: "page", path: "#jobs", entry: "/admin/js/views/jobs.js" },
    { id: "terminal", pluginId: "terminal", label: "Terminal", order: 70, layout: "workspace", path: "#terminal", entry: "/admin/js/views/terminal.js" },
  ],
};

const paint = async (view: string, body: unknown, status = 200) => {
  setCurrentView(view);
  responder = () => Promise.resolve(jsonResponse(status, body));
  await registry.reloadPluginInventory();
};

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    // h.js classifies built children with instanceof Node; FakeNode genuinely extends it.
    Node: NodeStub,
    document: {
      getElementById: (id: string) => {
        let e = els.get(id);
        if (!e) { e = fakeEl(); els.set(id, e); }
        return e;
      },
      createElement: (t: string) => new FakeNode(t),
      createElementNS: (_ns: string, t: string) => new FakeNode(t),
      createDocumentFragment: () => new FakeNode("#document-fragment"),
      createTextNode: (s: string) => { const n = new FakeNode("#text"); n.textContent = s; return n; },
      // navigatePage toggles the resource sidebar's hidden flag on the way in.
      querySelector: (sel: string) => (sel === ".sidebar" ? fakeEl() : null),
      querySelectorAll: () => [],
      addEventListener: () => {},
      removeEventListener: () => {},
      documentElement: fakeEl(),
      body: fakeEl(),
      head: fakeEl(),
      visibilityState: "visible",
      activeElement: null,
    },
    fetch: (path: unknown) => responder(String(path)),
    // navigatePage calls history.replaceState after landing; node has no history global.
    history: { replaceState: () => {} },
    // Data's canLeave asks "discard uncommitted changes?" - the veto test answers no.
    confirm: () => false,
    localStorage: {
      getItem: (k: string) => (memoryStore.has(k) ? memoryStore.get(k) as string : null),
      setItem: (k: string, v: string) => { memoryStore.set(k, v); },
      removeItem: (k: string) => { memoryStore.delete(k); },
    },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  ({ setCurrentView, currentView } = await import("../src/ui-state.js"));
  registry = await import("../src/page-registry.js");
});

const byId = (id: string): FakeNode => els.get(id) as FakeNode;

describe("the plugin rail (global navigation)", () => {
  it("paints one seat per plugin group — never one per page", async () => {
    await paint("mcps", inventory);
    const rail = byId("railNav").innerHTML;
    // One seat for the MCP plugin, landing on its first page; Traffic/Token are pages, not plugins.
    expect(rail).toContain('data-group="mcp" data-view="mcps"');
    expect(rail).not.toContain('data-view="traffic"');
    expect(rail).not.toContain('data-view="tokens"');
    // Single-page plugins keep their own seats; a plugin's SECOND page never takes a rail
    // seat of its own (the context bar switches pages, the rail switches plugins).
    expect(rail).toContain('data-group="tunnels" data-view="tunnels"');
    expect(rail).not.toContain('data-view="tunnel-forwards"');
    expect(rail).toContain('data-group="jobs" data-view="jobs"');
    // The workspace plugin rides the rail like any peer.
    expect(rail).toContain('data-group="terminal" data-view="terminal"');
    // The synthesized management page has no inventory row: GROUP_LABELS names its group.
    expect(rail).toContain('data-group="host" data-view="plugins"');
    expect(rail).toContain(">Settings</span>");
    // The ... seat opens the palette; the rail is a shortlist, not the ceiling.
    expect(rail).toContain('class="rail-btn rail-more" id="railMore"');
    expect(typeof byId("railNav").onclick).toBe("function");
  });

  it("the active plugin is marked once, with aria-current on its seat", async () => {
    await paint("mcps", inventory);
    const rail = byId("railNav").innerHTML;
    expect(rail).toContain('data-group="mcp" data-view="mcps" aria-current="true"');
    expect(rail).not.toMatch(/data-group="tunnels"[^>]*aria-current/);
  });

  it("marks an unavailable plugin's seat without removing it", async () => {
    const disabled: typeof inventory = {
      ...inventory,
      plugins: inventory.plugins.map((p) => (p.id === "mcp" ? { ...p, enabled: false } : p)),
    };
    await paint("mcps", disabled);
    const rail = byId("railNav").innerHTML;
    expect(rail).toMatch(/data-group="mcp"[^>]*aria-disabled="true"/);
    expect(rail).toContain('title="MCP — Plugin disabled"');
    // Other groups are untouched.
    expect(rail).not.toMatch(/data-group="tunnels"[^>]*aria-disabled/);
  });

  it("a seat reopens the plugin's LAST page; data-view stays the first page id (docs/39 S4)", async () => {
    // Page data fetches fail softly (500) so the real view modules mount without painting -
    // this test is about the shell's navigation, not the traffic or jobs bodies.
    memoryStore.clear();
    await paint("mcps", inventory);
    responder = (path: string) => Promise.resolve(
      path === "/api/plugins" ? jsonResponse(200, inventory) : jsonResponse(500, { error: "stub" }));

    // Walk #traffic, then #jobs, through the REAL navigatePage - it is what records the visit.
    await registry.navigatePage("traffic");
    expect(JSON.parse(memoryStore.get("swiss.lastPage") as string)).toEqual({ mcp: "traffic" });
    await registry.navigatePage("jobs");
    expect(currentView()).toBe("jobs");

    // Click the MCP seat through the rail's delegated handler (the fake target answers the
    // two closest() probes the real DOM would).
    const seat = { dataset: { group: "mcp" } };
    const click = byId("railNav").onclick as unknown as (ev: { target: unknown }) => void;
    click({ target: { closest: (sel: string) => (sel === "[data-group]" ? seat : null) } });
    await new Promise((resolve) => { setTimeout(resolve, 0); });

    // The landing is Traffic, the rail still marks MCP, and the seat's data-view attribute
    // is still the FIRST page id - jobs.ts's deep selector and the assertion above it rely on it.
    expect(currentView()).toBe("traffic");
    expect(byId("railNav").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-current="true"');
    expect(memoryStore.get("swiss.lastPage")).toContain('"mcp":"traffic"');
  });

  it("a leave the page vetoes neither navigates nor rewrites the memory (docs/39 S4)", async () => {
    memoryStore.clear();
    // The shared inventory has no Data page; this test needs the one plugin whose view vetoes.
    const withData = {
      ...inventory,
      pages: inventory.pages.concat([
        { id: "data", pluginId: "data", label: "Data", order: 40, layout: "workspace", path: "#data", entry: "/admin/js/views/data.js" },
      ]),
    };
    await paint("mcps", withData);
    responder = (path: string) => Promise.resolve(
      path === "/api/plugins" ? jsonResponse(200, withData) : jsonResponse(500, { error: "stub" }));
    await registry.navigatePage("traffic");
    await registry.navigatePage("data");
    expect(currentView()).toBe("data");
    // One buffered Data edit is what makes the real view veto the leave (dbPending() > 0).
    const dbState = await import("../src/db-state.js");
    dbState.dbView().inserts = [{ values: { a: 1 } }];
    const before = memoryStore.get("swiss.lastPage");

    const seat = { dataset: { group: "mcp" } };
    const click = byId("railNav").onclick as unknown as (ev: { target: unknown }) => void;
    click({ target: { closest: (sel: string) => (sel === "[data-group]" ? seat : null) } });
    await new Promise((resolve) => { setTimeout(resolve, 0); });

    // Still on Data, hash pushed back, memory untouched - the veto kept both.
    expect(currentView()).toBe("data");
    expect(memoryStore.get("swiss.lastPage")).toBe(before);
    expect(memoryStore.get("swiss.lastPage")).not.toContain("mcp");
  });
});

describe("the plugin context bar (page navigation)", () => {
  it("MCP carries Servers / Traffic / Token in the switcher and its menu", async () => {
    await paint("mcps", inventory);
    expect(byId("ctxBar").hidden).toBe(false);
    const btn = byId("pageBtn");
    expect(btn.innerHTML).toContain('ctx-plugin">MCP</span>');
    expect(btn.innerHTML).toContain('ctx-page">Servers</span>');
    expect(typeof btn.onclick).toBe("function");
    const mcpPages = inventory.pages
      .filter((p) => p.pluginId === "mcp")
      .sort((a, b) => a.order - b.order);
    const items = registry.pageMenuItems({ id: "mcp", pages: mcpPages });
    expect(items.map((i: { label: string }) => i.label)).toEqual(["Servers", "Traffic", "Token"]);
    expect(items[0].on).toBe(true);
    expect(items.every((i: { pick: boolean }) => i.pick)).toBe(true);
  });

  it("deep-linking #traffic selects MCP in the rail and Traffic in the switcher", async () => {
    await paint("traffic", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-current="true"');
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">Traffic</span>');
  });

  it("deep-linking #tokens selects MCP in the rail and Token in the switcher", async () => {
    await paint("tokens", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-current="true"');
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">Token</span>');
  });

  it("synthesizes System under Settings instead of giving a destructive action permanent chrome", async () => {
    await paint("system", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="host" data-view="plugins" aria-current="true"');
    expect(byId("pageBtn").hidden).toBe(false);
    expect(byId("pageBtn").innerHTML).toContain('ctx-plugin">Settings</span>');
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">System</span>');
  });

  it("tunnels is a two-page group: #tunnels is SSH Connections, #tunnel-forwards is Port Forwards", async () => {
    const tunnelsPages = inventory.pages.filter((p) => p.pluginId === "tunnels").sort((a, b) => a.order - b.order);
    expect(tunnelsPages.map((p) => p.label)).toEqual(["SSH Connections", "Port Forwards"]);
    const items = registry.pageMenuItems({ id: "tunnels", pages: tunnelsPages });
    expect(items.map((i: { label: string }) => i.label)).toEqual(["SSH Connections", "Port Forwards"]);

    // The legacy deep link keeps its meaning: rail selects Tunnels, bar says SSH Connections.
    await paint("tunnels", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-current="true"');
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageBtn").hidden).toBe(false);
    expect(byId("pageBtn").innerHTML).toContain('ctx-plugin">Tunnels</span>');
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">SSH Connections</span>');

    // The new sibling lands in the SAME group, on its own page.
    await paint("tunnel-forwards", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-current="true"');
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">Port Forwards</span>');
  });

  it("a single-page plugin keeps the bar — a static location, not a fake dropdown", async () => {
    await paint("jobs", inventory);
    expect(byId("ctxBar").hidden).toBe(false);
    // The location reads as text (same slot and weight), because a one-entry menu would lie.
    expect(byId("pageBtn").hidden).toBe(true);
    expect(byId("pageBtn").onclick).toBe(null);
    expect(byId("pageLoc").hidden).toBe(false);
    expect(byId("pageLoc").innerHTML).toContain('ctx-page">Jobs</span>');
    // No redundant "Jobs / Jobs" breadcrumb either.
    expect(byId("pageLoc").innerHTML).not.toContain("ctx-sep");
    // ...and the rail still seats and selects it.
    expect(byId("railNav").innerHTML).toContain('data-group="jobs" data-view="jobs" aria-current="true"');
  });

  it("the terminal is a workspace page and still lives under the same bar", async () => {
    await paint("terminal", inventory);
    // Workspace frames the BODY only; the shell keeps drawing its chrome (docs/13 D5 rev.).
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageLoc").hidden).toBe(false);
    expect(byId("pageLoc").innerHTML).toContain('ctx-page">Terminal</span>');
    expect(byId("railNav").innerHTML).toContain('data-group="terminal" data-view="terminal" aria-current="true"');
  });

  it("page chips sit left, the app trio (mem, theme, focus) owns the far right of the bar", () => {
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");
    const rail = shell.slice(shell.indexOf('class="rail"'), shell.indexOf('class="workbench"'));
    const ctx = shell.slice(shell.indexOf('class="ctxbar"'), shell.indexOf('class="shell"'));
    expect(rail).not.toContain("countChip");
    expect(ctx).toContain('id="countChip"');
    expect(ctx).toContain('id="memChip"');
    // The reserved app zone: theme moved here from the rail foot, focus stays far right.
    expect(rail).not.toContain('id="expandBtn"');
    expect(rail).not.toContain('id="themeBtn"');
    expect(ctx).toContain('id="themeBtn"');
    expect(ctx).toContain('id="expandBtn"');
    // The two-segment-control era is gone from the shell.
    expect(shell).not.toContain('id="viewSeg"');
    expect(shell).not.toContain('id="subBar"');
    expect(shell).not.toContain('id="subSeg"');
  });
});

describe("layouts (docs/13 D5)", () => {
  it("the descriptor states the layout; sidebar derives it when a gateway is older", () => {
    expect(registry.layoutOf({ layout: "workspace" })).toBe("workspace");
    expect(registry.layoutOf({ layout: "resource" })).toBe("resource");
    expect(registry.layoutOf({ layout: "page" })).toBe("page");
    expect(registry.layoutOf({ sidebar: true })).toBe("resource");
    expect(registry.layoutOf({ sidebar: false })).toBe("page");
    expect(registry.layoutOf(undefined)).toBe("page");
  });

  it("an older gateway answering 404 falls back to the legacy manifest and its labels", async () => {
    await paint("mcps", null, 404);
    const rail = byId("railNav").innerHTML;
    // Group label from GROUP_LABELS; the seat lands on the manifest's first page.
    expect(rail).toContain('data-group="mcp" data-view="mcps"');
    expect(rail).not.toContain('data-view="traffic"');
    expect(rail).toContain(">MCP</span>");
    // The switcher keeps working: the legacy manifest's MCP group has three pages.
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">MCPs</span>');
  });

  it("the legacy manifest grows the same two tunnels pages as the inventory", async () => {
    await paint("tunnel-forwards", null, 404);
    // One group, both pages, correct labels — the 404 path must not collapse back to one.
    expect(byId("railNav").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-current="true"');
    expect(byId("pageBtn").innerHTML).toContain('ctx-plugin">Tunnels</span>');
    expect(byId("pageBtn").innerHTML).toContain('ctx-page">Port Forwards</span>');
  });

  it("a multi-page group whose pages cannot serve keeps its switcher, marked · off", async () => {
    const disabled: typeof inventory = {
      ...inventory,
      plugins: inventory.plugins.map((p) => (p.id === "mcp" ? { ...p, enabled: false } : p)),
    };
    await paint("mcps", disabled);
    const mcpPages = inventory.pages.filter((p) => p.pluginId === "mcp");
    const items = registry.pageMenuItems({ id: "mcp", pages: mcpPages });
    expect(items.map((i: { label: string }) => i.label)).toEqual(["Servers · off", "Traffic · off", "Token · off"]);
    expect(items[0].title).toBe("Plugin disabled");
    // The switcher itself stays reachable - an unavailable plugin is one click from Plugins.
    expect(byId("ctxBar").hidden).toBe(false);
  });
});

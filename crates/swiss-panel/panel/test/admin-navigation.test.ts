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

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* The adaptive shell (SPEC §panel.nav, redrawn; SPEC §panel.nav): a plugin RAIL for global
   navigation and a TITLE + underline TABS strip in the context bar for page navigation.
   No bundler and no browser in the suite, so this drives the real page-registry under a
   hand-rolled DOM: fake elements that record what paintNavigation writes, and a fetch stub
   answering /api/plugins the way the plugin-aware gateway (full inventory) or an older
   gateway (404) would. What is asserted is the generated markup and the hidden state — the
   exact contract the browser renders. */

/* SPEC §panel.toolchain: the rail/context bar builders construct NODES now, so the fake element is
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
  href = "";
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
  /* The tab strip's measuring half (SPEC §panel.nav) walks the DOM the browser offers: element
   * children, classList, dataset, a scoped querySelector and box widths. The fake carries
   * just enough of each for layoutTabs to run un-mocked. width models a tab's natural box
   * width and offsetWidth folds in the CSS rule that matters ([hidden] is display:none, so
   * a hidden element measures 0) - the default 0 keeps every unpinned test on fitTabs' own
   * "everything fits at zero" rule, while the refit regression below can stage real boxes. */
  get children(): unknown[] {
    return this.kids.filter((k) => {
      const t = (k as FakeNode).tag;
      return typeof t === "string" && t !== "#text" && t !== "#document-fragment";
    });
  }
  get dataset(): Record<string, string> {
    const out: Record<string, string> = {};
    for (const k of Object.keys(this.attrs)) {
      if (k.startsWith("data-")) out[k.slice(5).replace(/-([a-z])/g, (_m, c) => String(c).toUpperCase())] = this.attrs[k];
    }
    return out;
  }
  classList = { contains: (c: string) => (" " + this.className + " ").includes(" " + c + " ") };
  querySelector(sel: string): unknown {
    if (!sel.startsWith(".")) return null;
    const want = sel.slice(1);
    return this.children.find((k) => (k as FakeNode).classList.contains(want)) ?? null;
  }
  clientWidth = 0;
  width = 0;
  get offsetWidth(): number { return this.hidden ? 0 : this.width; }
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
/* Map-backed so the last-page memory round-trips (SPEC §panel.nav): navigatePage writes the
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
/* The painted tab strip's links, in order. Most suites leave the box widths at 0, where
 * the fitting half degrades to "everything fits" (see FakeNode), and assert what is
 * painted: every page as a real link, aria-current on exactly the landing page, the ⋯
 * seat present but hidden. fitTabs' arithmetic has its own suite (fit-tabs.test.ts), and
 * the refit regression below stages real widths on these nodes. */
const tabLinks = (): FakeNode[] => (byId("pageTabs").kids as FakeNode[]).filter((k) => k.tag === "a");

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
    // Seats carry their caption again (SPEC §panel.nav icon-only reversed by owner, 2026-09-21):
    // the group's translated name sits under the glyph at --f-caption, and the title
    // tooltip still carries it (plus the error text when the plugin is down).
    expect(rail).toContain('title="Settings" data-group="host" data-view="plugins"');
    expect(rail).toContain('class="rail-label">Settings</span>');
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

  it("a seat reopens the plugin's LAST page; data-view stays the first page id (SPEC §panel.nav)", async () => {
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

  it("a leave the page vetoes neither navigates nor rewrites the memory (SPEC §panel.nav)", async () => {
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
    const dbTabRec = dbState.dbTab();
    if (dbTabRec.kind === "table") dbTabRec.inserts = [{ values: { a: 1 } }];
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

describe("the tab strip's refit (SPEC §panel.nav, found live at 480px)", () => {
  it("a re-entry pass over an already-fitted strip keeps the fit instead of re-expanding it", async () => {
    await paint("mcps", inventory);
    /* Stage real boxes: a 120px strip, 60px tabs, a 32px ... seat - the widths of the
     * live repro. [hidden] models display:none, so a hidden tab measures 0 (FakeNode).
     * Pass 1 fits: total 180 > 120, budget 88, prefix cut leaves [mcps] (the active tab),
     * traffic/tokens behind the ... seat. */
    const tabs = byId("pageTabs");
    tabs.clientWidth = 120;
    tabLinks().forEach((t) => { t.width = 60; });
    (tabs.querySelector(".ctx-more") as FakeNode).width = 32;
    registry.layoutTabs();
    const fitted = tabLinks().map((t) => !!t.hidden);
    expect(fitted).toEqual([false, true, true]);
    expect((tabs.querySelector(".ctx-more") as FakeNode).hidden).toBe(false);
    /* The bug this pins: any later pass (observer delivery, refit) used to measure the
     * hidden tabs at 0, read the strip as "everything fits", and un-hide exactly what the
     * fit had hidden - the strip overflowed its clip with the active tab out of view.
     * A pass over the SAME fitted DOM must land on the same fit. */
    registry.layoutTabs();
    expect(tabLinks().map((t) => !!t.hidden)).toEqual(fitted);
    expect((tabs.querySelector(".ctx-more") as FakeNode).hidden).toBe(false);
    // And a third, for good measure: convergence is a fixed point now.
    registry.layoutTabs();
    expect(tabLinks().map((t) => !!t.hidden)).toEqual(fitted);
  });
});

describe("the plugin context bar (page navigation)", () => {
  it("MCP carries Servers / Traffic / Token as underline tabs, its title wearing the rail glyph", async () => {
    await paint("mcps", inventory);
    expect(byId("ctxBar").hidden).toBe(false);
    // The title names the plugin once: the same glyph its rail seat wears, beside the label.
    const title = byId("pageTitle");
    expect(title.hidden).toBe(false);
    expect(title.innerHTML).toContain('href="#i-mcp"');
    expect(title.innerHTML).toContain('ctx-name">MCP</span>');
    // Three real links through the hash, the landing one marked, in declaration order.
    const tabs = byId("pageTabs");
    expect(tabs.hidden).toBe(false);
    expect(tabLinks().map((t) => t.attrs["data-page"])).toEqual(["mcps", "traffic", "tokens"]);
    expect(tabLinks().map((t) => t.href)).toEqual(["#mcps", "#traffic", "#tokens"]);
    expect(tabLinks()[0].attrs["aria-current"]).toBe("page");
    expect(tabLinks()[1].attrs["aria-current"]).toBeUndefined();
    // The ⋯ seat exists (overflow safety net) but hides while everything fits.
    const more = tabs.querySelector(".ctx-more") as FakeNode;
    expect(more).not.toBe(null);
    expect(more.hidden).toBe(true);
    // The ⋯ menu lists the same pages in the same order - the net and the strip agree.
    const mcpPages = inventory.pages
      .filter((p) => p.pluginId === "mcp")
      .sort((a, b) => a.order - b.order);
    const items = registry.pageMenuItems({ id: "mcp", pages: mcpPages });
    expect(items.map((i: { label: string }) => i.label)).toEqual(["Servers", "Traffic", "Token"]);
    expect(items[0].on).toBe(true);
    expect(items.every((i: { pick: boolean }) => i.pick)).toBe(true);
  });

  it("deep-linking #traffic selects MCP in the rail and marks the Traffic tab", async () => {
    await paint("traffic", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-current="true"');
    const current = tabLinks().filter((t) => t.attrs["aria-current"] === "page");
    expect(current.map((t) => t.attrs["data-page"])).toEqual(["traffic"]);
  });

  it("deep-linking #tokens selects MCP in the rail and marks the Token tab", async () => {
    await paint("tokens", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-current="true"');
    const current = tabLinks().filter((t) => t.attrs["aria-current"] === "page");
    expect(current.map((t) => t.attrs["data-page"])).toEqual(["tokens"]);
  });

  it("synthesizes System under Settings instead of giving a destructive action permanent chrome", async () => {
    await paint("system", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="host" data-view="plugins" aria-current="true"');
    const title = byId("pageTitle");
    expect(title.hidden).toBe(false);
    expect(title.innerHTML).toContain('ctx-name">Settings</span>');
    const current = tabLinks().filter((t) => t.attrs["aria-current"] === "page");
    expect(current.map((t) => t.attrs["data-page"])).toEqual(["system"]);
  });

  it("tunnels is a two-page group: #tunnels is SSH Connections, #tunnel-forwards is Port Forwards", async () => {
    const tunnelsPages = inventory.pages.filter((p) => p.pluginId === "tunnels").sort((a, b) => a.order - b.order);
    expect(tunnelsPages.map((p) => p.label)).toEqual(["SSH Connections", "Port Forwards"]);
    const items = registry.pageMenuItems({ id: "tunnels", pages: tunnelsPages });
    expect(items.map((i: { label: string }) => i.label)).toEqual(["SSH Connections", "Port Forwards"]);

    // The legacy deep link keeps its meaning: rail selects Tunnels, the title says Tunnels.
    await paint("tunnels", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-current="true"');
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageTitle").innerHTML).toContain('ctx-name">Tunnels</span>');
    expect(tabLinks().map((t) => t.attrs["data-page"])).toEqual(["tunnels", "tunnel-forwards"]);
    expect(tabLinks()[0].attrs["aria-current"]).toBe("page");

    // The new sibling lands in the SAME group, on its own tab.
    await paint("tunnel-forwards", inventory);
    expect(byId("railNav").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-current="true"');
    expect(tabLinks()[1].attrs["aria-current"]).toBe("page");
  });

  it("a single-page plugin keeps the bar — the title alone, no one-entry tabs", async () => {
    await paint("jobs", inventory);
    expect(byId("ctxBar").hidden).toBe(false);
    // The title IS the location; a one-entry tab strip would lie about what it does.
    const title = byId("pageTitle");
    expect(title.hidden).toBe(false);
    expect(title.innerHTML).toContain('ctx-name">Jobs</span>');
    expect(byId("pageTabs").hidden).toBe(true);
    // ...and the rail still seats and selects it.
    expect(byId("railNav").innerHTML).toContain('data-group="jobs" data-view="jobs" aria-current="true"');
  });

  it("the terminal is a workspace page and still lives under the same bar", async () => {
    await paint("terminal", inventory);
    // Workspace frames the BODY only; the shell keeps drawing its chrome (SPEC §panel.nav.).
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageTitle").hidden).toBe(false);
    expect(byId("pageTitle").innerHTML).toContain('ctx-name">Terminal</span>');
    expect(byId("pageTabs").hidden).toBe(true);
    expect(byId("railNav").innerHTML).toContain('data-group="terminal" data-view="terminal" aria-current="true"');
  });

  it("the title and tabs sit left, the app trio (mem, theme, focus) owns the far right of the bar", () => {
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");
    const rail = shell.slice(shell.indexOf('class="rail"'), shell.indexOf('class="workbench"'));
    const ctx = shell.slice(shell.indexOf('class="ctxbar"'), shell.indexOf('class="shell"'));
    // Page naming precedes the flex slack; readouts and the app zone follow it.
    const titleAt = ctx.indexOf('id="pageTitle"');
    const tabsAt = ctx.indexOf('id="pageTabs"');
    const growAt = ctx.indexOf('class="grow"');
    const countAt = ctx.indexOf('id="countChip"');
    const memAt = ctx.indexOf('id="memChip"');
    const zoneAt = ctx.indexOf('class="app-zone"');
    for (const at of [titleAt, tabsAt, growAt, countAt, memAt, zoneAt]) expect(at).toBeGreaterThan(-1);
    expect(titleAt).toBeLessThan(growAt);
    expect(tabsAt).toBeLessThan(growAt);
    expect(growAt).toBeLessThan(countAt);
    expect(countAt).toBeLessThan(memAt);
    expect(memAt).toBeLessThan(zoneAt);
    expect(rail).not.toContain("countChip");
    // The reserved app zone: theme moved here from the rail foot, focus stays far right.
    expect(rail).not.toContain('id="expandBtn"');
    expect(rail).not.toContain('id="themeBtn"');
    expect(ctx).toContain('id="themeBtn"');
    expect(ctx).toContain('id="expandBtn"');
    // The two-segment-control era is gone from the shell, and so is the boxed switcher.
    expect(shell).not.toContain('id="viewSeg"');
    expect(shell).not.toContain('id="subBar"');
    expect(shell).not.toContain('id="subSeg"');
    expect(shell).not.toContain('id="pageBtn"');
    expect(shell).not.toContain('id="pageLoc"');
  });
});

describe("layouts (SPEC §panel.nav)", () => {
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
    // Group label from GROUP_LABELS names the seat's tooltip (SPEC §panel.nav); the seat lands
    // on the manifest's first page.
    expect(rail).toContain('data-group="mcp" data-view="mcps"');
    expect(rail).not.toContain('data-view="traffic"');
    expect(rail).toContain('title="MCP"');
    // The tabs keep working: the legacy manifest's MCP group has three pages.
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageTitle").innerHTML).toContain('ctx-name">MCP</span>');
    expect(tabLinks().map((t) => t.attrs["data-page"])).toEqual(["mcps", "traffic", "tokens"]);
  });

  it("the legacy manifest grows the same two tunnels pages as the inventory", async () => {
    await paint("tunnel-forwards", null, 404);
    // One group, both pages, correct labels — the 404 path must not collapse back to one.
    expect(byId("railNav").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-current="true"');
    expect(byId("pageTitle").innerHTML).toContain('ctx-name">Tunnels</span>');
    expect(tabLinks().map((t) => t.attrs["data-page"])).toEqual(["tunnels", "tunnel-forwards"]);
  });

  it("a multi-page group whose pages cannot serve keeps its tabs, marked · off", async () => {
    const disabled: typeof inventory = {
      ...inventory,
      plugins: inventory.plugins.map((p) => (p.id === "mcp" ? { ...p, enabled: false } : p)),
    };
    await paint("mcps", disabled);
    const mcpPages = inventory.pages.filter((p) => p.pluginId === "mcp");
    const items = registry.pageMenuItems({ id: "mcp", pages: mcpPages });
    expect(items.map((i: { label: string }) => i.label)).toEqual(["Servers · off", "Traffic · off", "Token · off"]);
    expect(items[0].title).toBe("Plugin disabled");
    // The tabs stay reachable - an unavailable plugin is one click from Plugins. Each is
    // marked in the strip itself (label + aria-disabled), not only inside the ⋯ menu.
    expect(byId("ctxBar").hidden).toBe(false);
    expect(byId("pageTitle").title).toBe("Plugin disabled");
    for (const link of tabLinks()) {
      expect(link.attrs["aria-disabled"]).toBe("true");
      expect(link.textContent).toContain("· off");
    }
    // ...and still navigate: the landing tab carries aria-current like any other.
    expect(tabLinks()[0].attrs["aria-current"]).toBe("page");
  });
});

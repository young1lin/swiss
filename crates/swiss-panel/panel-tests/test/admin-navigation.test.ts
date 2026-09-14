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

interface FakeEl {
  innerHTML: string;
  hidden: boolean;
  textContent: string;
  className: string;
  title: string;
  onclick: unknown;
  dataset: Record<string, string>;
  setAttribute: (k: string, v: string) => void;
}

function fakeEl(): FakeEl {
  return { innerHTML: "", hidden: false, textContent: "", className: "", title: "", onclick: null, dataset: {}, setAttribute: () => {} };
}

const els = new Map<string, FakeEl>();
let responder: (path: string) => Promise<{ status: number; ok: boolean; json: () => Promise<unknown> }>;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let registry: any; // page-registry.js and util.js are plain JS modules
let state: { view: string };

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
  state.view = view;
  responder = () => Promise.resolve(jsonResponse(status, body));
  await registry.reloadPluginInventory();
};

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    document: {
      getElementById: (id: string) => {
        let e = els.get(id);
        if (!e) { e = fakeEl(); els.set(id, e); }
        return e;
      },
      querySelector: () => null,
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
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  const util = await import("../../src/admin_assets/js/util.js");
  state = (util as { state: { view: string } }).state;
  registry = await import("../../src/admin_assets/js/page-registry.js");
});

const byId = (id: string): FakeEl => els.get(id) as FakeEl;

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
    expect(rail).toContain(">Gateway</span>");
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

  it("the count chip, the memory reading and the focus control live on the context bar, not the rail", () => {
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");
    const rail = shell.slice(shell.indexOf('class="rail"'), shell.indexOf('class="workbench"'));
    const ctx = shell.slice(shell.indexOf('class="ctxbar"'), shell.indexOf('class="shell"'));
    expect(rail).not.toContain("countChip");
    expect(ctx).toContain('id="countChip"');
    expect(ctx).toContain('id="memChip"');
    // Focus mode is shell-owned and lives at the bar's far right — never in the rail foot.
    expect(rail).not.toContain('id="expandBtn"');
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

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* Two-level navigation (docs/13, D5 revised by docs/18 V3): the primary bar shows one tab per
   plugin group, the page bar is always present — the group's pages, or a single page's name. There is no bundler
   and no browser in the test suite, so this suite drives the real page-registry module under a
   hand-rolled DOM: fake elements that record what paintNavigation writes, and a fetch stub that
   answers /api/plugins the way the plugin-aware gateway (full inventory) or an older gateway
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
}

function fakeEl(): FakeEl {
  return { innerHTML: "", hidden: false, textContent: "", className: "", title: "", onclick: null, dataset: {} };
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

/* Shaped like the Rust gateway's /api/plugins: pages[] carries the pluginId that grouping keys
   on, plugins[] carries the label the primary bar shows. Labels are the target shape (docs/13
   N4): the plugin is "MCP", its list page is "Servers". */
const inventory = {
  plugins: [
    { id: "mcp", label: "MCP", enabled: true, state: "running" },
    { id: "tunnels", label: "Tunnels", enabled: true, state: "running" },
    { id: "jobs", label: "Jobs", enabled: true, state: "running" },
  ],
  pages: [
    { id: "mcps", pluginId: "mcp", label: "Servers", order: 10, sidebar: true, path: "#mcps", entry: "/admin/js/views/mcps.js" },
    { id: "traffic", pluginId: "mcp", label: "Traffic", order: 20, path: "#traffic", entry: "/admin/js/views/traffic.js" },
    { id: "tokens", pluginId: "mcp", label: "Token", order: 30, path: "#tokens", entry: "/admin/js/views/tokens.js" },
    { id: "tunnels", pluginId: "tunnels", label: "Tunnels", order: 30, sidebar: true, path: "#tunnels", entry: "/admin/js/views/tunnels.js" },
    { id: "jobs", pluginId: "jobs", label: "Jobs", order: 50, path: "#jobs", entry: "/admin/js/views/jobs.js" },
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

describe("two-level navigation painting", () => {
  it("paints one primary tab per plugin group, carrying data-group and the default page's data-view", async () => {
    await paint("mcps", inventory);
    const seg = byId("viewSeg").innerHTML;
    // The first group: plugin label, default page as data-view, group selected.
    expect(seg).toContain('<button role="tab" data-group="mcp" data-view="mcps" aria-selected="true">MCP</button>');
    // Traffic is a page of the mcp group, not a level-one entry any more.
    expect(seg).not.toContain('data-view="traffic"');
    // Single-page groups stay level one, unselected.
    expect(seg).toContain('data-group="tunnels" data-view="tunnels" aria-selected="false">Tunnels</button>');
    expect(seg).toContain('data-group="jobs" data-view="jobs" aria-selected="false">Jobs</button>');
    // The synthesized management page has no inventory row: GROUP_LABELS names its group.
    expect(seg).toContain('data-group="host" data-view="plugins" aria-selected="false">Gateway</button>');
    // Both bars are click-delegated the same way.
    expect(typeof byId("viewSeg").onclick).toBe("function");
    expect(typeof byId("subSeg").onclick).toBe("function");
  });

  it("paints the current group's pages into the secondary bar, in page order", async () => {
    await paint("mcps", inventory);
    expect(byId("subBar").hidden).toBe(false);
    const sub = byId("subSeg").innerHTML;
    expect(sub).toContain('data-view="mcps" aria-selected="true">Servers</button>');
    expect(sub).toContain('data-view="traffic" aria-selected="false">Traffic</button>');
    expect(sub).toContain('data-view="tokens" aria-selected="false">Token</button>');
    // Page order decides inside the group: Servers (10), Traffic (20), Token (30).
    expect(sub.indexOf("Servers")).toBeLessThan(sub.indexOf("Traffic"));
    expect(sub.indexOf("Traffic")).toBeLessThan(sub.indexOf("Token"));
  });

  it("deep-linking #tokens selects the MCP group on level one and Token on level two", async () => {
    await paint("tokens", inventory);
    expect(byId("viewSeg").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-selected="true">MCP</button>');
    expect(byId("subSeg").innerHTML).toContain('data-view="tokens" aria-selected="true">Token</button>');
  });

  it("deep-linking #traffic selects the MCP group on level one and Traffic on level two", async () => {
    await paint("traffic", inventory);
    expect(byId("viewSeg").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-selected="true">MCP</button>');
    expect(byId("subSeg").innerHTML).toContain('data-view="traffic" aria-selected="true">Traffic</button>');
    expect(byId("subBar").hidden).toBe(false);
  });

  it("keeps the page bar for a single-page group: no tabs, the page's name (docs/18 V3, revising D5)", async () => {
    await paint("tunnels", inventory);
    expect(byId("subBar").hidden).toBe(false);
    expect(byId("subSeg").innerHTML).toBe("");
    expect(byId("subName").textContent).toBe("Tunnels");
    expect(byId("viewSeg").innerHTML).toContain('data-group="tunnels" data-view="tunnels" aria-selected="true">Tunnels</button>');
    // The emptied #subSeg must not PAINT: .seg's 3px padding over --sep-soft renders as a
    // 6px grey square beside the page name — exactly what a review of 19998 saw. The CSS owns
    // the fix (not subSeg.hidden) so every .seg that empties itself is covered.
    const views = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "views.css"), "utf8");
    expect(views).toContain(".seg:empty");
  });

  it("clears the page name when the group has real second-level tabs", async () => {
    await paint("mcps", inventory);
    expect(byId("subBar").hidden).toBe(false);
    expect(byId("subName").textContent).toBe("");
    expect(byId("subSeg").innerHTML).toContain(">Servers</button>");
  });

  it("the count chip lives in the page bar, not the toolbar (docs/18 V3)", () => {
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");
    const toolbar = shell.slice(shell.indexOf("<header"), shell.indexOf("</header>"));
    const subbar = shell.slice(shell.indexOf('id="subBar"'), shell.indexOf('<div class="shell">'));
    expect(toolbar).not.toContain("countChip");
    expect(subbar).toContain('id="countChip"');
  });

  it("groups the legacy manifest through GROUP_LABELS when an older gateway answers 404", async () => {
    await paint("mcps", null, 404);
    const seg = byId("viewSeg").innerHTML;
    // Group label from GROUP_LABELS ("MCP"), page labels from the manifest ("MCPs" stays on the page tab).
    expect(seg).toContain('data-group="mcp" data-view="mcps" aria-selected="true">MCP</button>');
    expect(seg).not.toContain('data-view="traffic"');
    expect(byId("subBar").hidden).toBe(false);
    expect(byId("subSeg").innerHTML).toContain('data-view="mcps" aria-selected="true">MCPs</button>');
    // The legacy manifest carries the Token page too, so an older gateway gets the same tab.
    expect(byId("subSeg").innerHTML).toContain('data-view="tokens" aria-selected="false">Token</button>');
  });

  it("marks the primary tab · off only when every page of the group is unavailable", async () => {
    const disabled: typeof inventory = {
      ...inventory,
      plugins: inventory.plugins.map((p) => (p.id === "mcp" ? { ...p, enabled: false } : p)),
    };
    await paint("mcps", disabled);
    expect(byId("viewSeg").innerHTML).toContain('data-group="mcp" data-view="mcps" aria-selected="true">MCP · off</button>');
    // The page tabs keep their own marker and the title hint.
    const sub = byId("subSeg").innerHTML;
    expect(sub).toContain('title="Plugin disabled"');
    expect(sub).toContain('data-view="mcps" aria-selected="true" title="Plugin disabled">Servers · off</button>');
    expect(sub).toContain('data-view="traffic" aria-selected="false" title="Plugin disabled">Traffic · off</button>');
    // Other groups are untouched.
    expect(byId("viewSeg").innerHTML).toContain(">Tunnels</button>");
  });
});

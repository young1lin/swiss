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

// @vitest-environment happy-dom

/* The Plugins page (the host's own management view): name + one grey line + a switch per row,
   a dot only when the state disagrees with the switch (docs/46 §3.5), a start-at-sign-in row,
   and a poll that patches state in place without ever rebuilding structure.

   REWRITTEN FOR R5 (docs/37 §7), and the rewrite is the point. The builders used to return
   HTML strings, so every assertion here was a substring test against markup nobody parsed.
   Now they return nodes, so the suite runs on a real DOM (happy-dom, per file) and asks what
   it always meant: does the switch carry role and state, does the grey line name the pages,
   does the dot's title follow the host's vocabulary. The poll cases are new with the real
   DOM - the in-place patch is this page's own regression, and only a real tree can pin node
   identity across it. */

import { describe, expect, it, beforeAll, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { fill } from "../src/h.js";
import type { ApiPluginRow } from "../src/types/api.js";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view's public surface is
   asserted by name below (the pure builders), so a typed import would be circular. */
let view: any;
let inventory: unknown = { plugins: [], revision: 0 };
let autostartReply: unknown = null;
/* Runs inside the enable/disable POST, before it answers: a case uses it to do what the browser
 * does in that window (blur the switch it just disabled) and to move the inventory on. */
let duringToggle: (() => void) | null = null;

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;

/* The shell skeleton, taken from the SERVED index.html rather than invented here (the same
 * rule as the tokens suite): the page-registry graph paints the rail on inventory reload, so
 * the ids must exist before the import - and reading the real file means a renamed shell
 * control fails here instead of drifting. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

/* Detached probe: render a builder's children into a real span and hand it back, so a case
 * can assert text content and structure instead of substring-matching a string. */
function rendered(child: unknown): HTMLElement {
  const host = document.createElement("span");
  fill(host, child as never);
  return host;
}

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      const u = String(url);
      if (u === "/api/plugins") return Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve(inventory) } as Response);
      if (init?.method === "POST" && /^\/api\/plugins\/[^/]+\/(enable|disable)$/.test(u)) {
        if (duringToggle) duringToggle();
        return Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve({ plugin: {} }) } as Response);
      }
      if (u === "/api/autostart") {
        return Promise.resolve({ status: autostartReply == null ? 404 : 200, ok: autostartReply != null, json: () => Promise.resolve(autostartReply) } as Response);
      }
      return Promise.resolve({ status: 404, ok: false, json: () => Promise.resolve({ error: "no stub for " + u }) } as Response);
    },
  });
  document.body.innerHTML = shellSkeleton(); // before the import: the graph wires on evaluation
  view = await import("../src/views/plugins.js");
});

beforeEach(() => {
  inventory = { plugins: [], revision: 0, pages: [] };
  autostartReply = null;
  duringToggle = null;
  document.body.innerHTML = shellSkeleton();
  if (view) view.unmount(); // the real lifecycle between pages resets the module state
});

/** The dependency badge is the panel's half of the W3 contract (docs/12): the inventory
 *  states a plugin's required capabilities with a met/unmet verdict, and the row must SAY
 *  when the floor is missing — "needs connection-catalog (no provider)" — so disabling the
 *  provider reads as a consequence, not as a mysteriously broken Data view. A MET
 *  requirement is named too ("requires connection-catalog"), so the build's dependency
 *  structure is visible before anything breaks. */
describe("plugins view dependency badge", () => {
  it("names the capability when the requirement is unmet", () => {
    const badge = rendered(view.requiresBadge({ id: "data", requires: ["connection-catalog"], requiresMet: false } as ApiPluginRow));
    // docs/46 §3.5: the missing floor is a warn TAG (a state mark, amber), and the words carry
    // no separator of their own - the sub-line puts the " · " between its parts.
    expect(badge.textContent).toBe("needs connection-catalog no provider");
    expect(badge.querySelector(".tag.warn")?.textContent).toBe("no provider");
  });

  it("names the requirement while it is met — the dependency stays visible, not only the break", () => {
    expect(view.requiresBadge({ id: "data", requires: ["connection-catalog"], requiresMet: true } as ApiPluginRow))
      .toBe("requires connection-catalog");
    // An older inventory row that predates the verdict key reads as met-but-named.
    expect(view.requiresBadge({ id: "data", requires: ["connection-catalog"] } as ApiPluginRow))
      .toBe("requires connection-catalog");
  });

  it("has nothing to say for plugins without requirements", () => {
    // Plugins that need nothing carry no requires keys at all — absent, not null.
    expect(view.requiresBadge({ id: "mcp" } as ApiPluginRow)).toBeNull();
    expect(view.requiresBadge({ id: "jobs", requires: [], requiresMet: true } as unknown as ApiPluginRow)).toBeNull();
  });

  it("a capability name is text, never markup (docs/37 R5)", () => {
    const badge = rendered(view.requiresBadge({ id: "x", requires: ["a<b", "c&d"], requiresMet: false } as ApiPluginRow));
    expect(badge.textContent).toBe("needs a<b, c&d no provider");
    expect(badge.querySelector("b")).toBeNull();
  });
});

/* Visual refresh V4 (docs/18), on the library row since docs/46 P5: name + one grey line, the
   toggle is a switch, and the version moves into the row title. The switch says on or off, so
   the dot is left for what the switch cannot say - work in flight and a failed start. */
describe("the plugins row", () => {
  it("toggles with a switch, not a Disable/Enable button", () => {
    const row = view.rowNode({ id: "data", label: "Data", enabled: true, state: "active", pages: ["mcps", "traffic"], version: "0.1" } as ApiPluginRow);
    const sw = row.querySelector("[data-toggle]");
    expect(sw?.getAttribute("role")).toBe("switch");
    expect(sw?.getAttribute("aria-checked")).toBe("true");
    expect(sw?.textContent).toBe("");
  });

  it("the sub-line is the id in mono, then how many pages - the list itself on hover", () => {
    const row = view.rowNode({ id: "mcp", label: "MCP", enabled: true, state: "active", pages: ["mcps", "traffic", "tokens"], version: "0.1", requires: ["connection-catalog"], requiresMet: true } as ApiPluginRow);
    expect(row.classList.contains("lrow")).toBe(true);
    expect(row.querySelector(".lrow-sub code")?.textContent).toBe("mcp");
    expect(row.querySelector(".lrow-sub")?.textContent).toBe("mcp · 3 pages · requires connection-catalog");
    expect(row.querySelector('.lrow-sub [title="mcps, traffic, tokens"]')?.textContent).toBe("3 pages");
    expect(row.getAttribute("title")).toBe("v0.1");
    expect(row.textContent).not.toContain("v0.1");
    expect(row.textContent).not.toContain("active");
  });

  it("one page is singular, and a pageless plugin says so", () => {
    expect(view.rowNode({ id: "data", label: "Data", enabled: true, state: "active", pages: ["data"] } as ApiPluginRow).querySelector(".lrow-sub")?.textContent).toBe("data · 1 page");
    expect(view.rowNode({ id: "host", label: "Settings", enabled: true, state: "active" } as ApiPluginRow).querySelector(".lrow-sub")?.textContent).toBe("host · no page");
  });

  it("no dot while the state agrees with the switch - on and serving, on and lazily idle, off", () => {
    const dotOf = (p: ApiPluginRow): Element | null => view.rowNode(p).querySelector(".dot");
    expect(dotOf({ id: "data", label: "Data", enabled: true, state: "active" } as ApiPluginRow)).toBeNull();
    expect(dotOf({ id: "jobs", label: "Jobs", enabled: true, state: "idle" } as ApiPluginRow)).toBeNull();
    expect(dotOf({ id: "mcp", label: "MCP", enabled: false, state: "disabled" } as ApiPluginRow)).toBeNull();
    // ...and the off row has no "off" word either: the switch beside it already says it.
    expect(view.rowNode({ id: "mcp", label: "MCP", enabled: false, state: "disabled" } as ApiPluginRow).textContent).toBe("MCPmcp · no page");
  });

  it("a failed start is a red dot after the name and the reason as the row's red line", () => {
    const row = view.rowNode({ id: "x", label: "X", enabled: true, state: "failed", lastError: "port 9000 is taken", pages: ["x"] } as ApiPluginRow);
    const d = row.querySelector(".lrow-name .dot") as HTMLElement;
    expect(d.className).toBe("dot error");
    expect(d.getAttribute("title")).toBe("failed"); // the host's own word (docs/18 V6)
    expect(row.querySelector(".lrow-err")?.textContent).toBe("port 9000 is taken");
    expect(row.querySelector(".lrow-err")?.getAttribute("title")).toBe("port 9000 is taken");
    expect(row.querySelector(".lrow-sub")).toBeNull(); // the red line takes the sub-line's place
  });

  it("work in flight is the amber pulse, titled with the host's word", () => {
    const d = view.rowNode({ id: "x", label: "X", enabled: true, state: "stopping" } as ApiPluginRow).querySelector(".lrow-name .dot") as HTMLElement;
    expect([d.className, d.getAttribute("title")]).toEqual(["dot stopping", "stopping"]);
    const s = view.rowNode({ id: "x", label: "X", enabled: true, state: "starting" } as ApiPluginRow).querySelector(".lrow-name .dot") as HTMLElement;
    expect([s.className, s.getAttribute("title")]).toEqual(["dot starting", "starting"]);
  });

  it("a hostile label is text in the name and the switch's aria-label (docs/37 R5)", () => {
    const hostile = '<b>bold</b><img src=x onerror=1>';
    const row = view.rowNode({ id: "x", label: hostile, enabled: true, state: "active" } as ApiPluginRow);
    expect(row.querySelector(".lrow-name")?.textContent).toContain(hostile);
    expect(row.querySelector(".lrow-name b")).toBeNull();
    expect(row.querySelector("[data-toggle]")?.getAttribute("aria-label")).toBe("Toggle " + hostile);
  });
});

/* Start-at-sign-in (the Plugins page's Startup section): the OS-level host setting rendered
   with the same row - name + one grey line + the panel's switch. */
describe("plugins view start-at-sign-in row", () => {
  it("switches with the panel's switch and reflects the OS answer", () => {
    const row = view.startupRowNode({
      enabled: true,
      detail: "registry: HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
      command: '"C:/Program Files/swiss/swiss.exe" start --no-open',
    });
    expect(row).not.toBeNull();
    const sw = row.querySelector("[data-autostart-toggle]");
    expect(sw?.getAttribute("role")).toBe("switch");
    expect(sw?.getAttribute("aria-checked")).toBe("true");
    expect(row.textContent).toContain("registry: HKCU");
    // The title is the raw command, held as an attribute value - nothing to escape.
    expect(row.getAttribute("title")).toBe('"C:/Program Files/swiss/swiss.exe" start --no-open');
    expect(row.textContent).not.toContain("· off");
  });

  it("an off row is the switch checked off - no dot and no \"off\" word to repeat it", () => {
    const row = view.startupRowNode({ enabled: false, detail: "launch agent: /Users/a/Library/LaunchAgents/dev.swiss.swiss.plist", command: "c" });
    expect(row?.querySelector("[data-autostart-toggle]")?.getAttribute("aria-checked")).toBe("false");
    expect(row?.querySelector(".dot")).toBeNull();
    expect(row?.textContent).toBe("Start swiss when you sign inlaunch agent: /Users/a/Library/LaunchAgents/dev.swiss.swiss.plist");
  });

  it("renders nothing without an answer — an old gateway never grows a dead control", () => {
    expect(view.startupRowNode(null)).toBeNull();
  });
});

/* The page itself, through the real load path: mount reads /api/plugins and /api/autostart
 * under the fetch stub, and the poll patches state IN PLACE - the row nodes and their switches
 * keep their identity while the words, the dot and the revision follow the inventory. */
describe("the Plugins page through mount", () => {
  it("paints rows, the Startup section and a foot that is the revision alone", async () => {
    inventory = { plugins: [
      { id: "mcp", label: "MCP", enabled: true, state: "active", pages: ["mcps", "traffic", "tokens"] },
      { id: "data", label: "Data", enabled: true, state: "active", pages: ["data"], requires: ["connection-catalog"] },
    ], revision: 7, pages: [] };
    autostartReply = { enabled: false, detail: "registry: HKCU\\...\\Run", command: "c" };
    await view.mount();
    expect($("pane").querySelectorAll("[data-plugin]").length).toBe(2);
    expect($("pane").querySelector("[data-autostart]")).not.toBeNull();
    expect($("pane").textContent).toContain("data · 1 page · requires connection-catalog");
    expect(Array.from($("pane").querySelectorAll(".sec-cap")).map((c) => c.textContent)).toEqual(["Startup", "Installed"]);
    // The count is the context bar's chip (rule 25); the foot keeps what nothing else says.
    expect($("pane").querySelector(".page-foot")?.textContent).toBe("revision 7");
    expect(view.countText()).toBe("2 plugins · 2 on");
  });

  it("an old gateway without /api/autostart grows no Startup section", async () => {
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "active" }], revision: 1, pages: [] };
    autostartReply = null; // the route 404s
    await view.mount();
    expect($("pane").querySelector("[data-autostart]")).toBeNull();
  });

  it("poll patches state in place - the row and its switch are the SAME nodes, the words follow", async () => {
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "active" }], revision: 1, pages: [] };
    await view.mount();
    const row = $("pane").querySelector("[data-plugin]") as HTMLElement;
    const toggle = row.querySelector("[data-toggle]") as HTMLButtonElement;
    toggle.focus();
    expect(row.querySelector(".dot")).toBeNull();

    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "failed", lastError: "port taken" }], revision: 2, pages: [] };
    await view.poll();

    expect($("pane").querySelector("[data-plugin]")).toBe(row); // patched, not rebuilt
    expect(row.querySelector(".lrow-name .dot")?.className).toBe("dot error");
    expect(row.querySelector(".lrow-name .dot")?.getAttribute("title")).toBe("failed");
    expect(row.querySelector(".lrow-err")?.textContent).toBe("port taken");
    expect($("pane").querySelector(".page-foot-rev")?.textContent).toBe("revision 2");
    expect(view.countText()).toBe("1 plugin · 1 on · 1 failed");
    // The switch is the node a hand just pressed: a poll must not take the focus off it.
    expect(row.querySelector("[data-toggle]")).toBe(toggle);
    expect(document.activeElement).toBe(toggle);

    // Switched off elsewhere: the same switch moves to unchecked, and the dot goes away.
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: false, state: "disabled" }], revision: 3, pages: [] };
    await view.poll();
    expect(row.querySelector("[data-toggle]")).toBe(toggle);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(row.querySelector(".dot")).toBeNull();
    expect(row.querySelector(".lrow-sub")?.textContent).toBe("mcp · no page");
  });

  it("a pressed switch gets its focus back once the write answers", async () => {
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "active" }], revision: 1, pages: [] };
    await view.mount();
    const toggle = $("pane").querySelector("[data-toggle]") as HTMLButtonElement;
    toggle.focus();
    // The switch is disabled while its write is out (no double send), and a real browser
    // blurs a focused control the moment it is disabled - happy-dom does not, so the case
    // does it by hand, inside the request, where Chrome does it. Found on the docs/46 P5 walk:
    // after a click (or Space) the focus was on <body>, and the next Tab started over.
    duringToggle = (): void => {
      expect(toggle.disabled).toBe(true);
      toggle.disabled = false; toggle.blur(); toggle.disabled = true; // happy-dom ignores blur() on a disabled control
      expect(document.activeElement).not.toBe(toggle);
      inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: false, state: "disabled" }], revision: 2, pages: [] };
    };
    toggle.click();
    for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
    expect(toggle.disabled).toBe(false);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(document.activeElement).toBe(toggle);
  });

  it("a structural change - a plugin appearing - re-renders the list", async () => {
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "active" }], revision: 1, pages: [] };
    await view.mount();
    inventory = { plugins: [
      { id: "mcp", label: "MCP", enabled: true, state: "active" },
      { id: "tunnels", label: "Tunnels", enabled: false, state: "idle" },
    ], revision: 2, pages: [] };
    await view.poll();
    const rows = $("pane").querySelectorAll("[data-plugin]");
    expect(rows.length).toBe(2);
    expect(rows[1].querySelector("[data-toggle]")?.getAttribute("aria-checked")).toBe("false");
  });

  it("an empty inventory draws the empty state, not a bare list", async () => {
    inventory = { plugins: [], revision: 0, pages: [] };
    await view.mount();
    expect($("pane").querySelector(".empty h2")?.textContent).toBe("No plugins");
    expect($("pane").querySelector("[data-plugin]")).toBeNull();
    expect($("pane").querySelector(".page-foot-rev")?.textContent).toBe("revision 0");
    expect(view.countText()).toBe("0 plugins · 0 on");
  });

  // The pane frame moved into the library with pane() (docs/46 P1b-3): ui.css owns it now.
  it("the pane stops centring its content", () => {
    const css = readFileSync(join(here, "../..", "src", "admin_assets", "styles", "ui.css"), "utf8");
    expect(css).toMatch(/\.pane > \* \{[^}]*margin-inline: 0;/);
  });
});

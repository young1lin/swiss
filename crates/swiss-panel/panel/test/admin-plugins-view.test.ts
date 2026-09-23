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

/* The Plugins page (the host's own management view): dot + name + one grey line + a switch
   per row, a start-at-sign-in row, and a poll that patches state in place without ever
   rebuilding structure.

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
    fetch: (url: string): Promise<Response> => {
      const u = String(url);
      if (u === "/api/plugins") return Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve(inventory) } as Response);
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
    expect(badge.textContent).toBe("· needs connection-catalog (no provider)");
    expect(badge.querySelector(".warn")?.textContent).toBe("(no provider)");
  });

  it("names the requirement while it is met — the dependency stays visible, not only the break", () => {
    expect(view.requiresBadge({ id: "data", requires: ["connection-catalog"], requiresMet: true } as ApiPluginRow))
      .toBe("· requires connection-catalog");
    // An older inventory row that predates the verdict key reads as met-but-named.
    expect(view.requiresBadge({ id: "data", requires: ["connection-catalog"] } as ApiPluginRow))
      .toBe("· requires connection-catalog");
  });

  it("has nothing to say for plugins without requirements", () => {
    // Plugins that need nothing carry no requires keys at all — absent, not null.
    expect(view.requiresBadge({ id: "mcp" } as ApiPluginRow)).toBeNull();
    expect(view.requiresBadge({ id: "jobs", requires: [], requiresMet: true } as unknown as ApiPluginRow)).toBeNull();
  });

  it("a capability name is text, never markup (docs/37 R5)", () => {
    const badge = rendered(view.requiresBadge({ id: "x", requires: ["a<b", "c&d"], requiresMet: false } as ApiPluginRow));
    expect(badge.textContent).toBe("· needs a<b, c&d (no provider)");
    expect(badge.querySelector("b")).toBeNull();
  });
});

/* Visual refresh V4 (docs/18): the plugins row is dot + name + one grey line, the toggle is
   a switch, and the version moves into the row title — the row says what it is, the dot says
   what it is doing. */
describe("visual refresh V4 — the plugins row", () => {
  it("toggles with a switch, not a Disable/Enable button", () => {
    const row = view.rowNode({ id: "data", label: "Data", enabled: true, state: "active", pages: ["mcps", "traffic"], version: "0.1" } as ApiPluginRow);
    const sw = row.querySelector("[data-toggle]");
    expect(sw?.getAttribute("role")).toBe("switch");
    expect(sw?.getAttribute("aria-checked")).toBe("true");
    expect(sw?.textContent).toBe("");
  });

  it("keeps id and pages on the grey line; version rides the title, state is the dot", () => {
    const row = view.rowNode({ id: "data", label: "Data", enabled: true, state: "active", pages: ["mcps", "traffic"], version: "0.1" } as ApiPluginRow);
    expect(row.querySelector("code")?.textContent).toBe("data");
    expect(row.querySelector(".tun-sub")?.textContent).toContain("pages: mcps, traffic");
    expect(row.getAttribute("title")).toBe("v0.1");
    expect(row.textContent).not.toContain("v0.1");
    expect(row.textContent).not.toContain("· active");
  });

  it("a pageless plugin says so", () => {
    expect(view.rowNode({ id: "host", label: "Settings", enabled: true, state: "active" } as ApiPluginRow).textContent).toContain("· no page");
  });

  it("the dot carries the host's own state word as its title (docs/18 V6)", () => {
    const dotOf = (p: ApiPluginRow): HTMLElement => view.rowNode(p).querySelector("[data-dot]");
    expect(dotOf({ id: "data", label: "Data", enabled: true, state: "active" } as ApiPluginRow).getAttribute("title")).toBe("active");
    expect(dotOf({ id: "mcp", label: "MCP", enabled: false, state: "active" } as ApiPluginRow).getAttribute("title")).toBe("disabled");
    expect(dotOf({ id: "x", label: "X", enabled: true, state: "failed" } as ApiPluginRow).getAttribute("title")).toBe("failed");
    expect(dotOf({ id: "x", label: "X", enabled: true, state: "failed" } as ApiPluginRow).className).toBe("dot down");
  });

  it("a hostile label is text in the name and the switch's aria-label (docs/37 R5)", () => {
    const hostile = '<b>bold</b><img src=x onerror=1>';
    const row = view.rowNode({ id: "x", label: hostile, enabled: true, state: "active" } as ApiPluginRow);
    expect(row.querySelector(".tun-name")?.textContent).toContain(hostile);
    expect(row.querySelector(".tun-name b")).toBeNull();
    expect(row.querySelector("[data-toggle]")?.getAttribute("aria-label")).toBe("Toggle " + hostile);
  });
});

/* Start-at-sign-in (the Plugins page's Startup section): the OS-level host setting rendered
   with the same row vocabulary — dot + name + one grey line + the panel's switch. */
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

  it("an off row says so on the name line and checks off", () => {
    const row = view.startupRowNode({ enabled: false, detail: "launch agent: /Users/a/Library/LaunchAgents/dev.swiss.swiss.plist", command: "c" });
    expect(row?.textContent).toContain("· off");
    expect(row?.querySelector("[data-autostart-toggle]")?.getAttribute("aria-checked")).toBe("false");
    expect(row?.textContent).toContain("launch agent:");
  });

  it("renders nothing without an answer — an old gateway never grows a dead control", () => {
    expect(view.startupRowNode(null)).toBeNull();
  });
});

/* The page itself, through the real load path: mount reads /api/plugins and /api/autostart
 * under the fetch stub, and the poll patches state IN PLACE - the row nodes keep their
 * identity while their dots, error tails and the foot follow the inventory. */
describe("the Plugins page through mount", () => {
  it("paints rows, the Startup section and the foot with its revision", async () => {
    inventory = { plugins: [
      { id: "mcp", label: "MCP", enabled: true, state: "active", pages: ["mcps", "traffic", "tokens"] },
      { id: "data", label: "Data", enabled: true, state: "active", pages: ["data"], requires: ["connection-catalog"] },
    ], revision: 7, pages: [] };
    autostartReply = { enabled: false, detail: "registry: HKCU\\...\\Run", command: "c" };
    await view.mount();
    expect($("pane").querySelectorAll("[data-plugin]").length).toBe(2);
    expect($("pane").querySelector("[data-autostart]")).not.toBeNull();
    expect($("pane").textContent).toContain("· requires connection-catalog");
    expect($("pane").querySelector("[data-foot-text]")?.textContent).toBe("2 plugins · 2 on");
    expect($("pane").textContent).toContain("revision 7");
  });

  it("an old gateway without /api/autostart grows no Startup section", async () => {
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "active" }], revision: 1, pages: [] };
    autostartReply = null; // the route 404s
    await view.mount();
    expect($("pane").querySelector("[data-autostart]")).toBeNull();
  });

  it("poll patches state in place - the row is the SAME node, its dot and tail follow", async () => {
    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "active" }], revision: 1, pages: [] };
    await view.mount();
    const row = $("pane").querySelector("[data-plugin]") as HTMLElement;
    const dot = row.querySelector("[data-dot]") as HTMLElement;
    const err = row.querySelector("[data-err]") as HTMLElement;

    inventory = { plugins: [{ id: "mcp", label: "MCP", enabled: true, state: "failed", lastError: "port taken" }], revision: 2, pages: [] };
    await view.poll();

    expect($("pane").querySelector("[data-plugin]")).toBe(row); // patched, not rebuilt
    expect(row.querySelector("[data-dot]")?.className).toBe("dot down");
    expect(row.querySelector("[data-dot]")?.getAttribute("title")).toBe("failed");
    expect(row.querySelector("[data-err]")?.textContent).toContain("· port taken");
    expect($("pane").querySelector("[data-foot-text]")?.textContent).toBe("1 plugin · 1 on · 1 failed");
    // The dot and err nodes are the same ones too - patch, not replace.
    expect(row.querySelector("[data-dot]")).toBe(dot);
    expect(row.querySelector("[data-err]")).toBe(err);
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
    expect(rows[1].textContent).toContain("· off");
  });

  it("an empty inventory draws the empty state, not a bare list", async () => {
    inventory = { plugins: [], revision: 0, pages: [] };
    await view.mount();
    expect($("pane").querySelector(".empty h2")?.textContent).toBe("No plugins");
    expect($("pane").querySelector("[data-plugin]")).toBeNull();
    expect($("pane").querySelector("[data-foot-text]")?.textContent).toBe("0 plugins · 0 on");
  });

  // The pane frame moved into the library with pane() (docs/46 P1b-3): ui.css owns it now.
  it("the pane stops centring its content", () => {
    const css = readFileSync(join(here, "../..", "src", "admin_assets", "styles", "ui.css"), "utf8");
    expect(css).toMatch(/\.pane > \* \{[^}]*margin-inline: 0;/);
  });
});

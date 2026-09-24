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

import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { mcpDetail, mcpRows, resetMcpState, setMcpDetail, setMcpGroups, setMcpRows, setSelectedMcp } from "../src/mcp-state.js";
import { closeMenu } from "../src/ui/menu.js";

/* def revisions (docs/28 D1): the config tab's Replace flow and the rollback shelf. The
   rendering assertions go through the real renderPane markup; the confirm-gated restore is
   driven through a fetch queue — cancelled confirm must not fire a request at all.
   docs/46 P7-3: on happy-dom. This suite carried a micro-DOM whose innerHTML serialised its
   own children; the real DOM reads the built tree back the same way, and the menus are the
   #menu a person sees. */

const here = dirname(fileURLToPath(import.meta.url));
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

const confirmMock = vi.fn();
const calls: Array<{ url: string; init?: RequestInit }> = [];

let pane: typeof import("../src/pane.js");
let detail: typeof import("../src/detail.js");
let sidebar: typeof import("../src/sidebar.js");

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  Object.assign(globalThis, {
    confirm: confirmMock,
    fetch: vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return { ok: true, status: 200, json: async () => ({ revisions: [], parked: true }) } as never;
    }),
  });
  pane = await import("../src/pane.js");
  detail = await import("../src/detail.js");
  sidebar = await import("../src/sidebar.js");
});

function freshState(overrides: { mcps?: unknown[]; groups?: string[]; selected?: string | null; detail?: unknown } = {}) {
  resetMcpState();
  setMcpGroups(overrides.groups ?? ["default"]);
  if (overrides.mcps) setMcpRows(overrides.mcps as never);
  if (overrides.selected !== undefined) setSelectedMcp(overrides.selected);
  if (overrides.detail !== undefined) setMcpDetail(overrides.detail as never);
  closeMenu();
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
const paneHtml = (): string => document.getElementById("pane")!.innerHTML;

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  freshState();
  calls.length = 0;
  confirmMock.mockReset();
});

describe("config tab: the replace flow and the rollback shelf (docs/28 D1)", () => {
  it("renders Replace definition… beside Edit, and the parked revisions under the config rows", () => {
    const d = fakeDetail("m", { type: "echo" });
    (d as unknown as Record<string, unknown>).revisions = [
      { index: 0, at: 1789467260822, note: "proc version", type: "proc" },
    ];
    freshState({ mcps: [ROW] as never, detail: d });
    pane.renderPane();
    const root = document.getElementById("pane")!;
    expect(root.querySelector("#c-replace")!.textContent).toContain("Replace definition");
    expect(root.textContent).toContain("Saved revisions (1)");
    expect(root.textContent).toContain("proc version");
    expect(root.querySelector('[data-restore="0"]')).not.toBeNull();
    expect(root.querySelector('[data-revdel="0"]')).not.toBeNull();
  });

  it("an empty shelf says so instead of nothing", () => {
    const d = fakeDetail("m", { type: "echo" });
    (d as unknown as Record<string, unknown>).revisions = [];
    freshState({ mcps: [ROW] as never, detail: d });
    pane.renderPane();
    expect(paneHtml()).toContain("None yet");
  });

  it("startReplace opens the same form with a note field and the Replace verb", () => {
    const d = fakeDetail("m", { type: "echo" });
    freshState({ mcps: [ROW] as never, detail: d });
    detail.startReplace();
    expect((mcpDetail() as unknown as Record<string, unknown>).editMode).toBe("replace");
    const root = document.getElementById("pane")!;
    expect(root.querySelector("#e-note")).not.toBeNull();
    expect(Array.from(root.querySelectorAll("button")).map((b) => b.textContent)).toContain("Replace definition");
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
  it("the detail header's primary button says Disable on a started MCP and Enable on a stopped one", () => {
    freshState({ mcps: [{ ...ROW }] as never, detail: fakeDetail("m", { type: "echo" }) });
    pane.renderPane();
    const primary = (): HTMLButtonElement => document.getElementById("primaryBtn") as HTMLButtonElement;
    pane.patchDetailHead();
    expect(primary().textContent).toBe("Disable");
    (mcpRows()[0] as unknown as Record<string, unknown>).lifecycle = "stopped";
    pane.patchDetailHead();
    expect(primary().textContent).toBe("Enable");
  });

  it("the header subtitle and the overflow menu use the operator's word", () => {
    const stopped = { ...ROW, lifecycle: "stopped", state: "stopped" };
    expect(pane.headSubtitle(stopped as never)).toContain("disabled");
    // The menu is the library's (anchoredMenu, docs/46 P2-1): its rows are items, and the verb
    // row's label is the operator's word for the row it was opened on.
    const labels = (row: unknown): string[] => pane.menuItems(fakeDetail("m", { type: "echo" }) as never, row as never)
      .map((it) => ("label" in it ? it.label : "")).filter(Boolean);
    expect(labels(stopped)).toContain("Enable");
    expect(labels(stopped)).not.toContain("Disable");
    expect(labels({ ...ROW })).toContain("Disable");
    expect(labels({ ...ROW })).not.toContain("Enable");
  });
});

describe("docs/28 D3: the row's right-click menu", () => {
  const item = (label: string): HTMLButtonElement =>
    Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find((b) => b.textContent === label)!;

  it("a started row offers Rename, Disable, Delete — and Disable fires the stop verb", async () => {
    freshState({ mcps: [{ ...ROW }] as never });
    sidebar.rowMenu("m", { left: 10, top: 10, bottom: 10 });
    const menu = document.getElementById("menu")!;
    expect(Array.from(menu.querySelectorAll("button")).map((b) => b.textContent)).toEqual(["Rename…", "Disable", "Delete"]);
    item("Disable").click();
    await new Promise((r) => { setTimeout(r, 0); });
    expect(document.getElementById("menu"), "the menu closed behind its verb").toBeNull();
    expect(calls[0].url).toContain("/api/mcps/m/stop");
    expect((calls[0].init as { method?: string }).method).toBe("POST");
  });

  it("a stopped row offers Enable, and it fires the start verb", async () => {
    freshState({ mcps: [{ ...ROW, lifecycle: "stopped" }] as never });
    sidebar.rowMenu("m", { left: 10, top: 10, bottom: 10 });
    item("Enable").click();
    await new Promise((r) => { setTimeout(r, 0); });
    expect(calls[0].url).toContain("/api/mcps/m/start");
  });
});

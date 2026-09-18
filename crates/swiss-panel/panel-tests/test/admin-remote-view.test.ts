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
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* The Remote Targets page (docs/34 R6 + R8): a grouped table of alias rows plus an
   Add/Edit sheet. The suite pins the view module contract under the same hand-rolled
   DOM the Token page suite uses — the groups component draws real nodes, so the fake
   element grows appendChild/dataset and the row assertions read the GROUP CARDS'
   markup (the recursive walk below) instead of the pane's. The page must also carry
   the docs/20 family wiring: the group names from the same response become headers,
   the sheet's Group select rides the POST body, and the New-group button exists. */

interface FakeEl {
  innerHTML: string;
  textContent: string;
  hidden: boolean;
  onclick: unknown;
  className: string;
  title: string;
  type: string;
  value: string;
  checked: boolean;
  disabled: boolean;
  draggable: boolean;
  style: Record<string, string>;
  dataset: Record<string, string>;
  children: FakeEl[];
  classList: { add(): void; remove(): void; contains(): boolean };
  querySelector: (sel: string) => FakeEl | null;
  querySelectorAll: () => FakeEl[];
  appendChild: (n: FakeEl) => FakeEl;
  addEventListener(): void;
  setAttribute(): void;
  focus(): void;
  getBoundingClientRect(): { top: number; left: number; right: number; bottom: number; width: number; height: number };
  closest(): FakeEl | null;
}

function fakeEl(): FakeEl {
  const e = {
    innerHTML: "",
    textContent: "",
    hidden: false,
    onclick: null,
    className: "",
    title: "",
    type: "",
    value: "",
    checked: false,
    disabled: false,
    draggable: false,
    style: {},
    dataset: {},
    children: [] as FakeEl[],
  } as FakeEl;
  e.classList = { add() {}, remove() {}, contains: () => false };
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
  e.appendChild = (n: FakeEl) => { e.children.push(n); return n; };
  e.addEventListener = () => {};
  e.setAttribute = () => {};
  e.focus = () => {};
  e.getBoundingClientRect = () => ({ top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 });
  e.closest = () => null;
  return e;
}

const els = new Map<string, FakeEl>();
const groupsEl = fakeEl();
// The region repaints by id, not query, so the tracked element IS the region.
els.set("rmGroups", groupsEl);
const paneEl: FakeEl = { ...fakeEl(), querySelector: () => null };
els.set("pane", paneEl);

let responder: (path: string) => Promise<{ status: number; ok: boolean; json: () => Promise<unknown> }>;
let bodyByPath: Record<string, unknown> = {};
let lastPost: { path: string; init: { method?: string; body?: string } } | null = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let view: any;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    document: {
      getElementById: (id: string) => {
        if (id === "rmGroups") return groupsEl;
        let e = els.get(id);
        if (!e) { e = fakeEl(); els.set(id, e); }
        return e;
      },
      createElement: () => fakeEl(),
      querySelector: () => null,
      querySelectorAll: () => [],
      addEventListener() {},
      removeEventListener() {},
      documentElement: fakeEl(),
      body: fakeEl(),
      head: fakeEl(),
      visibilityState: "visible",
      activeElement: null,
    },
    fetch: (path: unknown, init: unknown) => {
      const p = String(path);
      if (init && (init as { method?: string }).method) {
        lastPost = { path: p, init: init as { method?: string; body?: string } };
      }
      return responder(p);
    },
    // The groups component reads window.CSS for CSS.escape and window.localStorage for
    // fold state; both are guarded, but the global must exist.
    window: { innerWidth: 1440, innerHeight: 900, addEventListener() {}, localStorage: { getItem: () => null, setItem() {}, removeItem() {} } },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    confirm: () => true,
    location: { origin: "http://127.0.0.1:19998" },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  responder = (p: string) => Promise.resolve({ status: 200, ok: true, json: async () => bodyByPath[p] ?? {} });
  view = await import("../../src/admin_assets/js/views/remote.js");
});

const byId = (id: string): FakeEl => {
  let e = els.get(id);
  if (!e) { e = fakeEl(); els.set(id, e); }
  return e;
};

/** Everything the groups region drew, flattened: markup plus every node below it — the
 *  fake DOM keeps children as elements, not markup, so the walk is recursive. Group
 *  headers set textContent (not innerHTML), so both join the walk. */
function flatten(e: FakeEl): string {
  return e.innerHTML + e.textContent + e.children.map(flatten).join("");
}
function drawn(): string {
  return flatten(groupsEl);
}

/* Fire the pane delegated click at a stub the selector would have caught. */
function click(stub: FakeEl) {
  const handler = paneEl.onclick as (e: { target: FakeEl }) => void;
  handler({ target: stub });
}

function serve(targets: unknown[], groups?: string[]) {
  bodyByPath = {
    "/api/remote/endpoints": { presence: "serving", endpoints: [{ id: "conn-1", label: "Build box", state: "idle" }] },
    "/api/remote/targets": groups ? { targets, groups } : { targets },
  };
  // The fake DOM's innerHTML = "" does not clear children; the view's paint() relies
  // on that clearing, so serve() models it before each scenario.
  groupsEl.children.length = 0;
  groupsEl.innerHTML = "";
}

describe("the Remote Targets page (remote plugin, R6 + R8)", () => {
  it("the view module exists at the entry the page descriptor points at", () => {
    const entry = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "views", "remote.js");
    expect(existsSync(entry), entry).toBe(true);
  });

  it("mount paints the transport line, the rows with endpoint labels and the count chip", async () => {
    serve([{ id: "build", label: "Build", endpoint: "conn-1", workspaceRoot: "/data/ws/proj", capabilities: ["exec", "sync"] }]);
    await view.mount();
    expect(paneEl.innerHTML).toContain("served by tunnels");
    expect(drawn()).toContain(">build<");
    expect(drawn()).toContain("Build box");
    expect(drawn()).toContain("/data/ws/proj");
    expect(byId("countChip").textContent).toBe("1 target");
  });

  it("an empty table draws the empty state in the groups slot, under the standing header", async () => {
    serve([]);
    await view.mount();
    // The tunnels page's shape: the header and its actions stay above the empty state
    // (Add target stays one click away), and the empty state takes the groups slot.
    expect(drawn()).toContain("No targets yet");
    expect(paneEl.innerHTML).toContain('id="rmAdd"');
    expect(paneEl.innerHTML).toContain('id="rmNewGroup"');
    expect(byId("countChip").textContent).toBe("");
  });

  it("a group the user created stays visible with no rows yet - a place, not an empty state", async () => {
    // The production bug this pins: create a group on an empty table and the page
    // answered the full empty state, so the new group vanished (docs/20: an empty
    // group is not an empty state - it is a drop target with a +).
    serve([], ["default", "test"]);
    await view.mount();
    expect(drawn()).toContain("default");
    expect(drawn()).toContain("test");
    expect(groupsEl.children.length).toBe(2);
    expect(drawn()).not.toContain("No targets yet");
    expect(byId("countChip").textContent).toBe("");
  });

  it("the rows render through the groups component: headers, one card per group, the family's page chrome", async () => {
    serve(
      [
        { id: "build", label: "Build", endpoint: "conn-1", workspaceRoot: "/data/ws", capabilities: ["exec"], group: "prod" },
        { id: "flash", label: "Flash", endpoint: "conn-1", workspaceRoot: "/opt", capabilities: ["exec"], group: null },
      ],
      ["default", "prod"],
    );
    await view.mount();
    // The names from the same response became headers (textContent of grp-name).
    expect(drawn()).toContain("default");
    expect(drawn()).toContain("prod");
    // Both headers mounted, not one merged card.
    expect(groupsEl.children.length).toBe(2);
    // Rows are addressable by the drag selector's key: data-rmrow on each row.
    expect(drawn()).toContain('data-rmrow="build"');
    expect(drawn()).toContain('data-rmrow="flash"');
    // The New-group button exists beside Add target (docs/20: the family's page chrome).
    expect(paneEl.innerHTML).toContain('id="rmNewGroup"');
    expect(paneEl.innerHTML).toContain('id="rmAdd"');
  });

  it("a stale row group falls back to the first group - the sink rule the family shares", async () => {
    serve(
      [{ id: "build", endpoint: "conn-1", workspaceRoot: "/data/ws", capabilities: ["exec"], group: "deleted-group" }],
      ["default", "prod"],
    );
    await view.mount();
    const first = groupsEl.children[0];
    // The first card is "default" and carries the stranded row (flatten includes names).
    expect(flatten(first)).toContain("build");
    expect(flatten(groupsEl.children[1])).not.toContain("build");
  });

  it("Edit reopens the sheet prefilled and posts to the row route with the group", async () => {
    serve([{ id: "build", label: "Build", endpoint: "conn-1", workspaceRoot: "/data/ws/proj", capabilities: ["exec"], group: "prod" }], ["default", "prod"]);
    await view.mount();
    const rowBtn = fakeEl();
    rowBtn.dataset.rmedit = "build";
    rowBtn.closest = (sel: string) => (sel === "[data-rmedit]" ? rowBtn : null);
    click(rowBtn);
    const sheet = byId("sheet");
    expect(sheet.hidden).toBe(false);
    expect(sheet.innerHTML).toContain("Edit build");
    expect(byId("rm-id").disabled).toBe(true);
    expect(byId("rm-label").value).toBe("Build");
    expect(byId("rm-root").value).toBe("/data/ws/proj");
    // The Group select is part of the sheet (docs/20's #g-sel), prefilled by markup.
    expect(sheet.innerHTML).toContain('id="g-sel"');
    expect(sheet.innerHTML).toContain('<option value="prod" selected>');
    byId("rm-label").value = "Renamed";
    byId("g-sel").value = "default";
    const save = byId("rm-save").onclick as () => Promise<void>;
    await save();
    expect(lastPost?.path).toBe("/api/remote/targets/build");
    const sent = JSON.parse(String(lastPost?.init.body));
    expect(sent.id).toBeUndefined();
    expect(sent.label).toBe("Renamed");
    expect(sent.shell).toBe("posix");
    expect(sent.group).toBe("default");
  });

  it("Add opens the sheet and Save posts the typed row - group included - to the same route the CLI uses", async () => {
    serve([], ["default", "prod"]);
    await view.mount();
    const addBtn = fakeEl();
    addBtn.closest = (sel: string) => (sel === "#rmAdd" ? addBtn : null);
    click(addBtn);
    expect(byId("sheet").hidden).toBe(false);
    expect(byId("sheet").innerHTML).toContain("Add a remote target");
    byId("rm-id").value = "build";
    byId("rm-label").value = "Build";
    byId("rm-endpoint").value = "conn-1";
    byId("g-sel").value = "prod";
    byId("rm-root").value = "/data/ws/proj";
    byId("rmcap-exec").checked = true;
    byId("rmcap-sync").checked = true;
    const save = byId("rm-save").onclick as () => Promise<void>;
    await save();
    expect(lastPost?.path).toBe("/api/remote/targets");
    expect(lastPost?.init.method).toBe("POST");
    const sent = JSON.parse(String(lastPost?.init.body));
    expect(sent.id).toBe("build");
    expect(sent.endpoint).toBe("conn-1");
    expect(sent.workspaceRoot).toBe("/data/ws/proj");
    expect(sent.shell).toBe("posix");
    expect(sent.capabilities).toEqual(["exec", "sync"]);
    expect(sent.group).toBe("prod");
  });

  it("a poll that changes nothing repaints nothing - the sheet the user types into stays put", async () => {
    serve([{ id: "build", endpoint: "conn-1", workspaceRoot: "/data/ws", capabilities: ["exec"] }]);
    await view.mount();
    const before = paneEl.innerHTML;
    const sheetStamp = byId("sheet").innerHTML;
    await view.refresh();
    expect(paneEl.innerHTML).toBe(before);
    expect(byId("sheet").innerHTML).toBe(sheetStamp);
  });
});

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

/* The Remote Targets page (docs/34 R6): one table of alias rows plus an Add/Edit sheet.
   The suite pins the view module contract under the same hand-rolled DOM the Token page
   suite uses: what mount paints (rows, endpoint labels, count chip), the empty state,
   and that the sheet saves through POST /api/remote/targets with the typed fields -
   the same route the CLI drives, which is the whole point of the page. */

interface FakeEl {
  innerHTML: string;
  textContent: string;
  hidden: boolean;
  onclick: unknown;
  value: string;
  checked: boolean;
  disabled: boolean;
  dataset: Record<string, string>;
  focus(): void;
  closest(sel: string): FakeEl | null;
}

function fakeEl(): FakeEl {
  return {
    innerHTML: "",
    textContent: "",
    hidden: false,
    onclick: null,
    value: "",
    checked: false,
    disabled: false,
    dataset: {},
    focus() {},
    closest() { return null; },
  };
}

const els = new Map<string, FakeEl>();
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
      return Promise.resolve({ status: 200, ok: true, json: async () => bodyByPath[p] ?? {} });
    },
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
  view = await import("../../src/admin_assets/js/views/remote.js");
});

const byId = (id: string): FakeEl => (globalThis as unknown as { document: { getElementById(k: string): FakeEl } }).document.getElementById(id);

/* Fire the pane delegated click at a stub the selector would have caught. */
function click(stub: FakeEl) {
  const handler = byId("pane").onclick as (e: { target: FakeEl }) => void;
  handler({ target: stub });
}

describe("the Remote Targets page (remote plugin, R6)", () => {
  it("the view module exists at the entry the page descriptor points at", () => {
    const entry = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "views", "remote.js");
    expect(existsSync(entry), entry).toBe(true);
  });

  it("mount paints the transport line, the rows with endpoint labels and the count chip", async () => {
    bodyByPath = {
      "/api/remote/endpoints": { presence: "serving", endpoints: [{ id: "conn-1", label: "Build box", state: "idle" }] },
      "/api/remote/targets": { targets: [
        { id: "build", label: "Build", endpoint: "conn-1", workspaceRoot: "/data/ws/proj", capabilities: ["exec", "sync"] },
      ] },
    };
    await view.mount();
    const pane = byId("pane").innerHTML;
    expect(pane).toContain("served by tunnels");
    expect(pane).toContain(">build<");
    expect(pane).toContain("Build box");
    expect(pane).toContain("/data/ws/proj");
    expect(byId("countChip").textContent).toBe("1 target");
  });

  it("an empty table draws the empty state, not a bare table", async () => {
    bodyByPath = {
      "/api/remote/endpoints": { presence: "none", endpoints: [] },
      "/api/remote/targets": { targets: [] },
    };
    await view.mount();
    expect(byId("pane").innerHTML).toContain("No targets yet");
    expect(byId("countChip").textContent).toBe("");
  });

    it("Edit reopens the sheet prefilled and posts to the row route", async () => {
    bodyByPath = {
      "/api/remote/endpoints": { presence: "serving", endpoints: [{ id: "conn-1", label: "Build box", state: "idle" }] },
      "/api/remote/targets": { targets: [
        { id: "build", label: "Build", endpoint: "conn-1", workspaceRoot: "/data/ws/proj", capabilities: ["exec"] },
      ] },
    };
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
    byId("rm-label").value = "Renamed";
    const save = byId("rm-save").onclick as () => Promise<void>;
    await save();
    expect(lastPost?.path).toBe("/api/remote/targets/build");
    const sent = JSON.parse(String(lastPost?.init.body));
    expect(sent.id).toBeUndefined();
    expect(sent.label).toBe("Renamed");
    expect(sent.shell).toBe("posix");
  });
it("Add opens the sheet and Save posts the typed row to the same route the CLI uses", async () => {
    bodyByPath = {
      "/api/remote/endpoints": { presence: "serving", endpoints: [{ id: "conn-1", label: "Build box", state: "idle" }] },
      "/api/remote/targets": { targets: [] },
    };
    await view.mount();
    const addBtn = fakeEl();
    addBtn.closest = (sel: string) => (sel === "#rmAdd" ? addBtn : null);
    click(addBtn);
    expect(byId("sheet").hidden).toBe(false);
    expect(byId("sheet").innerHTML).toContain("Add a remote target");
    byId("rm-id").value = "build";
    byId("rm-label").value = "Build";
    byId("rm-endpoint").value = "conn-1";
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
  });
});
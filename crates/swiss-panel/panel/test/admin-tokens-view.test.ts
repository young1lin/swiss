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

/* Token management lives in the MCP group (the toolbar button is gone): the token exists
   FOR MCP clients, so the page that lists, creates, rotates and revokes them sits beside
   Servers and Traffic. This suite pins the view module's own contract under a hand-rolled
   DOM: what mount paints, the count chip, and — the regression this page must never
   recommit — that the 6s poll patches the GROUPS REGION only, because rebuilding the pane
   would wipe the label input the user may be typing a new token's name into. The groups
   component (docs/20 G7) draws real nodes, so the fake element grows appendChild/dataset —
   the rows assertions read the group cards' markup instead of the pane's. */

interface FakeEl {
  innerHTML: string;
  textContent: string;
  hidden: boolean;
  onclick: unknown;
  className: string;
  title: string;
  type: string;
  value: string;
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
// The pane answers #tkGroups' element nowhere — the region repaints by id, not query, so the
// patch path is the tracked element itself.
const paneEl: FakeEl = { ...fakeEl(), querySelector: () => null };
els.set("pane", paneEl);

let responder: (path: string) => Promise<{ status: number; ok: boolean; json: () => Promise<unknown> }>;
let body: unknown = { tokens: [] };
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let view: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let state: any;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    document: {
      getElementById: (id: string) => {
        if (id === "tkGroups") return groupsEl;
        let e = els.get(id);
        if (!e) { e = fakeEl(); els.set(id, e); }
        return e;
      },
      createElement: () => fakeEl(),
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
    // The groups component reads window.CSS for CSS.escape and window.localStorage for fold
    // state; both are guarded, but the global must exist.
    window: { innerWidth: 1440, innerHeight: 900, addEventListener() {}, localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} } },
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
    confirm: () => true,
    location: { origin: "http://127.0.0.1:19998" },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  responder = () => Promise.resolve({ status: 200, ok: true, json: async () => body });
  state = ((await import("../src/util.js")) as unknown as { state: Record<string, unknown> }).state;
  view = await import("../src/views/tokens.js");
});

const byId = (id: string): FakeEl => els.get(id) as FakeEl;

/** Everything the groups region drew, flattened: the container's markup plus every node
 * below it — the fake DOM keeps children as elements, not markup, so the walk is recursive. */
function flatten(e: FakeEl): string {
  return e.innerHTML + e.children.map(flatten).join("");
}
function drawnRows(): string {
  return flatten(groupsEl);
}

describe("the Token page (MCP group)", () => {
  it("the view module exists at the entry every page descriptor points at", () => {
    const entry = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "views", "tokens.ts");
    expect(existsSync(entry), entry).toBe(true);
  });

  it("mount paints the list, the create form and the count chip", async () => {
    body = { tokens: [
      { id: "t0", label: "default", createdAt: 1700000000000 },
      { id: "t1", label: "claude-code", createdAt: 1700000001000 },
    ] };
    await view.mount();
    // No location title: the context bar already says "MCP / Token"; the body opens with
    // its workflow description instead (docs/13 D5 rev., body-chrome dedup).
    expect(paneEl.innerHTML).toContain("pane-desc");
    expect(paneEl.innerHTML).not.toContain("pane-title");
    expect(drawnRows()).toContain("claude-code");
    expect(paneEl.innerHTML).toContain('id="tkLabel"');
    // The create form carries the Group select (docs/20 G7) — one option on a bare gateway.
    expect(paneEl.innerHTML).toContain('id="tkGroup"');
    expect(paneEl.innerHTML).toContain('id="tkNewGroup"');
    expect(byId("countChip").textContent).toBe("2 tokens");
  });

  it("the once-only secret box appears with its copy actions once a secret exists", async () => {
    state.tokenViewSecret = "s3cr3t-oneshot";
    await view.refresh(); // an explicit refresh must keep the box alive
    expect(paneEl.innerHTML).toContain("shown only once");
    expect(paneEl.innerHTML).toContain("Copy secret");
    expect(paneEl.innerHTML).toContain("Copy connect commands (all MCPs)");
    state.tokenViewSecret = null;
  });

  it("countText pluralizes honestly", async () => {
    body = { tokens: [{ id: "t0", label: "default" }] };
    await view.refresh();
    expect(view.countText()).toBe("1 token");
    body = { tokens: [] };
    await view.refresh();
    expect(view.countText()).toBe("0 tokens");
  });

  it("poll patches the groups region only - a half-typed label survives it", async () => {
    body = { tokens: [{ id: "t0", label: "default" }] };
    await view.refresh();
    // The canary: if poll() takes the full-render path, this string is replaced.
    paneEl.innerHTML = "CANARY";
    groupsEl.innerHTML = "";
    body = { tokens: [{ id: "t0", label: "default" }, { id: "t1", label: "cursor" }] };
    await view.poll();
    expect(paneEl.innerHTML).toBe("CANARY"); // no pane rebuild
    expect(drawnRows()).toContain("cursor"); // the rows themselves moved on
    expect(byId("countChip").textContent).toBe("2 tokens");
    // An unchanged list must not even touch the groups region.
    groupsEl.innerHTML = "ROWS-CANARY";
    groupsEl.children.length = 0;
    await view.poll();
    expect(groupsEl.innerHTML).toBe("ROWS-CANARY");
  });

  it("still exports the connect-flow helpers connect.js depends on", () => {
    for (const name of ["pickCopyToken", "refreshTokens", "rememberedTokenId"]) {
      expect(typeof view[name], name).toBe("function");
    }
  });
});
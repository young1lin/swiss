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

/* The Data view's lifecycle bug this suite pins: views/data.js unmount() nulls state.db to
   free it, but a dynamic import evaluates data-view.js exactly once — the module-top literal
   that used to build state.db never ran again, so every SECOND entry crashed inside
   renderDbView's wiring ("Cannot read properties of null (reading 'grep')") and left the
   rest of the wiring — SQL console Run, cell-edit handlers — dead. The fix: a state factory
   plus a rebuild at loadDbView's entry. This suite drives the REAL modules under a
   hand-rolled DOM (same trick as admin-navigation.test.ts): mount, unmount, mount again. */

interface FakeNode {
  children: unknown[];
  style: Record<string, string>;
  dataset: Record<string, string>;
  hidden: boolean;
  value: string;
  checked: boolean;
  className: string;
  textContent: string;
  innerHTML: string;
  title: string;
  scrollTop: number;
  scrollLeft: number;
  onclick: unknown;
  oninput: unknown;
  onscroll: unknown;
  appendChild(c: unknown): unknown;
  removeChild(c: unknown): unknown;
  remove(): void;
  classList: { add(): void; remove(): void; toggle(): void; contains(): boolean };
  addEventListener(): void;
  removeEventListener(): void;
  setAttribute(): void;
  getAttribute(): string | null;
  focus(): void;
  blur(): void;
  closest(): null;
  querySelector(): FakeNode;
  querySelectorAll(): [];
}

function node(): FakeNode {
  const n = {
    children: [], style: {}, dataset: {}, hidden: false, value: "", checked: false,
    className: "", textContent: "", innerHTML: "", title: "", scrollTop: 0, scrollLeft: 0,
    onclick: null, oninput: null, onscroll: null,
    appendChild(c: unknown) { n.children.push(c); return c; },
    removeChild(c: unknown) { return c; },
    remove() {},
    classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
    addEventListener() {}, removeEventListener() {},
    setAttribute() {}, getAttribute: () => null,
    focus() {}, blur() {}, closest: () => null,
    querySelector: () => node(), querySelectorAll: () => [],
  } as FakeNode;
  return n;
}

const els = new Map<string, FakeNode>();
let responder: (path: string) => Promise<{ status: number; ok: boolean; json(): Promise<unknown> }>;

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let state: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let view: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let dataView: any;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  Object.assign(globalThis, {
    document: {
      getElementById: (id: string) => {
        let e = els.get(id);
        if (!e) { e = node(); els.set(id, e); }
        return e;
      },
      createElement: () => node(),
      createTextNode: () => node(),
      querySelector: () => null,
      querySelectorAll: () => [],
      addEventListener: () => {},
      removeEventListener: () => {},
      documentElement: node(),
      body: node(),
      head: node(),
      visibilityState: "visible",
      activeElement: null,
    },
    fetch: (path: unknown) => responder(String(path)),
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
    confirm: () => true,
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  responder = () => Promise.resolve({ status: 200, ok: true, json: async () => ({ connections: [] }) });
  state = (await import("../src/util.js")).state;
  dataView = await import("../src/data-view.js");
  view = await import("../src/views/data.js");
});

describe("the Data view survives unmount and remount", () => {
  it("mounts, and the state factory has run", async () => {
    await view.mount();
    expect(state.db).not.toBeNull();
    expect(state.db).toHaveProperty("grep", "");
    expect(state.db).toHaveProperty("conn", null);
  });

  it("unmount frees the state — a null state reports no pending changes", async () => {
    view.unmount();
    expect(state.db).toBeNull();
    // hasPendingChanges runs from the reload guard; it must not read through null.
    expect(dataView.dbPending()).toBe(0);
    expect(view.hasPendingChanges()).toBe(false);
  });

  it("mounts AGAIN without crashing, rebuilding the state (the regression)", async () => {
    // Before the factory+rebuild fix this threw "Cannot read properties of null
    // (reading 'grep')" inside renderDbView, leaving the SQL console and cell
    // editing unwired — the "Data is dead after navigating away and back" bug.
    await expect(view.mount()).resolves.toBeUndefined();
    expect(state.db).not.toBeNull();
    expect(state.db).toHaveProperty("grep", "");
  });

  it("re-entering a third time still works (the rebuild is not once-only)", async () => {
    view.unmount();
    await view.mount();
    expect(state.db).not.toBeNull();
  });
});

/* The workspace framing (docs/13 D5, as revised): renderDbView turns the pane into a
   full-bleed body by adding .db-host, and views/data.js's unmount must take it back off —
   navigatePage always awaits unmount before the next page paints, so a class left behind
   would strip the NEXT page's padding and measure no matter which page it is. */
describe("the workspace framing class is taken back off on unmount", () => {
  it("mount adds db-host to the pane; unmount removes it — no page inherits it", async () => {
    const pane = els.get("pane")!;
    const ops: string[] = [];
    pane.classList = {
      add: (c: string) => ops.push("+" + c),
      remove: (c: string) => ops.push("-" + c),
      toggle: () => {},
      contains: () => false,
    };
    await view.mount();
    view.unmount();
    expect(ops).toEqual(["+db-host", "-db-host"]);
  });
});

/* The page bar's count chip (docs/18 V3 moved it there). countText() used to return "Data",
   which the group label one slot left already says — the bar read "Data … Data". It now names
   the connection being browsed, in the sidebar dropdown's own words, and renders nothing when
   no connection is selected. */
describe("countText names the connection, not the page (docs/18 follow-up)", () => {
  it("mirrors the connection dropdown's label for the selected connection", async () => {
    await view.mount();
    state.db.conns = [
      { name: "shop-redis", dialect: "redis" },
      { name: "pg-app", dialect: "postgres" },
    ];
    state.db.conn = "shop-redis";
    expect(view.countText()).toBe("shop-redis · redis");
    state.db.conn = "pg-app";
    // No read-only suffix anymore: there is no readonly flag anywhere in the picker.
    expect(view.countText()).toBe("pg-app · postgres");
  });

  it("is empty with no connection selected — and after unmount freed the state", async () => {
    state.db.conn = null;
    expect(view.countText()).toBe("");
    view.unmount();
    expect(state.db).toBeNull();
    expect(view.countText()).toBe("");
  });
});

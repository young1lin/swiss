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
import { dbIsMounted, dbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";

/* The Data view's lifecycle contract this suite pins: mount, unmount, mount again, and the
   view works every time. It was written for a crash — unmount nulled state.db, a dynamic
   import evaluated data-view.js exactly once, so the module-top literal never ran again and
   every SECOND entry died inside renderDbView's wiring ("Cannot read properties of null
   (reading 'grep')"), leaving SQL console Run and the cell-edit handlers dead. docs/37 R4
   retired the mechanism rather than the symptom: db-state.ts owns the record, unmount resets
   it instead of nulling it, and "is the view on screen" became a flag of its own. The crash
   is now unreachable, so these tests assert the CONTRACT — fresh record on entry, nothing
   buffered after leaving, a second and third entry that work — which is what the user was
   owed all along. Drives the REAL modules under a hand-rolled DOM (same trick as
   admin-navigation.test.ts).
   docs/37 R5: renderDbView builds its skeleton with h()/fill(), so the fake nodes extend a
   Node stub (h() instanceof-checks every child) and document carries the NS/fragment
   factories the builders call. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

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
  classList: { add(c?: string): void; remove(c?: string): void; toggle(): void; contains(): boolean };
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
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
}

const els = new Map<string, FakeNode>();
let responder: (path: string) => Promise<{ status: number; ok: boolean; json(): Promise<unknown> }>;

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
      createElementNS: () => node(),
      createDocumentFragment: () => node(),
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
  dataView = await import("../src/data-view.js");
  view = await import("../src/views/data.js");
});

describe("the Data view survives unmount and remount", () => {
  it("mounts, and the record starts from the fresh literal", async () => {
    await view.mount();
    expect(dbIsMounted()).toBe(true);
    expect(dbView().grep).toBe("");
    expect(dbView().conn).toBeNull();
  });

  it("unmount resets the record — an unmounted view reports no pending changes", async () => {
    dbView().inserts = [{ values: { a: 1 } }];
    view.unmount();
    expect(dbIsMounted()).toBe(false);
    // What nulling it used to buy: the rows, the buffered edits and the results are gone.
    expect(dbView().inserts).toEqual([]);
    // hasPendingChanges runs from the reload guard, on a view that is no longer there.
    expect(dataView.dbPending()).toBe(0);
    expect(view.hasPendingChanges()).toBe(false);
  });

  it("mounts AGAIN without crashing (the regression)", async () => {
    // The original bug: unmount nulled state.db, and the module-top literal that built it
    // ran exactly once, so this threw "Cannot read properties of null (reading 'grep')"
    // inside renderDbView and left the SQL console and cell editing unwired — "Data is
    // dead after navigating away and back". docs/37 R4 made it unreachable rather than
    // handled: db-state.ts owns a record that is never absent. This still drives the real
    // path, because what the user is owed is a working second entry, not a mechanism.
    await expect(view.mount()).resolves.toBeUndefined();
    expect(dbIsMounted()).toBe(true);
    expect(dbView().grep).toBe("");
  });

  it("re-entering a third time still works (the reset is not once-only)", async () => {
    view.unmount();
    await view.mount();
    expect(dbIsMounted()).toBe(true);
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
    dbView().conns = [
      dbConn("shop-redis", "redis"),
      dbConn("pg-app", "postgres"),
    ];
    dbView().conn = "shop-redis";
    expect(view.countText()).toBe("shop-redis · redis");
    dbView().conn = "pg-app";
    // No read-only suffix anymore: there is no readonly flag anywhere in the picker.
    expect(view.countText()).toBe("pg-app · postgres");
  });

  it("is empty with no connection selected — and after unmount reset the record", async () => {
    dbView().conn = null;
    expect(view.countText()).toBe("");
    view.unmount();
    expect(dbIsMounted()).toBe(false);
    expect(view.countText()).toBe("");
  });
});

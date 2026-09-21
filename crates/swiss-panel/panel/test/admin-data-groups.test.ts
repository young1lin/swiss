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
import { dbConn, mountDbView, unmountDbView } from "../src/db-state.js";

/* The Data sidebar dropdown's grouping (docs/20 G5): every /api/db row carries the group
   its connection lists under, and the dropdown folds its options into one optgroup per
   group when — and only when — there is more than one. A single group stays a flat list
   (an optgroup around everything is noise that says nothing), and an older gateway that
   answers no group at all renders exactly the flat list it always drew. This suite drives
   the real renderDbSide under a hand-rolled DOM (the admin-data-state trick). */

interface FakeNode {
  tag: string;
  label: string;
  value: string;
  textContent: string;
  selected: boolean;
  children: FakeNode[];
  appendChild(n: FakeNode): FakeNode;
}

function node(tag: string): FakeNode {
  const n = { tag, label: "", value: "", textContent: "", selected: false, children: [] as FakeNode[] } as FakeNode;
  n.appendChild = (c: FakeNode) => { n.children.push(c); return c; };
  return n;
}

let sel: FakeNode | null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let dataView: any;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.assign(globalThis, {
    document: {
      getElementById: (id: string) => (id === "dbConn" ? sel : node("div")),
      createElement: (tag: string) => node(tag),
      querySelector: () => null,
      querySelectorAll: () => [],
      addEventListener() {},
      removeEventListener() {},
      documentElement: node("html"),
      body: node("body"),
      head: node("head"),
      visibilityState: "visible",
    },
    window: { addEventListener() {}, localStorage: { getItem: () => null, setItem() {}, removeItem() {} } },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    location: { origin: "http://127.0.0.1:19998" },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
  });
  dataView = await import("../src/data-view.js");
});

/** Render the given connection rows through the real renderDbSide and hand back the select. */
function render(conns: unknown[]): FakeNode {
  sel = node("select");
  // The record is the view's own now (docs/37 R4). Replacing it wholesale is no longer a
  // seam a test can reach, so each render starts from the fresh literal — which is what
  // assigning over the whole object used to buy — and then names what it cares about.
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), { conns, conn: "", tables: [], redis: null });
  dataView.renderDbSide();
  return sel;
}

describe("the Data dropdown's groups (docs/20 G5)", () => {
  it("a single group stays a flat list — no optgroup around everything", () => {
    const s = render([
      { name: "shop-mysql", dialect: "mysql", group: "default" },
      { name: "shop-pg", dialect: "pg", group: "default" },
    ]);
    expect(s.children.every((c) => c.tag === "option")).toBe(true);
    expect(s.children.map((c) => c.value)).toEqual(["shop-mysql", "shop-pg"]);
  });

  it("several groups fold into one optgroup per group, in first-appearance order", () => {
    const s = render([
      { name: "shop-pg", dialect: "pg", group: "Ops" },
      { name: "shop-mysql", dialect: "mysql" }, // no group: the default bucket
      { name: "shop-redis", dialect: "redis", group: "Ops" },
      { name: "shop-sqlite", dialect: "sqlite", group: "Lab" },
    ]);
    expect(s.children.map((c) => c.tag)).toEqual(["optgroup", "optgroup", "optgroup"]);
    expect(s.children.map((c) => c.label)).toEqual(["Ops", "default", "Lab"]);
    expect(s.children[0].children.map((c) => c.value)).toEqual(["shop-pg", "shop-redis"]);
    expect(s.children[1].children.map((c) => c.value)).toEqual(["shop-mysql"]);
    expect(s.children[2].children.map((c) => c.value)).toEqual(["shop-sqlite"]);
  });

  it("an older gateway answers no group at all — the same flat list it always drew", () => {
    const s = render([
      { name: "shop-mysql", dialect: "mysql" },
      { name: "shop-pg", dialect: "pg" },
    ]);
    expect(s.children.every((c) => c.tag === "option")).toBe(true);
    expect(s.children.length).toBe(2);
  });
});
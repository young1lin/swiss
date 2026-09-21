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

import { describe, it, expect, afterAll } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { ApiDbConnectionRow } from "../src/types/api.js";
import type { MenuItemAction } from "../src/types/dom.js";

/* The connection menu's grouping (docs/20 G5, restated by docs/43 M3): the dropdown became
   a row that opens a menu, so the optgroups became heading rows and the flat list became
   rows without one. The three facts the old optgroup suite pinned are the same three this
   one pins — a single group stays flat (a heading around everything is noise that says
   nothing), several groups fold one heading per group in first-appearance order, and an
   older gateway that answers no group at all reads as one flat list.
   The suite only calls the pure builder, but importing data-view pulls connect.ts, whose
   module top level reaches for document — so the stub is installed (and later restored,
   process-wide under the default pool) before the dynamic import, the admin-data-state
   trick. */

class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

const el = (): Record<string, any> => {
  const n: any = {
    children: [], style: {}, dataset: {}, hidden: false, textContent: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Record<string, any>) { n.children.push(c); return c; },
    removeChild(c: Record<string, any>) { n.children = n.children.filter((x: Record<string, any>) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    setAttribute() {}, getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {}, dispatchEvent: () => true,
    querySelector: () => null, querySelectorAll: () => [],
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0, right: 0, bottom: 0 }),
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => {} });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};

const SAVED = (["document", "window", "localStorage", "location", "matchMedia",
  "confirm", "alert", "prompt", "setInterval", "clearInterval", "addEventListener",
  "removeEventListener", "Node"] as const).map((k: string) => ({
    k,
    d: Object.getOwnPropertyDescriptor(globalThis, k),
  }));
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    createElement: () => el(), createElementNS: () => el(),
    createDocumentFragment: () => el(),
    createTextNode: (s: string) => { const n = el(); n.textContent = s; return n; },
    getElementById: () => el(), querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener() {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { origin: "http://127.0.0.1:19998", reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: (): number => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
const dataView = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  dbConnMenuItems: (
    conns: ApiDbConnectionRow[], current: string, pick: (name: string) => void,
  ) => MenuItemAction[];
};

afterAll(() => {
  // Put the process back the way this file found it.
  for (const { k, d } of SAVED) {
    if (d) Object.defineProperty(globalThis, k, d);
    else delete (globalThis as unknown as Record<string, unknown>)[k];
  }
});

interface Row {
  name: string;
  dialect: string;
  group?: string;
}

function rows(conns: Row[]): MenuItemAction[] {
  return dataView.dbConnMenuItems(conns as ApiDbConnectionRow[], "", (): void => {})
    .filter((r): r is MenuItemAction => "label" in r);
}

function labels(items: MenuItemAction[]): string[] {
  // Separators carry no label; the walker only names rows that can be read. A row's label
  // is dbConnLabel's "name · dialect" - the name is what the fixtures vary.
  return items.map((r) => r.label.split(" · ")[0]);
}

describe("the connection menu's groups (docs/20 G5, docs/43 M3)", () => {
  it("a single group stays a flat list — no heading around everything", () => {
    const items = rows([
      { name: "shop-mysql", dialect: "mysql", group: "default" },
      { name: "shop-pg", dialect: "pg", group: "default" },
    ]);
    expect(items.some((r) => r.heading)).toBe(false);
    expect(labels(items)).toEqual(["shop-mysql", "shop-pg"]);
  });

  it("several groups fold into one heading per group, in first-appearance order", () => {
    const all = dataView.dbConnMenuItems([
      { name: "shop-pg", dialect: "pg", group: "Ops" },
      { name: "shop-mysql", dialect: "mysql" }, // no group: the default bucket
      { name: "shop-redis", dialect: "redis", group: "Ops" },
      { name: "shop-sqlite", dialect: "sqlite", group: "Lab" },
    ] as ApiDbConnectionRow[], "", (): void => {});
    const named = all.filter((r): r is MenuItemAction => "label" in r);
    const heads = named.filter((r) => r.heading).map((r) => r.label);
    expect(heads).toEqual(["Ops", "default", "Lab"]);
    // One separator between groups; the first group opens cold (no leading separator
    // when there is more than one - the single-group flat list is what carries one).
    expect(all.filter((r) => "sep" in r).length).toBe(2);
    // The rows read in first-appearance order: Ops' two, then default's, then Lab's.
    expect(named.filter((r) => !r.heading).map((r) => r.label.split(" · ")[0]))
      .toEqual(["shop-pg", "shop-redis", "shop-mysql", "shop-sqlite"]);
  });

  it("an older gateway answers no group at all — the same flat list it always drew", () => {
    const items = rows([
      { name: "shop-mysql", dialect: "mysql" },
      { name: "shop-pg", dialect: "pg" },
    ]);
    expect(items.some((r) => r.heading)).toBe(false);
    expect(labels(items).length).toBe(2);
  });

  it("picking a row hands the connection's name to the caller and marks the current one", () => {
    const picked: string[] = [];
    const items = dataView.dbConnMenuItems([
      { name: "shop-mysql", dialect: "mysql", group: "default" },
      { name: "shop-pg", dialect: "pg", group: "default" },
    ] as ApiDbConnectionRow[], "shop-pg", (n: string): void => { picked.push(n); })
      .filter((r): r is MenuItemAction => "label" in r);
    const mysql = items.find((r) => r.label.startsWith("shop-mysql"));
    mysql?.fn();
    expect(picked).toEqual(["shop-mysql"]);
    expect(items.find((r) => r.label.startsWith("shop-pg"))?.on).toBe(true);
    expect(mysql?.on).toBe(false);
  });
});

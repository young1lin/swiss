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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-grid.test.ts: the panel ships browser ES modules, so
// the module graph needs the globals stubbed before it will evaluate under Node.
const el = (): Record<string, unknown> => {
  const node: Record<string, unknown> = {
    style: {}, dataset: {}, hidden: false, disabled: false, checked: false, value: "",
    textContent: "", innerHTML: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    setAttribute: () => {}, getAttribute: () => "", removeAttribute: () => {},
    addEventListener: () => {}, removeEventListener: () => {},
    appendChild: (c: unknown) => c, removeChild: (c: unknown) => c, remove: () => {},
    querySelector: () => null, querySelectorAll: () => [],
    focus: () => {}, blur: () => {}, click: () => {},
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
  };
  node.cloneNode = () => el();
  return node;
};
const doc = {
  documentElement: el(), body: el(), head: el(),
  hidden: false, visibilityState: "visible", activeElement: null,
  getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};

// The one piece of storage these tests actually read: a scripted localStorage, so load and
// save round-trips pin against real key/value behaviour instead of a no-op stub.
const store = new Map<string, string>();
const localStorage = {
  getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
};
Object.assign(globalThis, {
  document: doc, window: globalThis, localStorage,
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const grid = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-grid.ts")).href
) as {
  dbGridConfigKey: (conn: string, schema: string | null, table: string) => string;
  dbGridConfigParse: (raw: string | null) => { widths: Record<string, number>; hidden: string[] };
  dbGridConfigLoad: (key: string) => { widths: Record<string, number>; hidden: string[] };
  dbGridConfigSave: (key: string, cfg: { widths: Record<string, number>; hidden: string[] }) => void;
  dbGridVisibleColumns: <C extends { name: string }>(columns: C[], hidden: string[]) => C[];
};

// docs/22 W2.1 — per-connection grid config: one localStorage object per connection+table
// holding dragged widths and hidden columns (dbgate's useGridConfig precedent, one object
// per grid, never one key per value).
describe("grid config storage", () => {
  it("keys on connection and schema-qualified table, under the swiss.dbGrid.* namespace", () => {
    expect(grid.dbGridConfigKey("shop-mysql", null, "users"))
      .toBe("swiss.dbGrid.shop-mysql_users");
    expect(grid.dbGridConfigKey("shop-pg", "public", "events"))
      .toBe("swiss.dbGrid.shop-pg_public.events");
  });

  it("writes only swiss.* keys, never anything else", () => {
    store.clear();
    const key = grid.dbGridConfigKey("shop-pg", "public", "events");
    grid.dbGridConfigSave(key, { widths: { a: 100 }, hidden: [] });
    expect(Array.from(store.keys()).every((k) => k.startsWith("swiss."))).toBe(true);
    expect(store.has(key)).toBe(true);
  });

  it("parses a well-formed entry", () => {
    var cfg = grid.dbGridConfigParse('{"widths":{"id":120},"hidden":["note"]}');
    expect(cfg.widths).toEqual({ id: 120 });
    expect(cfg.hidden).toEqual(["note"]);
  });

  it("returns a fresh config for missing, corrupt or non-object entries — never a broken grid", () => {
    expect(grid.dbGridConfigParse(null)).toEqual({ widths: {}, hidden: [] });
    expect(grid.dbGridConfigParse("not json {")).toEqual({ widths: {}, hidden: [] });
    expect(grid.dbGridConfigParse('"a string"')).toEqual({ widths: {}, hidden: [] });
    expect(grid.dbGridConfigParse('{"widths":"nope","hidden":7}')).toEqual({ widths: {}, hidden: [] });
  });

  it("drops width values outside the clamp so a hand-edited 0 or 999999 cannot break a column", () => {
    // A quoted number is coerced (the same Number() tolerance the query params get); only the
    // out-of-clamp and non-numeric values are gone.
    var cfg = grid.dbGridConfigParse('{"widths":{"a":10,"b":48,"c":5000,"d":"120","e":"wide"}}');
    expect(cfg.widths).toEqual({ b: 48, d: 120 });
  });

  it("keeps only string names in hidden", () => {
    var cfg = grid.dbGridConfigParse('{"hidden":["a",3,null,"b"]}');
    expect(cfg.hidden).toEqual(["a", "b"]);
  });

  it("round-trips through the scripted localStorage", () => {
    var key = grid.dbGridConfigKey("shop-mysql", null, "t1");
    grid.dbGridConfigSave(key, { widths: { x: 200 }, hidden: ["y"] });
    expect(store.get(key)).toBe('{"widths":{"x":200},"hidden":["y"]}');
    expect(grid.dbGridConfigLoad(key)).toEqual({ widths: { x: 200 }, hidden: ["y"] });
    // A key never written is a fresh config, not a throw.
    expect(grid.dbGridConfigLoad("swiss.dbGrid.none_never")).toEqual({ widths: {}, hidden: [] });
  });
});

describe("dbGridVisibleColumns", () => {
  var cols = [{ name: "id" }, { name: "name" }, { name: "note" }];
  it("shows everything when nothing is hidden", () => {
    expect(grid.dbGridVisibleColumns(cols, [])).toEqual(cols);
  });
  it("drops the hidden names, keeps order", () => {
    expect(grid.dbGridVisibleColumns(cols, ["name"])).toEqual([{ name: "id" }, { name: "note" }]);
    expect(grid.dbGridVisibleColumns(cols, ["id", "note"])).toEqual([{ name: "name" }]);
  });
  it("tolerates names that are not columns", () => {
    expect(grid.dbGridVisibleColumns(cols, ["gone"])).toEqual(cols);
  });
});

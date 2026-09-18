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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-cellview.test.ts: the panel ships browser ES modules,
// so the module graph needs the globals stubbed before it will evaluate under Node.
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
Object.assign(globalThis, {
  document: doc, window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const form = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-form.ts")).href
) as {
  dbFormRowFields: (
    columns: { name: string; dataType?: string }[],
    row: Record<string, unknown> | null,
    key: string | null,
    updates: Record<string, { pk: unknown; changes: Record<string, unknown> }>,
    insert: { values: Record<string, unknown> } | null,
  ) => { name: string; value: unknown; orig: unknown; pending: boolean }[];
  dbFormWrite: (
    d: { updates: Record<string, { pk: unknown; changes: Record<string, unknown> }>; inserts: { values: Record<string, unknown> }[] },
    kind: "update" | "insert",
    key: string | null,
    i: number,
    column: string,
    meta: { pk?: unknown; orig?: unknown },
    v: string | null | undefined,
  ) => void;
};

/* docs/22 W5.1 — the single-record form. The field resolution and the buffer write are the
   form's whole contract: the form paints dbFormRowFields' answer, and its controls write
   through dbFormWrite with EXACTLY the grid's collapse-back semantics, so the buffer counts
   on the bar are the two views' shared truth. */
describe("dbFormRowFields — one row as the form paints it", () => {
  const columns = [{ name: "id", dataType: "int" }, { name: "name", dataType: "varchar" }, { name: "flag", dataType: "tinyint" }];
  const row = { id: 1, name: "alice", flag: 0 };

  it("an untouched update row shows originals, nothing pending", () => {
    var f = form.dbFormRowFields(columns, row, "k", {}, null);
    expect(f.map((x) => [x.name, x.value, x.pending]))
      .toEqual([["id", 1, false], ["name", "alice", false], ["flag", 0, false]]);
  });

  it("a buffered change wins over the original and is marked pending", () => {
    var f = form.dbFormRowFields(columns, row, "k", { k: { pk: {}, changes: { name: "bob" } } }, null);
    var name = f.find((x) => x.name === "name")!;
    expect(name.value).toBe("bob");
    expect(name.orig).toBe("alice");
    expect(name.pending).toBe(true);
  });

  it("a buffered NULL paints as null, not as the original", () => {
    var f = form.dbFormRowFields(columns, row, "k", { k: { pk: {}, changes: { name: null } } }, null);
    var name = f.find((x) => x.name === "name")!;
    expect(name.value).toBeNull();
    expect(name.pending).toBe(true);
  });

  it("an insert row reads the buffered values; an unset column is default (undefined), never null", () => {
    var f = form.dbFormRowFields(columns, null, null, {}, { values: { name: "new" } });
    expect(f.find((x) => x.name === "name")!.value).toBe("new");
    expect(f.find((x) => x.name === "name")!.pending).toBe(true);
    expect(f.find((x) => x.name === "id")!.value).toBeUndefined();
    expect(f.find((x) => x.name === "id")!.pending).toBe(false);
  });
});

describe("dbFormWrite — the form's buffer semantics ARE the grid's", () => {
  const meta = { pk: { id: 1 }, orig: "alice" };
  const d0 = () => ({ updates: {}, inserts: [{ values: {} }] });

  it("writes a changed value into the update buffer, keyed like the grid", () => {
    var d = d0();
    form.dbFormWrite(d, "update", "[1]", 0, "name", meta, "bob");
    expect(d.updates["[1]"].changes).toEqual({ name: "bob" });
  });

  it("collapses back when the raw text equals the original — and drops the emptied entry", () => {
    var d = { updates: { "[1]": { pk: { id: 1 }, changes: { name: "bob" } } }, inserts: [] };
    form.dbFormWrite(d, "update", "[1]", 0, "name", { pk: { id: 1 }, orig: "bob" }, "bob");
    expect(d.updates["[1]"]).toBeUndefined();
  });

  it("writes a text form of a numeric original back as a collapse, like the inline editor", () => {
    var d = d0();
    form.dbFormWrite(d, "update", "[1]", 0, "flag", { pk: { id: 1 }, orig: 0 }, "0");
    expect(d.updates["[1]"]).toBeUndefined();
  });

  it("the NULL button writes null; a NULL original collapses", () => {
    var d = d0();
    form.dbFormWrite(d, "update", "[1]", 0, "name", meta, null);
    expect(d.updates["[1]"].changes).toEqual({ name: null });
    var d2 = { updates: { "[1]": { pk: {}, changes: { name: null } } }, inserts: [] };
    form.dbFormWrite(d2, "update", "[1]", 0, "name", { pk: {}, orig: null }, null);
    expect(d2.updates["[1]"]).toBeUndefined();
  });

  it("an insert's empty text reverts to the default (deleted), other text and NULL both set", () => {
    var d = d0();
    form.dbFormWrite(d, "insert", null, 0, "name", { orig: undefined }, "new");
    expect(d.inserts[0].values).toEqual({ name: "new" });
    form.dbFormWrite(d, "insert", null, 0, "name", { orig: undefined }, "");
    expect(d.inserts[0].values).toEqual({});
    form.dbFormWrite(d, "insert", null, 0, "name", { orig: undefined }, null);
    expect(d.inserts[0].values).toEqual({ name: null });
  });
});

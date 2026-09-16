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
Object.assign(globalThis, {
  document: doc, window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const grid = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-grid.js")).href
) as {
  dbTsvRows: (text: string) => string[][];
  dbKbdMove: (r: number, c: number, key: string, maxR: number, maxC: number) => { r: number; c: number };
};

// docs/22 W2.2 — the two pure halves of keyboard navigation and TSV paste. The handlers that
// own focus and the clipboard are verified live on 19998; the arithmetic is pinned here.
describe("dbTsvRows", () => {
  it("splits rows on newlines and cells on tabs", () => {
    expect(grid.dbTsvRows("a\tb\tc\nd\te\tf")).toEqual([["a", "b", "c"], ["d", "e", "f"]]);
  });

  it("normalizes CRLF and lone CR (Excel and Windows clipboards both speak them)", () => {
    expect(grid.dbTsvRows("a\tb\r\nd\te")).toEqual([["a", "b"], ["d", "e"]]);
    expect(grid.dbTsvRows("a\tb\rd\te")).toEqual([["a", "b"], ["d", "e"]]);
  });

  it("a trailing newline ends the last row, it is not an empty row after it", () => {
    expect(grid.dbTsvRows("a\tb\n")).toEqual([["a", "b"]]);
    expect(grid.dbTsvRows("a\tb\r\n\r\n")).toEqual([["a", "b"], [""]]);
  });

  it("empty text is no rows; one cell with no tab is one row of one cell", () => {
    expect(grid.dbTsvRows("")).toEqual([]);
    expect(grid.dbTsvRows("solo")).toEqual([["solo"]]);
  });

  it("keeps empty cells (two tabs in a row are an empty middle cell, not a lost one)", () => {
    expect(grid.dbTsvRows("a\t\tc")).toEqual([["a", "", "c"]]);
  });
});

describe("dbKbdMove", () => {
  it("moves one cell in each direction", () => {
    expect(grid.dbKbdMove(2, 2, "ArrowUp", 9, 4)).toEqual({ r: 1, c: 2 });
    expect(grid.dbKbdMove(2, 2, "ArrowDown", 9, 4)).toEqual({ r: 3, c: 2 });
    expect(grid.dbKbdMove(2, 2, "ArrowLeft", 9, 4)).toEqual({ r: 2, c: 1 });
    expect(grid.dbKbdMove(2, 2, "ArrowRight", 9, 4)).toEqual({ r: 2, c: 3 });
  });

  it("clamps at the grid's edges rather than leaving it", () => {
    expect(grid.dbKbdMove(0, 0, "ArrowUp", 9, 4)).toEqual({ r: 0, c: 0 });
    expect(grid.dbKbdMove(0, 0, "ArrowLeft", 9, 4)).toEqual({ r: 0, c: 0 });
    expect(grid.dbKbdMove(9, 4, "ArrowDown", 9, 4)).toEqual({ r: 9, c: 4 });
    expect(grid.dbKbdMove(9, 4, "ArrowRight", 9, 4)).toEqual({ r: 9, c: 4 });
  });

  it("Home and End jump the row's first and last column", () => {
    expect(grid.dbKbdMove(3, 2, "Home", 9, 4)).toEqual({ r: 3, c: 0 });
    expect(grid.dbKbdMove(3, 2, "End", 9, 4)).toEqual({ r: 3, c: 4 });
  });

  it("an unhandled key leaves the cell where it was", () => {
    expect(grid.dbKbdMove(3, 2, "Shift", 9, 4)).toEqual({ r: 3, c: 2 });
  });
});

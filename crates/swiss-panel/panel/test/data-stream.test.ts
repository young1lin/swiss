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
import { pathToFileURL, fileURLToPath } from "node:url";

// Same DOM-stub technique as admin-data-activity.test.ts: the panel ships browser ES
// modules, so the module graph needs the globals stubbed before it will evaluate under Node.
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
  confirm: () => true, alert: () => {}, prompt: () => "",
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const stream = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-stream.ts")).href
) as {
  streamMerge: (rows: StreamEntry[], page: StreamEntry[]) => StreamEntry[];
  streamColumns: (rows: StreamEntry[]) => string[];
};
interface StreamEntry { id: string; ts: string | null; fields: Record<string, string> }
const streamMerge = stream.streamMerge;
const streamColumns = stream.streamColumns;

const entry = (id: string, fields: Record<string, string>): StreamEntry => {
  return { id: id, ts: null, fields: fields };
};

// docs/45 S2: the merge the Load-earlier walk runs — strictly older pages PREPEND, seams
// dedupe by id, and the walk's honest empty last page is a no-op.
describe("streamMerge", () => {
  it("prepends an older page ahead of the cached newest window", () => {
    const rows = [entry("5-0", { a: "1" }), entry("4-0", { a: "2" })];
    const page = [entry("3-0", { a: "3" }), entry("2-0", { a: "4" })];
    expect(streamMerge(rows, page).map((e: StreamEntry) => e.id)).toEqual(["3-0", "2-0", "5-0", "4-0"]);
  });

  it("never repeats a row at a seam overlap", () => {
    const rows = [entry("5-0", { a: "1" }), entry("4-0", { a: "2" })];
    const page = [entry("4-0", { a: "2" }), entry("3-0", { a: "3" })];
    expect(streamMerge(rows, page).map((e: StreamEntry) => e.id)).toEqual(["3-0", "5-0", "4-0"]);
  });

  it("merges the walk's empty last page as a no-op", () => {
    const rows = [entry("5-0", { a: "1" })];
    expect(streamMerge(rows, [])).toEqual(rows);
  });
});

// docs/45 S2/D3: the columns derivation — the field union in first-seen-scanning-
// newest-first order, the same algorithm the server runs per window, re-run over the
// merged rows so an older page's new field lands at the tail.
describe("streamColumns", () => {
  it("unions ragged fields newest-first, never alphabetized", () => {
    // The ragged seed's five entries, newest first: {a,c,d} / {c} / {b,c} / {a,b} / {a}.
    const rows = [
      entry("4-0", { a: "1", c: "3", d: "4" }),
      entry("3-0", { c: "5" }),
      entry("2-0", { b: "6", c: "7" }),
      entry("1-0", { a: "8", b: "9" }),
      entry("0-0", { a: "0" }),
    ];
    expect(streamColumns(rows)).toEqual(["a", "c", "d", "b"]);
  });

  it("keeps the first window's order and appends what an older page introduces", () => {
    const rows = [entry("9-0", { sym: "AAA", px: "1" }), entry("8-0", { qty: "2", sym: "BBB" })];
    expect(streamColumns(rows)).toEqual(["sym", "px", "qty"]);
  });

  it("answers [] for no rows and skips entries with no fields", () => {
    expect(streamColumns([])).toEqual([]);
    expect(streamColumns([entry("1-0", {})])).toEqual([]);
  });
});

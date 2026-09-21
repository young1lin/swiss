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
Object.assign(globalThis, {
  document: doc, window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const cell = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-cell.ts")).href
) as { dbCellView: (value: unknown, colType: string | null | undefined) => {
  text: string | null; cls: string | null; title: string | null; href: string | null;
} };

// docs/22 W2.3 — the typed rendering of one cell value. Pure so every intent (NULL, numbers,
// booleans, URLs, folded JSON, binary) is pinned without a DOM; the grid painter is a thin
// wrapper over this.
describe("dbCellView", () => {
  it("keeps NULL as the painter's own path (text null, no classes)", () => {
    expect(cell.dbCellView(null, "int")).toEqual({ text: null, cls: null, title: null, href: null });
    expect(cell.dbCellView(undefined, "text")).toEqual({ text: null, cls: null, title: null, href: null });
  });

  it("right-aligns numbers — a JS number by itself, a numeric column even when the driver sent a string", () => {
    expect(cell.dbCellView(42, null).cls).toBe("db-num");
    expect(cell.dbCellView("9223372036854775807", "bigint").cls).toBe("db-num");
    expect(cell.dbCellView("9223372036854775807", "bigint").text).toBe("9223372036854775807");
    expect(cell.dbCellView("12.50", "decimal").cls).toBe("db-num");
    expect(cell.dbCellView(1, "double precision").cls).toBe("db-num");
    // int64 beyond 2^53 must survive verbatim — never through a JS number round-trip.
    expect(cell.dbCellView("9223372036854775807", "bigint").text).not.toContain("808");
  });

  it("leaves text columns unclassed", () => {
    expect(cell.dbCellView("hello", "varchar").cls).toBeNull();
    expect(cell.dbCellView("hello", "character varying").cls).toBeNull();
  });

  it("words booleans as TRUE/FALSE — column-typed or a literal boolean", () => {
    expect(cell.dbCellView(true, "boolean").text).toBe("TRUE");
    expect(cell.dbCellView(false, "boolean").text).toBe("FALSE");
    expect(cell.dbCellView(1, "boolean").text).toBe("TRUE");
    expect(cell.dbCellView("0", "boolean").text).toBe("FALSE");
    // A query-result grid has no column types; a literal boolean still words itself.
    expect(cell.dbCellView(true, null).text).toBe("TRUE");
  });

  it("keeps MySQL tinyint columns numeric (information_schema never says bool for them)", () => {
    expect(cell.dbCellView(1, "tinyint").cls).toBe("db-num");
    expect(cell.dbCellView(1, "tinyint").text).toBe("1");
  });

  it("makes http(s) values clickable links with the noopener guard, title carrying the full URL", () => {
    var v = cell.dbCellView("https://example.com/a/very/long/path", "text");
    expect(v.href).toBe("https://example.com/a/very/long/path");
    expect(v.text).toBe("https://example.com/a/very/long/path");
    expect(v.title).toBe("https://example.com/a/very/long/path");
    expect(cell.dbCellView("http://127.0.0.1:19999/x", null).href).toBe("http://127.0.0.1:19999/x");
    // Not a URL the browser should open from a data grid.
    expect(cell.dbCellView("ftp://example.com/x", null).href).toBeNull();
    expect(cell.dbCellView("httpsonly-prefix-spoof", null).href).toBeNull();
  });

  it("folds JSON over 100 characters into (JSON) with the full text as the title", () => {
    var long = '{"a":' + Array(30).fill('"xxxxxxxx"').join(",") + "}";
    expect(long.length).toBeGreaterThan(100);
    var v = cell.dbCellView(long, "json");
    expect(v.text).toBe("(JSON)");
    expect(v.title).toBe(long);
    expect(v.cls).toBe("db-fold");
    // A parsed object (the adapters parse JSON columns) folds the same way, pretty-printed.
    var obj = { rows: Array(20).fill({ id: 123456, name: "something" }) };
    var ov = cell.dbCellView(obj, "jsonb");
    expect(ov.text).toBe("(JSON)");
    expect(ov.title).toBe(JSON.stringify(obj, null, 2));
  });

  it("shows short JSON-like values in full", () => {
    var short = '{"a":1,"b":2}';
    expect(short.length).toBeLessThanOrEqual(100);
    var v = cell.dbCellView(short, "json");
    expect(v.text).toBe(short);
    expect(v.title).toBeNull();
    expect(v.cls).toBeNull(); // "json" is not a numeric column
  });

  it("summarizes binary columns as a byte count (the value sheet owns the content)", () => {
    expect(cell.dbCellView("abc", "bytea").text).toBe("binary, 3 bytes");
    expect(cell.dbCellView("abc", "blob").text).toBe("binary, 3 bytes");
    expect(cell.dbCellView("abc", "varbinary").cls).toBe("db-fold");
    // Multi-byte characters count as their UTF-8 bytes, not UTF-16 units.
    expect(cell.dbCellView("\u00e9\u00e9", "bytea").text).toBe("binary, 4 bytes");
  });

  it("does not fold a long plain-text value (only JSON collapses)", () => {
    var long = "x".repeat(500);
    var v = cell.dbCellView(long, "text");
    expect(v.text).toBe(long);
    expect(v.title).toBeNull();
    expect(v.cls).toBeNull();
  });
});

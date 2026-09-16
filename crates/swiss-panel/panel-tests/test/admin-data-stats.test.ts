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

// Same DOM-stub technique as admin-data-grep.test.ts: the panel ships browser ES modules, so
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
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const mod = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-sql.js")).href
) as { dbStatsSql: (d: string, s: string, t: string, c: string, k: string) => string };

// docs/22 W1.4: the header right-click stats statements — dialect-correct quoting through the
// identifier whitelist, never a bare splice, and refusal of anything not a bare word.
describe("dbStatsSql", () => {
  it("builds the top-values shape with LIMIT 50", () => {
    expect(mod.dbStatsSql("mysql", "", "users", "roles", "dist")).toBe(
      "SELECT `roles`, COUNT(*) AS count\n" +
      "FROM `users`\n" +
      "GROUP BY 1\n" +
      "ORDER BY 2 DESC\n" +
      "LIMIT 50");
  });

  it("quotes postgres identifiers double and carries the schema", () => {
    const sql = mod.dbStatsSql("pg", "app", "events", "user_id", "dist");
    expect(sql.startsWith('SELECT "user_id", COUNT(*) AS count')).toBe(true);
    expect(sql).toContain('FROM "app"."events"');
  });

  it("numeric stats count, min, max and average the column", () => {
    const sql = mod.dbStatsSql("pg", "", "events", "amount", "num");
    expect(sql).toContain('COUNT("amount") AS count');
    expect(sql).toContain('MIN("amount") AS min');
    expect(sql).toContain('MAX("amount") AS max');
    expect(sql).toContain('AVG("amount") AS avg');
    expect(sql).not.toContain("GROUP BY");
  });

  it("refuses a column name that is not a bare identifier", () => {
    expect(() => mod.dbStatsSql("mysql", "", "users", 'x"; DROP TABLE users', "dist")).toThrow();
  });
});

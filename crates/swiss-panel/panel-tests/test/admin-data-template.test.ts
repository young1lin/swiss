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
) as { dbTemplateSql: (k: string, d: string, s: string, t: string, c: string[], pk: string[]) => string };

// docs/22 W1.10: the table menu's templates — every column listed, identifiers quoted through
// the whitelist, value positions as ? placeholders behind one comment hint.
describe("dbTemplateSql", () => {
  it("SELECT lists every column and keys the WHERE", () => {
    const sql = mod.dbTemplateSql("select", "pg", "app", "users", ["id", "order", "name"], ["id"]);
    expect(sql).toBe(
      "-- replace each ? with a value before running\n" +
      'SELECT "id", "order", "name"\n' +
      'FROM "app"."users"\n' +
      'WHERE "id" = ?;');
  });

  it("INSERT quotes per dialect and fills one ? per column", () => {
    const sql = mod.dbTemplateSql("insert", "mysql", "", "users", ["id", "name"], ["id"]);
    expect(sql).toBe(
      "-- replace each ? with a value before running\n" +
      "INSERT INTO `users` (`id`, `name`)\n" +
      "VALUES (?, ?);");
  });

  it("UPDATE sets non-key columns and keys the WHERE", () => {
    const sql = mod.dbTemplateSql("update", "pg", "", "users", ["id", "name"], ["id"]);
    expect(sql).toContain('SET "name" = ?');
    expect(sql).toContain('WHERE "id" = ?');
    expect(sql).not.toContain('SET "id"');
  });

  it("DELETE keys the WHERE; a keyword column stays quoted", () => {
    const sql = mod.dbTemplateSql("delete", "pg", "app", "order", ["id"], ["id"]);
    expect(sql).toBe(
      "-- replace each ? with a value before running\n" +
      'DELETE FROM "app"."order"\n' +
      'WHERE "id" = ?;');
  });

  it("refuses an identifier outside the whitelist", () => {
    expect(() => mod.dbTemplateSql("select", "pg", "", "users", ['x"; DROP'], ["id"])).toThrow();
  });
});

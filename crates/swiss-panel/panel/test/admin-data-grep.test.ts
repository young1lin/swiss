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

// Same DOM-stub technique as admin-data-sql.test.ts: the panel ships browser ES modules, so
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

const view = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-view.ts")).href
) as { dbFilterMatches: (tokens: string, name: string) => boolean };

// SPEC §data.browse: the sidebar grep grammar — comma terms AND, | inside a term OR, * wildcard,
// case-insensitive, and a bare term keeps the substring behaviour the list always had.
describe("dbFilterMatches", () => {
  it("empty tokens match everything", () => {
    expect(view.dbFilterMatches("", "users")).toBe(true);
    expect(view.dbFilterMatches(" , ", "users")).toBe(true);
  });

  it("a bare term is the substring match the list always had", () => {
    expect(view.dbFilterMatches("user", "app_users")).toBe(true);
    expect(view.dbFilterMatches("user", "app_users_settings")).toBe(true);
    expect(view.dbFilterMatches("user", "accounts")).toBe(false);
  });

  it("comma-separated terms AND together", () => {
    expect(view.dbFilterMatches("user, set", "users_settings")).toBe(true);
    expect(view.dbFilterMatches("user, nope", "users_settings")).toBe(false);
  });

  it("| inside a term is OR", () => {
    expect(view.dbFilterMatches("user*|account", "account_log")).toBe(true);
    expect(view.dbFilterMatches("user*|account", "user_events")).toBe(true);
    expect(view.dbFilterMatches("user*|account", "sessions")).toBe(false);
  });

  it("* is a wildcard, case-insensitive", () => {
    expect(view.dbFilterMatches("user*", "USER_EVENTS")).toBe(true);
    expect(view.dbFilterMatches("*_log", "audit_log")).toBe(true);
    expect(view.dbFilterMatches("user*", "sessions")).toBe(false);
    expect(view.dbFilterMatches("a*b*c", "a-x-b-y-c")).toBe(true);
  });

  it("regex metacharacters in a term stay literal", () => {
    expect(view.dbFilterMatches("a.b", "a.b")).toBe(true);
    expect(view.dbFilterMatches("a.b", "axb")).toBe(false);
    expect(view.dbFilterMatches("(x)+", "(x)+")).toBe(true);
  });
});

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
  confirm: () => true, alert: () => {}, prompt: () => "",
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const act = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-activity.js")).href
) as { dbActivityDuration: (secs: number) => string };

// docs/22 W3.2: the Activity table's duration column — one compact form across the whole
// range a session can run for, and junk reads as zero instead of NaN-in-a-table.
describe("dbActivityDuration", () => {
  it("reads seconds under a minute as plain seconds", () => {
    expect(act.dbActivityDuration(0)).toBe("0s");
    expect(act.dbActivityDuration(42)).toBe("42s");
  });

  it("reads minutes with seconds, then hours with padded minutes", () => {
    expect(act.dbActivityDuration(60)).toBe("1m 0s");
    expect(act.dbActivityDuration(72)).toBe("1m 12s");
    expect(act.dbActivityDuration(3599)).toBe("59m 59s");
    expect(act.dbActivityDuration(3600)).toBe("1h 00m");
    expect(act.dbActivityDuration(4320)).toBe("1h 12m");
  });

  it("reads days with padded hours", () => {
    expect(act.dbActivityDuration(86400)).toBe("1d 00h");
    expect(act.dbActivityDuration(93720)).toBe("1d 02h");
  });

  it("clamps junk instead of printing NaN in a column", () => {
    expect(act.dbActivityDuration(-5)).toBe("0s");
    expect(act.dbActivityDuration(NaN)).toBe("0s");
    expect(act.dbActivityDuration(undefined as unknown as number)).toBe("0s");
  });
});

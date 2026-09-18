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
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-sql.ts")).href
) as { dbSubqueryAt: (text: string, caret: number | null) => string };

// docs/22 W1.8: blank lines split the console into blocks; the run covers the block the caret
// is in. A gap caret belongs to the block after it, no caret means the end, and a text without
// blank lines is one block — exactly the whole box, the old behaviour.
describe("dbSubqueryAt", () => {
  const script = "SELECT 1;\n\nSELECT 2;\n\nSELECT 3;";

  it("returns the whole text when there are no blank lines", () => {
    expect(mod.dbSubqueryAt("SELECT 1;\nSELECT 2;", 4)).toBe("SELECT 1;\nSELECT 2;");
  });

  it("returns the block the caret is in", () => {
    expect(mod.dbSubqueryAt(script, "SELECT 1;\n\n".length + 4)).toBe("SELECT 2;");
    expect(mod.dbSubqueryAt(script, 3)).toBe("SELECT 1;");
  });

  it("a caret inside the blank gap belongs to the block after it", () => {
    expect(mod.dbSubqueryAt(script, "SELECT 1;".length + 1)).toBe("SELECT 2;");
  });

  it("a missing caret means the end of the text", () => {
    expect(mod.dbSubqueryAt(script, null)).toBe("SELECT 3;");
  });

  it("a whitespace-only line is a separator too", () => {
    const ws = "SELECT 1;\n   \nSELECT 2;";
    expect(mod.dbSubqueryAt(ws, "SELECT 1;\n   \n".length)).toBe("SELECT 2;");
  });

  it("blocks keep their own inner newlines", () => {
    const two = "SELECT 1\nFROM t;\n\nSELECT 2";
    expect(mod.dbSubqueryAt(two, 0)).toBe("SELECT 1\nFROM t;");
  });
});

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

/* docs/46 G1 (U2) - the library's boundary, zero tolerance. src/ui/** imports ../h.js,
   ../i18n.js and its own siblings, nothing else: no api, no state, no view. That is what lets
   the gallery render every component with made-up data and nothing else loaded, and what
   keeps a component from quietly growing a fetch. A mechanism that needs more (a menu that
   closes the others, a sheet the shell owns) is either rewritten to take what it needs as
   arguments or stays outside ui/ and builds on it.

   The same gate pins the other half of the contract: ui/index.ts re-exports every module in
   ui/, so the barrel is the whole library and G6's "every export is in the gallery" covers
   every file. */
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const UI = join(import.meta.dirname, "..", "src", "ui");
const ALLOWED_PARENT = new Set(["../h.js", "../i18n.js"]);

function uiFiles(): string[] {
  return readdirSync(UI).filter((f) => f.endsWith(".ts")).sort();
}

/** Every module specifier a file names: static import / export-from and dynamic import(). */
function specifiersOf(src: string): string[] {
  const out: string[] = [];
  const re = /(?:^|\n)\s*(?:import|export)\b[^;]*?\bfrom\s*["']([^"']+)["']|(?:^|\n)\s*import\s*["']([^"']+)["']|\bimport\s*\(\s*["']([^"']+)["']\s*\)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src)) !== null) out.push(m[1] || m[2] || m[3]);
  return out;
}

describe("specifiersOf", () => {
  it("finds static, type-only, side-effect, re-export and dynamic imports", () => {
    const src = [
      'import { h } from "../h.js";',
      'import type { AttrMap } from "../h.js";',
      'import "../boot.js";',
      'export { tag } from "./status.js";',
      'export type { RowOpts } from "./row.js";',
      'const m = await import("../api.js");',
      "import {\n  a,\n  b,\n} from '../util.js';",
    ].join("\n");
    expect(specifiersOf(src)).toEqual(["../h.js", "../h.js", "../boot.js", "./status.js", "./row.js", "../api.js", "../util.js"]);
  });
});

describe("docs/46 G1 - src/ui imports only ../h.js, ../i18n.js and ./*", () => {
  it("the library exists", () => {
    expect(uiFiles().length).toBeGreaterThan(5);
  });

  for (const f of uiFiles()) {
    it(f, () => {
      const bad = specifiersOf(readFileSync(join(UI, f), "utf8")).filter((s) => {
        if (ALLOWED_PARENT.has(s)) return false;
        // A sibling: one path segment, no climbing - "./x.js", never "./../api.js".
        return !/^\.\/[a-z0-9-]+\.js$/.test(s);
      });
      expect(bad, f + " reaches outside the library").toEqual([]);
    });
  }

  it("every sibling import names a module that exists", () => {
    const missing: string[] = [];
    for (const f of uiFiles()) {
      for (const s of specifiersOf(readFileSync(join(UI, f), "utf8"))) {
        if (s.startsWith("./") && !existsSync(join(UI, s.slice(2).replace(/\.js$/, ".ts")))) missing.push(f + " -> " + s);
      }
    }
    expect(missing).toEqual([]);
  });
});

describe("docs/46 - ui/index.ts is the whole library", () => {
  it("re-exports every module in ui/", () => {
    const index = readFileSync(join(UI, "index.ts"), "utf8");
    const named = new Set(specifiersOf(index).map((s) => s.replace(/^\.\//, "").replace(/\.js$/, ".ts")));
    const orphans = uiFiles().filter((f) => f !== "index.ts" && !named.has(f));
    expect(orphans, "a ui/ module the barrel does not export").toEqual([]);
  });
});

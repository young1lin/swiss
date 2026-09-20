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

/* The completeness gate (docs/38 L10a), first of the two machine gates: every tr/tk
   literal key and every trn "other" key must have a zh entry, every zh entry must be
   reachable from a literal, and no entry may equal its key (an untranslated copy is a bug
   dressed as coverage). Counted with the real parser, not a regex: only CallExpressions
   whose callee is exactly tr/tk/trn with literal arguments count, and a dynamic first
   argument (a tk()-marked table painted through tr(x) at call time) is legal by design —
   that half stays on review. */

import * as fs from "node:fs";
import * as path from "node:path";
import ts from "typescript";
import { describe, expect, it } from "vitest";

const here = import.meta.dirname;
const srcDir = path.resolve(here, "../src");

const { listSources } = await import(new URL("../build.mjs", import.meta.url).href) as {
  listSources: () => string[];
};
const zh: Record<string, string> = (await import("../src/locales/zh.js")).default;

/* key -> "file" of its first literal sighting, so a failure names where to look. */
function collect(): Map<string, string> {
  const used = new Map<string, string>();
  for (const rel of listSources()) {
    const text = fs.readFileSync(path.join(srcDir, rel), "utf8");
    const sf = ts.createSourceFile(rel, text, ts.ScriptTarget.Latest, true);
    const visit = (node: ts.Node): void => {
      if (ts.isCallExpression(node)) {
        const name = ts.isIdentifier(node.expression) ? node.expression.text : "";
        const lit = (i: number): string | null => {
          const a = node.arguments[i];
          return a && (ts.isStringLiteral(a) || ts.isNoSubstitutionTemplateLiteral(a)) ? a.text : null;
        };
        if (name === "tr" || name === "tk") {
          const key = lit(0);
          if (key !== null && !used.has(key)) used.set(key, rel);
        } else if (name === "trn") {
          // The "other" form is the key every language needs; "one" is English-only (L3).
          const key = lit(2);
          if (key !== null && !used.has(key)) used.set(key, rel);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(sf);
  }
  return used;
}

const has = (o: Record<string, string>, k: string): boolean => Object.prototype.hasOwnProperty.call(o, k);

describe("i18n dictionary completeness (docs/38 L10a)", () => {
  const used = collect();

  it("every literal key has a zh entry", () => {
    expect(used.size, "the scanner found nothing - it is broken, not the tree clean").toBeGreaterThan(0);
    const missing = [...used.entries()].filter(([k]) => !has(zh, k)).map(([k, where]) => k + " (used in " + where + ")");
    expect(missing, "missing zh: …").toEqual([]);
  });

  it("no zh entry is orphaned", () => {
    const orphans = Object.keys(zh).filter((k) => !used.has(k));
    expect(orphans, "orphan zh: …").toEqual([]);
  });

  it("no zh entry forgot to translate (value === key)", () => {
    const copied = Object.entries(zh).filter(([k, v]) => v === k).map(([k]) => k);
    expect(copied, "untranslated copy: …").toEqual([]);
  });
});

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

/* The docs/38 L10b coverage ratchet. The spec names an eslint no-restricted-syntax
 * warn, but that rule's single severity slot in the src block is held by R5's
 * innerHTML error (one config per rule per file - a second block would replace it,
 * not add to it), so the ratchet lands here in the house non-null-ratchet shape
 * instead: an AST count of bare English literals in the spec's VISIBLE positions,
 * frozen per file, allowed only to shrink. The warn count doubles as the docs/38
 * §7 "remaining bare literals" stage number; at I10 the frozen rows go to zero and
 * growth fails the suite - the same gate an eslint error would be.
 *
 * Positions selected (docs/38 §5.1): h() children (3rd argument on, tag code/kbd/pre
 * exempt), h() props title/placeholder/aria.label, the first argument of
 * toast/confirm/prompt/say, and emptyNode's title/hint/action values. A literal
 * counts when it carries two consecutive letters - " · ", "—" and "…" are layout,
 * not copy. tr/trn/tk arguments sit in none of these positions, which is how wrapped
 * copy stays exempt. */
import * as fs from "node:fs";
import * as path from "node:path";
import ts from "typescript";
import { describe, expect, it } from "vitest";

/* Frozen at the I1 commit (docs/38 stage table): the shell files are swept to their
 * deliberate survivors - add-sheet's two example-identifier placeholders ("git-mcp",
 * "prod", docs/38 §8 keeps identifiers untranslated) - and every remaining row is a
 * view file a later stage owns. Keys are paths relative to src/; the tree has
 * basename twins (jobs.ts and views/jobs.ts). */
const FROZEN: Record<string, number> = {
  "add-sheet.ts": 2,
  "data-activity.ts": 2,
  "data-browsers.ts": 9,
  "data-cell.ts": 6,
  "data-csv.ts": 14,
  "data-ddl.ts": 12,
  "data-edit.ts": 3,
  "data-filters.ts": 10,
  "data-form.ts": 7,
  "data-grid.ts": 23,
  "data-sql.ts": 11,
  "data-structure.ts": 2,
  "data-value.ts": 3,
  "data-view.ts": 20,
  "detail.ts": 9,
  "jobs.ts": 50,
  "logs.ts": 33,
  "run-history.ts": 38,
  "run.ts": 7,
  "traffic.ts": 20,
  "tunnel-sheets.ts": 57,
  "tunnels.ts": 6,
  "views/jobs.ts": 2,
  "views/plugins.ts": 11,
  "views/remote-runs.ts": 18,
  "views/remote.ts": 13,
  "views/secrets.ts": 14,
  "views/system.ts": 13,
  "views/terminal-settings.ts": 7,
  "views/terminal.ts": 26,
  "views/tokens.ts": 16,
};

const QUALIFIES = /[A-Za-z]{2}/;
const srcDir = path.resolve(import.meta.dirname, "../src");

function srcFiles(): Set<string> {
  const out = new Set<string>();
  const walkDir = (rel: string): void => {
    for (const ent of fs.readdirSync(path.join(srcDir, rel), { withFileTypes: true })) {
      const child = rel ? rel + "/" + ent.name : ent.name;
      if (ent.isDirectory()) walkDir(child);
      else if (ent.isFile() && ent.name.endsWith(".ts") && !ent.name.endsWith(".d.ts")) out.add(child);
    }
  };
  walkDir("");
  return out;
}

function propValue(obj: ts.ObjectLiteralExpression, key: string): ts.Expression | null {
  for (const p of obj.properties) {
    if (ts.isPropertyAssignment(p) && ts.isIdentifier(p.name) && p.name.text === key) return p.initializer;
  }
  return null;
}

function countBare(rel: string): number {
  const sf = ts.createSourceFile(rel, fs.readFileSync(path.join(srcDir, rel), "utf8"), ts.ScriptTarget.Latest, true);
  let n = 0;
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node)) {
      const name = ts.isIdentifier(node.expression) ? node.expression.text : "";
      if (name === "h") {
        const tagArg = node.arguments[0];
        const tag = tagArg !== undefined && ts.isStringLiteral(tagArg) ? tagArg.text : "";
        const exempt = tag === "code" || tag === "kbd" || tag === "pre";
        for (let i = 2; i < node.arguments.length; i++) {
          const a = node.arguments[i];
          if (a !== undefined && ts.isStringLiteral(a) && !exempt && QUALIFIES.test(a.text)) n++;
        }
        const props = node.arguments[1];
        if (props !== undefined && ts.isObjectLiteralExpression(props)) {
          for (const key of ["title", "placeholder"]) {
            const v = propValue(props, key);
            if (v && ts.isStringLiteral(v) && QUALIFIES.test(v.text)) n++;
          }
          const aria = propValue(props, "aria");
          if (aria && ts.isObjectLiteralExpression(aria)) {
            const v = propValue(aria, "label");
            if (v && ts.isStringLiteral(v) && QUALIFIES.test(v.text)) n++;
          }
        }
      } else if (name === "toast" || name === "confirm" || name === "prompt" || name === "say") {
        const a = node.arguments[0];
        if (a !== undefined && ts.isStringLiteral(a) && QUALIFIES.test(a.text)) n++;
      } else if (name === "emptyNode") {
        const a = node.arguments[0];
        if (a !== undefined && ts.isObjectLiteralExpression(a)) {
          for (const key of ["title", "hint", "action"]) {
            const v = propValue(a, key);
            if (v && ts.isStringLiteral(v) && QUALIFIES.test(v.text)) n++;
          }
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return n;
}

describe("docs/38 L10b bare-literal coverage ratchet", () => {
  it("every src file's bare-visible count is at or below its frozen row", () => {
    const failures: string[] = [];
    for (const rel of srcFiles()) {
      const actual = countBare(rel);
      const frozen = FROZEN[rel] ?? 0;
      if (actual > frozen) failures.push(rel + ": " + actual + " > " + frozen);
    }
    expect(failures, "visible copy may only move under tr() - counts may only shrink").toEqual([]);
  });

  it("no frozen row points at a file that is already clean", () => {
    const present = srcFiles();
    const stale: string[] = [];
    for (const rel of Object.keys(FROZEN)) {
      if (!present.has(rel)) { stale.push(rel + " (deleted)"); continue; }
      if (countBare(rel) === 0) stale.push(rel + " (now 0 - remove the row; I10 removes the map)");
    }
    expect(stale).toEqual([]);
  });

  it("reports the docs/38 §7 stage number (the burn-down total)", () => {
    let total = 0;
    for (const rel of srcFiles()) total += countBare(rel);
    // The frozen map's own sum - kept in lockstep so a row edit cannot lose count.
    let frozenTotal = 0;
    for (const v of Object.values(FROZEN)) frozenTotal += v;
    expect(total, "total " + total + " bare literals remain; frozen rows sum to " + frozenTotal).toBeLessThanOrEqual(frozenTotal);
  });
});

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

/* docs/46 G5 - the library's shapes are drawn by the library. A class ui.css owns (the base
   class of a subject it styles - the same identity G2 uses) written into markup OUTSIDE
   src/ui/ is a page hand-drawing a component: an h("div", { class: "lrow" }) is a second row
   implementation the day it lands, and the drift docs/46 §0.2 measured (four rows, three
   segs) came from exactly that. Counted: every token of every string literal inside an h()
   call's `class` value and inside el()'s class argument, per file.

   A ratchet in the non-null-ratchet shape: frozen at P1b-3, a file may only go down, and a
   commit that lowers a count lowers its row in the same commit (a row above the actual count
   fails too, so the table never lags the tree). A file with no row is held at 0 - the gallery
   and its scenes are, from their first commit. A migrated view's target is 0 (docs/46 P9). */
import * as fs from "node:fs";
import * as path from "node:path";
import ts from "typescript";
import { describe, expect, it } from "vitest";
import { uiOwnedClasses } from "./styles.js";

/* Frozen at docs/46 P1b-3: 623 classes in 35 files. P2-1: pane.ts 19 -> 0 (the MCP resource head).
   P2-2: logs.ts 85 -> 50 (the call log is the event list; Tools/Resources/Prompts are P2-3). */
const FROZEN: Record<string, number> = {
  "add-sheet.ts": 10,
  "data-browsers.ts": 12,
  "data-cell.ts": 11,
  "data-csv.ts": 12,
  "data-ddl.ts": 19,
  "data-filters.ts": 1,
  "data-form.ts": 5,
  "data-grid.ts": 16,
  "data-sql.ts": 6,
  "data-stream.ts": 6,
  "data-structure.ts": 2,
  "data-tabs.ts": 1,
  "data-value.ts": 6,
  "data-view.ts": 3,
  "fields.ts": 6,
  "groups.ts": 1,
  "jobs.ts": 68,
  "logs.ts": 50,
  "page-registry.ts": 3,
  "polling.ts": 12,
  "run-history.ts": 47,
  "run.ts": 24,
  "traffic.ts": 32,
  "tunnel-sheets.ts": 61,
  "tunnels.ts": 8,
  "views/jobs.ts": 2,
  "views/plugins.ts": 13,
  "views/remote-runs.ts": 30,
  "views/remote.ts": 25,
  "views/secrets.ts": 18,
  "views/system.ts": 17,
  "views/terminal-settings.ts": 11,
  "views/terminal.ts": 7,
  "views/tokens.ts": 24,
};

const srcDir = path.resolve(import.meta.dirname, "../src");

function srcFiles(): string[] {
  const out: string[] = [];
  const walk = (rel: string): void => {
    for (const ent of fs.readdirSync(path.join(srcDir, rel), { withFileTypes: true })) {
      const child = rel ? rel + "/" + ent.name : ent.name;
      if (ent.isDirectory()) { if (child !== "ui" && child !== "locales" && child !== "types") walk(child); }
      else if (ent.isFile() && ent.name.endsWith(".ts") && !ent.name.endsWith(".d.ts")) out.push(child);
    }
  };
  walk("");
  return out.sort();
}

/** Every string a class expression can produce a token from: literals, template pieces, both
 *  arms of a conditional, both sides of a +. Identifiers contribute nothing - a class held in a
 *  variable is invisible here, which is why the count is a floor and the review is the rest. */
function literalsIn(n: ts.Node, out: string[]): void {
  if (ts.isStringLiteral(n) || ts.isNoSubstitutionTemplateLiteral(n)) { out.push(n.text); return; }
  if (ts.isTemplateExpression(n)) {
    out.push(n.head.text);
    n.templateSpans.forEach((s) => { out.push(s.literal.text); literalsIn(s.expression, out); });
    return;
  }
  ts.forEachChild(n, (c) => { literalsIn(c, out); });
}

export function uiClassTokens(rel: string, owned: Set<string>): string[] {
  const text = fs.readFileSync(path.join(srcDir, rel), "utf8");
  const sf = ts.createSourceFile(rel, text, ts.ScriptTarget.Latest, true);
  const hits: string[] = [];
  const count = (expr: ts.Node): void => {
    const lits: string[] = [];
    literalsIn(expr, lits);
    for (const l of lits) for (const tok of l.split(/\s+/)) if (tok && owned.has(tok)) hits.push(tok);
  };
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression)) {
      const name = node.expression.text;
      if (name === "h") {
        const props = node.arguments[1];
        if (props && ts.isObjectLiteralExpression(props)) {
          for (const p of props.properties) {
            if (ts.isPropertyAssignment(p) && (ts.isIdentifier(p.name) || ts.isStringLiteral(p.name)) && p.name.text === "class") count(p.initializer);
          }
        }
      } else if (name === "el") {
        const cls = node.arguments[1];
        if (cls) count(cls);
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return hits;
}

describe("docs/46 G5 - ui.css classes are drawn by src/ui/ only", () => {
  const owned = uiOwnedClasses();

  it("the counter sees what it must (a clean pass must not be a blind pass)", () => {
    // Known markup in the tree today: the MCP pane builds its own .btn words.
    expect(owned.has("btn") && owned.has("lrow") && owned.has("pane")).toBe(true);
    const total = srcFiles().reduce((n, f) => n + uiClassTokens(f, owned).length, 0);
    expect(total).toBeGreaterThan(50);
  });

  it("every file is at its frozen count - lower only, and lower the row with it", () => {
    const failures: string[] = [];
    for (const rel of srcFiles()) {
      const hits = uiClassTokens(rel, owned);
      const frozen = FROZEN[rel] ?? 0;
      if (hits.length > frozen) {
        failures.push(rel + ": " + hits.length + " > " + frozen + " - draw it with ui/ (" + Array.from(new Set(hits)).join(", ") + ")");
      } else if (hits.length < frozen) {
        failures.push(rel + ": " + hits.length + " < " + frozen + " - it went down: set the row to " + hits.length);
      }
    }
    expect(failures).toEqual([]);
  });

  it("no row names a file that is gone", () => {
    const present = new Set(srcFiles());
    expect(Object.keys(FROZEN).filter((f) => !present.has(f))).toEqual([]);
  });

  it("the gallery and its scenes draw nothing by hand", () => {
    expect(uiClassTokens("ui-gallery.ts", owned)).toEqual([]);
    expect(uiClassTokens("ui-scenes.ts", owned)).toEqual([]);
  });
});

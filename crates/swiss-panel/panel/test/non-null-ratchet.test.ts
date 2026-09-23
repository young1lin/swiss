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

/* The docs/37 R2 assertion ratchet. no-non-null-assertion is an eslint error, but 33
 * src files still carry survivors of the 1,193 the migration started from; those files sit
 * in the eslint override block at "warn" so the tree stays green, and THIS test is what
 * makes the budget real: each file's count is frozen below and may only shrink. When a
 * file reaches zero, delete its row here AND drop it from the eslint override block - the
 * ratchet then protects it at full error strength.
 *
 * Counted with the real parser (typescript is a devDependency), not a regex: `! ==`,
 * `!==` and `!foo` must not be counted, and \u0021 escapes must be. */
import * as fs from "node:fs";
import * as path from "node:path";
import ts from "typescript";
import { describe, expect, it } from "vitest";

/* Frozen at R2 commit time (docs/37 M5) at 326, of which 121 were the single boundary
 * assertion a function carried when it opened the Data pane (const d_ = d!). R4's db slice
 * collected that debt: db-state.ts owns a record that is never null, so the boundary had
 * nothing left to assert and the thirteen data-* files fell from 229 to 113 - 207 total.
 * What survives is property-level (d.conn!, e.dataTransfer!, regex groups); the rest of it
 * goes with R5's rendering pass. R4's last slice took detail.ts 9 -> 8 and left 206.
 * Keys are paths relative to src/ - the tree has basename twins (jobs.ts and views/jobs.ts)
 * and a basename map would silently count the wrong one. */
const FROZEN: Record<string, number> = {
  "add-sheet.ts": 1,
  "menu.ts": 1,
  "sidebar.ts": 1,
  "views/secrets.ts": 1,
  "views/tokens.ts": 6,
  "detail.ts": 8,
  "main.ts": 1,
  "page-core.ts": 2,
  "page-registry.ts": 5,
  "pane.ts": 4,
  "run-history.ts": 3,
  "tunnel-sheets.ts": 10,
  "views/remote.ts": 1,
  "views/terminal-settings.ts": 1,
  "views/terminal.ts": 7,
  "data-activity.ts": 3,
  "data-browsers.ts": 16,
  "data-ddl.ts": 13,
  "data-form.ts": 7,
  "data-structure.ts": 4,
  "data-suggest.ts": 5,
  "groups.ts": 15,
  "jobs.ts": 16,
  "tunnels.ts": 7,
  "data-cell.ts": 6,
  "data-edit.ts": 5,
  "data-grid.ts": 17,
  "data-sql.ts": 15,
  "data-view.ts": 8,
  "data-csv.ts": 8,
};

const srcDir = path.resolve(import.meta.dirname, "../src");

/* Every .ts file under src/ (except ambient declarations), keyed by its path relative
 * to src/. */
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

function countAssertions(rel: string): number {
  const text = fs.readFileSync(path.join(srcDir, rel), "utf8");
  const sf = ts.createSourceFile(rel, text, ts.ScriptTarget.Latest, true);
  let n = 0;
  const visit = (node: ts.Node): void => {
    if (ts.isNonNullExpression(node)) n++;
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return n;
}

describe("docs/37 R2 non-null assertion ratchet", () => {
  it("every src file's assertion count is at or below its frozen row", () => {
    const failures: string[] = [];
    for (const rel of srcFiles()) {
      const actual = countAssertions(rel);
      const frozen = FROZEN[rel] ?? 0;
      if (actual > frozen) failures.push(rel + ": " + actual + " > " + frozen);
    }
    expect(failures, "counts may only shrink (update the map when one does)").toEqual([]);
  });

  it("no frozen row points at a file that is already clean", () => {
    const present = srcFiles();
    const stale: string[] = [];
    for (const rel of Object.keys(FROZEN)) {
      if (!present.has(rel)) { stale.push(rel + " (deleted)"); continue; }
      if (countAssertions(rel) === 0) stale.push(rel + " (now 0 - remove the row and the eslint override)");
    }
    expect(stale).toEqual([]);
  });
});

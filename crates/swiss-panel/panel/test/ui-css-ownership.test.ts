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

/* docs/46 G2 - the component layer owns its classes. ui.css defines every class a ui/
   component draws; views.css may lay a page out, but it may not restyle a library class. A
   views.css rule whose SUBJECT (the last compound of the selector) is a library element is a
   page reaching into a component - the drift docs/46 §0.2 measured, one "row" in four
   implementations. The count is a ratchet frozen at P1a-2: it may only go down, a page's
   migration lowers it in the same commit, and docs/46 P9 drives it to its floor.

   Identity is the compound's FIRST class, on both sides. ui.css owns the base class of every
   subject it styles (".menu button .ic" owns "ic"; ".pane > .wide > .pane-head" owns
   "pane-head", not the "pane" it merely sits in). A views.css subject violates when its base
   class is owned: ".db-bar .btn" restyles the button, while ".db-drawer.open" is the page's
   own drawer wearing a state modifier the library also happens to use. */
import { describe, expect, it } from "vitest";
import { classesOf, allClassesOf, baseClassOf, parseCss, specificity, subjectOf } from "./css-rules.js";
import { sheet, uiOwnedClasses } from "./styles.js";

const FROZEN_VIOLATIONS = 48; // docs/46 P1a-2

export function ownershipViolations(): string[] {
  const owned = uiOwnedClasses();
  const out: string[] = [];
  for (const r of parseCss(sheet("views.css"))) {
    for (const s of r.selectors) {
      const c = baseClassOf(s);
      if (c && owned.has(c)) out.push(s + "  (" + c + ")");
    }
  }
  return out;
}

describe("docs/46 G2 - views.css does not restyle the component layer", () => {
  it("the reader under the gate parses what the gates need", () => {
    const rules = parseCss("/* x { } */ .a .b > .c:hover, .d { color: red; font-weight: var(--w-body) } @media (x) { .e { margin: 0 } } @keyframes k { from { opacity: 0 } }");
    expect(rules.map((r) => r.selectors)).toEqual([[".a .b > .c:hover", ".d"], [".e"]]);
    expect(rules[1].at).toBe("@media (x)");
    expect(subjectOf(".a .b > .c.d:hover")).toBe(".c.d:hover");
    expect(classesOf(".c.d:not(.e)")).toEqual(["c", "d"]);
    expect(allClassesOf(".a:not(.b) .c")).toEqual(["a", "b", "c"]);
    expect(baseClassOf(".db-bar .btn.commit:hover")).toBe("btn");
    expect(baseClassOf(".menu button")).toBeUndefined();
    expect(specificity(".side-head .btn:hover")).toEqual([0, 3, 0]);
    expect(specificity("body.immersive #expandBtn")).toEqual([1, 1, 1]);
    expect(specificity(".x::before")).toEqual([0, 1, 1]);
  });

  it("the violation count is at or below its frozen value", () => {
    const v = ownershipViolations();
    expect(v.length, "views.css restyles a ui.css class - move the rule into ui.css or give the page its own class:\n" + v.join("\n")).toBeLessThanOrEqual(FROZEN_VIOLATIONS);
  });
});

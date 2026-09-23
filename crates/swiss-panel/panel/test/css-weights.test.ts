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

/* docs/46 G3 - four weights. The panel had nine font-weight values (400 450 500 550 560 590
   600 650 700), every one a literal; on a dark ground in Segoe UI that read as "everything is
   bold". Direction B names four roles in the token block - --w-body 400, --w-name 450,
   --w-emph 500, --w-title 600 - and every font-weight in every sheet uses one of them. */
import { describe, expect, it } from "vitest";
import { parseCss } from "./css-rules.js";
import { linkedSheets } from "./styles.js";

describe("docs/46 G3 - every font-weight is a --w-* token", () => {
  it("the four roles are defined once, in the light token block", () => {
    const defs: string[] = [];
    for (const s of linkedSheets()) {
      for (const r of parseCss(s.css)) for (const d of r.decls) {
        if (d.prop.startsWith("--w-")) defs.push(s.name + " " + r.selectors.join(",") + " " + d.prop + ": " + d.value);
      }
    }
    // Theme-independent: a dark override of a weight would be a second definition, not a role.
    expect(defs).toEqual([
      "base.css :root --w-body: 400",
      "base.css :root --w-name: 450",
      "base.css :root --w-emph: 500",
      "base.css :root --w-title: 600",
    ]);
  });

  it("no sheet writes a literal weight", () => {
    const bad: string[] = [];
    for (const s of linkedSheets()) {
      for (const r of parseCss(s.css)) for (const d of r.decls) {
        if (d.prop === "font-weight" && !/^var\(--w-(body|name|emph|title)\)$/.test(d.value)) bad.push(s.name + ": " + r.selectors.join(", ") + " { font-weight: " + d.value + " }");
        if (d.prop === "font" && /\b[1-9]00\b|\bbold\b/.test(d.value)) bad.push(s.name + ": " + r.selectors.join(", ") + " { font: " + d.value + " }");
      }
    }
    expect(bad, bad.join("\n")).toEqual([]);
  });
});

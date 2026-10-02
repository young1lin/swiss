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

/* SPEC §panel.ui - values come from tokens. There were once 247 px literals in views.css
   alone: every one is a size somebody picked by eye instead of reading the scale, and two
   pages that picked 5px and 6px for the same gap drift apart for good. This gate counts,
   per sheet, the literals a declaration writes OUTSIDE the token blocks (:root,
   :root[data-theme=...] and the code schemes' :root[data-code=...], SPEC §panel.code):

     px     any length in px except 0, 1px, 2px and -1px - a hairline, a focus ring and the
            one-pixel overlap are structure, not scale
     hex    #rgb / #rrggbb / #rrggbbaa
     rgb    rgb() / rgba() / hsl() / hsla()

   Percentages, em, ch, vh/vw, unitless numbers and everything inside a var() fallback-free
   token reference are fine. The count is a ratchet per file (the non-null-ratchet idiom):
   frozen at P1a-2, it may only go down, a commit that lowers it lowers the table in the same
   commit, and a sheet not in the table (the gallery's, a new page's) starts at 0. ui.css is
   headed for 0 - a component that needs a size the scale lacks adds a token first. */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { parseCss } from "./css-rules.js";
import { stylesDir } from "./styles.js";

/* Frozen at SPEC §panel.design. base.css keeps a floor (the scrollbar's 10px, the shell's
 * fixed widths); ui.css and views.css fall with every migrated page. */
const FROZEN: Record<string, number> = {
  "base.css": 22,
  "ui.css": 72, // P6-2: the legacy .row family (.row .k 152px) and the .call chevron (12px) went with Remote (P5: 74, the inline form's 260px wide-input basis)
  "views.css": 159, // P9: the restated library defaults (.dot's 6px) and the DDL grid's hand-sized remove (26px) went - glyph sizes ride the host's --ic knob now; P8-2 (cap lowered at P9): 170, Terminal off its --t-* palette (the defs lived outside :root and counted); P8-1: 210, Data's tab cards are the library's objTab - the .db-tab block went with its picked-by-eye sizes; P7-2: 220, the stream pill's 13px glyph size went (the library sizes a btn({ icon }) glyph); P7-1: 222, Data off .ctx-menu, .db-tabs, .db-chip and .db-keytype, the grid and drawer on tokens; P6-2: 234, Remote off .call, .rm-* and #rrTarget; P6-1: 240, Jobs off .tun-* (its 42px and 6px); P2-3c: 243, MCP Logs, items, Run, Config (P1b-2: 251, the held-edits dot moved to ui.css on --dot)
};

const TOKEN_BLOCK = /^:root(\[data-theme="(dark|light)"\]|\[data-code="[a-z-]+"\])?$/;
const PX = /(?<![\w.#-])(-?\d*\.?\d+)px\b/g;
const HEX = /#[0-9a-fA-F]{3,8}\b/g;
const RGB = /\b(rgba?|hsla?)\(/g;
const FREE_PX = new Set(["0", "1", "2", "-1"]);

export function literalsIn(css: string): string[] {
  const out: string[] = [];
  for (const r of parseCss(css)) {
    if (r.selectors.length && r.selectors.every((s) => TOKEN_BLOCK.test(s))) continue;
    for (const d of r.decls) {
      const where = r.selectors.join(", ") + " { " + d.prop + " }";
      for (const m of d.value.matchAll(PX)) if (!FREE_PX.has(m[1])) out.push(where + " " + m[0]);
      for (const m of d.value.matchAll(HEX)) out.push(where + " " + m[0]);
      for (const m of d.value.matchAll(RGB)) out.push(where + " " + m[0]);
    }
  }
  return out;
}

function sheetNames(): string[] {
  return readdirSync(stylesDir).filter((f) => f.endsWith(".css")).sort();
}

describe("SPEC §panel.ui - px, hex and rgb literals live in the token block", () => {
  it("the counter reads what the gate means", () => {
    const css = ":root { --x: 13px; --c: #fff; } :root[data-theme=\"dark\"] { --c: rgba(0,0,0,.5); } :root[data-code=\"darcula\"] { --c: #2b2b2b; }"
      + " .a { padding: 0 1px 2px 5px; margin: -1px; width: 100%; color: #abc; }"
      + " .b { box-shadow: 0 8px 24px rgba(0, 0, 0, 0.14); gap: var(--s2); line-height: 1.45; }"
      + " #pane .c { width: calc(var(--s4) + 6px); } @media (max-width: 960px) { .d { top: 0; } }";
    expect(literalsIn(css).map((s) => s.slice(s.lastIndexOf(" ") + 1))).toEqual(["5px", "#abc", "8px", "24px", "rgba(", "6px"]);
  });

  for (const name of sheetNames()) {
    it(name + " is at or below its frozen count", () => {
      const found = literalsIn(readFileSync(join(stylesDir, name), "utf8"));
      const cap = FROZEN[name] ?? 0;
      expect(found.length, name + " gained a literal - read the scale (base.css tokens) or add a token:\n" + found.join("\n")).toBeLessThanOrEqual(cap);
    });
  }
});

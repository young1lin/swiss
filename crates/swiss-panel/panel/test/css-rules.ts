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

/* ================================================================================================
   A deliberately small CSS reader for the SPEC §panel.ui gates (G2 ownership, G3 weights, G4 literals).

   The panel's stylesheets are hand-written and flat: plain rules, plus @media / @supports blocks
   that hold plain rules, plus @keyframes. No nesting, no @layer, no @import. So a full parser
   (postcss is only a transitive dev dependency here, and not one the gates may lean on) buys
   nothing: this reader strips comments, walks braces, and hands back every style rule with its
   selectors, its declarations and the at-rule that wraps it. @keyframes bodies are skipped -
   their "selectors" are percentages, not elements.
   ================================================================================================ */

export interface CssDecl { prop: string; value: string }
export interface CssRule {
  selectors: string[];
  decls: CssDecl[];
  /** The wrapping at-rule prelude ("@media (max-width: 960px)"), or "" at top level. */
  at: string;
  /** Order of appearance within its file, 0-based. */
  index: number;
}

/** Comments out, strings kept (a content: "·" value must not be read as structure). */
export function stripComments(css: string): string {
  let out = "";
  for (let i = 0; i < css.length; i++) {
    const c = css[i];
    if (c === "/" && css[i + 1] === "*") {
      const end = css.indexOf("*/", i + 2);
      i = end < 0 ? css.length : end + 1;
      continue;
    }
    if (c === "\"" || c === "'") {
      const q = c;
      let j = i + 1;
      while (j < css.length && css[j] !== q) j += css[j] === "\\" ? 2 : 1;
      out += css.slice(i, j + 1);
      i = j;
      continue;
    }
    out += c;
  }
  return out;
}

/** Split on commas that are not inside parentheses (":is(a, b)" stays one selector). */
export function splitTop(text: string, sep: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let cur = "";
  for (const c of text) {
    if (c === "(") depth++;
    if (c === ")") depth--;
    if (c === sep && depth === 0) { parts.push(cur); cur = ""; continue; }
    cur += c;
  }
  parts.push(cur);
  return parts.map((p) => p.trim()).filter((p) => p.length > 0);
}

function parseDecls(body: string): CssDecl[] {
  // Declarations split on ";" outside parentheses (a data: URL or a gradient keeps its own).
  return splitTop(body, ";").map((d) => {
    const colon = d.indexOf(":");
    return colon < 0 ? null : { prop: d.slice(0, colon).trim().toLowerCase(), value: d.slice(colon + 1).trim() };
  }).filter((d): d is CssDecl => d !== null);
}

export function parseCss(css: string): CssRule[] {
  const text = stripComments(css);
  const rules: CssRule[] = [];
  let index = 0;
  const walk = (src: string, at: string): void => {
    let i = 0;
    while (i < src.length) {
      const open = src.indexOf("{", i);
      if (open < 0) return;
      const prelude = src.slice(i, open).trim();
      // Find the matching close brace.
      let depth = 1;
      let j = open + 1;
      while (j < src.length && depth > 0) {
        if (src[j] === "{") depth++;
        else if (src[j] === "}") depth--;
        j++;
      }
      const body = src.slice(open + 1, j - 1);
      if (prelude.startsWith("@keyframes")) {
        // percentages, not elements
      } else if (prelude.startsWith("@")) {
        walk(body, prelude);
      } else {
        rules.push({ selectors: splitTop(prelude, ","), decls: parseDecls(body), at, index: index++ });
      }
      i = j;
    }
  };
  walk(text, "");
  return rules;
}

/** The last compound of a selector: ".a .b > .c.d:hover" -> ".c.d:hover". */
export function subjectOf(selector: string): string {
  let depth = 0;
  let cut = 0;
  for (let i = 0; i < selector.length; i++) {
    const c = selector[i];
    if (c === "(" || c === "[") depth++;
    else if (c === ")" || c === "]") depth--;
    else if (depth === 0 && (c === " " || c === ">" || c === "+" || c === "~")) cut = i + 1;
  }
  return selector.slice(cut).trim();
}

/** Class names in a compound, outside :not()/:has()/:is() arguments. */
export function classesOf(compound: string): string[] {
  const out: string[] = [];
  let depth = 0;
  for (let i = 0; i < compound.length; i++) {
    const c = compound[i];
    if (c === "(" || c === "[") { depth++; continue; }
    if (c === ")" || c === "]") { depth--; continue; }
    if (depth === 0 && c === ".") {
      const m = /^[A-Za-z_-][A-Za-z0-9_-]*/.exec(compound.slice(i + 1));
      if (m) out.push(m[0]);
    }
  }
  return out;
}

/** The class a compound is: ".btn.icon:hover" -> "btn", "pre.logs" -> "logs". Identity for the
 *  ownership gates (G2, G5): a sheet owns the base class of every subject it styles. */
export function baseClassOf(selector: string): string | undefined {
  return classesOf(subjectOf(selector))[0];
}

/** Every class name a selector mentions anywhere (arguments included). */
export function allClassesOf(selector: string): string[] {
  return Array.from(selector.matchAll(/\.([A-Za-z_-][A-Za-z0-9_-]*)/g)).map((m) => m[1]);
}

/** [ids, classes/attributes/pseudo-classes, elements/pseudo-elements] - enough for the flat
 *  selectors this panel writes (:not/:is/:has take their argument's specificity). */
export function specificity(selector: string): [number, number, number] {
  let a = 0, b = 0, c = 0;
  const s = selector.replace(/::?(not|is|has|where)\(([^()]*(\([^()]*\))*[^()]*)\)/g, (_m, fn: string, arg: string) => {
    if (fn === "where") return "";
    const inner = specificity(splitTop(arg, ",")[0] ?? "");
    a += inner[0]; b += inner[1]; c += inner[2];
    return "";
  });
  for (const m of s.matchAll(/#[A-Za-z0-9_-]+|\.[A-Za-z0-9_-]+|\[[^\]]*\]|::?[A-Za-z-]+(\([^)]*\))?|(?<![A-Za-z0-9_-])[A-Za-z][A-Za-z0-9-]*|\*/g)) {
    const t = m[0];
    if (t.startsWith("#")) a++;
    else if (t.startsWith(".") || t.startsWith("[")) b++;
    else if (t.startsWith("::")) c++;
    else if (t.startsWith(":")) {
      // Legacy single-colon pseudo-elements count as elements.
      if (/^:(before|after|first-line|first-letter)$/.test(t)) c++; else b++;
    } else if (t !== "*") c++;
  }
  return [a, b, c];
}

/** Braced blocks whose prelude is exactly ":root", ":root[data-theme=...]" or a code scheme's
 *  ":root[data-code=...]" (SPEC §panel.code) hold the tokens. */
export function tokenBlocks(css: string): string[] {
  return parseCss(css)
    .filter((r) => r.selectors.every((s) => /^:root(\[data-theme="(dark|light)"\]|\[data-code="[a-z-]+"\])?$/.test(s)))
    .map((r) => r.selectors.join(","));
}

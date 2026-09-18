/* Copyright 2026 The swiss authors
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
 * limitations under the License. */

/* The any budget is zero (docs/36 D10): every source file under src carries no ": any" /
   "as any" / "<any>" / "any[]" - the types directory and the vendored mirrors are held to
   the same rule, so a loose .d.ts cannot smuggle one back in. Comments are stripped before
   the patterns run: the word "any" in prose ("Precedence: any, …") is not an annotation, and
   a regex that flags prose would train people to ignore the guard. */

import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const SRC = join(__dirname, "..", "src");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.ts$/.test(name)) out.push(full);
  }
  return out;
}

/* Strip comments but never touch string contents, so a "//" inside a URL cannot hide the
   rest of the line and prose cannot trip the patterns. Line-preserving: offsets stay true. */
function stripComments(text: string): string {
  let out = "";
  let i = 0;
  const n = text.length;
  while (i < n) {
    const c = text[i];
    if (c === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      const stop = end === -1 ? n : end + 2;
      // Newlines inside the span survive so later line numbers stay true.
      out += text.slice(i, stop).replace(/[^\n]/g, " ");
      i = stop;
    } else if (c === "/" && text[i + 1] === "/") {
      let end = text.indexOf("\n", i);
      if (end === -1) end = n;
      out += text.slice(i, end).replace(/[^\n]/g, " ");
      i = end;
    } else if (c === '"' || c === "'" || c === "`") {
      // Skip the string verbatim (escapes included) so its contents are never stripped.
      let j = i + 1;
      while (j < n && text[j] !== c) {
        if (text[j] === "\\") j++;
        j++;
      }
      out += text.slice(i, Math.min(j + 1, n));
      i = j + 1;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

const PATTERNS: RegExp[] = [
  /:\s*any\b/,       // x: any
  /\bas\s+any\b/,   // x as any
  /<any[>,]/,        // Foo<any> / <any, (generic argument)
  /\bany\s*\[\]/,  // any[]
];

describe("panel source tree: the any budget is zero (docs/36 D10)", () => {
  it("no written any under src, types and vendored mirrors included", () => {
    const offenders: string[] = [];
    for (const file of walk(SRC)) {
      const stripped = stripComments(readFileSync(file, "utf8"));
      const rel = file.slice(SRC.length + 1).split("\\").join("/");
      stripped.split("\n").forEach((line, i) => {
        for (const re of PATTERNS) {
          if (re.test(line)) offenders.push(`src/${rel}:${i + 1}: ${line.trim()}`);
        }
      });
    }
    expect(offenders).toEqual([]);
  });
});

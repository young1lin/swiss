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

/* The shipped stylesheets, for tests that pin a CSS contract. Since docs/46 the panel links
   three sheets - base.css (tokens, reset, shell), ui.css (every component class) and views.css
   (page layouts) - in that order, and a rule's winner depends on that order. Tests read the
   sheets through here instead of naming a file, so a rule moving between layers does not
   silently turn a contract test into a test of nothing. */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const assetsDir = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets");
export const stylesDir = join(assetsDir, "styles");

export type SheetName = "base.css" | "ui.css" | "views.css";

export function sheet(name: SheetName): string {
  return readFileSync(join(stylesDir, name), "utf8");
}

/** The shell's stylesheets in link order (index.html) - which is cascade order. */
export function linkedSheets(): Array<{ name: string; css: string }> {
  const shell = readFileSync(join(assetsDir, "index.html"), "utf8");
  return Array.from(shell.matchAll(/<link rel="stylesheet" href="\/admin\/styles\/([\w.-]+)">/g))
    .map((m) => ({ name: m[1], css: readFileSync(join(stylesDir, m[1]), "utf8") }));
}

/** Every linked sheet, concatenated in cascade order. */
export function allCss(): string {
  return linkedSheets().map((s) => s.css).join("\n");
}

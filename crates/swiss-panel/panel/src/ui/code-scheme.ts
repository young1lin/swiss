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

/* --- SPEC §panel.code: the code colour schemes ---------------------------------------------------
   Every code block - the JSON view, a document card, the SQL console, a DDL - is an editor
   surface painted in a colour scheme of its own, named after the IDE schemes its tokens copy.
   The preference is per browser (localStorage, like the theme): it describes this screen.
   What lands on <html data-code> is always a concrete scheme, so styles/base.css writes each
   scheme once: "auto" follows the theme with IntelliJ Light / IntelliJ Dark, "github" with its
   own light and dark halves, and the three IDE schemes stay what they are whatever the theme -
   a Darcula block on the light panel is a choice, as in the IDE. */

/** The preferences, in the order the picker lists them. */
const CODE_SCHEMES: string[] = ["auto", "intellij-light", "darcula", "intellij-dark", "github"];

/** The concrete scheme data-code holds for a preference under a resolved theme; anything
 *  unknown is "auto". index.html's head repeats this to beat the first paint. */
function codeSchemeFor(pref: string | null, theme: "light" | "dark"): string {
  if (pref === "intellij-light" || pref === "darcula" || pref === "intellij-dark") return pref;
  if (pref === "github") return theme === "dark" ? "github-dark" : "github-light";
  return theme === "dark" ? "intellij-dark" : "intellij-light";
}

export { CODE_SCHEMES, codeSchemeFor };

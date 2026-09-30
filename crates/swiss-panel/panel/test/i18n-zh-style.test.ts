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

/* SPEC §panel.ui, on SPEC §panel.i18n's translation style: Chinese prose punctuates with the
 * full-width ，。 A half-width comma sitting right after a Han character is a
 * sentence written in Chinese that lapsed into ASCII punctuation mid-sentence -
 * the exact drift the P7 walkthrough caught 51 times. This gate reads every zh
 * value (the dictionary, not the source, so comments never reach it) and fails
 * on any Han-then-"," pair. Values that wrap technical spans ("cmd /c …",
 * {placeholders}, KEY=VALUE) keep their inner ASCII punctuation: the rule is
 * about the comma that separates Chinese clauses, and a "," after a Han
 * character is always that comma. */
import { describe, expect, it } from "vitest";
import zh from "../src/locales/zh.js";

describe("SPEC §panel.ui - zh prose punctuates full-width", () => {
  it("no half-width comma touches a Han character in any zh value", () => {
    /* Either side counts: a comma after a Latin word or a placeholder is still prose when a
       Han character follows it ("EXPIRE，留空", "{ref}，按"), and a fragment that opens on a
       comma is glued after prose (tunnels.nActive). A comma between digits (100,000) or
       inside a code sample touches no Han character and stays. */
    const hits: string[] = [];
    for (const [k, v] of Object.entries(zh)) {
      if (typeof v !== "string") continue;
      if (/[\u4e00-\u9fff],|,[\u4e00-\u9fff]|^,/.test(v)) hits.push(k + ": ..." + v.slice(0, 40) + "...");
    }
    expect(hits, "zh values with a half-width comma in prose:\n" + hits.join("\n")).toEqual([]);
  });
});

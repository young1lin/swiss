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

/* docs/46 G7 - views.css only shrinks. The library is a replacement, not a layer on top
   (docs/46 §1.1 "ruthlessly small"): every page that migrates to ui/ deletes its own rules
   from views.css, so the file's size is the plainest measure of whether that is happening.
   Frozen at P1a-2 in bytes with LF line endings (a CRLF checkout must not read as growth);
   a commit that shrinks it lowers the number in the same commit. docs/46 §0.2 started it at
   94,663 bytes. */
import { describe, expect, it } from "vitest";
import { sheet } from "./styles.js";

const FROZEN_VIEWS_BYTES = 63479; // docs/46 P7-1: Data (P6-2: 64049, P6-1: 67801, P5: 69920, P4: 70223, P3: 70330, P2-3c: 71473, P1b-3: 74519, P1b-2: 76099, P1a-2: 76412)

export function lfBytes(css: string): number {
  return Buffer.byteLength(css.replace(/\r\n/g, "\n"), "utf8");
}

describe("docs/46 G7 - views.css does not grow", () => {
  it("is at or below its frozen size", () => {
    const n = lfBytes(sheet("views.css"));
    expect(n, "views.css grew to " + n + " bytes - a new shape belongs in ui.css (and ui/), not in a page's sheet").toBeLessThanOrEqual(FROZEN_VIEWS_BYTES);
  });
});

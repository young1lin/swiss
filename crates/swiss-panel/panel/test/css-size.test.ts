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

/* SPEC §panel.ui - views.css only shrinks. The library is a replacement, not a layer on top
   (SPEC §panel.ui "ruthlessly small"): every page that migrates to ui/ deletes its own rules
   from views.css, so the file's size is the plainest measure of whether that is happening.
   Frozen at P1a-2 in bytes with LF line endings (a CRLF checkout must not read as growth);
   a commit that shrinks it lowers the number in the same commit. SPEC §panel.ui started it at
   94,663 bytes. */
import { describe, expect, it } from "vitest";
import { sheet } from "./styles.js";

const FROZEN_VIEWS_BYTES = 59932; // SPEC §panel.ui; each shrink's reason is in git log

export function lfBytes(css: string): number {
  return Buffer.byteLength(css.replace(/\r\n/g, "\n"), "utf8");
}

describe("SPEC §panel.ui - views.css does not grow", () => {
  it("is at or below its frozen size", () => {
    const n = lfBytes(sheet("views.css"));
    expect(n, "views.css grew to " + n + " bytes - a new shape belongs in ui.css (and ui/), not in a page's sheet").toBeLessThanOrEqual(FROZEN_VIEWS_BYTES);
  });
});

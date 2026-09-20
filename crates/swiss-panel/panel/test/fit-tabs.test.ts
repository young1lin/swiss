/*
 * Copyright 2026 The swiss authors
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

import { describe, it, expect } from "vitest";
import { fitTabs } from "../src/page-registry.js";

/* The tab strip's fitting rule (docs/39 S3), pinned with the spec's own numbers: all fit
 * when they fit; otherwise the ⋯ seat is reserved, tabs stay in order while they fit, and
 * the ACTIVE page is guaranteed a visible slot even when it fell into the overflow. Pure
 * numbers in, ids out - the DOM half only toggles hidden. */
const three = [{ id: "a", width: 60 }, { id: "b", width: 60 }, { id: "c", width: 60 }];

describe("fitTabs (docs/39 S3)", () => {
  it("everything fits: all visible, no overflow", () => {
    expect(fitTabs(three, "a", 300, 32)).toEqual({ visible: ["a", "b", "c"], overflow: [] });
  });

  it("exactly the available width still fits - the ⋯ seat is not reserved needlessly", () => {
    expect(fitTabs(three, "a", 180, 32)).toEqual({ visible: ["a", "b", "c"], overflow: [] });
  });

  it("tight bar, active first: one tab plus the ⋯ seat (60+32 fits, a second 60 does not)", () => {
    expect(fitTabs(three, "a", 150, 32)).toEqual({ visible: ["a"], overflow: ["b", "c"] });
  });

  it("the active page is guaranteed: fell into the overflow, it takes the last visible slot", () => {
    expect(fitTabs(three, "c", 150, 32)).toEqual({ visible: ["c"], overflow: ["a", "b"] });
  });

  it("zero available width: visible is the active page alone", () => {
    expect(fitTabs(three, "b", 0, 32)).toEqual({ visible: ["b"], overflow: ["a", "c"] });
  });

  it("an active id that is not a tab changes nothing (the guarantee only covers members)", () => {
    expect(fitTabs(three, "elsewhere", 150, 32)).toEqual({ visible: ["a"], overflow: ["b", "c"] });
  });

  it("no tabs: nothing visible, nothing overflowing", () => {
    expect(fitTabs([], "a", 0, 32)).toEqual({ visible: [], overflow: [] });
  });

  it("mixed widths stop at the first tab that does not fit (prefix, not best-fit)", () => {
    const mixed = [{ id: "a", width: 40 }, { id: "b", width: 80 }, { id: "c", width: 20 }];
    // total 140 > avail 130, budget 98: a(40) fits, b would make 120 - cut after a.
    expect(fitTabs(mixed, "a", 130, 32)).toEqual({ visible: ["a"], overflow: ["b", "c"] });
    // budget 123 (avail 135, more 12): a+b=120 fits, c would make 140 - cut after b even
    // though c alone is narrow. The total (140) over avail (135) still trips the fit check.
    expect(fitTabs(mixed, "a", 135, 12)).toEqual({ visible: ["a", "b"], overflow: ["c"] });
  });
});

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

import { describe, it, expect } from "vitest";
import { railCaptionSize, RAIL_CAPTION_CEIL, RAIL_CAPTION_FLOOR } from "../src/page-registry.js";

/* The rail caption's fitting rule (docs/39 S1 reversed; the 2026-09-21 owner request that a
 * caption never press against its seat's edge), pinned with the panel's own measurements:
 * a 56px rail, --s1 of padding, so a 48px seat and a 40px caption box; "Terminal" is 37.2px
 * at 10px in the panel's font. Pure numbers in, one size out - the DOM half only writes
 * font-size. The sizes are the exported constants so a retune of the ceiling or the floor
 * does not silently rewrite what these assert. */
const seat = (need: number, room = 40): { room: number; need: number } => ({ room, need });

describe("railCaptionSize (rail captions, one size for the rail)", () => {
  it("every stock name fits the 40px box at the ceiling - the rail stays at 10px", () => {
    const stock = [20.8, 34.1, 20.6, 19.6, 33.3, 37.2, 34.0, 35.5].map((w) => seat(Math.ceil(w)));
    expect(railCaptionSize(stock)).toBe(RAIL_CAPTION_CEIL);
    expect(RAIL_CAPTION_CEIL).toBe(10);
  });

  it("one long name steps the WHOLE rail down, to the largest half-pixel that fits it", () => {
    // 44px at 10px needs 40/44 = 0.909 -> 9.09 -> floored to the half-pixel: 9.
    expect(railCaptionSize([seat(21), seat(44)])).toBe(9);
    // 42px needs 0.952 -> 9.52 -> 9.5.
    expect(railCaptionSize([seat(21), seat(42)])).toBe(9.5);
  });

  it("never goes below the floor - what still overflows there is the ellipsis rule's job", () => {
    // "Observability", 57.6px at 10px, would need 6.9px.
    expect(railCaptionSize([seat(58)])).toBe(RAIL_CAPTION_FLOOR);
    expect(RAIL_CAPTION_FLOOR).toBe(9);
  });

  it("never goes above the ceiling: short names do not grow to fill the box", () => {
    expect(railCaptionSize([seat(12), seat(15)])).toBe(RAIL_CAPTION_CEIL);
  });

  it("a seat with no room is a hidden rail (focus mode), not a tight one: nothing is decided", () => {
    expect(railCaptionSize([seat(37, 0)])).toBeNull();
    expect(railCaptionSize([seat(37), seat(20, -8)])).toBeNull();
    expect(railCaptionSize([])).toBeNull();
  });

  it("the caption box is the seat's content box: 48px seat, --s1 padding a side, 40px box", () => {
    // What fitRailLabels feeds in: room = seat.clientWidth - paddingLeft - paddingRight. The
    // arithmetic that keeps a 37px caption clear of the edge by 1.5px+ a side at the ceiling.
    const room = 48 - 2 * 4;
    expect(room).toBe(40);
    expect(railCaptionSize([seat(37, room)])).toBe(RAIL_CAPTION_CEIL);
  });
});

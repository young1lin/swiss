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

import { afterEach, describe, expect, it, vi } from "vitest";

/* docs/37 R1 - the three narrowings that replaced the retired global augmentations:
 * errText (Function's message reads in catches), isTyping (the RegExp.test(string | null)
 * overload: main.ts's typing guards), targetEl (EventTarget's optional closest/tagName).
 * Each case here pins the branch whose behavior the augmentation used to decide. */

const util = await import("../src/util.js");

/* The duck-typed probe only reads target, so a two-field stand-in is the honest event. */
const ev = (target: unknown) => ({ target }) as unknown as Event;

describe("errText: the catch-side reader (docs/37 M5)", () => {
  it("an Error yields its message, everything else its String form", () => {
    expect(util.errText(new Error("boom"))).toBe("boom");
    expect(util.errText("plain string")).toBe("plain string");
    expect(util.errText(42)).toBe("42");
    expect(util.errText(null)).toBe("null");
    expect(util.errText(undefined)).toBe("undefined");
  });
});

describe("isTyping: the typing guard without the RegExp overload (docs/37 M3)", () => {
  afterEach(() => { vi.unstubAllGlobals(); });

  it("no active element is not typing - the branch the old null-to-\"null\" coercion decided", () => {
    vi.stubGlobal("document", { activeElement: null });
    expect(util.isTyping()).toBe(false);
  });

  it("an input, textarea or select is typing; anything else is not", () => {
    for (const tag of ["INPUT", "TEXTAREA", "SELECT"]) {
      vi.stubGlobal("document", { activeElement: { tagName: tag } });
      expect(util.isTyping(), tag).toBe(true);
    }
    vi.stubGlobal("document", { activeElement: { tagName: "DIV" } });
    expect(util.isTyping()).toBe(false);
    vi.stubGlobal("document", { activeElement: { tagName: "BODY" } });
    expect(util.isTyping()).toBe(false);
  });
});

describe("targetEl: event.target narrowed without the EventTarget augmentation (docs/37 M3)", () => {
  it("a target with closest comes back; one without (a Window) answers null", () => {
    const row = { closest: (sel: string) => ({ sel }) } as unknown as Element;
    expect(util.targetEl(ev(row))).toBe(row);
    // Window targets declare no closest - the old inline probe answered falsy, not a throw.
    const win = { alert: () => {} } as unknown as Element;
    expect(util.targetEl(ev(win))).toBeNull();
    expect(util.targetEl(ev(null))).toBeNull();
  });
});

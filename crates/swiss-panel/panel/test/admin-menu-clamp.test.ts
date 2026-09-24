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
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts). */
const el = (): Record<string, unknown> => {
  const node: Record<string, unknown> = {
    style: {}, dataset: {}, hidden: false, disabled: false, value: "", textContent: "",
    innerHTML: "", className: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    setAttribute: () => {}, getAttribute: () => "", removeAttribute: () => {},
    addEventListener: () => {}, removeEventListener: () => {},
    appendChild: (c: unknown) => c, removeChild: (c: unknown) => c, remove: () => {},
    querySelector: () => null, querySelectorAll: () => [],
    focus: () => {}, blur: () => {}, click: () => {},
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
  };
  node.cloneNode = () => el();
  return node;
};
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener: () => {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const menu = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "ui", "menu.ts")).href
) as { clampMenuPos: (anchor: { left: number; top: number; bottom: number }, w: number, h: number, vw: number, vh: number) => { left: number; top: number } };

/* docs/22 closeout audit: the right-click cell menus (data-csv.js) and the Table menu
   (data-edit.js) positioned their hand-built .ctx-menu at the raw cursor / button rect with no
   clamp, so a click near the right or bottom edge put the menu off-screen with no way to reach
   it. popupMenu (ui/menu.js) has always clamped; that arithmetic is clampMenuPos, and since
   docs/46 P7 those three menus ARE popupMenu. Pure, so the clamp pins here. */
describe("clampMenuPos (anchored-menu viewport clamp)", () => {
  const VW = 1920, VH = 1080;

  it("an anchor mid-screen keeps the menu below-left of it, unchanged", () => {
    const pos = menu.clampMenuPos({ left: 100, top: 500, bottom: 520 }, 150, 200, VW, VH);
    expect(pos.left).toBe(100);
    expect(pos.top).toBe(524); // bottom + 4
  });

  it("a cursor at the bottom-right corner clamps left and flips the menu above", () => {
    const pos = menu.clampMenuPos({ left: 1900, top: 1000, bottom: 1000 }, 200, 300, VW, VH);
    expect(pos.left).toBe(VW - 200 - 8); // 1712 — the 8px right margin holds
    expect(pos.top).toBe(1000 - 300 - 4); // 696 — above the anchor, 4px gap
  });

  it("a menu wider than the viewport pins to the left margin rather than a negative x", () => {
    const pos = menu.clampMenuPos({ left: 300, top: 100, bottom: 120 }, 500, 100, 400, 800);
    expect(pos.left).toBe(8);
  });

  it("a flip that would leave the viewport pins to the top margin", () => {
    const pos = menu.clampMenuPos({ left: 50, top: 5, bottom: 5 }, 150, 300, 800, 300);
    // below (9) + 300 exceeds 292 -> flip above: 5 - 300 - 4 is negative -> pinned at 8.
    expect(pos.top).toBe(8);
  });
});

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

// @vitest-environment happy-dom

/* docs/46 U9 - the pinned page head's hairline follows the pane's scroll offset, and a page
   switch never inherits the previous page's. The CSS half (.pane.scrolled > .wide >
   .pane-head::after) is pinned in the sheet test below; this is the shell half. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { resetPaneScroll, trackPaneScroll } from "../src/pane-scroll.js";
import { sheet } from "./styles.js";

function scrollTo(pane: HTMLElement, top: number): void {
  pane.scrollTop = top;
  pane.dispatchEvent(new Event("scroll"));
}

describe("docs/46 U9 - .pane.scrolled", () => {
  it("is on while the pane is scrolled and off at the top", () => {
    const pane = document.createElement("main");
    trackPaneScroll(pane);
    expect(pane.classList.contains("scrolled")).toBe(false);
    scrollTo(pane, 120);
    expect(pane.classList.contains("scrolled")).toBe(true);
    scrollTo(pane, 0);
    expect(pane.classList.contains("scrolled")).toBe(false);
  });

  it("a page switch resets the offset and the class", () => {
    const pane = document.createElement("main");
    trackPaneScroll(pane);
    scrollTo(pane, 300);
    resetPaneScroll(pane);
    expect(pane.scrollTop).toBe(0);
    expect(pane.classList.contains("scrolled")).toBe(false);
  });

  it("tolerates the navigation suites' hand-rolled DOM", () => {
    const stub = { scrollTop: 40 } as unknown as HTMLElement;
    expect(() => { trackPaneScroll(stub); resetPaneScroll(stub); }).not.toThrow();
    expect(stub.scrollTop).toBe(40);
  });

  it("the registry installs the listener once and resets on every navigation", () => {
    const src = readFileSync(join(import.meta.dirname, "..", "src", "page-registry.ts"), "utf8");
    expect(src.match(/trackPaneScroll\(/g)).toHaveLength(1);
    // The reset sits before the pane is repainted, inside navigatePage.
    const nav = src.slice(src.indexOf("async function navigatePage("));
    expect(nav.indexOf("resetPaneScroll($(\"pane\"))")).toBeGreaterThan(0);
    expect(nav.indexOf("resetPaneScroll($(\"pane\"))")).toBeLessThan(nav.indexOf("fill($(\"pane\")"));
  });

  it("the sheet pins a content page's head and draws its hairline only when scrolled", () => {
    const ui = sheet("ui.css");
    expect(ui).toMatch(/\.pane > \.wide > \.pane-head \{\s*position: sticky;/);
    expect(ui).toMatch(/\.pane > \.wide > \.pane-head::after \{[^}]*visibility: hidden;/);
    expect(ui).toMatch(/\.pane\.scrolled > \.wide > \.pane-head::after \{ visibility: visible; \}/);
  });
});

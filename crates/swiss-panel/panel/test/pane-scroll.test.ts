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

/* docs/46 U9, §3.2, U18 - the pinned page head's hairline follows the pane's scroll offset, and a page
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

  it("a resource head's hairline waits until its nav is stuck, not the first pixel (§3.2)", async () => {
    const pane = document.createElement("main");
    pane.style.paddingTop = "20px";
    document.body.appendChild(pane);
    trackPaneScroll(pane);
    const frame = document.createElement("div");
    frame.className = "wide";
    const head = document.createElement("div");
    head.className = "pane-head res";
    const meta = document.createElement("div");
    meta.className = "res-meta";
    const nav = document.createElement("div");
    nav.className = "pane-nav";
    Object.defineProperty(head, "offsetHeight", { value: 56 });
    Object.defineProperty(nav, "offsetHeight", { value: 44 });
    // At rest the words above the nav end 150px down the pane.
    meta.getBoundingClientRect = () => ({ bottom: 150 - pane.scrollTop }) as DOMRect;
    pane.getBoundingClientRect = () => ({ top: 0 }) as DOMRect;
    frame.append(head, meta, nav);
    pane.appendChild(frame);
    await Promise.resolve();
    expect(pane.style.getPropertyValue("--pin-title-h")).toBe("56px");
    // The stack ends 56 + 44 px below the pane's top edge = 80px below the sticky origin.
    expect(pane.style.getPropertyValue("--pane-head-h")).toBe("80px");
    // The nav meets the name row at 150 - 56 = 94.
    scrollTo(pane, 60);
    expect(pane.classList.contains("scrolled")).toBe(false);
    scrollTo(pane, 95);
    expect(pane.classList.contains("scrolled")).toBe(true);
    pane.remove();
  });

  it("a content head is stuck from the first pixel, and no pin measures zero", async () => {
    const pane = document.createElement("main");
    pane.style.paddingTop = "20px";
    document.body.appendChild(pane);
    trackPaneScroll(pane);
    expect(pane.style.getPropertyValue("--pane-head-h")).toBe("0px");
    const frame = document.createElement("div");
    frame.className = "wide";
    const head = document.createElement("div");
    head.className = "pane-head";
    Object.defineProperty(head, "offsetHeight", { value: 72 });
    frame.append(head);
    pane.appendChild(frame);
    await Promise.resolve();
    expect(pane.style.getPropertyValue("--pane-head-h")).toBe("52px");
    scrollTo(pane, 1);
    expect(pane.classList.contains("scrolled")).toBe(true);
    pane.remove();
  });

  it("installs one back-to-top button (U18), and a page switch hides it at once", () => {
    const before = document.querySelectorAll("body > .to-top").length;
    const pane = document.createElement("main");
    trackPaneScroll(pane);
    const all = document.querySelectorAll<HTMLElement>("body > .to-top");
    expect(all.length).toBe(before + 1);
    const top = all[all.length - 1];
    Object.defineProperty(pane, "clientHeight", { value: 500 });
    scrollTo(pane, 900);
    expect(top.classList.contains("on")).toBe(true);
    resetPaneScroll(pane);
    expect(top.classList.contains("on")).toBe(false);
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
    expect(ui).toMatch(/\.pane > \.wide > \.pane-head::after, \.pane > \.wide > \.pane-nav::after \{[^}]*visibility: hidden;/);
    expect(ui).toMatch(/\.pane\.scrolled > \.wide > \.pane-head::after, \.pane\.scrolled > \.wide > \.pane-nav::after \{ visibility: visible; \}/);
  });

  it("the sheet stacks a resource head's nav under its name row, and only the last layer draws the line", () => {
    const ui = sheet("ui.css");
    expect(ui).toMatch(/\.pane > \.wide > \.pane-nav \{\s*position: sticky; top: calc\(var\(--pin-title-h, 0px\) - var\(--s5\)\); z-index: 3;/);
    expect(ui).toMatch(/\.pane > \.wide > \.pane-head \{\s*position: sticky; top: calc\(var\(--s5\) \* -1\); z-index: 4;/);
    expect(ui).toMatch(/\.pane > \.wide > \.pane-head:has\(~ \.pane-nav\)::after \{ display: none; \}/);
    // The gap above the nav is the words' own padding, so their bottom edge IS the nav's rest top.
    expect(ui).toMatch(/\.res-meta \{ padding-bottom: var\(--s5\); \}/);
    expect(ui).toMatch(/\.tl-day \{[^}]*top: var\(--pane-head-h, 0px\)/);
  });
});

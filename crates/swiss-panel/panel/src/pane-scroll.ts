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

/* The pane's scrolled state (docs/46 U9, §3.2, U18). The shell owns three facts about #pane,
 * not each page - installed ONCE at boot, reset on every page switch so a page never opens
 * with the previous page's hairline (or its scroll offset):
 *
 *   .scrolled      something has scrolled under the pinned stack, so its last layer draws its
 *                  hairline. A content page's head is stuck from the first pixel; a resource
 *                  head's nav sticks only once the words above it have gone under the name
 *                  row, so the line waits for that instead of underlining a seg at rest.
 *   --pin-title-h  the pinned head's height - where the nav layer sticks (resHead).
 *   --pane-head-h  where the pinned stack ends, measured from the sticky origin (the pane's
 *                  padding edge): the timeline's day headings stop there (§2.4).
 *   the back-to-top button (toTop), fixed on <body>.
 *
 * The layers are re-measured when they change size and when a view repaints the pane. Every
 * function tolerates the navigation suites' hand-rolled DOM, which carries neither
 * addEventListener nor a full classList - the same guard page-registry's layout pass uses. */
import { toTop } from "./ui/index.js";

const SCROLLED = "scrolled";

/** The scrollTop past which the pinned stack's last layer is stuck. 0 for a content head
 *  (stuck at rest) and when nothing is pinned. */
let pinAt = 0;

function pinLayers(pane: HTMLElement): { head: HTMLElement | null; nav: HTMLElement | null } {
  return {
    head: pane.querySelector<HTMLElement>(":scope > .wide > .pane-head"),
    nav: pane.querySelector<HTMLElement>(":scope > .wide > .pane-nav"),
  };
}

function measurePins(pane: HTMLElement): void {
  const { head, nav } = pinLayers(pane);
  const pad = parseFloat(getComputedStyle(pane).paddingTop) || 0;
  const titleH = head ? head.offsetHeight : 0;
  const stackH = titleH + (nav ? nav.offsetHeight : 0);
  pane.style.setProperty("--pin-title-h", titleH + "px");
  pane.style.setProperty("--pane-head-h", (stackH ? Math.max(0, stackH - pad) : 0) + "px");
  // Where the nav sits at rest, in scroll coordinates: at the bottom of the words above it
  // (their box carries the gap as padding, so its edge IS the nav's top), and those words
  // never stick, so their box is always where the flow put it.
  const above = nav ? nav.previousElementSibling : null;
  pinAt = nav && above
    ? Math.max(0, above.getBoundingClientRect().bottom - pane.getBoundingClientRect().top + pane.scrollTop - titleH)
    : 0;
}

function syncScrolled(pane: HTMLElement): void {
  pane.classList.toggle(SCROLLED, pane.scrollTop > pinAt);
}

export function trackPaneScroll(pane: HTMLElement): void {
  if (typeof pane.addEventListener !== "function") return;
  pane.addEventListener("scroll", () => { syncScrolled(pane); }, { passive: true });
  if (typeof ResizeObserver === "function" && typeof MutationObserver === "function") {
    const sizes = new ResizeObserver(() => { measurePins(pane); syncScrolled(pane); });
    // A view repaints by replacing the pane's frame (fill(#pane, …)) or the frame's children.
    const repaints = new MutationObserver(() => { watch(); });
    const watch = (): void => {
      sizes.disconnect();
      repaints.disconnect();
      repaints.observe(pane, { childList: true });
      pane.querySelectorAll(":scope > .wide").forEach((frame) => { repaints.observe(frame, { childList: true }); });
      const { head, nav } = pinLayers(pane);
      [head, nav, nav ? nav.previousElementSibling : null].forEach((n) => { if (n) sizes.observe(n); });
      measurePins(pane);
      syncScrolled(pane);
    };
    watch();
  }
  // The shell's one back-to-top button. Built once, so the language flip relabels it by this
  // id (i18n.ts paintChrome) instead of rebuilding it.
  const top = toTop(pane);
  top.id = "toTop";
  document.body.appendChild(top);
}

export function resetPaneScroll(pane: HTMLElement): void {
  const cls = pane.classList as DOMTokenList | undefined;
  if (typeof cls?.remove !== "function") return;
  pane.scrollTop = 0;
  cls.remove(SCROLLED);
  // The offset changed under the listeners: tell them now rather than on the browser's next
  // scroll event, so the back-to-top button never lingers into the next page.
  if (typeof pane.dispatchEvent === "function") pane.dispatchEvent(new Event("scroll"));
}

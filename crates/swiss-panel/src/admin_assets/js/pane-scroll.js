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

/* The pane's scrolled state (docs/46 U9). A pinned page head draws its hairline only once
 * something has scrolled under it - at rest a line under the description is one more line.
 * The shell owns that fact, not each page: ONE passive scroll listener on #pane, installed
 * once at boot, and a reset on every page switch so a page never opens with the previous
 * page's hairline (or its scroll offset).
 *
 * Both functions tolerate the navigation suites' hand-rolled DOM, which carries neither
 * addEventListener nor a full classList - the same guard page-registry's layout pass uses. */

const SCROLLED = "scrolled";

export function trackPaneScroll(pane             )       {
  if (typeof pane.addEventListener !== "function") return;
  pane.addEventListener("scroll", () => { pane.classList.toggle(SCROLLED, pane.scrollTop > 0); }, { passive: true });
}

export function resetPaneScroll(pane             )       {
  const cls = pane.classList                            ;
  if (typeof cls?.remove !== "function") return;
  pane.scrollTop = 0;
  cls.remove(SCROLLED);
}

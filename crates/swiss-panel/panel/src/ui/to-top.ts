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

/* Back to the top (docs/46 U18). A long page - forty open calls in MCP Logs, a busy traffic
 * log - costs a long scroll home, and the pinned head only keeps the page's name and tabs in
 * reach, not its top. One round glyph at the scroller's bottom-right corner answers it:
 *
 *   - it appears once the reader is more than one screen down (scrollTop > clientHeight):
 *     nearer the top, the wheel is the shorter way and a floating control is only clutter;
 *   - it is hidden with visibility, so a hidden one is out of the Tab order too;
 *   - a click scrolls smoothly, or jumps under prefers-reduced-motion.
 *
 * A DOM-only mechanism (§2.5): it listens to its scroller and nothing else. The caller puts
 * the button where a fixed control belongs (the shell: <body>, for #pane), and a workspace
 * page never shows it because .pane.full never scrolls. Tolerates a stub scroller with no
 * addEventListener, as the shell's other pane hooks do. */
import { tr } from "../i18n.js";
import { iconBtn } from "./button.js";

export function toTop(scroller: HTMLElement): HTMLButtonElement {
  const b = iconBtn("arrow-up", tr("ui.toTop"));
  b.classList.add("to-top");
  const sync = (): void => { b.classList.toggle("on", scroller.scrollTop > scroller.clientHeight); };
  if (typeof scroller.addEventListener === "function") scroller.addEventListener("scroll", sync, { passive: true });
  b.addEventListener("click", () => {
    const still = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (typeof scroller.scrollTo === "function") scroller.scrollTo({ top: 0, behavior: still ? "auto" : "smooth" });
    else scroller.scrollTop = 0;
  });
  sync();
  return b;
}

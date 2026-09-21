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

/* An in-terminal toast: one small pill, centered over the terminal surface, faded in
   and out. ttyd's overlay addon (originally hterm's) proved the shape and every call
   site; this is the vanilla-ESM restyling for this panel's language — tokens only, no
   shadow, nothing focusable. It is the feedback channel a terminal needs that never
   touches the PTY stream: resize geometry, the copy scissors, reconnect states. */

export function createOverlay(holder             )                                                             {
  let el                        = null;
  let fade                                       = null;
  let hide                                       = null;
  return {
    show: (text        , ms         )       => {
      if (!el) {
        el = document.createElement("div");
        el.className = "term-overlay";
        el.setAttribute("aria-hidden", "true");
      }
      el.textContent = text;
      if (!el.parentNode) holder.appendChild(el);
      /* Forced reflow so a back-to-back show() can fade the same element in again. */
      void el.offsetWidth;
      el.classList.add("on");
      if (hide) clearTimeout(hide);
      if (fade) clearTimeout(fade);
      hide = setTimeout(() => {
        el?.classList.remove("on");
        fade = setTimeout(() => {
          if (el && el.parentNode) el.parentNode.removeChild(el);
        }, 220);
      }, ms || 900);
    },
    dispose: ()       => {
      if (hide) clearTimeout(hide);
      if (fade) clearTimeout(fade);
      if (el && el.parentNode) el.parentNode.removeChild(el);
      el = null;
    },
  };
}

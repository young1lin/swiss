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

/* Copy to the clipboard (SPEC §panel.ui). copyText is the house idiom - the async clipboard
 * first, a hidden textarea's copy second, so a browser that withholds the async one (an http
 * origin other than localhost) still copies. copyBtn is the glyph that does it in place, as an
 * assistant's column offers it: a quiet copy glyph that turns to a check for a moment once the
 * words are on the clipboard, its label saying Copied (or Copy failed, a cross). */
import { tr } from "../i18n.js";
import { iconBtn } from "./button.js";
import { iconNode } from "./icon.js";

/** How long the check stays before the glyph is Copy again. */
const COPIED_MS = 1500;

/** Put the words on the clipboard; false when the browser refused both ways. */
export async function copyText(text        )                   {
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch { /* refused: the textarea's copy next */ }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try { ok = typeof document.execCommand === "function" && document.execCommand("copy"); } catch { ok = false; }
  ta.remove();
  return ok;
}

/** A copy glyph (a ghost iconBtn, .copy-btn): a click puts `text` on the clipboard - a function
 *  is read at the click, so what is copied is what is shown then - and the glyph says how it
 *  went for a moment (data-copied while the check shows). `label` names what it copies. */
export function copyBtn(text                         , o                     = {})                    {
  const label = o.label ?? tr("ui.copy");
  const b = iconBtn("copy", label, { ghost: true });
  b.classList.add("copy-btn");
  let timer = 0;
  const mark = (icon        , words        )       => {
    b.replaceChildren(iconNode(icon));
    b.setAttribute("aria-label", words);
    b.title = words;
  };
  b.onclick = async ()                => {
    const ok = await copyText(typeof text === "function" ? text() : text);
    mark(ok ? "check" : "x", ok ? tr("ui.copied") : tr("ui.copyFailed"));
    b.toggleAttribute("data-copied", ok);
    window.clearTimeout(timer);
    timer = window.setTimeout(()       => {
      mark("copy", label);
      b.removeAttribute("data-copied");
    }, COPIED_MS);
  };
  return b;
}

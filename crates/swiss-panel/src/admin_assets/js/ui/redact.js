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

/* An address shown only on request (2026-09-28). The panel gets screenshotted - into bug
 * reports, chats, docs - and a server's address in a row's sub-line went out with every one.
 * So a remote host draws as a fixed-width mask with an eye beside it; the eye reveals that
 * value, everywhere it is drawn, until the page reloads. The revealed set lives here, in
 * memory only, never in storage: a 6s poll that rebuilds a row keeps what the user opened,
 * and a reload closes everything again.
 *
 * Loopback (127.0.0.0/8, localhost, ::1) is not a secret and draws plain: a forward's
 * 127.0.0.1 target is the common case, and masking it would only add eyes to click.
 *
 * The value never rides an attribute - not a title (a hover would show it), not a data-*
 * (it would travel with any copied markup). The node keeps it in a WeakMap, and the one
 * click listener, installed on first use, finds it there. The listener runs in the capture
 * phase and stops the click: the eye sits inside rows whose own delegated click means
 * something else. */
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { iconNode } from "./icon.js";

const MASK = "••••••";

                                                                 

const revealed = new Set        ();
const entries = new WeakMap                ();
let wired = false;

/** Loopback, matched whole: `localhost`, `::1` (bracketed or not), anything in 127.0.0.0/8. */
export function isLocalAddress(host        )          {
  const s = String(host || "").trim().toLowerCase().replace(/^\[(.*)\]$/, "$1");
  if (s === "localhost" || s === "::1") return true;
  return /^127(\.\d{1,3}){3}$/.test(s);
}

;                            
                                                                
                  
                                                                
                  
 

/** A host, masked until its eye is pressed. Loopback and empty values draw as plain code. */
export function redacted(value        , o             = {})              {
  const v = String(value ?? "");
  const prefix = o.prefix || "";
  const suffix = o.suffix || "";
  if (!v || isLocalAddress(v)) return h("code", null, prefix + v + suffix);
  wire();
  const e        = { value: v, prefix, suffix };
  const open = revealed.has(v);
  const label = eyeLabel(open);
  const node = h("span", { class: "redact" },
    h("code", null, shownText(e, open)),
    h("button", { type: "button", class: "redact-eye", title: label, aria: { label, pressed: String(open) } },
      iconNode(open ? "eye-off" : "eye")));
  entries.set(node, e);
  return node;
}

function shownText(e       , open         )         {
  return e.prefix + (open ? e.value : MASK) + e.suffix;
}

function eyeLabel(open         )         {
  return tr(open ? "ui.redact.hide" : "ui.redact.show");
}

/** Brings a drawn node in line with the revealed set, in place: the button keeps its focus. */
function paint(node         )       {
  const e = entries.get(node);
  const code = node.firstElementChild;
  const eye = node.lastElementChild;
  if (!e || !code || !eye) return;
  const open = revealed.has(e.value);
  code.textContent = shownText(e, open);
  const label = eyeLabel(open);
  eye.setAttribute("title", label);
  eye.setAttribute("aria-label", label);
  eye.setAttribute("aria-pressed", String(open));
  const use = eye.querySelector("use");
  if (use) use.setAttribute("href", open ? "#i-eye-off" : "#i-eye");
}

function wire()       {
  if (wired || typeof document === "undefined") return;
  wired = true;
  document.addEventListener("click", (ev       )       => {
    const t = ev.target                  ;
    const eye = t && typeof t.closest === "function" ? t.closest(".redact-eye") : null;
    const host = eye ? eye.parentElement : null;
    const e = host ? entries.get(host) : undefined;
    if (!e) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (revealed.has(e.value)) revealed.delete(e.value);
    else revealed.add(e.value);
    document.querySelectorAll(".redact").forEach((n) => {
      const x = entries.get(n);
      if (x && x.value === e.value) paint(n);
    });
  }, true);
}

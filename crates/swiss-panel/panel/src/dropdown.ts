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

import { el } from "./util.js";

/* ---------------------------------------------------------------------------------------------
   The panel-wide custom dropdown.

   A native <select>'s CLOSED box can be styled, but its option POPUP cannot — on Windows it is
   the system list, white in both themes, and it is the one control in the panel that never
   matched. So every select's UI is replaced here: the native element stays in the DOM, hidden,
   as the source of truth (value, options, change events all keep their meaning — call sites
   never change), and a trigger button plus a floating menu render its face.

   Attached automatically: a MutationObserver styles every <select> the moment it enters the
   DOM (the sheets and the Data view build theirs dynamically), so a view never opts in or out.
   ---------------------------------------------------------------------------------------------- */

// Prototype accessors, resolved lazily: importing this module under a DOM-stubbed Node (the
// panel boot test) must not throw on a browser global that only exists where a select does.
let valDesc: { get(): string; set(v: string): void } | null = null, idxDesc: { get(): number; set(v: number): void } | null = null, disDesc: { get(): boolean; set(v: boolean): void } | null = null, htmlDesc: { get(): string; set(v: string): void } | null = null;
function descs() {
  if (valDesc) return;
  const proto = HTMLSelectElement.prototype;
  valDesc = Object.getOwnPropertyDescriptor(proto, "value") as { get(): string; set(v: string): void };
  idxDesc = Object.getOwnPropertyDescriptor(proto, "selectedIndex") as { get(): number; set(v: number): void };
  disDesc = Object.getOwnPropertyDescriptor(proto, "disabled") as { get(): boolean; set(v: boolean): void };
  htmlDesc = Object.getOwnPropertyDescriptor(Element.prototype, "innerHTML") as { get(): string; set(v: string): void };
}

let openState: { menu: HTMLElement; trig: HTMLButtonElement; sel: HTMLSelectElement; items: HTMLButtonElement[] } | null = null; // { menu, trig, sel, items, active }

// A styled select's face, by select: removal handling needs it (below).
const trigs = new WeakMap<HTMLSelectElement, HTMLButtonElement>();

/** A styled select that leaves the DOM takes its face with it. The trigger sits BESIDE the
 *  select ("afterend"), so removing the select alone orphans a live-looking dropdown —
 *  docs/22 closeout B7 caught one on a redis page still showing the previous pg connection's
 *  schema pick, because the view removes the select when the connection kind changes. */
function dropTrig(sel: HTMLSelectElement): void {
  const t = trigs.get(sel);
  if (t && t.parentNode) t.parentNode.removeChild(t);
}

function closeMenu(): void {
  if (!openState) return;
  openState.menu.remove();
  openState.trig.setAttribute("aria-expanded", "false");
  openState = null;
}

/** The trigger's label mirrors the selected option's text (not its value). */
function paint(sel: HTMLSelectElement, trig: HTMLButtonElement): void {
  descs();
  const opt = sel.options && sel.options[sel.selectedIndex];
  trig.querySelector<HTMLElement>(".dd-label")!.textContent = opt ? opt.textContent : "";
  trig.disabled = disDesc!.get.call(sel);
}

function openMenuFor(sel: HTMLSelectElement, trig: HTMLButtonElement): void {
  closeMenu();
  const menu = el("div", "menu float dd-menu");
  menu.setAttribute("role", "listbox");
  menu.style.minWidth = trig.offsetWidth + "px";
  const items: HTMLButtonElement[] = [];
  let mark: HTMLButtonElement | null = null;
  Array.prototype.forEach.call(sel.options, (o) => {
    const b = el("button", "pick" + (o.selected ? " on" : ""));
    b.type = "button";
    b.setAttribute("role", "option");
    b.setAttribute("aria-selected", String(o.selected));
    b.title = o.textContent;
    b.textContent = o.textContent;
    b.onclick = () => {
      descs();
      if (sel.value !== o.value) {
        valDesc?.set.call(sel, o.value);
        paint(sel, trig);
        // Native change events do not fire on programmatic assignment, so the pick dispatches
        // one - and it must BUBBLE like the native event does: since docs/37 R5 the views listen
        // once, on the pane root (data-view dbPaneChange, run-history paneTabChange, remote-runs),
        // and a non-bubbling Event("change") stops at the select and never reaches them. That
        // was the Data connection picker doing nothing on 19998 (2026-09-20).
        sel.dispatchEvent(new Event("change", { bubbles: true }));
      }
      closeMenu();
      trig.focus();
    };
    menu.appendChild(b);
    items.push(b);
    if (o.selected) mark = b;
  });
  if (!items.length) return; // nothing to choose — the trigger stays inert
  document.body.appendChild(menu);

  // Below the trigger, unless that runs off the window — then above it. Same policy as popupMenu.
  const r = trig.getBoundingClientRect();
  const mh = Math.min(menu.offsetHeight, 280);
  menu.style.maxHeight = "280px";
  const left = Math.max(8, Math.min(r.left, window.innerWidth - menu.offsetWidth - 8));
  menu.style.left = left + "px";
  const below = r.bottom + 4;
  menu.style.top = (below + mh > window.innerHeight - 8 ? Math.max(8, r.top - mh - 4) : below) + "px";

  openState = { menu: menu, trig: trig, sel: sel, items: items };
  trig.setAttribute("aria-expanded", "true");
  (mark || items[0]).focus();
}

function buildTrigger(sel: HTMLSelectElement, ownClasses: string): HTMLButtonElement {
  // Carry the select's own classes over: the per-view sizing rules (.db-pagesize, …) then shape
  // the trigger exactly as they shaped the select it stands in for.
  const trig = el("button", "dd" + (ownClasses ? " " + ownClasses : ""));
  trig.type = "button";
  if (sel.title) trig.title = sel.title;
  if (sel.getAttribute("aria-label")) trig.setAttribute("aria-label", sel.getAttribute("aria-label") as string);
  trig.setAttribute("aria-haspopup", "listbox");
  trig.setAttribute("aria-expanded", "false");
  trig.appendChild(el("span", "dd-label"));
  trig.appendChild(el("span", "dd-chev"));

  trig.onclick = () => {
    if (trig.disabled) return;
    if (openState && openState.trig === trig) closeMenu();
    else openMenuFor(sel, trig);
  };
  // The keyboard contract of a native select, on the trigger: open on Enter/Space/arrows.
  trig.onkeydown = (e) => {
    if (trig.disabled) return;
    if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      openMenuFor(sel, trig);
    }
  };
  return trig;
}

/** Take over one select. Idempotent; skips multi-selects, which have no dropdown face. */
function styleSelect(sel: HTMLSelectElement | null | undefined): void {
  descs();
  if (!sel || !valDesc || sel.multiple || sel.dataset.ddDone) return; // no DOM select here — a stub
  sel.dataset.ddDone = "1";
  const ownClasses = sel.className; // captured BEFORE dd-native is stamped on, or the trigger inherits the hider
  sel.classList.add("dd-native"); // hidden; the trigger beside it is the face

  const trig = buildTrigger(sel, ownClasses);
  sel.insertAdjacentElement("afterend", trig);
  paint(sel, trig);

  // Programmatic writes must reach the face. The instance-level overrides shadow the prototype
  // accessors, so every existing "sel.value = x" / "sel.innerHTML = …" repaints the label.
  Object.defineProperty(sel, "value", {
    get: () => { return valDesc?.get.call(sel); },
    set: (v) => { valDesc?.set.call(sel, v); paint(sel, trig); },
    configurable: true,
  });
  Object.defineProperty(sel, "selectedIndex", {
    get: () => { return idxDesc?.get.call(sel); },
    set: (v) => { idxDesc?.set.call(sel, v); paint(sel, trig); },
    configurable: true,
  });
  Object.defineProperty(sel, "disabled", {
    get: () => { return disDesc?.get.call(sel); },
    set: (v) => { disDesc?.set.call(sel, v); paint(sel, trig); },
    configurable: true,
  });
  Object.defineProperty(sel, "innerHTML", {
    get: () => { return htmlDesc?.get.call(sel); },
    set: (v) => { htmlDesc?.set.call(sel, v); paint(sel, trig); },
    configurable: true,
  });
  const origAdd = sel.appendChild.bind(sel);
  sel.appendChild = (node) => { origAdd(node); paint(sel, trig); return node; };
  trigs.set(sel, trig);
}

/** Style everything already in the DOM, then watch for selects the views create later. Every
 *  global this wires is feature-detected: the module must import cleanly under the DOM-stubbed
 *  Node of the panel boot test, which has no MutationObserver and no body to observe. */
function initSelects(): void {
  Array.prototype.forEach.call(document.querySelectorAll("select"), styleSelect);
  if (typeof MutationObserver !== "undefined" && document.body) new MutationObserver((muts) => {
    for (const m of muts) {
      // Node lists carry every node kind; nodeType 1 is the element test that works under
      // every DOM this module imports through (real, happy-dom, and the boot test's stubs,
      // which have no Element class to instanceof against). The downcast then names what
      // the walk needs - nodeType 1 already promised an element.
      for (const n of m.addedNodes) {
        if (n.nodeType !== 1) continue;
        const added = n as Element;
        if (added.tagName === "SELECT") styleSelect(added as HTMLSelectElement);
        else if (added.querySelectorAll) Array.prototype.forEach.call(added.querySelectorAll("select"), styleSelect);
      }
      // docs/22 closeout B7: a removed styled select must not leave its trigger behind.
      for (const n of m.removedNodes) {
        if (n.nodeType !== 1) continue;
        const gone = n as Element;
        if (gone.tagName === "SELECT") dropTrig(gone as HTMLSelectElement);
        else if (gone.querySelectorAll) Array.prototype.forEach.call(gone.querySelectorAll("select"), dropTrig);
      }
    }
    // A re-render can remove an open trigger's subtree; its menu would be left floating.
    if (openState && !document.body.contains(openState.trig)) closeMenu();
  }).observe(document.body, { childList: true, subtree: true });

  // One open dropdown at a time, closed by any click outside it or by Escape/Tab.
  document.addEventListener("mousedown", (e) => {
    if (openState && !openState.menu.contains(e.target as Node) && e.target !== openState.trig) closeMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (!openState) return;
    if (e.key === "Escape" || e.key === "Tab") { closeMenu(); openState.trig.focus(); e.preventDefault(); return; }
    const items = openState.items;
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown") { e.preventDefault(); items[(i + 1 + items.length) % items.length].focus(); }
    if (e.key === "ArrowUp") { e.preventDefault(); items[(i - 1 + items.length) % items.length].focus(); }
  });
  window.addEventListener("resize", closeMenu);
}

export { initSelects, styleSelect };

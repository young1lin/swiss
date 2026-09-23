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

/* The select (docs/46 §2.5): the panel-wide custom dropdown, moved here from dropdown.ts at
 * P1b-2 so the library owns the one control every form has.
 *
 * A native <select>'s CLOSED box can be styled, but its option POPUP cannot - on Windows it is
 * the system list, white in both themes, and it is the one control in the panel that never
 * matched. So every select's UI is replaced here: the native element stays in the DOM, hidden,
 * as the source of truth (value, options, change events all keep their meaning - call sites
 * never change), and a trigger button plus a floating menu render its face.
 *
 * Attached automatically: initSelects styles every <select> already in the DOM and a
 * MutationObserver styles each one the moment it enters (the sheets and the Data view build
 * theirs dynamically), so a view never opts in or out. A page therefore builds a plain
 * h("select", ...) and gets this face; there is no select() builder to call.
 *
 * The open dropdown is this module's own state (openState), like the menu's open flag in
 * ui/menu.ts: one at a time, closed by a click outside, Escape, Tab, a resize, or its trigger
 * leaving the DOM. */
import { h } from "../h.js";

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

let openState: { menu: HTMLElement; trig: HTMLButtonElement; sel: HTMLSelectElement; items: HTMLButtonElement[] } | null = null;

// A styled select's face, by select: removal handling needs it (below).
const trigs = new WeakMap<HTMLSelectElement, HTMLButtonElement>();

/** A styled select that leaves the DOM takes its face with it. The trigger sits BESIDE the
 *  select ("afterend"), so removing the select alone orphans a live-looking dropdown -
 *  docs/22 closeout B7 caught one on a redis page still showing the previous pg connection's
 *  schema pick, because the view removes the select when the connection kind changes. */
function dropTrig(sel: HTMLSelectElement): void {
  const t = trigs.get(sel);
  if (t && t.parentNode) t.parentNode.removeChild(t);
}

function closeSelect(): void {
  if (!openState) return;
  openState.menu.remove();
  openState.trig.setAttribute("aria-expanded", "false");
  openState = null;
}

/** Whether a dropdown's option list is open. */
function selectOpen(): boolean { return openState != null; }

/** The trigger's label mirrors the selected option's text (not its value). */
function paint(sel: HTMLSelectElement, trig: HTMLButtonElement): void {
  descs();
  const opt = sel.options.item(sel.selectedIndex); // null when nothing is selected (-1)
  const label = trig.querySelector<HTMLElement>(".dd-label");
  if (label) label.textContent = opt ? opt.textContent : "";
  if (disDesc) trig.disabled = disDesc.get.call(sel);
}

function openMenuFor(sel: HTMLSelectElement, trig: HTMLButtonElement): void {
  closeSelect();
  const menu = h("div", { class: "menu float dd-menu", role: "listbox" });
  menu.style.minWidth = trig.offsetWidth + "px";
  const items: HTMLButtonElement[] = [];
  let mark: HTMLButtonElement | null = null;
  for (const o of Array.from(sel.options)) {
    const text = o.textContent;
    const b = h("button", {
      type: "button", class: "pick" + (o.selected ? " on" : ""), role: "option",
      title: text, aria: { selected: String(o.selected) },
    }, text);
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
      closeSelect();
      trig.focus();
    };
    menu.appendChild(b);
    items.push(b);
    if (o.selected) mark = b;
  }
  if (!items.length) return; // nothing to choose - the trigger stays inert
  document.body.appendChild(menu);

  // Below the trigger, unless that runs off the window - then above it. Same policy as popupMenu.
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
  // Carry the select's own classes over: the per-view sizing rules (.db-pagesize, ...) then shape
  // the trigger exactly as they shaped the select it stands in for.
  const trig = h("button", {
    type: "button", class: "dd" + (ownClasses ? " " + ownClasses : ""),
    title: sel.title || undefined,
    aria: { label: sel.getAttribute("aria-label"), haspopup: "listbox", expanded: "false" },
  }, h("span", { class: "dd-label" }), h("span", { class: "dd-chev" }));

  trig.onclick = () => {
    if (trig.disabled) return;
    if (openState && openState.trig === trig) closeSelect();
    else openMenuFor(sel, trig);
  };
  // The keyboard contract of a native select, on the trigger: open on Enter/Space/arrows. A key
  // that opened the list was the trigger's and goes no further: an ArrowDown that opened it also
  // walked the MCP sidebar behind the Add sheet (found live on 19997, docs/46 P1b-2).
  trig.onkeydown = (e) => {
    if (trig.disabled) return;
    if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      openMenuFor(sel, trig);
    }
  };
  return trig;
}

/** Take over one select. Idempotent; skips multi-selects, which have no dropdown face. */
function styleSelect(sel: HTMLSelectElement | null | undefined): void {
  descs();
  if (!sel || !valDesc || sel.multiple || sel.dataset.ddDone) return; // no DOM select here - a stub
  sel.dataset.ddDone = "1";
  const ownClasses = sel.className; // captured BEFORE dd-native is stamped on, or the trigger inherits the hider
  sel.classList.add("dd-native"); // hidden; the trigger beside it is the face

  const trig = buildTrigger(sel, ownClasses);
  sel.insertAdjacentElement("afterend", trig);
  paint(sel, trig);

  // Programmatic writes must reach the face. The instance-level overrides shadow the prototype
  // accessors, so every existing "sel.value = x" / "sel.innerHTML = ..." repaints the label.
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

/** The open list's keys. On document in the CAPTURE phase, so it runs before every bubbling
 *  listener - main.ts's Escape chain and sidebar arrows among them - wherever focus sits.
 *
 *  Escape and the arrows are the list's own and go no further. Before P1b-2 this listener
 *  closed first and then read openState.trig, which closeSelect had just nulled: every Escape
 *  threw, focus never came back to the trigger, and the key fell through to main.ts, which
 *  closed the sheet the dropdown sat in along with whatever was typed into it. The arrows fell
 *  through the same way and moved the MCP selection behind the list.
 *
 *  Tab closes and hands focus back to the trigger but keeps its default, so focus then moves
 *  on from the trigger - where it goes when a native select is tabbed out of. */
function onKey(e: KeyboardEvent): void {
  if (!openState) return;
  const { trig, items } = openState;
  if (e.key === "Escape" || e.key === "Tab") {
    closeSelect();
    trig.focus();
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); }
    return;
  }
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  e.preventDefault();
  e.stopPropagation();
  const i = items.indexOf(document.activeElement as HTMLButtonElement);
  const n = e.key === "ArrowDown" ? i + 1 : i - 1;
  items[(n + items.length) % items.length].focus();
}

/** Style everything already in the DOM, then watch for selects the views create later. Every
 *  global this wires is feature-detected: the module must import cleanly under the DOM-stubbed
 *  Node of the panel boot test, which has no MutationObserver. */
function initSelects(): void {
  document.querySelectorAll("select").forEach((s) => { styleSelect(s); });
  if (typeof MutationObserver !== "undefined") new MutationObserver((muts) => {
    for (const m of muts) {
      // Node lists carry every node kind; nodeType 1 is the element test that works under
      // every DOM this module imports through (real, happy-dom, and the boot test's stubs,
      // which have no Element class to instanceof against). The downcast then names what
      // the walk needs - nodeType 1 already promised an element.
      for (const n of m.addedNodes) {
        if (n.nodeType !== 1) continue;
        const added = n as Element;
        if (added.tagName === "SELECT") styleSelect(added as HTMLSelectElement);
        else added.querySelectorAll("select").forEach((s) => { styleSelect(s); });
      }
      // docs/22 closeout B7: a removed styled select must not leave its trigger behind.
      for (const n of m.removedNodes) {
        if (n.nodeType !== 1) continue;
        const gone = n as Element;
        if (gone.tagName === "SELECT") dropTrig(gone as HTMLSelectElement);
        else gone.querySelectorAll("select").forEach((s) => { dropTrig(s); });
      }
    }
    // A re-render can remove an open trigger's subtree; its menu would be left floating.
    if (openState && !document.body.contains(openState.trig)) closeSelect();
  }).observe(document.body, { childList: true, subtree: true });

  // One open dropdown at a time, closed by any click outside it.
  document.addEventListener("mousedown", (e) => {
    if (openState && !openState.menu.contains(e.target as Node) && e.target !== openState.trig) closeSelect();
  });
  document.addEventListener("keydown", onKey, true);
  window.addEventListener("resize", closeSelect);
}

export { closeSelect, initSelects, selectOpen, styleSelect };

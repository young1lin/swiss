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

/* The floating menu (SPEC §panel.ui): the one popup every "⋯", switcher and row menu opens.
 * Moved here from menu.ts (popupMenu, clampMenuPos) and pane.ts (closeMenu) with the row
 * shapes from types/dom.d.ts, so the library owns the whole mechanism: build, place, focus,
 * walk with the arrows, close.
 *
 * The open flag is the menu's own state, like the dropdown's (ui/select.ts): main.ts must not
 * reload the panel under an open menu and Escape closes a menu before anything else, and both
 * ask menuOpen(). Every menu in the panel is built here: the MCP pane's overflow became
 * anchoredMenu (SPEC §panel.pages) and Data's hand-built .ctx-menu popups became popupMenu.
 * setMenuOpen stays exported for the suites that reset the flag between cases. */
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { iconNode } from "./icon.js";
import { heldDot } from "./status.js";

/** One popupMenu row. The union is load-bearing: a separator is { sep: true } with NO
 *  label/fn (tunnels.ts was the strict-mode error that proved it), an action is label+fn with
 *  optional styling flags. pick/on drive the checked-mark row styles (danger reds the item);
 *  popupMenu never reads a field the arm does not carry. */
                                 
                
                                
                   
                 
               
                 
                                                                                              
                                                                                       
                
                
                                                                               
                                                                                         
                                                                                         
                                                                           
                
                
                                                                                                
                                                                                       
                                                            
                     
                                                                                               
                                                                                      
                                                                                     
                  
                      
                                                                                                    
                                                                         
                    
              
 

                              
            
 

                                                    

/** popupMenu's options. */
                           
                                                                                                 
                                                                                               
                                                                             
                  
 

let open = false;
let returnFocus                     = null;

/** Whether a menu holds the pointer: outside clicks and Escape close it first. */
export function menuOpen()          { return open; }

/** For the two hand-built menus above only; popupMenu and closeMenu keep it themselves. */
export function setMenuOpen(v         )       { open = v; }

/** Close whatever menu is open: there is only ever one, #menu. */
export function closeMenu()       {
  const node = document.getElementById("menu");
  if (node) node.remove();
  open = false;
}

/** Where an anchored menu lands, clamped to the viewport: growing right from the anchor's
 *  left edge and down from 4px below its bottom, flipping above the anchor when the box
 *  would run off the bottom, never closer than 8px to an edge. A right-click passes the
 *  cursor POINT as the anchor ({left, top, bottom} all the cursor): Data's cell menus and the
 *  MCP sidebar's row menu do. Pure, so the clamp pins in a test of its own. */
function clampMenuPos(anchor                                               , w        , h        , vw        , vh        )                                {
  const below = anchor.bottom + 4;
  return {
    left: Math.max(8, Math.min(anchor.left, vw - w - 8)),
    top: below + h > vh - 8 ? Math.max(8, anchor.top - h - 4) : below,
  };
}

/* --- a menu anchored to a button ---------------------------------------------------------------
   The pane's overflow menu anchors to .pane-actions; menus raised from the sidebar have no such
   anchor, so they are positioned against the button that opened them. Items are
   { label, fn, danger, sep, pick, on }. Keyboard (SPEC §panel.nav): the menu is a real menu —
   first item focused on open, arrows walk the items, Escape closes — so a page switcher
   built on it needs no second menu idiom. */
/** The rows of a menu, as popupMenu and anchoredMenu both draw them. */
function menuNode(cls        , items            )              {
  return h("div", { class: cls, id: "menu" }, h("div", { class: "menu-rows" }, menuRows(items)));
}

function menuRows(items            )                {
  return items.map((it)              => {
    if (it.sep) return h("hr");
    // SPEC §data.databases: a heading is chrome, not a choice — a plain div, so it can neither take
    // focus from the first real item nor answer a click.
    if (it.heading) return h("div", { class: "menu-head", data: { heading: "" } }, it.label);
    // Order is the object card's (SPEC §data.tabs): the type glyph first, then the word, then a
    // trailing affordance for a row that opens ANOTHER menu (SPEC §panel.design - a direction about
    // what happens next), then the held-writes dot last, a fact about the row. A refused row
    // (SPEC §data.databases: a database the server will not browse) is shown disabled, its reason in
    // the title, and a disabled button runs no fn.
    const b = h("button", {
      type: "button", class: [it.pick ? "pick" : "", it.on ? "on" : "", it.danger ? "danger" : ""].filter(Boolean).join(" ") || undefined,
      title: it.title, disabled: it.disabled,
    }, it.icon ? iconNode(it.icon) : null, it.label, it.affordance ? iconNode(it.affordance) : null, it.dot ? heldDot(null) : null);
    b.onclick = (ev) => { ev.stopPropagation(); closeMenu(); if (!b.disabled) it.fn(); };
    return b;
  });
}

/** A menu with a find box (MenuOpts.filter): the box over a list that scrolls under it, and a
 *  quiet line for a text nothing holds. A heading or a separator has no label to match, so a
 *  typed text leaves them out. Clicks inside stay inside: the document's click closes an open
 *  menu, and a click into the box is not a choice. */
function findNode(cls        , items            , placeholder        )              {
  const box = h("input", {
    class: "menu-filter", type: "search", autocomplete: "off", spellcheck: false,
    placeholder, aria: { label: placeholder },
  })                    ;
  const list = h("div", { class: "menu-rows" }, menuRows(items));
  const none = h("div", { class: "menu-head", hidden: true }, tr("ui.menu.none"));
  box.addEventListener("input", () => {
    const q = box.value.trim().toLowerCase();
    let any = false;
    for (const row of Array.from(list.children)                 ) {
      // A row's title counts too: a model menu's rows read names, their ids one hover away.
      const hit = !q || (row.tagName === "BUTTON" && ((row.textContent || "") + " " + row.title).toLowerCase().includes(q));
      row.hidden = !hit;
      if (hit && row.tagName === "BUTTON") any = true;
    }
    // Keep only headings whose own group still has a match. Search never leaves empty bands.
    const rows = Array.from(list.children)                 ;
    rows.forEach((row, i) => {
      if (!row.hasAttribute("data-heading")) return;
      let end = i + 1;
      while (end < rows.length && !rows[end].hasAttribute("data-heading") && rows[end].tagName !== "HR") end++;
      row.hidden = !rows.slice(i + 1, end).some(r => r.tagName === "BUTTON" && !r.hidden);
    });
    none.hidden = any;
    list.scrollTop = 0;
  });
  const node = h("div", { class: cls + " find", id: "menu" }, box, list, none);
  node.addEventListener("click", (ev) => { ev.stopPropagation(); });
  return node;
}

/** Roles and keys, once the menu is in the document: the first item focused, arrows walk the
 *  items, Escape closes. It used to guard every DOM call (and menuNode used createElement, not
 *  h()) for four suites that drove popupMenu on hand-rolled micro-DOMs; three moved to
 *  happy-dom and the fourth's stub grew the calls (SPEC §panel.pages), so the guards are gone. */
function wireMenu(node             )       {
  open = true;
  node.setAttribute("role", "menu");
  const all = Array.from(node.querySelectorAll                   ("button")).filter((b) => !b.disabled);
  all.forEach((b) => { b.setAttribute("role", "menuitem"); });
  // A find box takes the focus and leads the walk; the rows a typed text left out drop from it.
  const find = node.querySelector                  ("input.menu-filter");
  const ring = ()                => {
    const shown = all.filter((b) => !b.hidden);
    return find ? [find, ...shown] : shown;
  };
  const first = ring();
  if (first.length) first[0].focus();
  node.addEventListener("keydown", (ev) => {
    const walk = ring();
    const i = walk.indexOf(document.activeElement               );
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      // The arrows are the menu's own, like Escape below: left to bubble, they also reached
      // main.ts's sidebar walk, which moved the MCP selection behind an open menu - and
      // selecting an MCP closes every menu, so the first ArrowDown shut the one in use.
      ev.preventDefault();
      ev.stopPropagation();
      if (!walk.length) return;
      let n = ev.key === "ArrowDown" ? i + 1 : i - 1;
      if (n < 0) n = walk.length - 1;
      if (n >= walk.length) n = 0;
      walk[n].focus();
      walk[n].scrollIntoView?.({ block: "nearest" });
    } else if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      const back = returnFocus;
      closeMenu();
      if (back?.isConnected) back.focus();
    } else if (find && ev.target === find && ev.key === "Enter") {
      ev.preventDefault();
      ev.stopPropagation();
      if (walk.length > 1) walk[1].click();
    } else if (find && ev.target !== find && ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
      // A letter typed on a row goes on into the box, which takes it as it lands.
      find.focus();
    }
  });
}

/** A floating menu was placed against a point - a button's rect or a right-click - that a
 *  scrolling box carries away, so a scroll anywhere but inside the menu closes it, as a native
 *  context menu does (the select list, which keeps its trigger, follows it instead: ui/select.ts).
 *  Wired once, on the first popup; capture, because scroll does not bubble. */
let scrollWired = false;
function closeOnScroll()       {
  if (scrollWired || typeof document.addEventListener !== "function") return;
  scrollWired = true;
  window.addEventListener("resize", closeMenu);
  document.addEventListener("scroll", (ev) => {
    const node = document.getElementById("menu");
    if (!open || !node || !node.classList.contains("float")) return;
    if (ev.target && node.contains(ev.target        )) return;
    closeMenu();
  }, true);
}

function popupMenu(anchor                                                               , items            , o           = {})       {
  closeMenu();
  returnFocus = document.activeElement                      ;
  closeOnScroll();
  const node = o.filter ? findNode("menu float", items, o.filter) : menuNode("menu float", items);
  document.body.appendChild(node);
  node.style.maxHeight = Math.max(0, window.innerHeight - 16) + "px";
  node.style.maxWidth = Math.max(0, window.innerWidth - 16) + "px";
  // A menu dropped from a row never sits narrower than the row itself (found live on
  // 19998: the connection menu measured 160px against its 233px sidebar row - the CSS
  // content floor won and the dropdown floated 73px short of the thing that opened it).
  // The anchor's width joins the CSS floor through max(), so a small anchor (a toolbar
  // overflow button) keeps the stylesheet's 160px unchanged.
  if (anchor.width) node.style.minWidth = window.innerWidth - 16 < Math.max(160, Math.ceil(anchor.width))
    ? Math.max(0, window.innerWidth - 16) + "px" : "max(160px, " + Math.ceil(anchor.width) + "px)";
  // Aligned to the button's LEFT edge and growing right, over the detail pane. Right-aligning it
  // instead pushed a sidebar menu back across the list it was opened from, hiding those rows.
  // The clamp itself is clampMenuPos (SPEC §data).
  const r = node.getBoundingClientRect();
  const pos = clampMenuPos(anchor, r.width, r.height, window.innerWidth, window.innerHeight);
  node.style.left = pos.left + "px";
  // Below the button, unless that would run off the bottom — then above it.
  node.style.top = pos.top + "px";
  wireMenu(node);
}

/** A menu owned by a page head, right-aligned to it and clamped to the viewport.
 *  Fixed positioning prevents a short head or a clipped ancestor from hiding its rows.
 *  Scrolling or resizing closes it; keys and focus restoration match popupMenu. */
function anchoredMenu(host             , items            )       {
  closeMenu();
  returnFocus = document.activeElement                      ;
  closeOnScroll();
  const node = menuNode("menu float", items);
  host.appendChild(node);
  node.style.maxHeight = Math.max(0, window.innerHeight - 16) + "px";
  node.style.maxWidth = Math.max(0, window.innerWidth - 16) + "px";
  const anchor = host.getBoundingClientRect(), size = node.getBoundingClientRect();
  const pos = clampMenuPos({ left: anchor.right - size.width, top: anchor.top, bottom: anchor.bottom }, size.width, size.height, window.innerWidth, window.innerHeight);
  node.style.left = pos.left + "px"; node.style.top = pos.top + "px"; node.style.right = "auto";
  wireMenu(node);
}

export { anchoredMenu, clampMenuPos, popupMenu };

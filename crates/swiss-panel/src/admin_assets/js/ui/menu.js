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

/* The floating menu (docs/46 §2.5): the one popup every "⋯", switcher and row menu opens.
 * Moved here from menu.ts (popupMenu, clampMenuPos) and pane.ts (closeMenu) with the row
 * shapes from types/dom.d.ts, so the library owns the whole mechanism: build, place, focus,
 * walk with the arrows, close.
 *
 * The open flag is the menu's own state, like the dropdown's (ui/select.ts): main.ts must not
 * reload the panel under an open menu and Escape closes a menu before anything else, and both
 * ask menuOpen(). Two menus are still built by hand and set it themselves - the MCP pane's
 * overflow (it lives inside #pane and answers #pane's delegated click; P2 rebuilds it) and
 * Data's context menus (.ctx-menu, P7) - which is the only reason setMenuOpen is exported. */
import { iconNode } from "./icon.js";

/** One popupMenu row. The union is load-bearing: a separator is { sep: true } with NO
 *  label/fn (tunnels.ts was the strict-mode error that proved it), an action is label+fn with
 *  optional styling flags. pick/on drive the checked-mark row styles (danger reds the item);
 *  popupMenu never reads a field the arm does not carry. */
                                 
                
                                
                   
                 
               
                 
                                                                                         
                                                                                       
                
                
                                                                               
                                                                                         
                                                                                         
                                                                           
                
                
                                                                                      
                                                                                       
                                                            
                     
                                                                                         
                                                                                      
                                                                                     
                  
                      
                                                                                          
                                                                         
                    
              
 

                              
            
 

                                                    

let open = false;

/** Whether a menu holds the pointer: outside clicks and Escape close it first. */
export function menuOpen()          { return open; }

/** For the two hand-built menus above only; popupMenu and closeMenu keep it themselves. */
export function setMenuOpen(v         )       { open = v; }

/** Close whatever menu is open: the anchored one (#menu) and any Data context menu. */
export function closeMenu()       {
  const node = document.getElementById("menu");
  if (node) node.remove();
  document.querySelectorAll(".ctx-menu").forEach((m) => { m.remove(); });
  open = false;
}

/** Where an anchored menu lands, clamped to the viewport: growing right from the anchor's
 *  left edge and down from 4px below its bottom, flipping above the anchor when the box
 *  would run off the bottom, never closer than 8px to an edge. A right-click passes the
 *  cursor POINT as the anchor ({left, top, bottom} all the cursor). popupMenu's arithmetic,
 *  factored out so the ctx menus (data-csv.js) and the Table menu (data-edit.js) clamp the
 *  same way instead of landing off-screen at an edge. Pure. */
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
   { label, fn, danger, sep, pick, on }. Keyboard (docs/13 D5): the menu is a real menu —
   first item focused on open, arrows walk the items, Escape closes — so a page switcher
   built on it needs no second menu idiom. */
function popupMenu(anchor                                                               , items            )       {
  closeMenu();
  const node = document.createElement("div");
  node.className = "menu float";
  node.id = "menu";
  items.forEach((it) => {
    if (it.sep) { node.appendChild(document.createElement("hr")); return; }
    // docs/43 M3: a heading is chrome, not a choice — a plain div, so it can neither take
    // focus from the first real item nor answer a click.
    if (it.heading) {
      const h = document.createElement("div");
      h.className = "menu-head";
      h.textContent = it.label;
      node.appendChild(h);
      return;
    }
    const cls = (it.pick ? "pick" : "") + (it.on ? " on" : "") + (it.danger ? " danger" : "");
    const b = document.createElement("button");
    b.type = "button";
    b.className = cls.trim();
    b.textContent = it.label;
    if (it.title) b.title = it.title;
    // docs/43 M3: shown but refused — the database selector's browsable:false rows carry
    // the server's reason in title; a disabled button cannot be clicked, so no fn runs.
    if (it.disabled) b.disabled = true;
    // docs/43 M1: the same type glyph and dirty dot the object's card carries, on the menu
    // row that stands in for it. Order is the card's order: glyph first, dot last.
    if (it.icon) b.insertBefore(iconNode(it.icon), b.firstChild);
    // fix-plan #14: a trailing affordance for rows that open ANOTHER menu - the caret class
    // of promise, retired from the label copy. Painted after the dot for the same reason the
    // dot paints last: it is a direction about what happens next, not a fact about the row.
    if (it.affordance) b.appendChild(iconNode(it.affordance));
    if (it.dot) {
      const d = document.createElement("span");
      d.className = "db-tab-dot";
      b.appendChild(d);
    }
    b.onclick = (ev) => { ev.stopPropagation(); closeMenu(); if (!b.disabled) it.fn(); };
    node.appendChild(b);
  });
  document.body.appendChild(node);
  // A menu dropped from a row never sits narrower than the row itself (found live on
  // 19998: the connection menu measured 160px against its 233px sidebar row - the CSS
  // content floor won and the dropdown floated 73px short of the thing that opened it).
  // The anchor's width joins the CSS floor through max(), so a small anchor (a toolbar
  // overflow button) keeps the stylesheet's 160px unchanged.
  if (anchor.width) node.style.minWidth = "max(160px, " + Math.ceil(anchor.width) + "px)";
  // Aligned to the button's LEFT edge and growing right, over the detail pane. Right-aligning it
  // instead pushed a sidebar menu back across the list it was opened from, hiding those rows.
  // The clamp itself is clampMenuPos — shared with the ctx menus (docs/22 closeout audit).
  const r = node.getBoundingClientRect();
  const pos = clampMenuPos(anchor, r.width, r.height, window.innerWidth, window.innerHeight);
  node.style.left = pos.left + "px";
  // Below the button, unless that would run off the bottom — then above it.
  node.style.top = pos.top + "px";
  open = true;
  // Roles and keys, guarded: four suites still drive this menu on a hand-rolled micro-DOM
  // with no querySelectorAll, focus or setAttribute (admin-revisions, admin-row-menu,
  // admin-data-redis-cellmenu, admin-logs-pagination). It is also why the menu is built with
  // createElement rather than h(): those stubs never derive textContent from a text child.
  // Both go when the suites move to happy-dom with the pages that own them (P2 MCP, P7 Data).
  if (typeof node.setAttribute === "function") node.setAttribute("role", "menu");
  const buttons = typeof node.querySelectorAll === "function"
    ? Array.prototype.slice.call(node.querySelectorAll                   ("button"))
      .filter((b                   ) => !b.disabled)
    : [];
  buttons.forEach((b) => { if (b.setAttribute) b.setAttribute("role", "menuitem"); });
  if (buttons[0] && typeof buttons[0].focus === "function") buttons[0].focus();
  if (typeof node.addEventListener === "function") {
    node.addEventListener("keydown", (ev) => {
      const i = buttons.indexOf(document.activeElement                     );
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        // The arrows are the menu's own, like Escape below: left to bubble, they also reached
        // main.ts's sidebar walk, which moved the MCP selection behind an open menu - and
        // selecting an MCP closes every menu, so the first ArrowDown shut the one in use.
        ev.preventDefault();
        ev.stopPropagation();
        let n = ev.key === "ArrowDown" ? i + 1 : i - 1;
        if (n < 0) n = buttons.length - 1;
        if (n >= buttons.length) n = 0;
        if (buttons[n] && typeof buttons[n].focus === "function") buttons[n].focus();
      } else if (ev.key === "Escape") {
        ev.stopPropagation();
        closeMenu();
      }
    });
  }
}

export { clampMenuPos, popupMenu };

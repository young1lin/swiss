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
 * anchoredMenu (SPEC §panel.pages) and Data's hand-built .ctx-menu popups became popupMenu (P7).
 * setMenuOpen stays exported for the suites that reset the flag between cases. */
import { h } from "../h.js";
import { iconNode } from "./icon.js";
import { heldDot } from "./status.js";

/** One popupMenu row. The union is load-bearing: a separator is { sep: true } with NO
 *  label/fn (tunnels.ts was the strict-mode error that proved it), an action is label+fn with
 *  optional styling flags. pick/on drive the checked-mark row styles (danger reds the item);
 *  popupMenu never reads a field the arm does not carry. */
export interface MenuItemAction {
  label: string;
  fn: (ev?: MouseEvent) => void;
  danger?: boolean;
  pick?: boolean;
  on?: boolean;
  title?: string;
  /* SPEC §data.tabs: a row can carry the type glyph and the dirty dot its card does — the tab
   *  strip's overflow lists open objects, and the menu is the whole set's one read. */
  icon?: string;
  dot?: boolean;
  /* The drawer follow-up: a row can carry a MARK (the dialect word, painted by
   * typeTagNode - glyph for whitelisted dialects, mono word otherwise) and a META (a dim
   * trailing value like a table count). popupMenu ignores both; the sidebar drawers read
   * them, because a drawer row has room a one-line menu label does not. */
  mark?: string;
  meta?: string;
  /* SPEC §data.databases: a row the menu shows but refuses to run — the database selector lists
   *  every database the instance names, browsable or not, so the reason (title) is one
   *  hover away instead of the row simply being missing. */
  disabled?: boolean;
  /* SPEC §panel.design: a trailing glyph for rows that open ANOTHER menu (the "Table" row that
   *  used to spell its caret in the label) - the leading icon field is the row's TYPE
   *  glyph; this one is a direction, painted at the end of the row like the caret it
   *  replaces. */
  affordance?: string;
  /* SPEC §data.databases: a non-interactive heading row (a connection GROUP name, "system" bands) —
   *  styled like the menu's own chrome, never focused, never clicked. */
  heading?: boolean;
  sep?: never;
}

export interface MenuItemSep {
  sep: true;
}

export type MenuItem = MenuItemSep | MenuItemAction;

let open = false;

/** Whether a menu holds the pointer: outside clicks and Escape close it first. */
export function menuOpen(): boolean { return open; }

/** For the two hand-built menus above only; popupMenu and closeMenu keep it themselves. */
export function setMenuOpen(v: boolean): void { open = v; }

/** Close whatever menu is open: there is only ever one, #menu. */
export function closeMenu(): void {
  const node = document.getElementById("menu");
  if (node) node.remove();
  open = false;
}

/** Where an anchored menu lands, clamped to the viewport: growing right from the anchor's
 *  left edge and down from 4px below its bottom, flipping above the anchor when the box
 *  would run off the bottom, never closer than 8px to an edge. A right-click passes the
 *  cursor POINT as the anchor ({left, top, bottom} all the cursor): Data's cell menus and the
 *  MCP sidebar's row menu do. Pure, so the clamp pins in a test of its own. */
function clampMenuPos(anchor: { left: number; top: number; bottom: number }, w: number, h: number, vw: number, vh: number): { left: number; top: number } {
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
function menuNode(cls: string, items: MenuItem[]): HTMLElement {
  return h("div", { class: cls, id: "menu" }, items.map((it): HTMLElement => {
    if (it.sep) return h("hr");
    // SPEC §data.databases: a heading is chrome, not a choice — a plain div, so it can neither take
    // focus from the first real item nor answer a click.
    if (it.heading) return h("div", { class: "menu-head" }, it.label);
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
  }));
}

/** Roles and keys, once the menu is in the document: the first item focused, arrows walk the
 *  items, Escape closes. It used to guard every DOM call (and menuNode used createElement, not
 *  h()) for four suites that drove popupMenu on hand-rolled micro-DOMs; three moved to
 *  happy-dom and the fourth's stub grew the calls (SPEC §panel.pages), so the guards are gone. */
function wireMenu(node: HTMLElement): void {
  open = true;
  node.setAttribute("role", "menu");
  const buttons = Array.from(node.querySelectorAll<HTMLButtonElement>("button")).filter((b) => !b.disabled);
  buttons.forEach((b) => { b.setAttribute("role", "menuitem"); });
  if (buttons[0]) buttons[0].focus();
  node.addEventListener("keydown", (ev) => {
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      // The arrows are the menu's own, like Escape below: left to bubble, they also reached
      // main.ts's sidebar walk, which moved the MCP selection behind an open menu - and
      // selecting an MCP closes every menu, so the first ArrowDown shut the one in use.
      ev.preventDefault();
      ev.stopPropagation();
      let n = ev.key === "ArrowDown" ? i + 1 : i - 1;
      if (n < 0) n = buttons.length - 1;
      if (n >= buttons.length) n = 0;
      buttons[n].focus();
    } else if (ev.key === "Escape") {
      ev.stopPropagation();
      closeMenu();
    }
  });
}

function popupMenu(anchor: { left: number; top: number; bottom: number; width?: number }, items: MenuItem[]): void {
  closeMenu();
  const node = menuNode("menu float", items);
  document.body.appendChild(node);
  // A menu dropped from a row never sits narrower than the row itself (found live on
  // 19998: the connection menu measured 160px against its 233px sidebar row - the CSS
  // content floor won and the dropdown floated 73px short of the thing that opened it).
  // The anchor's width joins the CSS floor through max(), so a small anchor (a toolbar
  // overflow button) keeps the stylesheet's 160px unchanged.
  if (anchor.width) node.style.minWidth = "max(160px, " + Math.ceil(anchor.width) + "px)";
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

/** The same menu hung INSIDE a positioned box - a page head's .pane-actions - right-aligned
 *  under it, instead of floating on <body>. A resource's ⋯ sits at the pane's right edge,
 *  where a float growing right from the button would run off-screen; hung in the pinned head,
 *  it also rides the head when the page scrolls. Rows, keys and closing are popupMenu's. */
function anchoredMenu(host: HTMLElement, items: MenuItem[]): void {
  closeMenu();
  const node = menuNode("menu", items);
  host.appendChild(node);
  wireMenu(node);
}

export { anchoredMenu, clampMenuPos, popupMenu };

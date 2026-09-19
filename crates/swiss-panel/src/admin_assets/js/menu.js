/*
 * Copyright 2026 The swiss authors
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

                                                
                                               
import { $, dotTitle, typeTagHtml } from "./util.js";
import { closeMenu } from "./pane.js";
import { groupedMcps, sideCfg, visibleMcps } from "./sidebar.js";
import { mountGroup } from "./groups.js";
import { currentView, draggingGroupName, draggingRow, foldMap, listFilter, setMenuOpen } from "./ui-state.js";
import { mcpBusyVerb, mcpRows, selectedMcp } from "./mcp-state.js";

/* --- a menu anchored to a button ---------------------------------------------------------------
   The pane's overflow menu anchors to .pane-actions; menus raised from the sidebar have no such
   anchor, so they are positioned against the button that opened them. Items are
   { label, fn, danger, sep, pick, on }. Keyboard (docs/13 D5): the menu is a real menu —
   first item focused on open, arrows walk the items, Escape closes — so a page switcher
   built on it needs no second menu idiom. */
function popupMenu(anchor                                               , items            )       {
  closeMenu();
  const node = document.createElement("div");
  node.className = "menu float";
  node.id = "menu";
  items.forEach((it) => {
    if (it.sep) { node.appendChild(document.createElement("hr")); return; }
    const cls = (it.pick ? "pick" : "") + (it.on ? " on" : "") + (it.danger ? " danger" : "");
    const b = document.createElement("button");
    b.type = "button";
    b.className = cls.trim();
    b.textContent = it.label;
    if (it.title && b.title !== undefined) b.title = it.title;
    b.onclick = (ev) => { ev.stopPropagation(); closeMenu(); it.fn(); };
    node.appendChild(b);
  });
  document.body.appendChild(node);
  // Aligned to the button's LEFT edge and growing right, over the detail pane. Right-aligning it
  // instead pushed a sidebar menu back across the list it was opened from, hiding those rows.
  // The clamp itself is clampMenuPos — shared with the ctx menus (docs/22 closeout audit).
  const r = node.getBoundingClientRect();
  const pos = clampMenuPos(anchor, r.width, r.height, window.innerWidth, window.innerHeight);
  node.style.left = pos.left + "px";
  // Below the button, unless that would run off the bottom — then above it.
  node.style.top = pos.top + "px";
  setMenuOpen(true);
  // Roles and keys (guarded: the vitest micro-DOM has neither querySelectorAll nor focus).
  if (node.setAttribute) node.setAttribute("role", "menu");
  const buttons = typeof node.querySelectorAll === "function"
    ? Array.prototype.slice.call(node.querySelectorAll                   ("button")) : [];
  buttons.forEach((b) => { if (b.setAttribute) b.setAttribute("role", "menuitem"); });
  if (buttons[0] && typeof buttons[0].focus === "function") buttons[0].focus();
  if (typeof node.addEventListener === "function") {
    node.addEventListener("keydown", (ev) => {
      const i = buttons.indexOf(document.activeElement                     );
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
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

/** A sidebar row is one line, so everything that used to be crammed onto a second one — the
 *  description, the source, the latency, the reason a row is red — lives here, and in the pane
 *  header for the selected MCP. */
function tooltipOf(m           )         {
  // Idle is the one state word that names no behaviour of its own (docs/18 V6): a lazy proc
  // has no child yet and wakes on the first request — say that, so a hollow ring explains itself.
  const stateWord = mcpBusyVerb(m.name) ? mcpBusyVerb(m.name) + "…"
    : m.state === "idle" ? "idle — lazy: no child yet, wakes on the first request"
    : m.state === "stopped" ? "disabled"
    : m.state;
  // docs/24: the endpoint path shown to the operator carries the /mcp/ domain prefix.
  const bits = ["/mcp/" + m.name, m.type, m.source, stateWord];
  if (m.latencyMs != null) bits.push(m.latencyMs + " ms");
  if (m.description) bits.unshift(m.description);
  if (m.reason) bits.push(m.reason);
  bits.push("right-click for actions"); // docs/28 D3: the row menu has no button of its own
  return bits.join("  ·  ");
}

/** Patch existing rows in place; only rebuild when the visible set changes. Keeps scroll, keeps
 *  focus, and stops the whole list flashing on every poll.
 *
 *  While a drag is in flight (draggingRow()) the rebuild is DEFERRED: a poll landing mid-drag would
 *  replace the DOM under the pointer and silently cancel it. Attribute patching never moves nodes,
 *  so rows stay live; dragend triggers one catch-up rebuild. */
function patchSidebar()       {
  const list = $("list");
  const groups = groupedMcps();
  const rows = visibleMcps();
  // The rebuild key covers everything structural: which groups exist, what is in each, and which are
  // folded shut. Dot colour, latency and selection are patched below and stay out of it on purpose.
  const sig = groups.map((g) => {
    return g.name + "\u0001" + (foldMap()[g.name] && !listFilter().trim() ? "c" : "o") + "\u0001" +
      g.rows.map((m) => { return m.name; }).join("\u0000");
  }).join("\u0002");

  if (list.dataset.sig !== sig && !draggingRow() && !draggingGroupName()) {
    list.innerHTML = "";
    const cfg = sideCfg();
    groups.forEach((g) => { list.appendChild(mountGroup(cfg, g)); });
    list.dataset.sig = sig;
  }
  rows.forEach((m) => {
    const node = list.querySelector             ('[data-name="' + (window.CSS && CSS.escape ? CSS.escape(m.name) : m.name) + '"]');
    if (!node) return;
    const busyVerb = mcpBusyVerb(m.name);
    const word = busyVerb ? "starting" : m.state;
    const dot = node.querySelector             (".dot");
    if (dot) {
      dot.className = "dot " + word;
      // The title rides the same patch pass as the class (docs/18 V6): the poll never
      // rebuilds the sidebar, so a title painted only at build time would go stale with the
      // first state change and never move again.
      dot.title = dotTitle(word, m.latencyMs, m.reason);
    }
    // Patched rather than set at build time: an Edit that switches an MCP from npx to http keeps the
    // same name, so the row is never rebuilt and the trailing label would otherwise go stale.
    const tagEl = node.querySelector             (".side-type") ;
    const tag = m.tag || m.type || "";
    // docs/29: a mapped tag renders its glyph (the word rides the aria-label); anything else
    // keeps the text chip exactly as before. innerHTML, not textContent — icon() is markup.
    tagEl.innerHTML = tag ? typeTagHtml(tag) : "";
    if (tag) tagEl.setAttribute("data-tag", tag);
    else tagEl.removeAttribute("data-tag");
    node.title = tooltipOf(m);
    node.setAttribute("aria-selected", selectedMcp() === m.name ? "true" : "false");
  });

  let up = 0, bad = 0;
  mcpRows().forEach((m) => {
    if (m.state === "up") up++;
    else if (m.state === "down" || m.state === "error") bad++;
  });
  // The chip belongs to whichever view is on screen; the tunnel and jobs views count their own
  // rows (updateCountChip owns those), so the MCP text must not overwrite them mid-poll.
  if (currentView() === "mcps") {
    $("countChip").textContent = mcpRows().length + " MCPs · " + up + " up" + (bad ? " · " + bad + " down" : "");
  }
  // Group headers name the sections now, so the standing "MCPS" caption is noise; it earns its line
  // only while a search is on, where the match count is the useful part.
  const cap = $("sideCap");
  cap.textContent = rows.length + " matching";
  cap.hidden = !listFilter();
}

export { clampMenuPos, patchSidebar, popupMenu, tooltipOf };

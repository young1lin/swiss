import { $, dotTitle, state } from "./util.js";
import { closeMenu } from "./pane.js";
import { groupOf, groupedMcps, rowOf, sideCfg, visibleMcps } from "./sidebar.js";
import { mountGroup } from "./groups.js";

/* --- a menu anchored to a button ---------------------------------------------------------------
   The pane's overflow menu anchors to .pane-actions; menus raised from the sidebar have no such
   anchor, so they are positioned against the button that opened them. Items are
   { label, fn, danger, sep, pick, on }. Keyboard (docs/13 D5): the menu is a real menu —
   first item focused on open, arrows walk the items, Escape closes — so a page switcher
   built on it needs no second menu idiom. */
function popupMenu(anchor, items) {
  closeMenu();
  var node = document.createElement("div");
  node.className = "menu float";
  node.id = "menu";
  items.forEach(function (it) {
    if (it.sep) { node.appendChild(document.createElement("hr")); return; }
    var cls = (it.pick ? "pick" : "") + (it.on ? " on" : "") + (it.danger ? " danger" : "");
    var b = document.createElement("button");
    b.type = "button";
    b.className = cls.trim();
    b.textContent = it.label;
    if (it.title && b.title !== undefined) b.title = it.title;
    b.onclick = function (ev) { ev.stopPropagation(); closeMenu(); it.fn(); };
    node.appendChild(b);
  });
  document.body.appendChild(node);
  // Aligned to the button's LEFT edge and growing right, over the detail pane. Right-aligning it
  // instead pushed a sidebar menu back across the list it was opened from, hiding those rows.
  // The clamp itself is clampMenuPos — shared with the ctx menus (docs/22 closeout audit).
  var r = node.getBoundingClientRect();
  var pos = clampMenuPos(anchor, r.width, r.height, window.innerWidth, window.innerHeight);
  node.style.left = pos.left + "px";
  // Below the button, unless that would run off the bottom — then above it.
  node.style.top = pos.top + "px";
  state.menuOpen = true;
  // Roles and keys (guarded: the vitest micro-DOM has neither querySelectorAll nor focus).
  if (node.setAttribute) node.setAttribute("role", "menu");
  var buttons = typeof node.querySelectorAll === "function"
    ? Array.prototype.slice.call(node.querySelectorAll("button")) : [];
  buttons.forEach(function (b) { if (b.setAttribute) b.setAttribute("role", "menuitem"); });
  if (buttons[0] && typeof buttons[0].focus === "function") buttons[0].focus();
  if (typeof node.addEventListener === "function") {
    node.addEventListener("keydown", function (ev) {
      var i = buttons.indexOf(document.activeElement);
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        var n = ev.key === "ArrowDown" ? i + 1 : i - 1;
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
function clampMenuPos(anchor, w, h, vw, vh) {
  var below = anchor.bottom + 4;
  return {
    left: Math.max(8, Math.min(anchor.left, vw - w - 8)),
    top: below + h > vh - 8 ? Math.max(8, anchor.top - h - 4) : below,
  };
}

/** A sidebar row is one line, so everything that used to be crammed onto a second one — the
 *  description, the source, the latency, the reason a row is red — lives here, and in the pane
 *  header for the selected MCP. */
function tooltipOf(m) {
  // Idle is the one state word that names no behaviour of its own (docs/18 V6): a lazy proc
  // has no child yet and wakes on the first request — say that, so a hollow ring explains itself.
  var stateWord = state.busy[m.name] ? state.busy[m.name] + "…"
    : m.state === "idle" ? "idle — lazy: no child yet, wakes on the first request"
    : m.state === "stopped" ? "disabled"
    : m.state;
  // docs/24: the endpoint path shown to the operator carries the /mcp/ domain prefix.
  var bits = ["/mcp/" + m.name, m.type, m.source, stateWord];
  if (m.latencyMs != null) bits.push(m.latencyMs + " ms");
  if (m.description) bits.unshift(m.description);
  if (m.reason) bits.push(m.reason);
  bits.push("right-click for actions"); // docs/28 D3: the row menu has no button of its own
  return bits.join("  ·  ");
}

/** Patch existing rows in place; only rebuild when the visible set changes. Keeps scroll, keeps
 *  focus, and stops the whole list flashing on every poll.
 *
 *  While a drag is in flight (state.dragging) the rebuild is DEFERRED: a poll landing mid-drag would
 *  replace the DOM under the pointer and silently cancel it. Attribute patching never moves nodes,
 *  so rows stay live; dragend triggers one catch-up rebuild. */
function patchSidebar() {
  var list = $("list");
  var groups = groupedMcps();
  var rows = visibleMcps();
  // The rebuild key covers everything structural: which groups exist, what is in each, and which are
  // folded shut. Dot colour, latency and selection are patched below and stay out of it on purpose.
  var sig = groups.map(function (g) {
    return g.name + "\u0001" + (state.collapsed[g.name] && !state.filter.trim() ? "c" : "o") + "\u0001" +
      g.rows.map(function (m) { return m.name; }).join("\u0000");
  }).join("\u0002");

  if (list.dataset.sig !== sig && !state.dragging && !state.draggingGroup) {
    list.innerHTML = "";
    var cfg = sideCfg();
    groups.forEach(function (g) { list.appendChild(mountGroup(cfg, g)); });
    list.dataset.sig = sig;
  }
  rows.forEach(function (m) {
    var node = list.querySelector('[data-name="' + (window.CSS && CSS.escape ? CSS.escape(m.name) : m.name) + '"]');
    if (!node) return;
    var busyVerb = state.busy[m.name];
    var word = busyVerb ? "starting" : m.state;
    var dot = node.querySelector(".dot");
    if (dot) {
      dot.className = "dot " + word;
      // The title rides the same patch pass as the class (docs/18 V6): the poll never
      // rebuilds the sidebar, so a title painted only at build time would go stale with the
      // first state change and never move again.
      dot.title = dotTitle(word, m.latencyMs, m.reason);
    }
    // Patched rather than set at build time: an Edit that switches an MCP from npx to http keeps the
    // same name, so the row is never rebuilt and the trailing label would otherwise go stale.
    var tagEl = node.querySelector(".side-type");
    var tag = m.tag || m.type || "";
    tagEl.textContent = tag;
    if (tag) tagEl.setAttribute("data-tag", tag);
    else tagEl.removeAttribute("data-tag");
    node.title = tooltipOf(m);
    node.setAttribute("aria-selected", state.selected === m.name ? "true" : "false");
  });

  var up = 0, bad = 0;
  state.mcps.forEach(function (m) {
    if (m.state === "up") up++;
    else if (m.state === "down" || m.state === "error") bad++;
  });
  // The chip belongs to whichever view is on screen; the tunnel and jobs views count their own
  // rows (updateCountChip owns those), so the MCP text must not overwrite them mid-poll.
  if (state.view === "mcps") {
    $("countChip").textContent = state.mcps.length + " MCPs · " + up + " up" + (bad ? " · " + bad + " down" : "");
  }
  // Group headers name the sections now, so the standing "MCPS" caption is noise; it earns its line
  // only while a search is on, where the match count is the useful part.
  var cap = $("sideCap");
  cap.textContent = rows.length + " matching";
  cap.hidden = !state.filter;
}

export { clampMenuPos, patchSidebar, popupMenu, tooltipOf };

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

                                                
import { $, dotTitle, typeTagNode } from "./util.js";
import { fill } from "./h.js";
import { groupedMcps, sideCfg, visibleMcps } from "./sidebar.js";
import { mountGroup } from "./groups.js";
import { currentView, draggingGroupName, draggingRow, foldMap, listFilter } from "./ui-state.js";
import { mcpBusyVerb, mcpRows, selectedMcp } from "./mcp-state.js";
import { locale, tr } from "./i18n.js";

/** A sidebar row is one line, so everything that used to be crammed onto a second one — the
 *  description, the source, the latency, the reason a row is red — lives here, and in the pane
 *  header for the selected MCP. */
function tooltipOf(m           )         {
  // Idle is the one state word that names no behaviour of its own (SPEC §panel.design): a lazy proc
  // has no child yet and wakes on the first request — say that, so a hollow ring explains itself.
  const stateWord = mcpBusyVerb(m.name) ? mcpBusyVerb(m.name) + "…"
    : m.state === "idle" ? tr("menu.idleLazyChildWakes")
    : m.state === "stopped" ? tr("menu.disabled")
    : m.state;
  // SPEC §mcp.endpoint: the endpoint path shown to the operator carries the /mcp/ domain prefix.
  const bits = ["/mcp/" + m.name, m.type, m.source, stateWord];
  if (m.latencyMs != null) bits.push(m.latencyMs + " ms");
  if (m.description) bits.unshift(m.description);
  if (m.reason) bits.push(m.reason);
  bits.push(tr("menu.rightClickActions")); // SPEC §mcp.revisions: the row menu has no button of its own
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
  // The language is in it too: the group bands' words (the empty line, the + and ⋯ labels) are
  // built once per rebuild, so without it a 文/A flip left them in the old language until the
  // membership next changed (found live on 19998 in a second-language pass (SPEC §panel.ui)).
  const sig = locale() + "\u0003" + groups.map((g) => {
    return g.name + "\u0001" + (foldMap()[g.name] && !listFilter().trim() ? "c" : "o") + "\u0001" +
      g.rows.map((m) => { return m.name; }).join("\u0000");
  }).join("\u0002");

  if (list.dataset.sig !== sig && !draggingRow() && !draggingGroupName()) {
    fill(list);
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
      // The title rides the same patch pass as the class (SPEC §panel.design): the poll never
      // rebuilds the sidebar, so a title painted only at build time would go stale with the
      // first state change and never move again.
      dot.title = dotTitle(word, m.latencyMs, m.reason);
    }
    // Patched rather than set at build time: an Edit that switches an MCP from npx to http keeps the
    // same name, so the row is never rebuilt and the trailing label would otherwise go stale.
    const tagEl = node.querySelector             (".side-type") ;
    const tag = m.tag || m.type || "";
    // SPEC §mcp.panel: a mapped tag renders its glyph (the word rides the aria-label); anything else
    // keeps the text chip exactly as before. fill() so the tag string lands as a text node.
    fill(tagEl, tag ? typeTagNode(tag) : null);
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
    $("countChip").textContent = bad
    ? tr("menu.nMcpsBadDown", { n: mcpRows().length, up, bad })
    : tr("menu.nMcps", { n: mcpRows().length, up });
  }
  // Group headers name the sections now, so the standing "MCPS" caption is noise; it earns its line
  // only while a search is on, where the match count is the useful part.
  const cap = $("sideCap");
  cap.textContent = tr("menu.nMatching", { n: rows.length });
  cap.hidden = !listFilter();
}

export { patchSidebar, tooltipOf };
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

import { state } from "./util.js";
import { openSheet } from "./add-sheet.js";
import { act, openDetail, removeMcp, renameMcp } from "./detail.js";
import { patchSidebar, popupMenu } from "./menu.js";
import { loadList } from "./polling.js";
import { assignMember, groupOf as makeGroupOf, newGroupFlow, saveGroupNames, saveOrder, slice } from "./groups.js";

/* --- rendering: sidebar ----------------------------------------------------------------------- */
/** The MCP side of the mcps scope: row rendering, the flat order and the glue between the
 *  panel's MCP state and the groups component. Everything a grouped list IS lives in
 *  groups.js now (docs/20); what stayed here is what only the MCP list knows — which row
 *  fields exist, what opening one does, and the chip text. */
function rowOf(name) {
  return state.mcps.find(function (m) { return m.name === name; });
}
function visibleMcps() {
  var f = state.filter.trim().toLowerCase();
  if (!f) return state.mcps;
  return state.mcps.filter(function (m) {
    return m.name.toLowerCase().indexOf(f) >= 0
      || String(m.type).toLowerCase().indexOf(f) >= 0
      || String(m.tag || "").toLowerCase().indexOf(f) >= 0 // "npx"/"uvx"/"http" finds a launch method
      || String(m.group || "").toLowerCase().indexOf(f) >= 0; // ...and a group name finds its members
  });
}

/** The group a row renders under — the one rule, shared with the server: the stored group
 *  while it exists, else the FIRST group (that slot, never a name, is the sink). */
function groupOf(m) {
  return makeGroupOf(state.groups)(m);
}

/** The sidebar's shape: one flat order sliced by group — which is why moving an MCP between
 *  groups never has to rewrite the ordering. */
function groupedMcps() {
  var fn = makeGroupOf(state.groups);
  var rows = visibleMcps();
  var sliced = slice(rows, state.groups, fn);
  return state.filter.trim() ? sliced.filter(function (g) { return g.rows.length; }) : sliced;
}

/** Rows in the order the eye sees them: group by group, skipping what is folded shut. Arrow-key
 *  navigation follows this, not the flat list — otherwise Down would jump into a collapsed group. */
function navRows() {
  var out = [];
  groupedMcps().forEach(function (g) {
    if (!state.collapsed[g.name] || state.filter.trim()) out = out.concat(g.rows);
  });
  return out;
}

/* --- sidebar drag-to-reorder ------------------------------------------------------------------ */

/** Persist the current list order. The server ranks /api/mcps by it and appends unknown names, so
 *  the panel and every other consumer agree on one order. */
function saveOrderFlat() {
  saveOrder("mcps", state.mcps.map(function (m) { return m.name; }))
    .catch(function () { /* the next reorder retries; the list is already right locally */ });
}

/** Move 'name' to just before/after 'target' in state.mcps, re-render, persist. Works on the FULL
 *  list (not the filtered view), so reordering with a search active does not shuffle the rest. */
function moveRow(name, target, before) {
  if (!name || !target || name === target) return;
  var item = state.mcps.find(function (m) { return m.name === name; });
  if (!item) return;
  state.mcps = state.mcps.filter(function (m) { return m.name !== name; });
  var to = state.mcps.findIndex(function (m) { return m.name === target; });
  if (to < 0) state.mcps.push(item); // target vanished mid-drag (deleted by a poll) — land at the end
  else state.mcps.splice(before ? to : to + 1, 0, item);
  patchSidebar();
  saveOrderFlat();
}

/** Swap the selected MCP with its neighbour IN ITS OWN GROUP (Alt+Up / Alt+Down), then persist.
 *  Deliberately stops at the group edge: a keystroke that silently re-homed an MCP would be a
 *  surprise, and dragging is right there for that. */
function nudgeSelected(up) {
  var sel = rowOf(state.selected);
  if (!sel) return false;
  var g = groupOf(sel);
  var rows = visibleMcps().filter(function (m) { return groupOf(m) === g; });
  var i = rows.findIndex(function (m) { return m.name === state.selected; });
  var j = up ? i - 1 : i + 1;
  if (i < 0 || j < 0 || j >= rows.length) return false;
  var a = state.mcps.findIndex(function (m) { return m.name === rows[i].name; });
  var b = state.mcps.findIndex(function (m) { return m.name === rows[j].name; });
  var tmp = state.mcps[a]; state.mcps[a] = state.mcps[b]; state.mcps[b] = tmp;
  patchSidebar();
  saveOrderFlat();
  return true;
}

/* --- the component configuration ---------------------------------------------------------------- */

/** One sidebar row: dot, name, launch tag. One line — the description, source and latency live
 *  in the tooltip and the pane header. Click opens the detail pane; drag is wired by the
 *  groups component (reorder + re-home in one gesture). Right-click raises the row's action
 *  menu (docs/28 D3): the row itself is a <button>, and a button cannot nest the ellipsis
 *  button the Jobs/Tunnels rows use — the ctx-menu anchor pattern (data-csv.js) fits instead,
 *  and the tooltip says so. */
function sideRowNode(m) {
  var b = document.createElement("button");
  b.className = "side-row";
  b.type = "button";
  b.dataset.name = m.name;
  b.setAttribute("role", "option");
  var dot = document.createElement("span");
  dot.className = "dot";
  b.appendChild(dot);
  var nm = document.createElement("span");
  nm.className = "side-name";
  nm.textContent = m.name;
  b.appendChild(nm);
  var tag = document.createElement("span");
  tag.className = "side-type"; // http / rest / npx / uvx — filled by the patch pass
  b.appendChild(tag);
  b.onclick = function () { openDetail(m.name); };
  b.addEventListener("contextmenu", function (ev) {
    if (ev.preventDefault) ev.preventDefault();
    // The cursor point as anchor — the same shape the data grid's ctx menus pass.
    var pt = { left: ev.clientX || 0, top: ev.clientY || 0, bottom: ev.clientY || 0 };
    rowMenu(m.name, pt);
  });
  return b;
}

/** The row's right-click menu (docs/28 D3): the three verbs the operator asked to have
 *  within reach — rename, disable/enable by state, delete. The label is read live from
 *  state.mcps (rowOf), never off the row's render-time snapshot, so a poll that flipped the
 *  lifecycle cannot make the menu offer the wrong verb. */
function rowMenu(name, anchor) {
  var m = rowOf(name) || { name: name, lifecycle: "stopped" };
  var started = m.lifecycle === "started";
  popupMenu(anchor, [
    { label: "Rename…", fn: function () { renameMcp(name); } },
    { label: started ? "Disable" : "Enable", fn: function () { act(name, started ? "stop" : "start"); } },
    { sep: true },
    { label: "Delete", danger: true, fn: function () { removeMcp(name); } },
  ]);
}

/** The mcps scope's cfg for mountGroup (see groups.js for the full contract). Built fresh each
 *  render so 'names' and 'collapsed' always reference the live state objects. */
function sideCfg() {
  return {
    scope: "mcps",
    density: "side",
    names: state.groups,
    collapsed: state.collapsed,
    noun: "MCP",
    addTitle: function (g) { return "Add an MCP to " + g; },
    onAdd: function (g) { openSheet(g); },
    reload: function () { return loadList(); },
    render: patchSidebar,
    afterDrag: patchSidebar, // the catch-up rebuild after a drag ends
    drag: {
      get: function () { return state.dragging; },
      set: function (v) { state.dragging = v; },
    },
    dragGroup: {
      get: function () { return state.draggingGroup; },
      set: function (v) { state.draggingGroup = v; },
    },
    rowNode: sideRowNode,
    rowId: function (m) { return m.name; },
    rowsById: function () { return state.mcps; },
    groupOfRow: groupOf,
    onMoveRow: moveRow,
    onAssign: assignGroup,
    filtered: !!state.filter.trim(),
  };
}

/* --- group mutations (the pane's menu and the add sheet still call these) ------------------------ */

/** Send the whole group list. Create, reorder and delete are all "here is the new list". */
async function saveGroups(next) {
  var j = await saveGroupNames("mcps", next);
  if (!j) return false;
  state.groups = j.groups || [];
  await loadList();
  return true;
}

function newGroup() {
  newGroupFlow("mcps", state.groups, function () { return loadList(); });
}

/** Move one MCP into a group. Applied locally first so the row jumps immediately, then persisted.
 *  The group name goes over as-is — 'default' is a real name now, and null (no explicit group)
 *  is only sent by the pane's "remove from group" paths, which the server reads as "first group". */
async function assignGroup(name, group) {
  var m = rowOf(name);
  if (!m || groupOf(m) === (group || state.groups[0])) return;
  m.group = group;
  patchSidebar();
  var j = await assignMember("mcps", name, group);
  if (!j) { await loadList(); return; } // rejected — take the server's word for it
  m.group = j.group;
  patchSidebar();
}

export { assignGroup, groupOf, groupedMcps, moveRow, navRows, newGroup, nudgeSelected, rowMenu, rowOf, saveGroups, sideCfg, sideRowNode, visibleMcps };

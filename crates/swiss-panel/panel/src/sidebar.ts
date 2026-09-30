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

import type { ApiMcpRow } from "./types/api.js";
import type { GroupCfg, GroupSlice, GroupedRow } from "./types/dom.js";
import { openSheet } from "./add-sheet.js";
import { act, openDetail, removeMcp, renameMcp } from "./detail.js";
import { patchSidebar } from "./menu.js";
import { loadList } from "./polling.js";
import { addTitle, assignMember, groupOf as makeGroupOf, newGroupFlow, saveGroupNames, saveOrder, slice } from "./groups.js";
import { draggingGroupName, draggingRow, foldMap, listFilter, setDraggingGroupName, setDraggingRow } from "./ui-state.js";
import { mcpGroups, mcpRows, selectedMcp, setMcpGroups, setMcpRows } from "./mcp-state.js";
import { tr } from "./i18n.js";
import { dot, popupMenu, sideRow } from "./ui/index.js";
import type { DotState } from "./ui/index.js";

/* --- rendering: sidebar ----------------------------------------------------------------------- */
/** The MCP side of the mcps scope: row rendering, the flat order and the glue between the
 *  panel's MCP state and the groups component. Everything a grouped list IS lives in
 *  groups.js now (SPEC §host.groups); what stayed here is what only the MCP list knows — which row
 *  fields exist, what opening one does, and the chip text. */
function rowOf(name: string): ApiMcpRow | undefined {
  return mcpRows().find((m) => { return m.name === name; });
}
function visibleMcps(): ApiMcpRow[] {
  const f = listFilter().trim().toLowerCase();
  if (!f) return mcpRows();
  return mcpRows().filter((m) => {
    return m.name.toLowerCase().includes(f)
      || String(m.type).toLowerCase().includes(f)
      || String(m.tag || "").toLowerCase().includes(f) // "npx"/"uvx"/"http" finds a launch method
      || String(m.group || "").toLowerCase().includes(f); // ...and a group name finds its members
  });
}

/** The group a row renders under — the one rule, shared with the server: the stored group
 *  while it exists, else the FIRST group (that slot, never a name, is the sink). */
function groupOf(m: GroupedRow): string {
  return makeGroupOf(mcpGroups())(m);
}

/** The sidebar's shape: one flat order sliced by group — which is why moving an MCP between
 *  groups never has to rewrite the ordering. */
function groupedMcps(): GroupSlice<ApiMcpRow>[] {
  const fn = makeGroupOf(mcpGroups());
  const rows = visibleMcps();
  const sliced = slice(rows, mcpGroups(), fn);
  return listFilter().trim() ? sliced.filter((g) => { return g.rows.length; }) : sliced;
}

/** Rows in the order the eye sees them: group by group, skipping what is folded shut. Arrow-key
 *  navigation follows this, not the flat list — otherwise Down would jump into a collapsed group. */
function navRows(): ApiMcpRow[] {
  let out: ApiMcpRow[] = [];
  groupedMcps().forEach((g) => {
    if (!foldMap()[g.name] || listFilter().trim()) out = out.concat(g.rows);
  });
  return out;
}

/* --- sidebar drag-to-reorder ------------------------------------------------------------------ */

/** Persist the current list order. The server ranks /api/mcps by it and appends unknown names, so
 *  the panel and every other consumer agree on one order. */
function saveOrderFlat(): void {
  saveOrder("mcps", mcpRows().map((m) => { return m.name; }))
    .catch(() => { /* the next reorder retries; the list is already right locally */ });
}

/** Move 'name' to just before/after 'target' in mcpRows(), re-render, persist. Works on the FULL
 *  list (not the filtered view), so reordering with a search active does not shuffle the rest. */
function moveRow(name: string, target: string, before?: boolean): void {
  if (!name || !target || name === target) return;
  const item = mcpRows().find((m) => { return m.name === name; });
  if (!item) return;
  setMcpRows(mcpRows().filter((m) => { return m.name !== name; }));
  const to = mcpRows().findIndex((m) => { return m.name === target; });
  if (to < 0) mcpRows().push(item); // target vanished mid-drag (deleted by a poll) — land at the end
  else mcpRows().splice(before ? to : to + 1, 0, item);
  patchSidebar();
  saveOrderFlat();
}

/** Swap the selected MCP with its neighbour IN ITS OWN GROUP (Alt+Up / Alt+Down), then persist.
 *  Deliberately stops at the group edge: a keystroke that silently re-homed an MCP would be a
 *  surprise, and dragging is right there for that. */
function nudgeSelected(up: boolean): boolean {
  const sel = rowOf(selectedMcp()!);
  if (!sel) return false;
  const g = groupOf(sel);
  const rows = visibleMcps().filter((m) => { return groupOf(m) === g; });
  const i = rows.findIndex((m) => { return m.name === selectedMcp(); });
  const j = up ? i - 1 : i + 1;
  if (i < 0 || j < 0 || j >= rows.length) return false;
  const a = mcpRows().findIndex((m) => { return m.name === rows[i].name; });
  const b = mcpRows().findIndex((m) => { return m.name === rows[j].name; });
  const tmp = mcpRows()[a]; mcpRows()[a] = mcpRows()[b]; mcpRows()[b] = tmp;
  patchSidebar();
  saveOrderFlat();
  return true;
}

/* --- the component configuration ---------------------------------------------------------------- */

/** One sidebar row: dot, name, launch tag. One line — the description, source and latency live
 *  in the tooltip and the pane header. Click opens the detail pane; drag is wired by the
 *  groups component (reorder + re-home in one gesture). Right-click raises the row's action
 *  menu (SPEC §mcp.revisions): the row itself is a <button>, and a button cannot nest the ellipsis
 *  button the Jobs/Tunnels rows use — a menu at the cursor fits instead, the way the data
 *  grid's cell menus open, and the tooltip says so. */
function sideRowNode(m: ApiMcpRow): HTMLButtonElement {
  // The library's source-list row (SPEC §panel.ui). Built once per rebuild key; the dot's state and
  // title, the trailing tag and the row title are painted by the patch pass (menu.ts
  // patchSidebar), which keeps them current between rebuilds.
  const b = sideRow({ name: m.name, lead: dot(m.state as DotState, m.state), data: { name: m.name } });
  b.onclick = () => { openDetail(m.name); };
  b.addEventListener("contextmenu", (ev) => {
    if (ev.preventDefault) ev.preventDefault();
    // The cursor point as anchor — the same shape the data grid's cell menus pass.
    const pt = { left: ev.clientX || 0, top: ev.clientY || 0, bottom: ev.clientY || 0 };
    rowMenu(m.name, pt);
  });
  return b;
}

/** The row's right-click menu (SPEC §mcp.revisions): the three verbs the operator asked to have
 *  within reach — rename, disable/enable by state, delete. The label is read live from
 *  mcpRows() (rowOf), never off the row's render-time snapshot, so a poll that flipped the
 *  lifecycle cannot make the menu offer the wrong verb. */
function rowMenu(name: string, anchor: { left: number; top: number; bottom: number }): void {
  const m = rowOf(name) || { name: name, lifecycle: "stopped" };
  const started = m.lifecycle === "started";
  popupMenu(anchor, [
    { label: tr("sidebar.rename"), fn: () => { void renameMcp(name); } },
    { label: started ? tr("sidebar.disable") : tr("sidebar.enable"), fn: () => { void act(name, started ? "stop" : "start"); } },
    { sep: true },
    { label: tr("sidebar.delete"), danger: true, fn: () => { void removeMcp(name); } },
  ]);
}

/** The mcps scope's cfg for mountGroup (see groups.js for the full contract). Built fresh each
 *  render so 'names' and 'collapsed' always reference the live state objects. */
function sideCfg(): GroupCfg<ApiMcpRow> {
  return {
    scope: "mcps",
    density: "side",
    names: mcpGroups(),
    collapsed: foldMap(),
    noun: tr("sidebar.mcp"),
    addTitle: (g) => { return addTitle(tr("sidebar.mcp"), g); },
    onAdd: (g) => { openSheet(g); },
    reload: () => { return loadList(); },
    render: patchSidebar,
    afterDrag: patchSidebar, // the catch-up rebuild after a drag ends
    drag: {
      get: () => { return draggingRow(); },
      set: (v) => { setDraggingRow(v); },
    },
    dragGroup: {
      get: () => { return draggingGroupName(); },
      set: (v) => { setDraggingGroupName(v); },
    },
    rowNode: sideRowNode,
    rowId: (m) => { return m.name; },
    rowsById: () => { return mcpRows(); },
    groupOfRow: groupOf,
    onMoveRow: moveRow,
    onAssign: assignGroup,
    filtered: !!listFilter().trim(),
  };
}

/* --- group mutations (the pane's menu and the add sheet still call these) ------------------------ */

/** Send the whole group list. Create, reorder and delete are all "here is the new list". */
async function saveGroups(next: string[]): Promise<boolean> {
  const j = await saveGroupNames("mcps", next);
  if (!j) return false;
  setMcpGroups(j.groups || []);
  await loadList();
  return true;
}

function newGroup(): void {
  newGroupFlow("mcps", mcpGroups(), () => { return loadList(); });
}

/** Move one MCP into a group. Applied locally first so the row jumps immediately, then persisted.
 *  The group name goes over as-is — 'default' is a real name now, and null (no explicit group)
 *  is only sent by the pane's "remove from group" paths, which the server reads as "first group". */
async function assignGroup(name: string, group?: string | null): Promise<void> {
  const m = rowOf(name);
  if (!m || groupOf(m) === (group || mcpGroups()[0])) return;
  m.group = group;
  patchSidebar();
  const j = await assignMember("mcps", name, group);
  if (!j) { await loadList(); return; } // rejected — take the server's word for it
  m.group = j.group;
  patchSidebar();
}

export { assignGroup, groupOf, groupedMcps, moveRow, navRows, newGroup, nudgeSelected, rowMenu, rowOf, saveGroups, sideCfg, sideRowNode, visibleMcps };

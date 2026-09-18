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

/* ================================================================================================
   Groups - the one grouped-list component (docs/20 §4).

   Seven scopes (mcps, conns, rules, jobs, secrets, tokens, targets), two densities, ONE
   shape (docs/35): a header BAND - chevron, name in mixed case, count, with + always
   visible and the ellipsis on hover - over the members. The band is what says "container";
   there is no folder glyph, no guide line and no second surface. The densities differ only
   in size and in what the members sit on:

     side  - the sidebar: a 28px band, members (.side-row) one grid step in beneath it.
     page  - the card: the .grp is the .group card itself, a 36px band across its top and
             the rows straight under it, edge to edge.

   The WHOLE head is the drag surface for reordering groups: pick it up by the name, the
   count or the empty band. The two buttons on it opt out at dragstart, so a twitch while
   clicking + still lands the click - there is no hover-only grip to find first.

   Before this module the sidebar and the Tunnels page each carried a private copy of the
   whole idea (two drag implementations, two header anatomies, two delete-confirm wordings)
   and they had already forked; Jobs, Secrets and Tokens were about to grow three more.
   Everything a grouped list does lives here once:

   - slice()/groupOf() - one flat order cut into group slices; moving a member between groups
     never rewrites the ordering.
   - the /api/groups/{scope} family - saveGroupNames/rename/assign/saveOrder, one request shape.
   - collapse state per scope in localStorage, keys carried across a rename.
   - mountGroup() - the DOM: header (collapse, +, ellipsis menu with Move/Rename/Delete; the
     head itself drags), drop targets (rows reorder + re-home in one gesture, a head or the
     empty line appends into a group, a whole group lands before/after another), and the
     empty-group line that keeps a fresh group visible.

   The caller keeps what is genuinely its own: the row markup, what a row does when opened,
   the flat order it stores, and the noun the delete confirm names.
   ================================================================================================ */
import { apiJson, el, esc, icon, toast } from "./util.js";
import { addTitle, deleteConfirmMsg, emptyLineText, groupOf, lastGroupKey, resolveDefaultGroup, slice } from "./group-logic.js";
import { openGroupSheet } from "./add-sheet.js";
import { popupMenu } from "./menu.js";

/* --- pure --------------------------------------------------------------------------------------
   groupOf / slice / deleteConfirmMsg / addTitle / resolveDefaultGroup / lastGroupKey live in
   group-logic.js - imported above and re-exported below so every caller keeps one import -
   because the pure half is what test/admin-groups.test.ts pins, and groups.js's own module
   graph (add-sheet, menu) cannot load under a node test. */

/* --- collapse state (per scope, localStorage) ---------------------------------------------------- */

/** Fold state is keyed by group name under swiss.groups.<scope>.collapsed. A rename carries
 *  the key across - otherwise a renamed group springs open. Panel-only state, so it lives
 *  here rather than in any store. */
function collapseKey(scope: string): string { return "swiss.groups." + scope + ".collapsed"; }

function loadCollapsed(scope: string): Record<string, boolean> {
  try { return JSON.parse(localStorage.getItem(collapseKey(scope)) as string) || {}; } catch (e) { return {}; }
}
function saveCollapsed(scope: string, map: Record<string, boolean>): void {
  try { localStorage.setItem(collapseKey(scope), JSON.stringify(map)); } catch (e) { /* full or blocked */ }
}

/** The group picked the last time something was created in this scope - read raw (null when
 *  never set); resolveDefaultGroup decides whether it still means anything. */
function lastGroup(scope: string): string | null {
  try { return localStorage.getItem(lastGroupKey(scope)); } catch (e) { return null; }
}
function rememberGroup(scope: string, name: string): void {
  try { localStorage.setItem(lastGroupKey(scope), name); } catch (e) { /* full or blocked */ }
}

/* --- the /api/groups/{scope} family (docs/20 §3) -------------------------------------------------- */

/** Create, reorder and delete are all "here is the new list"; the server drops omitted
 *  groups and returns their members to the first remaining one. */
async function saveGroupNames(scope: string, next: string[]): Promise<ApiGroupsSetResponse | null> {
  return apiJson("/api/groups/" + scope, { method: "PUT", body: JSON.stringify({ groups: next }) });
}

/** Rename in place: the slot is kept and the members ride along; the answer's moved count is
 *  what the toast reports, so renaming a full group looks like what it was. */
async function renameGroupApi(scope: string, from: string, to: string): Promise<ApiGroupsRenameResponse | null> {
  return apiJson("/api/groups/" + scope + "/rename", { method: "POST", body: JSON.stringify({ from: from, to: to }) });
}

/** Put one member in a group (the canonical name comes back), or null to render it in the
 *  first group. */
async function assignMember(scope: string, id: string, group?: string | null): Promise<ApiGroupAssignResponse | null> {
  return apiJson("/api/groups/" + scope + "/members/" + encodeURIComponent(id),
    { method: "PUT", body: JSON.stringify({ group: group === undefined ? null : group }) });
}

/** The scope's flat order: ids in the order the panel now sees. */
async function saveOrder(scope: string, ids: string[]): Promise<ApiGroupsOrderResponse | null> {
  return apiJson("/api/groups/" + scope + "/order", { method: "PUT", body: JSON.stringify({ order: ids }) });
}

/** The New-group flow every scope shares: the one-field sheet, then the whole list. */
function newGroupFlow(scope: string, names: string[], reload: () => Promise<unknown> | unknown): void {
  openGroupSheet(null, async function (name: string) {
    var j = await saveGroupNames(scope, names.concat([name]));
    if (!j) return false;
    await reload();
    toast("Group " + name + " created");
    return true;
  });
}
/** The Group field of a create sheet: every live group, `sel` selected. The select - not a
 *  hidden promise made by whichever + opened the sheet - is where the row lands, so a value
 *  the user changed wins. #g-sel is the one id every create sheet shares. */
function groupFieldHtml(names: string[], sel?: string): string {
  var opts = names.map(function (n) {
    return '<option value="' + esc(n) + '"' + (n === sel ? " selected" : "") + ">" + esc(n) + "</option>";
  }).join("");
  return '<label class="field"><span>Group</span><select id="g-sel">' + opts + "</select></label>";
}

/* --- the component ------------------------------------------------------------------------------- */

/** Build one group container: the header (disclosure, name, count, +, ellipsis; draggable whole)
 *  and the body (rows, or the empty line that keeps a fresh group visible). All gesture
 *  handling a group owns lives here; cfg carries the caller's own nouns and moves:
 *
 *    scope          "mcps" | "conns" | ... - API paths and the localStorage key
 *    density        "side" (28px header, .side-row children) | "page" (36px header, .group card)
 *    names          the scope's ordered group names (the ellipsis menu's Move edges, the sink)
 *    collapsed      the fold map (state.collapsed / the tunnels tab's own)
 *    noun           what the delete confirm counts ("MCP", "row")
 *    addTitle(g)    the header +'s title, e.g. "Add an MCP to learn"
 *    onAdd(group)   the header + - opens the scope's create flow on that group
 *    reload()       full data reload after a group-list mutation (create/reorder/delete)
 *    render()       re-render this view only (the optimistic local move needs it)
 *    afterDrag()    catch-up render after a drag ends (the loader deferred it)
 *    drag/dragGroup { get, set } - the two in-flight-drag slots; a poll must not rebuild
 *                   under either (the caller's loader checks them)
 *    rowNode(row)   side density: one row element; the component wires click + drag
 *    rowsHtml(g)    page density: the rows as one HTML string, straight into the card body
 *    wireRow(el, row) page density: extra per-row wiring (actions); drag is wired here
 *    rowId(row)     the id a drag carries (name for MCPs, id for tunnels)
 *    rowSel(row)    page density: a selector that finds rowId's node inside the body
 *    rowsById()     live rows, for drop-into's "slot after the last member" step
 *    groupOfRow(row) the rendering group of a row (a groupOf(names) closure)
 *    onMoveRow(id, targetId, before) flat reorder + order PUT + render (caller-owned list)
 *    onAssign(id, group)             member PUT, applied locally first (caller-owned rows)
 *    filtered       a search is on: groups with no match hide, matches force expansion
 */
function mountGroup<Row extends GroupedRow>(cfg: GroupCfg<Row>, g: GroupSlice<Row>): HTMLElement {
  var page = cfg.density === "page";
  // At page density the group IS the card: .group brings the ring, the radius and the clip,
  // so the rows need no second surface under the head (and jobs.js's "is the list painted"
  // probe keeps finding a .group).
  var wrap = el("div", "grp grp--" + cfg.density + (page ? " group" : "") +
    (cfg.collapsed[g.name] && !cfg.filtered ? " collapsed" : ""));
  wrap.dataset.group = g.name;

  var head = el("div", "grp-head");

  // The head's leading columns - chevron, name, count - are the contract base.css aligns
  // to: the members' dot column sits under the band's name (side) or the chevron sits in
  // the rows' dot column and the name over their names (page). aria-expanded says the fold
  // state to assistive tech.
  var toggle = el("button", "grp-toggle");
  toggle.type = "button";
  var folded = !!(cfg.collapsed[g.name] && !cfg.filtered);
  toggle.setAttribute("aria-expanded", String(!folded));
  var chev = el("span", "grp-chev");
  chev.innerHTML = icon("chevron-right");
  toggle.appendChild(chev);
  toggle.appendChild(el("span", "grp-name", g.name));
  // The count stays visible when folded - 0 versus 3 is exactly how a folded empty group
  // tells itself apart from a folded full one.
  toggle.appendChild(el("span", "grp-n", String(g.rows.length)));
  toggle.onclick = function () {
    if (cfg.collapsed[g.name]) delete cfg.collapsed[g.name];
    else cfg.collapsed[g.name] = true;
    saveCollapsed(cfg.scope, cfg.collapsed);
    if (cfg.render) cfg.render();
  };
  head.appendChild(toggle);

  // Adding lives on the group, not on some page-level button: a new item is always added INTO
  // a group, and doing it from here means it lands where you meant it to instead of appearing
  // in the first group to be dragged over afterwards. + stays visible (dimmed) because
  // adding is frequent; one persistent glyph per header is a hierarchy, two would be a toolbar.
  var add = el("button", "grp-add");
  add.innerHTML = icon("plus");
  add.type = "button";
  add.title = cfg.addTitle ? cfg.addTitle(g.name) : "Add to " + g.name;
  add.setAttribute("aria-label", add.title);
  add.onclick = function (ev) { ev.stopPropagation(); cfg.onAdd(g.name); };
  head.appendChild(add);

  // The ellipsis is rare, so it appears on hover/focus only. Move up/down are the
  // keyboard-and-precision path to what dragging the head does: present exactly when the move
  // exists, absent at the list's edges.
  var more = el("button", "grp-more");
  more.innerHTML = icon("ellipsis");
  more.type = "button";
  more.title = "Move, rename or delete this group";
  more.setAttribute("aria-label", "Group actions");
  more.onclick = function (ev) {
    ev.stopPropagation();
    var i = cfg.names.indexOf(g.name);
    var items: MenuItem[] = [];
    if (i > 0) items.push({ label: "Move up", fn: function () { moveGroupBy(cfg, g.name, -1); } });
    if (i >= 0 && i < cfg.names.length - 1) {
      items.push({ label: "Move down", fn: function () { moveGroupBy(cfg, g.name, 1); } });
    }
    if (items.length) items.push({ sep: true });
    items.push(
      { label: "Rename…", fn: function () { renameFlow(cfg, g.name); } },
      { sep: true },
      { label: "Delete group", danger: true, fn: function () { deleteFlow(cfg, g.name); } },
    );
    popupMenu(more.getBoundingClientRect(), items);
  };
  head.appendChild(more);

  wireHeadDrag(cfg, wrap, head, g.name);
  wireIntoDrop(cfg, head, g.name);
  wireGroupDrop(cfg, wrap, g.name);
  wrap.appendChild(head);

  var body = el("div", "grp-body");
  if (page && g.rows.length) {
    body.innerHTML = cfg.rowsHtml!(g);
    g.rows.forEach(function (row) {
      // rowSel, not a data-id: tunnels address rows by data-conn/data-rule, jobs and secrets
      // by their own keys — the caller owns the DOM it built.
      var node = body.querySelector<HTMLElement>(cfg.rowSel!(row));
      if (node) {
        wireRowDrag(cfg, node, row);
        if (cfg.wireRow) cfg.wireRow(node, row); // the caller's own actions on the row
      }
    });
  } else if (cfg.rowNode) {
    g.rows.forEach(function (row) {
      var node = cfg.rowNode!(row);
      wireRowDrag(cfg, node, row);
      body.appendChild(node);
    });
  }
  // An empty group is not an empty state - it is a place. One SHORT quiet line keeps the
  // container visible as a drop target (the only way in); the head's + explains itself on
  // hover, so the line does not have to repeat the instructions. A scope whose rows cannot
  // drag (tokens) says the honest half only.
  if (!g.rows.length && !cfg.filtered) {
    var empty = el("div", "grp-empty", emptyLineText(cfg.draggable !== false));
    // "drop here" has to be true of the line that says it, not only of the head above it.
    wireIntoDrop(cfg, empty, g.name);
    body.appendChild(empty);
  }
  wrap.appendChild(body);
  return wrap;
}

/** The whole head drags the group (docs/35). Picking it up anywhere - the name, the count,
 *  the band - starts a group drag; the browser's own drag threshold keeps a plain click a
 *  click, so the toggle still folds. The + and ellipsis buttons cancel the drag at its
 *  start instead: a hand that moves a pixel while pressing one must still land the click,
 *  and a cancelled dragstart is exactly a mouse-up that clicks. */
function wireHeadDrag<Row extends GroupedRow>(cfg: GroupCfg<Row>, wrap: HTMLElement, head: HTMLElement, group: string): void {
  head.draggable = true;
  head.addEventListener("dragstart", function (e) {
    var t = e.target as HTMLElement | null;
    if (t && t.closest && t.closest(".grp-add, .grp-more")) { e.preventDefault(); return; }
    cfg.dragGroup.set(group);
    wrap.classList.add("dragging");
    try { e.dataTransfer!.setData("text/plain", group); } catch (err) { /* old IE */ }
    e.dataTransfer!.effectAllowed = "move";
  });
  head.addEventListener("dragend", function () {
    cfg.dragGroup.set(null); // lets the deferred rebuild run - same contract as a row drag
    wrap.classList.remove("dragging");
    document.querySelectorAll(".grp.drop-before, .grp.drop-after").forEach(function (n) {
      n.classList.remove("drop-before", "drop-after");
    });
    if (cfg.afterDrag) cfg.afterDrag();
  });
}

/** Row drag: reorder and re-home in one gesture. The insertion point is the hovered row's
 *  half; the group comes from the target row, so a cross-group drag needs no second drop. */
function wireRowDrag<Row extends GroupedRow>(cfg: GroupCfg<Row>, node: HTMLElement, row: Row): void {
  if (cfg.draggable === false) return;
  var id = cfg.rowId(row);
  node.draggable = true;
  node.addEventListener("dragstart", function (e) {
    cfg.drag.set(id);
    node.classList.add("dragging");
    try { e.dataTransfer!.setData("text/plain", id); } catch (err) { /* old IE */ }
    e.dataTransfer!.effectAllowed = "move";
  });
  node.addEventListener("dragover", function (e) {
    if (!cfg.drag.get() || cfg.drag.get() === id) return;
    e.preventDefault();
    e.dataTransfer!.dropEffect = "move";
    var before = e.clientY < node.getBoundingClientRect().top + node.offsetHeight / 2;
    node.classList.toggle("drop-before", before);
    node.classList.toggle("drop-after", !before);
  });
  node.addEventListener("dragleave", function () {
    node.classList.remove("drop-before", "drop-after");
  });
  node.addEventListener("drop", function (e) {
    e.preventDefault();
    var before = e.clientY < node.getBoundingClientRect().top + node.offsetHeight / 2;
    var dragged = cfg.drag.get();
    // Order first, then membership: the move re-renders from the flat list, and doing it
    // after the reassignment would land the row at whatever slot it happened to hold.
    if (dragged && dragged !== id) {
      cfg.onMoveRow(dragged, id, before);
      cfg.onAssign(dragged, cfg.groupOfRow(row));
    }
  });
  node.addEventListener("dragend", function () {
    cfg.drag.set(null); // lets the deferred rebuild run
    node.classList.remove("dragging");
    document.querySelectorAll(".drop-before, .drop-after").forEach(function (r) {
      r.classList.remove("drop-before", "drop-after");
    });
    if (cfg.afterDrag) cfg.afterDrag();
  });
}

/** A ROW dropped on the head (or on the empty line) appends to this group - the only way
 *  into a group with no rows yet. Group drags pass through untouched: the whole .grp
 *  answers those (wireGroupDrop), so a head never has to know which kind it is under. */
function wireIntoDrop<Row extends GroupedRow>(cfg: GroupCfg<Row>, node: HTMLElement, group: string): void {
  node.addEventListener("dragover", function (e) {
    if (!cfg.drag.get()) return;
    e.preventDefault();
    e.dataTransfer!.dropEffect = "move";
    node.classList.add("drop-into");
  });
  node.addEventListener("dragleave", function () {
    node.classList.remove("drop-into");
  });
  node.addEventListener("drop", function (e) {
    var id = cfg.drag.get();
    if (!id) return; // a group drop: let it bubble to the .grp
    e.preventDefault();
    node.classList.remove("drop-into");
    cfg.drag.set(null);
    dropInto(cfg, id, group);
  });
}

/** Another GROUP dragged over this one lands before or after it - the hovered half of the
 *  WHOLE group decides, head and members alike, so the target is the block the user sees
 *  moving and not a 36px strip of it. Row drags never reach here with a group in flight;
 *  the rows and the head handle their own kind and let this one bubble. */
function wireGroupDrop<Row extends GroupedRow>(cfg: GroupCfg<Row>, wrap: HTMLElement, group: string): void {
  var half = function (e: DragEvent) {
    var r = wrap.getBoundingClientRect();
    return e.clientY < r.top + r.height / 2;
  };
  wrap.addEventListener("dragover", function (e) {
    var dg = cfg.dragGroup.get();
    if (!dg || dg === group) return;
    e.preventDefault();
    e.dataTransfer!.dropEffect = "move";
    wrap.classList.toggle("drop-before", half(e));
    wrap.classList.toggle("drop-after", !half(e));
  });
  wrap.addEventListener("dragleave", function (e) {
    // Crossing from the head into a row is not leaving the group.
    if (e.relatedTarget && wrap.contains(e.relatedTarget as Node)) return;
    wrap.classList.remove("drop-before", "drop-after");
  });
  wrap.addEventListener("drop", function (e) {
    var dg = cfg.dragGroup.get();
    if (!dg) return;
    e.preventDefault();
    wrap.classList.remove("drop-before", "drop-after");
    cfg.dragGroup.set(null);
    if (dg !== group) moveGroup(cfg, dg, group, half(e));
  });
}

/** Land a dragged row at the end of a group: slot it after that group's last member so the
 *  flat order agrees with what the list now shows, then reassign. */
function dropInto<Row extends GroupedRow>(cfg: GroupCfg<Row>, id: string, group: string): void {
  var members = cfg.rowsById().filter(function (r) {
    return cfg.groupOfRow(r) === group && cfg.rowId(r) !== id;
  });
  if (members.length) cfg.onMoveRow(id, cfg.rowId(members[members.length - 1]), false);
  cfg.onAssign(id, group); // the header's name is always a live group
}

/** Move a group to a new slot in the stored order. The whole list is sent - create, reorder
 *  and delete are all "here is the new list" on this API - and the server keeps the order it
 *  is given, so the panel and every other consumer agree. */
function moveGroup<Row extends GroupedRow>(cfg: GroupCfg<Row>, name: string, target: string, before: boolean): void {
  if (!name || name === target) return;
  var rest = cfg.names.filter(function (g) { return g !== name; });
  var to = rest.indexOf(target);
  if (to < 0) rest.push(name); // target vanished mid-drag - land at the end
  else rest.splice(before ? to : to + 1, 0, name);
  cfg.names = rest;
  if (cfg.render) cfg.render();
  void saveGroupNames(cfg.scope, rest).then(function (j) { if (j) cfg.reload(); });
}

/** Swap a group with its neighbour (the ellipsis menu's Move up/down): the click-precise
 *  and keyboard counterpart of dragging the head. Falls through at the edges, where the
 *  menu does not offer the move. */
function moveGroupBy<Row extends GroupedRow>(cfg: GroupCfg<Row>, name: string, delta: number): void {
  var i = cfg.names.indexOf(name);
  var j = i + delta;
  if (i < 0 || j < 0 || j >= cfg.names.length) return;
  var next = cfg.names.slice();
  var tmp = next[i]; next[i] = next[j]; next[j] = tmp;
  cfg.names = next;
  if (cfg.render) cfg.render();
  void saveGroupNames(cfg.scope, next).then(function (done) { if (done) cfg.reload(); });
}

/** Rename through the one-field sheet; the fold key rides along, or the group springs open. */
function renameFlow<Row extends GroupedRow>(cfg: GroupCfg<Row>, from: string): void {
  openGroupSheet(from, async function (to: string) {
    var j = await renameGroupApi(cfg.scope, from, to);
    if (!j) return false;
    if (cfg.collapsed[from]) { delete cfg.collapsed[from]; cfg.collapsed[to] = true; saveCollapsed(cfg.scope, cfg.collapsed); }
    await cfg.reload();
    toast("Renamed " + from + " → " + to + (j.moved ? " (" + j.moved + " moved)" : ""));
    return true;
  });
}

/** Deleting a group deletes nothing else, so the confirm only has to say where the members
 *  go: the first remaining group - that slot is the server's sink for them. */
function deleteFlow<Row extends GroupedRow>(cfg: GroupCfg<Row>, name: string): void {
  var count = cfg.rowsById().filter(function (r) { return cfg.groupOfRow(r) === name; }).length;
  var rest = cfg.names.filter(function (g) { return g !== name; });
  if (!rest.length) { toast("At least one group must remain"); return; }
  if (count && !confirm(deleteConfirmMsg(name, cfg.names, count, cfg.noun))) return;
  void saveGroupNames(cfg.scope, rest).then(async function (j) {
    if (!j) return;
    await cfg.reload();
    toast("Deleted group " + name);
  });
}

export {
  addTitle, assignMember, deleteConfirmMsg, emptyLineText, groupFieldHtml, groupOf, lastGroup,
  lastGroupKey, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, renameGroupApi,
  resolveDefaultGroup, saveCollapsed, saveGroupNames, saveOrder, slice,
};
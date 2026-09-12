import { DEFAULT_GROUP, api, apiJson, el, icon, saveCollapsed, state, toast } from "./util.js";
import { openGroupSheet, openSheet } from "./add-sheet.js";
import { openDetail } from "./detail.js";
import { patchSidebar, popupMenu, wireDrag } from "./menu.js";
import { loadList } from "./polling.js";

/* --- rendering: sidebar ----------------------------------------------------------------------- */
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

function groupOf(m) { return m && m.group ? m.group : DEFAULT_GROUP; }

/**
 * The sidebar's shape: `default` first, then the custom groups in their stored order, each holding
 * its members in the flat `order` the server already sorted by. One flat order sliced by group is
 * why moving an MCP between groups never has to rewrite the ordering.
 *
 * Empty groups keep their header — you have to be able to see a group you just made in order to drag
 * anything into it. A search is the one thing that hides a group, and only when nothing in it matches.
 */
function groupedMcps() {
  var rows = visibleMcps();
  var byGroup = {};
  rows.forEach(function (m) {
    var g = groupOf(m);
    (byGroup[g] = byGroup[g] || []).push(m);
  });
  var out = [{ name: DEFAULT_GROUP, rows: byGroup[DEFAULT_GROUP] || [] }];
  state.groups.forEach(function (g) { out.push({ name: g, rows: byGroup[g] || [] }); });
  if (state.filter.trim()) out = out.filter(function (g) { return g.rows.length; });
  return out;
}

/** Rows in the order the eye sees them: group by group, skipping what is folded shut. Arrow-key
 *  navigation follows this, not the flat list — otherwise Down would jump into a collapsed group. */
function navRows() {
  var out = [];
  groupedMcps().forEach(function (g) {
    if (!state.collapsed[g.name]) out = out.concat(g.rows);
  });
  return out;
}

/* --- sidebar drag-to-reorder ------------------------------------------------------------------ */

/** Persist the current list order. The server ranks /api/mcps by it and appends unknown names, so
 *  the panel and every other consumer agree on one order. */
function saveOrder() {
  api("/api/order", { method: "PUT", body: JSON.stringify({ order: state.mcps.map(function (m) { return m.name; }) }) })
    .catch(function () { /* the next reorder retries; the list is already right locally */ });
}

/** Move `name` to just before/after `target` in state.mcps, re-render, persist. Works on the FULL
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
  saveOrder();
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
  saveOrder();
  return true;
}

/* --- sidebar groups --------------------------------------------------------------------------- */

/** One group: a disclosure header, then its rows. Built as nodes rather than an HTML string because
 *  a group name is user input and every handler here needs the name in a closure anyway. */
function groupNode(g) {
  var isDefault = g.name === DEFAULT_GROUP;
  var wrap = el("div", "side-group" + (state.collapsed[g.name] ? " collapsed" : ""));
  wrap.dataset.group = g.name;

  var head = el("div", "grp-head");

  // A drag HANDLE, not a draggable header: the + and ⋯ buttons must never live inside a
  // draggable element — a hand that moves a pixel while pressing one turns the click into a
  // cancelled drag and the button silently does nothing. The grip is the only draggable thing
  // on the head, so buttons keep every click. `default` is pinned first (the fallback every
  // group lands in, not a peer to shuffle) and never drags, but keeps a spacer grip so every
  // header's name starts at the same x.
  var grip = el("span", "grp-grip" + (isDefault ? " spacer" : ""));
  grip.innerHTML = icon("grip");
  if (!isDefault) {
    grip.title = "Drag to reorder this group";
    grip.setAttribute("aria-label", "Reorder group " + g.name);
    grip.draggable = true;
    grip.addEventListener("dragstart", function (e) {
      state.draggingGroup = g.name;
      head.classList.add("dragging");
      try { e.dataTransfer.setData("text/plain", g.name); } catch (err) { /* old IE */ }
      e.dataTransfer.effectAllowed = "move";
    });
    grip.addEventListener("dragend", function () {
      state.draggingGroup = null; // lets the deferred rebuild run — same contract as a row drag
      head.classList.remove("dragging");
      document.querySelectorAll(".grp-head.drop-before, .grp-head.drop-after").forEach(function (h) {
        h.classList.remove("drop-before", "drop-after");
      });
      patchSidebar();
    });
  } else {
    grip.setAttribute("aria-hidden", "true");
  }
  head.appendChild(grip);
  var toggle = el("button", "grp-toggle");
  toggle.type = "button";
  var chev = el("span", "grp-chev");
  chev.innerHTML = icon("chevron-right");
  toggle.appendChild(chev);
  toggle.appendChild(el("span", "grp-name", g.name));
  toggle.appendChild(el("span", "grp-n", String(g.rows.length)));
  toggle.onclick = function () {
    if (state.collapsed[g.name]) delete state.collapsed[g.name];
    else state.collapsed[g.name] = true;
    saveCollapsed();
    patchSidebar();
  };
  head.appendChild(toggle);

  // Adding an MCP lives on the group, not in the sidebar header: an MCP is always added INTO a
  // group, and doing it from here means the new one lands where you meant it to instead of appearing
  // in `default` to be dragged over afterwards.
  var add = el("button", "grp-add");
  add.innerHTML = icon("plus");
  add.type = "button";
  add.title = "Add an MCP to " + g.name;
  add.setAttribute("aria-label", "Add an MCP to " + g.name);
  add.onclick = function (ev) { ev.stopPropagation(); openSheet(g.name); };
  head.appendChild(add);

  // `default` cannot be renamed or deleted: it is the fallback every other group's members land in,
  // so removing it would leave MCPs pointing at nothing.
  if (!isDefault) {
    var more = el("button", "grp-more");
    more.innerHTML = icon("ellipsis");
    more.type = "button";
    more.title = "Move, rename or delete this group";
    more.setAttribute("aria-label", "Group actions");
    more.onclick = function (ev) {
      ev.stopPropagation();
      // Move up/down are the keyboard-and-precision path to what the grip does by drag:
      // present exactly when the move exists, absent at the list's edges.
      var i = state.groups.indexOf(g.name);
      var items = [];
      if (i > 0) items.push({ label: "Move up", fn: function () { moveGroupBy(g.name, -1); } });
      if (i >= 0 && i < state.groups.length - 1) {
        items.push({ label: "Move down", fn: function () { moveGroupBy(g.name, 1); } });
      }
      if (items.length) items.push({ sep: true });
      items.push(
        { label: "Rename…", fn: function () { renameGroup(g.name); } },
        { sep: true },
        { label: "Delete group", danger: true, fn: function () { deleteGroup(g.name); } },
      );
      popupMenu(more.getBoundingClientRect(), items);
    };
    head.appendChild(more);
  } else {
    // A reserved slot, not a button. Without it `default`'s + lands where every other group's ⋯ sits,
    // so the one glyph that is always visible jumps 20px sideways from one group header to the next.
    var pad = el("span", "grp-more spacer");
    pad.innerHTML = icon("ellipsis");
    pad.setAttribute("aria-hidden", "true");
    head.appendChild(pad);
  }
  wireGroupDrop(head, g.name);
  wrap.appendChild(head);

  var body = el("div", "grp-rows");
  g.rows.forEach(function (m) {
    var b = el("button", "side-row");
    b.type = "button";
    b.dataset.name = m.name;
    b.setAttribute("role", "option");
    b.appendChild(el("span", "dot"));
    b.draggable = true; // reorder + regroup from the sidebar; see wireDrag
    b.appendChild(el("span", "side-name", m.name));
    b.appendChild(el("span", "side-type")); // http / rest / npx / uvx — filled by the patch pass
    b.onclick = function () { openDetail(m.name); };
    wireDrag(b);
    body.appendChild(b);
  });
  wrap.appendChild(body);
  return wrap;
}

/** Send the whole group list. Create, reorder and delete are all "here is the new list". */
async function saveGroups(next) {
  var j = await apiJson("/api/groups", { method: "PUT", body: JSON.stringify({ groups: next }) });
  if (!j) return false;
  state.groups = j.groups || [];
  await loadList();
  return true;
}

function newGroup() {
  openGroupSheet(null, async function (name) {
    if (!await saveGroups(state.groups.concat([name]))) return false;
    toast("Group " + name + " created");
    return true;
  });
}

function renameGroup(from) {
  openGroupSheet(from, async function (to) {
    var j = await apiJson("/api/groups/" + encodeURIComponent(from) + "/rename",
      { method: "POST", body: JSON.stringify({ name: to }) });
    if (!j) return false;
    state.groups = j.groups || [];
    // The fold state is keyed by name, so carry it across or a renamed group springs open.
    if (state.collapsed[from]) { delete state.collapsed[from]; state.collapsed[to] = true; saveCollapsed(); }
    await loadList();
    toast("Renamed " + from + " → " + to);
    return true;
  });
}

async function deleteGroup(name) {
  var n = state.mcps.filter(function (m) { return groupOf(m) === name; }).length;
  // Deleting a group deletes nothing else, so this only has to say where the MCPs go.
  if (n && !confirm("Delete group '" + name + "'?\n\nIts " + n + " MCP" + (n === 1 ? "" : "s") +
      " move to '" + DEFAULT_GROUP + "'. Nothing is removed.")) return;
  if (await saveGroups(state.groups.filter(function (g) { return g !== name; }))) toast("Deleted group " + name);
}

/** Move one MCP into a group. Applied locally first so the row jumps immediately, then persisted. */
async function assignGroup(name, group) {
  var m = rowOf(name);
  if (!m || groupOf(m) === (group || DEFAULT_GROUP)) return;
  m.group = group || DEFAULT_GROUP;
  patchSidebar();
  var j = await apiJson("/api/mcps/" + encodeURIComponent(name) + "/group",
    { method: "PUT", body: JSON.stringify({ group: group }) });
  if (!j) { await loadList(); return; } // rejected — take the server's word for it
  m.group = j.group;
  patchSidebar();
}

/** A group header is a drop target twice over: dropping a ROW on it is the only way into a group
 *  with no rows yet, and dropping another HEADER on it reorders the groups. The hovered half
 *  picks before/after; on `default` anything lands at the top of the custom list, shown as
 *  after — `default` keeps its pinned first slot. */
function wireGroupDrop(head, group) {
  var half = function (e) { return e.clientY < head.getBoundingClientRect().top + head.offsetHeight / 2; };
  head.addEventListener("dragover", function (e) {
    if (state.draggingGroup) {
      if (state.draggingGroup === group) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      var before = group !== DEFAULT_GROUP && half(e);
      head.classList.toggle("drop-before", before);
      head.classList.toggle("drop-after", !before);
      return;
    }
    if (!state.dragging) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    head.classList.add("drop-into");
  });
  head.addEventListener("dragleave", function () {
    head.classList.remove("drop-into", "drop-before", "drop-after");
  });
  head.addEventListener("drop", function (e) {
    e.preventDefault();
    head.classList.remove("drop-into", "drop-before", "drop-after");
    if (state.draggingGroup) {
      var dg = state.draggingGroup;
      state.draggingGroup = null;
      if (dg && dg !== group) moveGroup(dg, group, half(e));
      return;
    }
    var name = state.dragging;
    state.dragging = null;
    if (name) dropInto(name, group);
  });
}

/** Move a custom group to a new slot in the stored order. The whole list is sent — create,
 *  reorder and delete are all "here is the new list" on this API — and the server keeps
 *  the order it is given, so the sidebar and every other consumer agree. */
function moveGroup(name, target, before) {
  if (!name || name === target) return;
  var rest = state.groups.filter(function (g) { return g !== name; });
  var to = target === DEFAULT_GROUP ? 0 : rest.indexOf(target);
  if (to < 0) rest.push(name); // target vanished mid-drag — land at the end
  else rest.splice(before && target !== DEFAULT_GROUP ? to : to + 1, 0, name);
  state.groups = rest;
  patchSidebar();
  void saveGroups(rest);
}

/** Swap a group with its neighbour (⎳ menu's Move up/down): the click-precise counterpart of
 *  dragging the grip. Falls through at the edges, where the ⎳ menu does not offer the move. */
function moveGroupBy(name, delta) {
  var i = state.groups.indexOf(name);
  var j = i + delta;
  if (i < 0 || j < 0 || j >= state.groups.length) return;
  var next = state.groups.slice();
  var tmp = next[i]; next[i] = next[j]; next[j] = tmp;
  state.groups = next;
  patchSidebar();
  void saveGroups(next);
}

/** Land an MCP at the end of a group: reassign, then slot it after that group's last member so the
 *  flat order agrees with what the sidebar now shows. */
function dropInto(name, group) {
  var members = state.mcps.filter(function (m) { return groupOf(m) === group && m.name !== name; });
  if (members.length) moveRow(name, members[members.length - 1].name, false);
  assignGroup(name, group === DEFAULT_GROUP ? null : group);
}

export { assignGroup, deleteGroup, dropInto, groupNode, groupOf, groupedMcps, moveGroup, moveGroupBy, moveRow, navRows, newGroup, nudgeSelected, renameGroup, rowOf, saveGroups, saveOrder, visibleMcps, wireGroupDrop };

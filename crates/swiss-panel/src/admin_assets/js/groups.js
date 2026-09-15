/* ================================================================================================
   Groups - the one grouped-list component (docs/20 §4).

   Six scopes (mcps, conns, rules, jobs, secrets, tokens), two densities (the sidebar tree and
   the page card list), one anatomy: a tree-node header - chevron, folder glyph, name in mixed
   case, count - on a transparent ground (hover lifts it), with + always visible and the
   ellipsis and the drag grip on hover, and members indented one full tree gutter (24-32px of
   text) behind a 1px guide line that drops from the chevron column. Before this module the sidebar and the Tunnels
   page each carried a private copy of the whole idea (two drag implementations, two header
   anatomies, two delete-confirm wordings) and they had already forked; Jobs, Secrets and
   Tokens were about to grow three more. Everything a grouped list does lives here once:

   - slice()/groupOf() - one flat order cut into group slices; moving a member between groups
     never rewrites the ordering.
   - the /api/groups/{scope} family - saveGroupNames/rename/assign/saveOrder, one request shape.
   - collapse state per scope in localStorage, keys carried across a rename.
   - mountGroup() - the DOM: header (collapse, +, ellipsis menu with Move/Rename/Delete, grip),
     drop targets (rows reorder + re-home in one gesture, headers append into a group, grips
     reorder the groups), and the empty-group line that keeps a fresh group visible.

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
function collapseKey(scope) { return "swiss.groups." + scope + ".collapsed"; }

function loadCollapsed(scope) {
  try { return JSON.parse(localStorage.getItem(collapseKey(scope))) || {}; } catch (e) { return {}; }
}
function saveCollapsed(scope, map) {
  try { localStorage.setItem(collapseKey(scope), JSON.stringify(map)); } catch (e) { /* full or blocked */ }
}

/** The group picked the last time something was created in this scope - read raw (null when
 *  never set); resolveDefaultGroup decides whether it still means anything. */
function lastGroup(scope) {
  try { return localStorage.getItem(lastGroupKey(scope)); } catch (e) { return null; }
}
function rememberGroup(scope, name) {
  try { localStorage.setItem(lastGroupKey(scope), name); } catch (e) { /* full or blocked */ }
}

/* --- the /api/groups/{scope} family (docs/20 §3) -------------------------------------------------- */

/** Create, reorder and delete are all "here is the new list"; the server drops omitted
 *  groups and returns their members to the first remaining one. */
async function saveGroupNames(scope, next) {
  return apiJson("/api/groups/" + scope, { method: "PUT", body: JSON.stringify({ groups: next }) });
}

/** Rename in place: the slot is kept and the members ride along; the answer's moved count is
 *  what the toast reports, so renaming a full group looks like what it was. */
async function renameGroupApi(scope, from, to) {
  return apiJson("/api/groups/" + scope + "/rename", { method: "POST", body: JSON.stringify({ from: from, to: to }) });
}

/** Put one member in a group (the canonical name comes back), or null to render it in the
 *  first group. */
async function assignMember(scope, id, group) {
  return apiJson("/api/groups/" + scope + "/members/" + encodeURIComponent(id),
    { method: "PUT", body: JSON.stringify({ group: group === undefined ? null : group }) });
}

/** The scope's flat order: ids in the order the panel now sees. */
async function saveOrder(scope, ids) {
  return apiJson("/api/groups/" + scope + "/order", { method: "PUT", body: JSON.stringify({ order: ids }) });
}

/** The New-group flow every scope shares: the one-field sheet, then the whole list. */
function newGroupFlow(scope, names, reload) {
  openGroupSheet(null, async function (name) {
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
function groupFieldHtml(names, sel) {
  var opts = names.map(function (n) {
    return '<option value="' + esc(n) + '"' + (n === sel ? " selected" : "") + ">" + esc(n) + "</option>";
  }).join("");
  return '<label class="field"><span>Group</span><select id="g-sel">' + opts + "</select></label>";
}

/* --- the component ------------------------------------------------------------------------------- */

/** Build one group container: the header band (grip, disclosure, name, count, +, ellipsis)
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
 *    rowsHtml(g)    page density: the rows as one HTML string, inside one .group card
 *    wireRow(el, row) page density: extra per-row wiring (actions); drag is wired here
 *    rowId(row)     the id a drag carries (name for MCPs, id for tunnels)
 *    rowSel(row)    page density: a selector that finds rowId's node inside the card
 *    rowsById()     live rows, for drop-into's "slot after the last member" step
 *    groupOfRow(row) the rendering group of a row (a groupOf(names) closure)
 *    onMoveRow(id, targetId, before) flat reorder + order PUT + render (caller-owned list)
 *    onAssign(id, group)             member PUT, applied locally first (caller-owned rows)
 *    filtered       a search is on: groups with no match hide, matches force expansion
 */
function mountGroup(cfg, g) {
  var wrap = el("div", "grp grp--" + cfg.density + (cfg.collapsed[g.name] && !cfg.filtered ? " collapsed" : ""));
  wrap.dataset.group = g.name;

  var head = el("div", "grp-head");

  // The head is a TREE NODE, not a section band (the refresh of docs/20 §4.1): chevron,
  // folder glyph, name, count - in that column order, on a transparent ground that only
  // lifts on hover. The three leading columns are the contract base.css aligns to: the
  // chevron column is where the guide line drops, the folder column ends where the name
  // begins, and members sit one full gutter (24-32px of TEXT, not of padding) to the
  // right of the name. aria-expanded says the fold state to assistive tech.
  var toggle = el("button", "grp-toggle");
  toggle.type = "button";
  var folded = !!(cfg.collapsed[g.name] && !cfg.filtered);
  toggle.setAttribute("aria-expanded", String(!folded));
  var chev = el("span", "grp-chev");
  chev.innerHTML = icon("chevron-right");
  toggle.appendChild(chev);
  // The folder is the "this row is a container" hint a source list gives (Finder, VS Code):
  // a member can never grow one, so the glyph alone separates parents from children even
  // before the indent is seen.
  var folder = el("span", "grp-folder");
  folder.innerHTML = icon("folder");
  toggle.appendChild(folder);
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
  // keyboard-and-precision path to what the grip does by drag: present exactly when the move
  // exists, absent at the list's edges.
  var more = el("button", "grp-more");
  more.innerHTML = icon("ellipsis");
  more.type = "button";
  more.title = "Move, rename or delete this group";
  more.setAttribute("aria-label", "Group actions");
  more.onclick = function (ev) {
    ev.stopPropagation();
    var i = cfg.names.indexOf(g.name);
    var items = [];
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

  // A drag HANDLE, not a draggable header: the + and ellipsis buttons must never live inside
  // a draggable element - a hand that moves a pixel while pressing one turns the click into a
  // cancelled drag and the button silently does nothing. The grip is the only draggable thing
  // on the head, so buttons keep every click. It sits at the head's END, after the actions,
  // so the leading columns (chevron, folder, name) keep one stable x for the tree alignment;
  // it takes layout space but paints only on hover/focus.
  var grip = el("span", "grp-grip");
  grip.innerHTML = icon("grip");
  grip.title = "Drag to reorder this group";
  grip.setAttribute("aria-label", "Reorder group " + g.name);
  grip.draggable = true;
  grip.addEventListener("dragstart", function (e) {
    cfg.dragGroup.set(g.name);
    head.classList.add("dragging");
    try { e.dataTransfer.setData("text/plain", g.name); } catch (err) { /* old IE */ }
    e.dataTransfer.effectAllowed = "move";
  });
  grip.addEventListener("dragend", function () {
    cfg.dragGroup.set(null); // lets the deferred rebuild run - same contract as a row drag
    head.classList.remove("dragging");
    document.querySelectorAll(".grp-head.drop-before, .grp-head.drop-after").forEach(function (h) {
      h.classList.remove("drop-before", "drop-after");
    });
    if (cfg.afterDrag) cfg.afterDrag();
  });
  head.appendChild(grip);
  wireHeadDrop(cfg, head, g.name);
  wrap.appendChild(head);

  var body = el("div", "grp-body");
  if (cfg.density === "page" && g.rows.length) {
    var card = el("div", "group");
    card.innerHTML = cfg.rowsHtml(g);
    body.appendChild(card);
    g.rows.forEach(function (row) {
      // rowSel, not a data-id: tunnels address rows by data-conn/data-rule, jobs and secrets
      // by their own keys — the caller owns the DOM it built.
      var node = card.querySelector(cfg.rowSel(row));
      if (node) {
        wireRowDrag(cfg, node, row);
        if (cfg.wireRow) cfg.wireRow(node, row); // the caller's own actions on the row
      }
    });
  } else if (cfg.rowNode) {
    g.rows.forEach(function (row) {
      var node = cfg.rowNode(row);
      wireRowDrag(cfg, node, row);
      body.appendChild(node);
    });
  }
  // An empty group is not an empty state - it is a place. One SHORT quiet line keeps the
  // container visible as a drop target (the only way in); the head's + explains itself on
  // hover, so the line does not have to repeat the instructions. A scope whose rows cannot
  // drag (tokens) says the honest half only.
  if (!g.rows.length && !cfg.filtered) {
    body.appendChild(el("div", "grp-empty", emptyLineText(cfg.draggable !== false)));
  }
  wrap.appendChild(body);
  return wrap;
}

/** Row drag: reorder and re-home in one gesture. The insertion point is the hovered row's
 *  half; the group comes from the target row, so a cross-group drag needs no second drop. */
function wireRowDrag(cfg, node, row) {
  if (cfg.draggable === false) return;
  var id = cfg.rowId(row);
  node.draggable = true;
  node.addEventListener("dragstart", function (e) {
    cfg.drag.set(id);
    node.classList.add("dragging");
    try { e.dataTransfer.setData("text/plain", id); } catch (err) { /* old IE */ }
    e.dataTransfer.effectAllowed = "move";
  });
  node.addEventListener("dragover", function (e) {
    if (!cfg.drag.get() || cfg.drag.get() === id) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
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

/** A group header is a drop target twice over: dropping a ROW on it is the only way into a
 *  group with no rows yet, and dropping another header's GRIP on it reorders the groups. The
 *  hovered half picks before/after - for every group alike; no header is pinned. */
function wireHeadDrop(cfg, head, group) {
  var half = function (e) { return e.clientY < head.getBoundingClientRect().top + head.offsetHeight / 2; };
  head.addEventListener("dragover", function (e) {
    if (cfg.dragGroup.get()) {
      if (cfg.dragGroup.get() === group) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      head.classList.toggle("drop-before", half(e));
      head.classList.toggle("drop-after", !half(e));
      return;
    }
    if (!cfg.drag.get()) return;
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
    var dg = cfg.dragGroup.get();
    if (dg) {
      cfg.dragGroup.set(null);
      if (dg !== group) moveGroup(cfg, dg, group, half(e));
      return;
    }
    var id = cfg.drag.get();
    cfg.drag.set(null);
    if (id) dropInto(cfg, id, group);
  });
}

/** Land a dragged row at the end of a group: slot it after that group's last member so the
 *  flat order agrees with what the list now shows, then reassign. */
function dropInto(cfg, id, group) {
  var members = cfg.rowsById().filter(function (r) {
    return cfg.groupOfRow(r) === group && cfg.rowId(r) !== id;
  });
  if (members.length) cfg.onMoveRow(id, cfg.rowId(members[members.length - 1]), false);
  cfg.onAssign(id, group); // the header's name is always a live group
}

/** Move a group to a new slot in the stored order. The whole list is sent - create, reorder
 *  and delete are all "here is the new list" on this API - and the server keeps the order it
 *  is given, so the panel and every other consumer agree. */
function moveGroup(cfg, name, target, before) {
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
 *  counterpart of dragging the grip. Falls through at the edges, where the menu does not
 *  offer the move. */
function moveGroupBy(cfg, name, delta) {
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
function renameFlow(cfg, from) {
  openGroupSheet(from, async function (to) {
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
function deleteFlow(cfg, name) {
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
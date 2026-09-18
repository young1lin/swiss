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
   Remote Targets - the remote plugin page (#remote), docs/34 R6 + R8.

   A hairline card of house rows: dot (endpoint state) + alias + mono root sub-line +
   monospace chips for endpoint and capabilities + one overflow menu. The CLI
   (swiss remote ...) and this page write the SAME rows through the same routes; the
   page exists so a target never needs a terminal to exist.

   Groups (docs/34 R8): the list renders through the groups component as the seventh
   scope of the docs/20 family - page density (one card per group, docs/35), drag ON
   both ways. A row drag reorders the flat list (the scope's order route); a drop into
   another group moves the row (the member route); a group drags by its whole head. The
   group names ride with the rows in ONE /api/remote/targets response, and an older
   gateway without them answers the single default group the component draws as no
   divider at all.
   ================================================================================================ */
import { $, apiJson, emptyHtml, esc, icon, targetEl, toast } from "../util.js";
import { closeSheet } from "../add-sheet.js";
import { popupMenu } from "../menu.js";
import { assignMember, groupFieldHtml, groupOf, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, saveOrder, slice } from "../groups.js";

let targets = []                     ;
let endpoints = []                       ;
let presence = "";
let groupNames = ["default"];
let editing = null                 ; // the id an open sheet is editing, null when adding
let pendingGroup = null                 ; // the group whose header + opened the sheet; the select still wins
let dragging = null                 ; // in-flight row drag: a poll must not rebuild under it
let draggingGroup = null                 ; // in-flight group drag, same rule
let painted = ""; // structural signature of the drawn list; a poll that changes nothing repaints nothing
let collapsed = {}                           ; // the groups fold map, loaded once before the first paint

async function load() {
  const e = await apiJson                            ("/api/remote/endpoints");
  const t = await apiJson                          ("/api/remote/targets");
  if (!e || !t) return false; // apiJson toasted; keep the last paint
  presence = e.presence || "none";
  endpoints = e.endpoints || [];
  targets = t.targets || [];
  // The names arrive with the rows (docs/34 R8); an older gateway answers neither,
  // and the single default group renders as no divider at all.
  groupNames = t.groups && t.groups.length ? t.groups : ["default"];
  return true;
}

/** Full reload after a group-list mutation: refetch, then rebuild the whole page. */
async function reload() {
  if (!(await load())) return;
  render();
}

function endpointLabel(id        )         {
  const hit = endpoints.find((e) => { return e.id === id; });
  return hit ? hit.label || hit.id : id;
}

// The dot mirrors the endpoint's state: filled = connected, hollow = idle (will start
// on demand), amber = mid-transition. The title always says the state in words.
function endpointDot(id        )         {
  const st = (endpoints.find((e) => { return e.id === id; }) || {}                     ).state || "unknown";
  const cls = st === "connected" || st === "up" ? "up" : st === "idle" ? "idle" : "starting";
  return '<span class="dot ' + cls + '" title="endpoint ' + esc(st) + '"></span>';
}

function signature()         {
  return presence + "\u0000" + endpoints.map((e) => { return e.id + "=" + e.state; }).join(",") + "\u0000" +
    groupNames.join(",") + "\u0000" +
    targets.map((t) => {
      return [t.id, t.endpoint, t.workspaceRoot, (t.capabilities || []).join("+"), t.group || ""].join("|");
    }).join("\n");
}

function chip(text        )         {
  return '<span class="side-type">' + esc(text) + "</span>";
}

function row(t                 )         {
  const caps = (t.capabilities || []).map(chip).join("");
  // A label the sheet defaulted to the alias (see save) is not worth saying twice.
  const label = t.label && t.label !== t.id ? ' <span class="text-3">' + esc(t.label) + "</span>" : "";
  return (
    '<div class="row row-act" data-rmrow="' + esc(t.id) + '">' + endpointDot(t.endpoint) +
      '<div class="row-main">' +
        '<div class="name"><a href="#remote" class="rowname" data-rmedit="' + esc(t.id) + '">' + esc(t.id) + "</a>" + label + "</div>" +
        '<div class="rm-sub">' + esc(t.workspaceRoot || "") + "</div>" +
      "</div>" +
      '<div class="row-chips">' + chip(endpointLabel(t.endpoint)) + caps + "</div>" +
      '<button class="btn ghost icon" data-rmmore="' + esc(t.id) + '" aria-label="Actions for ' + esc(t.id) + '" title="Actions for ' + esc(t.id) + '">' + icon("ellipsis") + "</button>" +
    "</div>"
  );
}

/** The groups component's cfg (docs/20): this page's nouns, rows and moves. */
function cfg()                            {
  return {
    scope: "targets",
    density: "page",
    names: groupNames,
    collapsed: collapsed,
    noun: "target",
    addTitle: (g) => { return "Add a remote target to " + g; },
    onAdd: (g) => {
      pendingGroup = g; // a real name now - the sheet's Group select starts on it
      openSheet(null);
    },
    reload: () => { return reload(); },
    render: paint,
    afterDrag: () => { paint(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: () => { return dragging; },
      set: (v) => { dragging = v; },
    },
    dragGroup: {
      get: () => { return draggingGroup; },
      set: (v) => { draggingGroup = v; },
    },
    rowsHtml: (g) => { return g.rows.map(row).join(""); },
    rowId: (r) => { return r.id; },
    // Ids are validated slugs (a-z 0-9 -), so a bare attribute selector is safe.
    rowSel: (r) => { return '[data-rmrow="' + r.id + '"]'; },
    rowsById: () => { return targets; },
    groupOfRow: groupOf(groupNames),
    onMoveRow: moveRow,
    onAssign: (id, g) => { void assign(id, g); },
  };
}

/** Paint the groups region only - the head, the quiet line and the chip stay put. */
function paint()       {
  const region = $("rmGroups");
  if (!region) return;
  region.innerHTML = "";
  slice(targets, groupNames, groupOf(groupNames)).forEach((g) => {
    region.appendChild(mountGroup(cfg(), g));
  });
}

function render()       {
  painted = signature();
  // The body header is the family's (tunnels/secrets): task prose + status line on the
  // left, actions right-aligned in pane-actions - no location title (the context bar
  // already says Remote Targets) and no invented classes. One sentence of what, one of
  // who else writes it; the drag is taught by the list itself, not by prose.
  // The status line names the presence only when it is NOT the normal one: "serving ·
  // 3 endpoints served by tunnels" said serving twice.
  let status = endpoints.length + " endpoint" + (endpoints.length === 1 ? "" : "s") + " served by tunnels";
  if (presence !== "serving") status = "Tunnels " + esc(presence) + " · " + status;
  const head =
    '<div class="wide">' +
      '<div class="pane-head"><div>' +
        '<div class="pane-desc">Machines the gateway can run commands on, reached over Tunnels SSH connections. The CLI (swiss remote …) writes the same table.</div>' +
        '<div class="pane-sub">' + status + "</div>" +
      "</div>" +
      '<div class="pane-actions">' +
        '<button class="btn primary" id="rmAdd">Add target</button>' +
        '<button class="btn" id="rmNewGroup">New group</button>' +
      "</div></div>" +
      '<div id="rmGroups"></div>' +
    "</div>";
  $("pane").innerHTML = head;
  // The grouped list shows once there is anything to show: any row, or any group
  // beyond the implicit default. A group the user just made is a PLACE (docs/20: an
  // empty group is not an empty state) - hiding it behind "No targets yet" reads as
  // the create having failed, which is exactly the bug it was. The bare table (no
  // rows, only default) keeps the explaining empty state.
  if (!targets.length && groupNames.length <= 1) {
    $("rmGroups").innerHTML = emptyHtml({ icon: "globe", title: "No targets yet", hint: "Add a target to run commands on the machines the Tunnels connections reach." });
  } else {
    paint();
  }
  $("countChip").textContent = targets.length ? targets.length + " target" + (targets.length === 1 ? "" : "s") : "";
}

/** Flat reorder after a row drag: applied locally so the row jumps immediately, then
 *  the scope's order route - a reject takes the server's word for it. */
function moveRow(id        , targetId        , before         )       {
  if (!id || !targetId || id === targetId) return;
  const item = targets.filter((r) => { return r.id === id; })[0];
  if (!item) return;
  const to = targets.findIndex((r) => { return r.id === targetId; });
  if (to < 0) return; // target vanished mid-drag - leave everything where it is
  targets.splice(targets.indexOf(item), 1);
  targets.splice(before ? to : to + 1, 0, item);
  painted = ""; // the optimistic move changed the structure the signature would compare
  paint();
  saveOrder("targets", targets.map((r) => { return r.id; }));
}

/** Put one row in a group after a drop-into: applied locally first so the row jumps
 *  immediately, then the member PUT - the server's canonical spelling wins. */
async function assign(id        , group               )                {
  const row = targets.filter((r) => { return r.id === id; })[0];
  if (!row) return;
  if (groupOf(groupNames)(row) === (group || groupNames[0])) return;
  row.group = group;
  painted = "";
  paint();
  const j = await assignMember("targets", id, group);
  if (!j) { await reload(); return; }
  row.group = j.group; // the canonical name the server stored
  painted = "";
  paint();
}

/* The Add/Edit sheet. Same shape as every sheet: #sheet unhidden BEFORE innerHTML,
   closeSheet from add-sheet.js, backdrop click closes. */
function openSheet(target                        )       {
  editing = target ? target.id : null;
  const endpointOptions = endpoints
    .map((e) => {
      const sel = target && target.endpoint === e.id ? " selected" : "";
      return '<option value="' + esc(e.id) + '"' + sel + ">" + esc(e.label || e.id) + "</option>";
    })
    .join("");
  // The Group select is where the row lands: the group whose + opened the sheet
  // preselects, the last used one otherwise, and a value the user changed wins.
  const groupSel = target
    ? groupOf(groupNames)(target)
    : pendingGroup || lastGroup("targets") || groupNames[0];
  pendingGroup = null;
  const caps = ["exec", "sync", "files"];
  const capBoxes = caps
    .map((c) => {
      return '<label class="check"><input type="checkbox" id="rmcap-' + c + '"> ' + c + "</label>";
    })
    .join(" ");
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit" : "Add") + ' a remote target">' +
      '<div class="sheet-head"><h2>' + (editing ? "Edit " + esc(editing) : "Add a remote target") + "</h2></div>" +
      '<div class="sheet-body">' +
        '<label class="field"><span>Alias (the name commands call: swiss remote exec &lt;alias&gt;)</span>' +
          '<input id="rm-id" placeholder="build"></label>' +
        '<label class="field"><span>Label (optional)</span><input id="rm-label"></label>' +
        '<label class="field"><span>Endpoint (a Tunnels connection)</span><select id="rm-endpoint">' + endpointOptions + "</select></label>" +
        groupFieldHtml(groupNames, groupSel) +
        '<label class="field"><span>Workspace root (absolute POSIX path)</span><input id="rm-root" placeholder="/data/ws/proj"></label>' +
        '<div class="field"><span>Capabilities</span><div>' + capBoxes + "</div></div>" +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="rm-cancel">Cancel</button>' +
      '<button class="btn primary" id="rm-save">' + (editing ? "Save" : "Add") + "</button></div>" +
    "</div>";
  $("sheet").hidden = false;
  // Prefill programmatically, not via value=" markup": the pane redraws by signature,
  // and programmatic values are the one source of truth an edit and a test can both read.
  $                  ("rm-id").disabled = !!editing; // the alias never edits; state set here, not in markup
  caps.forEach((c) => {
    $                  ("rmcap-" + c).checked = target ? (target.capabilities || []).indexOf(c) >= 0 : c === "exec";
  });
  if (target) {
    $                  ("rm-label").value = target.label === target.id ? "" : target.label || "";
    $                  ("rm-root").value = target.workspaceRoot || "";
  }
  $("rm-cancel").onclick = closeSheet;
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
  $("rm-save").onclick = save;
  if (!editing) $("rm-id").focus();
}

async function save()                {
  const caps = ["exec", "sync", "files"].filter((c) => { return $                  ("rmcap-" + c).checked; });
  if (!caps.length) { toast("Pick at least one capability", true); return; }
  const id = editing || $                  ("rm-id").value.trim();
  const body                   = {
    // The store refuses an empty label (swiss-remote target.rs), and the field says
    // optional: an alias is a fine label, so a blank one becomes the alias rather than
    // a rejected save the user cannot see the reason for.
    label: $                  ("rm-label").value.trim() || id,
    endpoint: $                   ("rm-endpoint").value,
    // The group the select shows - the row is born INTO it server-side (docs/34 R8),
    // one write, no second assign round-trip.
    group: $                   ("g-sel") ? $                   ("g-sel").value : null,
    workspaceRoot: $                  ("rm-root").value.trim(),
    capabilities: caps,
    // The one shell the surface speaks today (docs/34): the route requires it, and a
    // select with a single honest option would be decoration.
    shell: "posix",
  };
  if (!body.workspaceRoot || body.workspaceRoot.charAt(0) !== "/") {
    toast("Workspace root must be an absolute POSIX path", true);
    return;
  }
  let url = "/api/remote/targets";
  if (editing) url += "/" + encodeURIComponent(editing);
  else body.id = id;
  const j = await apiJson(url, { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  if (body.group) rememberGroup("targets", body.group);
  closeSheet();
  await reload();
}

async function removeTarget(id        )                {
  if (!confirm("Delete target " + id + "? The Tunnels connection and any files on the machine are not touched.")) return;
  const d = await apiJson("/api/remote/targets/" + encodeURIComponent(id), { method: "DELETE" });
  if (!d) return;
  await reload();
}

export async function mount() {
  collapsed = loadCollapsed("targets");
  if (!(await load())) return;
  render();
  $("pane").onclick = (event            )       => {
    const add = targetEl(event)?.closest("#rmAdd");
    if (add) { pendingGroup = null; openSheet(null); return; }
    const newGroup = targetEl(event)?.closest("#rmNewGroup");
    if (newGroup) { void newGroupFlow("targets", groupNames, reload); return; }
    const edit = targetEl(event)?.closest             ("[data-rmedit]");
    if (edit) {
      const hit = targets.find((t) => { return t.id === edit?.dataset.rmedit; });
      if (hit) openSheet(hit);
      return;
    }
    const more = targetEl(event)?.closest             ("[data-rmmore]");
    if (more) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      const t = targets.find((x) => { return x.id === more?.dataset.rmmore; });
      if (!t) return;
      popupMenu(more.getBoundingClientRect(), [
        { label: "Edit", fn: () => { openSheet(t ); } },
        { sep: true },
        { label: "Delete", danger: true, fn: () => { removeTarget(t?.id); } },
      ]);
    }
  };
}

export async function refresh() {
  if (!(await load())) return;
  // Never rebuild under an in-flight gesture: the groups component finishes the drag
  // on nodes it captured, and afterDrag() runs the catch-up paint (docs/20).
  if (dragging || draggingGroup) return;
  // A poll that changed nothing repaints nothing: the pane keeps its nodes (and a
  // sheet the user may be typing into stays put, the same discipline as Tokens).
  if (signature() === painted) return;
  render();
}
export async function poll() { await refresh(); }
export function countText() { return targets.length ? targets.length + " targets" : ""; }
export function unmount() {}

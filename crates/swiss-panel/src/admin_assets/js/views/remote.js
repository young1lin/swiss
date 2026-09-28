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

/* ================================================================================================
   Remote Targets - the remote plugin page (#remote), docs/34 R6 + R8.

   Library rows (docs/46 P6-2): the alias, a sub-line saying where it runs (the endpoint, the
   workspace root in mono, what it may do), when it last ran, and one overflow menu. The CLI
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
                                                                                                                   
                                                
                                                                                                              
import { $, api, apiJson, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
                                      
import { assignMember, groupFieldNode, groupOf, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, saveOrder, slice } from "../groups.js";
import { locale, tr, trn } from "../i18n.js";
                                              
import { btn, checkField, closeSheet, dot, field, iconBtn, moreBtn, pair, paneBody, paneHead, popupMenu, relTime, row, sheet, showSheet, tag } from "../ui/index.js";
                                             
import { commandOf, runOutcome, runTarget } from "./remote-runs.js";

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
/* The last-run column (docs/46 §3.6), read from the run record's own route - no API of its own. */
let lastRuns = null                                                      ; // target -> its newest run; null: the record did not answer, no column
let olderLast = {}                                          ; // targets the newest page missed, asked once per visit

const CAPS = ["exec", "sync", "files"];
const RUNS_PAGE = 100; // the runs route's own ceiling (history.rs clamps the limit there)

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
  await loadLastRuns();
  return true;
}

/** A read the page can do without: null on any failure, and no toast - an optional column that
 *  toasted on every six-second poll would be louder than the list it decorates. */
async function quietJson   (path        )                    {
  try {
    const r = await api(path);
    return r.ok ? await r.json()      : null;
  } catch {
    return null;
  }
}

/** Each target's newest run. ONE read of the record's newest page, folded by target, the
 *  coordinator's in-flight runs first (they are newer than anything recorded). A target that page
 *  missed while older records exist is asked on its own, once per visit: the record answers that
 *  by walking back from its end, and for a target that never ran it walks all of it - fine once,
 *  not every poll. A new run on it lands in the newest page anyway. */
async function loadLastRuns()                {
  const page = await quietJson                       ("/api/remote/runs?limit=" + RUNS_PAGE);
  if (!page) { lastRuns = null; return; }
  const by                                              = {};
  const put = (r                 )       => {
    const t = runTarget(r);
    if (t && !by[t]) by[t] = r;
  };
  (page.active                     ).forEach(put);
  (page.runs                     ).forEach(put);
  if (page.nextBefore) {
    for (const t of targets) {
      if (by[t.id]) continue;
      if (!(t.id in olderLast)) {
        const one = await quietJson                       ("/api/remote/runs?limit=1&target=" + encodeURIComponent(t.id));
        if (one) olderLast[t.id] = one.runs.length ? one.runs[0]                    : null;
      }
      const old = olderLast[t.id];
      if (old) by[t.id] = old;
    }
  }
  lastRuns = by;
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

/** The endpoint's state, only when it is not a resting one (docs/46 §3.6, the P5 rule). Connected
 *  and idle both run a command (idle dials on demand), so they draw nothing; a dial in flight is
 *  the amber pulse, and a failed or missing endpoint - a command would not get through - is red.
 *  An `error` used to share the amber of a transition. The title says the state in words. */
function endpointDotNode(id        )                     {
  const st = (endpoints.find((e) => { return e.id === id; }) || {}                     ).state || "unknown";
  if (st === "connected" || st === "up" || st === "idle") return null;
  return dot(st === "connecting" ? "starting" : "down", tr("remote.endpointState", { state: st }));
}

function signature()         {
  return presence + "\u0000" + endpoints.map((e) => { return e.id + "=" + e.state; }).join(",") + "\u0000" +
    groupNames.join(",") + "\u0000" +
    targets.map((t) => {
      return [t.id, t.endpoint, t.workspaceRoot, (t.capabilities || []).join("+"), t.group || ""].join("|");
    }).join("\n");
}

/** When the target last ran and how it went, from the run record: a run in flight is the pulse and
 *  its word, a failure the red tag the Runs page gives it, then how long ago. The full moment and
 *  the command are the title. A target the record holds nothing for says so. */
function lastCol(t                 )         {
  const r = lastRuns ? lastRuns[t.id] : null;
  if (!r) return { v: tr("remote.noRuns"), w: "l" };
  const what = commandOf(r);
  if (r.state === "running" || r.state === "queued") {
    const queued = r.state === "queued";
    return {
      v: [dot(queued ? "idle" : "starting", null), " ", tr(queued ? "remote.lastQueued" : "remote.lastRunning")],
      w: "l", title: what,
    };
  }
  const at = Date.parse(r.endedAt || r.startedAt || r.queuedAt);
  const bad = runOutcome(r);
  return {
    v: bad ? [tag(bad, { tone: "bad" }), " " + relTime(at)] : relTime(at),
    w: "l",
    title: tr("remote.lastRunAt", { when: new Date(at).toLocaleString(locale()), what }),
  };
}

function rowNode(t                 )              {
  // A label the sheet defaulted to the alias (see save) is not worth saying twice.
  const label = t.label && t.label !== t.id ? t.label : null;
  const caps = (t.capabilities || []).join(", ");
  const sub           = [label ? label + " · " : null, endpointLabel(t.endpoint), " · ", h("code", null, t.workspaceRoot || ""),
    caps ? " · " + caps : null];
  return row({
    name: [t.id, endpointDotNode(t.endpoint)],
    sub,
    cols: lastRuns ? [lastCol(t)] : [],
    more: moreBtn(tr("remote.actionsName", { name: t.id }), { data: { rmmore: t.id } }),
    data: { rmrow: t.id },
    draggable: true,
  });
}


/* --- running a command from the page (2026-09-28) --------------------------------------------- */
/* The owner: "the Remote page should let me run write and exec straight from the web, and Runs
   should show what is running so I can cancel it." Runs already streams an open run and offers
   Cancel, and POST /api/runs is the same door the CLI knocks on - so this is only the door, and
   the run it starts is watched where every other run is.

   The typed line goes to the target's own shell (`sh -c <line>`) rather than through a shell
   parser written here: pipes, quotes and redirects then mean what the operator meant, and the
   Runs page shows exactly what was sent. A remote note for the target says the same. */

const RUN_TIMEOUT_MS = 15 * 60 * 1000;

/** Submit one recorded run and go watch it. The page never holds the answer: the Runs page is
 *  where an open run streams, cancels and ends up in the record. */
async function submitRun(action        , input                         )                {
  const j = await apiJson                   ("/api/runs", {
    method: "POST",
    body: JSON.stringify({ action, input, timeoutMs: RUN_TIMEOUT_MS, actor: "panel" }),
  });
  if (!j) return; // apiJson already said why
  closeSheet();
  location.hash = "#remote-runs";
}

/** Run a command on the target: the line, and the directory to run it in (its workspace root
 *  when left empty - the gateway's own default). */
function openRunSheet(t                 )       {
  showSheet(sheet({
    title: tr("remote.runOn", { name: t.id }),
    body: [
      field({
        label: tr("remote.command"),
        control: h("input", { id: "rmx-cmd", placeholder: tr("remote.commandPlaceholder"), autocomplete: "off" }),
        hint: tr("remote.commandHint"),
      }),
      field({
        label: tr("remote.directoryOptional"),
        control: h("input", { id: "rmx-cwd", placeholder: t.workspaceRoot || "", autocomplete: "off" }),
      }),
    ],
    foot: [
      btn(tr("remote.cancel"), { id: "rmx-cancel" }),
      btn(tr("remote.run"), { id: "rmx-run", kind: "primary" }),
    ],
  }));
  $("rmx-cancel").onclick = closeSheet;
  $("rmx-run").onclick = ()       => {
    const line = $                  ("rmx-cmd").value.trim();
    if (!line) { toast(tr("remote.commandRequired"), true); return; }
    const cwd = $                  ("rmx-cwd").value.trim();
    const input                          = { target: t.id, argv: ["sh", "-c", line] };
    if (cwd) input.cwd = cwd;
    void submitRun("remote.exec", input);
  };
  $("rmx-cmd").focus();
}

/** Write a file on the target: the path (relative to the workspace root, as the CLI takes it)
 *  and the body. The body is sealed beside the record, so the Runs row can show it back. */
function openWriteSheet(t                 )       {
  showSheet(sheet({
    title: tr("remote.writeOn", { name: t.id }),
    body: [
      field({
        label: tr("remote.path"),
        control: h("input", { id: "rmw-path", placeholder: tr("remote.pathPlaceholder"), autocomplete: "off" }),
        hint: t.workspaceRoot ? tr("remote.pathHint", { root: t.workspaceRoot }) : undefined,
      }),
      field({ label: tr("remote.contents"), control: h("textarea", { id: "rmw-body", spellcheck: false, style: "min-height:180px" }) }),
    ],
    foot: [
      btn(tr("remote.cancel"), { id: "rmw-cancel" }),
      btn(tr("remote.write"), { id: "rmw-save", kind: "primary" }),
    ],
  }));
  $("rmw-cancel").onclick = closeSheet;
  $("rmw-save").onclick = ()       => {
    const remote = $                  ("rmw-path").value.trim();
    if (!remote) { toast(tr("remote.pathRequired"), true); return; }
    void submitRun("remote.write", {
      target: t.id,
      remote,
      content: $                     ("rmw-body").value,
    });
  };
  $("rmw-path").focus();
}

/** The groups component's cfg (docs/20): this page's nouns, rows and moves. */
function cfg()                            {
  return {
    scope: "targets",
    density: "page",
    names: groupNames,
    collapsed: collapsed,
    noun: tr("remote.target"),
    addTitle: (g) => { return tr("remote.addRemoteTargetGroup", { group: g }); },
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
    rowNode: rowNode,
    rowId: (r) => { return r.id; },
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
  region.textContent = ""; // the fill() wipe without the children: groups append one by one
  slice(targets, groupNames, groupOf(groupNames)).forEach((g) => {
    region.appendChild(mountGroup(cfg(), g));
  });
}

function render()       {
  painted = signature();
  // The family's head (docs/46 §3.6): one sentence of what, the status line, and the actions -
  // the folder-plus glyph and the one primary. No location title (the context bar says Remote
  // Targets); the drag is taught by the list itself, not by prose.
  // The status line names the presence only when it is NOT the normal one: "serving ·
  // 3 endpoints served by tunnels" said serving twice.
  const status = trn(endpoints.length, "remote.nEndpointsServedTunnels.one", "remote.nEndpointsServedTunnels.other");
  // Hoisted so the comparison literal stays out of the h() children (i18n gate).
  const notServing = presence !== "serving";
  fill($("pane"),
    paneBody({ wide: true },
      paneHead({
        desc: tr("remote.descOneLine"),
        sub: [notServing ? tr("remote.tunnelsState", { state: presence }) : null, status],
        actions: [
          iconBtn("folder-plus", tr("remote.newGroup"), { id: "rmNewGroup" }),
          btn(tr("remote.addTarget"), { id: "rmAdd", kind: "primary" }),
        ],
      }),
      h("div", { id: "rmGroups" })));
  // The grouped list ALWAYS paints, rows or none. A group is a place (docs/20: an empty
  // group is not an empty state - it is a drop target with a +), and the default group is
  // one too: it exists on every gateway and its + is the first way a target gets added.
  // This page used to swap in the "No targets yet" empty state whenever the table was bare
  // AND only the default group remained, so deleting the last user-made group made the
  // default group vanish with it - and a fresh page showed no group at all (2026-09-20).
  // The pane-desc above already says what the page is for; the empty group's own line
  // says what to do next.
  paint();
  $("countChip").textContent = targets.length ? trn(targets.length, "remote.nTargets.one", "remote.nTargets.other") : "";
}

/** Flat reorder after a row drag: applied locally so the row jumps immediately, then
 *  the scope's order route - a reject takes the server's word for it. */
function moveRow(id        , targetId        , before         )       {
  if (!id || !targetId || id === targetId) return;
  const item = targets.find((r) => { return r.id === id; });
  if (!item) return;
  const to = targets.findIndex((r) => { return r.id === targetId; });
  if (to < 0) return; // target vanished mid-drag - leave everything where it is
  targets.splice(targets.indexOf(item), 1);
  targets.splice(before ? to : to + 1, 0, item);
  painted = ""; // the optimistic move changed the structure the signature would compare
  paint();
  void saveOrder("targets", targets.map((r) => { return r.id; }));
}

/** Put one row in a group after a drop-into: applied locally first so the row jumps
 *  immediately, then the member PUT - the server's canonical spelling wins. */
async function assign(id        , group               )                {
  const row = targets.find((r) => { return r.id === id; });
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

/** The Add/Edit sheet on the library (sheet() + ui/form.ts): showSheet makes it visible before
 *  anything reads it and closes it on a backdrop click. The endpoint and the group sit side by
 *  side - where the target is reached and where its row lands. */
function openSheet(target                        )       {
  editing = target ? target.id : null;
  const endpointOptions = endpoints.map((e) => {
    return h("option", { value: e.id, selected: !!(target && target.endpoint === e.id) }, e.label || e.id);
  });
  // The Group select is where the row lands: the group whose + opened the sheet
  // preselects, the last used one otherwise, and a value the user changed wins.
  const groupSel = target
    ? groupOf(groupNames)(target)
    : pendingGroup || lastGroup("targets") || groupNames[0];
  pendingGroup = null;
  showSheet(sheet({
    title: editing ? tr("remote.editName", { name: editing }) : tr("remote.addRemoteTarget"),
    label: editing ? tr("remote.editRemoteTarget") : tr("remote.addRemoteTarget"),
    body: [
      field({ label: tr("remote.alias"), control: h("input", { id: "rm-id", placeholder: tr("remote.build") }), hint: tr("remote.aliasHint") }),
      field({ label: tr("remote.labelOptional"), control: h("input", { id: "rm-label" }) }),
      pair(
        field({ label: tr("remote.endpointTunnelsConnection"), control: h("select", { id: "rm-endpoint" }, endpointOptions) }),
        groupFieldNode(groupNames, groupSel)),
      field({ label: tr("remote.workspaceRootAbsolutePosix"), control: h("input", { id: "rm-root", placeholder: tr("remote.dataWsProj") }) }),
      // Several checks under one caption: a group, so the caption presses none of them.
      field({
        group: true, label: tr("remote.capabilities"),
        control: h("div", null, CAPS.map((c) => {
          return checkField({ label: c, control: h("input", { type: "checkbox", id: "rmcap-" + c })                     });
        })),
      }),
    ],
    foot: [
      btn(tr("remote.cancel"), { id: "rm-cancel" }),
      btn(editing ? tr("remote.save") : tr("remote.add"), { id: "rm-save", kind: "primary" }),
    ],
  }));
  // Prefill programmatically, not via value=" markup": the pane redraws by signature,
  // and programmatic values are the one source of truth an edit and a test can both read.
  $                  ("rm-id").disabled = !!editing; // the alias never edits; state set here, not in markup
  CAPS.forEach((c) => {
    $                  ("rmcap-" + c).checked = target ? (target.capabilities || []).includes(c) : c === "exec";
  });
  if (target) {
    $                  ("rm-label").value = target.label === target.id ? "" : target.label || "";
    $                  ("rm-root").value = target.workspaceRoot || "";
  }
  $("rm-cancel").onclick = closeSheet;
  $("rm-save").onclick = save;
  if (!editing) $("rm-id").focus();
}

async function save()                {
  const caps = CAPS.filter((c) => { return $                  ("rmcap-" + c).checked; });
  if (!caps.length) { toast(tr("remote.pickLeastOneCapability"), true); return; }
  const id = editing || $                  ("rm-id").value.trim();
  const body                   = {
    // The id rides on an edit too: the row route compares it with the path's to refuse a
    // rename (api.rs write_target), and without it every Edit answered "target.id is required".
    id,
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
    toast(tr("remote.workspaceRootMustAbsolute"), true);
    return;
  }
  const url = "/api/remote/targets" + (editing ? "/" + encodeURIComponent(editing) : "");
  const j = await apiJson(url, { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  if (body.group) rememberGroup("targets", body.group);
  closeSheet();
  await reload();
}

async function removeTarget(id        )                {
  if (!confirm(tr("remote.deleteTargetIdTunnels", { id }))) return;
  const d = await apiJson("/api/remote/targets/" + encodeURIComponent(id), { method: "DELETE" });
  if (!d) return;
  await reload();
}

export async function mount() {
  collapsed = loadCollapsed("targets");
  olderLast = {}; // a new visit asks the record again
  if (!(await load())) return;
  render();
  $("pane").onclick = (event            )       => {
    const add = targetEl(event)?.closest("#rmAdd");
    if (add) { pendingGroup = null; openSheet(null); return; }
    const newGroup = targetEl(event)?.closest("#rmNewGroup");
    if (newGroup) { void newGroupFlow("targets", groupNames, reload); return; }
    const more = targetEl(event)?.closest             ("[data-rmmore]");
    if (more) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      const t = targets.find((x) => { return x.id === more?.dataset.rmmore; });
      if (!t) return;
      const caps = t.capabilities || [];
      const items             = [];
      // Only what the target is allowed to do: a run door on a target with no exec would be a
      // button whose only answer is the gateway's refusal.
      if (caps.includes("exec")) items.push({ label: tr("remote.runCommand"), fn: () => { openRunSheet(t); } });
      if (caps.includes("files")) items.push({ label: tr("remote.writeFile"), fn: () => { openWriteSheet(t); } });
      if (items.length) items.push({ sep: true });
      items.push({ label: tr("remote.edit"), fn: () => { openSheet(t ); } });
      items.push({ sep: true });
      items.push({ label: tr("remote.delete"), danger: true, fn: () => { void removeTarget(t?.id); } });
      popupMenu(more.getBoundingClientRect(), items);
    }
  };
}

export async function refresh() {
  if (!(await load())) return;
  // Never rebuild under an in-flight gesture: the groups component finishes the drag
  // on nodes it captured, and afterDrag() runs the catch-up paint (docs/20).
  if (dragging || draggingGroup) return;
  // A poll that changed nothing structural repaints nothing: the pane keeps its nodes (and a
  // sheet the user may be typing into stays put, the same discipline as Tokens). The last-run
  // column still moves - a run finished, "3 min. ago" became "4 min. ago" - so the columns
  // are patched in place, the row and its ⋯ staying the same nodes.
  if (signature() === painted) { patchCols(); return; }
  render();
}

/** Swap each drawn row's columns for fresh ones where they read differently. A column that
 *  appeared or went (the record stopped or started answering) is structure: repaint. */
function patchCols()       {
  const region = document.getElementById("rmGroups"); // null once another page took the pane
  if (!region) return;
  for (const t of targets) {
    const node = Array.prototype.find.call(region.querySelectorAll             ("[data-rmrow]"),
      (n             ) => n.dataset.rmrow === t.id)                           ;
    if (!node) continue;
    const was = node.querySelectorAll(":scope > .lrow-col");
    const now = rowNode(t).querySelectorAll(":scope > .lrow-col");
    if (was.length !== now.length) { render(); return; }
    was.forEach((c, i) => { if (c.outerHTML !== now[i].outerHTML) c.replaceWith(now[i]); });
  }
}
export async function poll() { await refresh(); }
export function countText() { return targets.length ? trn(targets.length, "remote.nTargets.one", "remote.nTargets.other") : ""; }
export function unmount() {}
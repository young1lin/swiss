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
import type { ApiRemoteEndpointsResponse, ApiRemoteTargetsResponse } from "../types/api.js";
import type { GroupCfg } from "../types/dom.js";
import type { RemoteEndpointRow, RemoteTargetBody, RemoteTargetRow } from "../types/runs.js";
import { $, apiJson, iconNode, targetEl, toast } from "../util.js";
import { closeSheet } from "../add-sheet.js";
import { fill, h } from "../h.js";
import { popupMenu } from "../menu.js";
import { assignMember, groupFieldNode, groupOf, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, saveOrder, slice } from "../groups.js";
import { tr, trn } from "../i18n.js";

let targets = [] as RemoteTargetRow[];
let endpoints = [] as RemoteEndpointRow[];
let presence = "";
let groupNames = ["default"];
let editing = null as string | null; // the id an open sheet is editing, null when adding
let pendingGroup = null as string | null; // the group whose header + opened the sheet; the select still wins
let dragging = null as string | null; // in-flight row drag: a poll must not rebuild under it
let draggingGroup = null as string | null; // in-flight group drag, same rule
let painted = ""; // structural signature of the drawn list; a poll that changes nothing repaints nothing
let collapsed = {} as Record<string, boolean>; // the groups fold map, loaded once before the first paint

async function load() {
  const e = await apiJson<ApiRemoteEndpointsResponse>("/api/remote/endpoints");
  const t = await apiJson<ApiRemoteTargetsResponse>("/api/remote/targets");
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

function endpointLabel(id: string): string {
  const hit = endpoints.find((e) => { return e.id === id; });
  return hit ? hit.label || hit.id : id;
}

// The dot mirrors the endpoint's state: filled = connected, hollow = idle (will start
// on demand), amber = mid-transition. The title always says the state in words.
function endpointDotNode(id: string): HTMLElement {
  const st = (endpoints.find((e) => { return e.id === id; }) || {} as RemoteEndpointRow).state || "unknown";
  const cls = st === "connected" || st === "up" ? "up" : st === "idle" ? "idle" : "starting";
  return h("span", { class: "dot " + cls, title: tr("remote.endpointState", { state: st }) });
}

function signature(): string {
  return presence + "\u0000" + endpoints.map((e) => { return e.id + "=" + e.state; }).join(",") + "\u0000" +
    groupNames.join(",") + "\u0000" +
    targets.map((t) => {
      return [t.id, t.endpoint, t.workspaceRoot, (t.capabilities || []).join("+"), t.group || ""].join("|");
    }).join("\n");
}

function chipNode(text: string): HTMLElement {
  return h("span", { class: "side-type" }, text);
}

function rowNode(t: RemoteTargetRow): HTMLElement {
  // A label the sheet defaulted to the alias (see save) is not worth saying twice.
  const label = t.label && t.label !== t.id ? h("span", { class: "text-3" }, " " + t.label) : null;
  return h("div", { class: "row row-act", data: { rmrow: t.id } },
    endpointDotNode(t.endpoint),
    h("div", { class: "row-main" },
      h("div", { class: "name" },
        h("a", { href: "#remote", class: "rowname", data: { rmedit: t.id } }, t.id),
        label),
      h("div", { class: "rm-sub" }, t.workspaceRoot || "")),
    h("div", { class: "row-chips" },
      chipNode(endpointLabel(t.endpoint)),
      (t.capabilities || []).map(chipNode)),
    h("button", { class: "btn ghost icon", data: { rmmore: t.id },
        aria: { label: tr("remote.actionsName", { name: t.id }) }, title: tr("remote.actionsName", { name: t.id }) },
      iconNode("ellipsis")));
}

/** The groups component's cfg (docs/20): this page's nouns, rows and moves. */
function cfg(): GroupCfg<RemoteTargetRow> {
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
function paint(): void {
  const region = $("rmGroups");
  if (!region) return;
  region.textContent = ""; // the fill() wipe without the children: groups append one by one
  slice(targets, groupNames, groupOf(groupNames)).forEach((g) => {
    region.appendChild(mountGroup(cfg(), g));
  });
}

function render(): void {
  painted = signature();
  // The body header is the family's (tunnels/secrets): task prose + status line on the
  // left, actions right-aligned in pane-actions - no location title (the context bar
  // already says Remote Targets) and no invented classes. One sentence of what, one of
  // who else writes it; the drag is taught by the list itself, not by prose.
  // The status line names the presence only when it is NOT the normal one: "serving ·
  // 3 endpoints served by tunnels" said serving twice.
  const status = trn(endpoints.length, "remote.nEndpointsServedTunnels.one", "remote.nEndpointsServedTunnels.other");
  fill($("pane"),
    h("div", { class: "wide" },
      h("div", { class: "pane-head" },
        h("div", null,
          h("div", { class: "pane-desc" }, tr("remote.machinesGatewayCanRun")),
          h("div", { class: "pane-sub" },
            presence !== "serving" ? h("span", null, tr("remote.tunnelsState", { state: presence })) : null,
            status)),
        h("div", { class: "pane-actions" },
          h("button", { class: "btn primary", id: "rmAdd" }, tr("remote.addTarget")),
          h("button", { class: "btn", id: "rmNewGroup" }, tr("remote.newGroup")))),
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
function moveRow(id: string, targetId: string, before: boolean): void {
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
async function assign(id: string, group: string | null): Promise<void> {
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

/* The Add/Edit sheet. Same shape as every sheet: #sheet unhidden BEFORE innerHTML,
   closeSheet from add-sheet.js, backdrop click closes. */
function openSheet(target: RemoteTargetRow | null): void {
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
  const caps = ["exec", "sync", "files"];
  // The house sheet idiom (panel-proof-of-life rule 1): visible BEFORE the body is painted.
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog",
        aria: { modal: "true", label: editing ? tr("remote.editRemoteTarget") : tr("remote.addRemoteTarget") } },
      h("div", { class: "sheet-head" },
        h("h2", null, editing ? tr("remote.editName", { name: editing }) : tr("remote.addRemoteTarget"))),
      h("div", { class: "sheet-body" },
        h("label", { class: "field" },
          h("span", null, tr("remote.aliasNameCommandsCall")),
          h("input", { id: "rm-id", placeholder: tr("remote.build") })),
        h("label", { class: "field" },
          h("span", null, tr("remote.labelOptional")), h("input", { id: "rm-label" })),
        h("label", { class: "field" },
          h("span", null, tr("remote.endpointTunnelsConnection")),
          h("select", { id: "rm-endpoint" }, endpointOptions)),
        groupFieldNode(groupNames, groupSel),
        h("label", { class: "field" },
          h("span", null, tr("remote.workspaceRootAbsolutePosix")),
          h("input", { id: "rm-root", placeholder: tr("remote.dataWsProj") })),
        h("div", { class: "field" },
          h("span", null, tr("remote.capabilities")),
          h("div", null, caps.map((c) => {
            return h("label", { class: "check" }, h("input", { type: "checkbox", id: "rmcap-" + c }), " " + c);
          })))),
      h("div", { class: "sheet-foot" },
        h("button", { class: "btn", id: "rm-cancel" }, tr("remote.cancel")),
        h("button", { class: "btn primary", id: "rm-save" }, editing ? tr("remote.save") : tr("remote.add")))));
  // Prefill programmatically, not via value=" markup": the pane redraws by signature,
  // and programmatic values are the one source of truth an edit and a test can both read.
  $<HTMLInputElement>("rm-id").disabled = !!editing; // the alias never edits; state set here, not in markup
  caps.forEach((c) => {
    $<HTMLInputElement>("rmcap-" + c).checked = target ? (target.capabilities || []).includes(c) : c === "exec";
  });
  if (target) {
    $<HTMLInputElement>("rm-label").value = target.label === target.id ? "" : target.label || "";
    $<HTMLInputElement>("rm-root").value = target.workspaceRoot || "";
  }
  $("rm-cancel").onclick = closeSheet;
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
  $("rm-save").onclick = save;
  if (!editing) $("rm-id").focus();
}

async function save(): Promise<void> {
  const caps = ["exec", "sync", "files"].filter((c) => { return $<HTMLInputElement>("rmcap-" + c).checked; });
  if (!caps.length) { toast(tr("remote.pickLeastOneCapability"), true); return; }
  const id = editing || $<HTMLInputElement>("rm-id").value.trim();
  const body: RemoteTargetBody = {
    // The store refuses an empty label (swiss-remote target.rs), and the field says
    // optional: an alias is a fine label, so a blank one becomes the alias rather than
    // a rejected save the user cannot see the reason for.
    label: $<HTMLInputElement>("rm-label").value.trim() || id,
    endpoint: $<HTMLSelectElement>("rm-endpoint").value,
    // The group the select shows - the row is born INTO it server-side (docs/34 R8),
    // one write, no second assign round-trip.
    group: $<HTMLSelectElement>("g-sel") ? $<HTMLSelectElement>("g-sel").value : null,
    workspaceRoot: $<HTMLInputElement>("rm-root").value.trim(),
    capabilities: caps,
    // The one shell the surface speaks today (docs/34): the route requires it, and a
    // select with a single honest option would be decoration.
    shell: "posix",
  };
  if (!body.workspaceRoot || body.workspaceRoot.charAt(0) !== "/") {
    toast(tr("remote.workspaceRootMustAbsolute"), true);
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

async function removeTarget(id: string): Promise<void> {
  if (!confirm(tr("remote.deleteTargetIdTunnels", { id }))) return;
  const d = await apiJson("/api/remote/targets/" + encodeURIComponent(id), { method: "DELETE" });
  if (!d) return;
  await reload();
}

export async function mount() {
  collapsed = loadCollapsed("targets");
  if (!(await load())) return;
  render();
  $("pane").onclick = (event: MouseEvent): void => {
    const add = targetEl(event)?.closest("#rmAdd");
    if (add) { pendingGroup = null; openSheet(null); return; }
    const newGroup = targetEl(event)?.closest("#rmNewGroup");
    if (newGroup) { void newGroupFlow("targets", groupNames, reload); return; }
    const edit = targetEl(event)?.closest<HTMLElement>("[data-rmedit]");
    if (edit) {
      const hit = targets.find((t) => { return t.id === edit?.dataset.rmedit; });
      if (hit) openSheet(hit);
      return;
    }
    const more = targetEl(event)?.closest<HTMLElement>("[data-rmmore]");
    if (more) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      const t = targets.find((x) => { return x.id === more?.dataset.rmmore; });
      if (!t) return;
      popupMenu(more.getBoundingClientRect(), [
        { label: tr("remote.edit"), fn: () => { openSheet(t!); } },
        { sep: true },
        { label: tr("remote.delete"), danger: true, fn: () => { void removeTarget(t?.id); } },
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
export function countText() { return targets.length ? trn(targets.length, "remote.nTargets.one", "remote.nTargets.other") : ""; }
export function unmount() {}
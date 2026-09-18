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
   Remote Targets - the remote plugin page (#remote), docs/34 R6.

   A hairline card of house rows: dot (endpoint state) + alias + mono root sub-line +
   monochrome chips for endpoint and capabilities + one overflow menu. The CLI
   (swiss remote ...) and this page write the SAME rows through the same routes; the
   page exists so a target never needs a terminal to exist.
   ================================================================================================ */
import { $, apiJson, emptyHtml, esc, toast } from "../util.js";
import { closeSheet } from "../add-sheet.js";
import { popupMenu } from "../menu.js";

var targets = [];
var endpoints = [];
var presence = "";
var editing = null; // the id an open sheet is editing, null when adding

async function load() {
  var e = await apiJson("/api/remote/endpoints");
  var t = await apiJson("/api/remote/targets");
  if (!e || !t) return false; // apiJson toasted; keep the last paint
  presence = e.presence || "none";
  endpoints = e.endpoints || [];
  targets = t.targets || [];
  return true;
}

function endpointLabel(id) {
  var hit = endpoints.find(function (e) { return e.id === id; });
  return hit ? hit.label || hit.id : id;
}

// The dot mirrors the endpoint's state: filled = connected, hollow = idle (will start
// on demand), amber = mid-transition. The title always says the state in words.
function endpointDot(id) {
  var st = (endpoints.find(function (e) { return e.id === id; }) || {}).state || "unknown";
  var cls = st === "connected" || st === "up" ? "up" : st === "idle" ? "idle" : "starting";
  return '<span class="dot ' + cls + '" title="endpoint ' + esc(st) + '"></span>';
}

var painted = ""; // structural signature of the drawn list; a poll that changes nothing repaints nothing

function signature() {
  return presence + "\u0000" + endpoints.map(function (e) { return e.id + "=" + e.state; }).join(",") + "\u0000" +
    targets.map(function (t) { return [t.id, t.endpoint, t.workspaceRoot, (t.capabilities || []).join("+")].join("|"); }).join("\n");
}

function chip(text) {
  return '<span class="side-type">' + esc(text) + "</span>";
}

function row(t) {
  var caps = (t.capabilities || []).map(chip).join("");
  var label = t.label ? ' <span class="text-3">' + esc(t.label) + "</span>" : "";
  return (
    '<div class="row row-act">' + endpointDot(t.endpoint) +
      '<div class="row-main">' +
        '<div class="name"><a href="#remote" class="rowname" data-rmedit="' + esc(t.id) + '">' + esc(t.id) + "</a>" + label + "</div>" +
        '<div class="sub mono">' + esc(t.workspaceRoot || "") + "</div>" +
      "</div>" +
      '<div class="row-chips">' + chip(endpointLabel(t.endpoint)) + caps + "</div>" +
      '<button class="ic" data-rmmore="' + esc(t.id) + '" aria-label="Actions for ' + esc(t.id) + '">&#8943;</button>' +
    "</div>"
  );
}

function render() {
  painted = signature();
  var head =
    '<div class="page-head"><h2>Targets</h2>' +
    '<button class="btn primary" id="rmAdd">Add target</button></div>' +
    '<p class="quiet">' + esc(presence) + " · " + endpoints.length + " endpoint" +
    (endpoints.length === 1 ? "" : "s") + " served by tunnels</p>";
  if (!targets.length) {
    $("pane").innerHTML = head + emptyHtml({ icon: "globe", title: "No targets yet", hint: "Add a target to run commands on the machines the Tunnels connections reach." });
    $("countChip").textContent = "";
  } else {
    $("pane").innerHTML = head + '<div class="group">' + targets.map(row).join("") + "</div>";
    $("countChip").textContent = targets.length + " target" + (targets.length === 1 ? "" : "s");
  }
}

/* The Add/Edit sheet. Same shape as every sheet: #sheet unhidden BEFORE innerHTML,
   closeSheet from add-sheet.js, backdrop click closes. */
function openSheet(target) {
  editing = target ? target.id : null;
  var endpointOptions = endpoints
    .map(function (e) {
      var sel = target && target.endpoint === e.id ? " selected" : "";
      return '<option value="' + esc(e.id) + '"' + sel + ">" + esc(e.label || e.id) + "</option>";
    })
    .join("");
  var caps = ["exec", "sync", "files"];
  var capBoxes = caps
    .map(function (c) {
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
        '<label class="field"><span>Workspace root (absolute POSIX path)</span><input id="rm-root" placeholder="/data/ws/proj"></label>' +
        '<div class="field"><span>Capabilities</span><div>' + capBoxes + "</div></div>" +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="rm-cancel">Cancel</button>' +
      '<button class="btn primary" id="rm-save">' + (editing ? "Save" : "Add") + "</button></div>" +
    "</div>";
  $("sheet").hidden = false;
  // Prefill programmatically, not via value=" markup": the pane redraws by signature,
  // and programmatic values are the one source of truth an edit and a test can both read.
  $("rm-id").disabled = !!editing; // the alias never edits; state set here, not in markup
  caps.forEach(function (c) {
    $("rmcap-" + c).checked = target ? (target.capabilities || []).indexOf(c) >= 0 : c === "exec";
  });
  if (target) {
    $("rm-label").value = target.label || "";
    $("rm-root").value = target.workspaceRoot || "";
  }
  $("rm-cancel").onclick = closeSheet;
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeSheet(); };
  $("rm-save").onclick = save;
  if (!editing) $("rm-id").focus();
}

async function save() {
  var caps = ["exec", "sync", "files"].filter(function (c) { return $("rmcap-" + c).checked; });
  if (!caps.length) { toast("Pick at least one capability", true); return; }
  var body = {
    label: $("rm-label").value.trim(),
    endpoint: $("rm-endpoint").value,
    workspaceRoot: $("rm-root").value.trim(),
    capabilities: caps,
    // The one shell the surface speaks today (docs/34): the route requires it, and a
    // select with a single honest option would be decoration.
    shell: "posix",
  };
  if (!body.workspaceRoot || body.workspaceRoot.charAt(0) !== "/") {
    toast("Workspace root must be an absolute POSIX path", true);
    return;
  }
  var url = "/api/remote/targets";
  if (editing) url += "/" + encodeURIComponent(editing);
  else body.id = $("rm-id").value.trim();
  var j = await apiJson(url, { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  closeSheet();
  await load();
  render();
}

async function removeTarget(id) {
  if (!confirm("Delete target " + id + "? The Tunnels connection and any files on the machine are not touched.")) return;
  var d = await apiJson("/api/remote/targets/" + encodeURIComponent(id), { method: "DELETE" });
  if (!d) return;
  await load();
  render();
}

export async function mount() {
  if (!(await load())) return;
  render();
  $("pane").onclick = function (event) {
    var add = event.target.closest("#rmAdd");
    if (add) { openSheet(null); return; }
    var edit = event.target.closest("[data-rmedit]");
    if (edit) {
      var hit = targets.find(function (t) { return t.id === edit.dataset.rmedit; });
      if (hit) openSheet(hit);
      return;
    }
    var more = event.target.closest("[data-rmmore]");
    if (more) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      var t = targets.find(function (x) { return x.id === more.dataset.rmmore; });
      if (!t) return;
      popupMenu(more.getBoundingClientRect(), [
        { label: "Edit", fn: function () { openSheet(t); } },
        { sep: true },
        { label: "Delete", danger: true, fn: function () { removeTarget(t.id); } },
      ]);
    }
  };
}

export async function refresh() {
  if (!(await load())) return;
  // A poll that changed nothing repaints nothing: the pane keeps its nodes (and a
  // sheet the user may be typing into stays put, the same discipline as Tokens).
  if (signature() === painted) return;
  render();
}
export async function poll() { await refresh(); }
export function countText() { return targets.length ? targets.length + " targets" : ""; }
export function unmount() {}

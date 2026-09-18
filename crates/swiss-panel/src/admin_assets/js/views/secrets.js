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
   Secrets — the Settings group's second page (docs/19 D6).

   The vault is host-owned: every plugin may depend on it, so it is not a plugin itself and
   lives beside Plugins, not inside it. Names only, ever (D5: write-only — a forgotten value
   is re-stored, never revealed); the value a row shows is the REFERENCE to copy into a
   credential field, not the credential. Writes carry the rev the list was drawn from, the
   same discipline Tokens and Plugins have.

   Groups (docs/20 G6, docs/26): the list renders through the groups component under the
   full family contract — rows drag within and across groups in one gesture, the grip
   reorders groups. The stored order is the vault's third list; empty means name order.
   The store form carries a Group select; the header + preselects it. Group labels are
   folder names, not credentials — they are the one thing about a secret a listing may say
   beyond its name.
   ================================================================================================ */
import { $, apiJson, emptyHtml, esc, icon, state, toast } from "../util.js";
import { copyText } from "../connect.js";
import { popupMenu } from "../menu.js";
import { assignMember, groupOf as makeGroupOf, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, saveOrder, slice } from "../groups.js";

var painted = ""; // structural signature of the drawn list; a change means the rows move
var collapsed                          = {}; // the secrets fold map, loaded once before the first paint
var draggingRow                = null; // the row name a gesture carries (the component reads it via cfg.drag)
var draggingGroup                = null; // the group name a header grip carries

/** The rendering group of one name: the stored label while its group lives, else the first
 *  group — the sink rule. An older gateway answers no groups at all: everything reads as
 *  the single default group, which the component draws as no divider at all. */
function groupOfName(name        )         {
  var g = secrets.memberGroups && secrets.memberGroups[name];
  return (secrets.groups || []).indexOf(g) >= 0 ? g : (secrets.groups || ["default"])[0];
}

function signature()         {
  return (secrets.groups || []).join("\n") + "\u0000" +
    secrets.list.map(function (n        )         { return n + "\u0001" + groupOfName(n); }).join("\n");
}

/** The in-page copy of GET /api/secrets. On an older gateway the route 404s: apiJson toasts
 *  once and we draw the empty page — the same degradation every host page already has. */
var secrets                                                                                                                             = { list: [], rev: 0, loaded: false, groups: ["default"], memberGroups: {} };

async function loadSecrets()                {
  var j = await apiJson                                                                                                                  ("/api/secrets");
  if (!j) return;
  secrets.list = j.secrets || [];
  secrets.rev = j.rev || 0;
  secrets.groups = j.groups && j.groups.length ? j.groups : ["default"];
  secrets.memberGroups = j.secretGroups || {};
  secrets.order = j.order || []; // an older gateway answers without one: name order
  secrets.loaded = true;
  applyOrder();
}

/** Rank by the stored order (docs/26): names the list mentions keep their slot, everything
 *  else lands after them in name order — the same rule the tunnels store ranks by. */
function applyOrder()       {
  var rank                         = {};
  (secrets.order || []).forEach(function (n        , i        )       {
    if (!(n in rank)) rank[n] = i;
  });
  var unranked = secrets.order ? secrets.order.length : 0;
  secrets.list.sort(function (a        , b        )         {
    var ra = a in rank ? rank[a] : unranked;
    var rb = b in rank ? rank[b] : unranked;
    return ra === rb ? a.localeCompare(b) : ra - rb;
  });
}

/** One secret's row: the one button (Copy ref) and the overflow menu holding Delete - red
 *  never sits on a row (design rule 4). The buttons stay delegated on #pane (wire), so the
 *  groups component rebuilding a card never rewires them. */
function rowHtml(name        )         {
  return '<div class="row" data-secret="' + esc(name) + '"><div class="row-main">' +
    '<div class="name">' + esc(name) + "</div>" +
    '<div class="desc"><code>${secret://' + esc(name) + '}</code> — substituted at run time wherever a credential is used</div>' +
    "</div>" + '<div class="row-act">' +
      '<button class="btn" data-skcopy="' + esc(name) + '">Copy ref</button> ' +
      '<button class="btn ghost icon" data-skmore="' + esc(name) + '" aria-label="Actions for ' + esc(name) + '" title="Delete">' + icon("ellipsis") + "</button>" +
    "</div></div>";
}

function rowsHtml()         {
  return secrets.list.length ? secrets.list.map(rowHtml).join("") : "";
}

/** The secrets scope's cfg for mountGroup (docs/26): the full family contract — rows drag
 *  within and across groups in one gesture (a drop carries order + membership), the grip
 *  reorders groups. Rows are plain names, so rowId is the identity. */
function skCfg()                   {
  return {
    scope: "secrets",
    density: "page",
    names: secrets.groups || ["default"],
    collapsed: collapsed,
    noun: "secret",
    addTitle: function (g        )         { return "Store a secret into " + g; },
    onAdd: function (g        )       {
      // The + points at the inline form: pick the group it names and put the cursor in the
      // name box — the sheet scopes open a modal; this page's flow was always inline.
      var sel = $              ("skGroup");
      if (sel) sel.value = g;
      $             ("skName").focus();
    },
    reload: function ()                { return loadSecrets().then(paintGroups); },
    render: paintGroups,
    afterDrag: function () { paintGroups(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: function ()                { return draggingRow; },
      set: function (v               )       { draggingRow = v; },
    },
    dragGroup: {
      get: function ()                { return draggingGroup; },
      set: function (v               )       { draggingGroup = v; },
    },
    rowsById: function ()           { return secrets.list; },
    rowId: function (r        )         { return r; },
    groupOfRow: groupOfName,
    rowsHtml: function (g                    )         { return g.rows.map(rowHtml).join(""); },
    rowSel: function (r        )         {
      var v = window.CSS && CSS.escape ? CSS.escape(r) : r;
      return '[data-secret="' + v + '"]';
    },
    onMoveRow: moveSecretRow,
    onAssign: function (id        , g               )       { void moveSecretGroup(id, g); },
  };
}

/** Move one row to just before/after another (docs/26): re-render from the moved list, then
 *  persist the flat order. The PUT bumps the rev the next value write must name, so the
 *  reload that follows it is not optional polish. */
function moveSecretRow(id        , target        , before         )       {
  if (!id || !target || id === target) return;
  var from = secrets.list.indexOf(id);
  if (from < 0) return;
  secrets.list.splice(from, 1);
  var to = secrets.list.indexOf(target);
  if (to < 0) return; // target vanished mid-drag — leave everything where it is
  secrets.list.splice(before ? to : to + 1, 0, id);
  paintGroups();
  void saveOrder("secrets", secrets.list.slice()).then(function (j         )       {
    if (j) loadSecrets().then(paintGroups); // the order PUT bumped the rev — resync it
  });
}

/** Put one secret in a group (the cross-group half of a row drag, docs/26): the family's
 *  member PUT bumps the rev too, so the reload resyncs both the list and the rev the next
 *  value write needs. */
async function moveSecretGroup(id        , group               )                {
  await assignMember("secrets", id, group);
  await loadSecrets();
  paintGroups();
}

/** (Re)build the groups region only — the store form lives outside it, so a repaint never
 *  wipes a half-typed value, the one place a value is ever typed. */
function paintGroups()       {
  var host = $             ("skGroups");
  if (!host) return;
  painted = signature();
  host.innerHTML = "";
  if (!secrets.list.length) {
    host.innerHTML = emptyHtml({ icon: "key", title: "No secrets yet", hint: "Store a credential once, reference it everywhere as ${secret://name}." });
    return;
  }
  slice(secrets.list, secrets.groups || ["default"], groupOfName).forEach(function (g                    )       {
    host.appendChild(mountGroup(skCfg(), g));
  });
  refreshGroupSelect();
  var chip = $             ("countChip");
  if (chip) chip.textContent = countText();
}

/** Poll-safe update: the groups region only, and only when the structure changed. */
function patch()       {
  if (signature() === painted) return;
  paintGroups(); // repaints the rows AND the chip
}

/** Keep the form's Group select honest after a group mutation (docs/20 G6): a rename or
 *  delete that the poll reports must not leave a chosen-but-dead option in the box - the
 * next store would assign into a 400. The selection survives while its group lives; a dead
 * one falls back to the scope's last-used, else the first group. */
function refreshGroupSelect()       {
  var sel = $              ("skGroup");
  if (!sel) return;
  var names = secrets.groups && secrets.groups.length ? secrets.groups : ["default"];
  var wanted = sel.value && names.indexOf(sel.value) >= 0
    ? sel.value
    : resolveDefaultGroup(names, lastGroup("secrets"));
  var next = names.map(function (n        )         {
    return '<option value="' + esc(n) + '"' + (n === wanted ? " selected" : "") + ">" + esc(n) + "</option>";
  }).join("");
  if (sel.innerHTML !== next) sel.innerHTML = next;
}

/** The Group select of the store form: the scope's groups, the last-used one selected. */
function groupSelectHtml()         {
  var names = secrets.groups && secrets.groups.length ? secrets.groups : ["default"];
  var picked = resolveDefaultGroup(names, lastGroup("secrets"));
  return '<select class="v v-sk-group" id="skGroup" title="The group this secret lists under">' +
    names.map(function (n        )         {
      return '<option value="' + esc(n) + '"' + (n === picked ? " selected" : "") + ">" + esc(n) + "</option>";
    }).join("") + "</select>";
}

function render()       {
  painted = signature();
  // No location title: the context bar already says "Settings / Secrets".
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div>' +
      '<div class="pane-desc">Device-bound vault (docs/19). A value is written once and never shown again — not here, not in any API answer; a forgotten one can only be re-stored. Reference it wherever a credential goes: <code>${secret://name}</code> in a header, a URL, a command or an env value. A missing reference fails loudly at first use, naming where it was needed.</div>' +
    "</div>" +
      '<div class="pane-actions"><button class="btn" id="skNewGroup">New group</button></div>' +
    "</div>" +
    // The inline create form (docs/35 §3): one row, the Group select beside the primary.
    '<div class="inline-form">' +
      '<input class="v" id="skName" placeholder="Name — lowercase kebab (a-z 0-9 -)">' +
      '<input class="v grow" id="skValue" type="password" placeholder="Value — write-only, never shown again">' +
      groupSelectHtml() +
      '<button class="btn primary" id="skStore">Store</button>' +
    "</div>" +
    '<div id="skGroups"></div>' +
  "</div>";
  paintGroups();
  var chip = $             ("countChip");
  if (chip) chip.textContent = countText();
  wire();
}

function wire()       {
  $("pane").onclick = async function (event            )                {
    var hit = event.target && event.target.closest ? event.target.closest             ("[data-skcopy],[data-skmore],#skStore,#skNewGroup") : null;
    if (!hit) return;
    if (hit.id === "skStore") { await storeSecret(); return; }
    if (hit.id === "skNewGroup") {
      newGroupFlow("secrets", secrets.groups || ["default"], function ()                {
        return loadSecrets().then(function ()       { render(); });
      });
      return;
    }
    if (hit.dataset.skcopy) { copyText("${secret://" + hit.dataset.skcopy + "}", "Reference"); return; }
    if (hit.dataset.skmore) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      var name = hit.dataset.skmore;
      popupMenu(hit.getBoundingClientRect(), [
        { label: "Delete", danger: true, fn: function ()       { void removeSecret(name ); } },
      ]);
    }
  };
}

/** Store one secret, then redraw from the vault's own answer. The rev goes with the write;
 *  a second panel that changed the vault in between loses the race with a visible 409.
 *  The selected group follows the name in one family write — an older gateway without the
 *  family keeps the first-group default, which is where the select's only option lands. */
async function storeSecret()                {
  var name = ($             ("skName").value || "").trim();
  var value = $             ("skValue").value || "";
  if (!name || !value) { toast("a secret needs both a name and a value", true); return; }
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(name)) { toast("names are lowercase kebab: a-z, 0-9, dashes", true); return; }
  var picked = $("skGroup") ? $              ("skGroup").value : null;
  var j = await apiJson         ("/api/secrets/" + encodeURIComponent(name), {
    method: "PUT",
    body: JSON.stringify({ value: value, rev: secrets.rev }),
  });
  if (!j) return; // the toast said why; keep the form exactly as typed
  if (picked) {
    rememberGroup("secrets", picked);
    await assignMember("secrets", name, picked);
  }
  $             ("skValue").value = "";
  await loadSecrets();
  paintGroups();
  toast("stored — reference it as ${secret://" + name + "}");
}

/** Delete one secret after an explicit confirm: everything referencing it starts failing
 *  honestly until it is re-stored. */
async function removeSecret(name        )                {
  if (!confirm('Delete secret "' + name + '"? Everything referencing ${secret://' + name + "} starts failing until it is re-stored.")) return;
  var j = await apiJson         ("/api/secrets/" + encodeURIComponent(name) + "?rev=" + secrets.rev, { method: "DELETE" });
  if (!j) return;
  await loadSecrets();
  paintGroups();
  toast("deleted " + name);
}

export async function mount() {
  collapsed = loadCollapsed("secrets"); // before the first paint, so folded groups never flash open
  await loadSecrets();
  render();
}
export async function refresh() { await loadSecrets(); render(); }
export async function poll() { await loadSecrets(); patch(); }
export function countText() {
  var n = secrets.list.length;
  return n + (n === 1 ? " secret" : " secrets");
}
export function unmount() { painted = ""; }

/* Exported for the suite (the requiresBadge precedent): the rows markup and the two mutations,
 * so the contract is testable without a DOM that parses HTML. __setVaultForTest restores the
 * pre-first-load state between cases. */
export { rowsHtml, storeSecret, removeSecret, moveSecretRow };
export function __setVaultForTest(list          , rev        , order          )       {
  secrets = { list: list || [], rev: rev || 0, loaded: true, groups: ["default"], memberGroups: {}, order: order || [] };
  applyOrder();
}
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
                                                            
import { $, apiJson, emptyNode, iconNode, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
import { copyText } from "../connect.js";
import { popupMenu } from "../menu.js";
import { assignMember, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, saveOrder, slice } from "../groups.js";
import { tr, trn } from "../i18n.js";

let painted = ""; // structural signature of the drawn list; a change means the rows move
let collapsed                          = {}; // the secrets fold map, loaded once before the first paint
let draggingRow                = null; // the row name a gesture carries (the component reads it via cfg.drag)
let draggingGroup                = null; // the group name a header grip carries

/** The rendering group of one name: the stored label while its group lives, else the first
 *  group — the sink rule. An older gateway answers no groups at all: everything reads as
 *  the single default group, which the component draws as no divider at all. */
function groupOfName(name        )         {
  const g = secrets.memberGroups && secrets.memberGroups[name];
  return (secrets.groups || []).includes(g) ? g : (secrets.groups || ["default"])[0];
}

function signature()         {
  return (secrets.groups || []).join("\n") + "\u0000" +
    secrets.list.map((n        )         => { return n + "\u0001" + groupOfName(n); }).join("\n");
}

/** The in-page copy of GET /api/secrets. On an older gateway the route 404s: apiJson toasts
 *  once and we draw the empty page — the same degradation every host page already has. */
const secrets                                                                                                                             = { list: [], rev: 0, loaded: false, groups: ["default"], memberGroups: {} };

async function loadSecrets()                {
  const j = await apiJson                                                                                                                  ("/api/secrets");
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
  const rank                         = {};
  (secrets.order || []).forEach((n        , i        )       => {
    if (!(n in rank)) rank[n] = i;
  });
  const unranked = secrets.order ? secrets.order.length : 0;
  secrets.list.sort((a        , b        )         => {
    const ra = a in rank ? rank[a] : unranked;
    const rb = b in rank ? rank[b] : unranked;
    return ra === rb ? a.localeCompare(b) : ra - rb;
  });
}

/** One secret's row: the one button (Copy ref) and the overflow menu holding Delete - red
 *  never sits on a row (design rule 4). The buttons stay delegated on #pane (wire), so the
 *  groups component rebuilding a card never rewires them.
 *
 *  Built with h() (docs/37 R5), which is why there is no esc() left in here: a name is a
 *  text node and an attribute value, and neither can close a tag. The literal " " child is
 *  the space the string version carried between the two buttons - an inline box, so the gap
 *  is real layout, not formatting. The reference in .desc keeps its <code> element: it is
 *  the thing a user is meant to recognise on sight. */
function rowNode(name        )              {
  return h("div", { class: "row", data: { secret: name } },
    h("div", { class: "row-main" },
      h("div", { class: "name" }, name),
      h("div", { class: "desc" },
        h("code", null, "${secret://" + name + "}"),
        " ", tr("secrets.substitutedRunTimeWherever"))),
    h("div", { class: "row-act" },
      h("button", { class: "btn", data: { skcopy: name } }, tr("secrets.copyRef")), " ",
      h("button", {
        class: "btn ghost icon",
        data: { skmore: name },
        aria: { label: tr("secrets.actionsName", { name }) },
        title: tr("secrets.delete"),
      }, iconNode("ellipsis"))));
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
    noun: tr("secrets.secret"),
    addTitle: (g        )         => { return tr("secrets.storeSecretIntoGroup", { group: g }); },
    onAdd: (g        )       => {
      // The + points at the inline form: pick the group it names and put the cursor in the
      // name box — the sheet scopes open a modal; this page's flow was always inline.
      const sel = $                   ("skGroup");
      if (sel) sel.value = g;
      $                  ("skName").focus();
    },
    reload: ()                => { return loadSecrets().then(paintGroups); },
    render: paintGroups,
    afterDrag: () => { paintGroups(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: ()                => { return draggingRow; },
      set: (v               )       => { draggingRow = v; },
    },
    dragGroup: {
      get: ()                => { return draggingGroup; },
      set: (v               )       => { draggingGroup = v; },
    },
    rowsById: ()           => { return secrets.list; },
    rowId: (r        )         => { return r; },
    groupOfRow: groupOfName,
    /* rowNode, not rowsHtml + rowSel (docs/37 R5): the component wires the node this builder
     * hands back, so the round trip through a parsed string and a [data-secret] lookup - and
     * the CSS.escape that lookup needed because a secret name is arbitrary bytes from the
     * vault's side - is gone. */
    rowNode: rowNode,
    onMoveRow: moveSecretRow,
    onAssign: (id        , g               )       => { void moveSecretGroup(id, g); },
  };
}

/** Move one row to just before/after another (docs/26): re-render from the moved list, then
 *  persist the flat order. The PUT bumps the rev the next value write must name, so the
 *  reload that follows it is not optional polish. */
function moveSecretRow(id        , target        , before         )       {
  if (!id || !target || id === target) return;
  const from = secrets.list.indexOf(id);
  if (from < 0) return;
  secrets.list.splice(from, 1);
  const to = secrets.list.indexOf(target);
  if (to < 0) return; // target vanished mid-drag — leave everything where it is
  secrets.list.splice(before ? to : to + 1, 0, id);
  paintGroups();
  void saveOrder("secrets", secrets.list.slice()).then((j         )       => {
    if (j) void loadSecrets().then(paintGroups); // the order PUT bumped the rev — resync it
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
  const host = $("skGroups");
  if (!host) return;
  painted = signature();
  if (!secrets.list.length) {
    fill(host, emptyNode({ icon: "key", title: tr("secrets.secrets"), hint: tr("secrets.storeCredentialOnceReference") }));
    return;
  }
  fill(host, slice(secrets.list, secrets.groups || ["default"], groupOfName).map((g                    )              => {
    return mountGroup(skCfg(), g);
  }));
  refreshGroupSelect();
  const chip = $("countChip");
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
  const sel = $                   ("skGroup");
  if (!sel) return;
  const names = secrets.groups && secrets.groups.length ? secrets.groups : ["default"];
  const wanted = sel.value && names.includes(sel.value)
    ? sel.value
    : resolveDefaultGroup(names, lastGroup("secrets"));
  // Only when the option LIST actually moved. The string version compared rendered markup to
  // decide that; the node version compares the values, which is the thing it meant. Rebuilding
  // options resets the box, and a poll must not do that under a hand that just picked a group.
  const shown = Array.from(sel.options).map((o                   )         => { return o.value; });
  const moved = shown.length !== names.length || shown.some((v        , i        )          => { return v !== names[i]; });
  if (moved) fill(sel, names.map((n        )                    => { return h("option", { value: n }, n); }));
  sel.value = wanted;
}

/** The Group select of the store form: the scope's groups, the last-used one selected.
 *  select.value = picked rather than a selected attribute on one option - the property is
 *  the selection, the attribute was only ever its initial default. */
function groupSelectNode()                    {
  const names = secrets.groups && secrets.groups.length ? secrets.groups : ["default"];
  const sel = h("select", { class: "v v-sk-group", id: "skGroup", title: tr("secrets.groupSecretListsUnder") },
    names.map((n        )                    => { return h("option", { value: n }, n); }));
  sel.value = resolveDefaultGroup(names, lastGroup("secrets"));
  return sel;
}

function render()       {
  painted = signature();
  // No location title: the context bar already says "Settings / Secrets".
  fill($("pane"), h("div", { class: "wide" },
    h("div", { class: "pane-head" },
      h("div", null,
        h("div", { class: "pane-desc" },
          tr("secrets.deviceBoundVaultValue"),
          h("code", null, "${secret://name}"),
          tr("secrets.headerUrlCommandEnv"))),
      h("div", { class: "pane-actions" },
        h("button", { class: "btn", id: "skNewGroup" }, tr("secrets.newGroup")))),
    // The inline create form (docs/35 §3): one row, the Group select beside the primary.
    h("div", { class: "inline-form" },
      h("input", { class: "v", id: "skName", placeholder: tr("secrets.nameLowercaseKebabZ") }),
      h("input", { class: "v grow", id: "skValue", type: "password", placeholder: tr("secrets.valueWriteOnlyNever") }),
      groupSelectNode(),
      h("button", { class: "btn primary", id: "skStore" }, tr("secrets.store"))),
    h("div", { id: "skGroups" })));
  paintGroups();
  const chip = $("countChip");
  if (chip) chip.textContent = countText();
  wire();
}

function wire()       {
  $("pane").onclick = async (event            )                => {
    const hit = targetEl(event)?.closest             ("[data-skcopy],[data-skmore],#skStore,#skNewGroup");
    if (!hit) return;
    if (hit.id === "skStore") { await storeSecret(); return; }
    if (hit.id === "skNewGroup") {
      newGroupFlow("secrets", secrets.groups || ["default"], ()                => {
        return loadSecrets().then(()       => { render(); });
      });
      return;
    }
    if (hit.dataset.skcopy) { void copyText("${secret://" + hit.dataset.skcopy + "}", tr("secrets.reference")); return; }
    if (hit.dataset.skmore) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      const name = hit.dataset.skmore;
      popupMenu(hit.getBoundingClientRect(), [
        { label: tr("secrets.delete"), danger: true, fn: ()       => { void removeSecret(name ); } },
      ]);
    }
  };
}

/** Store one secret, then redraw from the vault's own answer. The rev goes with the write;
 *  a second panel that changed the vault in between loses the race with a visible 409.
 *  The selected group follows the name in one family write — an older gateway without the
 *  family keeps the first-group default, which is where the select's only option lands. */
async function storeSecret()                {
  const name = ($                  ("skName").value || "").trim();
  const value = $                  ("skValue").value || "";
  if (!name || !value) { toast(tr("secrets.secretNeedsBothName"), true); return; }
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(name)) { toast(tr("secrets.namesLowercaseKebabZ"), true); return; }
  const picked = $("skGroup") ? $                   ("skGroup").value : null;
  const j = await apiJson         ("/api/secrets/" + encodeURIComponent(name), {
    method: "PUT",
    body: JSON.stringify({ value: value, rev: secrets.rev }),
  });
  if (!j) return; // the toast said why; keep the form exactly as typed
  if (picked) {
    rememberGroup("secrets", picked);
    await assignMember("secrets", name, picked);
  }
  $                  ("skValue").value = "";
  await loadSecrets();
  paintGroups();
  toast(tr("secrets.storedReferenceSecretName", { name }));
}

/** Delete one secret after an explicit confirm: everything referencing it starts failing
 *  honestly until it is re-stored. */
async function removeSecret(name        )                {
  if (!confirm(tr("secrets.deleteSecretNameEverything", { name }))) return;
  const j = await apiJson         ("/api/secrets/" + encodeURIComponent(name) + "?rev=" + secrets.rev, { method: "DELETE" });
  if (!j) return;
  await loadSecrets();
  paintGroups();
  toast(tr("secrets.deletedName", { name }));
}

export async function mount() {
  collapsed = loadCollapsed("secrets"); // before the first paint, so folded groups never flash open
  await loadSecrets();
  render();
}
export async function refresh() { await loadSecrets(); render(); }
export async function poll() { await loadSecrets(); patch(); }
export function countText() {
  const n = secrets.list.length;
  return trn(n, "secrets.nSecrets.one", "secrets.nSecrets.other");
}
export function unmount() { painted = ""; }

/* Exported for the suite (the requiresBadge precedent): the two mutations and the order move,
 * driven under a fetch stub against the real DOM the view builds. */
export { storeSecret, removeSecret, moveSecretRow };
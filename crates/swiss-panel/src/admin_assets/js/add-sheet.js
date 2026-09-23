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

                                                               
import { $, DEFAULT_GROUP, apiJson, toast } from "./util.js";
import { fill, h } from "./h.js";
import { openDetail, runConnTest } from "./detail.js";
import { TESTABLE_TYPES, TYPE_FIELDS, TYPE_LABELS, fieldsNode, readFields, translateOauth, translatePg } from "./fields.js";
import { loadList } from "./polling.js";
import { addTitle, groupFieldNode, lastGroup, rememberGroup, resolveDefaultGroup } from "./groups.js";
import { tr } from "./i18n.js";
import { closeSheet, openFieldSheet, sheet, showSheet } from "./ui/sheet.js";
import { newGroup } from "./sidebar.js";
import { addGroupTarget, setAddGroupTarget } from "./ui-state.js";
import { mcpGroups, selectedMcp, setSelectedMcp } from "./mcp-state.js";

/* --- Add sheet -------------------------------------------------------------------------------- */
/** `group` is the group the new MCP joins — the header + that opened this sheet. A null
 * `group` (the pane's empty-state button) resolves to the last group used in this scope
 * while it still exists. Either way the select is the truth: the title retitles with it, and
 * submit joins whatever it says, so a changed pick wins over the promise that opened it. */
function openSheet(group               )       {
  const names = mcpGroups() && mcpGroups().length ? mcpGroups() : [DEFAULT_GROUP];
  const initial = group || resolveDefaultGroup(names, lastGroup("mcps"));
  setAddGroupTarget(initial);
  const types = Object.keys(TYPE_FIELDS);
  // showSheet keeps the house order (panel-proof-of-life rule 1): visible BEFORE painted.
  showSheet(sheet({
    title: addTitle(tr("addSheet.mcp"), initial),
    titleId: "a-title",
    label: tr("addSheet.addMcp"),
    body: [
      h("div", { class: "two" },
        h("label", { class: "field" },
          h("span", null, tr("addSheet.name")),
          h("input", { id: "a-name", placeholder: tr("addSheet.gitMcp"), autocomplete: "off" })),
        h("label", { class: "field" },
          h("span", null, tr("addSheet.type")),
          h("select", { id: "a-type" }, types.map((t) => {
            return h("option", { value: t }, tr(TYPE_LABELS[t] || t));
          })))),
      groupFieldNode(names, initial),
      h("div", { id: "a-fields" }),
      h("label", { class: "check" }, h("input", { type: "checkbox", id: "a-start", checked: true }), tr("addSheet.startNow")),
      h("div", { class: "hint", id: "a-test-out", hidden: true })],
    foot: [
      h("button", { class: "btn", id: "a-import" }, tr("addSheet.importMcpJson")),
      h("input", { id: "a-file", type: "file", accept: ".json,application/json", hidden: true }),
      h("span", { class: "grow" }),
      h("button", { class: "btn", id: "a-cancel" }, tr("addSheet.cancel")),
      h("button", { class: "btn", id: "a-test", hidden: true }, tr("addSheet.testConnection")),
      h("button", { class: "btn primary", id: "a-save" }, tr("addSheet.add"))],
  }));
  const paint = ()       => {
    fill($("a-fields"), fieldsNode($                   ("a-type").value, {}, "a-"));
    // The test button exists only for the types that have something to test.
    const tb = $("a-test");
    if (tb) tb.hidden = !TESTABLE_TYPES.includes($                   ("a-type").value);
  };
  paint();
  $("a-type").onchange = paint;
  $("g-sel").onchange = () => {
    $("a-title").textContent = addTitle(tr("addSheet.mcp"), $                   ("g-sel").value);
  };
  $("a-cancel").onclick = closeSheet;
  $("a-test").onclick = () => { void runConnTest("a-"); };
  $("a-save").onclick = submitAdd;
  $("a-import").onclick = () => { $("a-file").click(); };
  $("a-file").onchange = () => { void submitImport($                  ("a-file")); };
  $("a-name").focus();
}

/** The group flavor of the one-field sheet — the same surface group create/rename already
 *  had, on the general mechanism: the title and button words by create/rename, the submit
 *  contract unchanged, and the required-name error now inline beside the field instead of
 *  a toast that dismisses the typing. */
function openGroupSheet(def               , submit                                              )       {
  openFieldSheet({
    title: def ? tr("addSheet.renameGroup") : tr("addSheet.newGroup"),
    def: def,
    submit: submit,
  });
}

async function submitImport(input                  )                {
  const f = input && input.files && input.files[0];
  if (!f) return;
  let text;
  try { text = await f.text(); } catch (e) { toast(tr("addSheet.couldReadFile"), true); return; }
  let json;
  try { json = JSON.parse(text); } catch (e) { toast(tr("addSheet.validJson"), true); return; }
  const j = await apiJson                          ("/api/mcpdefs/import", { method: "POST", body: JSON.stringify(json) });
  if (!j) return;
  closeSheet();
  const n = (j.imported || []).length;
  const s = (j.skipped || []).length;
  toast(s ? tr("addSheet.importedNSkippedS", { n, s }) : tr("addSheet.importedN", { n }));
  if (j.imported && j.imported[0]) setSelectedMcp(j.imported[0].name);
  await loadList();
  const landed = selectedMcp();
  if (landed) openDetail(landed);
}

async function submitAdd()                {
  const type = $                   ("a-type").value;
  /* The def body is dynamic by construction: readFields(type) emits the adapter type's own
   * fields (fields.ts TYPE_FIELDS: url / command / program / args / auth ...), then the two
   * translate passes rewrite more. Only the three shared keys are named; the rest is the
   * TYPE_FIELDS row set, which is why this is a Record and not an interface (docs/37 M9). */
  const body                                                                             = Object.assign({ name: $                  ("a-name").value.trim(), type: type, enabled: $                  ("a-start").checked }, readFields(type, "a-"));
  if (body.autostart !== undefined) { body.lazy = !body.autostart; delete body.autostart; }
  translateOauth(body); // the auth checkbox is the def's auth string (docs/24 D1)
  translatePg(type, body); // docs/30: the pg form's pieces become one url
  if (!body.name) { toast(tr("addSheet.nameRequired"), true); return; }
  if (type === "proc" && !body.command) { toast(tr("addSheet.commandRequired"), true); return; }
  // The select wins over the + that opened the sheet — a changed pick is the pick.
  if ($("g-sel")) setAddGroupTarget($                   ("g-sel").value);
  const j = await apiJson                        ("/api/mcps", { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  rememberGroup("mcps", addGroupTarget() );
  closeSheet();
  toast(tr("addSheet.addedNameState", { name: body.name, state: j.lifecycle || "stopped" }));
  setSelectedMcp(body.name);
  // Join the group whose + opened this sheet, BEFORE the list reload — so the row is drawn in its
  // group once, rather than hopping a moment later. `default` is a real group name now: joining it
  // is an explicit assignment like any other.
  if (addGroupTarget()) {
    await apiJson("/api/groups/mcps/members/" + encodeURIComponent(body.name),
      { method: "PUT", body: JSON.stringify({ group: addGroupTarget() }) });
  }
  await loadList();
  openDetail(body.name);
}
// The sidebar header's + makes a GROUP — that is the only thing that belongs to the sidebar as a
// whole. Adding an MCP belongs to a group, so it lives on each group's own + instead.
$("addBtn").onclick = newGroup;

export { openGroupSheet, openSheet, submitAdd, submitImport };
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

import type { ApiMcpDefsImportResponse } from "./types/api.js";
import { $, DEFAULT_GROUP, apiJson, esc, state, toast } from "./util.js";
import { openDetail, runConnTest } from "./detail.js";
import { TESTABLE_TYPES, TYPE_FIELDS, TYPE_LABELS, fieldsHtml, readFields, translateOauth, translatePg } from "./fields.js";
import { loadList } from "./polling.js";
import { addTitle, groupFieldHtml, lastGroup, rememberGroup, resolveDefaultGroup } from "./groups.js";
import { newGroup } from "./sidebar.js";

/* --- Add sheet -------------------------------------------------------------------------------- */
/** `group` is the group the new MCP joins — the header + that opened this sheet. A null
 * `group` (the pane's empty-state button) resolves to the last group used in this scope
 * while it still exists. Either way the select is the truth: the title retitles with it, and
 * submit joins whatever it says, so a changed pick wins over the promise that opened it. */
function openSheet(group: string | null): void {
  const names = state.groups && state.groups.length ? state.groups : [DEFAULT_GROUP];
  const initial = group || resolveDefaultGroup(names, lastGroup("mcps"));
  state.addGroup = initial;
  const types = Object.keys(TYPE_FIELDS);
  const opts = types.map((t) => { return '<option value="' + t + '">' + esc(TYPE_LABELS[t] || t) + "</option>"; }).join("");
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Add an MCP">' +
      '<div class="sheet-head"><h2 id="a-title">' + esc(addTitle("Add an", "MCP", initial)) + "</h2></div>" +
      '<div class="sheet-body">' +
        '<div class="two">' +
          '<label class="field"><span>Name</span><input id="a-name" placeholder="git-mcp" autocomplete="off"></label>' +
          '<label class="field"><span>Type</span><select id="a-type">' + opts + "</select></label>" +
        "</div>" +
        groupFieldHtml(names, initial) +
        '<div id="a-fields"></div>' +
        '<label class="check"><input type="checkbox" id="a-start" checked>Start it now</label>' +
        '<div class="hint" id="a-test-out" hidden></div>' +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="a-import">Import .mcp.json</button>' +
        '<input id="a-file" type="file" accept=".json,application/json" hidden>' +
        '<span class="grow"></span>' +
        '<button class="btn" id="a-cancel">Cancel</button>' +
        '<button class="btn" id="a-test" hidden>Test connection</button>' +
        '<button class="btn primary" id="a-save">Add</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  const paint = (): void => {
    $("a-fields").innerHTML = fieldsHtml($<HTMLSelectElement>("a-type").value, {}, "a-");
    // The test button exists only for the types that have something to test.
    const tb = $("a-test");
    if (tb) tb.hidden = TESTABLE_TYPES.indexOf($<HTMLSelectElement>("a-type").value) < 0;
  };
  paint();
  $("a-type").onchange = paint;
  $("g-sel").onchange = () => {
    $("a-title").textContent = addTitle("Add an", "MCP", $<HTMLSelectElement>("g-sel").value);
  };
  $("a-cancel").onclick = closeSheet;
  $("a-test").onclick = () => { void runConnTest("a-"); };
  $("a-save").onclick = submitAdd;
  $("a-import").onclick = () => { $("a-file").click(); };
  $("a-file").onchange = () => { void submitImport($<HTMLInputElement>("a-file")); };
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
  $("a-name").focus();
}
function closeSheet() { $("sheet").hidden = true; $("sheet").innerHTML = ""; }

/** A one-field sheet for group names, so creating a group is the same surface as creating
 *  everything else — not a browser prompt(). `def` is the current name when renaming, null when
 *  creating. `submit` gets the typed name and resolves true on success; the sheet closes on
 *  true and stays open on false, so a server refusal (a duplicate name) is readable next to
 *  what was typed instead of dismissing the work. */
function openGroupSheet(def: string | null, submit: (name: string) => Promise<boolean> | boolean): void {
  const editing = !!def;
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Rename group" : "New group") + '">' +
      '<div class="sheet-head"><h2>' + (editing ? "Rename group" : "New group") + "</h2></div>" +
      '<div class="sheet-body">' +
        '<label class="field"><span>Name</span><input id="g-name" value="' + esc(def || "") + '" placeholder="prod" autocomplete="off"></label>' +
      "</div>" +
      '<div class="sheet-foot"><span class="grow"></span>' +
        '<button class="btn" id="g-cancel">Cancel</button>' +
        '<button class="btn primary" id="g-save">' + (editing ? "Rename" : "Create") + "</button></div>" +
    "</div>";
  $("sheet").hidden = false;
  const save = async () => {
    const name = $<HTMLInputElement>("g-name").value.trim();
    if (!name) { toast("Name is required", true); return; }
    if (name === def) { closeSheet(); return; } // a rename that changed nothing is a cancel
    if (await submit(name)) closeSheet();
  };
  $("g-cancel").onclick = closeSheet;
  $("g-save").onclick = () => { void save(); };
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
  $("g-name").onkeydown = (ev) => { if (ev.key === "Enter") { ev.preventDefault(); void save(); } };
  $("g-name").focus();
  if (editing) $<HTMLInputElement>("g-name").select();
}

async function submitImport(input: HTMLInputElement): Promise<void> {
  const f = input && input.files && input.files[0];
  if (!f) return;
  let text;
  try { text = await f.text(); } catch (e) { toast("Could not read file", true); return; }
  let json;
  try { json = JSON.parse(text); } catch (e) { toast("Not valid JSON", true); return; }
  const j = await apiJson<ApiMcpDefsImportResponse>("/api/mcpdefs/import", { method: "POST", body: JSON.stringify(json) });
  if (!j) return;
  closeSheet();
  const n = (j.imported || []).length;
  const s = (j.skipped || []).length;
  toast("Imported " + n + (s ? ", skipped " + s : ""));
  if (j.imported && j.imported[0]) state.selected = j.imported[0].name;
  await loadList();
  if (state.selected) openDetail(state.selected);
}

async function submitAdd(): Promise<void> {
  const type = $<HTMLSelectElement>("a-type").value;
  /* the def body is open by construction: readFields(type) emits the type-specific
   * fields (url/command/program/args/auth...), then the two translate passes rewrite
   * more - only the three shared keys are spelled out (docs/37 M9 keep). */
  const body: { name: string; type: string; enabled: boolean; [key: string]: unknown } = Object.assign({ name: $<HTMLInputElement>("a-name").value.trim(), type: type, enabled: $<HTMLInputElement>("a-start").checked }, readFields(type, "a-"));
  if (body.autostart !== undefined) { body.lazy = !body.autostart; delete body.autostart; }
  translateOauth(body); // the auth checkbox is the def's auth string (docs/24 D1)
  translatePg(type, body); // docs/30: the pg form's pieces become one url
  if (!body.name) { toast("Name is required", true); return; }
  if (type === "proc" && !body.command) { toast("Command is required", true); return; }
  // The select wins over the + that opened the sheet — a changed pick is the pick.
  if ($("g-sel")) state.addGroup = $<HTMLSelectElement>("g-sel").value;
  const j = await apiJson<{ lifecycle?: string }>("/api/mcps", { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  rememberGroup("mcps", state.addGroup!);
  closeSheet();
  toast("Added " + body.name + " (" + (j.lifecycle || "stopped") + ")");
  state.selected = body.name;
  // Join the group whose + opened this sheet, BEFORE the list reload — so the row is drawn in its
  // group once, rather than hopping a moment later. `default` is a real group name now: joining it
  // is an explicit assignment like any other.
  if (state.addGroup) {
    await apiJson("/api/groups/mcps/members/" + encodeURIComponent(body.name),
      { method: "PUT", body: JSON.stringify({ group: state.addGroup }) });
  }
  await loadList();
  openDetail(body.name);
}
// The sidebar header's + makes a GROUP — that is the only thing that belongs to the sidebar as a
// whole. Adding an MCP belongs to a group, so it lives on each group's own + instead.
$("addBtn").onclick = newGroup;

export { closeSheet, openGroupSheet, openSheet, submitAdd, submitImport };

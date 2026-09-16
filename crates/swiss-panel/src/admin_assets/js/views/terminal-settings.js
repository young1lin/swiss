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
   Terminal settings - the Local shell settings sheet, split out of views/terminal.js
   (docs/16 §6) so that file carries the xterm wiring and the session machine only.

   The dependency direction mirrors the js/tunnels.js + js/tunnel-sheets.js split: the
   page imports this module's opener alone, and this module reads the page's live state
   (targets, sessions) and calls its reload() after a save. The cycle is safe because
   nothing here runs before a click, long after both modules have evaluated.
   ================================================================================================ */
import { $, apiJson, esc, toast } from "../util.js";
import { configPutBody, targetRows } from "../terminal-core.js";
import { closeSheet } from "../add-sheet.js";
import { reload, sessions, targets } from "./terminal.js";

/* The Local shell settings sheet (docs/15 §2): the switch docs/14 §6.1 asks for and
   the shell picker. Saving is a plugin-config PUT — the terminal plugin restarts on
   config change, so every open session closes with a reason; the confirm names how many,
   and the revision from the GET makes a save that raced another panel lose loudly (409)
   instead of silently overwriting it. */
export async function openLocalSheet() {
  var got = await apiJson("/api/plugins/terminal/config");
  if (!got) return;   // the toast already said why
  var local = (got.config && got.config.local) || {};
  var l = (targets && targets.local) || {};
  var shells = Array.isArray(l.shells) ? l.shells : [];
  /* The switch's reason line is the schema's own description: one source, no fork — the
     sentence next to the checkbox can never drift from the one the backend enforces. */
  var whyOff = "";
  try { whyOff = got.schema.properties.local.properties.enabled.description || ""; } catch (e) { /* an older schema: no line */ }
  var options = shells.map(function (s) {
    return '<option value="' + esc(s.program) + '">' + esc(s.label) + " · " + esc(s.program) + "</option>";
  }).join("");
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Local shell settings">' +
      '<div class="sheet-head"><h2>Local shell</h2></div>' +
      '<div class="sheet-body">' +
        '<div class="fld"><label class="check"><input type="checkbox" id="ls-enabled"' + (local.enabled ? " checked" : "") + ">Enabled</label>" +
          (whyOff ? '<div class="hint">' + esc(whyOff) + "</div>" : "") + "</div>" +
        /* A datalist, not a select: the candidates are suggestions, and any path the
           gateway can spawn is legal (docs/15 §2.1 — "may be typed by hand"). */
        '<label class="field"><span>Shell</span>' +
          '<input id="ls-shell" list="ls-shells" value="' + esc(local.shell || "") + '" placeholder="' + esc(l.shell || "the platform default") + '" autocomplete="off" spellcheck="false">' +
          '<datalist id="ls-shells">' + options + "</datalist></label>" +
        '<div class="hint">Empty = the platform default (' + esc(l.shell || "?") + "). Saving restarts the terminal plugin and closes every open session.</div>" +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="ls-cancel">Cancel</button>' +
        '<button class="btn primary" id="ls-save">Save</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  $("ls-cancel").onclick = closeSheet;
  $("ls-save").onclick = function () { void saveLocalSheet(got); };
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeSheet(); };
  $("ls-enabled").focus();
}

async function saveLocalSheet(got) {
  var enabled = $("ls-enabled").checked;
  var shell = $("ls-shell").value;
  /* restart_on_config_change is the honest cost of this save (docs/15 §2.1): the
     plugin restarts, and with it every session — say how many and let the user back out. */
  if (sessions.length) {
    var n = sessions.length;
    if (!window.confirm("Saving restarts the terminal plugin and closes " + n + " session" + (n === 1 ? "" : "s") + ".")) return;
  }
  var saved = await apiJson("/api/plugins/terminal/config", {
    method: "PUT",
    body: JSON.stringify(configPutBody(got.config, got.revision, enabled, shell)),
  });
  if (!saved) return;
  closeSheet();
  toast("Saved — the terminal plugin restarted with the new local shell settings");
  await reload();
  /* The row the user just switched on is the one they mean to open next; select it
     explicitly instead of trusting the rows' order to put it first forever. */
  var pick = $("term-target");
  if (pick && targetRows(targets).rows.some(function (r) { return r.id === "local"; })) pick.value = "local";
}

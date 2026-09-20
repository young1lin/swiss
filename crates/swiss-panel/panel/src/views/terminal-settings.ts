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
import type { TerminalLocalCfg, TerminalPluginConfigResponse } from "../types/terminal-view.js";
import { $, apiJson, toast } from "../util.js";
import { fill, h } from "../h.js";
import { configPutBody, targetRows } from "../terminal-core.js";
import { closeSheet } from "../add-sheet.js";
import { reload, sessions, targets } from "./terminal.js";
import { tr, trn } from "../i18n.js";

/* The Local shell settings sheet (docs/15 §2): the switch docs/14 §6.1 asks for and
   the shell picker. Saving is a plugin-config PUT — the terminal plugin restarts on
   config change, so every open session closes with a reason; the confirm names how many,
   and the revision from the GET makes a save that raced another panel lose loudly (409)
   instead of silently overwriting it. */
export async function openLocalSheet() {
  const got = await apiJson<TerminalPluginConfigResponse>("/api/plugins/terminal/config");
  if (!got) return;   // the toast already said why
  const local = (got.config && got.config.local) || {} as TerminalLocalCfg;
  const l = (targets && targets.local) || {} as TerminalLocalCfg;
  const shells = Array.isArray(l.shells) ? l.shells : [];
  /* The switch's reason line is the schema's own description: one source, no fork — the
     sentence next to the checkbox can never drift from the one the backend enforces. */
  let whyOff = "";
  try { whyOff = got.schema?.properties?.local.properties?.enabled.description || ""; } catch (e) { /* an older schema: no line */ }
  const options = shells.map((s) => {
    return h("option", { value: s.program }, s.label + " · " + s.program);
  });
  // Visible before the paint (panel-proof-of-life rule 1).
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label: tr("terminalSettings.localShellSettings") } },
      h("div", { class: "sheet-head" }, h("h2", null, tr("terminalSettings.localShell"))),
      h("div", { class: "sheet-body" },
        h("div", { class: "fld" },
          h("label", { class: "check" }, h("input", { type: "checkbox", id: "ls-enabled", checked: !!local.enabled }), tr("terminalSettings.enabled")),
          whyOff ? h("div", { class: "hint" }, whyOff) : null),
        /* A datalist, not a select: the candidates are suggestions, and any path the
           gateway can spawn is legal (docs/15 §2.1 — "may be typed by hand"). */
        h("label", { class: "field" },
          h("span", null, tr("terminalSettings.shell")),
          // `list` is a read-only input property, so it rides as an attribute post-build.
          (() => {
            const el = h("input", { id: "ls-shell", value: local.shell || "",
              placeholder: l.shell || tr("terminalSettings.platformDefault"), autocomplete: "off", spellcheck: false });
            el.setAttribute("list", "ls-shells");
            return el;
          })(),
          h("datalist", { id: "ls-shells" }, options)),
        h("div", { class: "hint" },
          tr("terminalSettings.emptyPlatformDefaultWhich", { which: l.shell || "?" }))),
      h("div", { class: "sheet-foot" },
        h("button", { class: "btn", id: "ls-cancel" }, tr("terminalSettings.cancel")),
        h("button", { class: "btn primary", id: "ls-save" }, tr("terminalSettings.save")))));
  $("ls-cancel").onclick = closeSheet;
  $("ls-save").onclick = () => { void saveLocalSheet(got!); };
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
  $("ls-enabled").focus();
}

async function saveLocalSheet(got: TerminalPluginConfigResponse) {
  const enabled = $<HTMLInputElement>("ls-enabled").checked;
  const shell = $<HTMLInputElement>("ls-shell").value;
  /* restart_on_config_change is the honest cost of this save (docs/15 §2.1): the
     plugin restarts, and with it every session — say how many and let the user back out. */
  if (sessions.length) {
    const n = sessions.length;
    if (!window.confirm(tr("terminalSettings.savingRestartsTerminalPlugin", { n: trn(n, "terminalSettings.nSessions.one", "terminalSettings.nSessions.other") }))) return;
  }
  const saved = await apiJson("/api/plugins/terminal/config", {
    method: "PUT",
    body: JSON.stringify(configPutBody(got.config, got.revision, enabled, shell)),
  });
  if (!saved) return;
  closeSheet();
  toast(tr("terminalSettings.savedTerminalPluginRestarted"));
  await reload();
  /* The row the user just switched on is the one they mean to open next; select it
     explicitly instead of trusting the rows' order to put it first forever. */
  const pick = $<HTMLSelectElement>("term-target");
  if (pick && targetRows(targets).rows.some((r) => { return r.id === "local"; })) pick.value = "local";
}

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

import { $, apiJson, emptyHtml, state } from "../util.js";
import { closeSheet } from "../add-sheet.js";

/** The process action is deliberately a Settings page, not permanent app chrome: quitting the
 *  whole toolbox is destructive, rare, and belongs beside other host-owned controls. */
function systemBody() {
  return '<div class="wide">' +
    '<div class="pane-head"><div>' +
      '<div class="pane-desc">Control this running swiss process. Quitting stops every plugin and local service cleanly; configuration and logs remain on disk.</div>' +
    "</div></div>" +
    '<div class="sec-head"><span class="sec-cap">Runtime</span></div>' +
    '<div class="group">' +
      '<div class="tun-row">' +
        '<div class="tun-main">' +
          '<div class="tun-name">Quit swiss</div>' +
          '<div class="tun-sub"><span class="via">Gracefully stop MCPs, tunnels, jobs, terminals, and this local process.</span></div>' +
        "</div>" +
        '<div class="tun-acts"><button class="btn danger" id="system-quit">Quit swiss</button></div>' +
      "</div>" +
    "</div>" +
  "</div>";
}

function quitSheetHtml() {
  return '<div class="sheet" role="dialog" aria-modal="true" aria-label="Quit swiss">' +
    '<div class="sheet-head"><h2>Quit swiss?</h2></div>' +
    '<div class="sheet-body">' +
      '<p>This disconnects every MCP client and stops active tunnels, jobs, and terminal sessions.</p>' +
      '<p class="hint">Your configuration and logs are kept. Start it again with <code>swiss start</code>.</p>' +
    "</div>" +
    '<div class="sheet-foot"><span class="grow"></span>' +
      '<button class="btn" id="quit-cancel">Cancel</button>' +
      '<button class="btn danger" id="quit-confirm">Quit swiss</button>' +
    "</div>" +
  "</div>";
}

function openQuitSheet() {
  var sheet = $("sheet");
  sheet.hidden = false;
  sheet.innerHTML = quitSheetHtml();
  $("quit-cancel").onclick = closeSheet;
  $("quit-confirm").onclick = function () { void requestQuit(); };
  sheet.onclick = function (event) { if (event.target === sheet) closeSheet(); };
  $("quit-cancel").focus();
}

async function requestQuit() {
  var button = $<HTMLButtonElement>("quit-confirm");
  if (button) { button.disabled = true; button.textContent = "Quitting…"; }
  var stopped = await apiJson("/api/shutdown", { method: "POST" });
  if (!stopped) {
    if (button) { button.disabled = false; button.textContent = "Quit swiss"; }
    return;
  }
  closeSheet();
  // A slow response must not overwrite a different page reached while the sheet was open.
  if (state.view === "system") {
    $("pane").innerHTML = emptyHtml({
      icon: "power",
      title: "swiss is stopping",
      hint: "The local process is closing gracefully. You can close this tab and run swiss start when you need it again.",
    });
  }
}

async function mount() {
  $("pane").innerHTML = systemBody();
  $("system-quit").onclick = openQuitSheet;
}

export { mount, openQuitSheet, quitSheetHtml, requestQuit, systemBody };

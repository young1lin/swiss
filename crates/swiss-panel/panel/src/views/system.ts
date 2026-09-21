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

import { $, apiJson, emptyNode, targetEl } from "../util.js";
import { fill, h } from "../h.js";
import { closeSheet } from "../add-sheet.js";
import { currentView } from "../ui-state.js";
import { tr } from "../i18n.js";

/** The process action is deliberately a Settings page, not permanent app chrome: quitting the
 *  whole toolbox is destructive, rare, and belongs beside other host-owned controls.
 *
 *  Built with h() (docs/37 R5): every string here is the panel's own prose, so the security
 *  face that moved the other views off innerHTML does not exist on this page - what the
 *  builders buy here is the ledger (the file joins the node side of the eventual ratchet)
 *  and a pane whose wiring survives its own repaint. */
function systemBodyNode(): HTMLElement {
  return h("div", { class: "wide" },
    h("div", { class: "pane-head" },
      h("div", null,
        h("div", { class: "pane-desc" }, tr("system.controlRunningSwissProcess")))),
    h("div", { class: "sec-head" }, h("span", { class: "sec-cap" }, tr("system.runtime"))),
    h("div", { class: "group" },
      h("div", { class: "tun-row" },
        h("div", { class: "tun-main" },
          h("div", { class: "tun-name" }, tr("system.quitSwiss")),
          h("div", { class: "tun-sub" },
            h("span", { class: "via" }, tr("system.gracefullyStopMcpsTunnels")))),
        h("div", { class: "tun-acts" },
          h("button", { class: "btn danger", id: "system-quit" }, tr("system.quitSwiss"))))));
}

function quitSheetNode(): HTMLElement {
  return h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label: tr("system.quitSwiss") } },
    h("div", { class: "sheet-head" }, h("h2", null, tr("system.quitSwiss2"))),
    h("div", { class: "sheet-body" },
      h("p", null, tr("system.disconnectsEveryMcpClient")),
      h("p", { class: "hint" },
        tr("system.configurationLogsKeptStart"),
        h("code", null, "swiss start"), ".")),
    h("div", { class: "sheet-foot" },
      h("span", { class: "grow" }),
      h("button", { class: "btn", id: "quit-cancel" }, tr("system.cancel")),
      h("button", { class: "btn danger", id: "quit-confirm" }, tr("system.quitSwiss"))));
}

function openQuitSheet(): void {
  const sheet = $("sheet");
  sheet.hidden = false; // BEFORE the content, per the house sheet idiom (add-sheet.js)
  fill(sheet, quitSheetNode());
  // Per-open wiring IS the house sheet idiom (add-sheet.js, the group sheet): #sheet is a
  // shared shell host that outlives this view, so its controls are claimed here and only
  // here - not delegated from the pane, which does not own the sheet.
  $("quit-cancel").onclick = closeSheet;
  $("quit-confirm").onclick = () => { void requestQuit(); };
  sheet.onclick = (event) => { if (event.target === sheet) closeSheet(); };
  $("quit-cancel").focus();
}

async function requestQuit(): Promise<void> {
  const button = $<HTMLButtonElement>("quit-confirm");
  if (button) { button.disabled = true; button.textContent = tr("system.quitting"); }
  const stopped = await apiJson("/api/shutdown", { method: "POST" });
  if (!stopped) {
    if (button) { button.disabled = false; button.textContent = tr("system.quitSwiss"); }
    return;
  }
  closeSheet();
  // A slow response must not overwrite a different page reached while the sheet was open.
  if (currentView() === "system") {
    fill($("pane"), emptyNode({
      icon: "power",
      title: tr("system.swissStopping"),
      hint: tr("system.localProcessClosingGracefully"),
    }));
  }
}

async function mount(): Promise<void> {
  fill($("pane"), systemBodyNode());
  // One delegated claim on the pane (docs/37 R5): the quit button is reached through the
  // pane's own listener, so the wiring survives any repaint of the pane's children.
  $("pane").onclick = (event: MouseEvent): void => {
    if (targetEl(event)?.closest("#system-quit")) openQuitSheet();
  };
}

export { mount, openQuitSheet, quitSheetNode, requestQuit, systemBodyNode };
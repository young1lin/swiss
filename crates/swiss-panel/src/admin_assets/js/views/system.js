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

import { $, apiJson, targetEl } from "../util.js";
import { fill, h } from "../h.js";
import { currentView } from "../ui-state.js";
import { tr } from "../i18n.js";
import { btn, card, closeSheet, emptyNode, hint, paneBody, paneHead, row, section, sheet, showSheet } from "../ui/index.js";

/** The process action is deliberately a Settings page, not permanent app chrome: quitting the
 *  whole toolbox is destructive, rare, and belongs beside other host-owned controls.
 *
 *  Built with h() (docs/37 R5): every string here is the panel's own prose, so the security
 *  face that moved the other views off innerHTML does not exist on this page - what the
 *  builders buy here is the ledger (the file joins the node side of the eventual ratchet)
 *  and a pane whose wiring survives its own repaint.
 *
 *  On the library since docs/46 P5: paneHead, section + card, and the one row(). The Quit
 *  button keeps its red on the row - the prototype's call, and the exception rule 4 allows
 *  for a page whose ONE act is the destructive one: there is no overflow to hide it in, and
 *  the sheet it opens is the confirmation. */
function systemBodyNode()              {
  return paneBody({ wide: true },
    paneHead({ desc: tr("system.descOneLine") }),
    section({ cap: tr("system.runtime") },
      card(row({
        name: tr("system.quitSwiss"),
        sub: tr("system.gracefullyStopMcpsTunnels"),
        primary: btn(tr("system.quitSwiss"), { kind: "danger", id: "system-quit" }),
      }))));
}

function quitSheetNode()              {
  return sheet({
    title: tr("system.quitSwiss2"),
    label: tr("system.quitSwiss"),
    body: [
      h("p", null, tr("system.disconnectsEveryMcpClient")),
      hint([tr("system.configurationLogsKeptStart"), h("code", null, "swiss start"), tr("system.startAgainTail")]),
    ],
    foot: [
      btn(tr("system.cancel"), { id: "quit-cancel" }),
      btn(tr("system.quitSwiss"), { kind: "danger", id: "quit-confirm" }),
    ],
  });
}

function openQuitSheet()       {
  // showSheet unhides the host BEFORE it paints (the house sheet idiom) and claims the
  // backdrop click. Per-open wiring of the two buttons IS the idiom (ui/sheet.ts, the
  // one-field sheet): #sheet is a shared shell host that outlives this view, so its controls
  // are claimed here and only here - not delegated from the pane, which does not own the sheet.
  showSheet(quitSheetNode());
  $("quit-cancel").onclick = closeSheet;
  $("quit-confirm").onclick = () => { void requestQuit(); };
  $("quit-cancel").focus();
}

async function requestQuit()                {
  const button = $                   ("quit-confirm");
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

async function mount()                {
  fill($("pane"), systemBodyNode());
  // One delegated claim on the pane (docs/37 R5): the quit button is reached through the
  // pane's own listener, so the wiring survives any repaint of the pane's children.
  $("pane").onclick = (event            )       => {
    if (targetEl(event)?.closest("#system-quit")) openQuitSheet();
  };
}

export { mount, openQuitSheet, quitSheetNode, requestQuit, systemBodyNode };
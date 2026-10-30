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

import { $, api, apiJson, refocusIfIdle, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
import { currentView } from "../ui-state.js";
import { tr } from "../i18n.js";
import { btn, card, closeSheet, emptyNode, hint, note, paneBody, paneHead, row, section, sheet, showSheet, sw, tag } from "../ui/index.js";
                                                                                                                             // Admin API contracts.
const machine                                            = { autostart: null, "user-path": null };
const busy = new Set            ();
let skills                          = null;
let generation = 0;

export function startupRowNode(a                       )                     {
  return a ? row({
    name: tr("plugins.startSwissWhenYou"), sub: a.detail || null,
    toggle: sw(a.enabled, tr("plugins.toggleStartSign"), { data: { "autostart-toggle": true, "machine-toggle": "autostart" }, disabled: busy.has("autostart") }),
    data: { autostart: true }, title: a.command || undefined,
  }) : null;
}

export function pathRowNode(p                       )                     {
  return p ? row({
    name: tr("plugins.swissOnYourPath"), sub: p.detail || null,
    toggle: sw(p.enabled, tr("plugins.togglePath"), { data: { "userpath-toggle": true, "machine-toggle": "user-path" }, disabled: busy.has("user-path") }),
    data: { userpath: true }, title: p.dir || undefined,
  }) : null;
}

function skillsSection()              {
  const body = skills ? card(skills.map(skill => row({
    name: skill.name, data: { skill: skill.name },
    cols: skill.locations.map(location => {
      const state = tr(location.installed === true ? "system.skillInstalled" : location.installed === false ? "system.skillMissing" : "system.skillUnknown");
      return { w: "l"         , v: h("span", { data: { "skill-root": location.root }, title: location.path + (location.error ? " · " + location.error : "") },
        h("code", null, location.root), " ", tag(state, { tone: location.installed === null ? "warn" : undefined })) };
    }),
  }))) : note(tr("system.skillsUnavailable"));
  const part = section({ cap: tr("system.skills"), note: tr("system.skillsHint"), tools: [btn(tr("system.refreshSkills"), { id: "system-skills-refresh" })] },
    body, note([tr("system.skillsInstallHint"), " ", h("code", null, "swiss skill install")]));
  part.id = "system-skills";
  return part;
}

async function readStatus   (path        )                    {
  try { const r = await api(path); return r.ok ? await r.json()      : null; }
  catch { return null; }
}

function patchMachine()       {
  for (const key of ["autostart", "user-path"]         ) {
    const toggle = document.querySelector                   ('[data-machine-toggle="' + key + '"]');
    if (toggle) {
      toggle.disabled = busy.has(key);
      toggle.setAttribute("aria-checked", String(!!machine[key]?.enabled));
    }
  }
}

async function toggleMachine(key            )                {
  const current = machine[key];
  if (!current || busy.has(key)) return;
  const ticket = generation;
  const pressed = document.activeElement;
  busy.add(key); patchMachine();
  const next = await apiJson                ("/api/" + key, { method: "PUT", body: JSON.stringify({ enabled: !current.enabled }) });
  if (ticket !== generation || currentView() !== "system") return;
  busy.delete(key);
  if (next) { machine[key] = next; if (key === "user-path") toast(tr("plugins.pathNewTerminals")); }
  patchMachine(); refocusIfIdle(pressed);
}

async function refreshSkills()                {
  const ticket = generation;
  const button = $                   ("system-skills-refresh");
  if (button) button.disabled = true;
  const next = await readStatus                              ("/api/skills");
  if (ticket !== generation || currentView() !== "system") return;
  skills = next?.skills || null;
  document.getElementById("system-skills")?.replaceWith(skillsSection());
}

/** The process action is deliberately a Settings page, not permanent app chrome: quitting the
 *  whole toolbox is destructive, rare, and belongs beside other host-owned controls.
 *
 *  Built with h() (SPEC §panel.toolchain): every string here is the panel's own prose, so the security
 *  face that moved the other views off innerHTML does not exist on this page - what the
 *  builders buy here is the ledger (the file joins the node side of the eventual ratchet)
 *  and a pane whose wiring survives its own repaint.
 *
 *  On the library since SPEC §panel.settings: paneHead, section + card, and the one row(). The Quit
 *  button keeps its red on the row - the prototype's call, and the exception rule 4 allows
 *  for a page whose ONE act is the destructive one: there is no overflow to hide it in, and
 *  the sheet it opens is the confirmation. */
function systemBodyNode()              {
  const controls = [startupRowNode(machine.autostart), pathRowNode(machine["user-path"])].filter((node)                      => node !== null);
  return paneBody({ wide: true },
    paneHead({ desc: tr("system.descOneLine") }),
    controls.length ? section({ cap: tr("plugins.thisMachine") }, card(controls)) : null,
    skillsSection(),
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
  const ticket = ++generation;
  busy.clear();
  const [startup, path, checked] = await Promise.all([
    readStatus                ("/api/autostart"), readStatus                ("/api/user-path"),
    readStatus                              ("/api/skills"),
  ]);
  if (ticket !== generation || currentView() !== "system") return;
  machine.autostart = startup; machine["user-path"] = path; skills = checked?.skills || null;
  fill($("pane"), systemBodyNode());
  // One delegated claim on the pane (SPEC §panel.toolchain): the quit button is reached through the
  // pane's own listener, so the wiring survives any repaint of the pane's children.
  $("pane").onclick = (event            )       => {
    if (targetEl(event)?.closest("#system-quit")) openQuitSheet();
    if (targetEl(event)?.closest("#system-skills-refresh")) { void refreshSkills(); return; }
    const key = targetEl(event)?.closest("[data-machine-toggle]")?.getAttribute("data-machine-toggle");
    if (key === "autostart" || key === "user-path") void toggleMachine(key);
  };
}

function unmount()       { generation++; busy.clear(); machine.autostart = null; machine["user-path"] = null; skills = null; }

export { mount, mount as refresh, unmount, openQuitSheet, quitSheetNode, requestQuit, systemBodyNode };

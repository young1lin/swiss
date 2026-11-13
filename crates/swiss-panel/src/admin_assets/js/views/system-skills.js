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

// One table per skill seat (SPEC §panel.settings). Nothing here installs or refreshes on mount:
// the page paints what GET /api/skills already returned, and every write is a click.

import { $, api, apiJson, toast } from "../util.js";
import { h } from "../h.js";
import { currentView } from "../ui-state.js";
import { servedError, tr, trn } from "../i18n.js";
import { btn, card, closeSheet, iconBtn, note, row, section, sheet, showSheet, tag } from "../ui/index.js";
import { actionBtn } from "../ui/action.js";
import { openFieldSheet } from "../ui/sheet.js";
                                                                                                         

let skills                          = null;
let seats                        = null;
let ledger                                         = null;
const skillBusy = new Set        ();
let epoch = 0;

export function resetSkills()       {
  epoch++;
  skillBusy.clear();
  skills = null;
  seats = null;
  ledger = null;
}

export function applySkills(body                          )       {
  if (!body || !body.skills) {
    skills = null;
    seats = null;
    ledger = null;
    return;
  }
  skills = body.skills;
  seats = body.seats && body.seats.length ? body.seats : null;
  ledger = body.ledger ?? null;
}

/** A ledger the gateway could not parse hides its seats; say so, and keep the reason in the title. */
function ledgerNote()                     {
  if (!ledger) return null;
  const line = note(tr("system.skillsLedgerBroken", { path: ledger.path }), { err: true, data: { "skills-ledger": "broken" } });
  line.title = ledger.error;
  return line;
}

/** POST or DELETE a skills route. A refusal is the served code's copy (SPEC §panel.i18n). */
async function skillsWrite(path        , method        , payload         )                                    {
  try {
    const r = await api(path, { method, body: JSON.stringify(payload) });
    const body          = await r.json().catch(()          => { return {}; });
    if (!r.ok) {
      toast(servedError(body, tr("util.httpN", { n: r.status })), true);
      return null;
    }
    return body                     ;
  } catch {
    toast(tr("util.requestFailedGatewayRunning"), true);
    return null;
  }
}

function sameDir(a        , b        )          {
  const fold = (s        )         => s.replace(/[\\/]+$/, "");
  const left = fold(a);
  const right = fold(b);
  if (left === right) return true;
  const windows = left.includes("\\") || right.includes("\\") || left.includes(":") || right.includes(":");
  return windows && left.toLowerCase() === right.toLowerCase();
}

function knownSeats()                 {
  if (seats && seats.length) return seats;
  const seen                 = [];
  for (const skill of skills || []) {
    for (const loc of skill.locations) {
      const dir = loc.dir || loc.root;
      if (seen.some(seat => sameDir(seat.dir, dir) && seat.kind === (loc.kind || (loc.root === ".agents" ? "agents" : "claude")))) continue;
      seen.push({
        kind: loc.kind || (loc.root === ".agents" ? "agents" : "claude"),
        dir,
        source: loc.source || "env",
        exists: loc.state !== "missing",
      });
    }
  }
  return seen;
}

function locationOf(skill                , seat              )                               {
  return skill.locations.find(loc => (loc.dir ? sameDir(loc.dir, seat.dir) : loc.root === (seat.kind === "agents" ? ".agents" : ".claude")));
}

function outdatedCount()         {
  let n = 0;
  for (const skill of skills || []) for (const loc of skill.locations) if (loc.state === "outdated") n++;
  return n;
}

function repaint()       {
  document.getElementById("system-skills")?.replaceWith(skillsSection());
}

function stateLabel(loc                  )         {
  if (loc.state === "outdated") return tr("system.skillOutdated");
  if (loc.state === "missing") return tr("system.skillFolderMissing");
  if (loc.state === "unreadable" || loc.installed === null) return tr("system.skillUnknown");
  if (loc.state === "current" || loc.installed === true) return tr("system.skillInstalled");
  return tr("system.skillMissing");
}

function seatBlock(seat              )              {
  const kind = tr(seat.kind === "agents" ? "system.skillSeatAgents" : "system.skillSeatClaude");
  const source = tr(seat.source === "ledger" ? "system.skillSeatLedger" : seat.source === "both" ? "system.skillSeatBoth" : "system.skillSeatEnv");
  const missing = seat.source === "ledger" && seat.exists === false;
  const forget = seat.source === "ledger"
    ? btn(tr("system.skillsForget"), { kind: "ghost", data: { "skill-forget": seat.dir } })
    : undefined;
  if (missing || !skills) {
    return card(row({
      name: kind,
      sub: h("span", null, h("code", null, seat.dir), " · ", source),
      muted: true,
      cols: [{ v: tag(tr("system.skillFolderMissing")) }],
      primary: forget,
      data: { "skill-seat": seat.dir, "skill-missing": "1" },
    }));
  }
  const rows = skills.map(skill => {
    const loc = locationOf(skill, seat);
    if (!loc || loc.state === "missing") {
      return row({
        name: skill.name,
        sub: tr("system.skillFolderMissing"),
        muted: true,
        data: { skill: skill.name, "skill-root": loc?.root || "" },
      });
    }
    const present = loc.installed === true || loc.state === "current" || loc.state === "outdated";
    const key = skill.name + ":" + (loc.dir || loc.root);
    const label = tr(present ? "system.updateSkill" : "system.installSkill", { name: skill.name, root: loc.root });
    const cell = h("span", {
      data: { "skill-root": loc.root },
      title: loc.path + (loc.error ? " · " + loc.error : ""),
    }, tag(stateLabel(loc), { tone: loc.installed === null || loc.state === "outdated" ? "warn" : undefined }), " ",
      iconBtn("download", label, {
        ghost: true,
        disabled: skillBusy.has(key),
        data: {
          "skill-install": skill.name,
          "skill-directory": loc.root,
          ...(loc.source === "ledger" && loc.dir ? { "skill-dir": loc.dir } : {}),
        },
      }));
    return row({ name: skill.name, data: { skill: skill.name }, cols: [{ v: cell }] });
  });
  return card(
    row({
      name: kind,
      sub: h("span", null, h("code", null, seat.dir), " · ", source),
      primary: forget,
      data: { "skill-seat": seat.dir },
    }),
    ...rows,
  );
}

export function skillsSection()              {
  const outdated = outdatedCount();
  const tools = [
    outdated ? tag(tr("system.skillsOutdated", { n: outdated }), { tone: "warn" }) : null,
    outdated ? btn(tr("system.skillsUpdateAll"), { id: "system-skills-update", disabled: skillBusy.size > 0 }) : null,
    btn(tr("system.skillsAdd"), { id: "system-skills-add", kind: "ghost" }),
    actionBtn("refresh", tr("system.refreshSkills"), {
      id: "system-skills-refresh",
      title: tr("system.refreshSkillsHint"),
      disabled: skillBusy.size > 0,
    }),
  ];
  const body = skills
    ? knownSeats().map(seatBlock)
    : note(tr("system.skillsUnavailable"));
  const part = section({
    cap: tr("system.skills"),
    note: tr("system.skillsHint"),
    tools,
  }, ledgerNote(), ...(Array.isArray(body) ? body : [body]), note(tr("system.skillsActionHint")));
  part.id = "system-skills";
  return part;
}

async function refreshSkills()                {
  if (skillBusy.size) return;
  const ticket = epoch;
  const button = $                   ("system-skills-refresh");
  if (button) button.disabled = true;
  try {
    const r = await api("/api/skills");
    const body = r.ok ? await r.json()                      : null;
    if (ticket !== epoch || currentView() !== "system") return;
    applySkills(body);
  } catch {
    if (ticket !== epoch || currentView() !== "system") return;
    applySkills(null);
  }
  repaint();
}

async function installSkill(name        , root        , dir        )                {
  const key = name + ":" + (dir || root);
  if (!name || skillBusy.has(key)) return;
  const ticket = epoch;
  skillBusy.add(key);
  repaint();
  const payload = dir ? { name, dir } : { name, root };
  const result = await skillsWrite("/api/skills/install", "POST", payload);
  if (ticket !== epoch || currentView() !== "system") return;
  skillBusy.delete(key);
  const location = result?.skills?.find(skill => skill.name === name)?.locations.find(item => dir ? sameDir(item.dir || "", dir) : item.root === root);
  const current = skills?.find(skill => skill.name === name)?.locations.find(item => dir ? sameDir(item.dir || "", dir) : item.root === root);
  if (location && current) Object.assign(current, location);
  if (result?.seats) seats = result.seats;
  if (result) ledger = result.ledger ?? null;
  repaint();
}

async function updateAll()                {
  if (skillBusy.size) return;
  const ticket = epoch;
  skillBusy.add("*");
  repaint();
  const result = await apiJson                   ("/api/skills/refresh", { method: "POST", body: "{}" });
  if (ticket !== epoch || currentView() !== "system") return;
  skillBusy.delete("*");
  if (result) {
    applySkills(result);
    const failed = result.failed || [];
    if (failed.length) toast(failed.join("\n"), true);
    else toast(trn((result.replaced || []).length, "system.skillsUpdated.one", "system.skillsUpdated.other"));
  }
  repaint();
}

function openAdd()       {
  openFieldSheet({
    title: tr("system.skillsAddTitle"),
    label: tr("system.skillsAddLabel"),
    save: tr("system.skillsAddSave"),
    submit: async (dir        )                            => {
      const ticket = epoch;
      try {
        const r = await api("/api/skills/seats", { method: "POST", body: JSON.stringify({ kind: "claude", dir }) });
        const body          = await r.json().catch(()          => { return {}; });
        if (!r.ok) return servedError(body, tr("served.skillsSeatInvalid"));
        if (ticket !== epoch || currentView() !== "system") return true;
        applySkills(body                     );
        repaint();
        return true;
      } catch {
        return tr("util.requestFailedGatewayRunning");
      }
    },
  });
}

function openForget(dir        )       {
  if (!dir) return;
  showSheet(sheet({
    title: tr("system.skillsForgetTitle"),
    label: tr("system.skillsForget"),
    body: [h("p", null, tr("system.skillsForgetBody")), h("p", null, h("code", null, dir))],
    foot: [
      btn(tr("system.cancel"), { id: "skill-forget-cancel" }),
      btn(tr("system.skillsForget"), { kind: "danger", id: "skill-forget-confirm" }),
    ],
  }));
  $("skill-forget-cancel").onclick = closeSheet;
  $("skill-forget-confirm").onclick = () => { void forgetSeat(dir); };
  $("skill-forget-cancel").focus();
}

async function forgetSeat(dir        )                {
  const ticket = epoch;
  const result = await skillsWrite("/api/skills/seats", "DELETE", { dir });
  closeSheet();
  if (ticket !== epoch || currentView() !== "system") return;
  if (result) applySkills(result);
  repaint();
}

/** Clicks inside the skills section. False leaves the event for the rest of the page. */
export function handleSkillsClick(hit         )          {
  if (hit.closest("#system-skills-refresh")) { void refreshSkills(); return true; }
  if (hit.closest("#system-skills-update")) { void updateAll(); return true; }
  if (hit.closest("#system-skills-add")) { openAdd(); return true; }
  const forget = hit.closest("[data-skill-forget]");
  if (forget instanceof HTMLElement) { openForget(forget.dataset.skillForget || ""); return true; }
  const install = hit.closest("[data-skill-install]");
  if (install instanceof HTMLElement) {
    void installSkill(install.dataset.skillInstall || "", install.dataset.skillDirectory || "", install.dataset.skillDir || "");
    return true;
  }
  return false;
}

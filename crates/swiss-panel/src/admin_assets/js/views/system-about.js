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

/* About, at the top of Settings → System (SPEC §panel.pages). The version comes from the
 * gateway info WELCOME already loaded. The update check runs only when the operator clicks,
 * and a 503 — a composition with no checker — removes that row until the page is left.
 * Diagnostics are English on purpose: they are pasted into a bug report, and they carry
 * counts and states only. */
import { h } from "../h.js";
import { locale, tr } from "../i18n.js";
import { gatewayInfo } from "../mcp-state.js";
                                                                            
import { currentView } from "../ui-state.js";
import { btn, card, copyBtn, copyText, row, section, spinner, tag } from "../ui/index.js";
import { api, toast } from "../util.js";

                  
                    
                        
                       
                                               
                    
                      

let phase              = { kind: "idle" };
let updatesHidden = false;
let updateGen = 0;

/** The `swiss --version` line, rebuilt from the fields /api/info already served. */
function versionStamp(build                          )         {
  const version = build?.version ?? "";
  const hash = build?.hash ?? "";
  const time = build?.time ?? "";
  if (!version && !hash && !time) return "swiss: unavailable";
  return version ? `swiss ${version} (${hash}, ${time})` : `swiss (${hash}, ${time})`;
}

/** whenLabel drops the date inside 24h. A build stamp needs the date even when it is today's. */
function builtWhen(iso        )         {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString(locale());
}

function versionRow()              {
  const build = gatewayInfo()?.build;
  const version = build?.version ?? "";
  const hash = build?.hash ?? "";
  const time = build?.time ?? "";
  const sub                           = [];
  if (hash) sub.push(h("code", null, hash));
  if (time) {
    if (sub.length) sub.push(" · ");
    sub.push(tr("system.aboutBuilt", { when: builtWhen(time) }));
  }
  const copy = copyBtn(() => versionStamp(build), { label: tr("ui.copy") });
  copy.dataset.aboutCopy = "1";
  return row({
    name: version ? tr("system.aboutName", { version }) : tr("system.aboutProduct"),
    sub: sub.length ? sub : null,
    cols: hash.endsWith("-dirty") ? [tag(tr("system.aboutDirty"), { tone: "warn" })] : [],
    primary: copy,
    data: { "about-version": "1" },
  });
}

function notesLink(url        )                     {
  // A release note is an https page. Anything else is not opened from this row.
  if (!url.startsWith("https://")) return null;
  return h("a", { href: url, target: "_blank", rel: "noopener" }, tr("system.aboutNotes"));
}

function updateRowNode(state             )              {
  const checking = state.kind === "checking";
  const name = state.kind === "checking"
    ? h("span", null, spinner(), " ", tr("system.aboutChecking"))
    : state.kind === "current" ? tr("system.aboutCurrent")
    : state.kind === "newer" ? tr("system.aboutNewer", { tag: state.tag })
    : state.kind === "none" ? tr("system.aboutNone")
    : state.kind === "error" ? tr("served.updateUnreachable")
    : tr("system.aboutUpdates");
  const sub = state.kind === "newer" ? notesLink(state.url) : null;
  const label = state.kind === "error" ? tr("system.aboutRetry") : tr("system.aboutCheck");
  return row({
    name,
    sub,
    data: { "about-update": state.kind },
    primary: btn(label, {
      id: "system-about-check",
      disabled: checking,
      data: { "about-check": "1" },
    }),
  });
}

function isUpdateBody(body         )                                 {
  if (!body || typeof body !== "object") return false;
  const row = body                          ;
  if (typeof row.newer !== "boolean") return false;
  if (row.latest == null) return true;
  return typeof row.latest === "object"
    && typeof row.latest.tag === "string"
    && typeof row.latest.url === "string";
}

function paintAbout()       {
  const node = document.getElementById("system-about");
  if (!node || currentView() !== "system") return;
  node.replaceWith(aboutSection());
}

async function checkForUpdates()                {
  if (phase.kind === "checking" || updatesHidden) return;
  const ticket = ++updateGen;
  phase = { kind: "checking" };
  paintAbout();
  let response          ;
  try {
    response = await api("/api/update/check");
  } catch {
    if (ticket !== updateGen || currentView() !== "system") return;
    phase = { kind: "error" };
    paintAbout();
    return;
  }
  if (ticket !== updateGen || currentView() !== "system") return;
  if (response.status === 503) {
    updatesHidden = true;
    phase = { kind: "idle" };
    paintAbout();
    return;
  }
  if (!response.ok) {
    phase = { kind: "error" };
    paintAbout();
    return;
  }
  const body          = await response.json().catch(()          => null);
  if (ticket !== updateGen || currentView() !== "system") return;
  if (!isUpdateBody(body)) {
    phase = { kind: "error" };
    paintAbout();
    return;
  }
  const latest = body.latest;
  if (latest && latest.tag) {
    phase = body.newer ? { kind: "newer", tag: latest.tag, url: latest.url } : { kind: "current" };
  } else {
    phase = { kind: "none" };
  }
  paintAbout();
}

async function readJson(path        )                          {
  try {
    const response = await api(path);
    if (!response.ok) return null;
    return await response.json();
  } catch {
    return null;
  }
}

function pluginLine(body         )         {
  if (!body || typeof body !== "object" || !("plugins" in body)) return "plugins: unavailable";
  const plugins = (body                        ).plugins;
  if (!Array.isArray(plugins)) return "plugins: unavailable";
  let on = 0;
  let off = 0;
  for (const item of plugins) {
    if (item && typeof item === "object" && (item                         ).enabled === true) on++;
    else off++;
  }
  return `plugins: ${on} on, ${off} off`;
}

function mcpLine(body         )         {
  if (!body || typeof body !== "object" || !("mcps" in body)) return "mcps: unavailable";
  const mcps = (body                     ).mcps;
  if (!Array.isArray(mcps)) return "mcps: unavailable";
  let running = 0;
  let failed = 0;
  for (const item of mcps) {
    const state = item && typeof item === "object" ? (item                       ).state : undefined;
    // The same split the MCP chip uses: up is running, down or error is the failure count.
    if (state === "up") running++;
    else if (state === "down" || state === "error") failed++;
  }
  return `mcps: ${mcps.length} (${running} running, ${failed} failed)`;
}

function memoryLine(body         )         {
  if (!body || typeof body !== "object" || !("gatewayMb" in body)) return "memory: unavailable";
  const mb = (body                          ).gatewayMb;
  if (typeof mb !== "number" || !Number.isFinite(mb)) return "memory: unavailable";
  return `memory: ${String(Math.round(mb * 10) / 10)} MB resident`;
}

async function diagnosticsText()                  {
  const [plugins, mcps, memory] = await Promise.all([
    readJson("/api/plugins"),
    readJson("/api/mcps"),
    readJson("/api/memory"),
  ]);
  const info = gatewayInfo();
  const build = info?.build;
  const agent = typeof navigator !== "undefined" && navigator.userAgent ? navigator.userAgent : "unavailable";
  return [
    versionStamp(build),
    `target: ${build?.target || "unavailable"}`,
    `panel: ${info?.panelVersion || "unavailable"}`,
    `ui: ${locale()} · ${agent}`,
    pluginLine(plugins),
    mcpLine(mcps),
    memoryLine(memory),
  ].join("\n");
}

async function copyDiagnostics()                {
  const text = await diagnosticsText();
  const ok = await copyText(text);
  toast(ok ? tr("system.aboutDiagnosticsCopied") : tr("ui.copyFailed"), !ok);
}

function diagnosticsRow()              {
  return row({
    name: tr("system.aboutDiagnostics"),
    primary: btn(tr("system.aboutCopyDiagnostics"), {
      id: "system-about-diagnostics",
      data: { "about-diagnostics": "1" },
    }),
  });
}

export function aboutSection()              {
  const node = section({ cap: tr("system.about") }, card(
    versionRow(),
    updatesHidden ? null : updateRowNode(phase),
    diagnosticsRow(),
  ));
  node.id = "system-about";
  return node;
}

export function handleAboutClick(target         )          {
  if (target.closest("[data-about-check]")) { void checkForUpdates(); return true; }
  if (target.closest("[data-about-diagnostics]")) { void copyDiagnostics(); return true; }
  return false;
}

export function resetAbout()       {
  updateGen++;
  phase = { kind: "idle" };
  updatesHidden = false;
}

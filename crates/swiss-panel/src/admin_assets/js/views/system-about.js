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
 * Apply is offered only when status says canApply; a refusal is the sentence instead.
 * HOST.UPDATE and WELCOME re-read status. Leaving the page drops the subscription.
 * Diagnostics are English on purpose: they are pasted into a bug report, and they carry
 * counts and states only. */
import { h } from "../h.js";
import { locale, servedError, tr } from "../i18n.js";
import { gatewayInfo } from "../mcp-state.js";
                                                                            
import { currentView } from "../ui-state.js";
import { btn, card, closeSheet, copyBtn, copyText, row, section, sheet, showSheet, spinner, tag } from "../ui/index.js";
import { $, api, toast } from "../util.js";
import { busOn } from "../bus/bus.js";
import { coalescedRead } from "../bus/live.js";
import { Module, NoticeSchema } from "../bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, UpdateId } from "../bus/pb/swiss/bus/v1/host_pb.js";

                                                                           

                    
                
              
               
                 
               
                 
                        
                       
                      
  

                   
                    
                         
                   
                           
  

                  
                    
                        
                       
                                                                          
                    
                      

let phase              = { kind: "idle" };
let updatesHidden = false;
let updateGen = 0;
let statusGen = 0;
let awaiting = false;
let applyTicket = 0;
let statusView             = { canApply: false, refusal: null, running: false, last: null };
let updateWatch                      = null;
let statusBurst                              = null;

function megabytes(bytes        )         {
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

function attemptWhen(iso        )         {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString(locale(), { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

function viaLabel(via                    )         {
  if (via === "patch") return tr("system.aboutViaPatch");
  if (via === "full") return tr("system.aboutViaFull");
  return via ?? "";
}

function lastText(last             )         {
  // An attempt that found nothing newer downloaded nothing: no via, no size.
  if (last.ok === true && !last.via) {
    return tr("system.aboutLastNewest", { when: last.finishedAt ? attemptWhen(last.finishedAt) : "" });
  }
  if (last.ok === true) {
    return tr("system.aboutLastOk", {
      from: last.from ?? "",
      to: last.to ?? "",
      via: viaLabel(last.via),
      size: megabytes(last.bytes ?? 0),
      when: last.finishedAt ? attemptWhen(last.finishedAt) : "",
    });
  }
  const failed = tr("system.aboutLastFailed", { phase: last.phase ?? "", error: last.error ?? "" });
  return last.rolledBack ? `${failed} ${tr("system.aboutRolledBack")}` : failed;
}

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
  const updating = statusView.running || awaiting;
  const checking = state.kind === "checking";
  const name = updating
    ? h("span", null, spinner(), " ", tr("system.aboutApplying"))
    : checking
      ? h("span", null, spinner(), " ", tr("system.aboutChecking"))
    : state.kind === "current" ? tr("system.aboutCurrent")
    : state.kind === "newer" ? tr("system.aboutNewer", { tag: state.tag })
    : state.kind === "none" ? tr("system.aboutNone")
    : state.kind === "error" ? tr("served.updateUnreachable")
    : tr("system.aboutUpdates");
  const notes = state.kind === "newer" ? notesLink(state.url) : null;
  const refusal = state.kind === "newer" && statusView.refusal && !updating
    ? servedError({ code: statusView.refusal, error: "" }, "")
    : null;
  const lastLine = statusView.last ? lastText(statusView.last) : "";
  // The sub line is one ellipsized line, so a long failure reason keeps its whole text in the
  // tooltip, and the parts are kept apart by the same " · " the version row uses.
  const previous = lastLine ? h("span", { class: "muted", title: lastLine }, lastLine) : null;
  const parts = [notes, refusal, previous].filter((part) => part != null && part !== "");
  const sub = parts.length ? parts.flatMap((part, i) => (i ? [" · ", part] : [part])) : null;
  const label = state.kind === "error" ? tr("system.aboutRetry") : tr("system.aboutCheck");
  const newer = state.kind === "newer" ? state : null;
  const plan = newer?.plan ?? null;
  const check = updating ? null : btn(label, {
    id: "system-about-check",
    disabled: checking,
    data: { "about-check": "1" },
  });
  const apply = !updating && newer && plan && statusView.canApply
    ? btn(tr("system.aboutApplyTo", { tag: newer.tag, size: megabytes(plan.bytes) }), {
      id: "system-about-apply",
      kind: "primary",
      data: { "about-apply": "1" },
    })
    : null;
  return row({
    name,
    sub,
    data: { "about-update": updating ? "updating" : state.kind },
    ...(check ? { primary: check } : {}),
    ...(apply ? { secondary: apply } : {}),
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
  const plan = readPlan(body.plan);
  if (latest && latest.tag) {
    phase = body.newer ? { kind: "newer", tag: latest.tag, url: latest.url, plan } : { kind: "current" };
  } else {
    phase = { kind: "none" };
  }
  paintAbout();
}

function readPlan(value         )                      {
  if (!value || typeof value !== "object") return null;
  const row = value                                                      ;
  if ((row.via !== "patch" && row.via !== "full") || typeof row.bytes !== "number" || typeof row.full !== "number") return null;
  return { via: row.via, bytes: row.bytes, full: row.full };
}

function openApply()       {
  if (phase.kind !== "newer" || !phase.plan || !statusView.canApply || statusView.running || awaiting) return;
  const kept = { tag: phase.tag, url: phase.url, plan: phase.plan };
  showSheet(sheet({
    title: tr("system.aboutApplyTitle"),
    body: [
      h("p", null, tr("system.aboutApplyBody")),
      h("p", null, tr("system.aboutApplyEnds")),
      h("p", null, tr("system.aboutApplyReloads")),
    ],
    foot: [
      btn(tr("system.cancel"), { id: "about-apply-cancel" }),
      btn(tr("system.aboutApply"), { id: "about-apply-confirm", kind: "primary" }),
    ],
  }));
  $("about-apply-cancel").onclick = closeSheet;
  $("about-apply-confirm").onclick = () => { void applyUpdate(kept); };
  $("about-apply-cancel").focus();
}

async function applyUpdate(kept                                                  )                {
  const ticket = ++updateGen;
  closeSheet();
  let response          ;
  try {
    response = await api("/api/update/apply", { method: "POST", body: "{}" });
  } catch {
    if (ticket !== updateGen || currentView() !== "system") return;
    phase = { kind: "newer", ...kept };
    paintAbout();
    toast(tr("system.aboutFailed"), true);
    return;
  }
  if (ticket !== updateGen || currentView() !== "system") return;
  if (response.status === 202) {
    applyTicket++;
    awaiting = true;
    phase = { kind: "newer", ...kept };
    paintAbout();
    return;
  }
  const body          = await response.json().catch(()          => null);
  phase = { kind: "newer", ...kept };
  paintAbout();
  toast(servedError(body, tr("system.aboutFailed")), true);
}

function parseStatus(body         )                    {
  if (!body || typeof body !== "object") return null;
  const row = body                                                                                ;
  const last = row.last && typeof row.last === "object" ? row.last                : null;
  return {
    canApply: row.canApply === true,
    refusal: typeof row.refusal === "string" && row.refusal ? row.refusal : null,
    running: row.running === true,
    last,
  };
}

async function readStatus()                {
  const ticket = statusGen;
  const seenApply = applyTicket;
  const wasAwaiting = awaiting;
  let response          ;
  try {
    response = await api("/api/update/status");
  } catch {
    return;
  }
  if (ticket !== statusGen || seenApply !== applyTicket || currentView() !== "system" || !response.ok) return;
  const parsed = parseStatus(await response.json().catch(()          => null));
  if (ticket !== statusGen || seenApply !== applyTicket || !parsed) return;
  statusView = parsed;
  if (!parsed.running) awaiting = false;
  if (wasAwaiting && !parsed.running && parsed.last?.ok === true && phase.kind === "newer") {
    phase = { kind: "current" };
  }
  paintAbout();
}

/** HOST.UPDATE while System is mounted. WELCOME re-reads status too. Neither path checks or applies. */
export function watchAbout()       {
  if (updateWatch) return;
  const burst = coalescedRead(() => readStatus(), { resync: true });
  statusBurst = burst;
  const off = busOn(
    { module: Module.HOST, sub: HostSub.UPDATE, key: "" },
    UpdateId.UPDATE_CHANGED,
    NoticeSchema,
    () => { burst.push(); },
  );
  updateWatch = () => {
    off();
    burst.stop();
    statusBurst = null;
  };
  burst.push();
}

/** Drop the HOST.UPDATE subscription when System is left. */
export function unwatchAbout()       {
  statusGen++;
  updateWatch?.();
  updateWatch = null;
}

/** The bus handler's read, so a test can deliver the notice without a socket. */
export function noteUpdate()       {
  statusBurst?.push();
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
  if (target.closest("[data-about-apply]")) { openApply(); return true; }
  if (target.closest("[data-about-check]")) { void checkForUpdates(); return true; }
  if (target.closest("[data-about-diagnostics]")) { void copyDiagnostics(); return true; }
  return false;
}

export function resetAbout()       {
  updateGen++;
  statusGen++;
  applyTicket++;
  phase = { kind: "idle" };
  updatesHidden = false;
  awaiting = false;
  statusView = { canApply: false, refusal: null, running: false, last: null };
}

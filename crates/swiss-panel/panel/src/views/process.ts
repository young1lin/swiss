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

/* Process owns two inventories with different lifetimes. Each mount owns its
 * requests and polling; the editor and log sheet own their own sessions. */
import { $, api, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
import { tr, trn } from "../i18n.js";
import {  iconBtn, moreBtn } from "../ui/button.js";
import { actionBtn } from "../ui/action.js";
import { dot, tag } from "../ui/status.js";
import { card, emptyNode, note, paneBody, paneHead, section } from "../ui/page.js";
import { row } from "../ui/row.js";
import { groupNode } from "../ui/group.js";
import { popupMenu, type MenuItem } from "../ui/menu.js";
import { durShort } from "../ui/timeline.js";
import { portChip } from "../ui/chip.js";
import type { ProcessRow, GatewayFacts, ChildRow } from "./process/types.js";
import { openLogs, disposeLogs } from "./process/logs.js";
import { openForm, deleteApp, disposeForm } from "./process/form.js";

interface PageSession {
  controller: AbortController;
  inventory: Promise<void> | null;
  children: Promise<void> | null;
  timer: ReturnType<typeof setInterval> | null;
  ticks: number;
  detach: () => void;
}

let active: PageSession | null = null;
let rows: ProcessRow[] = [];
let gateway: GatewayFacts | null = null;
let children: ChildRow[] = [];
const foldedOwners = new Set<string>();
const appSignatures = new Map<string, string>();
const pendingApps = new Map<string, string>();
const pendingChildren = new Set<number>();
let childrenSignature = "";

function current(s: PageSession): boolean { return active === s && !s.controller.signal.aborted; }

/* Check ownership before reporting failure as well as before changing the DOM.
 * Leaving a page is cancellation, not a failed request on the next page. */
async function request<T>(s: PageSession, path: string, options: RequestInit = {}): Promise<T | null> {
  if (!current(s)) return null;
  try {
    const response = await api(path, { ...options, signal: s.controller.signal });
    const body: unknown = await response.json().catch(() => ({}));
    if (!current(s)) return null;
    if (!response.ok) {
      const message = body && typeof body === "object" && "error" in body && typeof body.error === "string" ? body.error : tr("util.httpN", { n: response.status });
      toast(message, true);
      return null;
    }
    return body as T;
  } catch {
    if (current(s)) toast(tr("util.requestFailedGatewayRunning"), true);
    return null;
  }
}

function statusWord(status: ProcessRow["status"]): string {
  switch (status) {
    case "running": return tr("process.status.running");
    case "partial": return tr("process.status.partial");
    case "conflict": return tr("process.status.conflict");
    case "stopped": return tr("process.status.stopped");
    default: return tr("process.status.unknown");
  }
}

function actionWord(action: string): string {
  switch (action) {
    case "start": return tr("process.action.start");
    case "stop": return tr("process.action.stop");
    case "restart": return tr("process.action.restart");
    default: return action;
  }
}

function busyWord(action: string): string {
  switch (action) {
    case "start": return tr("process.busy.start");
    case "stop": return tr("process.busy.stop");
    case "restart": return tr("process.busy.restart");
    default: return tr("process.busyWord", { action: actionWord(action) });
  }
}

function busy(r: ProcessRow): string | undefined { return pendingApps.get(r.name) || r.busy; }
function canStart(r: ProcessRow): boolean { return !busy(r) && (r.status === "stopped" || r.status === "unknown"); }
function canStop(r: ProcessRow): boolean { return !busy(r) && (r.status === "running" || r.status === "partial"); }

function dotState(r: ProcessRow): Parameters<typeof dot>[0] {
  const action = busy(r);
  if (action) return action === "start" ? "starting" : "stopping";
  if (r.status === "running") return "up";
  if (r.status === "partial" || r.status === "conflict") return "down";
  return r.status === "stopped" ? "idle" : "off";
}

function diagnostic(r: ProcessRow): string {
  if (r.status === "conflict") return r.reason || tr("process.conflictDetail", { pid: String(r.conflictPid ?? "?"), image: r.conflictImage ?? "?" });
  if (r.status === "partial") return r.reason || tr("process.downPorts", { ports: (r.downPorts || []).join(", ") });
  if (r.status === "unknown") return r.reason || "";
  return "";
}

function statusTag(r: ProcessRow): HTMLElement {
  const action = busy(r);
  const word = action ? busyWord(action) : statusWord(r.status);
  return tag(word, {
    // unknown is NOT stopped: it is an unverified claim (another gateway's
    // child, or a record without ports) - the amber of partial, so a restart
    // that wipes the last action still leaves it distinct from a clean stop.
    tone: action ? undefined : r.status === "conflict" ? "bad" : r.status === "partial" || r.status === "unknown" ? "warn" : undefined,
    title: action ? tr("process.busyTitle", { action: actionWord(action) }) : diagnostic(r) || word,
  });
}

function gatewayLine(): string {
  return gateway ? tr("process.gatewayLine", { pid: String(gateway.pid), up: durShort(gateway.uptimeSec), mem: String(gateway.memMb) }) : "";
}

/* Occupancy first, promise second (SPEC §process.panel): the row's ports are
 * the LISTENER TABLE's fact - every port the row's pid actually holds, green,
 * whether the config declared it or not. What the config declared and the
 * pid does NOT hold rides after them as a red chip (a promise the running
 * app has not kept). An app with no pid holds nothing: its declared ports
 * show as quiet gray chips - the config's words, never a false alarm. */
function portChips(r: ProcessRow): HTMLElement[] {
  const declared = new Set(r.ports);
  const heldSet = new Set(r.heldPorts || []);
  if (r.pid != null && (r.status === "running" || r.status === "partial")) {
    return [
      ...(r.heldPorts || []).map((p) => portChip(String(p), { tone: "up", title: tr(declared.has(p) ? "process.port.up" : "process.port.held", { port: String(p) }) })),
      ...r.ports.filter((p) => !heldSet.has(p)).map((p) => portChip(String(p), { tone: "down", title: tr("process.port.down", { port: String(p) }) })),
    ];
  }
  return r.ports.map((p) => portChip(String(p), {
    tone: r.status === "conflict" ? "down" : "held",
    title: tr(r.status === "conflict" ? "process.port.conflict" : "process.port.declared", { port: String(p) }),
  }));
}

function appRow(r: ProcessRow): HTMLElement {
  const last = r.lastAction;
  const lastText = last ? tr("process.lastAction", { action: actionWord(last.action), message: last.message }) : "";
  const reason = diagnostic(r);
  const live = r.status === "running" || r.status === "partial";
  // The launcher pattern: a start can fail while the app heals - the launcher
  // exits before the window closes, its child binds the port right after. The
  // raw failure sentence on a running row reads as a contradiction; the row
  // says the port is held now, the sentence stays in the history sheet.
  const healedStart = last != null && !last.ok && last.action === "start" && r.status === "running";
  // The row's one action, the ACTION not the state (SPEC §panel.design rule 7): a live app offers
  // Stop (the stop square), a stopped one offers Start (play), and while a verb is in flight the button
  // names that verb instead of showing an ellipsis into a dead click.
  const inflight = busy(r);
  const click: "start" | "stop" = live ? "stop" : "start";
  const glyph: "start" | "stop" | "restart" = inflight === "restart" ? "restart" : click;
  const word = inflight ? busyWord(inflight) : click === "stop" ? tr("process.action.stop") : tr("process.action.start");
  return row({
    lead: dot(dotState(r), busy(r) ? busyWord(busy(r) || "") : statusWord(r.status)),
    name: r.name,
    err: last && !last.ok && !healedStart && r.status !== "conflict" && r.status !== "partial" ? lastText : undefined,
    sub: reason || (last?.ok ? lastText : (healedStart ? tr("process.startRecovered") : undefined)),
    title: reason || lastText || undefined,
    meta: [
      ...portChips(r),
      r.pid != null ? h("code", null, tr("process.pid", { pid: String(r.pid) })) : null,
      r.uptimeSec != null ? h("span", { data: { pruptime: "" } }, tr("process.uptime", { up: durShort(r.uptimeSec) })) : null,
      r.autostart ? tag(tr("process.mark.autostart"), { title: tr("process.mark.autostartHint") }) : null,
      r.stopIsKill ? tag(tr("process.mark.stopIsKill"), { title: tr("process.mark.stopIsKillHint") }) : null,
    ],
    cols: [statusTag(r)],
    primary: actionBtn(glyph, word, {
      title: inflight ? word : click === "stop" ? tr("process.action.stopHint") : tr("process.action.startHint"),
      data: { pract: click, prname: r.name },
      disabled: !!inflight || (click === "stop" ? !canStop(r) : !canStart(r)),
    }),
    secondary: iconBtn("file", tr("process.logs.forApp", { name: r.name }), { ghost: true, data: { prlogs: r.name } }),
    more: moreBtn(tr("process.actionsFor", { name: r.name }), { data: { prmore: r.name } }),
    data: { prrow: r.name },
  });
}

function childKind(kind: string): string {
  switch (kind) {
    case "mcp": return tr("process.childKind.mcp");
    case "run": return tr("process.childKind.run");
    case "terminal": return tr("process.childKind.terminal");
    default: return kind;
  }
}

function childrenCard(): HTMLElement {
  if (!children.length) return note(tr("process.childrenNone"));
  const owners = Array.from(new Set(children.map((c) => c.owner))).sort();
  return h("div", { class: "pr-children" }, ...owners.map((owner) => {
    const mine = children.filter((c) => c.owner === owner).sort((a, b) => a.label.localeCompare(b.label));
    const parts = groupNode({ name: owner, count: mine.length, density: "page", collapsed: foldedOwners.has(owner) },
      ...mine.map((c) => row({
        name: c.label, sub: childKind(c.kind),
        meta: [h("code", null, tr("process.pid", { pid: String(c.pid) })), tr("process.uptime", { up: durShort(c.uptimeSec) }), c.memMb != null ? tr("process.memMb", { n: String(c.memMb) }) : null],
        primary: c.stoppable ? actionBtn("stop", tr(pendingChildren.has(c.pid) ? "process.busy.stop" : "process.action.stopChild"), {
          disabled: pendingChildren.has(c.pid), data: { prstopchild: String(c.pid) },
          title: pendingChildren.has(c.pid) ? tr("process.busy.stop") : tr("process.action.stopChildTitle", { label: c.label }),
        }) : undefined,
        data: { prchild: String(c.pid) },
      })));
    parts.toggle.onclick = () => {
      const folded = parts.root.classList.toggle("collapsed");
      if (folded) foldedOwners.add(owner); else foldedOwners.delete(owner);
      parts.toggle.setAttribute("aria-expanded", String(!folded));
    };
    return parts.root;
  }));
}

function appSection(): HTMLElement {
  const content = rows.length ? card(...rows.map(appRow)) : emptyNode({ icon: "activity", title: tr("process.emptyTitle"), hint: tr("process.emptyHint"), action: tr("process.addApp") });
  content.id = "prAppList";
  const result = section({ cap: tr("process.appsCap"), note: tr("process.appsNote") }, content);
  result.id = "prApps";
  return result;
}

/* The structural signature decides replacement ONLY. The uptime text is
 * deliberately absent: durShort re-words a running row every few seconds, and
 * a replaceWith on that cadence destroys a control the keyboard just focused
 * inside that row. reconcile patches the uptime text in place instead. */
function rowSignature(r: ProcessRow): string {
  return JSON.stringify({ ...r, uptimeSec: null, pending: pendingApps.get(r.name) });
}
function childSignature(): string {
  return JSON.stringify([children.map((c) => ({ ...c, uptimeSec: durShort(c.uptimeSec) })), Array.from(pendingChildren)]);
}

function rememberPaint(): void {
  appSignatures.clear();
  rows.forEach((r) => appSignatures.set(r.name, rowSignature(r)));
  childrenSignature = childSignature();
}

function paint(): void {
  const childBody = h("div", { id: "prChildren" }, childrenCard());
  fill($("pane"), paneBody({},
    paneHead({ desc: tr("process.desc"), sub: h("span", { id: "prGateway" }, gatewayLine()), actions: [actionBtn("new", tr("process.addApp"), { id: "prAdd" })] }),
    appSection(),
    section({ cap: tr("process.childrenCap"), note: tr("process.childrenNote") }, childBody)));
  rememberPaint();
  $("countChip").textContent = countText();
}

/* Memory-only changes patch the gateway line, never the buttons under a pointer;
 * an uptime-only change patches the row's own text, never the row itself.
 * A structurally changed app replaces only its own row; a child-only response paints immediately. */
function reconcile(): void {
  const line = document.getElementById("prGateway");
  const sectionNode = document.getElementById("prApps");
  if (!line || !sectionNode) return;
  line.textContent = gatewayLine();
  const names = rows.map((r) => r.name);
  if (JSON.stringify(names) !== JSON.stringify(Array.from(appSignatures.keys()))) {
    sectionNode.replaceWith(appSection());
    appSignatures.clear();
  } else {
    const nodes = Array.from(sectionNode.querySelectorAll<HTMLElement>("[data-prrow]"));
    for (const r of rows) {
      const node = nodes.find((n) => n.dataset.prrow === r.name);
      if (appSignatures.get(r.name) === rowSignature(r)) {
        // Text-only churn (the uptime words moved): patch them in place so
        // the row node - and the focus inside it - survives the sample.
        if (node && r.uptimeSec != null) {
          const up = node.querySelector<HTMLElement>("[data-pruptime]");
          const text = tr("process.uptime", { up: durShort(r.uptimeSec) });
          if (up && up.textContent !== text) up.textContent = text;
        }
        continue;
      }
      node?.replaceWith(appRow(r));
    }
  }
  rows.forEach((r) => appSignatures.set(r.name, rowSignature(r)));
  if (childrenSignature !== childSignature()) {
    document.getElementById("prChildren")?.replaceChildren(childrenCard());
    childrenSignature = childSignature();
  }
  $("countChip").textContent = countText();
}

function load(): Promise<void> {
  const s = active;
  if (!s || !current(s)) return Promise.resolve();
  if (s.inventory) return s.inventory;
  s.inventory = (async () => {
    const body = await request<{ apps?: ProcessRow[]; gateway?: GatewayFacts }>(s, "/api/process");
    if (!body || !current(s)) return;
    rows = body.apps || [];
    gateway = body.gateway || null;
    reconcile();
  })().finally(() => { s.inventory = null; });
  return s.inventory;
}

function loadChildren(): Promise<void> {
  const s = active;
  if (!s || !current(s)) return Promise.resolve();
  if (s.children) return s.children;
  s.children = (async () => {
    const body = await request<{ children?: ChildRow[] }>(s, "/api/process/children");
    if (!body || !current(s)) return;
    children = body.children || [];
    reconcile();
  })().finally(() => { s.children = null; });
  return s.children;
}

async function act(name: string, verb: "start" | "stop" | "restart", force = false): Promise<void> {
  const s = active;
  if (!s || pendingApps.has(name)) return;
  pendingApps.set(name, verb);
  reconcile();
  await request(s, "/api/process/apps/" + encodeURIComponent(name) + "/" + verb, { method: "POST", body: JSON.stringify(force ? { force: true } : {}) });
  if (!current(s)) return;
  if (s.inventory) await s.inventory;
  pendingApps.delete(name);
  await load();
  reconcile();
}

async function forceStop(r: ProcessRow): Promise<void> {
  const port = r.ports.length ? String(r.ports[0]) : "...";
  const image = r.conflictImage || "...";
  const pid = String(r.pid ?? r.conflictPid ?? "...");
  if (confirm(tr("process.confirm.force", { name: r.name, pid, image, port }))) await act(r.name, "stop", true);
}

async function stopChild(c: ChildRow): Promise<void> {
  const s = active;
  if (!s || pendingChildren.has(c.pid)) return;
  const ask = c.kind === "mcp" ? tr("process.confirm.stopMcp", { label: c.label }) : tr("process.confirm.stopChild", { label: c.label });
  if (!confirm(ask)) return;
  pendingChildren.add(c.pid);
  reconcile();
  const answer = await request<{ ok?: boolean; message?: string }>(s, "/api/process/children/" + c.pid + "/stop", { method: "POST", body: "{}" });
  if (!current(s)) return;
  pendingChildren.delete(c.pid);
  if (answer) toast(answer.message || tr("process.childStopped", { label: c.label }), !answer.ok);
  await loadChildren();
  reconcile();
}

function edit(name: string | null): void { disposeLogs(); void openForm(name, load); }
function logs(r: ProcessRow): void { disposeForm(); void openLogs(r); }

function rowMenu(anchor: HTMLElement, r: ProcessRow): void {
  const items: MenuItem[] = [
    { label: tr("process.action.start"), disabled: !canStart(r), fn: () => { void act(r.name, "start"); } },
    { label: tr("process.action.stop"), disabled: !canStop(r), fn: () => { void act(r.name, "stop"); } },
    { label: tr("process.action.restart"), disabled: !canStop(r), fn: () => { void act(r.name, "restart"); } },
  ];
  if (r.ui) items.push({ label: tr("process.action.openUi"), disabled: !!busy(r) || (r.status !== "running" && r.status !== "partial"), fn: () => { window.open(r.ui, "_blank", "noopener"); } });
  items.push({ sep: true },
    { label: tr("process.action.logs"), fn: () => { logs(r); } },
    { label: tr("process.action.edit"), disabled: !!busy(r), fn: () => { edit(r.name); } });
  items.push({ sep: true });
  if (r.lastAction?.canForce) items.push({ label: tr("process.action.force"), danger: true, disabled: !!busy(r), fn: () => { void forceStop(r); } });
  items.push({ label: tr("process.action.delete"), danger: true, disabled: !!busy(r), fn: () => { void deleteApp(r.name, load); } });
  popupMenu(anchor.getBoundingClientRect(), items);
}

export async function mount(options: { signal?: AbortSignal } = {}): Promise<void> {
  unmount();
  if (options.signal?.aborted) return;
  const s: PageSession = { controller: new AbortController(), inventory: null, children: null, timer: null, ticks: 0, detach: () => {} };
  active = s;
  const abort = (): void => { if (active === s) unmount(); };
  options.signal?.addEventListener("abort", abort, { once: true });
  s.detach = () => { options.signal?.removeEventListener("abort", abort); };
  await Promise.all([load(), loadChildren()]);
  if (!current(s)) return;
  paint();
  s.timer = setInterval(() => {
    if (!current(s) || document.hidden) return;
    s.ticks++;
    if (s.ticks % (rows.some((r) => !!busy(r)) ? 1 : 4) === 0) void load();
    if (s.ticks % 10 === 0) void loadChildren();
  }, 500);
  $("pane").onclick = (event: MouseEvent): void => {
    const t = targetEl(event);
    if (!t || !current(s)) return;
    if (t.closest("[data-empty-action], #prAdd")) { edit(null); return; }
    const action = t.closest<HTMLElement>("[data-pract]");
    if (action && !(action as HTMLButtonElement).disabled) { void act(action.dataset.prname || "", action.dataset.pract === "stop" ? "stop" : "start"); return; }
    const child = t.closest<HTMLElement>("[data-prstopchild]");
    if (child) { const c = children.find((x) => x.pid === Number(child.dataset.prstopchild)); if (c) void stopChild(c); return; }
    const log = t.closest<HTMLElement>("[data-prlogs]");
    if (log) { const r = rows.find((x) => x.name === log.dataset.prlogs); if (r) logs(r); return; }
    const more = t.closest<HTMLElement>("[data-prmore]");
    if (more) { event.stopPropagation(); const r = rows.find((x) => x.name === more.dataset.prmore); if (r) rowMenu(more, r); }
  };
}

export async function refresh(): Promise<void> {
  const s = active;
  if (!s) return;
  await Promise.all([load(), loadChildren()]);
  if (current(s)) paint();
}
export function countText(): string { return trn(rows.length, "process.appsOne", "process.appsOther", { n: String(rows.length) }); }

export function unmount(): void {
  const s = active;
  active = null;
  if (s) { s.controller.abort(); s.detach(); if (s.timer != null) clearInterval(s.timer); }
  disposeLogs();
  disposeForm();
  $("pane").onclick = null;
  rows = []; gateway = null; children = [];
  pendingApps.clear(); pendingChildren.clear(); appSignatures.clear();
  childrenSignature = "";
}

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

import type { ApiJobRow, ApiJobsResponse, ApiMcpListResponse, ApiTunnelConnectionRow, ApiTunnelRuleRow, ApiTunnelsResponse } from "./types/api.js";
import { $, api, apiJson, dotTitle, iconNode, toast, whenLabel } from "./util.js";
import { frag, h } from "./h.js";
import type { HChild } from "./h.js";
import { currentPageCount, navigatePage, refreshPage } from "./page-registry.js";
import { patchSidebar } from "./menu.js";
import { patchDetailHead, renderPane } from "./pane.js";
import { rowOf } from "./sidebar.js";
import { triggerSummary } from "./jobs-v2.js";
import { currentView } from "./ui-state.js";
import { setTunResponse, tunBusyOf, tunDragging, tunResponse, mountedTunScope } from "./tunnel-state.js";
import { jobGroupNames, jobIsBusy, jobRows, setJobGroupNames, setJobRows } from "./job-state.js";
import { mcpDetail, mcpRows, memoryInfo, selectedMcp, setMcpDetail, setMcpGroups, setMcpRows, setMemoryInfo, setSelectedMcp } from "./mcp-state.js";


/* --- polling ---------------------------------------------------------------------------------- */
async function loadList(): Promise<void> {
  try {
    const r = await api("/api/mcps");
    if (!r.ok) { toast("HTTP " + r.status, true); return; }
    const j: ApiMcpListResponse = await r.json();
    setMcpRows(j.mcps || []);
    setMcpGroups(j.groups || []);
    const sel = selectedMcp();
    if (sel && !rowOf(sel)) { setSelectedMcp(null); setMcpDetail(null); }
    patchSidebar();
    if (mcpDetail()) patchDetailHead();
    else renderPane(); // empty state; nothing here can hold user input
  } catch (e) { /* handled */ }
}

async function loadMemory(tree?: boolean): Promise<void> {
  try {
    const r = await api("/api/memory" + (tree ? "?tree=1" : ""));
    if (!r.ok) return;
    setMemoryInfo(await r.json());
    renderMemory();
  } catch (e) { /* handled */ }
}

function renderMemory(): void {
  const m = memoryInfo();
  if (!m) return;
  const chip = $("memChip");
  const total = m.childrenMb ? Math.round((m.gatewayMb + m.childrenMb) * 10) / 10 : m.gatewayMb;
  chip.textContent = total + " MB" + (m.processCount > 1 ? " · " + m.processCount + " procs" : "");
  chip.title = "gateway RSS " + m.gatewayMb + " MB (heap " + m.heapUsedMb + "/" + m.heapTotalMb +
    " MB, external " + m.externalMb + " MB)" +
    (m.childrenMb ? "\nproc-MCP children " + m.childrenMb + " MB across " + (m.processCount - 1) + " processes" : "") +
    (m.childrenPending ? "\nChild processes could not be measured — click to retry." : "") +
    "\nClick — refresh the memory reading. Press r — refresh memory and the current view.";
  // The chip stays a control on every paint (the toolbar button is long gone), not only the
  // old childrenPending retry — this function re-runs every 6s poll and would otherwise
  // strip the class and the click with the first one. The click itself is memory-ONLY: the
  // chip shows a reading, and a reading is not a reload button — the full refresh lives on
  // the r key (refreshNow below).
  chip.className = "chip button";
  chip.onclick = refreshMemoryNow;
}

/** The chip's click: re-read memory with the child walk, nothing else. The chip shows a
 *  number, so a click asks for the number — reloading the active view under the pointer
 *  (focus, scroll, an expanded row) is a side effect nobody asked for. */
function refreshMemoryNow(): void { void loadMemory(true); }

/** The panel's ONE explicit view refresh (the r key — the chip's click is memory-only, see
 *  refreshMemoryNow): re-read memory with the child walk, and reload the active view. On
 *  Data this key is the only manual reload there is — its poll deliberately leaves the
 *  paged-in lists alone, so without this the view could never be forced up to date. */
function refreshNow(): void { void loadMemory(true); void refreshPage(); }

/* ================================================================================================
   Tunnels
   ------------------------------------------------------------------------------------------------
   Same rendering contract as the MCP side: renderTunnels() rebuilds the pane on an explicit action,
   and the 6s poll may only call patchTunnels(), which touches dots, state text, button labels and
   the footer. Every editable field lives in a sheet (#sheet, a separate subtree), so a poll can
   never wipe something being typed — but a poll that rebuilt the list would still steal focus from
   a keyboard user mid-tab, which is why patching exists at all.

   The plugin owns TWO L2 pages now (docs/13 D5, as revised): #tunnels (SSH Connections,
   scope "conns") and #tunnel-forwards (Port Forwards, scope "rules"). The page mount sets
   mountedTunScope() to its scope; there is no page-local tab control anymore.
   ================================================================================================ */

function tunData(): ApiTunnelsResponse {
  return tunResponse() || { connections: [], rules: [], ruleGroups: [], connGroups: [], mcps: [] };
}

/** Either tunnels page id — the render/patch guards and nothing else. */
function isTunnelsView(v: string | null): boolean { return v === "tunnels" || v === "tunnel-forwards"; }

/** Which tunnel scope is on screen — the /api/groups/{scope} family's own word. The wire keys
 *  stay ruleGroups/connGroups (docs/20 §3); this maps the mounted page to its scope. */
function tunScope(): "conns" | "rules" { return mountedTunScope() === "conns" ? "conns" : "rules"; }
function tunRows(): ApiTunnelConnectionRow[] | ApiTunnelRuleRow[] { return mountedTunScope() === "conns" ? tunData().connections : tunData().rules; }
function tunGroupsList(): string[] {
  return mountedTunScope() === "conns" ? tunData().connGroups || [] : tunData().ruleGroups || [];
}

/** The jobs scope's group names (docs/20 G4) — /api/jobs carries them at its top level. */
function jobGroupsList(): string[] { return jobGroupNames() || ["default"]; }

function setView(v: string): Promise<void> { return navigatePage(v); }

/** The MCP chip text — ONE builder (menu.js's patchSidebar reuses it). The two copies had
 *  drifted: patchSidebar counted "· N down" and this one did not, so the chip lost and regained
 *  that segment every 6s poll versus every view switch. */
function mcpChipText(): string {
  let up = 0, bad = 0;
  mcpRows().forEach((m) => {
    if (m.state === "up") up++;
    else if (m.state === "down" || m.state === "error") bad++;
  });
  return mcpRows().length + " MCPs · " + up + " up" + (bad ? " · " + bad + " down" : "");
}

function updateCountChip(): void { $("countChip").textContent = currentPageCount(); }

/** `patchOnly` is what the poll passes: refresh the data, then patch rather than rebuild. */
async function loadTunnels(patchOnly?: boolean): Promise<void> {
  const j = await apiJson<ApiTunnelsResponse>("/api/tunnels");
  if (!j) return;
  setTunResponse(j);
  if (isTunnelsView(currentView())) {
    const { patchTunnels, renderTunnels } = await import("./tunnels.js");
    if (tunDragging()) return; // a rebuild under the pointer would cancel the drag; patch later
    if (patchOnly && $("pane").querySelector(".tun-foot")) patchTunnels();
    else renderTunnels();
  }
  updateCountChip();
}

/* ================================================================================================
   Jobs
   ------------------------------------------------------------------------------------------------
   The scheduled-command view, split exactly like Tunnels: this file owns the data loader and the
   row template (jobs.js renders/wires/patches it). One builder (jobsChipText) feeds both the
   header chip and the view footer — the mcpChipText drift taught that lesson once already.
   ================================================================================================ */

// The row schedule text is jobs-v2.js's triggerSummary (imported at the top): the v2
// trigger object with a v1-field fallback, so the same row renders on any gateway this
// panel can talk to.

/** The row dot: running beats everything, off is idle, a recorded failure is down, and a job that
 *  never ran yet is idle rather than up — no run has ever succeeded. */
function jobDotClass(j: ApiJobRow): string {
  if (j.running) return "starting";
  if (!j.enabled) return "idle";
  if (j.lastOk === false) return "down";
  return j.lastRunAt ? "up" : "idle";
}

/** The job row as a node (docs/37 R5): every label, command and trigger sentence is a text
 *  node — data-last/data-next always render (possibly empty) so patchJobs can fill them in. */
function jobRowNode(j: ApiJobRow): HTMLElement {
  const busy = jobIsBusy(j.name);
  // The v2 identity (docs/11 §7.1): the title is the human name when one is set, the id
  // stays beside it because every action still addresses the id.
  const title: HChild = j.title && j.title !== j.name
    ? frag(j.title, " ", h("span", { class: "via" }, "· " + j.name))
    : j.name;
  const labels: HChild = (j.labels || []).length
    ? frag(" ", h("span", { class: "via" }, (j.labels || []).map((l: string): string => { return "#" + l; }).join(" ")))
    : null;
  const word = busy ? "starting" : jobDotClass(j);
  return h("div", { class: "tun-row", data: { job: j.name } },
    h("span", { class: "dot " + word, data: { dot: "" }, title: dotTitle(word) }),
    h("div", { class: "tun-main" },
      h("div", { class: "tun-name" }, title, labels, j.enabled ? null : frag(" ", h("span", { class: "via" }, "· off"))),
      h("div", { class: "tun-sub" },
        h("code", null, j.command),
        " ",
        h("span", { class: "via" }, "· " + triggerSummary(j)),
        " ",
        h("span", { class: "via", data: { last: "" } }, j.lastRunAt
          ? "· last " + whenLabel(j.lastRunAt) + (j.lastOk === false ? " · failed" : "")
          : ""),
        " ",
        h("span", { class: "via", data: { next: "" } }, j.enabled && j.nextDueAt ? "· next " + whenLabel(j.nextDueAt) : ""))),
    h("div", { class: "tun-acts" },
      h("button", { class: "btn", data: { run: "" }, disabled: busy || !!j.running }, busy ? "…" : "Run now"),
      // One primary per row (docs/18 V5): Edit, History and Delete answer from the ellipsis
      // menu (jobs.js), so Delete is not a red button repeated down the whole list.
      h("button", { class: "btn ghost icon", data: { more: "" }, aria: { label: "Row actions" }, title: "Row actions" },
        iconNode("ellipsis"))));
}

function jobsChipText(): string {
  const rows = jobRows();
  const on = rows.filter((j) => { return j.enabled; }).length;
  const failing = rows.filter((j) => { return j.enabled && j.lastOk === false; }).length;
  return rows.length + (rows.length === 1 ? " job" : " jobs") + " · " + on + " on" +
    (failing ? " · " + failing + " failing" : "");
}

/** `patchOnly` is what the poll passes: refresh the data, then patch rather than rebuild —
 *  the same contract as loadTunnels. */
async function loadJobs(patchOnly?: boolean): Promise<void> {
  const j = await apiJson<ApiJobsResponse>("/api/jobs");
  if (!j) return;
  setJobRows(j.jobs || []);
  setJobGroupNames(j.groups && j.groups.length ? j.groups : ["default"]);
  if (currentView() === "jobs") {
    const { patchJobs, renderJobs } = await import("./jobs.js");
    if (patchOnly && $("pane").querySelector("[data-foot]")) patchJobs();
    else renderJobs();
  }
  updateCountChip();
}

/** `18989 → 127.0.0.1:18989   via bastion · serves pg-app ●` — the sub-line's
 *  children (docs/37 R5): every host, connection name and remark the server sends is a
 *  text node, and the arrow is the character itself rather than the &rarr; entity a string
 *  builder had to spell out. */
function ruleSubNode(r: ApiTunnelRuleRow): HChild[] {
  const out: HChild[] = [String(r.localPort), " → ", r.targetHost + ":" + r.targetPort,
    " ", h("span", { class: "via" }, "via " + r.connectionName)];
  if (r.mcpRows && r.mcpRows.length) {
    out.push(" ", h("span", { class: "via" }, "· serves "));
    r.mcpRows.forEach((m, i) => {
      if (i) out.push(", ");
      // A known MCP's dot keeps its own one-word title (docs/18 V6); an unknown name paints
      // no state, so it takes no title either — the hover falls through to the span around
      // it, which already answers with "no MCP named …".
      out.push(h("span", { class: "serves" + (m.known ? "" : " unknown"),
          title: m.known ? "MCP " + m.name + " is " + m.state : "no MCP named " + m.name },
        m.name,
        h("span", { class: "dot " + (m.known ? m.state : ""), title: m.known ? dotTitle(m.state) : undefined })));
    });
  }
  if (r.remark) out.push(" ", h("span", { class: "via" }, "· " + r.remark));
  return out;
}

/** One rule row as a NODE (docs/37 R5): names, routes and reasons are text, and the two
 *  action buttons carry data-act/data-more for #pane's delegated click (tunnels.js) — no
 *  per-render handlers to re-attach after a repaint. */
function ruleRowNode(r: ApiTunnelRuleRow): HTMLElement {
  const busy = tunBusyOf(r.id);
  const word = busy ? "starting" : r.state;
  const running = r.state === "up" || r.state === "starting" || r.state === "reconnecting";
  return h("div", { class: "tun-row", draggable: true, data: { rule: r.id } },
    h("span", { class: "dot " + word, data: { dot: "" }, title: dotTitle(word, null, r.reason) }),
    h("div", { class: "tun-main" },
      h("div", { class: "tun-name" }, r.name),
      h("div", { class: "tun-sub" }, ruleSubNode(r)),
      (r.state === "error" || r.state === "reconnecting")
        ? h("div", { class: "tun-err", data: { reason: "" } }, r.reason || "")
        : null),
    h("div", { class: "tun-acts" },
      // Start/Stop is hairline, not solid (docs/18 V5 + 17 §2.1: one solid accent per page,
      // and that is the page's New — a column of solid Starts is a column of shouting).
      h("button", { class: "btn", data: { act: running ? "stop" : "start" }, disabled: !!busy },
        busy ? "…" : running ? "Stop" : "Start"),
      h("button", { class: "btn ghost icon", data: { more: "" }, aria: { label: "Row actions" }, title: "Row actions" },
        iconNode("ellipsis"))));
}

/** A jump id -> its connection's name (docs/27 §4). The row carries the id; the panel
 *  resolves it because this list is the one place both ids and names live. Falls back to
 *  the raw id when the target is missing — deleting a jump in use is refused, so this is a
 *  stale-tab guard, and an id says more than an empty tag. */
function tunConnName(id: string): string {
  const hit = tunData().connections.find((c) => { return c.id === id; });
  return hit ? hit.name : id;
}

/** docs/27 §4: the transport tags a connection row carries. Going through a proxy or a
 *  jump is part of the row's identity ("this one dials through clash / through bastion"),
 *  so it rides the sub-line as the monochrome tag — the same form the sheet's Advanced
 *  summary uses, one word for the same fact in both places. */
function connBadgeNodes(c: ApiTunnelConnectionRow): HChild[] {
  const out: HChild[] = [];
  if (c.proxy) out.push(" ", h("span", { class: "tag" }, "proxy"));
  if (c.jump) out.push(" ", h("span", { class: "tag" }, "via " + tunConnName(c.jump)));
  return out;
}

/** One connection row as a NODE (docs/37 R5) — same contract as ruleRowNode: the host line
 *  and any failure reason are text, the Test and ellipsis buttons carry data-test/data-more
 *  for #pane's delegated click. */
function connRowNode(c: ApiTunnelConnectionRow): HTMLElement {
  const busy = tunBusyOf(c.id);
  const word = busy ? "starting" : c.state === "connected" ? "up" : c.state;
  return h("div", { class: "tun-row", draggable: true, data: { conn: c.id } },
    h("span", { class: "dot " + word, data: { dot: "" }, title: dotTitle(word, null, c.reason) }),
    h("div", { class: "tun-main" },
      h("div", { class: "tun-name" }, c.name),
      h("div", { class: "tun-sub" },
        c.host + ":" + c.port, " ",
        h("span", { class: "via" },
          "· " + c.username + " · " + c.authType +
          (c.ruleCount ? " · " + c.ruleCount + " rule" + (c.ruleCount === 1 ? "" : "s") : "")),
        connBadgeNodes(c)),
      c.reason ? h("div", { class: "tun-err", data: { reason: "" } }, c.reason) : null),
    h("div", { class: "tun-acts" },
      h("button", { class: "btn", data: { test: "" }, disabled: !!busy }, busy ? "…" : "Test"),
      h("button", { class: "btn ghost icon", data: { more: "" }, aria: { label: "Row actions" }, title: "Row actions" },
        iconNode("ellipsis"))));
}

export { connRowNode, isTunnelsView, jobDotClass, jobGroupsList, jobRowNode, jobsChipText, loadJobs, loadList, loadMemory, loadTunnels, mcpChipText, refreshMemoryNow, refreshNow, renderMemory, ruleRowNode, setView, tunConnName, tunData, tunGroupsList, tunRows, tunScope, updateCountChip };

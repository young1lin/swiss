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

                                                                                                                                        
import { $, api, apiJson, dotTitle, toast } from "./util.js";
import { tr, trn } from "./i18n.js";
import { h } from "./h.js";
                                     
import { currentPageCount, navigatePage, refreshPage } from "./page-registry.js";
import { patchSidebar } from "./menu.js";
import { patchDetailHead, renderPane } from "./pane.js";
import { rowOf } from "./sidebar.js";
import { currentView } from "./ui-state.js";
import { setTunResponse, tunBusyOf, tunDragging, tunResponse, mountedTunScope } from "./tunnel-state.js";
import { jobGroupNames, jobRows, setJobGroupNames, setJobRows } from "./job-state.js";
import { mcpDetail, mcpRows, memoryInfo, selectedMcp, setMcpDetail, setMcpGroups, setMcpRows, setMemoryInfo, setSelectedMcp } from "./mcp-state.js";
import { btn, dot, moreBtn, redacted, row, tag } from "./ui/index.js";
                                              


/* --- polling ---------------------------------------------------------------------------------- */
async function loadList()                {
  try {
    const r = await api("/api/mcps");
    if (!r.ok) { toast(tr("polling.httpN", { n: r.status }), true); return; }
    const j                     = await r.json();
    setMcpRows(j.mcps || []);
    setMcpGroups(j.groups || []);
    const sel = selectedMcp();
    if (sel && !rowOf(sel)) { setSelectedMcp(null); setMcpDetail(null); }
    patchSidebar();
    if (mcpDetail()) patchDetailHead();
    else renderPane(); // empty state; nothing here can hold user input
  } catch (e) { /* handled */ }
}

async function loadMemory(tree          )                {
  try {
    const r = await api("/api/memory" + (tree ? "?tree=1" : ""));
    if (!r.ok) return;
    setMemoryInfo(await r.json());
    renderMemory();
  } catch (e) { /* handled */ }
}

function renderMemory()       {
  const m = memoryInfo();
  if (!m) return;
  const chip = $("memChip");
  const total = m.childrenMb ? Math.round((m.gatewayMb + m.childrenMb) * 10) / 10 : m.gatewayMb;
  chip.textContent = m.processCount > 1
    ? tr("polling.nMbPProcs", { n: total, p: m.processCount })
    : tr("polling.nMb", { n: total });
  const lines = [tr("polling.gatewayRssNMb",
    { n: m.gatewayMb, a: m.heapUsedMb, b: m.heapTotalMb, c: m.externalMb })];
  if (m.childrenMb) lines.push(tr("polling.procMcpChildrenN", { n: m.childrenMb, p: m.processCount - 1 }));
  if (m.childrenPending) lines.push(tr("polling.childProcessesCouldMeasured"));
  lines.push(tr("polling.clickRefreshMemoryReading"));
  chip.title = lines.join("\n");
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
function refreshMemoryNow()       { void loadMemory(true); }

/** The panel's ONE explicit view refresh (the r key — the chip's click is memory-only, see
 *  refreshMemoryNow): re-read memory with the child walk, and reload the active view. On
 *  Data this key is the only manual reload there is — its poll deliberately leaves the
 *  paged-in lists alone, so without this the view could never be forced up to date. */
function refreshNow()       { void loadMemory(true); void refreshPage(); }

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

function tunData()                     {
  return tunResponse() || { connections: [], rules: [], ruleGroups: [], connGroups: [], mcps: [] };
}

/** Either tunnels page id — the render/patch guards and nothing else. */
function isTunnelsView(v               )          { return v === "tunnels" || v === "tunnel-forwards"; }

/** Which tunnel scope is on screen — the /api/groups/{scope} family's own word. The wire keys
 *  stay ruleGroups/connGroups (docs/20 §3); this maps the mounted page to its scope. */
function tunScope()                    { return mountedTunScope() === "conns" ? "conns" : "rules"; }
function tunRows()                                                { return mountedTunScope() === "conns" ? tunData().connections : tunData().rules; }
/* Both fall back to the default group on an empty or missing list, as the Remote, Tokens and
 * Secrets pages do: the groups are the whole list region now (no empty state takes it over),
 * so an answer without groups must still paint the one group every scope has. */
function tunGroupsList()           {
  const names = mountedTunScope() === "conns" ? tunData().connGroups : tunData().ruleGroups;
  return names && names.length ? names : ["default"];
}

/** The jobs scope's group names (docs/20 G4) — /api/jobs carries them at its top level. */
function jobGroupsList()           {
  const names = jobGroupNames();
  return names && names.length ? names : ["default"];
}

function setView(v        )                { return navigatePage(v); }

/** The MCP chip text — ONE builder (menu.js's patchSidebar reuses it). The two copies had
 *  drifted: patchSidebar counted "· N down" and this one did not, so the chip lost and regained
 *  that segment every 6s poll versus every view switch. */
function mcpChipText()         {
  let up = 0, bad = 0;
  mcpRows().forEach((m) => {
    if (m.state === "up") up++;
    else if (m.state === "down" || m.state === "error") bad++;
  });
  return bad
    ? tr("polling.nMcpsBadDown", { n: mcpRows().length, up, bad })
    : tr("polling.nMcps", { n: mcpRows().length, up });
}

function updateCountChip()       { $("countChip").textContent = currentPageCount(); }

/** `patchOnly` is what the poll passes: refresh the data, then patch rather than rebuild. */
async function loadTunnels(patchOnly          )                {
  const j = await apiJson                    ("/api/tunnels");
  if (!j) return;
  setTunResponse(j);
  if (isTunnelsView(currentView())) {
    const { patchTunnels, renderTunnels } = await import("./tunnels.js");
    if (tunDragging()) return; // a rebuild under the pointer would cancel the drag; patch later
    if (patchOnly && $("pane").querySelector("#tunGroups")) patchTunnels();
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

// The job row lives with the page that draws it (jobs.ts jobRowNode, docs/46 P6): its schedule
// column speaks the sheet's cron translator, which is jobs.ts's.

function jobsChipText()         {
  const rows = jobRows();
  const on = rows.filter((j) => { return j.enabled; }).length;
  const failing = rows.filter((j) => { return j.enabled && j.lastOk === false; }).length;
  return failing
    ? trn(rows.length, "polling.nJobsBadFailing.one", "polling.nJobsBadFailing.other", { on, bad: failing })
    : trn(rows.length, "polling.nJobs.one", "polling.nJobs.other", { on });
}

/** `patchOnly` is what the poll passes: refresh the data, then patch rather than rebuild —
 *  the same contract as loadTunnels. */
async function loadJobs(patchOnly          )                {
  const j = await apiJson                 ("/api/jobs");
  if (!j) return;
  setJobRows(j.jobs || []);
  setJobGroupNames(j.groups && j.groups.length ? j.groups : ["default"]);
  if (currentView() === "jobs") {
    const { patchJobs, renderJobs } = await import("./jobs.js");
    // patchJobs owns the patch-or-rebuild call (a structural change, or a pane another page
    // drew, is a rebuild). This used to look for the page foot first, and once the foot was gone
    // (docs/46 P6) every poll rebuilt the list - cancelling a drag, dropping the focus.
    if (patchOnly) patchJobs();
    else renderJobs();
  }
  updateCountChip();
}

/** A tunnel's state as a status dot (rule 10): up is green, a failure red, a tunnel on its way
 *  up (starting, reconnecting) the amber pulse, and a stopped one the plain grey dot. */
function tunDot(state        )           {
  if (state === "up" || state === "connected") return "up";
  if (state === "error" || state === "down") return "error";
  if (state === "starting" || state === "reconnecting") return "starting";
  if (state === "stopping") return "stopping";
  return "off";
}

/** `→ 127.0.0.1:18989 · via bastion · serves pg-app ●` — a rule's sub-line (docs/37 R5: every
 *  host, connection name and remark the server sends is a text node). The local port is the
 *  row's own column now (docs/46 §3.4), so the line starts at the target; the target is a value
 *  you would type, so mono. */
function ruleSubNode(r                  )           {
  const out           = ["→ ", redacted(r.targetHost, { suffix: ":" + r.targetPort }), " · ", tr("polling.conn", { conn: r.connectionName })];
  if (r.mcpRows && r.mcpRows.length) {
    out.push(" · ", tr("polling.serves"), " ");
    r.mcpRows.forEach((m, i) => {
      if (i) out.push(", ");
      // A known MCP's dot keeps its own one-word title (docs/18 V6); an unknown name paints
      // no state - the hover falls through to the span around it ("no MCP named …").
      out.push(h("span", { class: "serves" + (m.known ? "" : " unknown"),
          title: m.known ? tr("polling.mcpNameState", { name: m.name, state: m.state }) : tr("polling.mcpNamedName", { name: m.name }) },
        m.name,
        m.known ? dot(m.state            , dotTitle(m.state)) : dot("off", null)));
    });
  }
  if (r.remark) out.push(" · ", r.remark);
  return out;
}

/** One rule row: the library row (docs/46 §3.4). The dot, the name, the route; the local port
 *  in its own column (the value you point a client at); a failure as the row's one red line with
 *  the whole reason in its title, so a failing row is as tall as a healthy one. Start / Stop is
 *  the row's one word button - hairline, never the page's solid primary (docs/18 V5) - and the
 *  rest is behind ⋯. data-act / data-more answer #pane's delegated click (tunnels.ts). */
function ruleRowNode(r                  )              {
  const busy = tunBusyOf(r.id);
  const word = busy ? "starting" : r.state;
  const running = r.state === "up" || r.state === "starting" || r.state === "reconnecting";
  return row({
    lead: dot(tunDot(word), dotTitle(word, null, r.reason)),
    name: r.name,
    sub: ruleSubNode(r),
    err: ruleFailing(r) ? r.reason || undefined : undefined,
    cols: [{ v: String(r.localPort), mono: true, title: tr("tunnels.localPort") }],
    primary: btn(busy ? "…" : running ? tr("polling.stop") : tr("polling.start"), { data: { act: running ? "stop" : "start" }, disabled: !!busy }),
    more: moreBtn(tr("polling.rowActions"), { data: { more: "" } }),
    data: { rule: r.id },
    draggable: true,
  });
}

/** Whether a rule's reason is a failure worth the red line (a stopped rule's reason is not). */
function ruleFailing(r                  )          {
  return r.state === "error" || r.state === "reconnecting";
}

/** A jump id -> its connection's name (docs/27 §4). The row carries the id; the panel
 *  resolves it because this list is the one place both ids and names live. Falls back to
 *  the raw id when the target is missing — deleting a jump in use is refused, so this is a
 *  stale-tab guard, and an id says more than an empty tag. */
function tunConnName(id        )         {
  const hit = tunData().connections.find((c) => { return c.id === id; });
  return hit ? hit.name : id;
}

/** docs/27 §4: the transport tags a connection row carries. Going through a proxy or a
 *  jump is part of the row's identity ("this one dials through clash / through bastion"),
 *  so it rides the sub-line as the monochrome tag — the same form the sheet's Advanced
 *  summary uses, one word for the same fact in both places. */
function connBadgeNodes(c                        )           {
  const out           = [];
  if (c.proxy) out.push(" ", tag(tr("polling.proxy")));
  if (c.jump) out.push(" ", tag(tr("polling.conn", { conn: tunConnName(c.jump) })));
  return out;
}

/** One connection row: the library row (docs/46 §3.4). The host (a value, mono, masked until its
 *  eye is pressed - redacted(), 2026-09-28), who it signs in
 *  as and how, the transport tags; how many rules ride it is the row's column now, not a clause
 *  in the sub-line; a failure is the one red line. Test is the row's one word button. */
function connRowNode(c                        )              {
  const busy = tunBusyOf(c.id);
  const word = busy ? "starting" : c.state;
  return row({
    lead: dot(tunDot(word), dotTitle(word === "connected" ? "up" : word, null, c.reason)),
    name: c.name,
    sub: [redacted(c.host, { suffix: ":" + c.port }), " · ", tr("polling.userAuth", { user: c.username, auth: c.authType }), connBadgeNodes(c)],
    err: c.reason || undefined,
    cols: [trn(c.ruleCount || 0, "tunnels.nRules.one", "tunnels.nRules.other")],
    primary: btn(busy ? "…" : tr("polling.test"), { data: { test: "" }, disabled: !!busy }),
    more: moreBtn(tr("polling.rowActions"), { data: { more: "" } }),
    data: { conn: c.id },
    draggable: true,
  });
}


export { connRowNode, isTunnelsView, tunDot, jobGroupsList, jobsChipText, loadJobs, loadList, loadMemory, loadTunnels, mcpChipText, refreshMemoryNow, refreshNow, renderMemory, ruleRowNode, setView, tunConnName, tunData, tunGroupsList, tunRows, tunScope, updateCountChip };
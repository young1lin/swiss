import { $, DEFAULT_GROUP, api, apiJson, dotTitle, esc, icon, state, toast, whenLabel } from "./util.js";
import { currentPageCount, navigatePage, refreshPage } from "./page-registry.js";
import { patchSidebar } from "./menu.js";
import { patchDetailHead, renderPane } from "./pane.js";
import { rowOf } from "./sidebar.js";
import { triggerSummary } from "./jobs-v2.js";


/* --- polling ---------------------------------------------------------------------------------- */
async function loadList() {
  try {
    var r = await api("/api/mcps");
    if (!r.ok) { toast("HTTP " + r.status, true); return; }
    var j = await r.json();
    state.mcps = j.mcps || [];
    state.groups = j.groups || [];
    if (state.selected && !rowOf(state.selected)) { state.selected = null; state.detail = null; }
    patchSidebar();
    if (state.detail) patchDetailHead();
    else renderPane(); // empty state; nothing here can hold user input
  } catch (e) { /* handled */ }
}

async function loadMemory(tree) {
  try {
    var r = await api("/api/memory" + (tree ? "?tree=1" : ""));
    if (!r.ok) return;
    state.mem = await r.json();
    renderMemory();
  } catch (e) { /* handled */ }
}

function renderMemory() {
  var m = state.mem;
  if (!m) return;
  var chip = $("memChip");
  var total = m.childrenMb ? Math.round((m.gatewayMb + m.childrenMb) * 10) / 10 : m.gatewayMb;
  chip.textContent = total + " MB" + (m.processCount > 1 ? " · " + m.processCount + " procs" : "");
  chip.title = "gateway RSS " + m.gatewayMb + " MB (heap " + m.heapUsedMb + "/" + m.heapTotalMb +
    " MB, external " + m.externalMb + " MB)" +
    (m.childrenMb ? "\nproc-MCP children " + m.childrenMb + " MB across " + (m.processCount - 1) + " processes" : "") +
    (m.childrenPending ? "\nChild processes could not be measured — click to retry." : "") +
    "\nClick or press r — refreshes memory and the current view.";
  // The chip is the toolbar's refresh control now (the button is gone): a control on every
  // paint, not only the old childrenPending retry — this function re-runs every 6s poll and
  // would otherwise strip the class and the click with the first one.
  chip.className = "chip button";
  chip.onclick = refreshNow;
}

/** The panel's ONE explicit refresh (the toolbar button is gone — the memory chip and the r
 *  key both land here): re-read memory with the child walk, and reload the active view. On
 *  Data this click is the only manual reload there is — its poll deliberately leaves the
 *  paged-in lists alone, so without this the view could never be forced up to date. */
function refreshNow() { loadMemory(true); void refreshPage(); }

/* ================================================================================================
   Tunnels
   ------------------------------------------------------------------------------------------------
   Same rendering contract as the MCP side: renderTunnels() rebuilds the pane on an explicit action,
   and the 6s poll may only call patchTunnels(), which touches dots, state text, button labels and
   the footer. Every editable field lives in a sheet (#sheet, a separate subtree), so a poll can
   never wipe something being typed — but a poll that rebuilt the list would still steal focus from
   a keyboard user mid-tab, which is why patching exists at all.
   ================================================================================================ */

function tunData() {
  return state.tun.data || { connections: [], rules: [], ruleGroups: [], connGroups: [], mcps: [] };
}

/** Which tunnel list is on screen — the group/order APIs take this same word. */
function tunKind() { return state.tun.tab === "conns" ? "connections" : "rules"; }
function tunRows() { return state.tun.tab === "conns" ? tunData().connections : tunData().rules; }
function tunGroupsList() {
  return state.tun.tab === "conns" ? tunData().connGroups || [] : tunData().ruleGroups || [];
}
function tunGroupOf(r) { return r.group || DEFAULT_GROUP; }

/** The list's shape: the implicit default group first, then the stored groups in their order. */
function tunGrouped() {
  var byGroup = {};
  tunRows().forEach(function (r) {
    var g = tunGroupOf(r);
    (byGroup[g] = byGroup[g] || []).push(r);
  });
  var out = [{ name: DEFAULT_GROUP, rows: byGroup[DEFAULT_GROUP] || [] }];
  tunGroupsList().forEach(function (g) { out.push({ name: g, rows: byGroup[g] || [] }); });
  return out;
}

function setView(v) { return navigatePage(v); }

/** The MCP chip text — ONE builder (menu.js's patchSidebar reuses it). The two copies had
 *  drifted: patchSidebar counted "· N down" and this one did not, so the chip lost and regained
 *  that segment every 6s poll versus every view switch. */
function mcpChipText() {
  var up = 0, bad = 0;
  state.mcps.forEach(function (m) {
    if (m.state === "up") up++;
    else if (m.state === "down" || m.state === "error") bad++;
  });
  return state.mcps.length + " MCPs · " + up + " up" + (bad ? " · " + bad + " down" : "");
}

function updateCountChip() { $("countChip").textContent = currentPageCount(); }

/** `patchOnly` is what the poll passes: refresh the data, then patch rather than rebuild. */
async function loadTunnels(patchOnly) {
  var j = await apiJson("/api/tunnels");
  if (!j) return;
  state.tun.data = j;
  if (state.view === "tunnels") {
    var { patchTunnels, renderTunnels } = await import("./tunnels.js");
    if (state.tun.dragging) return; // a rebuild under the pointer would cancel the drag; patch later
    if (patchOnly && $("pane").querySelector(".tun-foot")) patchTunnels();
    else renderTunnels();
  }
  updateCountChip();
}

async function tunTab(t) {
  state.tun.tab = t;
  var { renderTunnels } = await import("./tunnels.js");
  if (state.view === "tunnels") renderTunnels();
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
function jobDotClass(j) {
  if (j.running) return "starting";
  if (!j.enabled) return "idle";
  if (j.lastOk === false) return "down";
  return j.lastRunAt ? "up" : "idle";
}

function jobRowHtml(j) {
  var busy = state.jobs.busy[j.name];
  // The v2 identity (docs/11 §7.1): the title is the human name when one is set, the id
  // stays beside it because every action still addresses the id.
  var title = j.title && j.title !== j.name ? esc(j.title) + ' <span class="via">· ' + esc(j.name) + "</span>" : esc(j.name);
  var labels = (j.labels || []).length
    ? ' <span class="via">' + j.labels.map(function (l) { return "#" + esc(l); }).join(" ") + "</span>"
    : "";
  // data-last/data-next always render (possibly empty) so patchJobs can always fill them in.
  var word = busy ? "starting" : jobDotClass(j);
  return '<div class="tun-row" data-job="' + esc(j.name) + '">' +
      '<span class="dot ' + esc(word) + '" data-dot title="' + esc(dotTitle(word)) + '"></span>' +
      '<div class="tun-main">' +
        '<div class="tun-name">' + title + labels + (j.enabled ? "" : ' <span class="via">· off</span>') + "</div>" +
        '<div class="tun-sub"><code>' + esc(j.command) + "</code>" +
          ' <span class="via">· ' + esc(triggerSummary(j)) + "</span>" +
          ' <span class="via" data-last>' + (j.lastRunAt
            ? "· last " + esc(whenLabel(j.lastRunAt)) + (j.lastOk === false ? " · failed" : "")
            : "") + "</span>" +
          ' <span class="via" data-next>' + (j.enabled && j.nextDueAt ? "· next " + esc(whenLabel(j.nextDueAt)) : "") + "</span>" +
        "</div>" +
      "</div>" +
      '<div class="tun-acts">' +
        '<button class="btn" data-run' + (busy || j.running ? " disabled" : "") + ">" + (busy ? "…" : "Run now") + "</button>" +
        // One primary per row (docs/18 V5): Edit, History and Delete answer from the ellipsis
        // menu (jobs.js), so Delete is not a red button repeated down the whole list.
        '<button class="btn ghost icon" data-more aria-label="Row actions" title="Row actions">' + icon("ellipsis") + "</button>" +
      "</div>" +
    "</div>";
}

function jobsChipText() {
  var rows = state.jobs.data;
  var on = rows.filter(function (j) { return j.enabled; }).length;
  var failing = rows.filter(function (j) { return j.enabled && j.lastOk === false; }).length;
  return rows.length + (rows.length === 1 ? " job" : " jobs") + " · " + on + " on" +
    (failing ? " · " + failing + " failing" : "");
}

/** `patchOnly` is what the poll passes: refresh the data, then patch rather than rebuild —
 *  the same contract as loadTunnels. */
async function loadJobs(patchOnly) {
  var j = await apiJson("/api/jobs");
  if (!j) return;
  state.jobs.data = j.jobs || [];
  if (state.view === "jobs") {
    var { patchJobs, renderJobs } = await import("./jobs.js");
    if (patchOnly && $("pane").querySelector("[data-foot]")) patchJobs();
    else renderJobs();
  }
  updateCountChip();
}

/** `18989 → 127.0.0.1:18989   via bastion · serves pg-app ●` */
function ruleSubHtml(r) {
  var out = esc(String(r.localPort)) + " &rarr; " + esc(r.targetHost + ":" + r.targetPort);
  out += ' <span class="via">via ' + esc(r.connectionName) + "</span>";
  if (r.mcpRows && r.mcpRows.length) {
    out += ' <span class="via">· serves </span>' + r.mcpRows.map(function (m) {
      // A known MCP's dot keeps its own one-word title (docs/18 V6); an unknown name paints
      // no state, so it takes no title either — the hover falls through to the span around
      // it, which already answers with "no MCP named …".
      return '<span class="serves' + (m.known ? "" : " unknown") + '" title="' +
        esc(m.known ? "MCP " + m.name + " is " + m.state : "no MCP named " + m.name) + '">' +
        esc(m.name) + '<span class="dot ' + esc(m.known ? m.state : "") + '"' +
        (m.known ? ' title="' + esc(dotTitle(m.state)) + '"' : "") + "></span></span>";
    }).join(", ");
  }
  if (r.remark) out += ' <span class="via">· ' + esc(r.remark) + "</span>";
  return out;
}

function ruleRowHtml(r) {
  var busy = state.tun.busy[r.id];
  var word = busy ? "starting" : r.state;
  var running = r.state === "up" || r.state === "starting" || r.state === "reconnecting";
  return '<div class="tun-row" draggable="true" data-rule="' + esc(r.id) + '">' +
      '<span class="dot ' + esc(word) + '" data-dot title="' + esc(dotTitle(word, null, r.reason)) + '"></span>' +
      '<div class="tun-main">' +
        '<div class="tun-name">' + esc(r.name) + "</div>" +
        '<div class="tun-sub">' + ruleSubHtml(r) + "</div>" +
        (r.state === "error" || r.state === "reconnecting"
          ? '<div class="tun-err" data-reason>' + esc(r.reason || "") + "</div>" : "") +
      "</div>" +
      '<div class="tun-acts">' +
        // Start/Stop is hairline, not solid (docs/18 V5 + 17 §2.1: one solid accent per page,
        // and that is the page's New — a column of solid Starts is a column of shouting).
        '<button class="btn" data-act="' + (running ? "stop" : "start") +
          '"' + (busy ? " disabled" : "") + ">" + (busy ? "…" : running ? "Stop" : "Start") + "</button>" +
        '<button class="btn ghost icon" data-more aria-label="Row actions" title="Row actions">' + icon("ellipsis") + "</button>" +
      "</div>" +
    "</div>";
}

function connRowHtml(c) {
  var busy = state.tun.busy[c.id];
  var word = busy ? "starting" : c.state === "connected" ? "up" : c.state;
  return '<div class="tun-row" draggable="true" data-conn="' + esc(c.id) + '">' +
      '<span class="dot ' + esc(word) + '" data-dot title="' + esc(dotTitle(word, null, c.reason)) + '"></span>' +
      '<div class="tun-main">' +
        '<div class="tun-name">' + esc(c.name) + "</div>" +
        '<div class="tun-sub">' + esc(c.host + ":" + c.port) + ' <span class="via">· ' + esc(c.username) +
          " · " + esc(c.authType) + (c.ruleCount ? " · " + c.ruleCount + " rule" + (c.ruleCount === 1 ? "" : "s") : "") +
          "</span></div>" +
        (c.reason ? '<div class="tun-err" data-reason>' + esc(c.reason) + "</div>" : "") +
      "</div>" +
      '<div class="tun-acts">' +
        '<button class="btn" data-test' + (busy ? " disabled" : "") + ">" + (busy ? "…" : "Test") + "</button>" +
        '<button class="btn ghost icon" data-more aria-label="Row actions" title="Row actions">' + icon("ellipsis") + "</button>" +
      "</div>" +
    "</div>";
}

export { connRowHtml, jobDotClass, jobRowHtml, jobsChipText, loadJobs, loadList, loadMemory, loadTunnels, mcpChipText, refreshNow, renderMemory, ruleRowHtml, ruleSubHtml, setView, tunData, tunGroupOf, tunGrouped, tunGroupsList, tunKind, tunRows, tunTab, updateCountChip };

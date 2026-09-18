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

import { $, api, apiJson, dotTitle, emptyHtml, esc, state, toast } from "./util.js";
import { copyText } from "./connect.js";
import { popupMenu } from "./menu.js";
import { assignMember, groupOf as makeGroupOf, mountGroup, newGroupFlow, saveOrder, slice } from "./groups.js";
import { connRowHtml, isTunnelsView, loadList, loadTunnels, ruleRowHtml, tunData, tunGroupsList, tunRows, tunScope } from "./polling.js";
import { openConnSheet, openRuleSheet } from "./tunnel-sheets.js";

/* --- tunnels: groups and drag-to-reorder --------------------------------------------------------
   The sidebar's model, shared through the groups component (docs/20 §4): one flat order per
   list (array order in tunnels.json), a group name on each row, and a stored list of group
   names — default included, as an ordinary name now. Dragging onto a row re-orders and re-homes
   in one gesture; dropping on a header appends to that group — the only way into an empty one. */

/** The rendering group of a tunnel row: same one rule as everywhere else — the stored group
 *  while it exists, else the first group. */
function tunGroupOfRow(r) {
  return makeGroupOf(tunGroupsList())(r);
}

/** Persist the current order of the list on screen. The family's order route is per-scope,
 *  so only the tab being dragged over is sent. */
function saveTunOrder() {
  void saveOrder(tunScope(), tunRows().map(function (r) { return r.id; }));
}

/** Move one row to just before/after another in its list, re-render, persist. */
function moveTunRow(id, target, before) {
  if (!id || !target || id === target) return;
  var rows = tunRows();
  var item = rows.filter(function (r) { return r.id === id; })[0];
  if (!item) return;
  var to = rows.findIndex(function (r) { return r.id === target; });
  if (to < 0) return; // target vanished mid-drag — leave everything where it is
  rows.splice(rows.indexOf(item), 1);
  rows.splice(before ? to : to + 1, 0, item);
  renderTunnels();
  saveTunOrder();
}

/** Put one row in a group. Applied locally first so the row jumps immediately, then persisted
 *  — a reject takes the server's word for it. Scope is the family's own word ("conns"|"rules"). */
async function assignTunScoped(scope, id, group) {
  var rows = scope === "rules" ? tunData().rules : tunData().connections;
  var row = rows.filter(function (r) { return r.id === id; })[0];
  if (!row) return;
  var names = scope === "rules" ? tunData().ruleGroups : tunData().connGroups;
  if (makeGroupOf(names || [])(row) === (group || (names || [])[0])) return;
  row.group = group;
  renderTunnels();
  var j = await assignMember(scope, id, group);
  if (!j) { await loadTunnels(); return; }
  row.group = j.group; // the canonical name the server stored
  renderTunnels();
}

/* --- the page -----------------------------------------------------------------------------------
   The tunnels plugin contributes TWO L2 pages (docs/13 D5, as revised): the Context Bar's
   "Tunnels / SSH Connections ▾" and "Tunnels / Port Forwards ▾" switch between them, and the
   mounted page decides which scope this module renders (state.tun.tab, set by the page's
   mount). The body header is the page's TASK, not its location: one line of scope prose and
   the scope's actions on the right — no repeated "Tunnels" title, no second copy of the page
   switcher. */

/** The scope sentence under the bar: what THIS page operates on, never where we are (the
 *  context bar owns location). */
function tunDescHtml(isConns) {
  return isConns
    ? "SSH hosts this gateway can forward ports over. Test one before pointing a rule at it."
    : "Local ports forwarded over an SSH connection. A local port stays bound only while its tunnel can carry traffic.";
}

/** The one count builder: the context bar's chip AND the page footer read the same words,
 *  or the two would drift apart between polls (the mcpChipText lesson). */
function tunnelsCountText(scope) {
  var d = tunData();
  if (scope === "rules") {
    var active = d.rules.filter(function (r) { return r.state === "up"; }).length;
    return d.rules.length + " rule" + (d.rules.length === 1 ? "" : "s") + ", " + active + " active";
  }
  var connected = d.connections.filter(function (c) { return c.state === "connected"; }).length;
  return d.connections.length + " connection" + (d.connections.length === 1 ? "" : "s") + ", " + connected + " connected";
}

function renderTunnels() {
  var d = tunData();
  var isConns = state.tun.tab === "conns";
  // The page's actions, right-aligned in the body header (pane-actions is the panel's own
  // vocabulary for exactly this slot). Rules carry the bulk start/stop pair; connections
  // carry only New — Test lives on each row.
  var acts =
    '<button class="btn primary" id="' + (isConns ? "tNewConn" : "tNewRule") + '">New</button>' +
    '<button class="btn" id="tNewGroup">New group</button>';
  if (!isConns) {
    acts += '<button class="btn" id="tStartAll">Start all</button>' +
      '<button class="btn" id="tStopAll">Stop all</button>';
  }

  // One group per slice — the component owns the header band, the indent and the empty line
  // now (docs/20 §4.1). Empty groups keep their place: that is how you drag the first row into
  // one (or use its +).
  var cfg = tunCfg();
  var grouped = slice(tunRows(), tunGroupsList(), tunGroupOfRow);
  var list = isConns ? d.connections : d.rules;
  var foot = tunnelsCountText(isConns ? "conns" : "rules");

  // .wide for the same reason as the traffic log: a rule row is name + route + who it serves +
  // three buttons. .pane-desc keeps its own 60ch cap, so the prose does not stretch with it.
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><div class="pane-desc">' + tunDescHtml(isConns) + "</div></div>" +
      '<div class="pane-actions">' + acts + "</div></div>" +
    '<div id="tunGroups"></div>' +
    '<div class="tun-foot">' + esc(foot) + "</div>" +
  "</div>";
  var host = $("tunGroups");
  if (list.length) grouped.forEach(function (g) { host.appendChild(mountGroup(cfg, g)); });
  else host.innerHTML = isConns
    ? emptyHtml({ icon: "plug", title: "No SSH connections", hint: "Add one with New, then point a forwarding rule at it." })
    : emptyHtml({ icon: "plug", title: "No forwarding rules", hint: "Add one with New. Each rule binds a local port and forwards it over SSH." });
  wireTunnels();
}

/** The conns/rules scope's cfg for mountGroup (see groups.js for the full contract). Built
 *  fresh each render so names and the tab's fold map are always the live objects. */
function tunCfg() {
  return {
    scope: tunScope(),
    density: "page",
    names: tunGroupsList(),
    collapsed: state.tun.collapsed[state.tun.tab] || {},
    noun: "row",
    addTitle: function (g) {
      return (state.tun.tab === "conns" ? "Add an SSH connection to " : "Add a forwarding rule to ") + g;
    },
    onAdd: function (g) {
      state.tun.pendingGroup = g; // a real name now — the sheet's save lands the row in it
      if (state.tun.tab === "conns") openConnSheet(null); else openRuleSheet(null);
    },
    reload: function () { return loadTunnels(); },
    render: renderTunnels,
    afterDrag: function () { renderTunnels(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: function () { return state.tun.dragging; },
      set: function (v) { state.tun.dragging = v; },
    },
    dragGroup: {
      get: function () { return state.tun.draggingGroup; },
      set: function (v) { state.tun.draggingGroup = v; },
    },
    rowId: function (r) { return r.id; },
    rowSel: function (r) {
      var v = window.CSS && CSS.escape ? CSS.escape(r.id) : r.id;
      return state.tun.tab === "conns" ? '[data-conn="' + v + '"]' : '[data-rule="' + v + '"]';
    },
    rowsById: tunRows,
    groupOfRow: tunGroupOfRow,
    rowsHtml: function (g) {
      return g.rows.map(state.tun.tab === "conns" ? connRowHtml : ruleRowHtml).join("");
    },
    onMoveRow: moveTunRow,
    onAssign: function (id, g) { void assignTunScoped(tunScope(), id, g); },
  };
}

/** Poll-safe update: dots, reasons, button labels and the footer. Never structure. */
function patchTunnels() {
  if (!isTunnelsView(state.view)) return;
  var d = tunData();
  var rows = state.tun.tab === "conns" ? d.connections : d.rules;
  var attr = state.tun.tab === "conns" ? "data-conn" : "data-rule";
  var nodes = $("pane").querySelectorAll("[" + attr + "]");
  // A row appeared or vanished (another tab, or a reconnect that deleted nothing) — structure
  // changed, so a patch cannot express it. Never mid-drag: a rebuild there cancels the gesture.
  if (nodes.length !== rows.length) {
    if (!state.tun.dragging && !state.tun.draggingGroup) renderTunnels();
    return;
  }
  for (var i = 0; i < rows.length; i++) {
    var row = rows[i];
    var node = $("pane").querySelector("[" + attr + '="' + (window.CSS && CSS.escape ? CSS.escape(row.id) : row.id) + '"]');
    if (!node) { if (!state.tun.dragging && !state.tun.draggingGroup) renderTunnels(); return; }
    var busy = state.tun.busy[row.id];
    var dot = node.querySelector("[data-dot]");
    var live = state.tun.tab === "conns" ? (row.state === "connected" ? "up" : row.state) : row.state;
    // Title and class move together (docs/18 V6): the poll only patches, and a dot whose
    // class moved but whose title stayed would keep explaining the previous state.
    if (dot) { dot.className = "dot " + (busy ? "starting" : live); dot.title = dotTitle(busy ? "starting" : live, null, row.reason); }
    var reason = node.querySelector("[data-reason]");
    if (reason && reason.textContent !== (row.reason || "")) reason.textContent = row.reason || "";
    var act = node.querySelector("[data-act]");
    if (act) {
      var running = row.state === "up" || row.state === "starting" || row.state === "reconnecting";
      var label = busy ? "…" : running ? "Stop" : "Start";
      if (act.textContent !== label) {
        act.textContent = label;
        act.className = "btn" + (running ? "" : " primary");
        act.dataset.act = running ? "stop" : "start";
      }
      act.disabled = !!busy;
    }
  }
  // Group counts move with rows: a count that only refreshed on rebuild would disagree with
  // the patched dots beside it for the rest of the poll.
  Array.prototype.forEach.call($("pane").querySelectorAll("[data-group]"), function (grp) {
    var n = rows.filter(function (r) { return tunGroupOfRow(r) === grp.dataset.group; }).length;
    var badge = grp.querySelector(".grp-n");
    if (badge) badge.textContent = String(n);
  });
  var foot = $("pane").querySelector(".tun-foot");
  if (foot) foot.textContent = tunnelsCountText(state.tun.tab === "conns" ? "conns" : "rules");
}

function wireTunnels() {
  var pane = $("pane");
  // The top New carries no group promise; the sheet falls back to the scope's last-used.
  if ($("tNewConn")) $("tNewConn").onclick = function () { openConnSheet(null); };
  if ($("tNewRule")) $("tNewRule").onclick = function () { openRuleSheet(null); };
  if ($("tNewGroup")) $("tNewGroup").onclick = function () {
    newGroupFlow(tunScope(), tunGroupsList(), function () { return loadTunnels(); });
  };
  if ($("tStartAll")) $("tStartAll").onclick = startAllRules;
  if ($("tStopAll")) $("tStopAll").onclick = function () { stopAllRules(false); };

  Array.prototype.forEach.call(pane.querySelectorAll("[data-rule]"), function (node) {
    var id = node.dataset.rule;
    var rule = tunData().rules.filter(function (r) { return r.id === id; })[0];
    var act = node.querySelector("[data-act]");
    if (act) act.onclick = function () { ruleAct(id, act.dataset.act); };
    var ruleMore = node.querySelector("[data-more]");
    if (ruleMore) ruleMore.onclick = function (ev) {
      // The overflow half of the row (docs/18 V5). Force free appears only when a port is
      // actually held — it is a remedy, not a standing action. stopPropagation first:
      // connect.js closes open menus on clicks that reach document (the group-head menu
      // above does the same).
      ev.stopPropagation();
      var items = [
        { label: "Edit", fn: function () { openRuleSheet(rule); } },
        { label: "Copy local port", fn: function () { copyText(String(rule.localPort), "Local port"); } },
      ];
      if (rule.portOwner) items.push({ label: "Force free " + rule.localPort, fn: function () { forceFreePort(rule.localPort, id); } });
      items.push({ sep: true }, { label: "Delete", danger: true, fn: function () { deleteRule(rule, false); } });
      popupMenu(ruleMore.getBoundingClientRect(), items);
    };
  });
  Array.prototype.forEach.call(pane.querySelectorAll("[data-conn]"), function (node) {
    var id = node.dataset.conn;
    var conn = tunData().connections.filter(function (c) { return c.id === id; })[0];
    node.querySelector("[data-test]").onclick = function () { testConn(id); };
    var connMore = node.querySelector("[data-more]");
    if (connMore) connMore.onclick = function (ev) {
      // Same as the rule rows above: the opening click must not reach document.
      ev.stopPropagation();
      popupMenu(connMore.getBoundingClientRect(), [
        { label: "Edit", fn: function () { openConnSheet(conn); } },
        { label: "Copy host", fn: function () { copyText(conn.host + ":" + conn.port, "Host"); } },
        { sep: true },
        { label: "Delete", danger: true, fn: function () { deleteConn(conn); } },
      ]);
    };
  });
}

async function withTunBusy(id, verb, fn) {
  if (state.tun.busy[id]) return null;
  state.tun.busy[id] = verb;
  patchTunnels();
  try {
    return await fn();
  } finally {
    delete state.tun.busy[id];
    await loadTunnels();
  }
}

/* --- rule actions ----------------------------------------------------------------------------- */

async function ruleAct(id, verb) {
  await withTunBusy(id, verb, async function () {
    var r = await api("/api/tunnels/rules/" + encodeURIComponent(id) + "/" + verb, { method: "POST" });
    var j = await r.json().catch(function () { return {}; });
    // 409 means an MCP is using this tunnel. Tell the user who, then obey them.
    if (r.status === 409 && j.confirmRequired) {
      if (!confirm(j.dependents.join(", ") + " depend" + (j.dependents.length === 1 ? "s" : "") +
          " on this tunnel.\n\nStop it anyway?")) return;
      var forced = await apiJson("/api/tunnels/rules/" + encodeURIComponent(id) + "/stop?force=1", { method: "POST" });
      if (forced) toast("Stopped");
      return;
    }
    if (!r.ok) { toast(j.error || "HTTP " + r.status, true); return; }
    if (j.ok === false) { toast(j.error || "Start failed", true); return; }
    toast(verb === "start" ? "Started" : "Stopped");
  });
}

async function startAllRules() {
  var j = await apiJson("/api/tunnels/start-all", { method: "POST" });
  await loadTunnels();
  if (!j) return;
  var failed = (j.results || []).filter(function (x) { return !x.ok; });
  toast(failed.length
    ? (j.results.length - failed.length) + " started, " + failed.length + " failed: " + failed[0].name + " — " + failed[0].error
    : j.results.length + " tunnels started", failed.length > 0);
}

async function stopAllRules(force) {
  var r = await api("/api/tunnels/stop-all" + (force ? "?force=1" : ""), { method: "POST" });
  var j = await r.json().catch(function () { return {}; });
  if (r.status === 409 && j.confirmRequired) {
    if (!confirm("These MCPs are using tunnels you are about to stop:\n\n" + j.dependents.join(", ") +
        "\n\nStop them anyway?")) return;
    return stopAllRules(true);
  }
  await loadTunnels();
  if (!r.ok) { toast(j.error || "HTTP " + r.status, true); return; }
  toast((j.results || []).length + " tunnels stopped");
}

async function deleteRule(rule, force) {
  if (!force && !confirm('Delete forwarding rule "' + rule.name + '"?')) return;
  var r = await api("/api/tunnels/rules/" + encodeURIComponent(rule.id) + (force ? "?force=1" : ""), { method: "DELETE" });
  var j = await r.json().catch(function () { return {}; });
  if (r.status === 409 && j.confirmRequired) {
    if (!confirm(j.dependents.join(", ") + " depend on this tunnel.\n\nDelete it anyway?")) return;
    return deleteRule(rule, true);
  }
  await loadTunnels();
  if (!r.ok) { toast(j.error || "HTTP " + r.status, true); return; }
  toast("Deleted " + rule.name);
}

/** Kill whatever holds a local port. Confirmed here because it can kill a process doing real work. */
async function forceFreePort(port, ruleId) {
  var d = tunData();
  var rule = d.rules.filter(function (r) { return r.id === ruleId; })[0];
  var owner = rule && rule.portOwner;
  if (!confirm("Port " + port + " is held by pid " + (owner ? owner.pid + " (" + owner.name + ")" : "?") +
      ".\n\nForce-kill that process? It may be doing real work.")) return;
  var j = await apiJson("/api/tunnels/port/" + port + "/free", { method: "POST" });
  if (!j) { await loadTunnels(); return; }
  toast("Killed pid " + j.killed.pid + " (" + j.killed.name + ")");
  await ruleAct(ruleId, "start");
}

/* --- connection actions ------------------------------------------------------------------------ */

async function testConn(id) {
  await withTunBusy(id, "test", async function () {
    var j = await apiJson("/api/tunnels/connections/" + encodeURIComponent(id) + "/test", { method: "POST" });
    if (!j) return;
    if (j.ok) { toast("Connected in " + j.ms + " ms" + (j.banner ? " — " + j.banner : "")); return; }
    // A changed host key is the one failure with an action attached.
    if (j.kind === "hostkey" && j.fingerprint) {
      if (confirm(j.error + "\n\nTrust the new key?")) {
        var t = await apiJson("/api/tunnels/connections/" + encodeURIComponent(id) + "/trust", { method: "POST" });
        if (t) toast("Host key trusted — test again");
      }
      return;
    }
    toast(j.error + " (" + j.kind + ")", true);
  });
}

async function deleteConn(conn) {
  if (!confirm('Delete SSH connection "' + conn.name + '"?')) return;
  var j = await apiJson("/api/tunnels/connections/" + encodeURIComponent(conn.id), { method: "DELETE" });
  await loadTunnels();
  if (j) toast("Deleted " + conn.name);
}

/* --- the two L2 pages' shared lifecycle -----------------------------------------------------------
   views/tunnels.js (#tunnels, SSH Connections) and views/tunnel-forwards.js
   (#tunnel-forwards, Port Forwards) are thin modules over this one: same data, same
   rendering, same polling — only the scope differs, and each mount states its own. No
   business logic is duplicated on either side of the pair. */

/** Mount one tunnels page: pin the scope, then load. Scope is the /api/groups family's own
 *  word ("conns"|"rules"); everything in this module reads it through state.tun.tab. */
async function mountTunnelsPage(scope) {
  state.tun.tab = scope === "rules" ? "rules" : "conns";
  return loadTunnels();
}

function refreshTunnelsPage() { return loadTunnels(); }

async function pollTunnelsPage() {
  await loadList();
  await loadTunnels(true);
}

function unmountTunnelsPage() {
  state.tun.data = null;
  state.tun.keys = null;
  state.tun.dragging = null;
}

export { assignTunScoped, deleteConn, deleteRule, forceFreePort, mountTunnelsPage, moveTunRow, patchTunnels, pollTunnelsPage, refreshTunnelsPage, renderTunnels, ruleAct, saveTunOrder, startAllRules, stopAllRules, testConn, tunCfg, tunnelsCountText, unmountTunnelsPage, wireTunnels, withTunBusy };

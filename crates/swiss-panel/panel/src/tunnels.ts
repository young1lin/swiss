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

import type { ApiTunnelConnectionRow, ApiTunnelRuleRow } from "./types/api.js";
import type { GroupCfg, GroupSlice, MenuItem } from "./types/dom.js";
import { $, api, apiJson, dotTitle, targetEl, toast } from "./util.js";
import { fill, h } from "./h.js";
import type { HChild } from "./h.js";
import { copyText } from "./connect.js";
import { assignMember, groupOf as makeGroupOf, mountGroup, newGroupFlow, saveOrder, slice } from "./groups.js";
import { connRowNode, isTunnelsView, loadList, loadTunnels, ruleRowNode, tunData, tunDot, tunGroupsList, tunRows, tunScope } from "./polling.js";
import { openConnSheet, openRuleSheet } from "./tunnel-sheets.js";
import { currentView } from "./ui-state.js";
import { clearTunBusy, clearTunView, setTunBusy, setTunDragging, setTunDraggingGroup, setTunPendingGroup, setMountedTunScope, tunBusyOf, tunDragging, tunDraggingGroup, tunFolds, mountedTunScope } from "./tunnel-state.js";
import { tr, trn } from "./i18n.js";
import { btn, closeMenu, dot, iconBtn, menuOpen, moreBtn, paneBody, paneHead, popupMenu } from "./ui/index.js";

/* --- tunnels: groups and drag-to-reorder --------------------------------------------------------
   The sidebar's model, shared through the groups component (docs/20 §4): one flat order per
   list (array order in tunnels.json), a group name on each row, and a stored list of group
   names — default included, as an ordinary name now. Dragging onto a row re-orders and re-homes
   in one gesture; dropping on a header appends to that group — the only way into an empty one. */

/** The rendering group of a tunnel row: same one rule as everywhere else — the stored group
 *  while it exists, else the first group. */
function tunGroupOfRow(r: ApiTunnelConnectionRow | ApiTunnelRuleRow): string {
  return makeGroupOf(tunGroupsList())(r);
}

/** Persist the current order of the list on screen. The family's order route is per-scope,
 *  so only the tab being dragged over is sent. */
function saveTunOrder(): void {
  void saveOrder(tunScope(), tunRows().map((r: ApiTunnelConnectionRow | ApiTunnelRuleRow): string => { return r.id; }));
}

/** Move one row to just before/after another in its list, re-render, persist. */
function moveTunRow(id: string, target: string, before: boolean): void {
  if (!id || !target || id === target) return;
  const rows = tunRows() as (ApiTunnelConnectionRow | ApiTunnelRuleRow)[];
  const item = rows.find((r: ApiTunnelConnectionRow | ApiTunnelRuleRow): boolean => { return r.id === id; });
  if (!item) return;
  const to = rows.findIndex((r: ApiTunnelConnectionRow | ApiTunnelRuleRow): boolean => { return r.id === target; });
  if (to < 0) return; // target vanished mid-drag — leave everything where it is
  rows.splice(rows.indexOf(item), 1);
  rows.splice(before ? to : to + 1, 0, item);
  renderTunnels();
  saveTunOrder();
}

/** Put one row in a group. Applied locally first so the row jumps immediately, then persisted
 *  — a reject takes the server's word for it. Scope is the family's own word ("conns"|"rules"). */
async function assignTunScoped(scope: string, id: string, group: string | null): Promise<void> {
  const rows = scope === "rules" ? tunData().rules : tunData().connections;
  const row = rows.find((r: ApiTunnelConnectionRow | ApiTunnelRuleRow): boolean => { return r.id === id; });
  if (!row) return;
  const names = scope === "rules" ? tunData().ruleGroups : tunData().connGroups;
  if (makeGroupOf(names || [])(row) === (group || (names || [])[0])) return;
  row.group = group;
  renderTunnels();
  const j = await assignMember(scope, id, group);
  if (!j) { await loadTunnels(); return; }
  row.group = j?.group; // the canonical name the server stored
  renderTunnels();
}

/* --- the page -----------------------------------------------------------------------------------
   The tunnels plugin contributes TWO L2 pages (docs/13 D5, as revised): the Context Bar's
   "Tunnels / SSH Connections ▾" and "Tunnels / Port Forwards ▾" switch between them, and the
   mounted page decides which scope this module renders (mountedTunScope(), set by the page's
   mount). The body header is the page's TASK, not its location: one line of scope prose and
   the scope's actions on the right — no repeated "Tunnels" title, no second copy of the page
   switcher. */

/** The scope sentence under the bar: what THIS page operates on, never where we are (the
 *  context bar owns location). Plain prose — it lands in the built tree as a text node
 *  (docs/37 R5), so the Html suffix it carried as a string builder is gone. */
function tunDesc(isConns: boolean): string {
  return isConns
    ? tr("tunnels.sshHostsGatewayCan")
    : tr("tunnels.localPortsForwardedOver");
}

/** The one count builder: the context bar's chip AND the page footer read the same words,
 *  or the two would drift apart between polls (the mcpChipText lesson). */
function tunnelsCountText(scope: string): string {
  const d = tunData();
  if (scope === "rules") {
    const active = d.rules.filter((r: ApiTunnelRuleRow): boolean => { return r.state === "up"; }).length;
    return trn(d.rules.length, "tunnels.nRules.one", "tunnels.nRules.other") + tr("tunnels.nActive", { n: active });
  }
  const connected = d.connections.filter((c: ApiTunnelConnectionRow): boolean => { return c.state === "connected"; }).length;
  return trn(d.connections.length, "tunnels.nConnections.one", "tunnels.nConnections.other") + tr("tunnels.nConnected", { n: connected });
}

function renderTunnels(): void {
  const isConns = mountedTunScope() === "conns";
  // The page's actions, right-aligned in the body header (pane-actions is the panel's own
  // vocabulary for exactly this slot). Rules carry the bulk start/stop pair; connections
  // carry only New — Test lives on each row.
  // docs/46 §3.4: New group is the folder-plus glyph (the sidebar's, rule 7), the rules' bulk
  // Start all / Stop all wait behind the head's ⋯, and New is the page's one primary.
  const acts: HChild[] = [iconBtn("folder-plus", tr("tunnels.newGroup"), { id: "tNewGroup" })];
  if (!isConns) acts.push(moreBtn(tr("tunnels.moreForwardActions"), { id: "tMore" }));
  acts.push(btn(tr("tunnels.new"), { kind: "primary", id: isConns ? "tNewConn" : "tNewRule" }));

  // One group per slice — the component owns the header band, the indent and the empty line
  // now (docs/20 §4.1). Empty groups keep their place: that is how you drag the first row into
  // one (or use its +). They ALWAYS paint, rows or none (the Remote Targets rule, 2026-09-20):
  // both tabs used to swap in "No SSH connections" / "No forwarding rules" whenever the list
  // was bare, so a group made before the first row was listed in the sheet's Group select
  // and nowhere else - no header to rename or delete it by.
  const cfg = tunCfg();
  const grouped = slice(tunRows(), tunGroupsList(), tunGroupOfRow);

  // .wide: a rule row is name + route + who it serves + its port column + its buttons. No foot:
  // the count it carried is the context bar's chip (rule 25), and tunnels have no revision.
  fill($("pane"),
    paneBody({ wide: true },
      paneHead({ desc: tunDesc(isConns), actions: acts }),
      h("div", { id: "tunGroups" })));
  const host = $("tunGroups");
  grouped.forEach((g: GroupSlice<ApiTunnelConnectionRow | ApiTunnelRuleRow>): void => { host.appendChild(mountGroup(cfg, g)); });
  wireTunnels();
}

/** The conns/rules scope's cfg for mountGroup (see groups.js for the full contract). Built
 *  fresh each render so names and the tab's fold map are always the live objects. */
function tunCfg(): GroupCfg<ApiTunnelConnectionRow | ApiTunnelRuleRow> {
  return {
    scope: tunScope(),
    density: "page",
    names: tunGroupsList(),
    collapsed: tunFolds(mountedTunScope()),
    noun: tr("tunnels.row"),
    addTitle: (g: string): string => {
      return mountedTunScope() === "conns" ? tr("tunnels.addSshConnectionGroup", { group: g }) : tr("tunnels.addForwardingRuleGroup", { group: g });
    },
    onAdd: (g: string): void => {
      setTunPendingGroup(g); // a real name now — the sheet's save lands the row in it
      if (mountedTunScope() === "conns") openConnSheet(null); else openRuleSheet(null);
    },
    reload: (): Promise<void> => { return loadTunnels(); },
    render: renderTunnels,
    afterDrag: (): void => { renderTunnels(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: (): string | null => { return tunDragging(); },
      set: (v: string | null): void => { setTunDragging(v); },
    },
    dragGroup: {
      get: (): string | null => { return tunDraggingGroup(); },
      set: (v: string | null): void => { setTunDraggingGroup(v); },
    },
    rowId: (r: ApiTunnelConnectionRow | ApiTunnelRuleRow): string => { return r.id; },
    rowsById: tunRows,
    groupOfRow: tunGroupOfRow,
    // docs/37 R5: the rows are built, not parsed — the component wires drag on the node each
    // builder returns, so this scope no longer round-trips through rowsHtml + rowSel.
    rowNode: (r: ApiTunnelConnectionRow | ApiTunnelRuleRow): HTMLElement => {
      return mountedTunScope() === "conns" ? connRowNode(r as ApiTunnelConnectionRow) : ruleRowNode(r as ApiTunnelRuleRow);
    },
    onMoveRow: moveTunRow,
    onAssign: (id: string, g: string | null): void => { void assignTunScoped(tunScope(), id, g); },
  } as GroupCfg<ApiTunnelConnectionRow | ApiTunnelRuleRow>;
}

/** Poll-safe update: dots, reasons, button labels and the footer. Never structure. */
function patchTunnels() {
  if (!isTunnelsView(currentView())) return;
  const d = tunData();
  const rows = mountedTunScope() === "conns" ? d.connections : d.rules;
  const attr = mountedTunScope() === "conns" ? "data-conn" : "data-rule";
  const nodes = $("pane").querySelectorAll("[" + attr + "]");
  // A row appeared or vanished (another tab, or a reconnect that deleted nothing) — structure
  // changed, so a patch cannot express it. Never mid-drag: a rebuild there cancels the gesture.
  if (nodes.length !== rows.length) {
    if (!tunDragging() && !tunDraggingGroup()) renderTunnels();
    return;
  }
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i];
    const node = $("pane").querySelector("[" + attr + '="' + (window.CSS && CSS.escape ? CSS.escape(row.id) : row.id) + '"]');
    if (!node) { if (!tunDragging() && !tunDraggingGroup()) renderTunnels(); return; }
    const busy = tunBusyOf(row.id);
    // A failure line appearing, going or changing words is structure - the row swaps its sub
    // for the red line - so it rebuilds (never mid-drag); a poll with no such change patches.
    const isRule = mountedTunScope() !== "conns";
    const wantErr = (isRule ? (row.state === "error" || row.state === "reconnecting") : true) ? (row.reason || "") : "";
    const hasErr = node.querySelector(".lrow-err")?.textContent || "";
    if (wantErr !== hasErr) { if (!tunDragging() && !tunDraggingGroup()) renderTunnels(); return; }
    const mark = node.querySelector(".lrow-lead .dot");
    const live = busy ? "starting" : row.state;
    // A fresh dot, not a patched class: its class, title and label move together (docs/18 V6),
    // and a dot whose class moved but whose title stayed would explain the previous state.
    if (mark) mark.replaceWith(dot(tunDot(live), dotTitle(live === "connected" ? "up" : live, null, row.reason)));
    const act = node.querySelector("[data-act]") as HTMLButtonElement | null;
    if (act) {
      const running = row.state === "up" || row.state === "starting" || row.state === "reconnecting";
      const label = busy ? "…" : running ? tr("tunnels.stop") : tr("tunnels.start");
      if (act.textContent !== label) {
        act.textContent = label;
        act.dataset.act = running ? "stop" : "start";
      }
      act.disabled = !!busy;
    }
  }
  // Group counts move with rows: a count that only refreshed on rebuild would disagree with
  // the patched dots beside it for the rest of the poll.
  Array.prototype.forEach.call($("pane").querySelectorAll("[data-group]"), (grp) => {
    const n = rows.filter((r) => { return tunGroupOfRow(r) === grp.dataset.group; }).length;
    const badge = grp.querySelector(".grp-n");
    if (badge) badge.textContent = String(n);
  });
}

/* --- wiring: ONE delegated click on #pane (docs/37 R5) --------------------------------------------
   The page used to re-query every row after each render and assign onclick per button; a
   repaint destroyed the handlers and the wiring pass rebuilt them. The rows are nodes now
   and fill() rebuilds the pane, so one delegated listener answers every click this page
   owns — assigned as a property, so a repaint re-assigns the same slot instead of stacking
   listeners. Buttons are matched with closest("#id"), never t.id: a real pointer click on
   an icon button lands on its svg glyph, and the glyph carries no id. Row menus read the
   LIVE row at click time (docs/37 §10.1) — a 6s poll may have replaced tunData() between
   the render and the click, and the menu must not offer a Force free the row no longer
   needs. */
function wireTunnels() {
  $("pane").onclick = (ev: MouseEvent): void => {
    const t = targetEl(ev);
    if (!t) return;
    // The top New carries no group promise; the sheet falls back to the scope's last-used.
    if (t.closest("#tNewConn")) { openConnSheet(null); return; }
    if (t.closest("#tNewRule")) { openRuleSheet(null); return; }
    if (t.closest("#tNewGroup")) {
      newGroupFlow(tunScope(), tunGroupsList(), () => { return loadTunnels(); });
      return;
    }
    const bulk = t.closest<HTMLElement>("#tMore");
    if (bulk) {
      ev.stopPropagation();
      if (menuOpen()) { closeMenu(); return; }
      popupMenu(bulk.getBoundingClientRect(), [
        { label: tr("tunnels.startAll"), fn: (): void => { void startAllRules(); } },
        { label: tr("tunnels.stopAll"), fn: (): void => { void stopAllRules(false); } },
      ]);
      return;
    }
    // A rule's Start/Stop. Scoped to [data-rule] rows: connections carry [data-test].
    const act = t.closest<HTMLElement>("[data-act]");
    if (act) {
      const id = act.closest<HTMLElement>("[data-rule]")?.dataset.rule;
      if (id) void ruleAct(id, act.dataset.act || "start");
      return;
    }
    const test = t.closest<HTMLElement>("[data-test]");
    if (test) {
      const id = test.closest<HTMLElement>("[data-conn]")?.dataset.conn;
      if (id) void testConn(id);
      return;
    }
    const more = t.closest<HTMLElement>("[data-more]");
    if (!more) return;
    // The overflow half of the row (docs/18 V5). stopPropagation first: connect.js closes
    // open menus on clicks that reach document, so the very click that opens this one must
    // not also tear it down.
    ev.stopPropagation();
    const ruleRow = more.closest<HTMLElement>("[data-rule]");
    if (ruleRow) {
      const rule = tunData().rules.find((r) => { return r.id === ruleRow.dataset.rule; });
      if (!rule) return;
      // Force free appears only when a port is actually held — it is a remedy, not a
      // standing action.
      const items: MenuItem[] = [
        { label: tr("tunnels.edit"), fn: (): void => { openRuleSheet(rule); } },
        { label: tr("tunnels.copyLocalPort"), fn: (): void => { void copyText(String(rule.localPort), tr("tunnels.localPort")); } },
      ];
      if (rule.portOwner) items.push({ label: tr("tunnels.forceFreePort", { port: rule.localPort }), fn: (): void => { void forceFreePort(rule.localPort, rule.id); } });
      items.push({ sep: true }, { label: tr("tunnels.delete"), danger: true, fn: (): void => { void deleteRule(rule, false); } });
      popupMenu(more.getBoundingClientRect(), items);
      return;
    }
    const connRow = more.closest<HTMLElement>("[data-conn]");
    if (connRow) {
      const conn = tunData().connections.find((c: ApiTunnelConnectionRow): boolean => { return c.id === connRow.dataset.conn; });
      if (!conn) return;
      popupMenu(more.getBoundingClientRect(), [
        { label: tr("tunnels.edit"), fn: (): void => { openConnSheet(conn); } },
        { label: tr("tunnels.copyHost"), fn: (): void => { void copyText(conn.host + ":" + conn.port, tr("tunnels.host")); } },
        { sep: true },
        { label: tr("tunnels.delete"), danger: true, fn: (): void => { void deleteConn(conn); } },
      ]);
    }
  };
}

async function withTunBusy(id: string, verb: string, fn: () => Promise<unknown>): Promise<unknown> {
  if (tunBusyOf(id)) return null;
  setTunBusy(id, verb);
  patchTunnels();
  try {
    return await fn();
  } finally {
    clearTunBusy(id);
    await loadTunnels();
  }
}

/* --- rule actions ----------------------------------------------------------------------------- */

async function ruleAct(id: string, verb: string): Promise<void> {
  await withTunBusy(id, verb, async (): Promise<void> => {
    const r = await api("/api/tunnels/rules/" + encodeURIComponent(id) + "/" + verb, { method: "POST" });
    const j = await r.json().catch((): object => { return {}; }) as { confirmRequired?: boolean; dependents?: string[]; error?: string; ok?: boolean };
    // 409 means an MCP is using this tunnel. Tell the user who, then obey them.
    if (r.status === 409 && j.confirmRequired) {
      const names = j.dependents?.join(", ") || "";
      if (!confirm(trn(j.dependents?.length || 0, "tunnels.namesDependTunnelStop.one", "tunnels.namesDependTunnelStop.other", { names }))) return;
      const forced = await apiJson<unknown>("/api/tunnels/rules/" + encodeURIComponent(id) + "/stop?force=1", { method: "POST" });
      if (forced) toast(tr("tunnels.stopped"));
      return;
    }
    if (!r.ok) { toast(j.error || tr("tunnels.httpN", { n: r.status }), true); return; }
    if (j.ok === false) { toast(j.error || tr("tunnels.startFailed"), true); return; }
    toast(tr(verb === "start" ? "tunnels.started" : "tunnels.stopped"));
  });
}

async function startAllRules(): Promise<void> {
  const j = await apiJson<{ results?: { ok?: boolean; name?: string; error?: string }[] }>("/api/tunnels/start-all", { method: "POST" });
  await loadTunnels();
  if (!j) return;
  const failed = (j.results || []).filter((x: { ok?: boolean }): boolean => { return !x.ok; });
  toast(failed.length
    ? tr("tunnels.startedBFailedFirst", { a: j.results!.length - failed.length, b: failed.length, first: failed[0]!.name || "", error: failed[0]!.error || "" })
    : trn(j.results?.length || 0, "tunnels.nTunnelsStarted.one", "tunnels.nTunnelsStarted.other"), failed.length > 0);
}

async function stopAllRules(force?: boolean): Promise<void> {
  const r = await api("/api/tunnels/stop-all" + (force ? "?force=1" : ""), { method: "POST" });
  const j = await r.json().catch((): object => { return {}; }) as { confirmRequired?: boolean; dependents?: string[]; error?: string; results?: unknown[] };
  if (r.status === 409 && j.confirmRequired) {
    if (!confirm(tr("tunnels.theseMcpsUsingTunnels", { names: j.dependents?.join(", ") || "" }))) return;
    return stopAllRules(true);
  }
  await loadTunnels();
  if (!r.ok) { toast(j.error || tr("tunnels.httpN", { n: r.status }), true); return; }
  toast(trn((j.results || []).length, "tunnels.nTunnelsStopped.one", "tunnels.nTunnelsStopped.other"));
}

async function deleteRule(rule: ApiTunnelRuleRow, force?: boolean): Promise<void> {
  if (!force && !confirm(tr("tunnels.deleteForwardingRuleName", { name: rule.name }))) return;
  const r = await api("/api/tunnels/rules/" + encodeURIComponent(rule.id) + (force ? "?force=1" : ""), { method: "DELETE" });
  const j = await r.json().catch((): object => { return {}; }) as { confirmRequired?: boolean; dependents?: string[]; error?: string };
  if (r.status === 409 && j.confirmRequired) {
    if (!confirm(tr("tunnels.namesDependTunnelDelete", { names: j.dependents?.join(", ") || "" }))) return;
    return deleteRule(rule, true);
  }
  await loadTunnels();
  if (!r.ok) { toast(j.error || tr("tunnels.httpN", { n: r.status }), true); return; }
  toast(tr("tunnels.deletedName", { name: rule.name }));
}

/** Kill whatever holds a local port. Confirmed here because it can kill a process doing real work. */
async function forceFreePort(port: number, ruleId: string): Promise<void> {
  const d = tunData();
  const rule = d.rules.find((r: ApiTunnelRuleRow): boolean => { return r.id === ruleId; });
  const owner = rule && rule.portOwner;
  if (!confirm(tr("tunnels.portPortHeldPid", { port, owner: owner ? owner.pid + " (" + owner.name + ")" : "?" }))) return;
  const j = await apiJson<{ killed: { pid: number; name: string } }>("/api/tunnels/port/" + port + "/free", { method: "POST" });
  if (!j) { await loadTunnels(); return; }
  toast(tr("tunnels.killedPidPidName", { pid: j.killed.pid, name: j.killed.name }));
  await ruleAct(ruleId, "start");
}

/* --- connection actions ------------------------------------------------------------------------ */

async function testConn(id: string): Promise<void> {
  await withTunBusy(id, "test", async (): Promise<void> => {
    const j = await apiJson<{ ok?: boolean; ms?: number; banner?: string; kind?: string; fingerprint?: string; error?: string }>("/api/tunnels/connections/" + encodeURIComponent(id) + "/test", { method: "POST" });
    if (!j) return;
    if (j.ok) { toast(j.banner ? tr("tunnels.connectedMsMsBanner", { ms: j.ms ?? 0, banner: j.banner }) : tr("tunnels.connectedMsMs", { ms: j.ms ?? 0 })); return; }
    // A changed host key is the one failure with an action attached.
    if (j.kind === "hostkey" && j.fingerprint) {
      if (confirm(tr("tunnels.errorTrustNewKey", { error: j.error || "" }))) {
        const t = await apiJson<unknown>("/api/tunnels/connections/" + encodeURIComponent(id) + "/trust", { method: "POST" });
        if (t) toast(tr("tunnels.hostKeyTrustedTest"));
      }
      return;
    }
    toast(tr("tunnels.errorKind", { error: j.error || "", kind: j.kind || "" }), true);
  });
}

async function deleteConn(conn: ApiTunnelConnectionRow): Promise<void> {
  if (!confirm(tr("tunnels.deleteSshConnectionName", { name: conn.name }))) return;
  const j = await apiJson<unknown>("/api/tunnels/connections/" + encodeURIComponent(conn.id), { method: "DELETE" });
  await loadTunnels();
  if (j) toast(tr("tunnels.deletedName", { name: conn.name }));
}

/* --- the two L2 pages' shared lifecycle -----------------------------------------------------------
   views/tunnels.js (#tunnels, SSH Connections) and views/tunnel-forwards.js
   (#tunnel-forwards, Port Forwards) are thin modules over this one: same data, same
   rendering, same polling — only the scope differs, and each mount states its own. No
   business logic is duplicated on either side of the pair. */

/** Mount one tunnels page: pin the scope, then load. Scope is the /api/groups family's own
 *  word ("conns"|"rules"); everything in this module reads it through mountedTunScope(). */
async function mountTunnelsPage(scope: string): Promise<void> {
  setMountedTunScope(scope);
  return loadTunnels();
}

function refreshTunnelsPage(): Promise<void> { return loadTunnels(); }

async function pollTunnelsPage() {
  await loadList();
  await loadTunnels(true);
}

function unmountTunnelsPage() {
  clearTunView();
}

export { assignTunScoped, deleteConn, deleteRule, forceFreePort, mountTunnelsPage, moveTunRow, patchTunnels, pollTunnelsPage, refreshTunnelsPage, renderTunnels, ruleAct, saveTunOrder, startAllRules, stopAllRules, testConn, tunCfg, tunnelsCountText, unmountTunnelsPage, wireTunnels, withTunBusy };
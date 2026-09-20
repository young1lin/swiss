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

import type { ApiTrafficClientRow, ApiTrafficFull, ApiTrafficPage, ApiTrafficRow } from "./types/api.js";
import { $, apiJson, iconNode, targetEl, whenLabel } from "./util.js";
import { fill, frag, h } from "./h.js";
import type { HChild } from "./h.js";
import { fmtJson } from "./logs.js";
import { currentView } from "./ui-state.js";
import { locale, tr, trn } from "./i18n.js";

/* The traffic domain owns its state (docs/37 R4, slice 2 of 7): ONE page of interactions, the
 * ring-wide client fold, the query that produced them (filter, client, page) and the per-row
 * expansion cache. This module reads the record directly; views/traffic.ts reaches it only
 * through resetTrafficSig/clearTrafficView below, so no other module can poke a field. */
const traffic: {
  rows: ApiTrafficRow[];
  clients: ApiTrafficClientRow[];
  filter: "actions" | "all";
  client: string | null;
  page: number;
  more: boolean;
  total: number;
  all: number;
  open: Record<string, boolean>;
  full: Record<string, ApiTrafficFull>;
  sig: string | null;
} = {
  rows: [], clients: [], filter: "actions", client: null, page: 0,
  more: false, total: 0, all: 0, open: {}, full: {}, sig: null,
};

/** A forced repaint: drop the signature so the next load rebuilds instead of skipping it. */
export function resetTrafficSig(): void { traffic.sig = null; }
/** Leaving the view: drop the page and the expansion cache so a return starts clean. */
export function clearTrafficView(): void {
  traffic.rows = [];
  traffic.full = {};
  traffic.open = {};
  traffic.sig = null;
}

/* --- traffic: who (token + self-reported client) asked what, across every MCP -----------------
   Two sections: a "Clients" summary (who has been talking to the gateway, and which MCPs), and an
   "Activity" log underneath. The log defaults to "Actions" (tools/call, resources/read, …) so the
   protocol handshake stops drowning out the things you actually want to see. Click a client to filter
   the log to it.

   The gateway speaks stateless HTTP — there is no session table, so "clients" is folded from recorded
   interaction traffic: last-seen is the honest signal. The newest entries are tailed to disk and
   restored on boot, so a gateway restart no longer blanks this view. */
/** Identity for grouping moved server-side (traffic.ts folds clients itself); the panel-side
 *  copy had no callers left. */
/** Relative "x ago"; refreshes every poll (6s) because renderTraffic re-runs while the view is open. */
function ago(iso: string): string {
  const ms = Date.now() - new Date(iso).getTime();
  if (ms < 5000) return tr("traffic.justNow");
  if (ms < 60000) return tr("traffic.nSAgo", { n: Math.round(ms / 1000) });
  if (ms < 3600000) return tr("traffic.nMAgo", { n: Math.round(ms / 60000) });
  return new Date(iso).toLocaleTimeString(locale());
}

/**
 * Fetch one page of the activity log.
 *
 * Filtering, paging and the client fold all happen on the server now. They used to happen here, over
 * a 200-entry dump that carried every entry's raw request and reply (8 KB each) — up to megabytes on
 * a 6-second poll, to draw a dozen lines whose JSON was hidden behind a chevron. Two consequences to
 * keep in mind while reading the rest of this section: a row has no `.body`/`.response` until it is
 * expanded (see toggleTraffic), and `traffic.clients` is the whole ring's fold, not this page's.
 */
async function loadTraffic(): Promise<void> {
  const q = "/api/traffic?page=" + (traffic.page || 0) +
    (traffic.filter !== "all" ? "&actions=1" : "") +
    (traffic.client ? "&client=" + encodeURIComponent(traffic.client) : "");
  const j = await apiJson<ApiTrafficPage>(q);
  if (!j) return;
  traffic.rows = j.entries || [];
  traffic.clients = j.clients || [];
  traffic.total = j.total || 0;
  traffic.all = j.totalUnfiltered || 0;
  traffic.more = !!j.more;
  if (currentView() !== "traffic") return;
  // Skip the rebuild when nothing changed: a 6s poll would otherwise collapse every expanded row and
  // reset scroll while you are reading one. Filter/refresh/view-entry call renderTraffic() directly.
  const sig = (traffic.rows.length ? traffic.rows[0].seq : 0) + ":" + traffic.rows.length +
    ":" + traffic.total + ":" + traffic.clients.length;
  if (traffic.sig === sig) return;
  traffic.sig = sig;
  renderTraffic();
}

/** Re-query after anything that changes which rows the server should return. */
function trafficReload(resetPage?: boolean): void {
  if (resetPage) traffic.page = 0;
  traffic.sig = null;
  void loadTraffic();
}
function trafficPageStep(delta: number): void {
  const next = (traffic.page || 0) + delta;
  if (next < 0 || (delta > 0 && !traffic.more)) return;
  traffic.page = next;
  trafficReload(false);
}
function trafficRowNode(e: ApiTrafficRow): HChild {
  const meta = tr("traffic.clientMcpStatusMs", {
    client: e.clientName ? e.clientName + (e.clientVersion ? " " + e.clientVersion : "") : "—",
    mcp: e.mcp,
    status: e.ok ? tr("traffic.ok") : tr("traffic.err"),
    ms: e.ms,
    when: whenLabel(e.at),
  });
  // Collapsed: method + a one-line params preview + meta. Expanded (chevron): the raw request JSON.
  return h("div", { class: "call" + (traffic.open[e.seq] ? " open" : ""), data: { tseq: e.seq } },
    h("div", { class: "call-sum", data: { tog: e.seq }, role: "button", tabIndex: 0 },
      h("span", { class: "chev", aria: { hidden: "true" } }, iconNode("chevron-right")),
      h("span", { class: "dot " + (e.ok ? "up" : "down") }),
      h("span", { class: "call-tool" }, e.method),
      h("span", { class: "call-arg" }, e.params || ""),
      h("span", { class: "call-meta" }, meta)),
    // Payload deliberately absent until the row is opened — trafficBodyNode renders whatever
    // toggleTraffic has fetched. Building 200 hidden <pre> blocks (two fmtJson round-trips each) was
    // most of what this view cost, and none of it was on screen.
    h("div", { class: "call-body" }, trafficBodyNode(e)));
}

function trafficBodyNode(e: { seq: number }): HChild {
  const full = traffic.full[e.seq];
  if (!full) return h("div", { class: "note" }, h("span", { class: "spin" }), " ", tr("traffic.loading"));
  if (full.gone) return h("div", { class: "note" }, tr("traffic.interactionRolledBuffer"));
  return frag(
    h("div", { class: "call-lbl" }, tr("traffic.request")),
    h("pre", { class: "logs" }, fmtJson(full.body || "")),
    h("div", { class: "call-lbl" }, tr("traffic.response"),
      full.response ? null : h("span", { style: "color:var(--text-3)" }, tr("traffic.none"))),
    h("pre", { class: "logs" },
      full.response ? fmtJson(full.response)
        : h("span", { style: "color:var(--text-3)" }, tr("traffic.replyCapturedEntry"))));
}

/** Fetch one interaction's raw request and reply, then paint it into the already-open row. */
async function loadTrafficBody(seq: number): Promise<void> {
  if (traffic.full[seq]) return;
  const j = await apiJson("/api/traffic/" + encodeURIComponent(seq));
  // A 404 means the ring rolled past it while the row sat there; say so rather than spin forever.
  traffic.full[seq] = j || { gone: true } as ApiTrafficFull;
  const node = document.querySelector('#pane .call[data-tseq="' + seq + '"] .call-body');
  if (node) fill(node as HTMLElement, trafficBodyNode({ seq: seq }));
}
/** Expand/collapse one row's raw JSON in place — a poll must not close what you just opened. */
function toggleTraffic(seq: number): void {
  traffic.open[seq] = !traffic.open[seq];
  const node = document.querySelector('#pane .call[data-tseq="' + seq + '"]');
  if (node) node.className = "call" + (traffic.open[seq] ? " open" : "");
  if (traffic.open[seq]) void loadTrafficBody(seq);
}
function renderTraffic(): void {
  // `entries` is one page; `clients`, `total` and `all` describe the whole ring (server-computed).
  const entries = traffic.rows || [];
  const clients = traffic.clients || [];
  const total = traffic.total || 0;
  const all = traffic.all || 0;
  const sel = traffic.client;

  // Controls: action/everything toggle + clear. This whole pane is the Traffic view's content.
  const ctrl = h("div", { class: "sec-head" },
    h("span", { class: "sec-cap" }, tr("traffic.traffic")),
    h("span", { style: "display:flex;gap:var(--s2);align-items:center" },
      h("div", { class: "seg", id: "trFilter" },
        h("button", { data: { filter: "actions" }, aria: { selected: traffic.filter !== "all" ? "true" : "false" } }, tr("traffic.actions")),
        h("button", { data: { filter: "all" }, aria: { selected: traffic.filter === "all" ? "true" : "false" } }, tr("traffic.everything"))),
      h("button", { class: "btn", id: "trClear", disabled: !all }, sel ? tr("traffic.clearClient") : tr("traffic.clear"))));

  // Clients — who has been talking to the gateway, folded from recorded traffic (stateless: no live
  // connection to query). Click a row to filter the activity log to that client.
  let clientBlock: HChild;
  if (clients.length) {
    // One line per client (docs/18 V4): name, token, paths, last seen, request count — a
    // five-column grid, not a card-per-client with two lines of prose.
    const head = h("div", { class: "cli-head" },
      h("span", null, tr("traffic.client")), h("span", null, tr("traffic.token")), h("span", null, tr("traffic.paths")),
      h("span", null, tr("traffic.last")), h("span", { class: "cli-n" }, tr("traffic.requests")), h("span"));
    const rows = clients.map((c) => {
      const isSel = sel === c.key;
      const mcps = (c.mcps || []).map((m) => { return "/mcp/" + m; }).join(" ");
      const tokens = c.tokens || [];
      const tokenLine = tokens.length ? tokens.join(", ") : tr("traffic.token2");
      return h("div", { class: "cli-row" + (isSel ? " sel" : ""), data: { ckey: c.key }, role: "button", tabIndex: 0 },
        h("span", { class: "cli-name" }, h("code", null, c.label)),
        h("span", { class: "cli-token" }, tokenLine),
        h("span", { class: "cli-paths" }, mcps),
        h("span", { class: "cli-last" }, ago(c.lastAt)),
        h("span", { class: "cli-n" }, c.count.toLocaleString(locale())),
        isSel
          ? h("button", { class: "btn ghost icon", data: { cclr: "" }, title: tr("traffic.stopFiltering") }, iconNode("x"))
          : h("span", { class: "cli-x" }));
    });
    clientBlock = frag(
      h("div", { class: "cap", style: "padding-top:var(--s5)" }, tr("traffic.clientsN", { n: clients.length })),
      h("div", { class: "group" }, head, rows));
  } else {
    clientBlock = frag(
      h("div", { class: "cap", style: "padding-top:var(--s5)" }, tr("traffic.clients")),
      h("div", { class: "group" }, h("div", { class: "row" },
        h("span", { class: "rowmsg" }, tr("traffic.clientsWhenClientSends")))));
  }

  // Activity log — de-noised by the toggle, narrowed by a selected client.
  const actCap = traffic.filter === "all" ? tr("traffic.activityEverything") : tr("traffic.activityActionsOnly");
  const countTxt = total !== all
    ? tr("traffic.nAllInteractions", { n: total.toLocaleString(locale()), all: all.toLocaleString(locale()) })
    : trn(total, "traffic.nInteractions.one", "traffic.nInteractions.other", { n: total.toLocaleString(locale()) });
  const actHead = h("div", { class: "sec-head", style: "padding-top:var(--s5)" },
    h("span", { class: "sec-cap" }, actCap),
    h("span", { class: "hint" }, countTxt));
  let body: HChild;
  if (!all) {
    body = h("div", { class: "group" }, h("div", { class: "row" },
      h("span", { class: "rowmsg" }, tr("traffic.interactionsEveryJsonRpc"))));
  } else if (!entries.length) {
    const hint = traffic.page
      ? tr("traffic.nothingPage")
      : sel
        ? (traffic.filter !== "all" ? tr("traffic.activityClientActionsOnly") : tr("traffic.activityClient"))
        : (traffic.filter !== "all" ? tr("traffic.actionsSwitchEverythingSee") : tr("traffic.interactions"));
    body = h("div", { class: "group" }, h("div", { class: "row" }, h("span", { class: "rowmsg" }, hint)));
  } else {
    body = h("div", { class: "group" }, entries.map(trafficRowNode));
  }

  // Newer/Older, worded and shaped exactly like the tool-call log's pager — same direction, so
  // "Newer" always means toward the top of a newest-first list in both views.
  const pager: HChild = (traffic.page > 0 || traffic.more)
    ? h("div", { class: "pager" },
        h("button", { class: "btn", id: "trPrev", disabled: traffic.page <= 0 }, tr("traffic.newer")),
        h("span", null, tr("traffic.pageN", { n: traffic.page + 1 })),
        h("button", { class: "btn", id: "trNext", disabled: !traffic.more }, tr("traffic.older")))
    : null;

  // Wrapped in .wide: an interaction row is method + params + client + timing on one line, which the
  // standard measure cannot hold. The wiring below queries nothing: ONE delegated click and ONE
  // delegated keydown on #pane (docs/37 R5) answer every control, rows and pager included, and a
  // repaint re-assigns the same properties instead of re-attaching per-row handlers.
  fill($("pane"), h("div", { class: "wide" }, ctrl, clientBlock, actHead, body, pager));

  // Every control below re-queries rather than re-filtering in place: the server owns the filter now,
  // so a page of "actions only for claude-code" can only come from asking for exactly that.
  const pane = $("pane");
  pane.onclick = (ev: MouseEvent): void => {
    const t = targetEl(ev);
    if (!t) return;
    const fbtn = t.closest<HTMLElement>("[data-filter]");
    if (fbtn) { traffic.filter = fbtn.dataset.filter as "actions" | "all"; trafficReload(true); return; }
    if (t.closest("[data-cclr]")) { ev.stopPropagation(); traffic.client = null; trafficReload(true); return; }
    const crow = t.closest<HTMLElement>("[data-ckey]");
    if (crow) {
      traffic.client = traffic.client === crow.dataset.ckey ? null : crow.dataset.ckey as string;
      trafficReload(true);
      return;
    }
    if (t.id === "trPrev") { trafficPageStep(-1); return; }
    if (t.id === "trNext") { trafficPageStep(1); return; }
    const tog = t.closest<HTMLElement>("[data-tog]");
    if (tog) { toggleTraffic(Number(tog.dataset.tog)); return; }
    if (t.id === "trClear") { void clearTraffic(); }
  };
  pane.onkeydown = (ev: KeyboardEvent): void => {
    if (ev.key !== "Enter" && ev.key !== " ") return;
    const t = targetEl(ev);
    if (!t) return;
    const crow = t.closest<HTMLElement>("[data-ckey]");
    if (crow) { ev.preventDefault(); crow.click(); return; }
    const tog = t.closest<HTMLElement>("[data-tog]");
    if (tog) { ev.preventDefault(); toggleTraffic(Number(tog.dataset.tog)); }
  };
}

/** Clear the selected client only when one is filtered ("Clear client"); otherwise clear all. */
async function clearTraffic(): Promise<void> {
  const sel = traffic.client;
  const url = "/api/traffic" + (sel ? "?client=" + encodeURIComponent(sel) : "");
  const j = await apiJson(url, { method: "DELETE" });
  if (!j) return;
  traffic.open = {};
  traffic.full = {};
  traffic.client = null; // the filtered client is gone (or we cleared all); drop the filter
  trafficReload(true);
}

export { ago, loadTraffic, loadTrafficBody, renderTraffic, toggleTraffic, trafficBodyNode, trafficPageStep, trafficReload, trafficRowNode };
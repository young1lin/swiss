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

                                                                                                         
import { $, apiJson, targetEl } from "./util.js";
import { fill, h } from "./h.js";
                                     
import { readableBody } from "./logs.js";
import { currentView } from "./ui-state.js";
import { locale, tr, trn } from "./i18n.js";
import {
  btn, card, closeMenu, collapseRuns, emptyNode, iconBtn, menuOpen, moreBtn, note, pager, paneBody, popupMenu, section, seg,
  timeline, timelineMeta, timelineToggle, valueBlock,
} from "./ui/index.js";
                                                  

/* The traffic domain owns its state (docs/37 R4, slice 2 of 7): ONE page of interactions, the
 * ring-wide client fold, the query that produced them (filter, client, page) and the per-row
 * expansion cache. This module reads the record directly; views/traffic.ts reaches it only
 * through resetTrafficSig/clearTrafficView below, so no other module can poke a field. */
const traffic   
                        
                                 
                            
                        
               
                
                
              
                                
                                       
                     
  = {
  rows: [], clients: [], filter: "actions", client: null, page: 0,
  more: false, total: 0, all: 0, open: {}, full: {}, sig: null,
};

/** A forced repaint: drop the signature so the next load rebuilds instead of skipping it. */
export function resetTrafficSig()       { traffic.sig = null; }
/** Leaving the view: drop the page and the expansion cache so a return starts clean. */
export function clearTrafficView()       {
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
function ago(iso        )         {
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
async function loadTraffic()                {
  const q = "/api/traffic?page=" + (traffic.page || 0) +
    (traffic.filter !== "all" ? "&actions=1" : "") +
    (traffic.client ? "&client=" + encodeURIComponent(traffic.client) : "");
  const j = await apiJson                (q);
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
function trafficReload(resetPage          )       {
  if (resetPage) traffic.page = 0;
  traffic.sig = null;
  void loadTraffic();
}
function trafficPageStep(delta        )       {
  const next = (traffic.page || 0) + delta;
  if (next < 0 || (delta > 0 && !traffic.more)) return;
  traffic.page = next;
  trafficReload(false);
}
/* --- docs/46 §3.3: the activity is the event list ----------------------------------------------
   The same list MCP Logs is: the time (the date is the day heading), the method, its params on one
   line, who asked - the client and the MCP, a column only while the page holds more than one - a
   failure as a red tag, the duration. Consecutive identical interactions (a client's poll) fold
   into ×N. The raw request and reply stay off the page until a row is opened. */

/** Who sent it: the client's own name (and version), or a dash for one that never said. */
function clientWords(e               )         {
  return e.clientName ? e.clientName + (e.clientVersion ? " " + e.clientVersion : "") : "—";
}

/** An interaction as a timeline item; `tseq` addresses it for the delegated listener. */
function trafficItem(e               )               {
  return {
    id: String(e.seq),
    at: Date.parse(e.at),
    title: e.method,
    arg: e.params || "",
    who: clientWords(e) + " · " + e.mcp,
    ms: e.ms,
    status: e.ok ? undefined : { text: tr("traffic.failed"), tone: "bad" },
    same: [e.method, e.params, e.ok ? "ok" : "failed", e.mcp, e.clientName || ""].join("\u0000"),
    data: { tseq: e.seq },
  };
}

/** The interactions a row stands for, newest first (the row's own and, for ×N, every identical
 *  one folded under it). */
function trafficRunOf(seq        )                  {
  const rows = traffic.rows;
  const bySeq = new Map(rows.map((r) => { return [String(r.seq), r]         ; }));
  for (const run of collapseRuns(rows.map(trafficItem))) {
    if (run.some((it) => { return it.id === String(seq); })) return run.map((it) => { return bySeq.get(it.id)                 ; });
  }
  return [];
}

/** An open row: who and where, then the raw request and the reply as the panel's code block. A
 *  ×N row says how many and over what span. The payload is fetched on open (loadTrafficBody). */
function trafficBodyNode(e               , run                  = [e])         {
  const meta = timelineMeta([
    tr("traffic.metaClient", { client: clientWords(e) }),
    h("code", null, tr("pane.mcpPath", { name: e.mcp })),
    run.length > 1 ? tr("traffic.runSame", { n: run.length }) : null,
  ]);
  const full = traffic.full[e.seq];
  if (!full) return [meta, note(tr("traffic.loading"), { busy: true })];
  if (full.gone) return [meta, note(tr("traffic.interactionRolledBuffer"))];
  return [
    meta,
    valueBlock({ label: tr("traffic.request") }, ...readableBody(full.body || "", false)),
    full.response
      ? valueBlock({ label: tr("traffic.response") }, ...readableBody(full.response, !e.ok))
      : valueBlock({ label: tr("traffic.response"), text: tr("traffic.replyCapturedEntry") }),
  ];
}

/** Fetch one interaction's raw request and reply, then paint it into its open row. */
async function loadTrafficBody(seq        )                {
  if (traffic.full[seq]) return;
  const j = await apiJson("/api/traffic/" + encodeURIComponent(seq));
  // A 404 means the ring rolled past it while the row sat there; say so rather than spin forever.
  traffic.full[seq] = j || { gone: true }                  ;
  const item = document.querySelector('#pane .tl-item[data-tseq="' + seq + '"]');
  const body = item ? item.querySelector             (":scope > .tl-body") : null;
  const run = trafficRunOf(seq);
  if (body && run.length) fill(body, trafficBodyNode(run[0], run));
}
/** Expand/collapse one row in place — a poll must not close what you just opened. The body is
 *  painted on open only; closing a folded row forgets every interaction in it. */
function toggleTraffic(seq        )       {
  const run = trafficRunOf(seq);
  const open = !run.some((r) => { return !!traffic.open[r.seq]; }) && !traffic.open[seq];
  run.forEach((r) => { traffic.open[r.seq] = false; });
  traffic.open[seq] = open;
  const root = document.querySelector             ("#pane .tl");
  if (root && run.length) timelineToggle(root, String(seq), open ? trafficBodyNode(run[0], run) : undefined);
  if (open) void loadTrafficBody(seq);
}

/** The activity as the event list; a folded row is open when any interaction in it was opened. */
function trafficTimeline(entries                 )              {
  const bySeq = new Map(entries.map((r) => { return [String(r.seq), r]         ; }));
  const items = entries.map(trafficItem);
  const open = new Set        ();
  for (const run of collapseRuns(items)) {
    if (run.some((it) => { return !!traffic.open[Number(it.id)]; })) open.add(run[0].id);
  }
  return timeline(items, {
    open,
    body: (_it, run) => {
      const rows = run.map((it) => { return bySeq.get(it.id)                 ; });
      return trafficBodyNode(rows[0], rows);
    },
  });
}

function renderTraffic()       {
  // `entries` is one page; `clients`, `total` and `all` describe the whole ring (server-computed).
  const entries = traffic.rows || [];
  const clients = traffic.clients || [];
  const total = traffic.total || 0;
  const all = traffic.all || 0;
  const sel = traffic.client;

  // Clients — who has been talking to the gateway, folded from recorded traffic (stateless: no live
  // connection to query). Click a row to filter the activity log to that client.
  let clientBlock        ;
  if (clients.length) {
    // One line per client (docs/18 V4): name, token, paths, last seen, request count — a
    // five-column grid (the page's own table, views.css .cli-*), not a card-per-client.
    const head = h("div", { class: "cli-head" },
      h("span", null, tr("traffic.client")), h("span", null, tr("traffic.token")), h("span", null, tr("traffic.paths")),
      h("span", null, tr("traffic.last")), h("span", { class: "cli-n" }, tr("traffic.requests")), h("span"));
    const rows = clients.map((c) => {
      const isSel = sel === c.key;
      const mcps = (c.mcps || []).map((m) => { return tr("pane.mcpPath", { name: m }); }).join(" ");
      const tokens = c.tokens || [];
      const tokenLine = tokens.length ? tokens.join(", ") : tr("traffic.token2");
      return h("div", { class: "cli-row" + (isSel ? " sel" : ""), data: { ckey: c.key }, role: "button", tabIndex: 0 },
        h("span", { class: "cli-name" }, h("code", null, c.label)),
        h("span", { class: "cli-token" }, tokenLine),
        h("span", { class: "cli-paths" }, mcps),
        h("span", { class: "cli-last" }, ago(c.lastAt)),
        h("span", { class: "cli-n" }, c.count.toLocaleString(locale())),
        isSel
          ? iconBtn("x", tr("traffic.stopFiltering"), { ghost: true, data: { cclr: "" } })
          : h("span", { class: "cli-x" }));
    });
    clientBlock = section({ cap: tr("traffic.clientsN", { n: clients.length }) }, card(head, rows));
  } else {
    clientBlock = section({ cap: tr("traffic.clients") }, note(tr("traffic.clientsWhenClientSends")));
  }

  // Activity — its filter is the section's own (it narrows the activity, nothing else on the
  // page), and Clear waits behind the section's ⋯ (a destructive verb gets no standing button).
  const countTxt = total !== all
    ? tr("traffic.nAllInteractions", { n: total.toLocaleString(locale()), all: all.toLocaleString(locale()) })
    : trn(total, "traffic.nInteractions.one", "traffic.nInteractions.other", { n: total.toLocaleString(locale()) });
  const tools = [
    seg([{ id: "actions", label: tr("traffic.actions") }, { id: "all", label: tr("traffic.everything") }],
      traffic.filter === "all" ? "all" : "actions", { key: "filter", label: tr("traffic.show") }),
    moreBtn(tr("traffic.moreActivityActions"), { id: "trMore" }),
  ];
  let body        ;
  if (!all) {
    body = emptyNode({ icon: "history", title: tr("traffic.noInteractionsYet"), hint: tr("traffic.noInteractionsHint") });
  } else if (!entries.length) {
    body = note(traffic.page
      ? tr("traffic.nothingPage")
      : sel
        ? (traffic.filter !== "all" ? tr("traffic.activityClientActionsOnly") : tr("traffic.activityClient"))
        : (traffic.filter !== "all" ? tr("traffic.actionsSwitchEverythingSee") : tr("traffic.interactions")));
  } else {
    body = trafficTimeline(entries);
  }

  // Newer/Older, worded and shaped exactly like the tool-call log's pager — same direction, so
  // "Newer" always means toward the top of a newest-first list in both views.
  const pages         = (traffic.page > 0 || traffic.more)
    ? pager({
        label: tr("traffic.activityPages"), status: tr("traffic.pageN", { n: traffic.page + 1 }),
        prev: btn(tr("traffic.newer"), { id: "trPrev", disabled: traffic.page <= 0 }),
        next: btn(tr("traffic.older"), { id: "trNext", disabled: !traffic.more }),
      })
    : null;

  // The wide frame: an interaction row is time + method + params + who + timing on one line. The
  // wiring below queries nothing: ONE delegated click and ONE delegated keydown on #pane (docs/37
  // R5) answer every control, rows and pager included.
  fill($("pane"), paneBody({ wide: true },
    clientBlock,
    section({ cap: tr("traffic.activity"), note: countTxt, tools }, body, pages)));

  // Every control below re-queries rather than re-filtering in place: the server owns the filter now,
  // so a page of "actions only for claude-code" can only come from asking for exactly that.
  const pane = $("pane");
  pane.onclick = (ev            )       => {
    const t = targetEl(ev);
    if (!t) return;
    const fbtn = t.closest             ("[data-filter]");
    if (fbtn) { traffic.filter = fbtn.dataset.filter                     ; trafficReload(true); return; }
    if (t.closest("[data-cclr]")) { ev.stopPropagation(); traffic.client = null; trafficReload(true); return; }
    const crow = t.closest             ("[data-ckey]");
    if (crow) {
      traffic.client = traffic.client === crow.dataset.ckey ? null : crow.dataset.ckey          ;
      trafficReload(true);
      return;
    }
    if (t.id === "trPrev") { trafficPageStep(-1); return; }
    if (t.id === "trNext") { trafficPageStep(1); return; }
    const more = t.closest             ("#trMore");
    if (more) {
      ev.stopPropagation();
      if (menuOpen()) { closeMenu(); return; }
      popupMenu(more.getBoundingClientRect(), [{
        label: traffic.client ? tr("traffic.clearClient") : tr("traffic.clear"),
        danger: true, disabled: !traffic.all,
        fn: ()       => { void clearTraffic(); },
      }]);
      return;
    }
    // A row's summary is a real <button> (ui/timeline.ts): Enter and Space arrive as this click.
    // Only the summary toggles - a click inside the open body is someone reading it.
    const sum = t.closest             (".tl-sum");
    const item = sum ? sum.closest             ("[data-tseq]") : null;
    if (item) toggleTraffic(Number(item.dataset.tseq));
  };
  pane.onkeydown = (ev               )       => {
    if (ev.key !== "Enter" && ev.key !== " ") return;
    const t = targetEl(ev);
    if (!t) return;
    const crow = t.closest             ("[data-ckey]");
    if (crow) { ev.preventDefault(); crow.click(); }
  };
}

/** Clear the selected client only when one is filtered ("Clear client"); otherwise clear all. */
async function clearTraffic()                {
  const sel = traffic.client;
  const url = "/api/traffic" + (sel ? "?client=" + encodeURIComponent(sel) : "");
  const j = await apiJson(url, { method: "DELETE" });
  if (!j) return;
  traffic.open = {};
  traffic.full = {};
  traffic.client = null; // the filtered client is gone (or we cleared all); drop the filter
  trafficReload(true);
}

export { ago, loadTraffic, loadTrafficBody, renderTraffic, toggleTraffic, trafficBodyNode, trafficItem, trafficPageStep, trafficReload };
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

                                                                                                         
import { $, apiJson, iconNode, targetEl, whenLabel } from "./util.js";
import { fill, frag, h } from "./h.js";
                                     
import { fmtJson } from "./logs.js";
import { currentView } from "./ui-state.js";

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
  if (ms < 5000) return "just now";
  if (ms < 60000) return Math.round(ms / 1000) + "s ago";
  if (ms < 3600000) return Math.round(ms / 60000) + "m ago";
  return new Date(iso).toLocaleTimeString();
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
function trafficRowNode(e               )         {
  const meta = (e.clientName ? e.clientName + (e.clientVersion ? " " + e.clientVersion : "") : "—") +
    "  ·  /" + e.mcp + "  ·  " + (e.ok ? "ok" : "err") + "  ·  " + e.ms + "ms  ·  " + whenLabel(e.at);
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

function trafficBodyNode(e                 )         {
  const full = traffic.full[e.seq];
  if (!full) return h("div", { class: "note" }, h("span", { class: "spin" }), " Loading…");
  if (full.gone) return h("div", { class: "note" }, "This interaction has rolled out of the buffer.");
  return frag(
    h("div", { class: "call-lbl" }, "Request"),
    h("pre", { class: "logs" }, fmtJson(full.body || "")),
    h("div", { class: "call-lbl" }, "Response",
      full.response ? null : h("span", { style: "color:var(--text-3)" }, " — none —")),
    h("pre", { class: "logs" },
      full.response ? fmtJson(full.response)
        : h("span", { style: "color:var(--text-3)" }, "no reply captured for this entry")));
}

/** Fetch one interaction's raw request and reply, then paint it into the already-open row. */
async function loadTrafficBody(seq        )                {
  if (traffic.full[seq]) return;
  const j = await apiJson("/api/traffic/" + encodeURIComponent(seq));
  // A 404 means the ring rolled past it while the row sat there; say so rather than spin forever.
  traffic.full[seq] = j || { gone: true }                  ;
  const node = document.querySelector('#pane .call[data-tseq="' + seq + '"] .call-body');
  if (node) fill(node               , trafficBodyNode({ seq: seq }));
}
/** Expand/collapse one row's raw JSON in place — a poll must not close what you just opened. */
function toggleTraffic(seq        )       {
  traffic.open[seq] = !traffic.open[seq];
  const node = document.querySelector('#pane .call[data-tseq="' + seq + '"]');
  if (node) node.className = "call" + (traffic.open[seq] ? " open" : "");
  if (traffic.open[seq]) void loadTrafficBody(seq);
}
function renderTraffic()       {
  // `entries` is one page; `clients`, `total` and `all` describe the whole ring (server-computed).
  const entries = traffic.rows || [];
  const clients = traffic.clients || [];
  const total = traffic.total || 0;
  const all = traffic.all || 0;
  const sel = traffic.client;

  // Controls: action/everything toggle + clear. This whole pane is the Traffic view's content.
  const ctrl = h("div", { class: "sec-head" },
    h("span", { class: "sec-cap" }, "Traffic"),
    h("span", { style: "display:flex;gap:var(--s2);align-items:center" },
      h("div", { class: "seg", id: "trFilter" },
        h("button", { data: { filter: "actions" }, aria: { selected: traffic.filter !== "all" ? "true" : "false" } }, "Actions"),
        h("button", { data: { filter: "all" }, aria: { selected: traffic.filter === "all" ? "true" : "false" } }, "Everything")),
      h("button", { class: "btn", id: "trClear", disabled: !all }, sel ? "Clear client" : "Clear")));

  // Clients — who has been talking to the gateway, folded from recorded traffic (stateless: no live
  // connection to query). Click a row to filter the activity log to that client.
  let clientBlock        ;
  if (clients.length) {
    // One line per client (docs/18 V4): name, token, paths, last seen, request count — a
    // five-column grid, not a card-per-client with two lines of prose.
    const head = h("div", { class: "cli-head" },
      h("span", null, "Client"), h("span", null, "Token"), h("span", null, "Paths"),
      h("span", null, "Last"), h("span", { class: "cli-n" }, "Requests"), h("span"));
    const rows = clients.map((c) => {
      const isSel = sel === c.key;
      const mcps = (c.mcps || []).map((m) => { return "/mcp/" + m; }).join(" ");
      const tokens = c.tokens || [];
      const tokenLine = tokens.length ? tokens.join(", ") : "no token";
      return h("div", { class: "cli-row" + (isSel ? " sel" : ""), data: { ckey: c.key }, role: "button", tabIndex: 0 },
        h("span", { class: "cli-name" }, h("code", null, c.label)),
        h("span", { class: "cli-token" }, tokenLine),
        h("span", { class: "cli-paths" }, mcps),
        h("span", { class: "cli-last" }, ago(c.lastAt)),
        h("span", { class: "cli-n" }, c.count.toLocaleString()),
        isSel
          ? h("button", { class: "btn ghost icon", data: { cclr: "" }, title: "Stop filtering" }, iconNode("x"))
          : h("span", { class: "cli-x" }));
    });
    clientBlock = frag(
      h("div", { class: "cap", style: "padding-top:var(--s5)" }, "Clients · " + clients.length),
      h("div", { class: "group" }, head, rows));
  } else {
    clientBlock = frag(
      h("div", { class: "cap", style: "padding-top:var(--s5)" }, "Clients"),
      h("div", { class: "group" }, h("div", { class: "row" },
        h("span", { class: "rowmsg" }, "No clients yet. When a client sends its first request (initialize, tools/list, …) it appears here with the MCPs it is using."))));
  }

  // Activity log — de-noised by the toggle, narrowed by a selected client.
  const actCap = "Activity · " + (traffic.filter === "all" ? "everything" : "actions only");
  const countTxt = total.toLocaleString() + (total !== all ? " of " + all.toLocaleString() : "") + " interactions";
  const actHead = h("div", { class: "sec-head", style: "padding-top:var(--s5)" },
    h("span", { class: "sec-cap" }, actCap),
    h("span", { class: "hint" }, countTxt));
  let body        ;
  if (!all) {
    body = h("div", { class: "group" }, h("div", { class: "row" },
      h("span", { class: "rowmsg" }, "No interactions yet. Every JSON-RPC request a client sends — initialize, tools/list, resources/read, tools/call — is recorded here; the clients above are summarized from it.")));
  } else if (!entries.length) {
    const hint = traffic.page
      ? "Nothing on this page."
      : sel
        ? "No activity for this client" + (traffic.filter !== "all" ? " in actions-only view." : ".")
        : (traffic.filter !== "all" ? "No actions yet — switch to Everything to see the protocol handshake." : "No interactions.");
    body = h("div", { class: "group" }, h("div", { class: "row" }, h("span", { class: "rowmsg" }, hint)));
  } else {
    body = h("div", { class: "group" }, entries.map(trafficRowNode));
  }

  // Newer/Older, worded and shaped exactly like the tool-call log's pager — same direction, so
  // "Newer" always means toward the top of a newest-first list in both views.
  const pager         = (traffic.page > 0 || traffic.more)
    ? h("div", { class: "pager" },
        h("button", { class: "btn", id: "trPrev", disabled: traffic.page <= 0 }, "Newer"),
        h("span", null, "Page " + (traffic.page + 1)),
        h("button", { class: "btn", id: "trNext", disabled: !traffic.more }, "Older"))
    : null;

  // Wrapped in .wide: an interaction row is method + params + client + timing on one line, which the
  // standard measure cannot hold. The wiring below queries nothing: ONE delegated click and ONE
  // delegated keydown on #pane (docs/37 R5) answer every control, rows and pager included, and a
  // repaint re-assigns the same properties instead of re-attaching per-row handlers.
  fill($("pane"), h("div", { class: "wide" }, ctrl, clientBlock, actHead, body, pager));

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
    const tog = t.closest             ("[data-tog]");
    if (tog) { toggleTraffic(Number(tog.dataset.tog)); return; }
    if (t.id === "trClear") { void clearTraffic(); }
  };
  pane.onkeydown = (ev               )       => {
    if (ev.key !== "Enter" && ev.key !== " ") return;
    const t = targetEl(ev);
    if (!t) return;
    const crow = t.closest             ("[data-ckey]");
    if (crow) { ev.preventDefault(); crow.click(); return; }
    const tog = t.closest             ("[data-tog]");
    if (tog) { ev.preventDefault(); toggleTraffic(Number(tog.dataset.tog)); }
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

export { ago, loadTraffic, loadTrafficBody, renderTraffic, toggleTraffic, trafficBodyNode, trafficPageStep, trafficReload, trafficRowNode };

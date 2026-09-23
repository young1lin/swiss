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

                                                                          
import { $, KINDS, api, apiJson, isMcpKind, now, toast } from "./util.js";
import { readFields, translateOauth, translatePg } from "./fields.js";
import { callsErrNode, callsStatusNode, goneNode, repaintCallBlock } from "./logs.js";
import { fill } from "./h.js";
// The one-field sheet (fix-plan #16). Same accepted cycle shape as data-view/data-tabs:
// add-sheet imports detail's openDetail, detail imports its sheet builder, and both sides
// only call across inside functions, never at module scope.
import { openFieldSheet } from "./add-sheet.js";
import { patchSidebar } from "./menu.js";
import { patchDetailHead, renderPane } from "./pane.js";
import { loadList } from "./polling.js";
import { renderCallsOnly } from "./run-history.js";
import { rowOf } from "./sidebar.js";
import { setMenuOpen } from "./ui-state.js";
import { clearMcpBusy, mcpBusyVerb, mcpDetail, selectedMcp, setLastAction, setMcpBusy, setMcpDetail, setSelectedMcp } from "./mcp-state.js";
import { tr, trn } from "./i18n.js";

/* --- lifecycle actions ------------------------------------------------------------------------ */
async function act(name        , verb        )                {
  if (mcpBusyVerb(name)) return;
  setMcpBusy(name, verb);
  patchSidebar(); patchDetailHead();
  // docs/28 D2: the wire keeps the stop/start verbs; the panel says disable/enable — a stop
  // that survives a boot and refuses every client is a disable, and the word owed it.
  const shown = verb === "stop" ? tr("detail.disable") : verb === "start" ? tr("detail.enable") : verb === "restart" ? tr("detail.restart") : verb;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/" + verb, { method: "POST" });
    const j = await r.json();
    if (!r.ok) {
      setLastAction(name, { msg: tr("detail.verbFailedError", { verb: shown, error: j.error || tr("detail.httpN", { n: r.status }) }), err: true, at: now() });
      toast(tr("detail.nameError", { name, error: j.error || tr("detail.failed") }), true);
    } else {
      const state_word = j.lifecycle === "stopped" ? "disabled" : j.lifecycle;
      // Record it and say it from the SAME object — reading the row back to quote it was
      // only ever a way to repeat what this line already knows.
      const done = { msg: tr("detail.verbState", { verb: shown, state: state_word || tr("detail.ok") }), err: false, at: now() };
      setLastAction(name, done);
      toast(tr("detail.nameMsg", { name, msg: done.msg }));
    }
  } catch (e) {
    setLastAction(name, { msg: tr("detail.verbRequestFailed", { verb: shown }), err: true, at: now() });
  }
  clearMcpBusy(name);
  await loadList();
  // The server rebuilt on start/restart, so any cached page list is stale.
  const d = mcpDetail();
  if (d && d.name === name) {
    KINDS.forEach((k) => { d[k] = pageState(); });
    renderPane();
    void loadMeta(name);
    if (isMcpKind(d.tab)) void loadPage(name, d.tab);
  }
}

/* fix-plan #16: the rename is the one-field sheet (openFieldSheet), the same surface as a
 * group rename and a table rename. The sheet owns "required" and "unchanged is a cancel";
 * a POST refusal keeps the sheet open with the typed name still in it. */
function renameMcp(name        )       {
  openFieldSheet({
    title: tr("detail.renameName", { name }),
    def: name,
    submit: async (next        )                   => {
      const ok = await apiJson("/api/mcps/" + encodeURIComponent(name) + "/rename", { method: "POST", body: JSON.stringify({ name: next }) });
      if (!ok) return false;
      if (selectedMcp() === name) setSelectedMcp(next);
      const open = mcpDetail();
      if (open && open.name === name) open.name = next;
      toast(tr("detail.renamedNameNext", { name, next }));
      await loadList();
      renderPane();
      return true;
    },
  });
}

async function removeMcp(name        )                {
  // Config-sourced MCPs are removed from gateway.config.json too (server-side), so the confirm
  // says so — "removes it permanently" alone used to hide that the file edit is part of it.
  const fromConfig = mcpDetail()?.source === "config";
  if (!confirm(fromConfig
      ? tr("detail.deleteNameStopsRemoves", { name })
      : tr("detail.deleteNameStopsRemovesPermanently", { name }))) return;
  const ok = await apiJson("/api/mcps/" + encodeURIComponent(name), { method: "DELETE" });
  if (!ok) return;
  if (selectedMcp() === name) { setSelectedMcp(null); setMcpDetail(null); }
  toast(tr("detail.deletedName", { name }));
  await loadList();
  renderPane();
}

/* --- OAuth authorize (docs/24 D5) -------------------------------------------------------------- */
/** One click, one flow: the POST plants it (or hands back the live one — the server
 *  single-flights per name), this opens the provider's consent page in a real browser
 *  window the moment its URL exists, and keeps polling every 3s until approval lands
 *  (credentials stored, MCP auto-started) or the flow errors. Status rides the pane's
 *  lastAction note; renderPane is skipped while a config edit is open, so a status update
 *  never eats a form the user is filling in. */
function pause(ms        )                { return new Promise((res)       => { setTimeout(res, ms); }); }

async function authorizeMcp(name        )                {
  const d = mcpDetail();
  if (!d || d.name !== name || d.oauthBusy) return;
  d.oauthBusy = true;
  const note = (msg        , err          )       => {
    setLastAction(name, { msg: msg, err: !!err, at: now() });
    const open = mcpDetail();
    if (!open || open.editing) return;
    renderPane();
  };
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/authorize", { method: "POST" });
    const j = await r.json().catch(() => { return {}; });
    if (!r.ok) { note(tr("detail.authorizeFailedError", { error: j.error || tr("detail.httpN", { n: r.status }) }), true); return; }
    note(tr("detail.authorizePreparingConsentPage"));
    let opened = false;
    // 100 polls x 3s = 5 min, the server flow's own callback cap.
    for (let i = 0; i < 100; i++) {
      if (i > 0) await pause(3000);
      const p = await api("/api/mcps/" + encodeURIComponent(name) + "/authorize");
      const s = await p.json().catch(() => { return {}; });
      if (s.status === "authorization_required" && s.authorizationUrl && !opened) {
        opened = true;
        window.open(s.authorizationUrl, "_blank");
        note(tr("detail.authorizeConsentPageOpened"));
      } else if (s.status === "approved") {
        setLastAction(name, { msg: tr("detail.authorizedWhat", { what: s.tools != null ? trn(s.tools, "detail.nTools.one", "detail.nTools.other") : tr("detail.mcpStarted") }), err: false, at: now() });
        toast(tr("detail.nameMsg", { name, msg: tr("detail.authorized") }));
        await loadList();
        void loadMeta(name);
        const open = mcpDetail();
        if (open && !open.editing) renderPane();
        return;
      } else if (s.status === "error") {
        note(tr("detail.authorizeFailedError", { error: s.error || tr("detail.unknownError") }), true);
        await loadList();
        return;
      }
      // starting, or the URL not yet in this answer: keep polling.
    }
    note(tr("detail.authorizeTimedWaitingApproval"), true);
  } catch (e) {
    note(tr("detail.authorizeRequestFailed"), true);
  } finally {
    const open = mcpDetail();
    if (open && open.name === name) open.oauthBusy = false;
  }
}

/* --- detail data ------------------------------------------------------------------------------ */
function pageState()                {
  return { items: [], nextCursor: undefined, total: undefined, pageSize: 50, cursors: [""], loading: false, loaded: false, error: null };
}
/** After a restart, restore or config save the three kind pages are stale: open them fresh
 *  on whatever detail is showing now (the async caller's detail may have been closed). */
function resetKindPages()       {
  const d = mcpDetail();
  if (d) KINDS.forEach((k) => { d[k] = pageState(); });
}

function openDetail(name        )       {
  if (mcpDetail()?.name === name) return;
  setSelectedMcp(name);
  const d            = {
    name: name, tab: "tools", config: null, source: undefined, editing: false, editType: null, editVals: null,
    // OAuth (docs/24 D5): the detail's auth state ("authorized" | "needs-auth" | undefined),
    // and whether an authorize flow this panel started is still polling.
    oauth: undefined, oauthBusy: false,
    run: {
      tool: null, result: null, running: false,
      // The refill control next to Run: one tool's newest recorded runs (hist), which tool they
      // belong to (histTool — null/other means stale and needs (re)loading), an in-flight fetch,
      // whether the popover is open, the seq being previewed, full entries cached by seq, and the
      // search box's live query (histQ — the server matches it against the FULL stored arguments).
      // histFull MUST be initialized: member access on undefined (histFull[seq]) throws, which is
      // exactly how a missing field here once broke both the preview pane and the refill.
      hist: null, histTool: null, histLoading: false,
      histOpen: false, histSelSeq: null, histFull: {}, histQ: "",
    },
    // Logs tab: a page of recorded tool calls (null until loaded), any child stderr, which rows are
    // expanded, replies fetched in full by seq, and the server-side search needle (docs/31).
    // docs/32 B1: paging is a transaction — callsPage is ALWAYS the committed page; a switch in
    // flight lives in callsPendingPage, its visible failure in callsError (with the target a
    // Retry owes in callsRetryTarget), and callsRequest is the generation that drops stale
    // responses before they can commit over a newer needle/page.
    calls: null, stderr: "", callsOpen: {},
    callsPage: 0, callsMore: false, callsFull: {}, callsGone: {}, callsQ: "",
    callsPendingPage: null, callsError: "", callsErrStatus: "", callsRetryTarget: null,
    callsRetryDir: null, callsSwitch: null, callsRequest: 0, callsActive: 0,
    callsAll: {}, // docs/33 C3: blocks the operator expanded past the line cap ("out:<seq>")
    // The three kind pages (views/mcps.ts stages the active one under d[d.tab]) open fresh.
    tools: pageState(), resources: pageState(), prompts: pageState(),
  };
  setMcpDetail(d);
  setMenuOpen(false);
  patchSidebar();
  renderPane();
  void loadMeta(name);
  if (rowOf(name) && rowOf(name) .lifecycle === "started") void loadPage(name, "tools");
}

async function loadMeta(name        )                {
  // Compare the detail OBJECT, not its name: leaving an MCP and coming back builds a fresh detail
  // under the same name, and a slow response from the first visit would otherwise write into the
  // second one. loadPage already does it this way.
  const d = mcpDetail();
  const d_ = d ;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/details");
    if (!r.ok) return;
    const j = await r.json();
    if (mcpDetail() !== d) return;
    d_.config = j.config || null;
    d_.source = j.source;
    d_.tunnels = j.tunnels || [];
    d_.oauth = j.oauth;
    if (d_.editing) return; // never rebuild a form the user is filling in
    // The pane header grows an Authorize button the moment the config is an OAuth one (an
    // http def that says so, or the figma type that implies it), so the pane re-renders for
    // those MCPs too — not only when the config tab is open.
    if (d_.tab === "config" || (d_.config && (d_.config.auth === "oauth" || d_.config.type === "figma"))) renderPane();
  } catch (e) { /* handled */ }
}

/** One page of recorded tool calls (arguments + reply) and, for a proc MCP, its stderr.
 * docs/32 B1: a load is a transaction — a response may only commit while it is still the newest
 * request for this detail (generation), and it commits exactly the page it asked for. The page,
 * rows, more-flag and stderr land together or not at all; a failure keeps everything committed
 * and shows itself in place with a Retry.
 * docs/32 B3: the poll (isPoll) refreshes the LIVE page only — never an offset page, never a
 * switch in mid-transaction — and its failures are silent: the poll says nothing the user asked
 * for, so it takes nothing away either. */
async function loadCalls(name        , isPoll          )                {
  const d = mcpDetail();
  if (!d || d.name !== name) return;
  // A poll dispatched while a FOREGROUND load still hangs would take the newest generation
  // for itself; the foreground failure would then land as "stale" and be reported to nobody.
  // The poll is a courtesy refresh — skipping one cycle costs 6 s, a swallowed failure costs
  // the user's trust (found live: a hung re-entry load during a server stop).
  if (isPoll && (d.callsPage > 0 || d.callsPendingPage != null || d.callsActive > 0)) return;
  const target = d.callsPendingPage != null ? d.callsPendingPage : d.callsPage;
  const gen = ++d.callsRequest;
  d.callsActive = (d.callsActive || 0) + 1;
  // A page switch rides this request; take its anchor intent now, so a request issued later (a
  // new needle) can never spend an anchor that belonged to this one (docs/32 B2).
  let sw                                                                      = null;
  if (d.callsPendingPage === target && d.callsSwitch) { sw = d.callsSwitch; d.callsSwitch = null; }
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/calls?page=" + target +
      (d.callsQ ? "&q=" + encodeURIComponent(d.callsQ) : ""));
    const j = await r.json().catch(() => { return {}; });
    // A revisit built a new detail object (see loadMeta), or a newer request superseded this
    // one — either way this response is stale and must not commit.
    if (mcpDetail() !== d || gen !== d.callsRequest) return;
    if (!r.ok) { if (!isPoll) callsLoadFailed(d, target, r.status, sw); return; }
    d.callsPage = target;
    if (d.callsPendingPage === target) d.callsPendingPage = null;
    d.callsError = "";
    d.callsErrStatus = "";
    d.callsRetryTarget = null;
    d.calls = j.calls || [];
    d.callsMore = !!j.more;
    d.stderr = j.stderr || "";
    renderCallsOnly();
    if (sw) restoreCallsAnchor(sw, d);
  } catch (e) {
    if (mcpDetail() !== d || gen !== d.callsRequest) return;
    if (!isPoll) callsLoadFailed(d, target, 0, sw);
  } finally {
    d.callsActive -= 1; // every exit path settles its own in-flight mark
  }
}

/** A foreground load failed: the committed page, its rows, the open expansions and the scroll
 *  position all stay exactly as they are; the failure says so in place and offers the same
 *  target again (docs/32 B1). */
function callsLoadFailed(d           , target        , status        , sw                               )       {
  d.callsPendingPage = null;
  d.callsSwitch = null;
  d.callsError = tr("detail.couldLoadCalls");
  d.callsErrStatus = status ? tr("detail.httpN", { n: status }) : "";
  d.callsRetryTarget = target;
  d.callsRetryDir = sw && sw.dir ? sw.dir : null; // a Retry re-anchors toward the same direction
  if (d.calls == null) renderCallsOnly(); // nothing painted yet — the spinner becomes the error
  else patchCallsChrome(d);
}

/** Newer/Older: begin a page switch. Nothing committed changes until the response lands — the
 *  rows stay on screen, marked busy, and the request carries the target page (docs/32 B1). */
function callsPageStep(delta        , opts                        )       {
  opts = opts || {};
  const d = mcpDetail();
  if (!d || d.callsPendingPage != null) return; // one switch at a time
  const next = d.callsPage + delta;
  if (next < 0 || (delta > 0 && !d.callsMore)) return;
  callsBegin(d, next, delta < 0 ? "newer" : "older", !!opts.fromKey);
}

/** Retry of a failed foreground load: the same target again, through the same transaction — and
 *  through the same anchor contract, with the direction the failed switch had. */
function callsRetry(opts                        )       {
  opts = opts || {};
  const d = mcpDetail();
  if (!d || d.callsPendingPage != null || d.callsRetryTarget == null) return;
  callsBegin(d, d.callsRetryTarget, d.callsRetryDir, !!opts.fromKey);
}

/** Begin a switch toward a target page. The anchor — the pager's viewport top, the direction,
 *  and whether a keyboard drove the action — rides the REQUEST (not the detail), so an anchor
 *  can never be spent by a response it did not belong to (docs/32 B2). */
function callsBegin(d           , target        , dir               , fromKey         )       {
  d.callsError = "";
  d.callsErrStatus = "";
  d.callsRetryTarget = null;
  d.callsPendingPage = target;
  const pager = $("clPager");
  d.callsSwitch = (pager && pager.getBoundingClientRect)
    ? { dir: dir, fromKey: fromKey, pagerTop: pager.getBoundingClientRect().top }
    : null; // without a pager on screen there is nothing to hold still (the very first load)
  patchCallsChrome(d);
  void loadCalls(d.name);
}

/** After a committed switch: put the pager back where it was on screen — the button the user
 *  clicked is where their hand and eye already are — and hand keyboard drivers their focus back
 *  on the equivalent button, falling to the other direction at a boundary. Never scrollIntoView:
 *  that drags the whole app shell (docs/32 B2). */
function restoreCallsAnchor(sw                                                              , d           )       {
  const pane = $("pane");
  const pager = $("clPager");
  if (pane && pager && pager.getBoundingClientRect) {
    pane.scrollTop += pager.getBoundingClientRect().top - sw.pagerTop ;
  }
  if (!sw.fromKey) return; // a mouse switch leaves focus alone — never steal the search box
  // Which button may take focus comes from the COMMITTED state, never from a node property:
  // the fresh markup in a real browser, and the same truth everywhere else.
  const newerOk = d.callsPage > 0;
  const olderOk = d.callsMore;
  const pickOk = sw.dir === "newer" ? newerOk : olderOk;
  const otherOk = sw.dir === "newer" ? olderOk : newerOk;
  const to = pickOk ? (sw.dir === "newer" ? $("clPrev") : $("clNext"))
    : otherOk ? (sw.dir === "newer" ? $("clNext") : $("clPrev"))
    : $("clStatus");
  if (to && to.focus) to.focus();
}

/** Sync the pager chrome — busy state, both buttons, the status cell, the error block — onto
 *  the PAINTED dom without repainting: a pending switch or a failed one must not detach the
 *  rows, the search input or the scroll position (docs/32 B1). */
function patchCallsChrome(d           )       {
  const busy = d.callsPendingPage != null;
  const region = $("callsRegion");
  if (region && region.setAttribute) region.setAttribute("aria-busy", busy ? "true" : "false");
  const status = $("clStatus");
  if (status) fill(status, callsStatusNode(d));
  const prev = $                   ("clPrev"), next = $                   ("clNext");
  if (prev) prev.disabled = busy || d.callsPage <= 0;
  if (next) next.disabled = busy || !d.callsMore;
  // The error block is idempotent — drop whatever is there, then insert when an error is set.
  // A new switch (pending or Retry) takes the old error away; a failure puts it back at the end
  // of the region — after the pager when one is painted, after the rows on a single-page log
  // that has none. The full repaint composes the same order (logs.js: body + pager + err).
  const err = $("clErr");
  if (err && err.remove) err.remove();
  if (d.callsError) {
    const regionEl = $("callsRegion");
    if (regionEl) regionEl.append(callsErrNode(d));
    const retry = $("clRetry");
    // The same keyboard contract as the painted wiring (run-history): a Retry driven by
    // Enter/Space (detail === 0) owes the user their focus back once the retry commits.
    if (retry) retry.onclick = (ev) => { callsRetry({ fromKey: !!ev && ev.detail === 0 }); };
  }
}

/* Replies being fetched right now, by "<mcp>:<seq>": opening, closing and reopening a row (or a
   click on the button while the open's own fetch is out) must not stack requests for one body. */
const fullInFlight = new Set        ();

/** Fetch one reply in full — the log page ships only the first 2 KB of each. Runs when a clipped
 *  row opens (docs/33 C3) and from its Show full result button; a no-op once the reply is here,
 *  known pruned, or already on its way. */
async function showFullResult(seq        )                {
  const d = mcpDetail();
  if (!d) return;
  const c = (d.calls || []).find((r) => { return r.seq === seq; });
  if (c && !c.preview) return; // the page already carried the whole reply
  if (d.callsFull[seq] != null || (d.callsGone || {})[seq]) return;
  const flight = d.name + ":" + seq;
  if (fullInFlight.has(flight)) return;
  fullInFlight.add(flight);
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls/" + encodeURIComponent(seq));
    if (!r.ok) { toast(tr("detail.httpN", { n: r.status }), true); return; }
    const j = await r.json();
    if (mcpDetail()?.name !== d.name || !j.call) return;
    if (j.call.bodyGone) {
      // Only the newest replies keep their payload; say which part is missing rather than showing a
      // short result as if it were whole. In place, not a toast: the fetch now runs on a plain
      // open, and the row is where the operator is looking.
      if (!d.callsGone) d.callsGone = {};
      d.callsGone[seq] = true;
      const btn = document.querySelector('#tabbody [data-full="' + seq + '"]');
      if (btn) (btn.closest(".form-actions") || btn).replaceWith(goneNode(seq));
      return;
    }
    d.callsFull[seq] = j.call.output;
    // docs/33 C3: the result block repaints from the whole reply — a preview clipped mid-value
    // could not be formatted, the full one usually can.
    repaintCallBlock(d, "out:" + seq);
    const btn = document.querySelector('#tabbody [data-full="' + seq + '"]');
    if (btn) (btn.closest(".form-actions") || btn).remove();
  } catch (e) { toast(tr("detail.requestFailed"), true); }
  finally { fullInFlight.delete(flight); }
}

async function clearCalls()                {
  const d = mcpDetail();
  if (!d) return;
  // docs/32 B4: one mis-click removes the index AND the stored full replies, and nothing can
  // undo it — so the confirm names both costs, and a cancelled confirm fires no request at all.
  if (!confirm(tr("detail.clearAllRecordedTool", { name: d.name }))) return;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls", { method: "DELETE" });
    if (!r.ok) { toast(tr("detail.httpN", { n: r.status }), true); return; }
    // The clear is part of the same transaction space as the paging loads: bump the generation so
    // a response that left before the DELETE (a poll, a parked switch — confirm blocks the event
    // loop, not the network) lands stale and cannot repaint rows over the emptied log.
    d.callsRequest++;
    d.calls = [];
    d.callsOpen = {};
    d.callsFull = {};
    d.callsGone = {};
    d.callsAll = {};
    d.callsPage = 0;
    d.callsMore = false;
    d.callsPendingPage = null;
    d.callsSwitch = null;
    d.callsError = "";
    d.callsErrStatus = "";
    d.callsRetryTarget = null;
    d.callsRetryDir = null;
    toast(tr("detail.callLogCleared"));
    renderCallsOnly();
  } catch (e) { toast(tr("detail.requestFailed"), true); }
}

async function loadPage(name        , kind         )                {
  const d = mcpDetail();
  if (!d || d.name !== name) return;
  const kd = d[kind];
  if (kd.loading) return;
  kd.loading = true;
  kd.error = null;
  if (d.tab === kind) renderPane();
  try {
    const cursor = kd.cursors[kd.cursors.length - 1];
    const url = "/api/mcps/" + encodeURIComponent(name) + "/" + kind + (cursor ? "?cursor=" + encodeURIComponent(cursor) : "");
    const r = await api(url);
    const j = await r.json().catch(() => { return {}; });
    if (r.ok) {
      kd.items = j[kind] || [];
      kd.nextCursor = j.nextCursor;
      kd.total = j.total;
      kd.pageSize = j.pageSize || kd.pageSize;
      kd.loaded = true;
      if (kind === "tools") kd.disabled = j.disabledTools || [];
      if (kind === "resources") kd.resourceEnabled = j.resourceEnabled !== false;
    } else {
      // Surface it instead of silently showing "none" — a direct adapter has no resources/prompts
      // capability at all, and that is worth saying out loud.
      kd.error = j.error || "HTTP " + r.status;
    }
  } catch (e) {
    kd.error = tr("detail.requestFailed");
  }
  kd.loading = false;
  // Run builds its form from the tool list, so it also needs a repaint when tools land.
  if (mcpDetail() === d && (d.tab === kind || (d.tab === "run" && kind === "tools"))) renderPane();
}

function showTab(tab        )       {
  const d = mcpDetail();
  if (!d) return;
  d.tab = tab;
  d.editing = false;
  renderPane();
  if (isMcpKind(tab) && !d[tab].loaded && !d[tab].loading) void loadPage(d.name, tab);
  // The config tab's revision list (docs/28 D1) rides along with the tab, not the poll.
  if (tab === "config") void loadRevisions(d.name);
  // Run needs the tool list to build its argument form.
  if (tab === "run" && !d.tools.loaded && !d.tools.loading) void loadPage(d.name, "tools");
  // docs/32 B3: a pending switch already owns the tab; re-entering it must not fire a second
  // request for the same target on top of the one in flight.
  if (tab === "logs" && d.callsPendingPage == null) void loadCalls(d.name);
}
/** The pager's target: the open detail's active kind page, or null when the tab is not a
 *  kind page (run / config / logs have no pager). */
function activeKindPage()                                                            {
  const d = mcpDetail();
  if (!d || !isMcpKind(d.tab)) return null;
  return { d, kind: d.tab, kd: d[d.tab] };
}
function pageNext()       {
  const at = activeKindPage();
  if (!at || !at.kd.nextCursor || at.kd.loading) return;
  at.kd.cursors.push(at.kd.nextCursor);
  void loadPage(at.d.name, at.kind);
}
function pagePrev()       {
  const at = activeKindPage();
  if (!at || at.kd.cursors.length <= 1 || at.kd.loading) return;
  at.kd.cursors.pop();
  void loadPage(at.d.name, at.kind);
}

/* --- config edit ------------------------------------------------------------------------------ */
function startEdit()       {
  const d = mcpDetail();
  if (!d || !d.config) return;
  d.editing = true;
  d.editMode = "edit";
  d.editType = d.config.type           || "proc";
  d.editVals = null; // start from what is stored
  renderPane();
}
// docs/28 D1: the same form, another verb — Save parks the current def as a revision and
// installs the new one under the SAME name. The operator's rollback lives one click away.
function startReplace()       {
  const d = mcpDetail();
  if (!d || !d.config) return;
  d.editing = true;
  d.editMode = "replace";
  d.editType = d.config.type           || "proc";
  d.editVals = null;
  renderPane();
}
function cancelEdit()       {
  const d = mcpDetail();
  if (!d) return;
  d.editing = false;
  d.editMode = null;
  d.editType = null;
  d.editVals = null;
  renderPane();
}

/* --- def revisions (docs/28 D1) ---------------------------------------------------------------- */
async function loadRevisions(name        )                {
  const d = mcpDetail();
  if (!d || d.name !== name) return;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/revisions");
    if (!r.ok) return;
    const j = await r.json();
    if (mcpDetail() !== d) return; // a revisit built a new detail object — see loadMeta
    d.revisions = j.revisions || [];
    if (d.tab === "config" && !d.editing) renderPane();
  } catch (e) { /* handled */ }
}

async function saveReplace()                {
  const d = mcpDetail();
  if (!d) return;
  const type = d.editType || (d.config && d.config.type          ) || "proc";
  const fields = readFields(type, "e-");
  const body = Object.assign({ type: type }, fields);
  if (body.autostart !== undefined) { body.lazy = !body.autostart; delete body.autostart; }
  translateOauth(body);
  translatePg(type, body); // docs/30: the pg form's pieces become one url
  const noteEl = $                  ("e-note");
  if (noteEl) body.note = noteEl.value;
  if (type === "proc" && !body.command) { toast(tr("detail.commandRequired"), true); return; }
  const name = d.name;
  const restore = () => {
    if (!mcpDetail() || mcpDetail() !== d) return;
    d.editing = true;
    d.editType = type;
    d.editVals = fields;
  };
  setMcpBusy(name, "save");
  d.editing = false;
  renderPane(); patchSidebar();
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/replace", { method: "POST", body: JSON.stringify(body) });
    const j = await r.json();
    if (!r.ok) {
      setLastAction(name, { msg: tr("detail.replaceFailedError", { error: j.error || tr("detail.httpN", { n: r.status }) }), err: true, at: now() });
      toast(j.error || tr("detail.replaceFailed"), true);
      restore();
    } else {
      setLastAction(name, { msg: tr("detail.defReplacedRevisionN", { n: j.revisions || "?" }), err: false, at: now() });
      toast(tr("detail.nameMsg", { name, msg: tr("detail.replacedPreviousDefParked", { n: j.revisions || "?" }) }));
      // The swap stands even when the new def will not start; the panel must say so, not hide it.
      if (j.restartError) toast(tr("detail.nameFailedStartError", { name, error: j.restartError }), true);
      if (mcpDetail() === d) d.editVals = null;
      resetKindPages();
      void loadMeta(name);
      void loadRevisions(name);
    }
  } catch (e) {
    setLastAction(name, { msg: tr("detail.replaceRequestFailed"), err: true, at: now() });
    toast(tr("detail.replaceRequestFailed"), true);
    restore();
  }
  clearMcpBusy(name);
  await loadList();
  renderPane();
}

async function restoreRevision(index        )                {
  const d = mcpDetail();
  if (!d) return;
  if (!confirm(tr("detail.restoreRevisionNCurrent", { n: index + 1 }))) return;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/revisions/" + index + "/restore", { method: "POST", body: "{}" });
    const j = await r.json();
    if (!r.ok) { toast(j.error || tr("detail.restoreFailed"), true); return; }
    toast(tr("detail.nameMsg", { name: d.name, msg: tr("detail.revisionNRestored", { n: index + 1 }) }));
    if (j.restartError) toast(tr("detail.nameFailedStartError", { name: d.name, error: j.restartError }), true);
    resetKindPages();
    void loadMeta(d.name);
    void loadRevisions(d.name);
    await loadList();
    renderPane();
  } catch (e) { toast(tr("detail.restoreRequestFailed"), true); }
}

async function deleteRevision(index        )                {
  const d = mcpDetail();
  if (!d) return;
  if (!confirm(tr("detail.deleteParkedRevisionN", { n: index + 1 }))) return;
  if (!await apiJson("/api/mcps/" + encodeURIComponent(d.name) + "/revisions/" + index, { method: "DELETE" })) return;
  toast(tr("detail.revisionNDeleted", { n: index + 1 }));
  void loadRevisions(d.name);
}
/** Switching type re-renders the form, so read what is in it first and carry it across — a field
 *  both types share (description, host, password) survives the switch. A masked secret carried into
 *  a type that never stored one is dropped server-side by unmaskBody, never saved as dots. */
function changeEditType(t        )       {
  const d = mcpDetail();
  if (!d) return;
  const prev = d.editType || (d.config && d.config.type          ) || "proc";
  d.editVals = Object.assign({}, d.editVals, readFields(prev, "e-"));
  d.editType = t;
  renderPane();
}

/**
 * Test connection — the DB types' form carries this beside Save. It POSTs the form's CURRENT
 * values (env refs intact) to /api/mcpdefs/test, which expands them server-side and opens a real
 * driver connection with exactly these credentials — the same adapter and ping the health probe
 * uses. Nothing is saved: this is "will these values work", asked before committing them.
 */
async function runConnTest(p        )                {
  // p is the form's id prefix: "e-" for the inline editor, "a-" for the Add sheet.
  const d = mcpDetail();
  const type = p === "a-" ? $                   ("a-type").value : (d && (d.editType || (d.config && d.config.type          ))) || "proc";
  const body = Object.assign({ type: type }, readFields(type, p));
  translatePg(type, body); // docs/30: pg's split fields travel as the url the server tests
  delete body.autostart; // a boot-time switch, not a credential — irrelevant to a connection test
  // figma implies OAuth the way a checked auth box states it: no keyless test exists for either.
  const wantsOauth = type === "figma" || (type === "http" && body.auth === true);
  delete body.auth; delete body.oauthClientName; // a credential question, not a connectivity one
  const btn = $                   (p + "test"), out = $(p + "test-out");
  if (!btn || !out) return;
  // A keyless handshake cannot test an OAuth remote: its endpoint answers 401 until the flow
  // runs, and the flow needs the MCP saved first (credentials are name-keyed). Say so rather
  // than firing a request whose only possible answer is the 401.
  if (wantsOauth) {
    out.textContent = tr("detail.oauthRemoteUseAuthorize");
    out.style.color = "";
    return;
  }
  // A metered vision API: the only honest "test" is a real generation, and that spends
  // tokens. The health probe already skips it for the same reason — say so instead.
  if (type === "zai-vision") {
    out.textContent = tr("detail.meteredGlmApiEvery");
    out.style.color = "";
    return;
  }
  btn.disabled = true;
  btn.textContent = tr("detail.testing");
  out.hidden = false;
  out.textContent = tr("detail.connectingTheseExactValues");
  out.style.color = "";
  try {
    const r = await api("/api/mcpdefs/test", { method: "POST", body: JSON.stringify(body) });
    const j = await r.json();
    if (j.ok) {
      // For a rest target any HTTP answer is reachable — show which one came back (404 from the
      // base path is fine; the tools live under their own paths).
      out.textContent = j.status
        ? tr("detail.connectedTheseValuesWork", { ms: j.ms, status: j.status })
        : tr("detail.connectedTheseValuesWorkMsMs", { ms: j.ms });
      out.style.color = "var(--green)";
    } else {
      out.textContent = j.ms != null
        ? tr("detail.errorMsMs", { error: j.error || tr("detail.failed"), ms: j.ms })
        : tr("detail.error", { error: j.error || tr("detail.failed") });
      out.style.color = "var(--red)";
    }
  } catch (e) {
    out.textContent = tr("detail.testRequestFailedGateway");
    out.style.color = "var(--red)";
  } finally {
    btn.disabled = false;
    btn.textContent = tr("detail.testConnection");
  }
}

async function saveEdit()                {
  const d = mcpDetail();
  if (!d) return;
  const type = d.editType || (d.config && d.config.type          ) || "proc";
  const fields = readFields(type, "e-");
  const body = Object.assign({ type: type }, fields);
  if (body.autostart !== undefined) { body.lazy = !body.autostart; delete body.autostart; }
  translateOauth(body); // the auth checkbox is the def auth string (docs/24 D1)
  translatePg(type, body); // docs/30: the pg form's pieces become one url
  if (type === "proc" && !body.command) { toast(tr("detail.commandRequired"), true); return; }
  const name = d.name;
  // Rendering the pane destroys the form, so hold on to what was typed: a save the server rejects
  // used to cost the user the whole form, with nothing to do but reopen it and retype.
  const restore = () => {
    if (!mcpDetail() || mcpDetail() !== d) return;
    d.editing = true;
    d.editType = type;
    d.editVals = fields;
  };
  setMcpBusy(name, "save");
  d.editing = false;
  renderPane(); patchSidebar();
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name), { method: "PUT", body: JSON.stringify(body) });
    const j = await r.json();
    if (!r.ok) {
      setLastAction(name, { msg: tr("detail.editFailedError", { error: j.error || tr("detail.httpN", { n: r.status }) }), err: true, at: now() });
      toast(j.error || tr("detail.editFailed"), true);
      restore();
    } else {
      setLastAction(name, { msg: tr("detail.configSavedRestarted"), err: false, at: now() });
      toast(tr("detail.nameMsg", { name, msg: tr("detail.configSavedRestarted2") }));
      if (mcpDetail() === d) d.editVals = null;
      resetKindPages();
      void loadMeta(name);
    }
  } catch (e) {
    setLastAction(name, { msg: tr("detail.editRequestFailed"), err: true, at: now() });
    toast(tr("detail.editRequestFailed"), true);
    restore();
  }
  clearMcpBusy(name);
  await loadList();
  renderPane();
}

export { act, authorizeMcp, callsPageStep, callsRetry, cancelEdit, changeEditType, clearCalls, deleteRevision, loadCalls, loadMeta, loadPage, loadRevisions, openDetail, pageNext, pagePrev, pageState, removeMcp, renameMcp, restoreRevision, runConnTest, saveEdit, saveReplace, showFullResult, showTab, startEdit, startReplace };
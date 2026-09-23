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

/* ================================================================================================
   Remote Runs - the remote plugin's second page (#remote-runs), 2026-09-18.

   Every remote.exec / sync / pull the coordinator ran, as the CLI (swiss remote ...) and
   the remote MCP tools ran it, read from the durable record the gateway keeps beside the
   target table (crates/swiss-remote/src/history.rs: 30 days, 500 MB, oldest out first).
   The page paints from ONE read of /api/remote/runs: the runs still in flight come first
   (the coordinator's view, output followed live through /api/runs/{id}/output), the
   recorded ones under them, newest first, paged by an older-than cursor.

   The Traffic idiom throughout: a card of .call rows, the summary line is the toggle, the
   body is fetched when the row opens and a poll never rebuilds what is open. A running
   row's body follows its output on a short timer while it is open; when the run ends the
   next poll moves the row from the live list into the record with the same run id.
   ================================================================================================ */
                                                                                                                              
                                                                                                       
import { $, apiJson, emptyNode, iconNode, targetEl, toast, whenLabel } from "../util.js";
import { fill, frag, h } from "../h.js";
import { tr, trn } from "../i18n.js";
                                      

const PAGE = 20;
const LIVE_EVERY_MS = 1500; // how often an OPEN live row pulls its output; the 6 s poll moves the list
/** What one open live row may hold, in characters — the same 256 KB the gateway itself keeps
 *  live (runs.rs MAX_LIVE_OUTPUT_BYTES): a runaway stream must not grow the tab's memory while
 *  someone watches, and the finished record repaints the row from its durable tail. */
const LIVE_TAIL_MAX = 256 * 1024;

let runs = []                     ; // the recorded page, newest first
let active = []                     ; // remote runs the coordinator still holds (queued / running)
let nextBefore = null                 ; // the cursor for the older page, null on the last one
let cursors = [null]                     ; // cursors[i] loaded page i; page 0 has none
let page = 0;
let usage = { bytes: 0, runs: 0 };
let limits = null                                          ;
let target = ""; // the target filter, "" for all
let targetIds = []            ; // for the filter select, from the targets table
let open = {}                           ; // runId -> true while a row is expanded
let bodies = {}                                 ; // runId -> { text, next, total, done, capped, tail } for opened recorded rows
let live = {}                                  ; // runId -> { text, cursor } for opened active rows
let liveTimer = null                                         ;
let painted = "";

async function load() {
  const q = "/api/remote/runs?limit=" + PAGE +
    (cursors[page] ? "&before=" + cursors[page] : "") +
    (target ? "&target=" + encodeURIComponent(target) : "");
  const j = await apiJson                       (q);
  if (!j) return false; // apiJson toasted; keep the last paint
  runs = j.runs                      || [];
  active = j.active                      || [];
  nextBefore = j.nextBefore || null;
  usage = j.usage || { bytes: 0, runs: 0 };
  limits = j.limits || null;
  return true;
}

async function loadTargets() {
  const t = await apiJson                          ("/api/remote/targets");
  if (t && t.targets) targetIds = t.targets.map((x) => { return x.id; });
}

function signature()         {
  return page + "|" + target + "|" + usage.bytes + "/" + usage.runs + "|" +
    active.map((r) => { return r.runId + "=" + r.state; }).join(",") + "|" +
    runs.map((r) => { return r.runId; }).join(",");
}

function fmtBytes(n        )         {
  if (!n) return "0 B";
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
  return (n / (1024 * 1024)).toFixed(1) + " MB";
}

function fmtMs(ms                           )         {
  if (ms == null) return "";
  if (ms < 1000) return tr("remoteRuns.nMs", { n: ms });
  if (ms < 60000) return tr("remoteRuns.nS", { n: (ms / 1000).toFixed(1) });
  const m = Math.floor(ms / 60000);
  return tr("remoteRuns.mMSS", { m, s: Math.round((ms - m * 60000) / 1000) });
}

/** The dot says the state in shape and colour; the title says it in words. */
function stateDotNode(r                 )              {
  const cls = r.state === "succeeded" ? "up"
    : r.state === "running" ? "starting"
    : r.state === "queued" || r.state === "canceled" ? "idle"
    : "down";
  return h("span", { class: "dot " + cls, title: r.state });
}

/** What ran, as a command line: the argv for an exec, the shape for a sync / pull. An
 *  active row that predates the record (an older gateway) falls back to its label. */
function commandOf(r                 )         {
  const input = r.input || {}                  ;
  const kind = (r.action || "").replace(/^remote\./, "");
  if (kind === "exec" && input.argv) return input.argv.join(" ");
  if (kind === "sync") return "sync " + (input.source || ".") + (input.to ? " \u2192 " + input.to : "");
  if (kind === "pull") return "pull " + (input.remote || "") + (input.to ? " \u2192 " + input.to : "");
  if (kind === "cat" || kind === "write") return kind + " " + (input.remote || "");
  return input.argv ? String(input.argv) : (r.label || kind || "run");
}

function targetOf(r                 )         {
  return (r.meta && r.meta.target) || (r.input && r.input.target) || "";
}

/** The row's meta: id, who (docs/41 A1 - the actor is an identifier, never translated;
 *  a row an older gateway recorded has none and skips it), how it ended, how long, when. */
function metaOf(r                 )         {
  const parts = ["#" + r.runId];
  if (r.actor) parts.push(r.actor);
  if (r.state === "running") parts.push(tr("remoteRuns.stateRunning"));
  else if (r.state === "queued") parts.push(tr("remoteRuns.stateQueued"));
  else if (r.exitCode != null) parts.push(tr("remoteRuns.exitN", { n: r.exitCode }));
  else if (r.state === "canceled") parts.push(tr("remoteRuns.canceled"));
  else if (r.state === "timeout") parts.push(tr("remoteRuns.timed"));
  // A file action (sync / pull / cat / write) has no exit code: its state is the word.
  else if (r.state === "succeeded") parts.push(tr("remoteRuns.stateSucceeded"));
  else if (r.state === "failed") parts.push(tr("remoteRuns.stateFailed"));
  else parts.push(r.state);
  if (r.ms != null) parts.push(fmtMs(r.ms));
  parts.push(whenLabel(r.startedAt || r.queuedAt));
  return parts.join(" \u00b7 ");
}

function rowNode(r                 , isLive         )              {
  const cwd = r.input && r.input.cwd ? " \u00b7 " + r.input.cwd : "";
  const data                                  = { rrun: r.runId };
  if (isLive) data.rlive = "1";
  return h("div", { class: "call" + (open[r.runId] ? " open" : ""), data: data },
    h("div", { class: "call-sum", data: { rtog: r.runId }, role: "button", tabIndex: 0 },
      h("span", { class: "chev", aria: { hidden: "true" } }, iconNode("chevron-right")),
      stateDotNode(r),
      h("span", { class: "call-tool" }, commandOf(r)),
      h("span", { class: "call-arg" }, targetOf(r) + cwd),
      h("span", { class: "call-meta" }, metaOf(r))),
    // The body stays empty until the row opens (the Traffic rule: nothing hidden is
    // built); bodyNode paints whatever the fetch brought.
    h("div", { class: "call-body" }, open[r.runId] ? bodyNode(r, isLive) : null));
}

function bodyNode(r                 , isLive         )         {
  if (isLive) {
    const l = live[r.runId];
    // Hoisted so the comparison literals stay out of the h() children (i18n gate).
    const cancellable = r.state === "running" || r.state === "queued";
    return frag(
      h("div", { class: "call-lbl rr-live-head" },
        h("span", null, tr("remoteRuns.liveOutput")),
        cancellable
          ? h("button", { class: "btn", data: { rcancel: r.runId } }, tr("remoteRuns.cancel"))
          : null),
      h("pre", { class: "logs", data: { rlivepre: r.runId } },
        l ? l.text : null,
        l && l.text ? null
          : h("span", { style: "color:var(--text-3)" },
            tr(r.state === "queued" ? "remoteRuns.queuedWaitingFreeSlot" : "remoteRuns.output"))));
  }
  const b = bodies[r.runId];
  const head           = [];
  if (r.error) {
    head.push(h("div", { class: "call-lbl" }, tr("remoteRuns.error")), h("pre", { class: "logs err" }, r.error));
  }
  if (!b) return frag(head, h("div", { class: "note" }, h("span", { class: "spin" }), " ", tr("remoteRuns.loading")));
  // The body union's two states: the gone marker (the record rolled past the run) or the
  // output read so far - "gone" in b is the discriminant.
  if ("gone" in b) return frag(head, h("div", { class: "note" }, tr("remoteRuns.runRolledRecord")));
  head.push(h("div", { class: "call-lbl" }, tr("remoteRuns.output2"), b.total ? [" \u00b7 ", fmtBytes(b.total)] : ""));
  if (r.outputEvicted) {
    // The size budget took the file (docs/41 A2); the line - and its tail, below, when
    // the run was capped - is what is left.
    head.push(h("div", { class: "note" },
      tr("remoteRuns.outputEvicted", { size: fmtBytes(r.outputBytes || 0), d: Math.round((limits ? limits.maxAgeMs : 0) / 86400000) })));
  } else if (!b.total) {
    head.push(h("pre", { class: "logs" }, h("span", { style: "color:var(--text-3)" }, tr("remoteRuns.outputProduced"))));
  } else {
    head.push(h("pre", { class: "logs" }, b.text));
    if (b.next < b.total) {
      head.push(h("div", { class: "pager" },
        h("button", { class: "btn", data: { rmore: r.runId } }, tr("remoteRuns.loadMore")),
        h("span", null, tr("remoteRuns.b", { a: fmtBytes(b.next), b: fmtBytes(b.total) }))));
    }
  }
  if (r.outputCapped && r.tail) {
    head.push(
      h("div", { class: "note" },
        tr("remoteRuns.outputCappedCapLast", { cap: fmtBytes(limits ? limits.maxOutputBytes : 0), tail: fmtBytes(r.tail.length) })),
      h("pre", { class: "logs" }, r.tail));
  }
  return frag(head);
}

function findRun(id        )                         {
  return active.find((r) => { return r.runId === id; }) || runs.find((r) => { return r.runId === id; }) || null;
}

function isLiveRun(id        )          {
  return active.some((r) => { return r.runId === id; });
}

function repaintBody(id        )       {
  const r = findRun(id);
  const node = document.querySelector('#pane .call[data-rrun="' + id + '"] .call-body');
  if (r && node) fill(node               , bodyNode(r, isLiveRun(id)));
}

/** One recorded run's output, from the cursor the previous read ended on (128 KB a read). */
async function loadBody(id        , more         )                {
  const b = bodies[id];
  if (b && !more) return;
  const after = b && !("gone" in b) ? b.next : 0;
  const j = await apiJson                    ("/api/remote/runs/" + id + "/output?after=" + after + "&max=131072");
  if (!j) { bodies[id] = { gone: true }; repaintBody(id); return; }
  bodies[id] = {
    text: (b && !("gone" in b) ? b.text : "") + (j.output || ""),
    next: j.nextCursor || 0,
    total: j.total || 0,
  };
  repaintBody(id);
}

/** Pull what an open live row has not shown yet; a terminal answer ends the following. */
async function pullLive(id        )                {
  const l = live[id] || (live[id] = { text: "", cursor: 0 });
  const j = await apiJson                   ("/api/runs/" + id + "/output?after=" + l.cursor + "&max=131072");
  if (!j) return;
  if (j.output) {
    l.text += j.output;
    if (l.text.length > LIVE_TAIL_MAX) {
      l.text = l.text.slice(l.text.length - LIVE_TAIL_MAX);
      l.capped = true;
    }
  }
  l.cursor = j.nextCursor || l.cursor;
  const pre = document.querySelector('#pane pre[data-rlivepre="' + id + '"]');
  if (pre) pre.textContent = (l.capped ? tr("remoteRuns.liveCappedTail") + "\n" : "") + l.text;
  if (j.terminal) void refresh(); // the run moved into the record: repaint from it
}

function armLive()       {
  const wanted = active.some((r) => { return open[r.runId]; });
  if (wanted && !liveTimer) {
    liveTimer = setInterval(() => {
      active.forEach((r) => { if (open[r.runId]) void pullLive(r.runId); });
    }, LIVE_EVERY_MS);
  } else if (!wanted && liveTimer) {
    clearInterval(liveTimer);
    liveTimer = null;
  }
}

function toggle(id        )       {
  open[id] = !open[id];
  const node = document.querySelector('#pane .call[data-rrun="' + id + '"]');
  if (node) node.className = "call" + (open[id] ? " open" : "");
  if (open[id]) {
    repaintBody(id);
    if (isLiveRun(id)) void pullLive(id); else void loadBody(id, false);
  }
  armLive();
}

async function cancelRun(id        )                {
  const j = await apiJson("/api/runs/" + id + "/cancel", { method: "POST" });
  if (!j) return;
  toast(tr("remoteRuns.cancelRequestedRunId", { id }));
  void refresh();
}

async function clearAll()                {
  if (!confirm(tr("remoteRuns.forgetEveryRecordedRemote"))) return;
  const j = await apiJson("/api/remote/runs", { method: "DELETE" });
  if (!j) return;
  bodies = {};
  open = {};
  cursors = [null];
  page = 0;
  toast(tr("remoteRuns.runRecordCleared"));
  await refresh();
}

function render()       {
  painted = signature();
  const kept = trn(usage.runs, "remoteRuns.nRunsRecordedBytes.one", "remoteRuns.nRunsRecordedBytes.other", { bytes: fmtBytes(usage.bytes) }) +
    (limits ? tr("remoteRuns.capKeptDDays", { cap: fmtBytes(limits.maxTotalBytes), d: Math.round(limits.maxAgeMs / 86400000) }) : "") +
    (limits && limits.auditWindowMs ? tr("remoteRuns.traceableWDays", { w: Math.round(limits.auditWindowMs / 86400000) }) : "");
  const options = [h("option", { value: "" }, tr("remoteRuns.allTargets"))].concat(targetIds.map((id) => {
    return h("option", { value: id, selected: id === target }, id);
  }));
  fill($("pane"),
    h("div", { class: "wide" },
      h("div", { class: "pane-head" },
        h("div", null,
          h("div", { class: "pane-desc" }, tr("remoteRuns.everyCommandSyncPull")),
          h("div", { class: "pane-sub" }, kept)),
        h("div", { class: "pane-actions" },
          h("button", { class: "btn", id: "rrClear", disabled: !usage.runs }, tr("remoteRuns.clear")))),
      h("div", { class: "sec-head" },
        h("span", { class: "sec-cap" }, tr("remoteRuns.runs")),
        h("select", { id: "rrTarget", aria: { label: tr("remoteRuns.filterTarget") } }, options)),
      h("div", { id: "rrList" })));
  paintList();
  $("countChip").textContent = usage.runs ? trn(usage.runs, "remoteRuns.nRuns.one", "remoteRuns.nRuns.other") : "";
}

function paintList()       {
  const region = $("rrList");
  if (!region) return;
  if (!active.length && !runs.length) {
    fill(region,
      page || target
        ? h("div", { class: "group" }, h("div", { class: "row" },
            h("span", { class: "rowmsg" }, page ? tr("remoteRuns.nothingPage") : tr("remoteRuns.runsTarget"))))
        : emptyNode({ icon: "history", title: tr("remoteRuns.runs2"), hint: tr("remoteRuns.runSomethingTargetSwiss") }),
      page ? pagerNode() : null);
    return;
  }
  fill(region,
    h("div", { class: "group" },
      active.map((r) => { return rowNode(r, true); }),
      runs.map((r) => { return rowNode(r, false); })),
    pagerNode());
}

// Newer/Older, the Traffic pager's shape: Newer always means toward the top of a
// newest-first list.
function pagerNode()         {
  if (!page && !nextBefore) return null;
  return h("div", { class: "pager" },
    h("button", { class: "btn", id: "rrPrev", disabled: page <= 0 }, tr("remoteRuns.newer")),
    h("span", null, tr("remoteRuns.pageN", { n: page + 1 })),
    h("button", { class: "btn", id: "rrNext", disabled: !nextBefore }, tr("remoteRuns.older")));
}

async function step(delta        )                {
  if (delta > 0) {
    if (!nextBefore) return;
    cursors[page + 1] = nextBefore;
    page += 1;
  } else {
    if (!page) return;
    page -= 1;
  }
  await refresh();
}

export async function mount() {
  await loadTargets();
  if (!(await load())) return;
  render();
  $("pane").onclick = (event            )       => {
    const tog = targetEl(event)?.closest             ("[data-rtog]");
    if (tog) { toggle(Number(tog.dataset.rtog)); return; }
    const more = targetEl(event)?.closest             ("[data-rmore]");
    if (more) { void loadBody(Number(more.dataset.rmore), true); return; }
    const cancel = targetEl(event)?.closest             ("[data-rcancel]");
    if (cancel) { void cancelRun(Number(cancel.dataset.rcancel)); return; }
    if (targetEl(event)?.closest("#rrClear")) { void clearAll(); return; }
    if (targetEl(event)?.closest("#rrPrev")) { void step(-1); return; }
    if (targetEl(event)?.closest("#rrNext")) { void step(1); }
  };
  // The target filter select - ONE delegated change listener instead of a per-render
  // assignment (docs/37 R5); the value is read at event time.
  $("pane").onchange = (event       )       => {
    const sel = targetEl(event)?.closest                   ("#rrTarget");
    if (!sel) return;
    target = sel.value;
    cursors = [null];
    page = 0;
    void refresh();
  };
}

export async function refresh() {
  if (!(await load())) return;
  // A poll that changed nothing repaints nothing: open rows keep their nodes and the
  // output someone is reading stays where it is (the Traffic discipline).
  if (signature() === painted) return;
  render();
  armLive();
}
export async function poll() { await refresh(); }
export function countText() { return usage.runs ? trn(usage.runs, "remoteRuns.nRuns.one", "remoteRuns.nRuns.other") : ""; }
export function unmount() {
  if (liveTimer) { clearInterval(liveTimer); liveTimer = null; }
  // A return visit starts on page 0 with no filter: the record moved on while the page
  // was away, so a stale cursor or filter would show a stale slice of it.
  open = {};
  bodies = {};
  live = {};
  painted = "";
  page = 0;
  cursors = [null];
  target = "";
}
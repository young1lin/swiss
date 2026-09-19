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
                                      

const PAGE = 20;
const LIVE_EVERY_MS = 1500; // how often an OPEN live row pulls its output; the 6 s poll moves the list

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
  if (ms < 1000) return ms + "ms";
  if (ms < 60000) return (ms / 1000).toFixed(1) + "s";
  const m = Math.floor(ms / 60000);
  return m + "m " + Math.round((ms - m * 60000) / 1000) + "s";
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
  if (kind === "exec" && Array.isArray(input.argv)) return input.argv.join(" ");
  if (kind === "sync") return "sync " + (input.source || ".") + (input.to ? " \u2192 " + input.to : "");
  if (kind === "pull") return "pull " + (input.remote || "") + (input.to ? " \u2192 " + input.to : "");
  if (kind === "cat" || kind === "write") return kind + " " + (input.path || input.remote || "");
  return input.argv ? String(input.argv) : (r.label || kind || "run");
}

function targetOf(r                 )         {
  return (r.meta && r.meta.target) || (r.input && r.input.target) || "";
}

function metaOf(r                 )         {
  const parts = ["#" + r.runId];
  if (r.state === "running" || r.state === "queued") parts.push(r.state);
  else if (r.exitCode != null) parts.push("exit " + r.exitCode);
  else if (r.state === "canceled") parts.push("canceled");
  else if (r.state === "timeout") parts.push("timed out");
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
    return frag(
      h("div", { class: "call-lbl rr-live-head" },
        h("span", null, "Live output"),
        r.state === "running" || r.state === "queued"
          ? h("button", { class: "btn", data: { rcancel: r.runId } }, "Cancel")
          : null),
      h("pre", { class: "logs", data: { rlivepre: r.runId } },
        l ? l.text : null,
        l && l.text ? null
          : h("span", { style: "color:var(--text-3)" },
            r.state === "queued" ? "Queued - waiting for a free slot." : "No output yet.")));
  }
  const b = bodies[r.runId];
  const head           = [];
  if (r.error) {
    head.push(h("div", { class: "call-lbl" }, "Error"), h("pre", { class: "logs err" }, r.error));
  }
  if (!b) return frag(head, h("div", { class: "note" }, h("span", { class: "spin" }), " Loading\u2026"));
  if (b.gone) return frag(head, h("div", { class: "note" }, "This run has rolled out of the record."));
  head.push(h("div", { class: "call-lbl" }, "Output" + (b.total ? " \u00b7 " + fmtBytes(b.total) : "")));
  if (!b.total) {
    head.push(h("pre", { class: "logs" }, h("span", { style: "color:var(--text-3)" }, "No output was produced.")));
  } else {
    head.push(h("pre", { class: "logs" }, b.text));
    if (b.next < b.total) {
      head.push(h("div", { class: "pager" },
        h("button", { class: "btn", data: { rmore: r.runId } }, "Load more"),
        h("span", null, fmtBytes(b.next) + " of " + fmtBytes(b.total))));
    }
  }
  if (r.outputCapped && r.tail) {
    head.push(
      h("div", { class: "note" },
        "Output capped at " + fmtBytes(limits ? limits.maxOutputBytes : 0) + " \u2014 the last " + fmtBytes(r.tail.length) + ":"),
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
  const after = b ? b.next : 0;
  const j = await apiJson                    ("/api/remote/runs/" + id + "/output?after=" + after + "&max=131072");
  if (!j) { bodies[id] = { gone: true }                            ; repaintBody(id); return; }
  bodies[id] = {
    text: (b ? b.text : "") + (j.output || ""),
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
  if (j.output) l.text += j.output;
  l.cursor = j.nextCursor || l.cursor;
  const pre = document.querySelector('#pane pre[data-rlivepre="' + id + '"]');
  if (pre) pre.textContent = l.text;
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
  toast("Cancel requested for run #" + id);
  void refresh();
}

async function clearAll()                {
  if (!confirm("Forget every recorded remote run and its output? Runs still in flight are not affected.")) return;
  const j = await apiJson("/api/remote/runs", { method: "DELETE" });
  if (!j) return;
  bodies = {};
  open = {};
  cursors = [null];
  page = 0;
  toast("Run record cleared");
  await refresh();
}

function render()       {
  painted = signature();
  const kept = usage.runs + " run" + (usage.runs === 1 ? "" : "s") + " recorded \u00b7 " + fmtBytes(usage.bytes) +
    (limits ? " of " + fmtBytes(limits.maxTotalBytes) + " \u00b7 kept " + Math.round(limits.maxAgeMs / 86400000) + " days" : "");
  const options = [h("option", { value: "" }, "All targets")].concat(targetIds.map((id) => {
    return h("option", { value: id, selected: id === target }, id);
  }));
  fill($("pane"),
    h("div", { class: "wide" },
      h("div", { class: "pane-head" },
        h("div", null,
          h("div", { class: "pane-desc" }, "Every command, sync and pull run on a remote target, with its output - as the CLI (swiss remote \u2026) and the remote MCP tools ran it."),
          h("div", { class: "pane-sub" }, kept)),
        h("div", { class: "pane-actions" },
          h("button", { class: "btn", id: "rrClear", disabled: !usage.runs }, "Clear"))),
      h("div", { class: "sec-head" },
        h("span", { class: "sec-cap" }, "Runs"),
        h("select", { id: "rrTarget", aria: { label: "Filter by target" } }, options)),
      h("div", { id: "rrList" })));
  paintList();
  $("countChip").textContent = usage.runs ? usage.runs + " run" + (usage.runs === 1 ? "" : "s") : "";
}

function paintList()       {
  const region = $("rrList");
  if (!region) return;
  if (!active.length && !runs.length) {
    fill(region,
      page || target
        ? h("div", { class: "group" }, h("div", { class: "row" },
            h("span", { class: "rowmsg" }, page ? "Nothing on this page." : "No runs on this target yet.")))
        : emptyNode({ icon: "history", title: "No runs yet", hint: "Run something on a target - swiss remote exec, or the remote MCP tools - and it is recorded here with its output." }),
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
    h("button", { class: "btn", id: "rrPrev", disabled: page <= 0 }, "Newer"),
    h("span", null, "Page " + (page + 1)),
    h("button", { class: "btn", id: "rrNext", disabled: !nextBefore }, "Older"));
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
export function countText() { return usage.runs ? usage.runs + " runs" : ""; }
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

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
import { $, apiJson, emptyHtml, esc, icon, targetEl, toast, whenLabel } from "../util.js";

var PAGE = 20;
var LIVE_EVERY_MS = 1500; // how often an OPEN live row pulls its output; the 6 s poll moves the list

var runs = [] as ApiRemoteRunRow[]; // the recorded page, newest first
var active = [] as ApiRemoteRunRow[]; // remote runs the coordinator still holds (queued / running)
var nextBefore = null as number | null; // the cursor for the older page, null on the last one
var cursors = [null] as (number | null)[]; // cursors[i] loaded page i; page 0 has none
var page = 0;
var usage = { bytes: 0, runs: 0 };
var limits = null as ApiRemoteRunsResponse["limits"] | null;
var target = ""; // the target filter, "" for all
var targetIds = [] as string[]; // for the filter select, from the targets table
var open = {} as Record<number, boolean>; // runId -> true while a row is expanded
var bodies = {} as Record<number, RemoteRunBody>; // runId -> { text, next, total, done, capped, tail } for opened recorded rows
var live = {} as Record<number, RemoteLiveBody>; // runId -> { text, cursor } for opened active rows
var liveTimer = null as ReturnType<typeof setInterval> | null;
var painted = "";

async function load() {
  var q = "/api/remote/runs?limit=" + PAGE +
    (cursors[page] ? "&before=" + cursors[page] : "") +
    (target ? "&target=" + encodeURIComponent(target) : "");
  var j = await apiJson<ApiRemoteRunsResponse>(q);
  if (!j) return false; // apiJson toasted; keep the last paint
  runs = j.runs as ApiRemoteRunRow[] || [];
  active = j.active as ApiRemoteRunRow[] || [];
  nextBefore = j.nextBefore || null;
  usage = j.usage || { bytes: 0, runs: 0 };
  limits = j.limits || null;
  return true;
}

async function loadTargets() {
  var t = await apiJson<ApiRemoteTargetsResponse>("/api/remote/targets");
  if (t && t.targets) targetIds = t.targets.map(function (x) { return x.id; });
}

function signature(): string {
  return page + "|" + target + "|" + usage.bytes + "/" + usage.runs + "|" +
    active.map(function (r) { return r.runId + "=" + r.state; }).join(",") + "|" +
    runs.map(function (r) { return r.runId; }).join(",");
}

function fmtBytes(n: number): string {
  if (!n) return "0 B";
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
  return (n / (1024 * 1024)).toFixed(1) + " MB";
}

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return "";
  if (ms < 1000) return ms + "ms";
  if (ms < 60000) return (ms / 1000).toFixed(1) + "s";
  var m = Math.floor(ms / 60000);
  return m + "m " + Math.round((ms - m * 60000) / 1000) + "s";
}

/** The dot says the state in shape and colour; the title says it in words. */
function stateDot(r: ApiRemoteRunRow): string {
  var cls = r.state === "succeeded" ? "up"
    : r.state === "running" ? "starting"
    : r.state === "queued" || r.state === "canceled" ? "idle"
    : "down";
  return '<span class="dot ' + cls + '" title="' + esc(r.state) + '"></span>';
}

/** What ran, as a command line: the argv for an exec, the shape for a sync / pull. An
 *  active row that predates the record (an older gateway) falls back to its label. */
function commandOf(r: ApiRemoteRunRow): string {
  var input = r.input || {} as RemoteRunInput;
  var kind = (r.action || "").replace(/^remote\./, "");
  if (kind === "exec" && Array.isArray(input.argv)) return input.argv.join(" ");
  if (kind === "sync") return "sync " + (input.source || ".") + (input.to ? " \u2192 " + input.to : "");
  if (kind === "pull") return "pull " + (input.remote || "") + (input.to ? " \u2192 " + input.to : "");
  if (kind === "cat" || kind === "write") return kind + " " + (input.path || input.remote || "");
  return input.argv ? String(input.argv) : (r.label || kind || "run");
}

function targetOf(r: ApiRemoteRunRow): string {
  return (r.meta && r.meta.target) || (r.input && r.input.target) || "";
}

function metaOf(r: ApiRemoteRunRow): string {
  var parts = ["#" + r.runId];
  if (r.state === "running" || r.state === "queued") parts.push(r.state);
  else if (r.exitCode != null) parts.push("exit " + r.exitCode);
  else if (r.state === "canceled") parts.push("canceled");
  else if (r.state === "timeout") parts.push("timed out");
  else parts.push(r.state);
  if (r.ms != null) parts.push(fmtMs(r.ms));
  parts.push(whenLabel(r.startedAt || r.queuedAt));
  return parts.join(" \u00b7 ");
}

function row(r: ApiRemoteRunRow, isLive: boolean): string {
  var cwd = r.input && r.input.cwd ? " \u00b7 " + r.input.cwd : "";
  return '<div class="call' + (open[r.runId] ? " open" : "") + '" data-rrun="' + r.runId + '"' + (isLive ? ' data-rlive="1"' : "") + ">" +
    '<div class="call-sum" data-rtog="' + r.runId + '" role="button" tabindex="0">' +
      '<span class="chev" aria-hidden="true">' + icon("chevron-right") + "</span>" +
      stateDot(r) +
      '<span class="call-tool">' + esc(commandOf(r)) + "</span>" +
      '<span class="call-arg">' + esc(targetOf(r) + cwd) + "</span>" +
      '<span class="call-meta">' + esc(metaOf(r)) + "</span>" +
    "</div>" +
    // The body stays empty until the row opens (the Traffic rule: nothing hidden is
    // built); bodyHtml paints whatever the fetch brought.
    '<div class="call-body">' + (open[r.runId] ? bodyHtml(r, isLive) : "") + "</div>" +
  "</div>";
}

function bodyHtml(r: ApiRemoteRunRow, isLive: boolean): string {
  if (isLive) {
    var l = live[r.runId];
    return '<div class="call-lbl rr-live-head"><span>Live output</span>' +
        (r.state === "running" || r.state === "queued"
          ? '<button class="btn" data-rcancel="' + r.runId + '">Cancel</button>'
          : "") +
      "</div>" +
      '<pre class="logs" data-rlivepre="' + r.runId + '">' + (l ? esc(l.text) : "") +
        (l && l.text ? "" : '<span style="color:var(--text-3)">' + (r.state === "queued" ? "Queued - waiting for a free slot." : "No output yet.") + "</span>") +
      "</pre>";
  }
  var b = bodies[r.runId];
  var html = "";
  if (r.error) html += '<div class="call-lbl">Error</div><pre class="logs err">' + esc(r.error) + "</pre>";
  if (!b) return html + '<div class="note"><span class="spin"></span> Loading\u2026</div>';
  if (b.gone) return html + '<div class="note">This run has rolled out of the record.</div>';
  html += '<div class="call-lbl">Output' + (b.total ? " \u00b7 " + fmtBytes(b.total) : "") + "</div>";
  if (!b.total) {
    html += '<pre class="logs"><span style="color:var(--text-3)">No output was produced.</span></pre>';
  } else {
    html += '<pre class="logs">' + esc(b.text) + "</pre>";
    if (b.next < b.total) {
      html += '<div class="pager"><button class="btn" data-rmore="' + r.runId + '">Load more</button>' +
        "<span>" + fmtBytes(b.next) + " of " + fmtBytes(b.total) + "</span></div>";
    }
  }
  if (r.outputCapped && r.tail) {
    html += '<div class="note">Output capped at ' + fmtBytes(limits ? limits.maxOutputBytes : 0) + " \u2014 the last " + fmtBytes(r.tail.length) + ":</div>" +
      '<pre class="logs">' + esc(r.tail) + "</pre>";
  }
  return html;
}

function findRun(id: number): ApiRemoteRunRow | null {
  return active.find(function (r) { return r.runId === id; }) || runs.find(function (r) { return r.runId === id; }) || null;
}

function isLiveRun(id: number): boolean {
  return active.some(function (r) { return r.runId === id; });
}

function repaintBody(id: number): void {
  var r = findRun(id);
  var node = document.querySelector('#pane .call[data-rrun="' + id + '"] .call-body');
  if (r && node) node.innerHTML = bodyHtml(r, isLiveRun(id));
}

/** One recorded run's output, from the cursor the previous read ended on (128 KB a read). */
async function loadBody(id: number, more: boolean): Promise<void> {
  var b = bodies[id];
  if (b && !more) return;
  var after = b ? b.next : 0;
  var j = await apiJson<ApiRemoteRunOutput>("/api/remote/runs/" + id + "/output?after=" + after + "&max=131072");
  if (!j) { bodies[id] = { gone: true } as unknown as RemoteRunBody; repaintBody(id); return; }
  bodies[id] = {
    text: (b ? b.text : "") + (j.output || ""),
    next: j.nextCursor || 0,
    total: j.total || 0,
  };
  repaintBody(id);
}

/** Pull what an open live row has not shown yet; a terminal answer ends the following. */
async function pullLive(id: number): Promise<void> {
  var l = live[id] || (live[id] = { text: "", cursor: 0 });
  var j = await apiJson<ApiRunOutputChunk>("/api/runs/" + id + "/output?after=" + l.cursor + "&max=131072");
  if (!j) return;
  if (j.output) l.text += j.output;
  l.cursor = j.nextCursor || l.cursor;
  var pre = document.querySelector('#pane pre[data-rlivepre="' + id + '"]');
  if (pre) pre.textContent = l.text;
  if (j.terminal) void refresh(); // the run moved into the record: repaint from it
}

function armLive(): void {
  var wanted = active.some(function (r) { return open[r.runId]; });
  if (wanted && !liveTimer) {
    liveTimer = setInterval(function () {
      active.forEach(function (r) { if (open[r.runId]) void pullLive(r.runId); });
    }, LIVE_EVERY_MS);
  } else if (!wanted && liveTimer) {
    clearInterval(liveTimer);
    liveTimer = null;
  }
}

function toggle(id: number): void {
  open[id] = !open[id];
  var node = document.querySelector('#pane .call[data-rrun="' + id + '"]');
  if (node) node.className = "call" + (open[id] ? " open" : "");
  if (open[id]) {
    repaintBody(id);
    if (isLiveRun(id)) void pullLive(id); else void loadBody(id, false);
  }
  armLive();
}

async function cancelRun(id: number): Promise<void> {
  var j = await apiJson("/api/runs/" + id + "/cancel", { method: "POST" });
  if (!j) return;
  toast("Cancel requested for run #" + id);
  void refresh();
}

async function clearAll(): Promise<void> {
  if (!confirm("Forget every recorded remote run and its output? Runs still in flight are not affected.")) return;
  var j = await apiJson("/api/remote/runs", { method: "DELETE" });
  if (!j) return;
  bodies = {};
  open = {};
  cursors = [null];
  page = 0;
  toast("Run record cleared");
  await refresh();
}

function render(): void {
  painted = signature();
  var kept = usage.runs + " run" + (usage.runs === 1 ? "" : "s") + " recorded \u00b7 " + fmtBytes(usage.bytes) +
    (limits ? " of " + fmtBytes(limits.maxTotalBytes) + " \u00b7 kept " + Math.round(limits.maxAgeMs / 86400000) + " days" : "");
  var options = '<option value="">All targets</option>' + targetIds.map(function (id) {
    return '<option value="' + esc(id) + '"' + (id === target ? " selected" : "") + ">" + esc(id) + "</option>";
  }).join("");
  var head =
    '<div class="wide">' +
      '<div class="pane-head"><div>' +
        '<div class="pane-desc">Every command, sync and pull run on a remote target, with its output - as the CLI (swiss remote \u2026) and the remote MCP tools ran it.</div>' +
        '<div class="pane-sub">' + esc(kept) + "</div>" +
      "</div>" +
      '<div class="pane-actions">' +
        '<button class="btn" id="rrClear"' + (usage.runs ? "" : " disabled") + ">Clear</button>" +
      "</div></div>" +
      '<div class="sec-head"><span class="sec-cap">Runs</span>' +
        '<select id="rrTarget" aria-label="Filter by target">' + options + "</select>" +
      "</div>" +
      '<div id="rrList"></div>' +
    "</div>";
  $("pane").innerHTML = head;
  paintList();
  var sel = $<HTMLSelectElement>("rrTarget");
  if (sel) sel.onchange = function () {
    target = sel.value;
    cursors = [null];
    page = 0;
    void refresh();
  };
  $("countChip").textContent = usage.runs ? usage.runs + " run" + (usage.runs === 1 ? "" : "s") : "";
}

function paintList(): void {
  var region = $("rrList");
  if (!region) return;
  if (!active.length && !runs.length) {
    region.innerHTML = page || target
      ? '<div class="group"><div class="row"><span class="rowmsg">' + (page ? "Nothing on this page." : "No runs on this target yet.") + "</span></div></div>"
      : emptyHtml({ icon: "history", title: "No runs yet", hint: "Run something on a target - swiss remote exec, or the remote MCP tools - and it is recorded here with its output." });
    if (page) region.innerHTML += pagerHtml();
    return;
  }
  var rows = active.map(function (r) { return row(r, true); }).join("") + runs.map(function (r) { return row(r, false); }).join("");
  region.innerHTML = '<div class="group">' + rows + "</div>" + pagerHtml();
}

// Newer/Older, the Traffic pager's shape: Newer always means toward the top of a
// newest-first list.
function pagerHtml(): string {
  if (!page && !nextBefore) return "";
  return '<div class="pager"><button class="btn" id="rrPrev"' + (page > 0 ? "" : " disabled") + ">Newer</button>" +
    "<span>Page " + (page + 1) + "</span>" +
    '<button class="btn" id="rrNext"' + (nextBefore ? "" : " disabled") + ">Older</button></div>";
}

async function step(delta: number): Promise<void> {
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
  $("pane").onclick = function (event: MouseEvent): void {
    var tog = targetEl(event)?.closest<HTMLElement>("[data-rtog]");
    if (tog) { toggle(Number(tog.dataset.rtog)); return; }
    var more = targetEl(event)?.closest<HTMLElement>("[data-rmore]");
    if (more) { void loadBody(Number(more.dataset.rmore), true); return; }
    var cancel = targetEl(event)?.closest<HTMLElement>("[data-rcancel]");
    if (cancel) { void cancelRun(Number(cancel.dataset.rcancel)); return; }
    if (targetEl(event)?.closest("#rrClear")) { void clearAll(); return; }
    if (targetEl(event)?.closest("#rrPrev")) { void step(-1); return; }
    if (targetEl(event)?.closest("#rrNext")) { void step(1); }
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

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

   The event list (SPEC §panel.pages, the Logs and Traffic list): the time, with the date as the
   day heading; what ran (the kind, then its command); who - the target and the actor, a
   column only while they vary; a run that did not succeed as a red tag saying how; the
   duration. Identical consecutive runs fold into ×N. A row's body is fetched when it opens
   and a poll never rebuilds what is open. A running row's body follows its output on a
   short timer while it is open; when the run ends the next poll moves the row from the
   live list into the record with the same run id.

   The search box is the first thing in the head, always there (2026-10-12): it searches what
   the recorded runs printed, and its results take the list's place as the same rows, each
   with the line that matched. Until then the search was a toggle that opened a second box
   UNDER the list and the pager, and answered with "Run #42 · byte 70000" links - the owner
   asked why a search sat at the bottom of the page behind a button ("默认就能搜索").
   ================================================================================================ */
                                                                                                                              
                                                                                                                         
import { $, api, apiJson, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
                                      
import { tr, trn } from "../i18n.js";
import { copyLogText, readableBody } from "../logs.js";
import {
  actionBtn, btn, closeMenu, collapseRuns, emptyNode, filterInput, iconBtn, menuOpen, moreBtn, note, pager, paneBody, paneHead, popupMenu, textNode, timeline, timelineMeta,
  timelineToggle, valueBlock,
} from "../ui/index.js";
                                                   
import { coalescedRead,                    } from "../bus/live.js";
import { busOn, busOnResync } from "../bus/bus.js";
import { Module, NoticeSchema } from "../bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, RunChangedSchema, RunsId } from "../bus/pb/swiss/bus/v1/host_pb.js";
import { HistoryId, RemoteSub, TargetsId } from "../bus/pb/swiss/bus/v1/remote_pb.js";

const PAGE = 20;
const LIVE_EVERY_MS = 1500; // how often an OPEN live row pulls its output; the list moves on the bus
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
let paintedTargets = ""; // the filter's painted list: a targets notice repaints the select, not the page
let open = {}                           ; // runId -> true while its row is expanded
let bodies = {}                                 ; // runId -> { text, next, total, done, capped, tail } for opened recorded rows
let live = {}                                  ; // runId -> { text, cursor } for opened active rows
let contents = {}                                    ; // runId -> a write's body or an exec's stdin, unsealed while shown
let unsealing = {}                           ; // runId -> true while its /content read is in flight
let liveTimer = null                                         ;
let painted = "";
/* Which visit of this page is on screen (2026-09-29). Every await below can outlive the visit
   that started it - a cancel waits for the run to STOP, which is seconds - and render() fills
   #pane, which by then may belong to another page: Cancel, then a click to Logs, and the Runs
   list painted itself over Logs when the stop came back. mount and unmount both move the
   number; an answer carrying an older one writes nothing and paints nothing. */
let visit = 0;
let onScreen = false;
/* The live wiring (SPEC §remote.panel): one coalesced re-read and the topics that drive it. */
let reRead                       = null;
let offs                    = [];

async function load(v        ) {
  const q = "/api/remote/runs?limit=" + PAGE +
    (cursors[page] ? "&before=" + cursors[page] : "") +
    (target ? "&target=" + encodeURIComponent(target) : "");
  const j = await apiJson                       (q);
  if (!j) return false; // apiJson toasted; keep the last paint
  if (v !== visit) return false; // a visit that has ended: its answer is nobody's
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

/* --- what a run was and how it ended (the Targets page's last-run column reads these too) ---- */

/** The capability that ran: exec, sync, pull, cat, write. */
function kindOf(r                 )         {
  return (r.action || "").replace(/^remote\./, "") || "run";
}

/** What ran, without the kind: the argv for an exec, the shape for a file action. An active row
 *  that predates the record (an older gateway) falls back to its label. */
function argOf(r                 )         {
  const input = r.input || {}                  ;
  const kind = kindOf(r);
  if (kind === "exec" && input.argv) {
    // The script an exec fed stdin is part of WHAT ran (the owner's finding, twice: a row
    // that says only `sh -s`/`bash -s` answers nothing). The line says it went and how big;
    // the text itself stays sealed - the open row's Stdin block is where it shows.
    const fed = input.stdinBytes != null ? " · " + tr("remoteRuns.argStdin", { size: fmtBytes(input.stdinBytes) }) : "";
    return input.argv.join(" ") + fed;
  }
  if (kind === "sync") return (input.source || ".") + (input.to ? " → " + input.to : "");
  if (kind === "pull") return (input.remote || "") + (input.to ? " → " + input.to : "");
  if (kind === "cat" || kind === "write") return input.remote || "";
  return input.argv ? String(input.argv) : r.label || "";
}

/** What ran, as one command line: the argv for an exec, "sync . → /to" for a file action. */
export function commandOf(r                 )         {
  const kind = kindOf(r);
  return kind === "exec" && r.input && r.input.argv ? argOf(r) : (kind + " " + argOf(r)).trim();
}

/** One argv word as a POSIX shell reads it back: bare when every character is in the gateway's
 *  safe set, else single-quoted with the '\'' splice - quote_posix in the tunnels crate, the
 *  quoting the exec was actually sent with. */
function shellWord(w        )         {
  if (w && /^[A-Za-z0-9_.:=/@%+,-]+$/.test(w)) return w;
  return "'" + w.replace(/'/g, "'\\''") + "'";
}

/** The whole command, exactly: an exec's argv quoted word by word (a pasted copy runs as the
 *  record says - the row's own line joins with plain spaces and is cut to fit), a file action
 *  as its kind and path. */
export function commandLine(r                 )         {
  const argv = r.input && r.input.argv;
  return kindOf(r) === "exec" && argv ? argv.map(shellWord).join(" ") : commandOf(r);
}

export function runTarget(r                 )         {
  return (r.meta && r.meta.target) || (r.input && r.input.target) || "";
}

/** How a finished run failed, in the words its red tag says - null for one that succeeded.
 *  Every run that did not succeed is red, the Jobs history's rule: a non-zero exit says its
 *  code; a file action has none, so its state says it (failed, timed out, canceled). */
export function runOutcome(r                 )                {
  if (r.exitCode != null && r.exitCode !== 0) return tr("remoteRuns.exitN", { n: r.exitCode });
  if (r.state === "canceled" || r.canceled) return tr("remoteRuns.canceled");
  if (r.state === "timeout" || r.timedOut) return tr("remoteRuns.timed");
  if (r.state === "failed") return tr("remoteRuns.stateFailed");
  return null;
}

/** The state in words for the open row's meta: the exit code when there is one, else the state. */
function stateWord(r                 )         {
  if (r.state === "running") return tr("remoteRuns.stateRunning");
  if (r.state === "queued") return tr("remoteRuns.stateQueued");
  if (r.exitCode != null) return tr("remoteRuns.exitN", { n: r.exitCode });
  return runOutcome(r) || (r.state === "succeeded" ? tr("remoteRuns.stateSucceeded") : r.state);
}

/* --- the list ------------------------------------------------------------------------------ */

function isLiveRun(id        )          {
  return active.some((r) => { return r.runId === id; });
}

/** A run as a timeline row; `rrun` addresses it for the delegated listener. */
function runItem(r                 , isLive         )               {
  const kind = kindOf(r);
  const arg = argOf(r);
  const tgt = runTarget(r);
  const bad = isLive ? null : runOutcome(r);
  return {
    id: String(r.runId),
    // new Date(), not Date.parse(): the wire carries an ISO string, but an epoch NUMBER reads
    // just as well and a caller passing one (master's live-tail suite does) must not NaN the
    // timeline's toISOString.
    at: new Date(r.startedAt || r.queuedAt).getTime(),
    title: kind,
    arg,
    // SPEC §remote.history: the actor is an identifier, never translated; a row an older gateway
    // recorded has none and says only its target.
    who: [tgt, actorLabel(r.actor)].filter(Boolean).join(" · "),
    ms: isLive ? undefined : r.ms,
    status: bad ? { text: bad, tone: "bad" } : undefined,
    live: isLive
      ? { text: r.state === "queued" ? tr("remoteRuns.stateQueued") : tr("remoteRuns.stateRunning"), queued: r.state === "queued" }
      : undefined,
    // Identical consecutive runs fold (an agent's `git status` loop). A run in flight never
    // folds, and a different amount of output keeps two runs apart - the row cannot show it.
    // Nor does a run fed stdin: the same `sh -s` may have read a different script, and the
    // record keeps only its size.
    same: isLive || r.input?.stdinBytes != null ? undefined
      : [kind, arg, tgt, actorLabel(r.actor), r.input?.cwd || "", bad || "ok", r.outputBytes ?? ""].join("\u0000"),
    data: { rrun: r.runId },
  };
}

/** The rows on screen: the search's results while a needle is in the box, else the list. */
function allItems()                 {
  if (search.q) return searchItems();
  return active.map((r) => { return runItem(r, true); }).concat(runs.map((r) => { return runItem(r, false); }));
}

function findRun(id        )                         {
  return active.find((r) => { return r.runId === id; }) || runs.find((r) => { return r.runId === id; }) || search.records[id] || null;
}

/** The runs a row stands for, newest first: its own and, for ×N, every identical one under it. */
function runOf(id        )                    {
  for (const run of collapseRuns(allItems())) {
    if (run.some((it) => { return it.id === String(id); })) {
      return run.map((it) => { return findRun(Number(it.id)); }).filter((r)                       => !!r);
    }
  }
  return [];
}

/** The Command block (2026-09-28): the row's line is cut with an ellipsis, so the open row
 *  states the command whole, with a Copy. */
function commandNode(r                 )              {
  return valueBlock(
    { label: tr("remoteRuns.command"), tools: [iconBtn("copy", tr("remoteRuns.copyCommand"), { ghost: true, data: { rcopy: r.runId } })] },
    textNode(commandLine(r), true, "logs").node);
}

/** The actor as a row may show it: the surface, never the machine's identity.
 *
 *  The CLI recorded `cli:<user>@<host>` until 2026-09-28, and on a personal machine that login
 *  name is the operator's own name — on screen in every row of this page, and in every
 *  screenshot of it ("页面上把我名称都暴露出来了"). The CLI records the bare surface now; a
 *  record written before that still carries the identity in the file, and this is where it
 *  stops being shown. What the row was ever saying is which surface submitted the run. */
export function actorLabel(actor                           )         {
  if (!actor) return "";
  const cut = actor.indexOf(":");
  return cut > 0 ? actor.slice(0, cut) : actor;
}

/** An open row: what its columns left out (the id, the target, who, the directory, the state),
 *  the whole command, then the output as the value block Logs and Traffic open to. */
function bodyNode(run                   )         {
  const r = run[0];
  const isLive = isLiveRun(r.runId);
  const meta = timelineMeta([
    "#" + r.runId, runTarget(r) || null, actorLabel(r.actor) || null,
    r.input && r.input.cwd ? h("code", null, r.input.cwd) : null,
    stateWord(r),
    run.length > 1 ? tr("remoteRuns.nIdenticalRuns", { n: run.length }) : null,
    search.q && search.first[r.runId] ? trn(search.first[r.runId].n, "remoteRuns.matchesN.one", "remoteRuns.matchesN.other") : null,
  ]);
  if (isLive) {
    const l = live[r.runId];
    // Hoisted so the comparison literals stay out of the h() children (i18n gate).
    const cancellable = r.state === "running" || r.state === "queued";
    // The capped note prefixes the text here too, matching pullLive's surgical update.
    const liveText = l && l.text
      ? (l.capped ? tr("remoteRuns.liveCappedTail") + "\n" : "") + l.text
      : null;
    return [meta, commandNode(r), stdinNode(r, true), valueBlock(
      {
        label: tr("remoteRuns.liveOutput"),
        data: { rlive: r.runId },
        // A cancel in flight IS the button: disabled, and saying so. The request behind it
        // does not return until the run has actually stopped.
        tools: cancellable
          ? [canceling[r.runId]
            ? actionBtn("stop", tr("remoteRuns.canceling"), { data: { rcancel: r.runId }, disabled: true })
            : actionBtn("stop", tr("remoteRuns.cancel"), { data: { rcancel: r.runId } })]
          : [],
      },
      liveText != null
        ? textNode(liveText, true, "logs").node
        : note(r.state === "queued" ? tr("remoteRuns.queuedWaitingFreeSlot") : tr("remoteRuns.output")))];
  }
  const out           = [meta, commandNode(r), stdinNode(r, false)];
  if (r.error) out.push(valueBlock({ label: tr("remoteRuns.error") }, ...readableBody(r.error, true)));
  const b = bodies[r.runId];
  if (!b) return out.concat(note(tr("remoteRuns.loading"), { busy: true }));
  // The body union's two states: the gone marker (the record rolled past the run) or the
  // output read so far - "gone" in b is the discriminant.
  if ("gone" in b) return out.concat(note(tr("remoteRuns.runRolledRecord")));
  if (r.outputEvicted) {
    // The size budget took the file (SPEC §remote.history); the line - and its tail, below, when
    // the run was capped - is what is left.
    out.push(note(tr("remoteRuns.outputEvicted", { size: fmtBytes(r.outputBytes || 0), d: Math.round((limits ? limits.maxAgeMs : 0) / 86400000) })));
  } else if (!b.total) {
    out.push(valueBlock({ label: tr("remoteRuns.output2"), text: tr("remoteRuns.outputProduced") }));
  } else {
    const partial = b.next < b.total;
    out.push(valueBlock(
      { label: tr("remoteRuns.output2"), notes: [partial ? tr("remoteRuns.b", { a: fmtBytes(b.next), b: fmtBytes(b.total) }) : fmtBytes(b.total)] },
      ...readableBody(b.text, !!runOutcome(r))));
    if (partial) out.push(btn(tr("remoteRuns.loadMore"), { data: { rmore: r.runId } }));
  }
  if (r.outputCapped && r.tail) {
    out.push(valueBlock(
      { label: tr("remoteRuns.tail"), notes: [tr("remoteRuns.cappedAtLast", { cap: fmtBytes(limits ? limits.maxOutputBytes : 0), tail: fmtBytes(r.tail.length) })] },
      textNode(r.tail, true, "logs").node));
  }
  out.push(writtenNode(r));
  return out;
}

/** An exec's stdin (SPEC §remote.history): the script or SQL the run actually ran, kept
 *  SEALED beside the record exactly as a write's body is - a stdin can carry a password
 *  in clear, so the index never holds the text. The open row unseals it as it opens
 *  (showStdin): until 2026-11-10 it waited for an icon-only Show at the far end of the
 *  block, and the owner read "Stdin sent 2.3 KB" with no text as a stdin that was lost -
 *  the third time a `bash -s` row answered nothing about WHAT ran. Hide puts it away.
 *  Rows recorded before it was kept say their size and nothing else. */
function stdinNode(r                 , isLive         )         {
  const bytes = r.input?.stdinBytes;
  if (bytes == null) return null;
  const label = tr("remoteRuns.stdinSent");
  const size = fmtBytes(bytes);
  // A live run's record is not filed yet, so nothing is sealed to read: the text is sealed
  // as the run finishes. A live row said "the text is not kept" here until 2026-11-10.
  if (isLive) return bytes > 0 ? valueBlock({ label, notes: [size] }, note(tr("remoteRuns.stdinAfterRun"))) : null;
  if (!r.stdinStored) {
    return kindOf(r) === "exec" && bytes > 0 ? note(tr("remoteRuns.stdinNotKept", { size })) : null;
  }
  const c = contents[r.runId];
  if (!c) {
    if (unsealing[r.runId]) return valueBlock({ label, notes: [size] }, note(tr("remoteRuns.loading"), { busy: true }));
    return valueBlock({ label, notes: [size], tools: [iconBtn("eye", tr("remoteRuns.showStdin"), { data: { rcontent: r.runId } })] });
  }
  if ("error" in c) return valueBlock({ label, notes: [size] }, note(c.error));
  const notes = c.truncated ? [size, tr("remoteRuns.contentHead", { cap: fmtBytes(256 * 1024) })] : [size];
  return valueBlock({ label, notes, tools: [iconBtn("eye-off", tr("remoteRuns.hideContent"), { data: { rcontenthide: r.runId } })] },
    textNode(c.text, true, "logs").node);
}

/** What a remote.write wrote (SPEC §remote.history): the body is kept sealed beside the record and
 *  unsealed only on Show - an opened row in a screenshot shares no file until asked. */
function writtenNode(r                 )         {
  const bytes = r.input && r.input.contentBytes || 0;
  const size = fmtBytes(bytes);
  if (r.contentEvicted) return note(tr("remoteRuns.contentEvicted", { size }));
  if (!r.contentStored) {
    // Between 9a6aebe and R12 a write kept its size and nothing else. Such a row drew no
    // block at all, which reads exactly like one whose Show has not been pressed yet.
    return kindOf(r) === "write" && bytes > 0 ? note(tr("remoteRuns.contentNotKept", { size })) : null;
  }
  const label = tr("remoteRuns.contentWritten");
  const c = contents[r.runId];
  if (!c) {
    return valueBlock({ label, notes: [size], tools: [iconBtn("eye", tr("remoteRuns.showContent"), { data: { rcontent: r.runId } })] });
  }
  if ("error" in c) return valueBlock({ label, notes: [size] }, note(c.error));
  const notes = c.truncated ? [size, tr("remoteRuns.contentHead", { cap: fmtBytes(256 * 1024) })] : [size];
  return valueBlock({ label, notes, tools: [btn(tr("remoteRuns.hideContent"), { data: { rcontenthide: r.runId } })] },
    textNode(c.text, true, "logs").node);
}

async function loadContent(id        )                {
  if (unsealing[id]) return; // a Show racing the open that already asked
  unsealing[id] = true;
  const v = visit;
  let j                                                 = null;
  try {
    j = await apiJson                                         ("/api/remote/runs/" + id + "/content");
  } finally {
    if (v === visit) delete unsealing[id];
  }
  if (v !== visit) return; // the page left: an unsealed body lives only while it is on screen
  contents[id] = j ? { text: j.content, truncated: !!j.truncated } : { error: tr("remoteRuns.contentUnavailable") };
  repaintBody(id);
}

/** Unseal a recorded run's stdin as its row opens - the run's script belongs beside its output.
 *  Asked when a row opens and when an open live row becomes a recorded one (adoptFinished),
 *  never on a re-read: a stdin someone put away with Hide stays away. A write's body still
 *  waits for Show (it can be a whole `.env`). */
function showStdin(id        )       {
  const r = findRun(id);
  if (!r || !r.stdinStored || isLiveRun(id) || contents[id] || unsealing[id]) return;
  void loadContent(id);
}

/** The drawn row a run id heads, or null. */
function itemEl(id        )                     {
  const root = document.querySelector             ("#pane .tl");
  if (!root) return null;
  return Array.prototype.find.call(root.querySelectorAll             (".tl-item"),
    (n             ) => n.dataset.tlId === String(id))                            || null;
}

function repaintBody(id        )       {
  const body = itemEl(id)?.querySelector             (":scope > .tl-body");
  const run = runOf(id);
  if (body && run.length) fill(body, bodyNode(run));
}

/** One recorded run's output, from the cursor the previous read ended on (128 KB a read). */
async function loadBody(id        , more         )                {
  const b = bodies[id];
  if (b && !more) return;
  const after = b && !("gone" in b) ? b.next : 0;
  const v = visit;
  const j = await apiJson                    ("/api/remote/runs/" + id + "/output?after=" + after + "&max=131072");
  if (v !== visit) return;
  if (!j) { bodies[id] = { gone: true }; repaintBody(id); return; }
  bodies[id] = {
    text: (b && !("gone" in b) ? b.text : "") + (j.output || ""),
    next: j.nextCursor || 0,
    total: j.total || 0,
  };
  repaintBody(id);
}

/** Pull what an open live row has not shown yet; a terminal answer ends the following. The
 *  first bytes repaint the body (the "No output yet" note becomes the output); after that only
 *  the text moves, so a reader's selection in it survives. */
async function pullLive(id        )                {
  const l = live[id] || (live[id] = { text: "", cursor: 0 });
  const had = !!l.text;
  const v = visit;
  const j = await apiJson                   ("/api/runs/" + id + "/output?after=" + l.cursor + "&max=131072");
  if (!j || v !== visit) return;
  if (j.output) {
    l.text += j.output;
    if (l.text.length > LIVE_TAIL_MAX) {
      l.text = l.text.slice(l.text.length - LIVE_TAIL_MAX);
      l.capped = true;
    }
  }
  l.cursor = j.nextCursor || l.cursor;
  // The capped note rides the text in BOTH paint paths - the surgical update below and
  // bodyNode's first paint - so a row opened after the cap reads the same story as one that
  // watched the cap happen (master's prefix, kept on the library timeline's own nodes).
  if (l.text && !had) repaintBody(id);
  else if (l.text) {
    // The live block by its mark: the body's first block is the Command.
    const pre = itemEl(id)?.querySelector(":scope > .tl-body [data-rlive] > pre");
    if (pre) pre.textContent = (l.capped ? tr("remoteRuns.liveCappedTail") + "\n" : "") + l.text;
  }
  // The run moved into the record: re-read through the owning route, coalesced - a burst of
  // terminals still costs one /api/remote/runs, never one per open row (SPEC §remote.panel).
  if (j.terminal) reRead?.push();
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

/** Expand or collapse one row in place - a poll must not close what was just opened. A folded
 *  row is open when any run in it is; closing it forgets them all. */
function toggle(id        )       {
  const run = runOf(id);
  const wasOpen = run.some((r) => { return !!open[r.runId]; });
  run.forEach((r) => { open[r.runId] = false; });
  open[id] = !wasOpen;
  // Before the first paint: the read marks the stdin as being unsealed, so the body opens on
  // its loading line rather than flashing the Show it is about to replace.
  if (open[id]) showStdin(id);
  const root = document.querySelector             ("#pane .tl");
  if (root && run.length) timelineToggle(root, String(id), open[id] ? bodyNode(run) : undefined);
  if (open[id]) {
    if (isLiveRun(id)) void pullLive(id); else void loadBody(id, false);
  }
  armLive();
}

/* Runs whose cancel is in flight. The gateway's cancel AWAITS the run's stop (runs.rs: "so
   when it returns the child is reaped and the pipe readers are joined"), which for a remote
   channel is seconds, not milliseconds — and until 2026-09-29 the button said nothing at all
   for that whole time. The owner clicked it twice and asked where the state was. */
let canceling = {}                           ;

async function cancelRun(id        )                {
  if (canceling[id]) return; // a second click would be a second wait for the same stop
  canceling[id] = true;
  repaintBody(id); // the button says what it is doing, before the request that takes the time
  const v = visit;
  let j          = null;
  try {
    j = await apiJson("/api/runs/" + id + "/cancel", { method: "POST" });
  } finally {
    delete canceling[id];
  }
  if (!j) { if (v === visit) repaintBody(id); return; } // apiJson toasted; the button comes back
  // The toast is the panel's, not the page's: it is said wherever the operator is now.
  toast(tr("remoteRuns.cancelRequestedRunId", { id }));
  if (v !== visit) return;
  await refresh();
  // The answer means the run has STOPPED, so this row is a recorded one now: its output has
  // to come from the record, or the open body sits on the loading spinner until the page is
  // reloaded by hand ("确实取消了，但是还得转圈，我刷新才有").
  adoptFinished();
}

/* Bodies being fetched for rows that finished while open — so the 6 s poll does not
   re-request the same output every time it runs. */
let adopting = {}                           ;

/** A row that was opened while its run was live, and whose run has since finished: the live
 *  body it was showing is gone from `active`, and bodyNode's recorded branch has nothing to
 *  draw but the loading note. Nobody asks for the recorded output in that transition — toggle
 *  only asks when a row is OPENED — so the row span forever. This asks, once. */
function adoptFinished()       {
  Object.keys(open).forEach((k        )       => {
    const id = Number(k);
    if (!open[id] || isLiveRun(id) || bodies[id] || adopting[id]) return;
    if (!runOf(id).length) return; // not on this page any more; nothing to fill
    adopting[id] = true;
    showStdin(id); // the record now holds the sealed stdin the live row could not show
    void loadBody(id, false).finally(()       => { delete adopting[id]; });
  });
}

async function clearAll()                {
  if (!confirm(tr("remoteRuns.forgetEveryRecordedRemote"))) return;
  const j = await apiJson("/api/remote/runs", { method: "DELETE" });
  if (!j) return;
  bodies = {};
  contents = {};
  unsealing = {};
  open = {};
  cursors = [null];
  page = 0;
  // The results named runs the record no longer holds: back to the (empty) list.
  const box = $                  ("rrQ");
  if (box) box.value = "";
  setNeedle("");
  toast(tr("remoteRuns.runRecordCleared"));
  await refresh();
}

function keptText()         {
  return trn(usage.runs, "remoteRuns.nRunsRecordedBytes.one", "remoteRuns.nRunsRecordedBytes.other", { bytes: fmtBytes(usage.bytes) }) +
    (limits ? tr("remoteRuns.capKeptDDays", { cap: fmtBytes(limits.maxTotalBytes), d: Math.round(limits.maxAgeMs / 86400000) }) : "") +
    (limits && limits.auditWindowMs ? tr("remoteRuns.traceableWDays", { w: Math.round(limits.auditWindowMs / 86400000) }) : "");
}

/** The page's frame, once a visit: a poll or a search repaints the list and patches the head
 *  (paintHead), so the search box keeps its text, caret and focus while someone types. */
function render()       {
  painted = signature();
  paintedTargets = targetIds.join(",");
  const options = [h("option", { value: "" }, tr("remoteRuns.allTargets"))].concat(targetIds.map((id) => {
    return h("option", { value: id, selected: id === target }, id);
  }));
  // One list is the whole page, so the head carries it: one sentence, what the record keeps,
  // and the list's search, filter and ⋯ (Clear - a destructive verb gets no standing button;
  // the Jobs and Tunnels head ⋯ hold page-wide verbs too). No section caption: "Runs" would
  // name the page the context bar already names, and a captionless section head was a row
  // holding only the tools, over the note (SPEC §panel.pages, found on a walk).
  fill($("pane"),
    paneBody({ wide: true },
      paneHead({
        desc: tr("remoteRuns.descOneLine"),
        sub: keptText(),
        actions: [
          filterInput({ id: "rrQ", placeholder: tr("remoteRuns.search"), label: tr("remoteRuns.searchLabel"), value: search.q }),
          h("select", { id: "rrTarget", aria: { label: tr("remoteRuns.filterTarget") } }, options),
          moreBtn(tr("remoteRuns.moreActions"), { id: "rrMore" }),
        ],
      }),
      h("div", { id: "rrList" })));
  paintList();
  $("countChip").textContent = countText();
}

/** Repaint ONLY the filter's options when the target list moved (SPEC §remote.verify: the
 *  filter follows the table). The select keeps its identity - its skin (ui/select.ts)
 *  repaints the label on innerHTML writes and builds the dropdown at open - so a targets
 *  change never rebuilds the head. A target the filter had chosen can vanish: back to All,
 *  and true - the read in hand is that target's slice, so the caller reads page 0 of the
 *  whole record before it paints anything. */
function paintTargetFilter()          {
  const sel = document.querySelector                   ("#rrTarget");
  if (!sel) return false; // not painted yet: render()'s options carry the list
  const keeping = sel.value;
  fill(sel, [h("option", { value: "", selected: keeping === "" }, tr("remoteRuns.allTargets"))].concat(targetIds.map((id) => {
    return h("option", { value: id, selected: id === keeping }, id);
  })));
  if (!keeping || targetIds.includes(keeping)) return false;
  sel.value = "";
  target = "";
  cursors = [null];
  page = 0;
  return true;
}

/** What a poll may change in the head: the budget line and the shell's count. */
function paintHead()       {
  const sub = document.querySelector             ("#pane .pane-sub");
  if (sub) sub.textContent = keptText();
  $("countChip").textContent = countText();
}

function paintList()       {
  const region = $("rrList");
  if (!region) return;
  if (search.q) { paintSearch(region); return; }
  if (!active.length && !runs.length) {
    fill(region,
      page || target
        ? note(page ? tr("remoteRuns.nothingPage") : tr("remoteRuns.runsTarget"))
        : emptyNode({ icon: "history", title: tr("remoteRuns.runs2"), hint: tr("remoteRuns.runSomethingTargetSwiss") }),
      page ? pagerNode() : null);
    return;
  }
  const items = allItems();
  const openIds = new Set        ();
  for (const run of collapseRuns(items)) {
    if (run.some((it) => { return !!open[Number(it.id)]; })) openIds.add(run[0].id);
  }
  fill(region,
    timeline(items, {
      open: openIds,
      body: (it) => { return bodyNode(runOf(Number(it.id))); },
    }),
    pagerNode());
}

// Newer/Older, the Traffic pager's shape: Newer always means toward the top of a
// newest-first list.
function pagerNode()         {
  if (!page && !nextBefore) return null;
  return pager({
    label: tr("remoteRuns.runPages"), status: tr("remoteRuns.pageN", { n: page + 1 }),
    prev: btn(tr("remoteRuns.newer"), { id: "rrPrev", disabled: page <= 0 }),
    next: btn(tr("remoteRuns.older"), { id: "rrNext", disabled: !nextBefore }),
  });
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
  onScreen = true;
  const v = ++visit;
  subscribeLive();
  await loadTargets();
  if (v !== visit) return;
  if (!(await load(v))) return;
  render();
  $("pane").onclick = (event            )       => {
    const t = targetEl(event);
    if (!t) return;
    if (t.closest("#rrSearchMore")) { void scanRound(); return; }
    if (t.closest("#rrSearchAgain")) { startSearch(search.q); return; }
    const more = t.closest             ("[data-rmore]");
    if (more) { void loadBody(Number(more.dataset.rmore), true); return; }
    const cancel = t.closest             ("[data-rcancel]");
    if (cancel) { void cancelRun(Number(cancel.dataset.rcancel)); return; }
    const copy = t.closest             ("[data-rcopy]");
    if (copy) {
      const r = findRun(Number(copy.dataset.rcopy));
      if (r) void copyLogText(commandLine(r));
      return;
    }
    const show = t.closest             ("[data-rcontent]");
    if (show) { void loadContent(Number(show.dataset.rcontent)); return; }
    const hide = t.closest             ("[data-rcontenthide]");
    if (hide) { delete contents[Number(hide.dataset.rcontenthide)]; repaintBody(Number(hide.dataset.rcontenthide)); return; }
    if (t.closest("#rrPrev")) { void step(-1); return; }
    if (t.closest("#rrNext")) { void step(1); return; }
    const menu = t.closest             ("#rrMore");
    if (menu) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      if (menuOpen()) { closeMenu(); return; }
      popupMenu(menu.getBoundingClientRect(), [{
        label: tr("remoteRuns.clear"), danger: true, disabled: !usage.runs,
        fn: ()       => { void clearAll(); },
      }]);
      return;
    }
    // A row's summary is a real <button> (ui/timeline.ts): Enter and Space arrive as this click.
    // Only the summary toggles - a click inside the open body is someone reading it.
    const sum = t.closest             (".tl-sum");
    const item = sum ? sum.closest             ("[data-rrun]") : null;
    if (item) toggle(Number(item.dataset.rrun));
  };
  // The target filter select - ONE delegated change listener instead of a per-render
  // assignment (SPEC §panel.toolchain); the value is read at event time.
  $("pane").onchange = (event       )       => {
    const sel = targetEl(event)?.closest                   ("#rrTarget");
    if (!sel) return;
    target = sel.value;
    cursors = [null];
    page = 0;
    // The results narrow on the spot - each found run's record names its target.
    if (search.q) paintList();
    void refresh();
  };
  // The search box: typing searches after a pause, Enter at once, Escape empties it. Read from
  // the pane at event time, so the box needs no handler of its own.
  $("pane").oninput = (event       )       => {
    const t = targetEl(event);
    if (t && t.id === "rrQ") queueNeedle((t                    ).value);
  };
  $("pane").onkeydown = (event               )       => {
    const t = targetEl(event);
    if (!t || t.id !== "rrQ") return;
    const box = t                    ;
    if (event.key === "Enter") {
      const q = box.value.trim();
      // The same needle again: a failed search starts over, a paused one goes on.
      if (q !== search.q) setNeedle(q);
      else if (search.error) startSearch(q);
      else void scanRound();
    } else if (event.key === "Escape" && box.value) {
      // The shell listens for Escape at document; emptying the box is all this one does.
      event.preventDefault();
      event.stopPropagation();
      box.value = "";
      setNeedle("");
    }
  };
}

export async function refresh() {
  if (!onScreen) return; // a late continuation of a visit that has ended
  const v = visit;
  if (!(await load(v))) return;
  // The filter's list is not part of the signature: a targets notice repaints the select
  // alone, so the rows keep their nodes (and an open live body its place) while the
  // table moves under them. A chosen target that vanished is read again here, unfiltered,
  // before anything paints: the slice in hand belongs to a target the select no longer offers.
  const idsNow = targetIds.join(",");
  if (idsNow !== paintedTargets) {
    paintedTargets = idsNow;
    if (paintTargetFilter()) {
      if (search.q) paintList(); // the results widen with the filter, as a choice in the select does
      if (!(await load(v))) return;
    }
  }
  // A poll that changed nothing repaints nothing: open rows keep their nodes and the
  // output someone is reading stays where it is (the Traffic discipline).
  if (signature() === painted) return;
  painted = signature();
  paintHead();
  // The results are what the search found, not this page of the record: a poll leaves them.
  if (!search.q) paintList();
  armLive();
  // A run that ended between two polls - cancelled, or simply finished - takes its live
  // body with it; an open row then has nothing to show until its recorded output is read.
  adoptFinished();
}
/** Subscribe before the first read (SPEC §remote.panel): every event of the page's topics
 *  re-reads through the owning routes, coalesced as a notice is (bus/live.ts), and every
 *  WELCOME re-reads too. The three topics: this lane's runs (HOST.RUNS `remote` — a run from
 *  any surface appears the moment it is queued or moves; the body is a RunChanged, not a bare
 *  notice), the record (REMOTE.HISTORY — a run filed, Clear, retention), and the target table
 *  (REMOTE.TARGETS — the filter's list).
 *
 *  Only a targets move (or a resync) re-reads the target table: a run event never touches it,
 *  and re-reading /api/remote/targets on every run move was a request the page did not owe
 *  (SPEC §remote.verify). */
function subscribeLive()       {
  stopLive();
  let targetsDirty = false; // mount's own load paints the filter's list before the first read
  reRead = coalescedRead(async () => {
    if (targetsDirty) {
      targetsDirty = false;
      await loadTargets();
    }
    await refresh();
  });
  const push = ()       => { reRead?.push(); };
  const targetsMoved = ()       => {
    targetsDirty = true;
    reRead?.push();
  };
  offs = [
    busOnResync(targetsMoved),
    busOn({ module: Module.HOST, sub: HostSub.RUNS, key: "remote" }, RunsId.RUNS_CHANGED, RunChangedSchema, push),
    busOn({ module: Module.REMOTE, sub: RemoteSub.TARGETS, key: "" }, TargetsId.TARGETS_CHANGED, NoticeSchema, targetsMoved),
    busOn({ module: Module.REMOTE, sub: RemoteSub.HISTORY, key: "" }, HistoryId.HISTORY_CHANGED, NoticeSchema, push),
  ];
}

function stopLive()       {
  for (const off of offs) off();
  offs = [];
  reRead?.stop();
  reRead = null;
}

/* No poll export (SPEC §remote.panel): the tick reads nothing here. The list moves on bus
 * events; an open live row follows its output on its own 1.5 s timer (armLive). */
export function countText() { return usage.runs ? trn(usage.runs, "remoteRuns.nRuns.one", "remoteRuns.nRuns.other") : ""; }
export function unmount() {
  onScreen = false;
  visit++;
  stopLive();
  if (liveTimer) { clearInterval(liveTimer); liveTimer = null; }
  // A return visit starts on page 0 with no filter: the record moved on while the page
  // was away, so a stale cursor or filter would show a stale slice of it.
  open = {};
  bodies = {};
  contents = {}; // an unsealed body lives only while its page is on screen
  unsealing = {};
  live = {};
  canceling = {};
  adopting = {};
  painted = "";
  paintedTargets = "";
  page = 0;
  cursors = [null];
  target = "";
  if (searchTimer) { clearTimeout(searchTimer); searchTimer = null; }
  search = newSearch("", search.gen + 1); // and the needle - a scan in flight answers no one
}

/* --- searching the recorded output (SPEC §remote.panel, §host.history) ----------------------
   The head's box searches what the recorded runs printed, through the host's literal scan
   (POST /api/history/query, module "remote"): ASCII letters fold, everything else matches as
   typed. The scan walks the record OLDEST first in bounded batches and resumes from a signed
   cursor, so one round asks until it holds ROUND_RUNS more runs, the record ends, or
   ROUND_ASKS batches went by - then Keep searching runs the next round. An unfinished scan
   that found nothing is never "no matches" (§host.history): only a complete one may say so.
   A run found is drawn as its own record row, newest first, with the line its first match is
   on. Runs still in flight are not searched: their output is not in the record yet. */
const SEARCH_PAUSE_MS = 300; // typing searches once the hand stops, not on every key
const ROUND_RUNS = 50;
const ROUND_ASKS = 32; // a batch reads 64 chunks (search.rs DEFAULT_BATCH_CHUNKS)
/* Hits one batch may answer before it stops early. The page lists RUNS, so a needle on every
   line of one output must not end a batch that has found one run. */
const HIT_LIMIT = 500;
const READ_AT_ONCE = 6; // records and matched lines fetched side by side
/* The bytes read on either side of a match for its line: a line's worth of context in any
   script, and still one short read a row. */
const CONTEXT_BYTES = 384;
const LINE_BEFORE = 48; // characters of the match's line kept in front of it
const LINE_AFTER = 160;

                                                                           

                       
            
                                                                                    
              
                
                    
                                         
                                                                                                
                  
                                                    
                                                                                               
                                                  
                                  
                
                       
 

                             
                                                              
                     
                                
                     
                                                
 

function newSearch(q        , gen        )              {
  return { q, gen, busy: false, complete: false, cursors: null, found: [], first: {}, records: {}, hits: {}, error: "", unavailable: false };
}

let search = newSearch("", 0);
let searchTimer = null                                        ;
let searchPainted = "";

/** A read whose failure the page says in place, or not at all - never apiJson's toast: a run
 *  that rolled out of the record between the scan and its read is nothing to shout about. */
async function quiet   (path        , opts              )                    {
  try {
    const r = await api(path, opts);
    return r.ok ? await r.json()      : null;
  } catch (e) {
    return null;
  }
}

function queueNeedle(raw        )       {
  if (searchTimer) clearTimeout(searchTimer);
  searchTimer = null;
  const q = raw.trim();
  // An emptied box (the field's ×, a select-all delete) is the list again, at once.
  if (!q) { setNeedle(""); return; }
  searchTimer = setTimeout(() => { searchTimer = null; setNeedle(q); }, SEARCH_PAUSE_MS);
}

/** A needle that differs from the one on screen starts over; the same one changes nothing. */
function setNeedle(q        )       {
  if (searchTimer) { clearTimeout(searchTimer); searchTimer = null; }
  if (q !== search.q) startSearch(q);
}

/** A search from the beginning; "" is the list. Rows open in the old picture close - the new
 *  one may not hold them. */
function startSearch(q        )       {
  search = newSearch(q, search.gen + 1);
  open = {};
  armLive();
  paintList();
  if (q) void scanRound();
}

/** One round: batches until ROUND_RUNS more runs are found, the record ends, a batch fails or
 *  ROUND_ASKS batches went by. Each batch's runs are read and painted as they come, so the
 *  first results show while the rest of the record is still being read. */
async function scanRound()                {
  const s = search;
  if (!s.q || s.busy || s.complete || s.error) return;
  s.busy = true;
  paintList();
  const want = s.found.length + ROUND_RUNS;
  for (let asks = 0; asks < ROUND_ASKS && !s.complete && s.found.length < want; asks++) {
    const fresh = await scanBatch(s);
    if (s !== search) return; // a newer needle, or the page was left
    if (!fresh) break;
    if (fresh.length) {
      await readFound(s, fresh);
      if (s !== search) return;
    }
    paintList();
  }
  s.busy = false;
  paintList();
}

/** One batch of the scan: the runs it found for the first time, or null when the scan cannot
 *  go on (the state says why). */
async function scanBatch(s             )                           {
  const body                          = { q: s.q, modules: ["remote"], limit: HIT_LIMIT };
  if (s.cursors) body.cursors = s.cursors;
  const j = await quiet                   ("/api/history/query", { method: "POST", body: JSON.stringify(body) });
  if (s !== search) return null;
  if (!j) { s.error = tr("remoteRuns.searchFailed"); return null; }
  // The remote plugin's record is not open (the plugin is stopped): nothing to search, said so.
  if ((j.skipped || []).includes("remote")) { s.unavailable = true; s.complete = true; return null; }
  const errs = j.errors || [];
  if (errs.length) {
    s.error = tr("remoteRuns.searchStopped", { why: errs.map((e) => { return e.reason; }).join("; ") });
    return null;
  }
  const fresh           = [];
  for (const x of j.hits || []) {
    const seen = s.first[x.runId];
    if (seen) { seen.n++; continue; }
    s.first[x.runId] = { ofs: x.byteOfs, n: 1 };
    s.found.push(x.runId);
    fresh.push(x.runId);
  }
  s.complete = !!j.complete;
  s.cursors = j.next && j.next.remote ? j.next : null;
  if (!s.complete && !s.cursors) {
    // Unfinished with no cursor to resume from: asking again would start over at the oldest run.
    s.error = tr("remoteRuns.searchFailed");
    return null;
  }
  return fresh;
}

/** Each found run's record row and matched line, a few reads at a time. A row the page already
 *  holds is not read again. */
async function readFound(s             , ids          )                {
  for (let i = 0; i < ids.length; i += READ_AT_ONCE) {
    await Promise.all(ids.slice(i, i + READ_AT_ONCE).map(async (id)                => {
      const onPage = runs.find((r) => { return r.runId === id; });
      const [row, hit] = await Promise.all([
        onPage ? Promise.resolve(onPage) : quiet                 ("/api/remote/runs/" + id),
        readHit(s, id),
      ]);
      if (s !== search) return;
      s.records[id] = row && row.runId === id ? row : null;
      if (hit) s.hits[id] = hit;
    }));
    if (s !== search) return;
  }
}

/** The line a run's first match is on, read through the record-body route around its offset. */
async function readHit(s             , id        )                            {
  const at = s.first[id].ofs;
  const from = Math.max(0, at - CONTEXT_BYTES);
  // Room for the needle itself at up to four bytes a character, then the context after it.
  const max = at - from + s.q.length * 4 + CONTEXT_BYTES;
  const j = await quiet                                   ("/api/history/remote/records/" + id + "/body?after=" + from + "&max=" + max);
  return j && !j.gone && j.text ? hitIn(j.text, s.q, from > 0) : null;
}

/** The scan's own folding (search.rs fold): ASCII letters only, so lengths and offsets agree. */
function foldAscii(text        )         {
  return text.replace(/[A-Z]/g, (c) => { return c.toLowerCase(); });
}

/** The first match of `q` in `text` with the rest of its line on either side: whitespace runs
 *  read as one space, each side cut to fit one row. `cut` says the text starts part-way into
 *  the output, so a line reaching its start was cut there too. */
export function hitIn(text        , q        , cut         )                   {
  const at = q ? foldAscii(text).indexOf(foldAscii(q)) : -1;
  if (at < 0) return null;
  const end = at + q.length;
  const lineStart = text.lastIndexOf("\n", at - 1) + 1;
  const lineEnd = text.indexOf("\n", end);
  const flat = (s        )         => { return s.replace(/\s+/g, " "); };
  let before = flat(text.slice(lineStart, at)).replace(/^ /, "");
  if (before.length > LINE_BEFORE) before = "…" + before.slice(before.length - LINE_BEFORE);
  else if (cut && lineStart === 0 && before) before = "…" + before;
  let after = flat(text.slice(end, lineEnd < 0 ? text.length : lineEnd)).replace(/ $/, "");
  if (after.length > LINE_AFTER) after = after.slice(0, LINE_AFTER) + "…";
  return { before, match: text.slice(at, end), after };
}

/** The found runs as rows, newest first: the target filter applies, a run the record no longer
 *  holds drops out, and nothing folds - every row is a match of its own. */
function searchItems()                 {
  return search.found.slice().sort((a, b) => { return b - a; })
    .map((id) => { return search.records[id]; })
    .filter((r)                       => { return !!r && (!target || runTarget(r) === target); })
    .map((r) => { return { ...runItem(r, false), same: undefined, hit: search.hits[r.runId] }; });
}

/** What the search is doing, above its rows. */
function searchStatusNode(shown        )         {
  const s = search;
  if (s.unavailable) return note(tr("remoteRuns.searchUnavailable"));
  if (s.error) return note(s.error, { err: true });
  if (s.busy) {
    return note(shown ? trn(shown, "remoteRuns.searchingN.one", "remoteRuns.searchingN.other") : tr("remoteRuns.searching"), { busy: true });
  }
  if (s.complete) {
    if (shown) return note(trn(shown, "remoteRuns.foundN.one", "remoteRuns.foundN.other", { q: s.q }));
    return note(target ? tr("remoteRuns.noMatchTarget", { q: s.q }) : tr("remoteRuns.noMatch", { q: s.q }));
  }
  // Paused with the newer part of the record unread - never "no matches" (§host.history).
  return note(shown ? trn(shown, "remoteRuns.pausedN.one", "remoteRuns.pausedN.other") : tr("remoteRuns.pausedNone"));
}

/** The results in the list's place: the status, the rows, then Keep searching or Search again.
 *  Three holders, so a batch that found nothing new repaints the status line alone and an open
 *  row is left as it is. */
function paintSearch(region             )       {
  let status = $("rrSearchStatus");
  let list = $("rrSearchList");
  let foot = $("rrSearchFoot");
  if (!status || !list || !foot) {
    status = h("div", { id: "rrSearchStatus", aria: { live: "polite" } });
    list = h("div", { id: "rrSearchList" });
    foot = h("div", { id: "rrSearchFoot" });
    fill(region, status, list, foot);
    searchPainted = "";
  }
  const items = searchItems();
  fill(status, searchStatusNode(items.length));
  const sig = search.gen + "|" + target + "|" + items.map((it) => { return it.id + (it.hit ? "+" : ""); }).join(",");
  if (sig !== searchPainted) {
    searchPainted = sig;
    const openIds = new Set(items.filter((it) => { return !!open[Number(it.id)]; }).map((it) => { return it.id; }));
    fill(list, items.length ? timeline(items, { open: openIds, body: (it) => { return bodyNode(runOf(Number(it.id))); } }) : null);
  }
  const s = search;
  fill(foot, s.error
    ? btn(tr("remoteRuns.searchAgain"), { id: "rrSearchAgain" })
    : !s.complete && !s.busy ? btn(tr("remoteRuns.keepSearching"), { id: "rrSearchMore" }) : null);
}

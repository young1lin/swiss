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

   The event list (docs/46 §3.6, the Logs and Traffic list): the time, with the date as the
   day heading; what ran (the kind, then its command); who - the target and the actor, a
   column only while they vary; a run that did not succeed as a red tag saying how; the
   duration. Identical consecutive runs fold into ×N. A row's body is fetched when it opens
   and a poll never rebuilds what is open. A running row's body follows its output on a
   short timer while it is open; when the run ends the next poll moves the row from the
   live list into the record with the same run id.
   ================================================================================================ */
import type { ApiRemoteRunOutput, ApiRemoteRunsResponse, ApiRemoteTargetsResponse, ApiRunOutputChunk } from "../types/api.js";
import type { ApiRemoteRunRow, RemoteLiveBody, RemoteRunBody, RemoteRunContent, RemoteRunInput } from "../types/runs.js";
import { $, apiJson, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
import type { HChild } from "../h.js";
import { tr, trn } from "../i18n.js";
import { copyLogText, readableBody } from "../logs.js";
import {
  btn, closeMenu, collapseRuns, emptyNode, iconBtn, menuOpen, moreBtn, note, pager, paneBody, paneHead, popupMenu, textNode, timeline, timelineMeta, timelineToggle, valueBlock,
} from "../ui/index.js";
import type { TimelineItem } from "../ui/index.js";

const PAGE = 20;
const LIVE_EVERY_MS = 1500; // how often an OPEN live row pulls its output; the 6 s poll moves the list
/** What one open live row may hold, in characters — the same 256 KB the gateway itself keeps
 *  live (runs.rs MAX_LIVE_OUTPUT_BYTES): a runaway stream must not grow the tab's memory while
 *  someone watches, and the finished record repaints the row from its durable tail. */
const LIVE_TAIL_MAX = 256 * 1024;

let runs = [] as ApiRemoteRunRow[]; // the recorded page, newest first
let active = [] as ApiRemoteRunRow[]; // remote runs the coordinator still holds (queued / running)
let nextBefore = null as number | null; // the cursor for the older page, null on the last one
let cursors = [null] as (number | null)[]; // cursors[i] loaded page i; page 0 has none
let page = 0;
let usage = { bytes: 0, runs: 0 };
let limits = null as ApiRemoteRunsResponse["limits"] | null;
let target = ""; // the target filter, "" for all
let targetIds = [] as string[]; // for the filter select, from the targets table
let open = {} as Record<number, boolean>; // runId -> true while its row is expanded
let bodies = {} as Record<number, RemoteRunBody>; // runId -> { text, next, total, done, capped, tail } for opened recorded rows
let live = {} as Record<number, RemoteLiveBody>; // runId -> { text, cursor } for opened active rows
let contents = {} as Record<number, RemoteRunContent>; // runId -> a write's unsealed body while Show is on
let liveTimer = null as ReturnType<typeof setInterval> | null;
let painted = "";

async function load() {
  const q = "/api/remote/runs?limit=" + PAGE +
    (cursors[page] ? "&before=" + cursors[page] : "") +
    (target ? "&target=" + encodeURIComponent(target) : "");
  const j = await apiJson<ApiRemoteRunsResponse>(q);
  if (!j) return false; // apiJson toasted; keep the last paint
  runs = j.runs as ApiRemoteRunRow[] || [];
  active = j.active as ApiRemoteRunRow[] || [];
  nextBefore = j.nextBefore || null;
  usage = j.usage || { bytes: 0, runs: 0 };
  limits = j.limits || null;
  return true;
}

async function loadTargets() {
  const t = await apiJson<ApiRemoteTargetsResponse>("/api/remote/targets");
  if (t && t.targets) targetIds = t.targets.map((x) => { return x.id; });
}

function signature(): string {
  return page + "|" + target + "|" + usage.bytes + "/" + usage.runs + "|" +
    active.map((r) => { return r.runId + "=" + r.state; }).join(",") + "|" +
    runs.map((r) => { return r.runId; }).join(",");
}

function fmtBytes(n: number): string {
  if (!n) return "0 B";
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
  return (n / (1024 * 1024)).toFixed(1) + " MB";
}

/* --- what a run was and how it ended (the Targets page's last-run column reads these too) ---- */

/** The capability that ran: exec, sync, pull, cat, write. */
function kindOf(r: ApiRemoteRunRow): string {
  return (r.action || "").replace(/^remote\./, "") || "run";
}

/** What ran, without the kind: the argv for an exec, the shape for a file action. An active row
 *  that predates the record (an older gateway) falls back to its label. */
function argOf(r: ApiRemoteRunRow): string {
  const input = r.input || {} as RemoteRunInput;
  const kind = kindOf(r);
  if (kind === "exec" && input.argv) return input.argv.join(" ");
  if (kind === "sync") return (input.source || ".") + (input.to ? " → " + input.to : "");
  if (kind === "pull") return (input.remote || "") + (input.to ? " → " + input.to : "");
  if (kind === "cat" || kind === "write") return input.remote || "";
  return input.argv ? String(input.argv) : r.label || "";
}

/** What ran, as one command line: the argv for an exec, "sync . → /to" for a file action. */
export function commandOf(r: ApiRemoteRunRow): string {
  const kind = kindOf(r);
  return kind === "exec" && r.input && r.input.argv ? argOf(r) : (kind + " " + argOf(r)).trim();
}

/** One argv word as a POSIX shell reads it back: bare when every character is in the gateway's
 *  safe set, else single-quoted with the '\'' splice - quote_posix in the tunnels crate, the
 *  quoting the exec was actually sent with. */
function shellWord(w: string): string {
  if (w && /^[A-Za-z0-9_.:=/@%+,-]+$/.test(w)) return w;
  return "'" + w.replace(/'/g, "'\\''") + "'";
}

/** The whole command, exactly: an exec's argv quoted word by word (a pasted copy runs as the
 *  record says - the row's own line joins with plain spaces and is cut to fit), a file action
 *  as its kind and path. */
export function commandLine(r: ApiRemoteRunRow): string {
  const argv = r.input && r.input.argv;
  return kindOf(r) === "exec" && argv ? argv.map(shellWord).join(" ") : commandOf(r);
}

export function runTarget(r: ApiRemoteRunRow): string {
  return (r.meta && r.meta.target) || (r.input && r.input.target) || "";
}

/** How a finished run failed, in the words its red tag says - null for one that succeeded.
 *  Every run that did not succeed is red, the Jobs history's rule: a non-zero exit says its
 *  code; a file action has none, so its state says it (failed, timed out, canceled). */
export function runOutcome(r: ApiRemoteRunRow): string | null {
  if (r.exitCode != null && r.exitCode !== 0) return tr("remoteRuns.exitN", { n: r.exitCode });
  if (r.state === "canceled" || r.canceled) return tr("remoteRuns.canceled");
  if (r.state === "timeout" || r.timedOut) return tr("remoteRuns.timed");
  if (r.state === "failed") return tr("remoteRuns.stateFailed");
  return null;
}

/** The state in words for the open row's meta: the exit code when there is one, else the state. */
function stateWord(r: ApiRemoteRunRow): string {
  if (r.state === "running") return tr("remoteRuns.stateRunning");
  if (r.state === "queued") return tr("remoteRuns.stateQueued");
  if (r.exitCode != null) return tr("remoteRuns.exitN", { n: r.exitCode });
  return runOutcome(r) || (r.state === "succeeded" ? tr("remoteRuns.stateSucceeded") : r.state);
}

/* --- the list ------------------------------------------------------------------------------ */

function isLiveRun(id: number): boolean {
  return active.some((r) => { return r.runId === id; });
}

/** A run as a timeline row; `rrun` addresses it for the delegated listener. */
function runItem(r: ApiRemoteRunRow, isLive: boolean): TimelineItem {
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
    // docs/41 A1: the actor is an identifier, never translated; a row an older gateway
    // recorded has none and says only its target.
    who: [tgt, actorLabel(r.actor)].filter(Boolean).join(" · "),
    ms: isLive ? undefined : r.ms,
    status: bad ? { text: bad, tone: "bad" } : undefined,
    live: isLive
      ? { text: r.state === "queued" ? tr("remoteRuns.stateQueued") : tr("remoteRuns.stateRunning"), queued: r.state === "queued" }
      : undefined,
    // Identical consecutive runs fold (an agent's `git status` loop). A run in flight never
    // folds, and a different amount of output keeps two runs apart - the row cannot show it.
    same: isLive ? undefined : [kind, arg, tgt, actorLabel(r.actor), r.input?.cwd || "", bad || "ok", r.outputBytes ?? ""].join("\u0000"),
    data: { rrun: r.runId },
  };
}

function allItems(): TimelineItem[] {
  return active.map((r) => { return runItem(r, true); }).concat(runs.map((r) => { return runItem(r, false); }));
}

function findRun(id: number): ApiRemoteRunRow | null {
  return active.find((r) => { return r.runId === id; }) || runs.find((r) => { return r.runId === id; }) || null;
}

/** The runs a row stands for, newest first: its own and, for ×N, every identical one under it. */
function runOf(id: number): ApiRemoteRunRow[] {
  for (const run of collapseRuns(allItems())) {
    if (run.some((it) => { return it.id === String(id); })) {
      return run.map((it) => { return findRun(Number(it.id)); }).filter((r): r is ApiRemoteRunRow => !!r);
    }
  }
  return [];
}

/** The Command block (2026-09-28): the row's line is cut with an ellipsis, so the open row
 *  states the command whole, with a Copy. */
function commandNode(r: ApiRemoteRunRow): HTMLElement {
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
export function actorLabel(actor: string | null | undefined): string {
  if (!actor) return "";
  const cut = actor.indexOf(":");
  return cut > 0 ? actor.slice(0, cut) : actor;
}

/** An open row: what its columns left out (the id, the target, who, the directory, the state),
 *  the whole command, then the output as the value block Logs and Traffic open to. */
function bodyNode(run: ApiRemoteRunRow[]): HChild {
  const r = run[0];
  const isLive = isLiveRun(r.runId);
  const meta = timelineMeta([
    "#" + r.runId, runTarget(r) || null, actorLabel(r.actor) || null,
    r.input && r.input.cwd ? h("code", null, r.input.cwd) : null,
    stateWord(r),
    run.length > 1 ? tr("remoteRuns.nIdenticalRuns", { n: run.length }) : null,
  ]);
  if (isLive) {
    const l = live[r.runId];
    // Hoisted so the comparison literals stay out of the h() children (i18n gate).
    const cancellable = r.state === "running" || r.state === "queued";
    // The capped note prefixes the text here too, matching pullLive's surgical update.
    const liveText = l && l.text
      ? (l.capped ? tr("remoteRuns.liveCappedTail") + "\n" : "") + l.text
      : null;
    return [meta, commandNode(r), valueBlock(
      {
        label: tr("remoteRuns.liveOutput"),
        data: { rlive: r.runId },
        // A cancel in flight IS the button: disabled, and saying so. The request behind it
        // does not return until the run has actually stopped.
        tools: cancellable
          ? [canceling[r.runId]
            ? btn(tr("remoteRuns.canceling"), { data: { rcancel: r.runId }, disabled: true })
            : btn(tr("remoteRuns.cancel"), { data: { rcancel: r.runId } })]
          : [],
      },
      liveText != null
        ? textNode(liveText, true, "logs").node
        : note(r.state === "queued" ? tr("remoteRuns.queuedWaitingFreeSlot") : tr("remoteRuns.output")))];
  }
  const out: HChild[] = [meta, commandNode(r)];
  if (r.error) out.push(valueBlock({ label: tr("remoteRuns.error") }, ...readableBody(r.error, true)));
  const b = bodies[r.runId];
  if (!b) return out.concat(note(tr("remoteRuns.loading"), { busy: true }));
  // The body union's two states: the gone marker (the record rolled past the run) or the
  // output read so far - "gone" in b is the discriminant.
  if ("gone" in b) return out.concat(note(tr("remoteRuns.runRolledRecord")));
  if (r.outputEvicted) {
    // The size budget took the file (docs/41 A2); the line - and its tail, below, when
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

/** What a remote.write wrote (docs/34 R12): the body is kept sealed beside the record and
 *  unsealed only on Show - an opened row in a screenshot shares no file until asked. */
function writtenNode(r: ApiRemoteRunRow): HChild {
  const bytes = r.input && r.input.contentBytes || 0;
  const size = fmtBytes(bytes);
  if (r.contentEvicted) return note(tr("remoteRuns.contentEvicted", { size }));
  if (!r.contentStored) {
    // Between e5bc766 and R12 a write kept its size and nothing else. Such a row drew no
    // block at all, which reads exactly like one whose Show has not been pressed yet.
    return kindOf(r) === "write" && bytes > 0 ? note(tr("remoteRuns.contentNotKept", { size })) : null;
  }
  const label = tr("remoteRuns.contentWritten");
  const c = contents[r.runId];
  if (!c) {
    return valueBlock({ label, notes: [size], tools: [btn(tr("remoteRuns.showContent"), { data: { rcontent: r.runId } })] });
  }
  if ("error" in c) return valueBlock({ label, notes: [size] }, note(c.error));
  const notes = c.truncated ? [size, tr("remoteRuns.contentHead", { cap: fmtBytes(256 * 1024) })] : [size];
  return valueBlock({ label, notes, tools: [btn(tr("remoteRuns.hideContent"), { data: { rcontenthide: r.runId } })] },
    textNode(c.text, true, "logs").node);
}

async function loadContent(id: number): Promise<void> {
  const j = await apiJson<{ content: string; truncated: boolean }>("/api/remote/runs/" + id + "/content");
  contents[id] = j ? { text: j.content, truncated: !!j.truncated } : { error: tr("remoteRuns.contentUnavailable") };
  repaintBody(id);
}

/** The drawn row a run id heads, or null. */
function itemEl(id: number): HTMLElement | null {
  const root = document.querySelector<HTMLElement>("#pane .tl");
  if (!root) return null;
  return Array.prototype.find.call(root.querySelectorAll<HTMLElement>(".tl-item"),
    (n: HTMLElement) => n.dataset.tlId === String(id)) as HTMLElement | undefined || null;
}

function repaintBody(id: number): void {
  const body = itemEl(id)?.querySelector<HTMLElement>(":scope > .tl-body");
  const run = runOf(id);
  if (body && run.length) fill(body, bodyNode(run));
}

/** One recorded run's output, from the cursor the previous read ended on (128 KB a read). */
async function loadBody(id: number, more: boolean): Promise<void> {
  const b = bodies[id];
  if (b && !more) return;
  const after = b && !("gone" in b) ? b.next : 0;
  const j = await apiJson<ApiRemoteRunOutput>("/api/remote/runs/" + id + "/output?after=" + after + "&max=131072");
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
async function pullLive(id: number): Promise<void> {
  const l = live[id] || (live[id] = { text: "", cursor: 0 });
  const had = !!l.text;
  const j = await apiJson<ApiRunOutputChunk>("/api/runs/" + id + "/output?after=" + l.cursor + "&max=131072");
  if (!j) return;
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
  if (j.terminal) void refresh(); // the run moved into the record: repaint from it
}

function armLive(): void {
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
function toggle(id: number): void {
  const run = runOf(id);
  const wasOpen = run.some((r) => { return !!open[r.runId]; });
  run.forEach((r) => { open[r.runId] = false; });
  open[id] = !wasOpen;
  const root = document.querySelector<HTMLElement>("#pane .tl");
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
let canceling = {} as Record<number, boolean>;

async function cancelRun(id: number): Promise<void> {
  if (canceling[id]) return; // a second click would be a second wait for the same stop
  canceling[id] = true;
  repaintBody(id); // the button says what it is doing, before the request that takes the time
  let j: unknown = null;
  try {
    j = await apiJson("/api/runs/" + id + "/cancel", { method: "POST" });
  } finally {
    delete canceling[id];
  }
  if (!j) { repaintBody(id); return; } // apiJson toasted; the button comes back
  toast(tr("remoteRuns.cancelRequestedRunId", { id }));
  await refresh();
  // The answer means the run has STOPPED, so this row is a recorded one now: its output has
  // to come from the record, or the open body sits on the loading spinner until the page is
  // reloaded by hand ("确实取消了，但是还得转圈，我刷新才有").
  adoptFinished();
}

/* Bodies being fetched for rows that finished while open — so the 6 s poll does not
   re-request the same output every time it runs. */
let adopting = {} as Record<number, boolean>;

/** A row that was opened while its run was live, and whose run has since finished: the live
 *  body it was showing is gone from `active`, and bodyNode's recorded branch has nothing to
 *  draw but the loading note. Nobody asks for the recorded output in that transition — toggle
 *  only asks when a row is OPENED — so the row span forever. This asks, once. */
function adoptFinished(): void {
  Object.keys(open).forEach((k: string): void => {
    const id = Number(k);
    if (!open[id] || isLiveRun(id) || bodies[id] || adopting[id]) return;
    if (!runOf(id).length) return; // not on this page any more; nothing to fill
    adopting[id] = true;
    void loadBody(id, false).finally((): void => { delete adopting[id]; });
  });
}

async function clearAll(): Promise<void> {
  if (!confirm(tr("remoteRuns.forgetEveryRecordedRemote"))) return;
  const j = await apiJson("/api/remote/runs", { method: "DELETE" });
  if (!j) return;
  bodies = {};
  contents = {};
  open = {};
  cursors = [null];
  page = 0;
  toast(tr("remoteRuns.runRecordCleared"));
  await refresh();
}

function render(): void {
  painted = signature();
  const kept = trn(usage.runs, "remoteRuns.nRunsRecordedBytes.one", "remoteRuns.nRunsRecordedBytes.other", { bytes: fmtBytes(usage.bytes) }) +
    (limits ? tr("remoteRuns.capKeptDDays", { cap: fmtBytes(limits.maxTotalBytes), d: Math.round(limits.maxAgeMs / 86400000) }) : "") +
    (limits && limits.auditWindowMs ? tr("remoteRuns.traceableWDays", { w: Math.round(limits.auditWindowMs / 86400000) }) : "");
  const options = [h("option", { value: "" }, tr("remoteRuns.allTargets"))].concat(targetIds.map((id) => {
    return h("option", { value: id, selected: id === target }, id);
  }));
  // One list is the whole page, so the head carries it: one sentence, what the record keeps,
  // and the list's filter and ⋯ (Clear - a destructive verb gets no standing button; the Jobs
  // and Tunnels head ⋯ hold page-wide verbs too). No section caption: "Runs" would name the
  // page the context bar already names, and a captionless section head was a row holding only
  // the tools, over the note (docs/46 P6-2 walk).
  fill($("pane"),
    paneBody({ wide: true },
      paneHead({
        desc: tr("remoteRuns.descOneLine"),
        sub: kept,
        actions: [
          h("select", { id: "rrTarget", aria: { label: tr("remoteRuns.filterTarget") } }, options),
          moreBtn(tr("remoteRuns.moreActions"), { id: "rrMore" }),
        ],
      }),
      h("div", { id: "rrList" })));
  paintList();
  $("countChip").textContent = usage.runs ? trn(usage.runs, "remoteRuns.nRuns.one", "remoteRuns.nRuns.other") : "";
}

function paintList(): void {
  const region = $("rrList");
  if (!region) return;
  if (!active.length && !runs.length) {
    fill(region,
      page || target
        ? note(page ? tr("remoteRuns.nothingPage") : tr("remoteRuns.runsTarget"))
        : emptyNode({ icon: "history", title: tr("remoteRuns.runs2"), hint: tr("remoteRuns.runSomethingTargetSwiss") }),
      page ? pagerNode() : null);
    return;
  }
  const items = allItems();
  const openIds = new Set<string>();
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
function pagerNode(): HChild {
  if (!page && !nextBefore) return null;
  return pager({
    label: tr("remoteRuns.runPages"), status: tr("remoteRuns.pageN", { n: page + 1 }),
    prev: btn(tr("remoteRuns.newer"), { id: "rrPrev", disabled: page <= 0 }),
    next: btn(tr("remoteRuns.older"), { id: "rrNext", disabled: !nextBefore }),
  });
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
  $("pane").onclick = (event: MouseEvent): void => {
    const t = targetEl(event);
    if (!t) return;
    const more = t.closest<HTMLElement>("[data-rmore]");
    if (more) { void loadBody(Number(more.dataset.rmore), true); return; }
    const cancel = t.closest<HTMLElement>("[data-rcancel]");
    if (cancel) { void cancelRun(Number(cancel.dataset.rcancel)); return; }
    const copy = t.closest<HTMLElement>("[data-rcopy]");
    if (copy) {
      const id = Number(copy.dataset.rcopy);
      const r = active.concat(runs).find((x) => { return x.runId === id; });
      if (r) void copyLogText(commandLine(r));
      return;
    }
    const show = t.closest<HTMLElement>("[data-rcontent]");
    if (show) { void loadContent(Number(show.dataset.rcontent)); return; }
    const hide = t.closest<HTMLElement>("[data-rcontenthide]");
    if (hide) { delete contents[Number(hide.dataset.rcontenthide)]; repaintBody(Number(hide.dataset.rcontenthide)); return; }
    if (t.closest("#rrPrev")) { void step(-1); return; }
    if (t.closest("#rrNext")) { void step(1); return; }
    const menu = t.closest<HTMLElement>("#rrMore");
    if (menu) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      if (menuOpen()) { closeMenu(); return; }
      popupMenu(menu.getBoundingClientRect(), [{
        label: tr("remoteRuns.clear"), danger: true, disabled: !usage.runs,
        fn: (): void => { void clearAll(); },
      }]);
      return;
    }
    // A row's summary is a real <button> (ui/timeline.ts): Enter and Space arrive as this click.
    // Only the summary toggles - a click inside the open body is someone reading it.
    const sum = t.closest<HTMLElement>(".tl-sum");
    const item = sum ? sum.closest<HTMLElement>("[data-rrun]") : null;
    if (item) toggle(Number(item.dataset.rrun));
  };
  // The target filter select - ONE delegated change listener instead of a per-render
  // assignment (docs/37 R5); the value is read at event time.
  $("pane").onchange = (event: Event): void => {
    const sel = targetEl(event)?.closest<HTMLSelectElement>("#rrTarget");
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
  // A run that ended between two polls - cancelled, or simply finished - takes its live
  // body with it; an open row then has nothing to show until its recorded output is read.
  adoptFinished();
}
export async function poll() { await refresh(); }
export function countText() { return usage.runs ? trn(usage.runs, "remoteRuns.nRuns.one", "remoteRuns.nRuns.other") : ""; }
export function unmount() {
  if (liveTimer) { clearInterval(liveTimer); liveTimer = null; }
  // A return visit starts on page 0 with no filter: the record moved on while the page
  // was away, so a stale cursor or filter would show a stale slice of it.
  open = {};
  bodies = {};
  contents = {}; // an unsealed body lives only while its page is on screen
  live = {};
  canceling = {};
  adopting = {};
  painted = "";
  page = 0;
  cursors = [null];
  target = "";
}

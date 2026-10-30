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

/* The log sheet owns its cursor, history and follow lifetime. */
import { api, targetEl, toast } from "../../util.js";
import { h } from "../../h.js";
import { tr, trn } from "../../i18n.js";
import { btn } from "../../ui/button.js";
import { dot, tag } from "../../ui/status.js";
import { note } from "../../ui/page.js";
import { row } from "../../ui/row.js";
import { closeSheet, sheet, showSheet } from "../../ui/sheet.js";
import { seg } from "../../ui/seg.js";
import { formCap } from "../../ui/form.js";
import { fmtMs, timeLabel } from "../../ui/timeline.js";
import { textNode, valueBlock } from "../../ui/json-view.js";
import type { ProcessRow, HistoryEntry, LogAnswer } from "./types.js";

let activeLogs: LogSession | null = null;

/* One run of the capture (SPEC §process.panel): the supervisor writes a
 * `--- <ts> <action> by <by> ---` header before every action, so the capture
 * is a SEQUENCE OF EVENTS, never one stream. Each event renders as its own
 * disclosure row - every start and stop stays a visible boundary however
 * long the output between them. The newest run is open; older runs fold to
 * their header. A lone headless group (the app-log tab, or a capture whose
 * head the 500-line tail cut away) stays one plain block. */
interface LogEvent { head: string[] | null; lines: string[] }

const EVENT_HEADER = /^--- (\S+) (\S+) by (\S+) ---$/;

function splitEvents(lines: string[]): LogEvent[] {
  const out: LogEvent[] = [];
  for (const line of lines) {
    const m = EVENT_HEADER.exec(line);
    if (m) out.push({ head: [m[1], m[2], m[3]], lines: [] });
    else {
      if (!out.length) out.push({ head: null, lines: [] });
      out[out.length - 1].lines.push(line);
    }
  }
  return out;
}

/* The lines the append path patches into the newest pre: the capture's newest
 * event, or the app tab's one plain block - the whole buffer. */
function tailLines(source: string, lines: string[]): string[] {
  if (source !== "capture") return lines;
  const events = splitEvents(lines);
  return events.length ? events[events.length - 1].lines : [];
}

function eventNode(e: LogEvent, isLast: boolean): HTMLElement {
  const n = e.lines.length;
  const name = e.head ? tag(e.head[1], { mono: true, title: e.head[0] }) : tag(tr("process.logs.earlier"));
  const when = e.head ? timeLabel(Date.parse(e.head[0]) || Date.now()) : "";
  // The "when \u00b7 by" rides in the COLUMNS, not the sub: an open
  // disclosure hides its sub (the open detail replaces it), and the newest
  // run is forced open - its timestamp would never show.
  const whenBy = e.head ? (when + " \u00b7 " + e.head[2]) : "";
  const pre = textNode(n ? e.lines.join("\n") : tr("process.logs.none"), true, "logs").node;
  if (isLast) pre.id = "prLogPre"; // follow addresses the newest event only
  const r = row({
    name,
    cols: [
      { v: whenBy, w: "s" as const },
      { v: h("span", { data: { prlogcount: "" } }, trn(n, "process.logs.lineOne", "process.logs.lineMany", { n })), w: "s" as const },
    ],
    detail: pre,
  });
  if (isLast) {
    const det = r.querySelector("details");
    if (det) (det as HTMLDetailsElement).open = true;
  }
  return r;
}

/* History as RUNS, not lines (SPEC §process.panel): a start opens a run, its
 * stop and exit close it, and the row says the whole story in one line - who
 * started it, how long it ran, how it ended. The raw sentences (an exit's
 * multi-line stderr included) fold into the row's detail; the reader who
 * wants them opens them, the reader who wants the shape does not scroll past
 * a wall. Entries that pair with nothing (an "already stopped", a head the
 * 50-entry window cut) ride in the run they followed, or a headless first
 * row. */
interface HistRun { start: HistoryEntry | null; stop?: HistoryEntry; exit?: HistoryEntry; extra: HistoryEntry[] }

function histRuns(entries: HistoryEntry[]): HistRun[] {
  const runs: HistRun[] = [];
  // A FAILED start's own exit line can land just BEFORE its start line (the
  // pump watcher races finish()): hold one unplaced exit so the failed run
  // can adopt it forward instead of leaving it stranded in a stranger's run.
  let pendingExit: HistoryEntry | null = null;
  const latestRun = (): HistRun | null => {
    if (!runs.length) return null;
    const r = runs[runs.length - 1];
    return r.start ? r : null;
  };
  const extra = (e: HistoryEntry): void => {
    if (!runs.length) runs.push({ start: null, extra: [] });
    runs[runs.length - 1].extra.push(e);
  };
  for (const e of entries) {
    if (e.action === "start") {
      const adopt = !e.ok && pendingExit ? pendingExit : null;
      if (pendingExit && !adopt) extra(pendingExit);
      pendingExit = null;
      runs.push({ start: e, exit: adopt || undefined, extra: [] });
    } else if (e.action === "stop") {
      // A stop pairs with the run it closed EVEN when the exit line already
      // landed (a stop-command app dies before finish() writes its line).
      const latest = latestRun();
      if (latest && !latest.stop) latest.stop = e; else extra(e);
    } else if (e.action === "exit") {
      const latest = latestRun();
      if (latest && !latest.exit) latest.exit = e;
      else {
        if (pendingExit) extra(pendingExit);
        pendingExit = e;
      }
    } else {
      if (pendingExit) { extra(pendingExit); pendingExit = null; }
      extra(e);
    }
  }
  if (pendingExit) extra(pendingExit);
  return runs;
}

function runRow(run: HistRun): HTMLElement {
  const parts = run.start ? [run.start, run.stop, run.exit, ...run.extra] : run.extra;
  const raw = parts.filter((e): e is HistoryEntry => { return !!e; }).map((e) => {
    return e.action + " \u00b7 " + timeLabel(e.ts) + " \u00b7 " + e.by + " \u2014 " + e.message;
  }).join("\n");
  const failed = !!run.start && !run.start.ok;
  const bad = failed || (run.exit ? !run.exit.ok : false);
  let outcome = "";
  if (failed) {
    // The failure sentence leads; an exit line that raced ahead of the start
    // line adds its code after it (the process DID die, and that is why the
    // start never settled).
    const code = run.exit && run.exit.exitCode != null ? tr("process.hist.exitCode", { code: String(run.exit.exitCode) }) : "";
    const why = run.start ? run.start.message : "";
    outcome = tr("process.hist.startFailed", { message: why }) + (code ? " \u00b7 " + code : "");
  } else if (run.exit) {
    const code = run.exit.exitCode != null ? tr("process.hist.exitCode", { code: String(run.exit.exitCode) }) : run.exit.message;
    outcome = (run.stop ? tr("process.hist.stoppedBy", { by: run.stop.by }) + " \u00b7 " : "") + code;
  } else if (run.stop) {
    outcome = tr("process.hist.stoppedBy", { by: run.stop.by });
  } else if (run.start) {
    outcome = tr("process.hist.stillRunning");
  }
  const dur = run.start && run.exit ? fmtMs(Math.max(0, run.exit.ts - run.start.ts)) : null;
  const r = row({
    lead: dot(run.start ? (bad ? "down" : "up") : "idle", raw),
    name: run.start ? (tr("process.hist.run") + " \u00b7 " + timeLabel(run.start.ts) + " \u00b7 " + run.start.by) : tr("process.logs.earlier"),
    sub: outcome || undefined,
    cols: dur ? [{ v: dur, mono: true, w: "s" as const, title: raw }] : [],
    detail: textNode(raw, true, "logs").node,
  });
  return r;
}

/* A session owns both its data and its DOM. Source reads serialize behind one
 * flight; a newer source generation makes the old answer inert. Sheet identity
 * is stronger than sheetOpen(): a different modal may occupy the same host. */
class LogSession {
  private readonly root: HTMLElement;
  private readonly sources: HTMLElement;
  private readonly output: HTMLElement;
  private readonly pathNode: HTMLElement;
  private readonly history: HTMLElement;
  private readonly followButton: HTMLButtonElement;
  private readonly name: string;
  private source = "capture";
  private generation = 0;
  private disposed = false;
  private after: number | null = null;
  private lines: string[] = [];
  private path = "";
  private truncated = false;
  private state: "loading" | "ready" | "missing" | "error" = "loading";
  private logError = "";
  private logBusy = false;
  private tailPending = false;
  private historySnapshot = "";
  private historyLoaded = false;
  private historyError = "";
  private historyBusy = false;
  private historyPending = false;
  private following = false;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(r: ProcessRow) {
    this.name = r.name;
    this.sources = seg(
      [{ id: "capture", label: tr("process.logs.capture") }, { id: "app", label: tr("process.logs.app"), hidden: !r.hasAppLog }],
      this.source,
      { key: "prlogsrc", label: tr("process.logs.sourceLabel") },
    );
    this.pathNode = note("", { id: "prLogPath" });
    this.pathNode.hidden = true;
    this.output = h("div", { id: "prLogBlock" }, note(tr("process.logs.loading"), { busy: true }));
    this.history = h("div", { id: "prHistBlock" }, note(tr("process.logs.loading"), { busy: true }));
    this.root = h("div", { id: "prLogSheet" }, this.sources, this.pathNode, this.output,
      formCap(tr("process.logs.historyCap")), this.history);
    this.followButton = btn(tr("process.logs.follow"), { id: "prFollow" });
    this.followButton.setAttribute("aria-pressed", "false");
  }

  async open(): Promise<void> {
    const close = btn(tr("process.action.close"), { kind: "ghost", id: "prLogClose" });
    showSheet(sheet({
      title: tr("process.logs.title", { name: this.name }),
      sub: this.name,
      body: this.root,
      foot: [this.followButton, h("span", { class: "grow" }), close],
    }));
    this.root.onclick = (e): void => {
      const b = targetEl(e)?.closest<HTMLElement>("[data-prlogsrc]");
      if (b && this.sources.contains(b)) void this.selectSource(b.dataset.prlogsrc || "capture");
    };
    this.followButton.onclick = (): void => { this.toggleFollow(); };
    close.onclick = (): void => { if (this.current()) this.dispose(true); };
    await Promise.all([this.pull(true), this.refreshHistory()]);
  }

  private current(): boolean {
    if (this.disposed || activeLogs !== this) return false;
    const host = document.getElementById("sheet");
    if (!host || host.hidden || !host.contains(this.root)) {
      this.dispose(false);
      return false;
    }
    return true;
  }

  private accepts(generation: number): boolean { return this.current() && this.generation === generation; }

  pause(): void {
    this.following = false;
    if (this.timer != null) { clearInterval(this.timer); this.timer = null; }
    this.followButton.textContent = tr("process.logs.follow");
    this.followButton.setAttribute("aria-pressed", "false");
  }

  dispose(closeOwnSheet: boolean): void {
    this.pause();
    this.disposed = true;
    this.generation++;
    this.tailPending = this.historyPending = false;
    this.root.onclick = this.followButton.onclick = null;
    if (activeLogs === this) activeLogs = null;
    const host = document.getElementById("sheet");
    if (closeOwnSheet && host && !host.hidden && host.contains(this.root)) closeSheet();
  }

  private toggleFollow(): void {
    if (!this.current()) return;
    if (this.following) { this.pause(); return; }
    this.following = true;
    this.followButton.textContent = tr("process.logs.following");
    this.followButton.setAttribute("aria-pressed", "true");
    this.timer = setInterval(() => {
      if (this.current()) void this.pull(false);
    }, 1000);
  }

  private async selectSource(source: string): Promise<void> {
    if (!this.current() || (source !== "capture" && source !== "app")) return;
    this.generation++;
    if (source !== this.source) {
      this.source = source;
      this.after = null;
      this.lines = [];
      this.path = this.logError = "";
      this.truncated = false;
      this.state = "loading";
      this.paintLogs();
    }
    this.sources.querySelectorAll<HTMLElement>("[data-prlogsrc]").forEach((b) => {
      b.setAttribute("aria-selected", String(b.dataset.prlogsrc === this.source));
    });
    // Clicking the active source refreshes its tail too, never concatenating it.
    await Promise.all([this.pull(true), this.refreshHistory()]);
  }

  private async request<T>(url: string, generation: number, missingLog = false): Promise<{ body?: T; missing?: boolean; error?: string } | null> {
    try {
      const r = await api(url);
      const body: unknown = await r.json().catch((): unknown => { return {}; });
      if (!this.accepts(generation)) return null;
      // A never-started capture is a state, not a repeated polling failure.
      if (missingLog && r.status === 404) return { missing: true };
      if (!r.ok) {
        const msg = typeof body === "object" && body !== null && "error" in body && typeof body.error === "string" ? body.error : "";
        return { error: msg || tr("util.httpN", { n: r.status }) };
      }
      return { body: body as T };
    } catch (e) {
      return this.accepts(generation) ? { error: tr("util.requestFailedGatewayRunning") } : null;
    }
  }

  private async pull(tail: boolean): Promise<void> {
    if (!this.current()) return;
    if (this.logBusy) {
      if (tail) this.tailPending = true;
      return;
    }
    this.logBusy = true;
    const generation = this.generation;
    const base = "/api/process/apps/" + encodeURIComponent(this.name) + "/log?source=" + this.source;
    const url = tail || this.after == null ? base + "&tail=500" : base + "&after=" + this.after;
    try {
      const result = await this.request<LogAnswer>(url, generation, true);
      if (!result || !this.accepts(generation)) return;
      if (result.missing) {
        if (this.state !== "missing") {
          this.lines = [];
          this.path = this.logError = "";
          this.after = null;
          this.truncated = false;
          this.state = "missing";
          this.paintLogs();
        }
        return;
      }
      if (result.error) {
        const changed = this.logError !== result.error || this.state !== "error";
        if (this.logError !== result.error) toast(result.error, true);
        this.logError = result.error;
        this.state = "error";
        if (changed) this.paintLogs();
        return;
      }
      if (result.body) {
        const j = result.body;
        const newOutput = !!j.reset || !!j.lines.length || this.after !== j.size;
        this.apply(j, tail);
        if (!tail && newOutput) void this.refreshHistory();
      }
    } finally {
      this.logBusy = false;
      if (this.tailPending && this.current()) {
        this.tailPending = false;
        void this.pull(true);
      }
    }
  }

  private apply(j: LogAnswer, tail: boolean): void {
    const replace = tail || j.reset;
    const joined = replace ? j.lines : this.lines.concat(j.lines);
    const trimmed = joined.length > 500;
    const path = j.path || (replace ? "" : this.path);
    const truncated = !!j.truncated || trimmed || (!replace && this.truncated);
    // A new event header repaints the CAPTURE only: an app-authored line that
    // happens to be header-shaped is data, never an event boundary - the app
    // tab stays one plain block (SPEC §process.panel).
    const repaint = replace || trimmed || this.state !== "ready" || path !== this.path || truncated !== this.truncated ||
      (this.source === "capture" && j.lines.some((line) => { return EVENT_HEADER.test(line); }));
    this.lines = joined.slice(-500);
    this.path = path;
    this.after = j.size;
    this.truncated = truncated;
    this.logError = "";
    this.state = "ready";
    if (repaint) { this.paintLogs(); return; }
    const pre = this.output.querySelector<HTMLElement>("#prLogPre");
    if (!pre) { this.paintLogs(); return; }
    // Derive text from the bounded buffer, never from previously rendered DOM.
    // Ordinary appends retain the newest pre; trims and resets rebuild output only.
    const lines = tailLines(this.source, this.lines);
    const box = this.root.closest<HTMLElement>(".sheet-body");
    const pinned = !!box && box.scrollHeight - box.scrollTop - box.clientHeight < 40;
    const text = lines.length ? lines.join("\n") : tr("process.logs.none");
    if (pre.textContent !== text) pre.textContent = text;
    const count = pre.closest(".lrow")?.querySelector<HTMLElement>("[data-prlogcount]");
    if (count) count.textContent = trn(lines.length, "process.logs.lineOne", "process.logs.lineMany", { n: lines.length });
    if (box && pinned) box.scrollTop = box.scrollHeight;
  }

  private paintLogs(): void {
    const box = this.root.closest<HTMLElement>(".sheet-body");
    const scrollTop = box?.scrollTop || 0;
    const pinned = !!box && box.scrollHeight - scrollTop - box.clientHeight < 40;
    this.pathNode.textContent = this.path;
    this.pathNode.hidden = !this.path;
    let content: HTMLElement;
    if (this.state === "loading") {
      content = note(tr("process.logs.loading"), { busy: true });
    } else if (this.state === "missing") {
      content = valueBlock({ label: tr("process.logs.cap"), notes: [tr("process.logs.gone")] }, note(tr("process.logs.none")));
    } else {
      // Only the capture is a SEQUENCE OF EVENTS: the split never runs on
      // the app tab, so an app line shaped like a header stays plain text
      // instead of birthing a bogus disclosure.
      const events = this.source === "capture" ? splitEvents(this.lines) : null;
      let body: HTMLElement;
      if (events && (events.length > 1 || (events.length === 1 && events[0].head))) {
        body = h("div", null, ...events.map((e, i) => { return eventNode(e, i === events.length - 1); }));
      } else {
        body = textNode(this.lines.length ? this.lines.join("\n") : tr("process.logs.none"), true, "logs").node;
        body.id = "prLogPre";
      }
      const notes = [this.truncated ? tr("process.logs.truncated") : null, this.path].filter(Boolean) as string[];
      content = valueBlock({ label: tr("process.logs.cap"), notes },
        this.logError ? h("div", null, note(this.logError, { err: true }), body) : body);
    }
    this.output.replaceChildren(content);
    if (box) box.scrollTop = pinned ? box.scrollHeight : scrollTop;
  }

  private async refreshHistory(): Promise<void> {
    if (!this.current()) return;
    if (this.historyBusy) { this.historyPending = true; return; }
    this.historyBusy = true;
    const generation = this.generation;
    try {
      const result = await this.request<{ entries?: HistoryEntry[] }>(
        "/api/process/history?app=" + encodeURIComponent(this.name) + "&limit=50", generation);
      if (!result || !this.accepts(generation)) return;
      if (result.error) {
        if (this.historyError !== result.error) toast(result.error, true);
        this.historyError = result.error;
        if (!this.historyLoaded) this.history.replaceChildren(note(result.error, { err: true }));
        return;
      }
      const entries = result.body?.entries || [];
      const snapshot = JSON.stringify(entries);
      const changed = !this.historyLoaded || !!this.historyError || snapshot !== this.historySnapshot;
      this.historySnapshot = snapshot;
      this.historyLoaded = true;
      this.historyError = "";
      // Unchanged history retains the reader's open disclosures and focus.
      if (changed) this.history.replaceChildren(...(entries.length ? histRuns(entries).map(runRow) : [note(tr("process.logs.historyNone"))]));
    } finally {
      this.historyBusy = false;
      if (this.historyPending && this.current()) {
        this.historyPending = false;
        void this.refreshHistory();
      }
    }
  }
}

export async function openLogs(r: ProcessRow): Promise<void> {
  disposeLogs();
  const session = new LogSession(r);
  activeLogs = session;
  await session.open();
}

/** Pause follow without invalidating a still-open sheet's manual source reads. */
export function stopFollow(): void { activeLogs?.pause(); }

/** Page leave or another process sheet invalidates even the initial requests. */
export function disposeLogs(): void { activeLogs?.dispose(true); }

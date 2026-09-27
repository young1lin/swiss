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
   Jobs — the scheduled-command view.

   Same architecture as Tunnels: the data loader and the row template live in polling.js next to
   loadTunnels/ruleRowHtml; this module owns the pane render, the poll-safe patch, the create/edit
   sheets, the run-history sheet and the row actions. The rendering contract (main.js) applies in
   full: the 6s poll may only call patchJobs(), never rebuild the list — everything editable lives
   in the shared #sheet.

   Availability is probed once at boot: an older gateway serves no /api/jobs (404) and a disabled
   subsystem answers 503 with the row that turned it off — either way the tab hides itself,
   because a tab that can only toast an error every 6 seconds is noise, not a feature.

   v2 (docs/10 §5, docs/11 §7): rows read the v2 fields (title, labels, trigger summary), Run
   now submits {"async": true} and polls /api/runs/{id} — closing the page never cancels a job —
   and the Advanced sheet edits the definition through the plugin config row: schema-driven form
   (action input built from GET /api/actions, run.js's builder) plus a JSON editor, round-tripping
   losslessly so fields this form does not know survive the save.
   ================================================================================================ */
import type { ApiActionRow, ApiActionsResponse, ApiJobRow, ApiJobRunRecord } from "./types/api.js";
import type { GroupCfg, GroupSlice } from "./types/dom.js";
import type { JobConfigRow, JobDef, JobFormValues, JobRetryOn, JobSched } from "./types/state.js";
import { $, api, apiJson, errText, refocusIfIdle, targetEl, toast } from "./util.js";
import { assignMember, groupFieldNode, groupOf as makeGroupOf, lastGroup, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, saveOrder, slice } from "./groups.js";
import { jobGroupsList, loadJobs } from "./polling.js";
import { argFieldsNode, readRunArgs } from "./run.js";
import { fill, frag, h } from "./h.js";
import type { HChild } from "./h.js";
import { JOB_BACKOFFS, JOB_CAPTURES, JOB_FIRST_RUNS, JOB_MISFIRES, JOB_OVERLAPS, JOB_RETRY_ONS, JOB_TRIGGER_KINDS, defTemplate, envToLines, formToV2, historyMeta, legalOf, parseEnvLines, triggerSummary, v2ToForm } from "./jobs-v2.js";
import { loadCronstrue } from "./vendor/cronstrue/2.52.0/index.js";
import { currentView } from "./ui-state.js";
import { appendJobRuns, clearJobBusy, jobDragging, jobDraggingGroup, jobFolds, jobHistory, jobHistoryIsFor, jobIsBusy, jobRows, paintedJobsSig, setJobBusy, setJobDragging, setJobDraggingGroup, setJobPendingGroup, setPaintedJobsSig, startJobHistory, takeJobPendingGroup } from "./job-state.js";
import { locale, tk, tr } from "./i18n.js";
import { readableBody } from "./logs.js";
import {
  btn, checkField, closeSheet, collapseRuns, dot, field, formCap, hint, iconBtn, moreBtn, note, pair, paneBody, paneHead,
  popupMenu, relTime, row, section, seg, sheet, showSheet, tag, timeline, timelineMeta, timelineToggle, valueBlock,
} from "./ui/index.js";
import type { RowCol, TimelineItem } from "./ui/index.js";

let probed: boolean | null = null; // null = not probed yet; then the cached boolean answer for this page load

/** One boot probe. Raw api(), never apiJson: a 404/503 here is a feature signal, not an error
 *  worth a toast. A network failure assumes present — the first real load reports properly. */
async function probeJobs(): Promise<boolean | null> {
  if (probed !== null) return probed;
  try { probed = (await api("/api/jobs")).ok; }
  catch (e) { probed = true; }
  if (!probed) {
    const b = document.querySelector('#railNav [data-view="jobs"]') as HTMLElement | null;
    if (b) b.hidden = true; // base.css [hidden]: the tab leaves the seg, and the hash gate in
  }                          // main.js waits for this probe before entering the view
  return probed;
}

/* --- render ----------------------------------------------------------------------------------- */

/** The rendering group of a job row — same one rule as every scope: the stored group while it
 *  still exists, else the first group. */
function jobGroupOfRow(j: ApiJobRow): string {
  return makeGroupOf(jobGroupsList())(j);
}

/** Persist the on-screen order: the family's order route for the jobs scope, ids in a row. */
function saveJobOrder(): void {
  void saveOrder("jobs", jobRows().map((j: ApiJobRow): string => { return j.name; }));
}

/** Move one row to just before/after another, re-render, persist. */
function moveJobRow(id: string, target: string, before: boolean): void {
  if (!id || !target || id === target) return;
  const rows = jobRows();
  const item = rows.find((r: ApiJobRow): boolean => { return r.name === id; });
  if (!item) return;
  const to = rows.findIndex((r: ApiJobRow): boolean => { return r.name === target; });
  if (to < 0) return; // target vanished mid-drag — leave everything where it is
  rows.splice(rows.indexOf(item), 1);
  rows.splice(before ? to : to + 1, 0, item);
  renderJobs();
  saveJobOrder();
}

/** Put one job in a group. Applied locally first so the row jumps immediately, then persisted
 *  — a reject takes the server's word for it. */
async function assignJobGroup(id: string, group: string | null): Promise<void> {
  const row = jobRows().find((r: ApiJobRow): boolean => { return r.name === id; });
  if (!row) return;
  const names = jobGroupsList();
  if (jobGroupOfRow(row) === (group || names[0])) return;
  row.group = group;
  renderJobs();
  const j = await assignMember("jobs", id, group);
  if (!j) { await loadJobs(); return; }
  row.group = j.group; // the canonical name the server stored
  renderJobs();
}

/* --- the row (docs/46 §3.6) -------------------------------------------------------------------- */

/** The schedule in words, from the sheet's own translator: schedFromJob into the builder state,
 *  schedToBody's sentence out (docs/46 §3.6 - no second translator). A v2 trigger is read in the
 *  v1 spelling the builder speaks. What it cannot say - a manual job, a sub-second interval, a
 *  cron cronstrue refuses or has not loaded yet - is the raw spelling, which is also the
 *  column's title either way. */
function scheduleWords(j: ApiJobRow): string {
  const t = j.trigger;
  const everySec = t ? (t.kind === "interval" && t.everyMs != null && t.everyMs % 1000 === 0 ? t.everyMs / 1000 : null) : j.everySec;
  const cron = t ? (t.kind === "cron" ? t.expression || null : null) : j.cron;
  if (!everySec && !cron) return triggerSummary(j);
  try {
    return schedToBody(schedFromJob({ name: j.name, command: j.command, everySec, cron })).say.replace(/[.。]$/, "");
  } catch (e) {
    return triggerSummary(j);
  }
}

/** The whole moment for a column's title, date included. whenLabel is a list's own label - a
 *  clock time alone for anything not a day in the past - so tomorrow's 05:59 read as today's. */
function fullWhen(iso: string): string { return new Date(iso).toLocaleString(locale()); }

/** When it runs next, relative ("in 15 hr."), the moment itself on hover. A switched-off job has
 *  no next run: a dash, not a time it will not keep. */
function nextCol(j: ApiJobRow): RowCol {
  const due = j.enabled && j.nextDueAt ? j.nextDueAt : null;
  return { v: due ? relTime(Date.parse(due)) : "—", w: "s", title: due ? tr("jobs.nextAt", { when: fullWhen(due) }) : undefined };
}

/** How the last run went, relative. Only a failure is red, and it is a tag (docs/46 §3.6): a
 *  healthy "OK · 8 hr. ago" stays grey text. The time is the scheduler's lastRunAt, which only a
 *  scheduled run stamps (it is the schedule's anchor); a Run now settles lastOk alone. So an
 *  outcome with no time is "OK" or the tag by itself - "Never run" is for a job with neither. */
function lastCol(j: ApiJobRow): RowCol {
  if (j.lastOk == null && !j.lastRunAt) return { v: tr("jobs.neverRun"), w: "l" };
  const when = j.lastRunAt ? relTime(Date.parse(j.lastRunAt)) : null;
  return {
    v: j.lastOk === false
      ? [tag(tr("jobs.failedTag"), { tone: "bad" }), when ? " " + when : null]
      : when ? tr("jobs.okWhen", { when }) : tr("jobs.ok"),
    w: "l",
    title: j.lastRunAt ? tr("jobs.lastAt", { when: fullWhen(j.lastRunAt) }) : undefined,
  };
}

/** One job, on the library row (docs/46 §3.6): the name, then what it runs in mono, then three
 *  fixed columns that line up down the card - the schedule in words, the next run, the last
 *  run. There is no lead dot. A switched-off job is an Off tag after its name, not a greyed
 *  row, and a green "scheduled" dot would only repeat the next-run column. The amber pulse
 *  after the name appears only while the job runs. A v2 title is the name; the id every action
 *  addresses rides the sub-line in mono. Run now is the one word button, the rest is behind
 *  the ⋯. Every label, command and trigger sentence is a text node (docs/37 R5). */
function jobRowNode(j: ApiJobRow): HTMLElement {
  const busy = jobIsBusy(j.name);
  const titled = !!j.title && j.title !== j.name;
  return row({
    name: [
      titled ? j.title : j.name,
      busy || j.running ? [" ", dot("starting", tr("jobs.runningDot"))] : null,
      (j.labels || []).map((l: string): HChild => { return [" ", tag("#" + l)]; }),
      j.enabled ? null : [" ", tag(tr("jobs.offTag"))],
    ],
    sub: titled ? [h("code", null, j.name), " · ", h("code", null, j.command)] : h("code", null, j.command),
    cols: [{ v: scheduleWords(j), w: "m", title: triggerSummary(j) }, nextCol(j), lastCol(j)],
    primary: btn(busy ? "\u2026" : tr("jobs.runNow"), { data: { run: "" }, disabled: busy || !!j.running }),
    more: moreBtn(tr("jobs.actionsName", { name: j.name }), { data: { more: "" } }),
    data: { job: j.name },
    draggable: true,
  });
}

function renderJobs(): void {
  const rows = jobRows();
  // The structural signature the poll compares against: the group names, then every row as
  // name+group in order — any add/remove/reorder/regroup means a rebuild is due; dots and
  // labels alone never justify one.
  setPaintedJobsSig(jobGroupsList().join("\n") + "\u0000" +
    rows.map((j: ApiJobRow): string => { return j.name + "\u0001" + jobGroupOfRow(j); }).join("\n"));
  // No location title: the context bar already says "Jobs", and its chip is the count the foot
  // used to repeat (rule 25), so there is no foot. One sentence (U11). The page's actions sit in
  // the head like every other page's: New group is the folder-plus glyph (rule 7), New
  // (advanced) waits behind the ⋯, New is the one primary. The "Scheduled commands" caption
  // named the page a second time.
  fill($("pane"), paneBody({ wide: true },
    paneHead({
      desc: tr("jobs.descOneLine"),
      actions: [
        iconBtn("folder-plus", tr("jobs.newGroup"), { id: "jobNewGroup" }),
        moreBtn(tr("jobs.moreActions"), { id: "jobMore" }),
        btn(tr("jobs.new"), { kind: "primary", id: "jobNew" }),
      ],
    }),
    section({}, h("div", { id: "jobGroups" }))));
  // The schedule column speaks cronstrue, which loads on first use: until it lands a cron row
  // shows its raw spelling, and the landing patches the words in.
  if (!cronstrueLib) ensureCronstrue((): void => { if (currentView() === "jobs") patchJobs(); });
  // One group per slice — the component owns the header band and the empty line (docs/20
  // G4). The groups ALWAYS paint, jobs or none (the Remote Targets rule, 2026-09-20): an
  // empty group is a place - a drop target with a + - not an empty state. The page used to
  // swap in "No jobs" whenever it had no rows, so a group made before the first job was
  // listed in the sheet's Group select and nowhere else: no header to rename or delete it by.
  const host = $("jobGroups");
  slice(rows, jobGroupsList(), jobGroupOfRow).forEach((g: GroupSlice<ApiJobRow>): void => {
    host.appendChild(mountGroup(jobCfg(), g));
  });
  wireJobs();
}

/** The jobs scope's cfg for mountGroup (see groups.js for the full contract). Built fresh each
 *  render so names and the fold map are always the live objects. */
function jobCfg(): GroupCfg<ApiJobRow> {
  return {
    scope: "jobs",
    density: "page",
    names: jobGroupsList(),
    collapsed: jobFolds(),
    noun: tr("jobs.job"),
    addTitle: (g: string): string => { return tr("jobs.addJobGroup", { group: g }); },
    onAdd: (g: string): void => {
      setJobPendingGroup(g); // a real name now — the sheet's save lands the job in it
      openJobSheet(null);
    },
    reload: (): Promise<void> => { return loadJobs(); },
    render: renderJobs,
    afterDrag: (): void => { renderJobs(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: (): string | null => { return jobDragging(); },
      set: (v: string | null): void => { setJobDragging(v); },
    },
    dragGroup: {
      get: (): string | null => { return jobDraggingGroup(); },
      set: (v: string | null): void => { setJobDraggingGroup(v); },
    },
    rowId: (r: ApiJobRow): string => { return r.name; },
    rowsById: (): ApiJobRow[] => { return jobRows(); },
    groupOfRow: jobGroupOfRow,
    // The row as a node (docs/37 R5): the actions ride the pane's delegated click, so the
    // row carries no per-render handlers of its own.
    rowNode: jobRowNode,
    onMoveRow: moveJobRow,
    onAssign: (id: string, g: string | null): void => { void assignJobGroup(id, g); },
  };
}

/** Bring a drawn row up to a freshly built one in place: the text block and each column are
 *  swapped only when they read differently (a hover title under the pointer does not flicker
 *  every tick), and Run now stays the SAME node with its label and state moved, so a poll
 *  never takes the focus off the button a hand just pressed. */
function patchJobRow(node: Element, fresh: HTMLElement): void {
  const swap = (sel: string): void => {
    const now = fresh.querySelectorAll(sel);
    node.querySelectorAll(sel).forEach((was: Element, i: number): void => {
      const next = now[i];
      if (next && was.outerHTML !== next.outerHTML) was.replaceWith(next);
    });
  };
  swap(".lrow-main");
  swap(".lrow-col");
  const run = node.querySelector<HTMLButtonElement>("[data-run]");
  const next = fresh.querySelector<HTMLButtonElement>("[data-run]");
  if (run && next) { run.disabled = next.disabled; run.textContent = next.textContent; }
}

/** Poll-safe update: every row's words and columns, Run now, the group counts. Never
 *  structure - and never a rebuild mid-drag, which would cancel the gesture. */
function patchJobs(): void {
  if (currentView() !== "jobs") return;
  const pane = $("pane");
  const sig = jobGroupsList().join("\n") + "\u0000" +
    jobRows().map((j: ApiJobRow): string => { return j.name + "\u0001" + jobGroupOfRow(j); }).join("\n");
  if (!pane.querySelector("#jobNew") || sig !== paintedJobsSig()) {
    if (!jobDragging() && !jobDraggingGroup()) renderJobs();
    return;
  }
  Array.prototype.forEach.call(pane.querySelectorAll("[data-job]"), (node: Element): void => {
    const j = jobByName(node.getAttribute("data-job"));
    if (j) patchJobRow(node, jobRowNode(j));
  });
  // Group counts move with rows: a count that only refreshed on rebuild would disagree with
  // the patched dots beside it for the rest of the poll.
  Array.prototype.forEach.call(pane.querySelectorAll("[data-group]"), (grp: HTMLElement): void => {
    const n = jobRows().filter((j: ApiJobRow): boolean => { return jobGroupOfRow(j) === grp.dataset.group; }).length;
    const badge = grp.querySelector(".grp-n");
    if (badge) badge.textContent = String(n);
  });
}

function jobByName(name: string | null): ApiJobRow | null {
  let out: ApiJobRow | null = null;
  jobRows().forEach((j: ApiJobRow): void => { if (j.name === name) out = j; });
  return out;
}

function wireJobs(): void {
  // docs/37 R5: the head's buttons AND every row action answer through ONE delegated
  // click on #pane, climbed with closest() — a real pointer click lands on the button's
  // text or glyph and the glyph carries no id. The row is re-resolved from jobRows() at
  // click time, so a poll that landed between render and click cannot act on a stale row.
  $("pane").onclick = (ev: MouseEvent): void => {
    const t = targetEl(ev);
    if (!t) return;
    if (t.closest("#jobNew")) { openJobSheet(null); return; }
    const pageMore = t.closest<HTMLElement>("#jobMore");
    if (pageMore) {
      // The page's rarer way to create: the JSON-backed sheet for what the form cannot spell.
      ev.stopPropagation(); // the opening click must not reach document's menu closer
      popupMenu(pageMore.getBoundingClientRect(), [
        { label: tr("jobs.newAdvanced"), fn: (): void => { void openV2Sheet(null); } },
      ]);
      return;
    }
    if (t.closest("#jobNewGroup")) {
      newGroupFlow("jobs", jobGroupsList(), (): Promise<void> => { return loadJobs(); });
      return;
    }
    const rowEl = t.closest<HTMLElement>("[data-job]");
    if (!rowEl) return;
    const job = jobByName(rowEl.getAttribute("data-job"));
    if (!job) return;
    if (t.closest("[data-run]")) { void runJob(job.name); return; }
    const more = t.closest<HTMLElement>("[data-more]");
    if (more) {
      // The overflow half of the row (docs/18 V5): the rare verbs and the destructive one.
      // stopPropagation FIRST: connect.js closes any open menu on clicks that reach document,
      // and without this the very click that opens the menu also tears it down.
      ev.stopPropagation();
      popupMenu(more.getBoundingClientRect(), [
        { label: tr("jobs.edit"), fn: () => {
            // A definition the v1 shape cannot spell (docs/11 §7.1: editableInV1 false) goes
            // straight to the advanced sheet — the v1 form would silently drop its fields.
            if (job.editableInV1 === false) void openV2Sheet(job);
            else openJobSheet(job);
          } },
        { label: tr("jobs.history"), fn: (): void => { void openRunsSheet(job.name); } },
        { sep: true },
        { label: tr("jobs.delete"), danger: true, fn: (): void => { deleteJob(job); } },
      ]);
    }
  };
}

/* --- actions ---------------------------------------------------------------------------------- */

/** Run now submits {"async": true} (docs/11 §7.3) and polls the coordinator's run view: the
 * run outlives the page, and the row stays live through the poll. The toast still names the
 * outcome, because the record settles into the history either way. */
async function runJob(name: string): Promise<void> {
  const pressed = document.activeElement;
  setJobBusy(name);
  patchJobs();
  const j = await apiJson<{ runId?: number }>("/api/jobs/" + encodeURIComponent(name) + "/run", {
    method: "POST",
    body: JSON.stringify({ async: true }),
  });
  if (!j || j.runId == null) { clearJobBusy(name); patchJobs(); refocusIfIdle(pressed); return; }
  const runId = j.runId;
  // Bounded poll: queued -> running -> terminal. The job's own timeoutMs bounds the run; the
  // poll gives up after 30 minutes and lets the history sheet tell the rest of the story.
  for (let i = 0; i < 1800; i++) {
    await new Promise((res) => { setTimeout(res, 1000); });
    // The coordinator's run view is FLAT ({runId, state, ms, …}) — only the sync door wraps
    // its record in {run}. Reading v.run here left the button on "…" for the full 30-minute
    // poll after a 40 ms echo had long finished.
    const v = await apiJson<{ runId?: number; state?: string; ms?: number; error?: string }>("/api/runs/" + runId);
    if (!v || !v.state) continue;
    if (v.state === "queued" || v.state === "running") continue;
    clearJobBusy(name);
    const ok = v.state === "succeeded";
    toast(tr("jobs.nameState", { name, state: v.state }) + (v.ms != null ? tr("jobs.msMs", { ms: v.ms }) : "") +
      (v.error ? tr("jobs.error", { error: v.error }) : ""), !ok);
    await loadJobs(true); // the patch path: the pressed button stays the same node
    refocusIfIdle(pressed);
    return;
  }
  clearJobBusy(name);
  patchJobs();
  refocusIfIdle(pressed);
  toast(tr("jobs.nameStillRunningWatch", { name }), false);
}

function deleteJob(job: ApiJobRow): void {
  if (!confirm(tr("jobs.deleteJobNameJob", { name: job.name }))) return;
  void (async () => {
    const j = await apiJson<unknown>("/api/jobs/" + encodeURIComponent(job.name), { method: "DELETE" });
    if (!j) return;
    toast(tr("jobs.deletedName", { name: job.name }));
    await loadJobs();
  })();
}

/* --- the create/edit sheet (v1: the shape every field of this form can spell) ------------------- */

/* The schedule builder. Cron's grammar was the complaint: "every morning at 5:59" should not
   have to be composed as "59 5 * * *". The builder offers the four ways a person actually
   thinks about "when" (interval, daily, weekly, monthly) as plain controls, keeps raw cron as
   a fifth mode for expressions the presets cannot spell, and answers every state with one
   plain sentence (cronstrue, vendored under js/vendor/cronstrue, MIT) — the sentence IS the
   validation: what reads back wrong is wrong, and what cannot be said does not save. */

/** The wire takes one argv line: the textarea is typing comfort for a long command, and its
 *  breaks fold to a single space. The preview under the box shows exactly this join, so a line
 *  break never silently means a second command — one job supervises one process. */
function joinCommand(text: string): string { return text.trim().replace(/\s*\n+\s*/g, " "); }

/* tk() marks the table's English entries for the completeness scanner (docs/38 L7): the
   paint renders them through tr(label) at call time, so the key stays literal here. */
const DOW_LABELS = [tk("jobs.sun"), tk("jobs.mon"), tk("jobs.tue"), tk("jobs.wed"), tk("jobs.thu"), tk("jobs.fri"), tk("jobs.sat")];
/* The interval units stay PLAIN in the schedule state and in the select's option values —
 * schedFromJob, readFields and the everySec multiplier all speak this vocabulary — while
 * the display text paints through the dictionary at render time (docs/38 L5: keys never
 * travel as data). tk() marks the table's entries for the completeness scanner. */
const INTERVAL_UNITS: string[] = ["seconds", "minutes", "hours"];
const UNIT_LABELS: Record<string, string> = {
  seconds: tk("jobs.seconds"), minutes: tk("jobs.minutes"), hours: tk("jobs.hours"),
};
/* Singular forms for the "every 1 …" sentence: English singularises, Chinese does not
 * (the same word serves both numbers), so each unit has both entries in both tables. */
const UNIT_ONE: Record<string, string> = {
  seconds: tk("jobs.second"), minutes: tk("jobs.minute"), hours: tk("jobs.hour"),
};
/* The schedule modes are wire vocabulary in the state (data-mode, sched.mode); the seg
 * paints the display word through the dictionary, never the raw mode id. */
const SCHED_MODES: string[] = ["interval", "daily", "weekly", "monthly", "cron"];
const MODE_LABELS: Record<string, string> = {
  interval: tk("jobs.modeInterval"), daily: tk("jobs.modeDaily"), weekly: tk("jobs.modeWeekly"),
  monthly: tk("jobs.modeMonthly"), cron: tk("jobs.modeCron"),
};

function two(n: number): string { return (n < 10 ? "0" : "") + n; }

/** cronstrue, with the two checks of our own: exactly five fields (the API is a 5-field cron;
 *  cronstrue also speaks 6-7 field dialects, whose descriptions would lie about what saves),
 *  and a missing global means the vendor file was not served — say that, not a TypeError. */
let cronstrueLib: { toString: (expr: string, opts?: { throwExceptionOnParseError?: boolean; locale?: string }) => string } | null = null;

/** cronstrue, loaded once through the vendored shim (docs/14 §2: UMD bundles arrive as
 *  classic scripts, not imports). Sheets call ensureCronstrue with their own re-say as the
 *  ready callback; before the library lands, describeCron's complaint names it instead of
 *  throwing a TypeError about it. */
function ensureCronstrue(ready?: (lib: { toString: (expr: string, opts?: { throwExceptionOnParseError?: boolean; locale?: string }) => string }) => void): void {
  if (cronstrueLib) { if (ready) ready(cronstrueLib); return; }
  loadCronstrue().then((lib: { toString: (expr: string, opts?: { throwExceptionOnParseError?: boolean; locale?: string }) => string }): void => {
    cronstrueLib = lib;
    if (ready) ready(lib);
  }, () => { /* the say-line's own complaint covers the failure */ });
}

function describeCron(expr: string): string {
  if (!/^(\S+\s+){4}\S+$/.test(expr)) throw new Error(tr("jobs.giveAllFiveCron"));
  if (!cronstrueLib) throw new Error(tr("jobs.cronstrueStillLoadingOne"));
  /* The vendored bundle carries every cronstrue locale; the panel's schedule sentence
   *  follows the installed language (docs/38) - cronstrue names Chinese "zh_CN". */
  return cronstrueLib.toString(expr, {
    throwExceptionOnParseError: true,
    locale: locale() === "zh-CN" ? "zh_CN" : undefined,
  });
}

/** The builder state a job's trigger round-trips into. A cron the presets can spell comes back
 *  as its preset; anything else lands in the raw-cron mode rather than being flattened into a
 *  schedule the presets only almost express. */
function schedFromJob(v: { name: string; command: string; everySec?: string | number | null; cron?: string | null; timeoutMs?: string | number | null; cwd?: string | null; env?: Record<string, string>; enabled?: boolean }): JobSched {
  const every = v.everySec == null || v.everySec === "" ? null : Number(v.everySec);
  if (every) {
    // The largest unit that divides the interval evenly, so "7200" reads back as 2 hours.
    if (every % 3600 === 0) return { mode: "interval", every: every / 3600, unit: "hours" };
    if (every % 60 === 0) return { mode: "interval", every: every / 60, unit: "minutes" };
    return { mode: "interval", every: every, unit: "seconds" };
  }
  const cron = v.cron || ""; let m: RegExpMatchArray | null;
  if ((m = cron.match(/^(\d+) (\d+) \* \* \*$/))) {
    return { mode: "daily", time: two(+m[2]) + ":" + two(+m[1]) };
  }
  if ((m = cron.match(/^(\d+) (\d+) \* \* (.+)$/)) && m[4] !== "*") {
    const days: number[] = []; let ok = true;
    m[4].split(",").forEach((part: string): void => {
      const r = part.split("-");
      let a = +r[0], b = +r[r.length - 1];
      if (isNaN(a) || isNaN(b) || a < 0 || b > 7) { ok = false; return; }
      a = a % 7; b = b % 7; // cron's 7 is Sunday again
      for (let d = a; ; d = (d + 1) % 7) {
        if (!days.includes(d)) days.push(d);
        if (d === b) break;
      }
    });
    if (ok) {
      days.sort((a: number, b: number): number => { return a - b; });
      return { mode: "weekly", time: two(+m[2]) + ":" + two(+m[1]), days: days };
    }
  }
  if ((m = cron.match(/^(\d+) (\d+) (\d+) \* \*$/))) {
    return { mode: "monthly", time: two(+m[2]) + ":" + two(+m[1]), day: +m[3] };
  }
  if (cron) return { mode: "cron", cron: cron };
  return { mode: "daily", time: "08:00" }; // a new job's opening state: a benign default
}

/** Validate the builder state and produce what saves: {everySec}|{cron} plus the sentence the
 *  preview shows. Throws the field-level complaint; the sheet prints it, the save toasts it. */
function schedToBody(s: JobSched): { body: { everySec?: number; cron?: string }; say: string } {
  const unit_ = s.unit!;
  if (s.mode === "interval") {
    if (!(s.every! > 0)) throw new Error(tr("jobs.intervalNeedsNumberUnit", { unit: tr(UNIT_LABELS[unit_] ?? unit_) }));
    const mult = s.unit === "hours" ? 3600 : s.unit === "minutes" ? 60 : 1;
    // The sentence singularises for "every 1 hour" (English only); the dictionary key
    // for each form comes from the tables above, never from string surgery on a key.
    const unit = s.every === 1 ? (UNIT_ONE[unit_] ?? UNIT_LABELS[unit_] ?? unit_) : (UNIT_LABELS[unit_] ?? unit_);
    return { body: { everySec: s.every! * mult }, say: tr("jobs.everyNUnit", { n: s.every ?? 0, unit: tr(unit) }) };
  }
  const t = /^(\d{1,2}):(\d{2})$/.exec(s.time || "");
  if (!t) throw new Error(tr("jobs.timeMustHhMm"));
  const hh = +t[1], mm = +t[2];
  if (hh > 23 || mm > 59) throw new Error(tr("jobs.timeMustRealTime"));
  let expr;
  if (s.mode === "daily") expr = mm + " " + hh + " * * *";
  else if (s.mode === "weekly") {
    if (!s.days || !s.days.length) throw new Error(tr("jobs.weeklyNeedsLeastOne"));
    expr = mm + " " + hh + " * * " + s.days.join(",");
  } else if (s.mode === "monthly") {
    if (!(s.day! >= 1 && s.day! <= 31)) throw new Error(tr("jobs.dayMonthMustBetween"));
    expr = mm + " " + hh + " " + s.day + " * *";
  } else {
    expr = (s.cron || "").trim().replace(/\s+/g, " ");
    if (!expr) throw new Error(tr("jobs.cronExpressionEmpty"));
    return { body: { cron: expr }, say: describeCron(expr) + "." };
  }
  return { body: { cron: expr }, say: describeCron(expr) + "." };
}

/* The live builder state of whichever v1 sheet is open — saveJob reads it back; the sheet's
   own closures keep it current on every input. */
let schedState: JobSched | null = null;

function openJobSheet(job: ApiJobRow | null): void {
  const editing = !!job;
  const v = job || { name: "", command: "", everySec: "", cron: "", timeoutMs: "", cwd: "", env: {}, enabled: true };
  const sched = schedFromJob(v);
  schedState = sched;
  // The Group promise of a header "+", consumed here: the select is the truth from now on.
  let picked: string | null = null;
  if (!editing) {
    const staged = takeJobPendingGroup();
    picked = staged != null ? staged : resolveDefaultGroup(jobGroupsList(), lastGroup("jobs"));
  }
  // The library sheet and form pieces (docs/46 P6). showSheet unhides the host BEFORE it paints
  // (panel-proof-of-life rule 1) and claims the backdrop click. Each caption heads the fields it
  // names: Environment used to sit directly on top of Options, an empty section, with the env
  // variables filed under Options.
  showSheet(sheet({
    title: editing ? tr("jobs.editName", { name: job?.name || "" }) : tr("jobs.newJobGroup", { group: picked || "" }),
    label: editing ? tr("jobs.editJob") : tr("jobs.newJob"),
    body: [
      // The name IS the identity (the PUT path is the job), so it is fixed once created.
      field({ label: tr("jobs.name"), control: h("input", { id: "jf-name", value: v.name || "", disabled: editing, placeholder: tr("jobs.nightlyVacuum"), autocomplete: "off" }) }),
      editing ? null : groupFieldNode(jobGroupsList(), picked),
      field({
        label: tr("jobs.command"),
        control: h("textarea", { id: "jf-command", rows: 2, placeholder: tr("jobs.cmdCBackupBat"), spellcheck: false }, v.command),
        hint: tr("jobs.oneCommandJobSupervises"),
      }),
      h("div", { class: "sched-say cmd-say", id: "jf-cmd-say" }),
      formCap(tr("jobs.schedule")),
      h("div", { class: "sched", id: "jf-sched" },
        seg(SCHED_MODES.map((m: string) => { return { id: m, label: tr(MODE_LABELS[m] ?? m) }; }), sched.mode || "daily", { key: "mode", label: tr("jobs.schedule"), fill: true }),
        h("div", { id: "jf-sched-fields" }),
        h("div", { class: "sched-say", id: "jf-say" })),
      formCap(tr("jobs.options")),
      pair(
        field({
          label: tr("jobs.timeoutMinutes"),
          control: h("input", { id: "jf-timeout", type: "number", min: "0.5", step: "0.5",
            value: String(v.timeoutMs ? Math.max(0.5, Math.round(v.timeoutMs as number / 6000) / 10) : 10),
            autocomplete: "off" }),
        }),
        field({ label: tr("jobs.workingDirectory"), control: h("input", { id: "jf-cwd", value: v.cwd || "", placeholder: tr("jobs.optional"), autocomplete: "off" }) })),
      checkField({ label: tr("jobs.enabled"), control: h("input", { type: "checkbox", id: "jf-enabled", checked: !!v.enabled }) as HTMLInputElement }),
      formCap(tr("jobs.environment")),
      field({
        label: tr("jobs.environmentVariables"),
        // A property-assigned placeholder may carry a real newline; the entity dance an HTML
        // attribute needed does not apply to the property path.
        control: h("textarea", { id: "jf-env", rows: 3, placeholder: tr("jobs.deployEnvStagingLog"), spellcheck: false }, envToLines(v.env)),
        hint: tr("jobs.oneKeyValueLine"),
      }),
    ],
    foot: [
      btn(tr("jobs.advanced"), { id: "jf-advanced" }),
      btn(tr("jobs.cancel"), { id: "jf-cancel" }),
      btn(editing ? tr("jobs.save") : tr("jobs.add"), { kind: "primary", id: "jf-save" }),
    ],
  }));
  $("jf-cancel").onclick = closeSheet;
  $("jf-advanced").onclick = (): void => { void openV2Sheet(job); };
  $("jf-save").onclick = (): void => { void saveJob(job); };

  /* The builder's two-way wire: field edits flow into the state and the sentence re-says
     itself; a mode switch re-renders the mode's fields from the state it is switching to. */
  function readFields(): void {
    if (sched.mode === "interval") {
      sched.every = Number($<HTMLInputElement>("jf-ev").value) || 0;
      sched.unit = $<HTMLSelectElement>("jf-ev-u").value;
    } else if (sched.mode === "cron") {
      sched.cron = $<HTMLInputElement>("jf-cron-in").value;
    } else {
      sched.time = $<HTMLInputElement>("jf-at").value || "";
      if (sched.mode === "weekly") {
        sched.days = [];
        Array.prototype.forEach.call(document.querySelectorAll("#jf-days button.on"), (b: Element): void => {
          sched.days?.push(+b.getAttribute("data-dow")!);
        });
      } else if (sched.mode === "monthly") {
        sched.day = Number($<HTMLInputElement>("jf-md").value) || 0;
      }
    }
    say();
  }

  function say(): void {
    const el = $("jf-say");
    el.className = "sched-say";
    el.textContent = "";
    try { el.textContent = schedToBody(sched).say; }
    catch (err) { el.className = "sched-say bad"; el.textContent = errText(err); }
  }

  function renderFields(): void {
    let html: HChild;
    if (sched.mode === "interval") {
      html = pair(
        field({ label: tr("jobs.runEvery"), control: h("input", { id: "jf-ev", type: "number", min: "1", value: String(sched.every || ""), autocomplete: "off" }) }),
        field({
          label: tr("jobs.unit"),
          control: h("select", { id: "jf-ev-u" }, INTERVAL_UNITS.map((u: string) => {
            return h("option", { value: u, selected: sched.unit === u }, tr(UNIT_LABELS[u] ?? u));
          })),
        }));
    } else if (sched.mode === "cron") {
      html = field({
        label: tr("jobs.cronExpressionLocalTime"),
        control: h("input", { id: "jf-cron-in", value: sched.cron || "", placeholder: tr("jobs.n30N3"), autocomplete: "off" }),
        hint: tr("jobs.fiveFieldsMinuteHour"),
      });
    } else {
      const at = field({ label: tr("jobs.text"), control: h("input", { id: "jf-at", type: "time", value: sched.time || "08:00" }) });
      if (sched.mode === "daily") html = at;
      if (sched.mode === "weekly") {
        html = frag(
          // A group, not a <label>: the caption must not press the first weekday (ui/form.ts).
          field({
            group: true,
            label: tr("jobs.days"),
            control: h("div", { class: "days", id: "jf-days" }, DOW_LABELS.map((d: string, i: number) => {
              const on = sched.days && sched.days.includes(i);
              return h("button", { type: "button", data: { dow: i }, class: on ? "on" : "",
                aria: { pressed: on ? "true" : "false" } }, tr(d));
            })),
          }),
          at);
      }
      if (sched.mode === "monthly") {
        html = pair(
          field({ label: tr("jobs.day"), control: h("input", { id: "jf-md", type: "number", min: "1", max: "31", value: String(sched.day || 1), autocomplete: "off" }) }),
          at);
      }
    }
    fill($("jf-sched-fields"), html);
    Array.prototype.forEach.call($("jf-sched-fields").querySelectorAll("input, select"), (el2: HTMLElement): void => {
      el2.oninput = readFields;
      el2.onchange = readFields;
    });
    Array.prototype.forEach.call(document.querySelectorAll("#jf-days button"), (b: HTMLButtonElement): void => {
      b.onclick = (): void => {
        b.classList.toggle("on");
        b.setAttribute("aria-pressed", b.classList.contains("on") ? "true" : "false");
        readFields();
      };
    });
    say();
  }

  Array.prototype.forEach.call($("jf-sched").querySelectorAll("[data-mode]"), (b: HTMLElement): void => {
    b.onclick = (): void => {
      readFields();
      const next = b.getAttribute("data-mode");
      // Switching keeps what the modes share and fills a sane opening state for what they do
      // not, so the schedule the user was shaping stays shapeable.
      if (next !== "interval" && !sched.time) sched.time = "08:00";
      if (next === "weekly" && !sched.days) sched.days = [1];
      if (next === "monthly" && !sched.day) sched.day = 1;
      sched.mode = next as JobSched["mode"];
      Array.prototype.forEach.call($("jf-sched").querySelectorAll("[data-mode]"), (b2: HTMLElement): void => {
        b2.setAttribute("aria-selected", b2.getAttribute("data-mode") === next ? "true" : "false");
      });
      renderFields();
    };
  });
  renderFields();
  ensureCronstrue((): void => { say(); }); // the sentence appears the moment the library does
  const cmdSay = $("jf-cmd-say");
  function sayCommand(): void {
    cmdSay.className = "sched-say cmd-say";
    cmdSay.textContent = "";
    const joined = joinCommand($<HTMLTextAreaElement>("jf-command").value);
    if (joined) cmdSay.textContent = tr("jobs.savesCmd", { cmd: joined });
  }
  $<HTMLTextAreaElement>("jf-command").oninput = sayCommand;
  sayCommand();
  $<HTMLTextAreaElement>("jf-command").focus();
}

async function saveJob(existing: ApiJobRow | null): Promise<void> {
  // On edit the name input is disabled, so it still carries the identity the URL needs.
  const name = $<HTMLInputElement>("jf-name").value.trim();
  // The preview under the box has been showing this exact join all along.
  const command = joinCommand($<HTMLTextAreaElement>("jf-command").value);
  if (!name) { toast(tr("jobs.nameRequired"), true); return; }
  if (!command) { toast(tr("jobs.commandRequired"), true); return; }
  // The builder produces (and has already said) the schedule: the same sentence it showed is
  // what saves, and the same complaint it printed is what toasts here.
  let sched: { body: { everySec?: number; cron?: string }; say: string };
  try { sched = schedToBody(schedState!); } catch (err) { toast(errText(err), true); return; }
  const body: Record<string, unknown> = { name: name, command: command, enabled: $<HTMLInputElement>("jf-enabled").checked };
  if (sched.body.everySec != null) body.everySec = sched.body.everySec;
  else body.cron = sched.body.cron;
  // Minutes in the sheet, milliseconds on the wire — nobody should ever have to type 600000.
  const mins = Number($<HTMLInputElement>("jf-timeout").value);
  if (mins > 0) body.timeoutMs = Math.round(mins * 60000);
  const cwd = $<HTMLInputElement>("jf-cwd").value.trim();
  if (cwd) body.cwd = cwd;
  // The env box: every line a KEY=value the run will be handed. A bad line stops the
  // save — a job quietly running WITHOUT a variable the user believes it has is the
  // worst kind of wrong.
  const parsed = parseEnvLines($<HTMLTextAreaElement>("jf-env").value);
  if (parsed.error) { toast(parsed.error, true); return; }
  if (Object.keys(parsed.env!).length) body.env = parsed.env!;
  const j = await apiJson("/api/jobs/" + encodeURIComponent(name), { method: "PUT", body: JSON.stringify(body) });
  if (!j) return;
  const picked = $("g-sel") ? $<HTMLSelectElement>("g-sel").value : null;
  closeSheet();
  if (!existing && picked) {
    // The select the sheet carried is where the new job lands (docs/20 G4). A gateway
    // without the family route just keeps the first-group default - not worth a toast.
    rememberGroup("jobs", picked);
    await assignMember("jobs", name, picked);
  }
  toast(tr(existing ? "jobs.savedName" : "jobs.addedName", { name }));
  await loadJobs();
}

/* --- the advanced sheet (v2: schema-driven form + JSON editor over the config row) -------------- */

/** The live action list, fetched once per sheet open: the action input form is generated from
 *  each capability's own schema (docs/10 §7), exactly like the Run view's argument fields. */
async function fetchActions(): Promise<ApiActionRow[]> {
  const j = await apiJson<ApiActionsResponse>("/api/actions");
  return (j && j.actions) || [];
}

function actionByType(actions: ApiActionRow[], type: string): ApiActionRow | null {
  return actions.find((a) => { return a.type === type; }) || null;
}

/** The Advanced editor. `job` null = create. Edits go through PUT /api/plugins/jobs/config
 * (docs/11 §7.2 — there is no second CRUD surface): GET the row, replace this one definition,
 * PUT the whole config back with the revision just read. The form writes only the keys it
 * owns onto the definition object it loaded, so unknown-but-legal fields ride along, and the
 * JSON textarea is always the definition itself — the two editors are one object. */
async function openV2Sheet(job: ApiJobRow | null): Promise<void> {
  const editing = !!job;
  const row = await apiJson<{ config?: JobConfigRow }>("/api/plugins/jobs/config");
  if (!row) return;
  const config = row.config || {};
  const defs = config.definitions || {};
  const name = editing ? job?.name : "";
  const base = editing ? defs[name] : defTemplate("new-job");
  const actions = await fetchActions();
  const form = v2ToForm(base);
  // The row's own groups list is the truth for this sheet: what the config PUT will carry.
  // A header "+" promise is consumed here; the select is the truth from then on.
  const rowGroups = config.groups && config.groups.length ? config.groups : ["default"];
  let picked: string | null = null;
  if (!editing) {
    const staged = takeJobPendingGroup();
    picked = staged != null ? staged : resolveDefaultGroup(rowGroups, lastGroup("jobs"));
  }

  const actionOptNodes = actions.map((a) => {
    return h("option", { value: a.type, selected: a.type === form.actionType },
      a.type + (a.title && a.title !== a.type ? " \u2014 " + a.title : ""));
  });
  const current = actionByType(actions, form.actionType);
  const inputFields: HChild = current
    ? argFieldsNode({ inputSchema: current.schema }, "ja-", (base.action && base.action.input) || {})
    : hint(tr("jobs.actionTypeRegisteredRight"));

  /* Field helpers keep the long form flat: one line per field, no paren nesting deep enough to
     miscount. The ids are the contract the wiring below reads. The rows the trigger kind shows
     and hides keep theirs on the library pieces that draw them. */
  const fld = (label: string, control: HTMLElement): HTMLElement => { return field({ label, control }); };
  const sel = (id: string, opts: HTMLOptionElement[]): HTMLSelectElement => h("select", { id }, opts);
  const opt = (v: string, on: boolean): HTMLOptionElement => h("option", { value: v, selected: on }, v);
  const intervalRow = pair(
    fld("everyMs", h("input", { id: "jv-every", value: form.everyMs, placeholder: "3600000", autocomplete: "off" })),
    fld("firstRun", sel("jv-first", JOB_FIRST_RUNS.map((f) => opt(f, form.firstRun === f)))));
  intervalRow.id = "jv-interval-row";
  // The raw cron and its one-sentence answer, shown and hidden together.
  const cronRow = h("div", { id: "jv-cron-row" },
    fld(tr("jobs.cronLocalTime"), h("input", { id: "jv-cron", value: form.cron, placeholder: "30 3 * * *", autocomplete: "off" })),
    h("div", { class: "sched-say", id: "jv-cron-say" }));
  // The library sheet (docs/46 P6). It was drawn "sheet wide", a class no stylesheet ever
  // defined: this sheet has always been the standard width.
  showSheet(sheet({
    title: editing ? tr("jobs.definitionOf", { name }) : tr("jobs.newDefinitionIn", { picked: picked ?? "" }),
    label: tr(editing ? "jobs.editDefinition" : "jobs.newDefinition"),
    body: [
      editing ? null : fld("id", h("input", { id: "jv-id", value: name, placeholder: tr("jobs.nightlyVacuum"), autocomplete: "off" })),
      editing ? null : groupFieldNode(rowGroups, picked),
      pair(
        fld("title", h("input", { id: "jv-title", value: form.title, autocomplete: "off" })),
        fld(tr("jobs.labelsCommaSeparated"), h("input", { id: "jv-labels", value: form.labels, placeholder: tr("jobs.opsNightly"), autocomplete: "off" }))),
      // "One schedule per definition" is the trigger's hint - it used to pose as a second field
      // ("first firing") whose control was that sentence.
      field({ label: "trigger", control: sel("jv-kind", JOB_TRIGGER_KINDS.map((k) => opt(k, form.kind === k))), hint: tr("jobs.oneScheduleDefinition") }),
      intervalRow,
      cronRow,
      pair(
        fld("timeoutMs", h("input", { id: "jv-timeout", value: form.timeoutMs, placeholder: "600000", autocomplete: "off" })),
        checkField({ label: tr("jobs.disabled"), control: h("input", { type: "checkbox", id: "jv-disabled", checked: !!form.disabled }) as HTMLInputElement })),
      pair(
        fld("overlap", sel("jv-overlap", JOB_OVERLAPS.map((o) => opt(o, form.overlap === o)))),
        fld("misfire", sel("jv-misfire", JOB_MISFIRES.map((m) => opt(m, form.misfire === m))))),
      pair(
        fld("retry.maxAttempts", h("input", { id: "jv-rmax", value: form.retryMax, placeholder: "1", autocomplete: "off" })),
        fld("retry.delayMs", h("input", { id: "jv-rdelay", value: form.retryDelayMs, placeholder: "0", autocomplete: "off" }))),
      pair(
        fld("retry.backoff", sel("jv-rback", JOB_BACKOFFS.map((b) => opt(b, form.retryBackoff === b)))),
        field({
          group: true,
          label: tr("jobs.retryRetryon"),
          control: h("div", null, JOB_RETRY_ONS.map((r) => {
            return checkField({ label: r, control: h("input", { type: "checkbox", data: { retryon: r }, checked: form.retryOn.includes(r) }) as HTMLInputElement });
          })),
        })),
      pair(
        fld("output.capture", sel("jv-capture", JOB_CAPTURES.map((c) => opt(c, form.capture === c)))),
        fld("output.maxBytes", h("input", { id: "jv-maxbytes", value: form.maxBytes, placeholder: "16384", autocomplete: "off" }))),
      fld("action", sel("jv-action", actionOptNodes)),
      h("div", { id: "jv-inputs" }, inputFields),
      field({ label: tr("jobs.definitionJsonExact"), control: h("textarea", { id: "jv-json", rows: 12, spellcheck: false }), hint: tr("jobs.formWritesOnlyFields") }),
    ],
    foot: [
      btn(tr("jobs.formJson"), { id: "jv-form-to-json" }),
      btn(tr("jobs.cancel"), { id: "jv-cancel" }),
      btn(tr("jobs.save"), { kind: "primary", id: "jv-save" }),
    ],
  }));

  // The one definition object both editors work on. `dirty` marks JSON as hand-edited:
  // from then on the textarea wins, until Form -> JSON rebuilds it from the form again.
  let def = JSON.parse(JSON.stringify(base)) as JobDef;
  let dirty = false;
  const jsonBox = $<HTMLTextAreaElement>("jv-json");
  jsonBox.value = JSON.stringify(def, null, 2) + "\n";

  function syncTriggerRows(): void {
    const kind = $<HTMLSelectElement>("jv-kind").value;
    $("jv-interval-row").style.display = kind === "interval" ? "" : "none";
    $("jv-cron-row").style.display = kind === "cron" ? "" : "none";
  }
  syncTriggerRows();
  $<HTMLSelectElement>("jv-kind").onchange = syncTriggerRows;

  // The raw cron field gets the same one-sentence answer as the builder: whatever mode a
  // definition came from, the expression on screen is the schedule that will fire.
  const jvSay = $("jv-cron-say");
  function sayV2(): void {
    const expr = $<HTMLInputElement>("jv-cron").value.trim();
    jvSay.className = "sched-say";
    jvSay.textContent = "";
    try { if (expr) jvSay.textContent = describeCron(expr) + "."; }
    catch (err) { jvSay.className = "sched-say bad"; jvSay.textContent = errText(err); }
  }
  $<HTMLInputElement>("jv-cron").oninput = sayV2;
  sayV2();
  ensureCronstrue((): void => { sayV2(); });

  // Switching the action type rebuilds the input form from that capability's schema.
  $<HTMLSelectElement>("jv-action").onchange = (): void => {
    const next = actionByType(actions, $<HTMLSelectElement>("jv-action").value);
    fill($("jv-inputs"), next
      ? argFieldsNode({ inputSchema: next.schema }, "ja-", {})
      : hint(tr("jobs.registeredEditInputJson")));
  };

  /* The selects were rendered from the JOB_* lists, so legalOf is a type narrowing, not a
     second validation - it throws only if the DOM and the lists disagree (a panel bug). */
  function formValues(): JobFormValues {
    const retryOn: JobRetryOn[] = [];
    document.querySelectorAll<HTMLInputElement>("[data-retryon]").forEach((cb) => {
      if (cb.checked) retryOn.push(legalOf(cb.dataset.retryon || "", JOB_RETRY_ONS, "retry.retryOn"));
    });
    return {
      title: $<HTMLInputElement>("jv-title").value.trim(),
      labels: $<HTMLInputElement>("jv-labels").value,
      disabled: $<HTMLInputElement>("jv-disabled").checked,
      kind: legalOf($<HTMLSelectElement>("jv-kind").value, JOB_TRIGGER_KINDS, "trigger.kind"),
      everyMs: $<HTMLInputElement>("jv-every").value.trim(),
      firstRun: legalOf($<HTMLSelectElement>("jv-first").value, JOB_FIRST_RUNS, "trigger.firstRun"),
      cron: $<HTMLInputElement>("jv-cron").value.trim(),
      timeoutMs: $<HTMLInputElement>("jv-timeout").value.trim(),
      overlap: legalOf($<HTMLSelectElement>("jv-overlap").value, JOB_OVERLAPS, "overlap"),
      misfire: legalOf($<HTMLSelectElement>("jv-misfire").value, JOB_MISFIRES, "misfire"),
      retryMax: $<HTMLInputElement>("jv-rmax").value.trim(),
      retryDelayMs: $<HTMLInputElement>("jv-rdelay").value.trim(),
      retryBackoff: legalOf($<HTMLSelectElement>("jv-rback").value, JOB_BACKOFFS, "retry.backoff"),
      retryOn: retryOn,
      capture: legalOf($<HTMLSelectElement>("jv-capture").value, JOB_CAPTURES, "output.capture"),
      maxBytes: $<HTMLInputElement>("jv-maxbytes").value.trim(),
      actionType: $<HTMLSelectElement>("jv-action").value,
    };
  }

  function formToDef(): JobDef {
    const currentAction = actionByType(actions, $<HTMLSelectElement>("jv-action").value);
    const input = currentAction
      ? readRunArgs({ inputSchema: currentAction.schema }, "ja-")
      : (def.action && def.action.input) || {};
    return formToV2(formValues(), def, input);
  }

  const formToJson = (): void => {
    try {
      def = formToDef();
      jsonBox.value = JSON.stringify(def, null, 2) + "\n";
      dirty = false;
    } catch (e) {
      toast(errText(e), true);
    }
  };
  $<HTMLButtonElement>("jv-form-to-json").onclick = formToJson;

  jsonBox.oninput = (): void => { dirty = true; };

  $<HTMLButtonElement>("jv-cancel").onclick = closeSheet;
  $<HTMLButtonElement>("jv-save").onclick = (): void => { void save(); };

  async function save(): Promise<void> {
    const id = editing ? name : $<HTMLInputElement>("jv-id").value.trim();
    if (!id || !/^[\w.-]+$/.test(id)) { toast(tr("jobs.idRequiredLettersDigits"), true); return; }
    if (dirty) {
      // The JSON textarea is the source of truth once hand-edited.
      try { def = JSON.parse(jsonBox.value) as JobDef; }
      catch (e) { toast(tr("jobs.definitionJsonParseError", { error: errText(e) }), true); return; }
    } else {
      // Form -> def (and the box): that click IS what save means for the form path. A read
      // error here toasts on its own; the dirty flag stays set, so save stops for a fix.
      formToJson();
      if (dirty) return;
    }
    // A fresh GET right before the PUT: the whole-row CAS (docs/11 §7.2) wants the newest
    // revision, and a concurrent edit anywhere in the row must not be silently overwritten.
    const fresh = await apiJson<{ config?: JobConfigRow; revision?: number }>("/api/plugins/jobs/config");
    if (!fresh) return;
    const cfg = fresh.config || {};
    const definitions = cfg.definitions || {};
    if (!editing) {
      // The group the sheet promised: a definition key, so the whole-row CAS carries it and
      // no second request is needed (docs/20 G4). A "group" typed straight into the JSON
      // wins over the select - the textarea is the truth once hand-edited.
      const g = $("g-sel") ? $<HTMLSelectElement>("g-sel").value : null;
      if (g) {
        if (def.group == null) def.group = g;
        rememberGroup("jobs", g);
      }
    }
    definitions[id] = def;
    cfg.definitions = definitions;
    const reply = await apiJson<unknown>("/api/plugins/jobs/config", {
      method: "PUT",
      body: JSON.stringify({ revision: fresh.revision, config: cfg }),
    });
    if (!reply) return; // apiJson toasted the 400/409/500 with the field path
    closeSheet();
    toast(tr(editing ? "jobs.savedName" : "jobs.addedName", { name: id }));
    await loadJobs();
  }
}

/* --- the run-history sheet ---------------------------------------------------------------------- */

/* The sheet pages through the JSONL history: each fetch appends to the domain buffer and the
   whole sheet re-renders from it, so "Load more" is just another fetch with a cursor. The
   cursor parameter is the v2 spelling (docs/11 §7.3); the records carry the v2 outcome
   fields, so a skipped or missed line says so instead of masquerading as a failed run. */
async function openRunsSheet(name: string, cursor?: string | null): Promise<void> {
  if (!cursor || !jobHistoryIsFor(name)) {
    startJobHistory(name);
    runsOpen = new Set();
  }
  const j = await apiJson<{ runs?: ApiJobRunRecord[]; nextBefore?: string }>("/api/jobs/" + encodeURIComponent(name) + "/runs?limit=20" +
    (cursor ? "&cursor=" + encodeURIComponent(cursor) : ""));
  if (!j) return;
  appendJobRuns(j.runs || []);
  renderRunsSheet(j.nextBefore || null);
}

/* The open rows of the history sheet, by timeline id (a run's index in the buffer: Load more
   appends older runs behind it, so an open row stays open across a page). */
let runsOpen = new Set<string>();

/** The first line worth reading of what a run said - the error, else its output. */
function firstLine(text: string): string {
  const line = text.split("\n").map((s: string): string => { return s.trim(); }).find((s: string): boolean => { return !!s; }) || "";
  return line.length > 200 ? line.slice(0, 200) + "\u2026" : line;
}

/** One run as a timeline item (docs/46 §3.6): the time, what triggered it, the first line of what
 *  it said, the duration. A failure is a red tag - the exit code when there is one; an outcome
 *  that is not a run at all (skipped, missed) is an amber one, with no duration. Identical
 *  consecutive runs fold into ×N. */
function runItem(r: ApiJobRunRecord, i: number): TimelineItem {
  const ran = !r.outcome || r.outcome === "ran";
  const bad = r.exitCode != null && r.exitCode !== 0 ? tr("jobsV2.exitN", { n: r.exitCode })
    : r.timedOut ? tr("jobsV2.timedOut") : r.canceled ? tr("jobsV2.canceled") : tr("jobs.failedTag");
  return {
    id: String(i),
    at: Date.parse(r.at),
    title: r.trigger || "?",
    arg: firstLine(r.error || r.output || ""),
    ms: ran ? r.ms : undefined,
    status: !ran ? { text: r.outcome, tone: "warn" } : r.ok ? undefined : { text: bad, tone: "bad" },
    same: [r.trigger, r.ok, r.outcome, r.exitCode, r.error || "", r.output || ""].join("\u0001"),
  };
}

/** An open run: the history meta line (attempt, duration, exit, why it did not run), then the
 *  output as the value block the Logs and Traffic rows use. */
function runBody(run: ApiJobRunRecord[]): HChild {
  const r = run[0];
  const out = (r.error ? r.error + "\n\n" : "") + (r.output || "") + (r.preview ? tr("jobs.outputTailCappedN16") : "");
  return [
    timelineMeta([historyMeta(r), run.length > 1 ? tr("jobs.nIdenticalRuns", { n: run.length }) : null]),
    out
      ? valueBlock({ label: tr("jobs.output") }, ...readableBody(out, !r.ok))
      : note(r.outcome && r.outcome !== "ran" ? tr("jobs.outcomeOutputRecorded", { outcome: r.outcome }) : tr("jobs.noOutput")),
  ];
}

/** The run history in the library sheet, as the event list (docs/46 §3.6): the Logs call list's
 *  shape - day headings, time column, ×N, failure tags - for a job's runs. */
function renderRunsSheet(nextBefore?: string | null): void {
  const hist = jobHistory();
  const runs = hist?.runs || [];
  const items = runs.map(runItem);
  const recordsOf = (run: TimelineItem[]): ApiJobRunRecord[] => { return run.map((it: TimelineItem): ApiJobRunRecord => { return runs[Number(it.id)]; }); };
  showSheet(sheet({
    title: tr("jobs.runsName", { name: hist?.name || "" }),
    label: tr("jobs.runHistory"),
    body: items.length
      ? timeline(items, { open: runsOpen, body: (_it: TimelineItem, run: TimelineItem[]): HChild => { return runBody(recordsOf(run)); } })
      : note(tr("jobs.runsRecordedWaitSchedule")),
    foot: [
      nextBefore ? btn(tr("jobs.loadMore"), { id: "jr-more" }) : null,
      btn(tr("jobs.done"), { kind: "primary", id: "jr-done" }),
    ],
  }));
  $("jr-done").onclick = closeSheet;
  const more = document.getElementById("jr-more");
  if (more) more.onclick = (): void => { void openRunsSheet(hist!.name, nextBefore); };
  // Only a row's summary toggles it (a real <button>: Enter and Space arrive as this click); a
  // click inside an open body is someone reading or selecting the output.
  const root = $("sheet").querySelector<HTMLElement>(".tl");
  if (root) root.onclick = (ev: MouseEvent): void => {
    const item = targetEl(ev)?.closest(".tl-sum")?.closest<HTMLElement>(".tl-item");
    const id = item?.dataset.tlId;
    if (!id) return;
    const open = !runsOpen.has(id);
    if (open) runsOpen.add(id); else runsOpen.delete(id);
    const run = collapseRuns(items).find((r: TimelineItem[]): boolean => { return r[0].id === id; }) || [];
    timelineToggle(root, id, open ? runBody(recordsOf(run)) : undefined);
  };
}

export { jobRowNode, openRunsSheet, probeJobs, renderJobs, patchJobs, wireJobs };

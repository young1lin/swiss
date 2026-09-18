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
import type { ApiJobRow, ApiJobRunRecord, ApiMcpTool, ToolInputSchema } from "./types/api.js";
import type { GroupCfg, GroupSlice } from "./types/dom.js";
import type { JobConfigRow, JobDef, JobFormValues, JobSched } from "./types/state.js";
import { $, api, apiJson, dotTitle, emptyHtml, errText, esc, icon, state, toast, whenLabel } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { assignMember, groupFieldHtml, groupOf as makeGroupOf, lastGroup, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, saveOrder, slice } from "./groups.js";
import { popupMenu } from "./menu.js";
import { jobDotClass, jobGroupsList, jobRowHtml, jobsChipText, loadJobs } from "./polling.js";
import { argFieldsHtml, readRunArgs } from "./run.js";
import { defTemplate, envToLines, formToV2, historyMeta, parseEnvLines, v2ToForm } from "./jobs-v2.js";
import { loadCronstrue } from "./vendor/cronstrue/2.52.0/index.js";
import { currentView } from "./ui-state.js";

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
  void saveOrder("jobs", state.jobs.data.map((j: ApiJobRow): string => { return j.name; }));
}

/** Move one row to just before/after another, re-render, persist. */
function moveJobRow(id: string, target: string, before: boolean): void {
  if (!id || !target || id === target) return;
  const rows = state.jobs.data;
  const item = rows.filter((r: ApiJobRow): boolean => { return r.name === id; })[0];
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
  const row = state.jobs.data.filter((r: ApiJobRow): boolean => { return r.name === id; })[0];
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

function renderJobs(): void {
  const rows = state.jobs.data;
  // The structural signature the poll compares against: the group names, then every row as
  // name+group in order — any add/remove/reorder/regroup means a rebuild is due; dots and
  // labels alone never justify one.
  state.jobs.painted = jobGroupsList().join("\n") + "\u0000" +
    rows.map((j: ApiJobRow): string => { return j.name + "\u0001" + jobGroupOfRow(j); }).join("\n");
  const body = rows.length
    ? '<div id="jobGroups"></div>'
    : emptyHtml({ icon: "clock", title: "No jobs yet", hint: "Scheduled commands the gateway runs on this machine. Add one with New." });
  // .wide + .pane-head + .tun-foot: the Tunnels view's exact frame — the rows are the same
  // name-plus-subtext-plus-buttons shape, so they wear the same classes.
  // No location title: the context bar already says "Jobs". The body header carries the
  // workflow description and nothing else; the actions sit in the section head below.
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div>' +
      '<div class="pane-desc">Scheduled commands the gateway runs locally — an interval or a 5-field cron in local time. Overlap, misfire and retry policies per job; every outcome lands in the run history.</div>' +
    "</div></div>" +
    '<div class="sec-head"><span class="sec-cap">Scheduled commands</span>' +
      '<span style="display:flex;gap:var(--s2)"><button class="btn" id="jobNewGroup">New group</button>' +
      '<button class="btn" id="jobNewAdv">New (advanced)</button>' +
      '<button class="btn primary" id="jobNew">New</button></span></div>' +
    body +
    '<div class="tun-foot" data-foot>' + esc(jobsChipText()) + "</div>" +
  "</div>";
  if (rows.length) {
    // One group per slice — the component owns the header band and the empty line (docs/20
    // G4). Empty groups keep their place: that is how you drag the first job into one.
    const host = $("jobGroups");
    slice(rows, jobGroupsList(), jobGroupOfRow).forEach((g: GroupSlice<ApiJobRow>): void => {
      host.appendChild(mountGroup(jobCfg(), g));
    });
  }
  wireJobs();
}

/** The jobs scope's cfg for mountGroup (see groups.js for the full contract). Built fresh each
 *  render so names and the fold map are always the live objects. */
function jobCfg(): GroupCfg<ApiJobRow> {
  return {
    scope: "jobs",
    density: "page",
    names: jobGroupsList(),
    collapsed: state.jobs.collapsed,
    noun: "job",
    addTitle: (g: string): string => { return "Add a job to " + g; },
    onAdd: (g: string): void => {
      state.jobs.pendingGroup = g; // a real name now — the sheet's save lands the job in it
      openJobSheet(null);
    },
    reload: (): Promise<void> => { return loadJobs(); },
    render: renderJobs,
    afterDrag: (): void => { renderJobs(); }, // the catch-up rebuild a deferred poll owes
    drag: {
      get: (): string | null => { return state.jobs.dragging; },
      set: (v: string | null): void => { state.jobs.dragging = v; },
    },
    dragGroup: {
      get: (): string | null => { return state.jobs.draggingGroup; },
      set: (v: string | null): void => { state.jobs.draggingGroup = v; },
    },
    rowId: (r: ApiJobRow): string => { return r.name; },
    rowSel: (r: ApiJobRow): string => {
      const v = window.CSS && CSS.escape ? CSS.escape(r.name) : r.name;
      return '[data-job="' + v + '"]';
    },
    rowsById: (): ApiJobRow[] => { return state.jobs.data; },
    groupOfRow: jobGroupOfRow,
    rowsHtml: (g: GroupSlice<ApiJobRow>): string => { return g.rows.map(jobRowHtml).join(""); },
    onMoveRow: moveJobRow,
    onAssign: (id: string, g: string | null): void => { void assignJobGroup(id, g); },
  };
}

/** Poll-safe update: dots, last/next labels, the Run-now button, group counts, the footer.
 *  Never structure — and never a rebuild mid-drag, which would cancel the gesture. */
function patchJobs(): void {
  if (currentView() !== "jobs") return;
  const pane = $("pane");
  const sig = jobGroupsList().join("\n") + "\u0000" +
    state.jobs.data.map((j: ApiJobRow): string => { return j.name + "\u0001" + jobGroupOfRow(j); }).join("\n");
  if (!pane.querySelector(".group") || sig !== state.jobs.painted) {
    if (!state.jobs.dragging && !state.jobs.draggingGroup) renderJobs();
    return;
  }
  Array.prototype.forEach.call(pane.querySelectorAll("[data-job]"), (row: Element): void => {
    let j = null as ApiJobRow | null;
    state.jobs.data.forEach((cand: ApiJobRow): void => { if (cand.name === row.getAttribute("data-job")) j = cand; });
    if (!j) return;
    const busy = state.jobs.busy[j.name];
    const dot = row.querySelector(".dot") as HTMLElement | null;
    const word = busy ? "starting" : jobDotClass(j!);
    // Title and class move together (docs/18 V6): the poll never rebuilds the list, so a
    // title left behind by an earlier state would lie one hover later.
    if (dot) { dot.className = "dot " + word; dot.title = dotTitle(word); }
    // Rendered as (possibly empty) spans at build time so the patch can always fill them.
    const last = row.querySelector("[data-last]");
    if (last) last.textContent = j?.lastRunAt
      ? "\u00b7 last " + whenLabel(j?.lastRunAt) + (j?.lastOk === false ? " \u00b7 failed" : "") : "";
    const next = row.querySelector("[data-next]");
    if (next) next.textContent = j?.enabled && j?.nextDueAt ? "\u00b7 next " + whenLabel(j?.nextDueAt) : "";
    const run = row.querySelector("[data-run]") as HTMLButtonElement | null;
    if (run) { run.disabled = !!busy || !!j?.running; run.textContent = busy ? "\u2026" : "Run now"; }
  });
  // Group counts move with rows: a count that only refreshed on rebuild would disagree with
  // the patched dots beside it for the rest of the poll.
  Array.prototype.forEach.call(pane.querySelectorAll("[data-group]"), (grp: HTMLElement): void => {
    const n = state.jobs.data.filter((j: ApiJobRow): boolean => { return jobGroupOfRow(j) === grp.dataset.group; }).length;
    const badge = grp.querySelector(".grp-n");
    if (badge) badge.textContent = String(n);
  });
  const foot = pane.querySelector("[data-foot]");
  if (foot) foot.textContent = jobsChipText();
}

function jobByName(name: string | null): ApiJobRow | null {
  let out: ApiJobRow | null = null;
  state.jobs.data.forEach((j: ApiJobRow): void => { if (j.name === name) out = j; });
  return out;
}

function wireJobs(): void {
  $("jobNew").onclick = (): void => { openJobSheet(null); };
  $("jobNewAdv").onclick = (): void => { void openV2Sheet(null); };
  if ($("jobNewGroup")) $("jobNewGroup").onclick = (): void => {
    newGroupFlow("jobs", jobGroupsList(), () => { return loadJobs(); });
  };
  Array.prototype.forEach.call($("pane").querySelectorAll("[data-job]"), (row: Element): void => {
    const job = jobByName(row.getAttribute("data-job"));
    if (!job) return;
    row.querySelector<HTMLButtonElement>("[data-run]")!.onclick = (): void => { void runJob(job?.name); };
    const more = row.querySelector("[data-more]") as HTMLButtonElement | null;
    if (more) more.onclick = (ev: MouseEvent): void => {
      // The overflow half of the row (docs/18 V5): the rare verbs and the destructive one.
      // stopPropagation FIRST: connect.js closes any open menu on clicks that reach document,
      // and without this the very click that opens the menu also tears it down.
      ev.stopPropagation();
      popupMenu(more?.getBoundingClientRect(), [
        { label: "Edit", fn: () => {
            // A definition the v1 shape cannot spell (docs/11 §7.1: editableInV1 false) goes
            // straight to the advanced sheet — the v1 form would silently drop its fields.
            const job_ = job!;
            if (job_.editableInV1 === false) void openV2Sheet(job_);
            else openJobSheet(job_);
          } },
        { label: "History", fn: (): void => { void openRunsSheet(job?.name); } },
        { sep: true },
        { label: "Delete", danger: true, fn: (): void => { deleteJob(job!); } },
      ]);
    };
  });
}

/* --- actions ---------------------------------------------------------------------------------- */

/** Run now submits {"async": true} (docs/11 §7.3) and polls the coordinator's run view: the
 * run outlives the page, and the row stays live through the poll. The toast still names the
 * outcome, because the record settles into the history either way. */
async function runJob(name: string): Promise<void> {
  state.jobs.busy[name] = true;
  patchJobs();
  const j = await apiJson<{ runId?: number }>("/api/jobs/" + encodeURIComponent(name) + "/run", {
    method: "POST",
    body: JSON.stringify({ async: true }),
  });
  if (!j || j.runId == null) { delete state.jobs.busy[name]; patchJobs(); return; }
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
    delete state.jobs.busy[name];
    const ok = v.state === "succeeded";
    toast(name + ": " + v.state + (v.ms != null ? " in " + v.ms + " ms" : "") +
      (v.error ? " \u2014 " + v.error : ""), !ok);
    await loadJobs();
    return;
  }
  delete state.jobs.busy[name];
  patchJobs();
  toast(name + " is still running \u2014 watch it in History", false);
}

function deleteJob(job: ApiJobRow): void {
  if (!confirm('Delete job "' + job.name + '"? The job stops; its run history stays on disk.')) return;
  void (async () => {
    const j = await apiJson<unknown>("/api/jobs/" + encodeURIComponent(job.name), { method: "DELETE" });
    if (!j) return;
    toast("Deleted " + job.name);
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

const DOW_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function two(n: number): string { return (n < 10 ? "0" : "") + n; }

/** cronstrue, with the two checks of our own: exactly five fields (the API is a 5-field cron;
 *  cronstrue also speaks 6-7 field dialects, whose descriptions would lie about what saves),
 *  and a missing global means the vendor file was not served — say that, not a TypeError. */
let cronstrueLib: { toString: (expr: string, opts?: { throwExceptionOnParseError?: boolean }) => string } | null = null;

/** cronstrue, loaded once through the vendored shim (docs/14 §2: UMD bundles arrive as
 *  classic scripts, not imports). Sheets call ensureCronstrue with their own re-say as the
 *  ready callback; before the library lands, describeCron's complaint names it instead of
 *  throwing a TypeError about it. */
function ensureCronstrue(ready?: (lib: { toString: (expr: string, opts?: { throwExceptionOnParseError?: boolean }) => string }) => void): void {
  if (cronstrueLib) { if (ready) ready(cronstrueLib); return; }
  loadCronstrue().then((lib: { toString: (expr: string, opts?: { throwExceptionOnParseError?: boolean }) => string }): void => {
    cronstrueLib = lib;
    if (ready) ready(lib);
  }, () => { /* the say-line's own complaint covers the failure */ });
}

function describeCron(expr: string): string {
  if (!/^(\S+\s+){4}\S+$/.test(expr)) throw new Error("give all five cron fields");
  if (!cronstrueLib) throw new Error("cronstrue is still loading — one moment");
  return cronstrueLib.toString(expr, { throwExceptionOnParseError: true });
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
        if (days.indexOf(d) < 0) days.push(d);
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
    if (!(s.every! > 0)) throw new Error("the interval needs a number of " + unit_ + " (at least 1)");
    const mult = s.unit === "hours" ? 3600 : s.unit === "minutes" ? 60 : 1;
    // The unit options are plural ("hours"); the sentence singularises for "every 1 hour".
    const unit = s.every === 1 ? unit_.replace(/s$/, "") : unit_;
    return { body: { everySec: s.every! * mult }, say: "Every " + s.every + " " + unit + "." };
  }
  const t = /^(\d{1,2}):(\d{2})$/.exec(s.time || "");
  if (!t) throw new Error("the time must be HH:MM (24-hour)");
  const hh = +t[1], mm = +t[2];
  if (hh > 23 || mm > 59) throw new Error("the time must be a real time of day");
  let expr;
  if (s.mode === "daily") expr = mm + " " + hh + " * * *";
  else if (s.mode === "weekly") {
    if (!s.days || !s.days.length) throw new Error("weekly needs at least one day picked");
    expr = mm + " " + hh + " * * " + s.days.join(",");
  } else if (s.mode === "monthly") {
    if (!(s.day! >= 1 && s.day! <= 31)) throw new Error("the day of month must be between 1 and 31");
    expr = mm + " " + hh + " " + s.day + " * *";
  } else {
    expr = (s.cron || "").trim().replace(/\s+/g, " ");
    if (!expr) throw new Error("the cron expression is empty");
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
    picked = state.jobs.pendingGroup != null ? state.jobs.pendingGroup
      : resolveDefaultGroup(jobGroupsList(), lastGroup("jobs"));
    state.jobs.pendingGroup = null;
  }
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit job" : "New job") + '">' +
      '<div class="sheet-head"><h2>' + (editing ? "Edit " + esc(job?.name) : "New job in " + esc(picked!)) + "</h2></div>" +
      '<div class="sheet-body">' +
        // The name IS the identity (the PUT path is the job), so it is fixed once created.
        '<label class="field"><span>Name</span><input id="jf-name" value="' + esc(v.name || "") + '"' +
          (editing ? " disabled" : "") + ' placeholder="nightly-vacuum" autocomplete="off"></label>' +
        (editing ? "" : groupFieldHtml(jobGroupsList(), picked)) +
        '<label class="field"><span>Command</span><textarea id="jf-command" rows="2" placeholder="cmd /c backup.bat --flag value" spellcheck="false">' + esc(v.command) + "</textarea></label>" +
        '<div class="sched-say cmd-say" id="jf-cmd-say"></div>' +
        '<div class="hint">One command — a job supervises one process, not a shell script. Pipes and redirections need "cmd /c \u2026" (Windows) or "sh -c \u2026" (Unix) around them. ${ENV} refs expand at run time; several steps belong in a script the command runs, or in several jobs.</div>' +
        '<div class="sheet-cap">Schedule</div>' +
        '<div class="sched" id="jf-sched">' +
          '<div class="seg" role="tablist">' + ["interval", "daily", "weekly", "monthly", "cron"].map((m) => {
            return '<button type="button" role="tab" data-mode="' + m + '" aria-selected="' + (sched.mode === m) + '">' + m + "</button>";
          }).join("") + "</div>" +
          '<div id="jf-sched-fields"></div>' +
          '<div class="sched-say" id="jf-say"></div>' +
        "</div>" +
        '<div class="sheet-cap">Environment</div>' +
        '<div class="sheet-cap">Options</div>' +
        '<div class="two">' +
          '<label class="field"><span>Timeout (minutes)</span><input id="jf-timeout" type="number" min="0.5" step="0.5" value="' +
            (v.timeoutMs ? Math.max(0.5, Math.round(v.timeoutMs as number / 6000) / 10) : 10) + '" autocomplete="off"></label>' +
          '<label class="field"><span>Working directory</span><input id="jf-cwd" value="' + esc(v.cwd || "") + '" placeholder="Optional" autocomplete="off"></label>' +
        "</div>" +
        '<label class="field"><span>Environment variables</span><textarea id="jf-env" rows="3" placeholder="DEPLOY_ENV=staging&#10;LOG_DIR=C:\\logs" spellcheck="false">' + esc(envToLines(v.env)) + "</textarea></label>" +
        '<div class="hint">One KEY=value per line, added to the command\u2019s environment on top of what the gateway inherits. ${ENV} refs in a value resolve at run time.</div>' +
        '<label class="check"><input type="checkbox" id="jf-enabled"' + (v.enabled ? " checked" : "") + ">Enabled</label>" +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="jf-advanced">Advanced\u2026</button>' +
        '<button class="btn" id="jf-cancel">Cancel</button>' +
        '<button class="btn primary" id="jf-save">' + (editing ? "Save" : "Add") + "</button></div>" +
    "</div>";
  $("sheet").hidden = false;
  $("jf-cancel").onclick = closeSheet;
  $("jf-advanced").onclick = (): void => { void openV2Sheet(job); };
  $("jf-save").onclick = (): void => { void saveJob(job); };
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) closeSheet(); };

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
    let html = "";
    if (sched.mode === "interval") {
      html = '<div class="two">' +
        '<label class="field"><span>Run every</span><input id="jf-ev" type="number" min="1" value="' + (sched.every || "") + '" autocomplete="off"></label>' +
        '<label class="field"><span>Unit</span><select id="jf-ev-u">' +
          ["seconds", "minutes", "hours"].map((u: string): string => {
            return '<option value="' + u + '"' + (sched.unit === u ? " selected" : "") + ">" + u + "</option>";
          }).join("") + "</select></label></div>";
    } else if (sched.mode === "cron") {
      html = '<label class="field"><span>Cron expression (local time)</span><input id="jf-cron-in" value="' + esc(sched.cron || "") + '" placeholder="30 3 * * *" autocomplete="off"></label>' +
        '<div class="hint">Five fields: minute hour day-of-month month day-of-week.</div>';
    } else {
      const at = '<label class="field"><span>At</span><input id="jf-at" type="time" value="' + esc(sched.time || "08:00") + '"></label>';
      if (sched.mode === "daily") html = at;
      if (sched.mode === "weekly") {
        html = '<label class="field"><span>On</span><div class="days" id="jf-days">' +
          DOW_LABELS.map((d: string, i: number): string => {
            const on = sched.days && sched.days.indexOf(i) >= 0;
            return '<button type="button" data-dow="' + i + '" class="' + (on ? "on" : "") + '" aria-pressed="' + on + '">' + d + "</button>";
          }).join("") + "</div></label>" + at;
      }
      if (sched.mode === "monthly") {
        html = '<div class="two">' +
          '<label class="field"><span>On day</span><input id="jf-md" type="number" min="1" max="31" value="' + (sched.day || 1) + '" autocomplete="off"></label>' +
          at + "</div>";
      }
    }
    $("jf-sched-fields").innerHTML = html;
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
    if (joined) cmdSay.textContent = "Saves as: " + joined;
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
  if (!name) { toast("Name is required", true); return; }
  if (!command) { toast("Command is required", true); return; }
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
  toast((existing ? "Saved " : "Added ") + name);
  await loadJobs();
}

/* --- the advanced sheet (v2: schema-driven form + JSON editor over the config row) -------------- */

/** The live action list, fetched once per sheet open: the action input form is generated from
 *  each capability's own schema (docs/10 §7), exactly like the Run view's argument fields. */
async function fetchActions(): Promise<ApiMcpTool[]> {
  const j = await apiJson<{ actions?: ApiMcpTool[] }>("/api/actions");
  return (j && j.actions) || [];
}

function actionByType(actions: ApiMcpTool[], type: string): ApiMcpTool | null {
  for (let i = 0; i < actions.length; i++) if (actions[i].type === type) return actions[i];
  return null;
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
    picked = state.jobs.pendingGroup != null ? state.jobs.pendingGroup
      : resolveDefaultGroup(rowGroups, lastGroup("jobs"));
    state.jobs.pendingGroup = null;
  }

  const actionOpts = actions.map((a: ApiMcpTool): string => {
    return '<option value="' + esc(a.type) + '"' + (a.type === form.actionType ? " selected" : "") + ">" +
      esc(a.type) + (a.title && a.title !== a.type ? " \u2014 " + esc(a.title) : "") + "</option>";
  }).join("");
  const current = actionByType(actions, form.actionType);
  const inputFields = current
    ? argFieldsHtml({ inputSchema: current.schema as ToolInputSchema } as ApiMcpTool, "ja-", (base.action && base.action.input) || {})
    : '<div class="hint">This action type is not registered right now (its plugin is off) — edit its input in the JSON below.</div>';

  $("sheet").innerHTML =
    '<div class="sheet wide" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit definition" : "New definition") + '">' +
      '<div class="sheet-head"><h2>' + (editing ? "Definition \u2014 " + esc(name) : "New definition in " + esc(picked)) + "</h2></div>" +
      '<div class="sheet-body">' +
        (editing ? "" : '<label class="field"><span>id</span><input id="jv-id" value="' + esc(name) + '" placeholder="nightly-vacuum" autocomplete="off"></label>') +
        (editing ? "" : groupFieldHtml(rowGroups, picked)) +
        '<div class="two">' +
          '<label class="field"><span>title</span><input id="jv-title" value="' + esc(form.title) + '" autocomplete="off"></label>' +
          '<label class="field"><span>labels (comma-separated)</span><input id="jv-labels" value="' + esc(form.labels) + '" placeholder="ops, nightly" autocomplete="off"></label>' +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>trigger</span><select id="jv-kind">' +
            ["interval", "cron", "manual"].map((k) => {
              return '<option value="' + k + '"' + (form.kind === k ? " selected" : "") + ">" + k + "</option>";
            }).join("") + "</select></label>" +
          '<label class="field"><span>first firing</span><span class="hint">one schedule per definition</span></label>' +
        "</div>" +
        '<div class="two" id="jv-interval-row">' +
          '<label class="field"><span>everyMs</span><input id="jv-every" value="' + esc(form.everyMs) + '" placeholder="3600000" autocomplete="off"></label>' +
          '<label class="field"><span>firstRun</span><select id="jv-first">' +
            ["aligned", "immediate"].map((f) => {
              return '<option value="' + f + '"' + (form.firstRun === f ? " selected" : "") + ">" + f + "</option>";
            }).join("") + "</select></label>" +
        "</div>" +
        '<label class="field" id="jv-cron-row"><span>cron (local time)</span><input id="jv-cron" value="' + esc(form.cron) + '" placeholder="30 3 * * *" autocomplete="off">' +
          '<div class="sched-say" id="jv-cron-say"></div></label>' +
        '<div class="two">' +
          '<label class="field"><span>timeoutMs</span><input id="jv-timeout" value="' + esc(form.timeoutMs) + '" placeholder="600000" autocomplete="off"></label>' +
          '<label class="check"><input type="checkbox" id="jv-disabled"' + (form.disabled ? " checked" : "") + ">Disabled</label>" +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>overlap</span><select id="jv-overlap">' +
            ["skip", "queue-one"].map((o) => {
              return '<option value="' + o + '"' + (form.overlap === o ? " selected" : "") + ">" + o + "</option>";
            }).join("") + "</select></label>" +
          '<label class="field"><span>misfire</span><select id="jv-misfire">' +
            ["skip", "run-once"].map((m) => {
              return '<option value="' + m + '"' + (form.misfire === m ? " selected" : "") + ">" + m + "</option>";
            }).join("") + "</select></label>" +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>retry.maxAttempts</span><input id="jv-rmax" value="' + esc(form.retryMax) + '" placeholder="1" autocomplete="off"></label>' +
          '<label class="field"><span>retry.delayMs</span><input id="jv-rdelay" value="' + esc(form.retryDelayMs) + '" placeholder="0" autocomplete="off"></label>' +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>retry.backoff</span><select id="jv-rback">' +
            ["fixed", "exponential"].map((b) => {
              return '<option value="' + b + '"' + (form.retryBackoff === b ? " selected" : "") + ">" + b + "</option>";
            }).join("") + "</select></label>" +
          '<span class="field"><span>retry.retryOn</span><span style="display:flex;gap:var(--s2)">' +
            ["failure", "timeout"].map((r) => {
              return '<label class="check"><input type="checkbox" data-retryon="' + r + '"' +
                (form.retryOn.indexOf(r) >= 0 ? " checked" : "") + ">" + r + "</label>";
            }).join("") + "</span></span>" +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>output.capture</span><select id="jv-capture">' +
            ["tail", "none"].map((c) => {
              return '<option value="' + c + '"' + (form.capture === c ? " selected" : "") + ">" + c + "</option>";
            }).join("") + "</select></label>" +
          '<label class="field"><span>output.maxBytes</span><input id="jv-maxbytes" value="' + esc(form.maxBytes) + '" placeholder="16384" autocomplete="off"></label>' +
        "</div>" +
        '<label class="field"><span>action</span><select id="jv-action">' + actionOpts + "</select></label>" +
        '<div id="jv-inputs">' + inputFields + "</div>" +
        '<label class="field"><span>definition JSON — the exact object that will be saved</span>' +
          '<textarea id="jv-json" rows="12" spellcheck="false"></textarea></label>' +
        '<div class="hint">The form writes only the fields it shows onto this object; anything else it already carries rides along untouched. Edit the JSON directly for anything the form does not know.</div>' +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="jv-form-to-json">Form \u2192 JSON</button>' +
        '<button class="btn" id="jv-cancel">Cancel</button>' +
        '<button class="btn primary" id="jv-save">Save</button></div>' +
    "</div>";
  $("sheet").hidden = false;

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
    $("jv-inputs").innerHTML = next
      ? argFieldsHtml({ inputSchema: next.schema } as ApiMcpTool, "ja-", {})
      : '<div class="hint">Not registered — edit the input in the JSON below.</div>';
  };

  function formValues(): JobFormValues {
    const retryOn: string[] = [];
    Array.prototype.forEach.call(document.querySelectorAll("[data-retryon]"), (cb: HTMLInputElement): void => {
      if (cb.checked) retryOn.push(cb.getAttribute("data-retryon")!);
    });
    return {
      title: $<HTMLInputElement>("jv-title").value.trim(),
      labels: $<HTMLInputElement>("jv-labels").value,
      disabled: $<HTMLInputElement>("jv-disabled").checked,
      kind: $<HTMLSelectElement>("jv-kind").value,
      everyMs: $<HTMLInputElement>("jv-every").value.trim(),
      firstRun: $<HTMLSelectElement>("jv-first").value,
      cron: $<HTMLInputElement>("jv-cron").value.trim(),
      timeoutMs: $<HTMLInputElement>("jv-timeout").value.trim(),
      overlap: $<HTMLSelectElement>("jv-overlap").value,
      misfire: $<HTMLSelectElement>("jv-misfire").value,
      retryMax: $<HTMLInputElement>("jv-rmax").value.trim(),
      retryDelayMs: $<HTMLInputElement>("jv-rdelay").value.trim(),
      retryBackoff: $<HTMLSelectElement>("jv-rback").value,
      retryOn: retryOn,
      capture: $<HTMLSelectElement>("jv-capture").value,
      maxBytes: $<HTMLInputElement>("jv-maxbytes").value.trim(),
      actionType: $<HTMLSelectElement>("jv-action").value,
    };
  }

  function formToDef(): JobDef {
    const currentAction = actionByType(actions, $<HTMLSelectElement>("jv-action").value);
    const input = currentAction
      ? readRunArgs({ inputSchema: currentAction.schema as ToolInputSchema } as ApiMcpTool, "ja-")
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
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) closeSheet(); };
  $<HTMLButtonElement>("jv-save").onclick = (): void => { void save(); };

  async function save(): Promise<void> {
    const id = editing ? name : $<HTMLInputElement>("jv-id").value.trim();
    if (!id || !/^[\w.-]+$/.test(id)) { toast("id is required (letters, digits, -, _, .)", true); return; }
    if (dirty) {
      // The JSON textarea is the source of truth once hand-edited.
      try { def = JSON.parse(jsonBox.value) as JobDef; }
      catch (e) { toast("definition JSON does not parse: " + errText(e), true); return; }
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
    toast((editing ? "Saved " : "Added ") + id);
    await loadJobs();
  }
}

/* --- the run-history sheet ---------------------------------------------------------------------- */

/* The sheet pages through the JSONL history: each fetch appends to state.jobs.hist and the
   whole sheet re-renders from it, so "Load more" is just another fetch with a cursor. The
   cursor parameter is the v2 spelling (docs/11 §7.3); the records carry the v2 outcome
   fields, so a skipped or missed line says so instead of masquerading as a failed run. */
async function openRunsSheet(name: string, cursor?: string | null): Promise<void> {
  if (!cursor || !state.jobs.hist || state.jobs.hist.name !== name) {
    state.jobs.hist = { name: name, runs: [] };
  }
  const j = await apiJson<{ runs?: ApiJobRunRecord[]; nextBefore?: string }>("/api/jobs/" + encodeURIComponent(name) + "/runs?limit=20" +
    (cursor ? "&cursor=" + encodeURIComponent(cursor) : ""));
  if (!j) return;
  state.jobs.hist!.runs = state.jobs.hist?.runs.concat(j.runs || []);
  renderRunsSheet(j.nextBefore || null);
}

function renderRunsSheet(nextBefore?: string | null): void {
  const hist = state.jobs.hist;
  const rows = hist?.runs.map((r: ApiJobRunRecord): string => {
    return '<div class="call">' +
      '<button class="call-sum" type="button">' +
        '<span class="dot ' + (r.ok ? "up" : "down") + '"></span>' +
        '<span class="call-tool">' + esc(whenLabel(r.at)) + "</span>" +
        '<span class="call-meta">' + esc(historyMeta(r)) + "</span>" +
        '<span class="chev" aria-hidden="true">' + icon("chevron-right") + "</span>" +
      "</button>" +
      '<div class="call-body" hidden><pre class="logs">' +
        esc((r.error ? r.error + "\n\n" : "") + (r.output || "") +
          (r.preview ? "\n\u2026 output tail-capped at 16 KB" : "") +
          (r.outcome && r.outcome !== "ran" ? "(" + r.outcome + " \u2014 no output recorded)" : "")) +
      "</pre></div>" +
    "</div>";
  }).join("");
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Run history">' +
      '<div class="sheet-head"><h2>Runs — ' + esc(hist?.name) + "</h2></div>" +
      '<div class="sheet-body">' +
        // One card holding every run, so the rows stack flush instead of each taking the
        // sheet's field gap — forty runs read as one list, not forty islands.
        '<div class="group">' +
        (rows || '<div class="row"><span class="rowmsg">No runs recorded yet — wait for the schedule, or press Run now.</span></div>') +
        "</div>" +
      "</div>" +
      '<div class="sheet-foot">' +
        (nextBefore ? '<button class="btn" id="jr-more">Load more</button>' : "") +
        '<button class="btn primary" id="jr-done">Done</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  $("jr-done").onclick = closeSheet;
  const more = $("jr-more");
  if (more) more.onclick = (): void => { void openRunsSheet(hist!.name, nextBefore); };
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
  // The Traffic expand idiom: clicking the summary toggles the raw output beneath it.
  Array.prototype.forEach.call($("sheet").querySelectorAll(".call-sum"), (b) => {
    b.onclick = () => {
      const body = b.parentElement.querySelector(".call-body");
      if (!body) return;
      body.hidden = !body.hidden;
      // .open is what turns the chevron — the Logs and Traffic rows key on it, not on `hidden`.
      b.parentElement.classList.toggle("open", !body.hidden);
    };
  });
}

export { openRunsSheet, probeJobs, renderJobs, patchJobs, wireJobs };

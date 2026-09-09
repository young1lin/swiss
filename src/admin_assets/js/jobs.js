/* ================================================================================================
   Jobs — the scheduled-command view.

   Same architecture as Tunnels: the data loader and the row template live in polling.js next to
   loadTunnels/ruleRowHtml; this module owns the pane render, the poll-safe patch, the create/edit
   sheet, the run-history sheet and the row actions. The rendering contract (main.js) applies in
   full: the 6s poll may only call patchJobs(), never rebuild the list — everything editable lives
   in the shared #sheet.

   Availability is probed once at boot: an older gateway serves no /api/jobs (404) and a disabled
   subsystem answers 503 with the row that turned it off — either way the tab hides itself,
   because a tab that can only toast an error every 6 seconds is noise, not a feature.
   ================================================================================================ */
import { $, api, apiJson, esc, state, toast, whenLabel } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { jobDotClass, jobRowHtml, jobsChipText, loadJobs } from "./polling.js";

var probed = null; // null = not probed yet; then the cached boolean answer for this page load

/** One boot probe. Raw api(), never apiJson: a 404/503 here is a feature signal, not an error
 *  worth a toast. A network failure assumes present — the first real load reports properly. */
async function probeJobs() {
  if (probed !== null) return probed;
  try { probed = (await api("/api/jobs")).ok; }
  catch (e) { probed = true; }
  if (!probed) {
    var b = document.querySelector('#viewSeg [data-view="jobs"]');
    if (b) b.hidden = true; // base.css [hidden]: the tab leaves the seg, and the hash gate in
  }                          // main.js waits for this probe before entering the view
  return probed;
}

/* --- render ----------------------------------------------------------------------------------- */

function renderJobs() {
  var rows = state.jobs.data;
  // The structural signature the poll compares against: any name added/removed/reordered means
  // the row set changed and a full rebuild is due; dots and labels alone never justify one.
  state.jobs.painted = rows.map(function (j) { return j.name; }).join("\n");
  var body = rows.length
    ? '<div class="group">' + rows.map(jobRowHtml).join("") + "</div>"
    : '<div class="empty"><div><h2>No jobs yet</h2><p class="hint">Scheduled commands the gateway runs on this machine. Add one with New.</p></div></div>';
  // .wide + .pane-head + .tun-foot: the Tunnels view's exact frame — the rows are the same
  // name-plus-subtext-plus-buttons shape, so they wear the same classes.
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><h1 class="pane-title">Jobs</h1>' +
      '<div class="pane-desc">Scheduled commands the gateway runs locally — an everySec interval or a 5-field cron in local time. One run at a time; overlapping fires are skipped, and every outcome lands in the run history.</div>' +
    "</div></div>" +
    '<div class="sec-head"><span class="sec-cap">Scheduled commands</span>' +
      '<span style="display:flex;gap:var(--s2)"><button class="btn primary" id="jobNew">New</button></span></div>' +
    body +
    '<div class="tun-foot" data-foot>' + esc(jobsChipText()) + "</div>" +
  "</div>";
  wireJobs();
}

/** Poll-safe update: dots, last/next labels, the Run-now button, the footer. Never structure. */
function patchJobs() {
  if (state.view !== "jobs") return;
  var pane = $("pane");
  var sig = state.jobs.data.map(function (j) { return j.name; }).join("\n");
  if (!pane.querySelector(".group") || sig !== state.jobs.painted) { renderJobs(); return; }
  Array.prototype.forEach.call(pane.querySelectorAll("[data-job]"), function (row) {
    var j = null;
    state.jobs.data.forEach(function (cand) { if (cand.name === row.getAttribute("data-job")) j = cand; });
    if (!j) return;
    var busy = state.jobs.busy[j.name];
    var dot = row.querySelector(".dot");
    if (dot) dot.className = "dot " + (busy ? "starting" : jobDotClass(j));
    // Rendered as (possibly empty) spans at build time so the patch can always fill them.
    var last = row.querySelector("[data-last]");
    if (last) last.textContent = j.lastRunAt
      ? "· last " + whenLabel(j.lastRunAt) + (j.lastOk === false ? " · failed" : "") : "";
    var next = row.querySelector("[data-next]");
    if (next) next.textContent = j.enabled && j.nextDueAt ? "· next " + whenLabel(j.nextDueAt) : "";
    var run = row.querySelector("[data-run]");
    if (run) { run.disabled = !!busy || !!j.running; run.textContent = busy ? "…" : "Run now"; }
  });
  var foot = pane.querySelector("[data-foot]");
  if (foot) foot.textContent = jobsChipText();
}

function jobByName(name) {
  var out = null;
  state.jobs.data.forEach(function (j) { if (j.name === name) out = j; });
  return out;
}

function wireJobs() {
  $("jobNew").onclick = function () { openJobSheet(null); };
  Array.prototype.forEach.call($("pane").querySelectorAll("[data-job]"), function (row) {
    var job = jobByName(row.getAttribute("data-job"));
    if (!job) return;
    row.querySelector("[data-run]").onclick = function () { void runJob(job.name); };
    row.querySelector("[data-hist]").onclick = function () { void openRunsSheet(job.name); };
    row.querySelector("[data-edit]").onclick = function () { openJobSheet(job); };
    row.querySelector("[data-del]").onclick = function () { deleteJob(job); };
  });
}

/* --- actions ---------------------------------------------------------------------------------- */

/** Run now waits for the outcome (the endpoint runs the job to completion, bounded by its own
 *  timeoutMs), so the row's button is disabled for the whole run — the poll keeps the rest live. */
async function runJob(name) {
  state.jobs.busy[name] = true;
  patchJobs();
  var j = await apiJson("/api/jobs/" + encodeURIComponent(name) + "/run", { method: "POST" });
  delete state.jobs.busy[name];
  if (j && j.run) {
    toast(name + ": " + (j.run.ok ? "ok" : "failed") + " in " + j.run.ms + " ms" +
      (j.run.error ? " — " + j.run.error : ""), !j.run.ok);
  }
  await loadJobs();
}

function deleteJob(job) {
  if (!confirm('Delete job "' + job.name + '"? The job stops; its run history stays on disk.')) return;
  void (async function () {
    var j = await apiJson("/api/jobs/" + encodeURIComponent(job.name), { method: "DELETE" });
    if (!j) return;
    toast("Deleted " + job.name);
    await loadJobs();
  })();
}

/* --- the create/edit sheet ---------------------------------------------------------------------- */

function openJobSheet(job) {
  var editing = !!job;
  var v = job || { name: "", command: "", everySec: "", cron: "", timeoutMs: "", cwd: "", enabled: true };
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit job" : "New job") + '">' +
      '<div class="sheet-head"><h2>' + (editing ? "Edit " + esc(job.name) : "New job") + "</h2></div>" +
      '<div class="sheet-body">' +
        // The name IS the identity (the PUT path is the job), so it is fixed once created.
        '<label class="field"><span>Name</span><input id="jf-name" value="' + esc(v.name || "") + '"' +
          (editing ? " disabled" : "") + ' placeholder="nightly-vacuum" autocomplete="off"></label>' +
        '<label class="field"><span>Command</span><input id="jf-command" value="' + esc(v.command) + '"' +
          ' placeholder="cmd /c backup.bat" autocomplete="off"></label>' +
        '<div class="hint">ARGV, not a shell line — pipes and redirections need "cmd /c …" (Windows) or "sh -c …" (Unix) around them. ${ENV} refs expand at run time.</div>' +
        '<div class="two">' +
          '<label class="field"><span>everySec</span><input id="jf-every" value="' + esc(v.everySec == null ? "" : String(v.everySec)) + '" placeholder="3600" autocomplete="off"></label>' +
          '<label class="field"><span>cron (local time)</span><input id="jf-cron" value="' + esc(v.cron || "") + '" placeholder="30 3 * * *" autocomplete="off"></label>' +
        "</div>" +
        '<div class="hint">Fill exactly one of the two: an interval, or a 5-field cron in local time.</div>' +
        '<div class="two">' +
          '<label class="field"><span>timeoutMs</span><input id="jf-timeout" value="' + esc(v.timeoutMs == null ? "" : String(v.timeoutMs)) + '" placeholder="600000" autocomplete="off"></label>' +
          '<label class="field"><span>Working directory</span><input id="jf-cwd" value="' + esc(v.cwd || "") + '" placeholder="Optional" autocomplete="off"></label>' +
        "</div>" +
        '<label class="check"><input type="checkbox" id="jf-enabled"' + (v.enabled ? " checked" : "") + ">Enabled</label>" +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="jf-cancel">Cancel</button>' +
        '<button class="btn primary" id="jf-save">' + (editing ? "Save" : "Add") + "</button></div>" +
    "</div>";
  $("sheet").hidden = false;
  $("jf-cancel").onclick = closeSheet;
  $("jf-save").onclick = function () { void saveJob(job); };
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeSheet(); };
  $("jf-command").focus();
}

async function saveJob(existing) {
  // On edit the name input is disabled, so it still carries the identity the URL needs.
  var name = $("jf-name").value.trim();
  var command = $("jf-command").value.trim();
  var every = $("jf-every").value.trim();
  var cron = $("jf-cron").value.trim();
  if (!name) { toast("Name is required", true); return; }
  if (!command) { toast("Command is required", true); return; }
  if (!every && !cron) { toast("Give everySec or cron — exactly one", true); return; }
  if (every && cron) { toast("everySec and cron are mutually exclusive", true); return; }
  if (every && !/^\d+$/.test(every)) { toast("everySec must be a whole number of seconds", true); return; }
  var body = { name: name, command: command, enabled: $("jf-enabled").checked };
  if (every) body.everySec = Number(every);
  if (cron) body.cron = cron;
  var t = $("jf-timeout").value.trim();
  if (t) {
    if (!/^\d+$/.test(t)) { toast("timeoutMs must be a whole number", true); return; }
    body.timeoutMs = Number(t);
  }
  var cwd = $("jf-cwd").value.trim();
  if (cwd) body.cwd = cwd;
  var j = await apiJson("/api/jobs/" + encodeURIComponent(name), { method: "PUT", body: JSON.stringify(body) });
  if (!j) return;
  closeSheet();
  toast((existing ? "Saved " : "Added ") + name);
  await loadJobs();
}

/* --- the run-history sheet ---------------------------------------------------------------------- */

/* The sheet pages through the JSONL history: each fetch appends to state.jobs.hist and the
   whole sheet re-renders from it, so "Load more" is just another fetch with a cursor. */
async function openRunsSheet(name, before) {
  if (!before || !state.jobs.hist || state.jobs.hist.name !== name) {
    state.jobs.hist = { name: name, runs: [] };
  }
  var j = await apiJson("/api/jobs/" + encodeURIComponent(name) + "/runs?limit=20" +
    (before ? "&before=" + encodeURIComponent(before) : ""));
  if (!j) return;
  state.jobs.hist.runs = state.jobs.hist.runs.concat(j.runs || []);
  renderRunsSheet(j.nextBefore || null);
}

function renderRunsSheet(nextBefore) {
  var hist = state.jobs.hist;
  var rows = hist.runs.map(function (r) {
    return '<div class="call">' +
      '<button class="call-sum" type="button">' +
        '<span class="dot ' + (r.ok ? "up" : "down") + '"></span>' +
        '<span class="call-tool">' + esc(whenLabel(r.at)) + "</span>" +
        '<span class="call-meta">' + esc(r.trigger || "") + " · " + esc(r.ms == null ? "?" : String(r.ms)) + " ms" +
          (r.exitCode != null ? " · exit " + esc(String(r.exitCode)) : "") +
          (r.timedOut ? " · timed out" : "") + "</span>" +
        '<span class="chev">&#9662;</span>' +
      "</button>" +
      '<div class="call-body" hidden><pre class="logs">' +
        esc((r.error ? r.error + "\n\n" : "") + (r.output || "") +
          (r.preview ? "\n… output tail-capped at 16 KB" : "")) +
      "</pre></div>" +
    "</div>";
  }).join("");
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Run history">' +
      '<div class="sheet-head"><h2>Runs — ' + esc(hist.name) + "</h2></div>" +
      '<div class="sheet-body">' +
        (rows || '<div class="rowmsg">No runs recorded yet — wait for the schedule, or press Run now.</div>') +
      "</div>" +
      '<div class="sheet-foot">' +
        (nextBefore ? '<button class="btn" id="jr-more">Load more</button>' : "") +
        '<button class="btn primary" id="jr-done">Done</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  $("jr-done").onclick = closeSheet;
  var more = $("jr-more");
  if (more) more.onclick = function () { void openRunsSheet(hist.name, nextBefore); };
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeSheet(); };
  // The Traffic expand idiom: clicking the summary toggles the raw output beneath it.
  Array.prototype.forEach.call($("sheet").querySelectorAll(".call-sum"), function (b) {
    b.onclick = function () {
      var body = b.parentElement.querySelector(".call-body");
      if (body) body.hidden = !body.hidden;
    };
  });
}

export { openRunsSheet, probeJobs, renderJobs, patchJobs };

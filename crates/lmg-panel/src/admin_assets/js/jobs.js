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
import { $, api, apiJson, esc, state, toast, whenLabel } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { jobDotClass, jobRowHtml, jobsChipText, loadJobs } from "./polling.js";
import { argFieldsHtml, readRunArgs } from "./run.js";
import { defTemplate, envToLines, formToV2, historyMeta, parseEnvLines, v2ToForm } from "./jobs-v2.js";

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
      '<div class="pane-desc">Scheduled commands the gateway runs locally — an interval or a 5-field cron in local time. Overlap, misfire and retry policies per job; every outcome lands in the run history.</div>' +
    "</div></div>" +
    '<div class="sec-head"><span class="sec-cap">Scheduled commands</span>' +
      '<span style="display:flex;gap:var(--s2)"><button class="btn" id="jobNewAdv">New (advanced)</button>' +
      '<button class="btn primary" id="jobNew">New</button></span></div>' +
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
      ? "\u00b7 last " + whenLabel(j.lastRunAt) + (j.lastOk === false ? " \u00b7 failed" : "") : "";
    var next = row.querySelector("[data-next]");
    if (next) next.textContent = j.enabled && j.nextDueAt ? "\u00b7 next " + whenLabel(j.nextDueAt) : "";
    var run = row.querySelector("[data-run]");
    if (run) { run.disabled = !!busy || !!j.running; run.textContent = busy ? "\u2026" : "Run now"; }
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
  $("jobNewAdv").onclick = function () { void openV2Sheet(null); };
  Array.prototype.forEach.call($("pane").querySelectorAll("[data-job]"), function (row) {
    var job = jobByName(row.getAttribute("data-job"));
    if (!job) return;
    row.querySelector("[data-run]").onclick = function () { void runJob(job.name); };
    row.querySelector("[data-hist]").onclick = function () { void openRunsSheet(job.name); };
    row.querySelector("[data-edit]").onclick = function () {
      // A definition the v1 shape cannot spell (docs/11 §7.1: editableInV1 false) goes
      // straight to the advanced sheet — the v1 form would silently drop its fields.
      if (job.editableInV1 === false) void openV2Sheet(job);
      else openJobSheet(job);
    };
    row.querySelector("[data-del]").onclick = function () { deleteJob(job); };
  });
}

/* --- actions ---------------------------------------------------------------------------------- */

/** Run now submits {"async": true} (docs/11 §7.3) and polls the coordinator's run view: the
 * run outlives the page, and the row stays live through the poll. The toast still names the
 * outcome, because the record settles into the history either way. */
async function runJob(name) {
  state.jobs.busy[name] = true;
  patchJobs();
  var j = await apiJson("/api/jobs/" + encodeURIComponent(name) + "/run", {
    method: "POST",
    body: JSON.stringify({ async: true }),
  });
  if (!j || j.runId == null) { delete state.jobs.busy[name]; patchJobs(); return; }
  var runId = j.runId;
  // Bounded poll: queued -> running -> terminal. The job's own timeoutMs bounds the run; the
  // poll gives up after 30 minutes and lets the history sheet tell the rest of the story.
  for (var i = 0; i < 1800; i++) {
    await new Promise(function (res) { setTimeout(res, 1000); });
    var v = await apiJson("/api/runs/" + runId);
    if (!v || !v.run) continue;
    if (v.run.state === "queued" || v.run.state === "running") continue;
    delete state.jobs.busy[name];
    var ok = v.run.state === "succeeded";
    toast(name + ": " + v.run.state + (v.run.ms != null ? " in " + v.run.ms + " ms" : "") +
      (v.run.error ? " \u2014 " + v.run.error : ""), !ok);
    await loadJobs();
    return;
  }
  delete state.jobs.busy[name];
  patchJobs();
  toast(name + " is still running \u2014 watch it in History", false);
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

/* --- the create/edit sheet (v1: the shape every field of this form can spell) ------------------- */

function openJobSheet(job) {
  var editing = !!job;
  var v = job || { name: "", command: "", everySec: "", cron: "", timeoutMs: "", cwd: "", env: {}, enabled: true };
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit job" : "New job") + '">' +
      '<div class="sheet-head"><h2>' + (editing ? "Edit " + esc(job.name) : "New job") + "</h2></div>" +
      '<div class="sheet-body">' +
        // The name IS the identity (the PUT path is the job), so it is fixed once created.
        '<label class="field"><span>Name</span><input id="jf-name" value="' + esc(v.name || "") + '"' +
          (editing ? " disabled" : "") + ' placeholder="nightly-vacuum" autocomplete="off"></label>' +
        '<label class="field"><span>Command</span><input id="jf-command" value="' + esc(v.command) + '"' +
          ' placeholder="cmd /c backup.bat" autocomplete="off"></label>' +
        '<div class="hint">ARGV, not a shell line — pipes and redirections need "cmd /c \u2026" (Windows) or "sh -c \u2026" (Unix) around them. ${ENV} refs expand at run time.</div>' +
        '<div class="two">' +
          '<label class="field"><span>everySec</span><input id="jf-every" value="' + esc(v.everySec == null ? "" : String(v.everySec)) + '" placeholder="3600" autocomplete="off"></label>' +
          '<label class="field"><span>cron (local time)</span><input id="jf-cron" value="' + esc(v.cron || "") + '" placeholder="30 3 * * *" autocomplete="off"></label>' +
        "</div>" +
        '<div class="hint">Fill exactly one of the two: an interval, or a 5-field cron in local time.</div>' +
        '<div class="two">' +
          '<label class="field"><span>timeoutMs</span><input id="jf-timeout" value="' + esc(v.timeoutMs == null ? "" : String(v.timeoutMs)) + '" placeholder="600000" autocomplete="off"></label>' +
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
  $("jf-advanced").onclick = function () { void openV2Sheet(job); };
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
  // The env box: every line a KEY=value the run will be handed. A bad line stops the
  // save — a job quietly running WITHOUT a variable the user believes it has is the
  // worst kind of wrong.
  var parsed = parseEnvLines($("jf-env").value);
  if (parsed.error) { toast(parsed.error, true); return; }
  if (Object.keys(parsed.env).length) body.env = parsed.env;
  var j = await apiJson("/api/jobs/" + encodeURIComponent(name), { method: "PUT", body: JSON.stringify(body) });
  if (!j) return;
  closeSheet();
  toast((existing ? "Saved " : "Added ") + name);
  await loadJobs();
}

/* --- the advanced sheet (v2: schema-driven form + JSON editor over the config row) -------------- */

/** The live action list, fetched once per sheet open: the action input form is generated from
 *  each capability's own schema (docs/10 §7), exactly like the Run view's argument fields. */
async function fetchActions() {
  var j = await apiJson("/api/actions");
  return (j && j.actions) || [];
}

function actionByType(actions, type) {
  for (var i = 0; i < actions.length; i++) if (actions[i].type === type) return actions[i];
  return null;
}

/** The Advanced editor. `job` null = create. Edits go through PUT /api/plugins/jobs/config
 * (docs/11 §7.2 — there is no second CRUD surface): GET the row, replace this one definition,
 * PUT the whole config back with the revision just read. The form writes only the keys it
 * owns onto the definition object it loaded, so unknown-but-legal fields ride along, and the
 * JSON textarea is always the definition itself — the two editors are one object. */
async function openV2Sheet(job) {
  var editing = !!job;
  var row = await apiJson("/api/plugins/jobs/config");
  if (!row) return;
  var config = row.config || {};
  var defs = config.definitions || {};
  var name = editing ? job.name : "";
  var base = editing ? defs[name] : defTemplate("new-job");
  var actions = await fetchActions();
  var form = v2ToForm(base);

  var actionOpts = actions.map(function (a) {
    return '<option value="' + esc(a.type) + '"' + (a.type === form.actionType ? " selected" : "") + ">" +
      esc(a.type) + (a.title && a.title !== a.type ? " \u2014 " + esc(a.title) : "") + "</option>";
  }).join("");
  var current = actionByType(actions, form.actionType);
  var inputFields = current
    ? argFieldsHtml({ inputSchema: current.schema }, "ja-", (base.action && base.action.input) || {})
    : '<div class="hint">This action type is not registered right now (its plugin is off) — edit its input in the JSON below.</div>';

  $("sheet").innerHTML =
    '<div class="sheet wide" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit definition" : "New definition") + '">' +
      '<div class="sheet-head"><h2>' + (editing ? "Definition \u2014 " + esc(name) : "New definition") + "</h2></div>" +
      '<div class="sheet-body">' +
        (editing ? "" : '<label class="field"><span>id</span><input id="jv-id" value="' + esc(name) + '" placeholder="nightly-vacuum" autocomplete="off"></label>') +
        '<div class="two">' +
          '<label class="field"><span>title</span><input id="jv-title" value="' + esc(form.title) + '" autocomplete="off"></label>' +
          '<label class="field"><span>labels (comma-separated)</span><input id="jv-labels" value="' + esc(form.labels) + '" placeholder="ops, nightly" autocomplete="off"></label>' +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>trigger</span><select id="jv-kind">' +
            ["interval", "cron", "manual"].map(function (k) {
              return '<option value="' + k + '"' + (form.kind === k ? " selected" : "") + ">" + k + "</option>";
            }).join("") + "</select></label>" +
          '<label class="field"><span>first firing</span><span class="hint">one schedule per definition</span></label>' +
        "</div>" +
        '<div class="two" id="jv-interval-row">' +
          '<label class="field"><span>everyMs</span><input id="jv-every" value="' + esc(form.everyMs) + '" placeholder="3600000" autocomplete="off"></label>' +
          '<label class="field"><span>firstRun</span><select id="jv-first">' +
            ["aligned", "immediate"].map(function (f) {
              return '<option value="' + f + '"' + (form.firstRun === f ? " selected" : "") + ">" + f + "</option>";
            }).join("") + "</select></label>" +
        "</div>" +
        '<label class="field" id="jv-cron-row"><span>cron (local time)</span><input id="jv-cron" value="' + esc(form.cron) + '" placeholder="30 3 * * *" autocomplete="off"></label>' +
        '<div class="two">' +
          '<label class="field"><span>timeoutMs</span><input id="jv-timeout" value="' + esc(form.timeoutMs) + '" placeholder="600000" autocomplete="off"></label>' +
          '<label class="check"><input type="checkbox" id="jv-disabled"' + (form.disabled ? " checked" : "") + ">Disabled</label>" +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>overlap</span><select id="jv-overlap">' +
            ["skip", "queue-one"].map(function (o) {
              return '<option value="' + o + '"' + (form.overlap === o ? " selected" : "") + ">" + o + "</option>";
            }).join("") + "</select></label>" +
          '<label class="field"><span>misfire</span><select id="jv-misfire">' +
            ["skip", "run-once"].map(function (m) {
              return '<option value="' + m + '"' + (form.misfire === m ? " selected" : "") + ">" + m + "</option>";
            }).join("") + "</select></label>" +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>retry.maxAttempts</span><input id="jv-rmax" value="' + esc(form.retryMax) + '" placeholder="1" autocomplete="off"></label>' +
          '<label class="field"><span>retry.delayMs</span><input id="jv-rdelay" value="' + esc(form.retryDelayMs) + '" placeholder="0" autocomplete="off"></label>' +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>retry.backoff</span><select id="jv-rback">' +
            ["fixed", "exponential"].map(function (b) {
              return '<option value="' + b + '"' + (form.retryBackoff === b ? " selected" : "") + ">" + b + "</option>";
            }).join("") + "</select></label>" +
          '<span class="field"><span>retry.retryOn</span><span style="display:flex;gap:var(--s2)">' +
            ["failure", "timeout"].map(function (r) {
              return '<label class="check"><input type="checkbox" data-retryon="' + r + '"' +
                (form.retryOn.indexOf(r) >= 0 ? " checked" : "") + ">" + r + "</label>";
            }).join("") + "</span></span>" +
        "</div>" +
        '<div class="two">' +
          '<label class="field"><span>output.capture</span><select id="jv-capture">' +
            ["tail", "none"].map(function (c) {
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
  var def = JSON.parse(JSON.stringify(base));
  var dirty = false;
  var jsonBox = $("jv-json");
  jsonBox.value = JSON.stringify(def, null, 2) + "\n";

  function syncTriggerRows() {
    var kind = $("jv-kind").value;
    $("jv-interval-row").style.display = kind === "interval" ? "" : "none";
    $("jv-cron-row").style.display = kind === "cron" ? "" : "none";
  }
  syncTriggerRows();
  $("jv-kind").onchange = syncTriggerRows;

  // Switching the action type rebuilds the input form from that capability's schema.
  $("jv-action").onchange = function () {
    var next = actionByType(actions, $("jv-action").value);
    $("jv-inputs").innerHTML = next
      ? argFieldsHtml({ inputSchema: next.schema }, "ja-", {})
      : '<div class="hint">Not registered — edit the input in the JSON below.</div>';
  };

  function formValues() {
    var retryOn = [];
    Array.prototype.forEach.call(document.querySelectorAll("[data-retryon]"), function (cb) {
      if (cb.checked) retryOn.push(cb.getAttribute("data-retryon"));
    });
    return {
      title: $("jv-title").value.trim(),
      labels: $("jv-labels").value,
      disabled: $("jv-disabled").checked,
      kind: $("jv-kind").value,
      everyMs: $("jv-every").value.trim(),
      firstRun: $("jv-first").value,
      cron: $("jv-cron").value.trim(),
      timeoutMs: $("jv-timeout").value.trim(),
      overlap: $("jv-overlap").value,
      misfire: $("jv-misfire").value,
      retryMax: $("jv-rmax").value.trim(),
      retryDelayMs: $("jv-rdelay").value.trim(),
      retryBackoff: $("jv-rback").value,
      retryOn: retryOn,
      capture: $("jv-capture").value,
      maxBytes: $("jv-maxbytes").value.trim(),
      actionType: $("jv-action").value,
    };
  }

  function formToDef() {
    var currentAction = actionByType(actions, $("jv-action").value);
    var input = currentAction
      ? readRunArgs({ inputSchema: currentAction.schema }, "ja-")
      : (def.action && def.action.input) || {};
    return formToV2(formValues(), def, input);
  }

  $("jv-form-to-json").onclick = function () {
    try {
      def = formToDef();
      jsonBox.value = JSON.stringify(def, null, 2) + "\n";
      dirty = false;
    } catch (e) {
      toast(String(e.message || e), true);
    }
  };

  jsonBox.oninput = function () { dirty = true; };

  $("jv-cancel").onclick = closeSheet;
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeSheet(); };
  $("jv-save").onclick = function () { void save(); };

  async function save() {
    var id = editing ? name : $("jv-id").value.trim();
    if (!id || !/^[\w.-]+$/.test(id)) { toast("id is required (letters, digits, -, _, .)", true); return; }
    if (dirty) {
      // The JSON textarea is the source of truth once hand-edited.
      try { def = JSON.parse(jsonBox.value); }
      catch (e) { toast("definition JSON does not parse: " + (e.message || e), true); return; }
    } else {
      // Form -> def (and the box): that click IS what save means for the form path. A read
      // error here toasts on its own; the dirty flag stays set, so save stops for a fix.
      $("jv-form-to-json").onclick();
      if (dirty) return;
    }
    // A fresh GET right before the PUT: the whole-row CAS (docs/11 §7.2) wants the newest
    // revision, and a concurrent edit anywhere in the row must not be silently overwritten.
    var fresh = await apiJson("/api/plugins/jobs/config");
    if (!fresh) return;
    var cfg = fresh.config || {};
    var definitions = cfg.definitions || {};
    definitions[id] = def;
    cfg.definitions = definitions;
    var reply = await apiJson("/api/plugins/jobs/config", {
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
async function openRunsSheet(name, cursor) {
  if (!cursor || !state.jobs.hist || state.jobs.hist.name !== name) {
    state.jobs.hist = { name: name, runs: [] };
  }
  var j = await apiJson("/api/jobs/" + encodeURIComponent(name) + "/runs?limit=20" +
    (cursor ? "&cursor=" + encodeURIComponent(cursor) : ""));
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
        '<span class="call-meta">' + esc(historyMeta(r)) + "</span>" +
        '<span class="chev">&#9662;</span>' +
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

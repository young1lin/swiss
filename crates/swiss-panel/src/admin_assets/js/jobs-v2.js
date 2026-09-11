/* ================================================================================================
   Jobs v2 — the pure helpers behind the S6 panel (docs/10 §5, docs/11 §7).

   No DOM in here: everything is data in, data out, so the round-trip guarantees the config
   editor needs (a form edit must not drop fields the form does not know) are testable in
   vitest without a browser. jobs.js renders; this module decides.
   ================================================================================================ */

/** The row's schedule text (docs/11 §3.3): one line naming when the job fires. Reads the
 * v2 `trigger` object with a fallback to the v1 flat fields, so the same row renders on a
 * gateway that predates the v2 listing. */
function triggerSummary(j) {
  var t = j.trigger;
  if (!t || !t.kind) {
    // v1 spelling: everySec | cron directly on the row.
    return j.cron ? "cron " + j.cron : j.everySec ? "every " + j.everySec + " s" : "no schedule";
  }
  if (t.kind === "cron") return "cron " + (t.expression || "");
  if (t.kind === "interval") {
    var secs = t.everyMs != null && t.everyMs % 1000 === 0 ? t.everyMs / 1000 : t.everyMs + " ms";
    return "every " + secs + " s" + (t.firstRun === "immediate" ? " \u00b7 immediate" : "");
  }
  return "manual";
}

/** The history meta line for one run record (docs/11 §7.3): what happened, in one glance.
 * Outcome records say why they are not runs; attempts carry their retry position. */
function historyMeta(r) {
  var parts = [r.trigger || "?"];
  if (r.outcome && r.outcome !== "ran") {
    parts.push(r.outcome + (r.reason ? " (" + r.reason + ")" : ""));
    if (r.missedCount != null) parts.push(r.missedCount + " more missed");
  } else {
    if (r.attempt != null && r.attempts != null && r.attempts > 1) parts.push("attempt " + r.attempt + "/" + r.attempts);
    parts.push((r.ms == null ? "?" : String(r.ms)) + " ms");
    if (r.exitCode != null) parts.push("exit " + r.exitCode);
  }
  if (r.timedOut) parts.push("timed out");
  if (r.canceled) parts.push("canceled");
  if (r.error) parts.push(String(r.error).slice(0, 120));
  return parts.join(" \u00b7 ");
}

/** A fresh definition for the JSON editor's "New (advanced)" sheet — the minimum the
 * config validator accepts, in config spelling (docs/11 §3.2). */
function defTemplate(id) {
  return {
    title: id,
    trigger: { kind: "interval", everyMs: 3600000, firstRun: "aligned" },
    action: { type: "process.legacy-command", input: { command: "" } },
    timeoutMs: 600000,
  };
}

/** Definition \u2192 flat form values, every one a string the inputs can hold (except the
 * checkbox-shaped ones: disabled is boolean, retryOn is the checked-name list). Unknown
 * definition keys are NOT read here; they survive through the `base` object formToV2
 * writes onto. */
function v2ToForm(def) {
  var t = def.trigger || {};
  var retry = def.retry || {};
  var output = def.output || {};
  return {
    title: def.title || "",
    labels: (def.labels || []).join(", "),
    disabled: !!def.disabled,
    kind: t.kind || "interval",
    everyMs: t.everyMs == null ? "" : String(t.everyMs),
    firstRun: t.firstRun || "aligned",
    cron: t.expression || "",
    timeoutMs: def.timeoutMs == null ? "" : String(def.timeoutMs),
    overlap: def.overlap || "skip",
    misfire: def.misfire || "skip",
    retryMax: retry.maxAttempts == null ? "" : String(retry.maxAttempts),
    retryDelayMs: retry.delayMs == null ? "" : String(retry.delayMs),
    retryBackoff: retry.backoff || "fixed",
    retryOn: retry.retryOn || ["failure"],
    capture: output.capture || "tail",
    maxBytes: output.maxBytes == null ? "" : String(output.maxBytes),
    actionType: (def.action && def.action.type) || "",
  };
}

/** Deep-copy a plain JSON value (definitions are plain JSON by contract). */
function cloneJson(v) {
  return v == null ? v : JSON.parse(JSON.stringify(v));
}

/** Form values \u2192 the definition to save. Starts from `base` — the definition as the
 * config row holds it, unknown keys and all — and writes ONLY the keys this form owns, so
 * a field a future gateway understands survives an edit made by this panel (docs/10 §5:
 * losing a key here deletes configuration). Keys the form leaves at their default are
 * written explicitly: explicit defaults parse identically and keep the JSON editor honest. */
function formToV2(form, base, actionInput) {
  var def = cloneJson(base) || {};
  if (form.title) def.title = form.title; else delete def.title;
  var labels = form.labels.split(",").map(function (s) { return s.trim(); }).filter(Boolean);
  if (labels.length) def.labels = labels; else delete def.labels;
  if (form.disabled) def.disabled = true; else delete def.disabled;
  if (form.kind === "cron") {
    def.trigger = { kind: "cron", expression: form.cron, timezone: "local" };
  } else if (form.kind === "manual") {
    def.trigger = { kind: "manual" };
  } else {
    def.trigger = { kind: "interval", everyMs: Number(form.everyMs) || 0, firstRun: form.firstRun };
  }
  if (form.timeoutMs !== "") def.timeoutMs = Number(form.timeoutMs) || 0; else delete def.timeoutMs;
  def.overlap = form.overlap;
  def.misfire = form.misfire;
  var max = Number(form.retryMax) || 0;
  if (max > 1) {
    def.retry = {
      maxAttempts: max,
      delayMs: Number(form.retryDelayMs) || 0,
      backoff: form.retryBackoff,
      retryOn: form.retryOn.length ? form.retryOn : ["failure"],
    };
  } else {
    delete def.retry; // one attempt is the default spelling: absent, not a zeroed object
  }
  if (form.capture === "none" || form.maxBytes !== "") {
    def.output = { capture: form.capture };
    if (form.maxBytes !== "") def.output.maxBytes = Number(form.maxBytes) || 0;
  } else {
    delete def.output;
  }
  def.action = { type: form.actionType, input: actionInput || {} };
  return def;
}

export { cloneJson, defTemplate, envToLines, formToV2, historyMeta, parseEnvLines, triggerSummary, v2ToForm };

/* --- the v1 form's environment-variables box -----------------------------------------------
 * KEY=value lines in a textarea <-> the env object the PUT body and the action input
 * carry. Pure so the parsing rules are pinned by tests, not by typing into the sheet. */

/** env object -> "KEY=value" lines, in the object's own key order. */
function envToLines(env) {
  if (!env || typeof env !== "object") return "";
  return Object.keys(env)
    .map(function (k) { return k + "=" + env[k]; })
    .join("\n");
}

/** "KEY=value" lines -> { env, error }. Blank lines are skipped; a line without "=", an
 * empty key, or a key containing "=" or NUL is an error the form shows verbatim — a
 * silent drop here would mean a job that runs WITHOUT a variable the user believes it
 * has, which is the worst kind of wrong. */
function parseEnvLines(text) {
  var env = {};
  var lines = String(text == null ? "" : text).split(/\r?\n/);
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i].trim();
    if (!line) continue;
    var eq = line.indexOf("=");
    if (eq <= 0) {
      return { env: null, error: "line " + (i + 1) + ": expected KEY=value, got \"" + line + "\"" };
    }
    var key = line.slice(0, eq);
    if (key.indexOf("\u0000") >= 0) {
      return { env: null, error: "line " + (i + 1) + ": the key contains a NUL character" };
    }
    env[key] = line.slice(eq + 1);
  }
  return { env: env, error: "" };
}

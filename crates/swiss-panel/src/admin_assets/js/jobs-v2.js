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
   Jobs v2 — the pure helpers behind the S6 panel (docs/10 §5, docs/11 §7).

   No DOM in here: everything is data in, data out, so the round-trip guarantees the config
   editor needs (a form edit must not drop fields the form does not know) are testable in
   vitest without a browser. jobs.js renders; this module decides.
   ================================================================================================ */

/** The row's schedule text (docs/11 §3.3): one line naming when the job fires. Reads the
 * v2 `trigger` object with a fallback to the v1 flat fields, so the same row renders on a
 * gateway that predates the v2 listing. */
                                                                 
                                                                                                                                                                   
import { tr } from "./i18n.js";

/* The Advanced sheet's select options - the closed enums swiss-jobs/src/jobs/def.rs
 * enum_str accepts, in the order the selects show them. Rendering FROM these lists is what
 * keeps the form unable to offer a value the server refuses (docs/37 M9). */
const JOB_TRIGGER_KINDS                            = ["interval", "cron", "manual"];
const JOB_FIRST_RUNS                         = ["after-interval", "immediate"];
const JOB_OVERLAPS                        = ["skip", "queue-one"];
const JOB_MISFIRES                        = ["skip", "run-once"];
const JOB_BACKOFFS                        = ["fixed", "exponential"];
const JOB_RETRY_ONS                        = ["failure", "timeout"];
const JOB_CAPTURES                        = ["tail", "none"];

/** A select's value narrowed to its own option list. A select can only hold one of the
 * options the sheet rendered from `legal`, so a miss is a panel bug, and it throws the way
 * a malformed form value does - formToJson toasts it. */
function legalOf                  (value        , legal              , field        )    {
  if ((legal                     ).includes(value)) return value     ;
  throw new Error(field + " must be one of " + legal.join(", ") + ", got " + JSON.stringify(value));
}

function triggerSummary(j           )         {
  const t = j.trigger;
  if (!t || !t.kind) {
    // v1 spelling: everySec | cron directly on the row.
    return j.cron ? "cron " + j.cron : j.everySec ? "every " + j.everySec + " s" : "no schedule";
  }
  if (t.kind === "cron") return "cron " + (t.expression || "");
  if (t.kind === "interval") {
    const secs = t.everyMs != null && t.everyMs % 1000 === 0 ? t.everyMs / 1000 : t.everyMs + " ms";
    return "every " + secs + " s" + (t.firstRun === "immediate" ? " \u00b7 immediate" : "");
  }
  return "manual";
}

/** The history meta line for one run record (docs/11 §7.3): what happened, in one glance.
 * Outcome records say why they are not runs; attempts carry their retry position. */
function historyMeta(r                                                                                                     )         {
  const parts = [r.trigger || "?"];
  if (r.outcome && r.outcome !== "ran") {
    parts.push(r.outcome + (r.reason ? " (" + r.reason + ")" : ""));
    if (r.missedCount != null) parts.push(tr("{n} more missed", { n: r.missedCount }));
  } else {
    if (r.attempt != null && r.attempts != null && r.attempts > 1) parts.push(tr("attempt {a}/{b}", { a: r.attempt, b: r.attempts }));
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
function defTemplate(id        )         {
  return {
    title: id,
    trigger: { kind: "interval", everyMs: 3600000, firstRun: "after-interval" },
    action: { type: "process.legacy-command", input: { command: "" } },
    timeoutMs: 600000,
  };
}

/** Definition \u2192 flat form values, every one a string the inputs can hold (except the
 * checkbox-shaped ones: disabled is boolean, retryOn is the checked-name list). A slot the
 * definition leaves out reads as the server's default for it (def.rs), which is what an
 * absent key means on the wire. */
function v2ToForm(def        )                {
  const t                      = def.trigger || {};
  const retry                               = def.retry || {};
  const output                                = def.output || {};
  return {
    title: def.title || "",
    labels: (def.labels || []).join(", "),
    disabled: !!def.disabled,
    kind: t.kind || "interval",
    everyMs: t.everyMs == null ? "" : String(t.everyMs),
    firstRun: t.firstRun || "after-interval",
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
function cloneJson(v         )          {
  return v == null ? v : JSON.parse(JSON.stringify(v));
}

/** Form values \u2192 the definition to save. Starts from `base` — the definition as the
 * config row holds it, unknown keys and all — and writes ONLY the keys this form owns, so
 * a field a future gateway understands survives an edit made by this panel (docs/10 §5:
 * losing a key here deletes configuration). Keys the form leaves at their default are
 * written explicitly: explicit defaults parse identically and keep the JSON editor honest. */
function formToV2(form               , base        , actionInput                         )         {
  const def = cloneJson(base)           || {};
  if (form.title) def.title = form.title; else delete def.title;
  const labels = form.labels.split(",").map((s        )         => { return s.trim(); }).filter(Boolean);
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
  const max = Number(form.retryMax) || 0;
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

export { JOB_BACKOFFS, JOB_CAPTURES, JOB_FIRST_RUNS, JOB_MISFIRES, JOB_OVERLAPS, JOB_RETRY_ONS, JOB_TRIGGER_KINDS, cloneJson, defTemplate, envToLines, formToV2, historyMeta, legalOf, parseEnvLines, triggerSummary, v2ToForm };

/* --- the v1 form's environment-variables box -----------------------------------------------
 * KEY=value lines in a textarea <-> the env object the PUT body and the action input
 * carry. Pure so the parsing rules are pinned by tests, not by typing into the sheet. */

/** env object -> "KEY=value" lines, in the object's own key order. */
function envToLines(env                                           )         {
  if (!env || typeof env !== "object") return "";
  return Object.keys(env)
    .map((k        )         => { return k + "=" + env[k]; })
    .join("\n");
}

/** "KEY=value" lines -> { env, error }. Blank lines are skipped; a line without "=", an
 * empty key, or a key containing "=" or NUL is an error the form shows verbatim — a
 * silent drop here would mean a job that runs WITHOUT a variable the user believes it
 * has, which is the worst kind of wrong. */
function parseEnvLines(text        )                                                        {
  const env                         = {};
  const lines = String(text == null ? "" : text).split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (!line) continue;
    const eq = line.indexOf("=");
    if (eq <= 0) {
      return { env: null, error: "line " + (i + 1) + ": expected KEY=value, got \"" + line + "\"" };
    }
    const key = line.slice(0, eq);
    if (key.includes("\u0000")) {
      return { env: null, error: "line " + (i + 1) + ": the key contains a NUL character" };
    }
    env[key] = line.slice(eq + 1);
  }
  return { env: env, error: "" };
}

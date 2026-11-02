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
   Memory - the sync sheet (SPEC §memory.sync), opened from the projects rail.

   Commits and Claude Code or DSH conversations become memory: the sheet shows where the project
   stands (the synced head, what waits), the last runs with their verdicts, and edits the
   plugin row's `sync` block - a GET for the revision, then the CAS PUT, the plugin applies it
   in place and the worker wakes at once. Sync now queues a manual run; Dry run queues one
   that keeps the proposal for review and applies nothing.

   Who does the work is an agent from the llm plugin (SPEC §llm.agents), named here - its
   runtime, model and key live on that plugin's Agents page, so the sheet only picks one.

   Draw map (SPEC §memory.map) queues a map run instead: the checkout cut into areas from the
   code index, the agent asked one bounded question per area, the answers filled into domain
   hubs. Preview lists the areas without asking anyone; areas already on the map are kept.
   Redraw first removes the domains an earlier map drew that nobody touched since.

   Initialize (SPEC §memory.init) queues a full run: the map, then every source file read in
   slices and the agent asked what each teaches - the notes land in the semantic index when an
   embedding model is set, lexical search only without. Its preview counts the slices and says
   the index; while a run is queued or running the sheet follows it, saying the phase it is in.

   The lead (SPEC §memory.init Lead) is a second agent picked beside the first: it reviews the
   map's domains and wraps up each domain a full run wrote in; none leaves the agent alone.

   Stop (SPEC §memory.sync Cancel) sits on a queued or running run: a queued one never starts, a
   running one ends cancelled at once - the questions in flight dropped - and keeps the notes it saved - the next
   Initialize skips the slices it answered. The plugin itself stays on.
   ================================================================================================ */
import { api, toast } from "../util.js";
                                      
import { fill, h } from "../h.js";
import { locale, tk, tr } from "../i18n.js";
import { actionBtn, btn, card, checkField, checkGrid, closeSheet, dot, field, flow, flowToggle, formFold, hint, inlineForm, kvRow, pair, setActionBusy, sheet, showSheet, tag } from "../ui/index.js";
                                                         
                                                                                                                                                            
                                                   
import { ageText, errMessage, req, sizeText } from "./memory-core.js";
import { call, loadStatus, runtimeWord, showRun, taskAgents } from "./llm-core.js";
import { runBrief, runLead, runWhat } from "./llm-history.js";
                                                  

const CONFIG_PATH = "/api/plugins/memory/config";

export const STATE_TONE                                                     = {
  ok: null, empty: null, queued: "warn", running: "warn", failed: "bad", cancelled: null,
};

export const STATE_KEY                                      = {
  ok: tk("memory.sync.run.ok"), empty: tk("memory.sync.run.empty"), queued: tk("memory.sync.run.queued"),
  running: tk("memory.sync.run.running"), failed: tk("memory.sync.run.failed"), cancelled: tk("memory.sync.run.cancelled"),
};

/** A run's state as the dot that leads its row: a ring while it waits, a pulse while it runs. */
const STATE_DOT                                        = {
  ok: "up", empty: "off", queued: "idle", running: "starting", failed: "down", cancelled: "off",
};

const PHASE_KEY                               = {
  review: tk("memory.sync.run.phase.review"), areas: tk("memory.sync.run.phase.areas"),
  knowledge: tk("memory.sync.run.phase.knowledge"), wrapup: tk("memory.sync.run.phase.wrapup"),
  recheck: tk("memory.sync.run.phase.recheck"), docs: tk("memory.sync.run.phase.docs"),
  ask: tk("memory.sync.run.phase.ask"), apply: tk("memory.sync.run.phase.apply"),
};

/** How many filed agent runs a sync run's list shows beside the ones still running. */
const AGENT_RUNS_SHOWN = 6;

/** The agent runs each sync run started (SPEC §llm.history Live), by sync run id: the rows
 *  read, or why the read failed; `shown` holds the runs whose list is on the sheet. */
                                                                                          

/** How often the open sheet re-reads a queued or running run. */
const FOLLOW_MS = 3000;

/** The questions one full run or recheck asks when the cap is left empty (SPEC §memory.init maxAsks). */
const MAX_ASKS = 200;

const TRIGGER_KEY                                        = {
  manual: tk("memory.sync.run.manual"), nudge: tk("memory.sync.run.nudge"), map: tk("memory.sync.run.map"), full: tk("memory.sync.run.full"),
  recheck: tk("memory.sync.run.recheck"), docs: tk("memory.sync.run.docs"), review: tk("memory.sync.run.review"),
};

/** The plugin row through the host's config API: a non-2xx is the host's {error} sentence. */
export async function configReq   (method               , body          )                                           {
  try {
    const r = await api(CONFIG_PATH, body === undefined ? { method } : { method, body: JSON.stringify(body) });
    const j          = await r.json().catch(()          => null);
    if (r.ok) return { data: j     , err: "" };
    const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : "";
    return { data: null, err: msg || tr("util.httpN", { n: r.status }) };
  } catch {
    return { data: null, err: tr("util.requestFailedGatewayRunning") };
  }
}

/** Which layer said whom a run asks (SPEC §memory.settings): the word, or none when no
 *  layer did. */
const FROM_KEY                         = {
  run: tk("memory.sync.from.run"), project: tk("memory.sync.from.project"), plugin: tk("memory.sync.from.plugin"),
  newest: tk("memory.sync.from.newest"), "": tk("memory.sync.from.none"),
};

export function fromWord(from                           )         {
  return tr(FROM_KEY[from || ""] || from || "");
}

/** What an agents pick reads as: inherit (absent), off, or the names picked. */
;                                                                        

;                         
                    
                          
                                                                                         
                                                             
                         
  

/** One agents pick (SPEC §memory.settings): inherit (absent - the layers below decide),
 *  off, or one or more names as a multi-select. A name the llm row no longer holds stays
 *  shown and checked, so an untouched save never drops it. */
export function agentsPick(o                                                                                                        )             {
  const held = Array.isArray(o.value) ? o.value : o.value && o.value !== "off" ? [o.value] : [];
  // The wanted mode is assigned after the options exist: a "selected" prop on a detached
  // option does not survive every DOM.
  const modeValue = o.value === "off" ? "off" : held.length > 0 ? "named" : "inherit";
  const mode = h("select", { id: o.id + "Mode" },
    h("option", { value: "inherit" }, tr("memory.sync.pick.inherit")),
    h("option", { value: "off" }, tr("memory.sync.block.agent.off")),
    h("option", { value: "named" }, o.namedLabel));
  mode.value = modeValue;
  const box = (name        )                   => {
    const input = h("input", { type: "checkbox", checked: held.includes(name) });
    input.dataset.magent = name;
    return input;
  };
  const grid = checkGrid({ id: o.id + "Names" },
    ...o.agents.map((a) => checkField({ label: a.name + " · " + runtimeWord(a.runtime), control: box(a.name) })),
    ...held.filter((n) => !o.agents.some((a) => a.name === n))
      .map((n) => checkField({ label: tr("memory.sync.block.agent.missing", { name: n }), control: box(n) })));
  const paint = ()       => { grid.hidden = mode.value !== "named"; };
  mode.addEventListener("change", paint);
  paint();
  return {
    node: h("div", { id: o.id }, mode, grid),
    mode,
    read: ()             => {
      if (mode.value === "off") return { kind: "off", names: [] };
      if (mode.value === "inherit") return { kind: "inherit", names: [] };
      const names = [...grid.querySelectorAll                  ("input[data-magent]")].filter((b) => b.checked).map((b) => b.dataset.magent || "");
      return { kind: "named", names };
    },
  };
}

/** One lead pick: inherit (absent - the project's settings, else none), off, or a name. */
function leadPick(o                                                                         )                                                                      {
  const named = o.value && o.value !== "off" ? o.value : "";
  const sel = h("select", { id: o.id },
    h("option", { value: "inherit" }, tr("memory.sync.pick.leadInherit")),
    h("option", { value: "off" }, tr("memory.sync.block.lead.off")),
    ...o.agents.map((a) => h("option", { value: a.name }, a.name + " · " + runtimeWord(a.runtime))),
    named && !o.agents.some((a) => a.name === named) ? h("option", { value: named }, tr("memory.sync.block.agent.missing", { name: named })) : null);
  sel.value = o.value === "off" ? "off" : named || "inherit";
  return { node: sel, read: () => (sel.value === "inherit" ? "inherit" : sel.value === "off" ? "off" : sel.value) };
}

/** A commit hash as the sheet says it: the first 7, blank while none is synced. */
function short(hash                           )         {
  return hash ? hash.slice(0, 7) : "-";
}

/** One agent as the sheet says it: up with its runtime and model, or the sentence a run
 *  would fail with. */
export function agentNode(a              )           {
  return a.error
    ? [dot("error", a.error), " ", a.name]
    : [dot("up", a.name), " ", a.name, " · ", runtimeWord(a.runtime || ""), a.model ? " · " + a.model : null];
}

/** Where the project stands, and the runs under it. */
function statusNode(o                 )              {
  // The workers a run would ask now (§memory.settings), and which layer decided - an
  // older gateway answers only the first worker, and the sheet still says it.
  const chosenAgents = o.chosen && o.chosen.agents;
  const workersValue         = o.workers
    ? o.workers.length
      ? [...o.workers.flatMap((w, i) => [...(i ? [" + "] : []), agentNode(w)]),
        chosenAgents ? [" · ", fromWord(chosenAgents.from)] : null]
      : [dot("off", tr("memory.sync.agent.none")), " ", tr("memory.sync.agent.none"),
        chosenAgents ? [" · ", fromWord(chosenAgents.from)] : null]
    : !o.agent
      ? [dot("off", tr("memory.sync.agent.none")), " ", tr("memory.sync.agent.none")]
      : agentNode(o.agent);
  const chosenLead = o.chosen && o.chosen.lead;
  const leadValue         = o.lead
    ? [...agentNode(o.lead), chosenLead ? [" · ", fromWord(chosenLead.from)] : null]
    : [tr("memory.sync.agent.none"), chosenLead ? [" · ", fromWord(chosenLead.from)] : null];
  const rows = [
    kvRow(tr("memory.sync.workers"), workersValue),
    o.chosen ? kvRow(tr("memory.sync.lead"), leadValue) : null,
    kvRow(tr("memory.sync.head"), short(o.state.head), { mono: true }),
    kvRow(tr("memory.sync.pending"), [
      tr("memory.sync.pendingN", { n: o.state.pendingCommits }),
      o.state.pendingBytes ? " · " + tr("memory.sync.pendingTalk", { size: sizeText(o.state.pendingBytes) }) : null,
    ]),
    kvRow(tr("memory.sync.lastRun"), o.state.lastFinishedMs ? ageText(o.state.lastFinishedMs) : tr("memory.sync.never")),
  ];
  return h("div", { id: "memSyncStatus" },
    card(rows),
    o.configError ? hint(tr("memory.sync.configError", { error: o.configError }), { bad: true }) : null,
    ...workerErrors(o));
}

/** The sentences a run would fail with, one hint per worker that carries one. */
function workerErrors(o                 )           {
  const errs = (o.workers || []).filter((w) => w.error).map((w) => hint(tr("memory.sync.agentError", { error: w.name + ": " + w.error }), { bad: true }));
  return !o.workers && o.agent && o.agent.error ? [hint(tr("memory.sync.agentError", { error: o.agent.error }), { bad: true })] : errs;
}

/** A map or full run's areas described against those asked about, while it runs and after. */
function described(r            )         {
  return (r.trigger === "map" || r.trigger === "full") && r.scope && r.scope.asks
    ? tr("memory.sync.run.described", { filled: r.scope.filled || 0, asks: r.scope.asks })
    : "";
}

/** The semantic index in one phrase: off is lexical search only. */
function embeddingText(e                               )         {
  if (!e || e.state === "off") return tr("memory.sync.embed.off");
  const base = tr("memory.sync.embed.on", { state: e.state, model: e.model || "?", indexed: e.indexed, pending: e.pending });
  return e.error ? base + " · " + e.error : base;
}

/** A full run's knowledge phase: the notes saved from the slices asked, and the index. */
function knowledgeNode(r            )                     {
  const k = r.scope && r.scope.knowledge;
  if (r.trigger !== "full" || !k) return null;
  const node = hint([
    tr("memory.sync.run.knowledge", { saved: k.saved, held: k.held, known: k.duplicates, asked: k.asked, slices: k.slices }),
    k.more ? [" · ", tr("memory.sync.run.more", { n: k.more })] : null,
    k.failed ? [" · ", tr("memory.sync.run.slicesFailed", { n: k.failed })] : null,
    k.embedding ? [" · ", embeddingText(k.embedding)] : null]);
  node.dataset.mknow = r.id;
  return node;
}

/** A full run's wrap-up by the lead: the domains wrapped up, the hubs refreshed, the notes. */
function wrapupNode(r            )                     {
  const w = r.scope && r.scope.wrapup;
  if (r.trigger !== "full" || !w) return null;
  const node = hint([
    tr("memory.sync.run.wrapup", { wrapped: w.wrapped, domains: w.domains, hubs: w.hubs, saved: w.saved, held: w.held }),
    w.failed ? [" · ", tr("memory.sync.run.wrapupFailed", { n: w.failed })] : null]);
  node.dataset.mwrap = r.id;
  return node;
}

/** Whether the run reviews only the notes whose code changed - the daily review's kind. */
export function changedReview(r            )          {
  return r.trigger === "review" && !!(r.scope && r.scope.changed);
}

/** A recheck's count - a full run closes with one too: the notes judged and what came of them,
 *  then the contradictions it lifted, the rounds it asked again to settle them and those still
 *  open, when any. A dry recheck judged nothing and says what it would ask; one that found no
 *  note says so. A review run (the inbox handed over) says the same, with what it confirmed; one
 *  of the notes whose code changed says that is what it judged. */
export function recheckNode(r            )                     {
  const c = r.scope && r.scope.recheck;
  const review = r.trigger === "review";
  if ((r.trigger !== "full" && r.trigger !== "recheck" && !review) || !c) return null;
  if (r.dryRun && r.trigger === "full") return null;
  const would = { notes: c.notes, domains: c.domains, questions: c.questions, more: c.more };
  const changed = changedReview(r);
  const node = r.dryRun
    ? hint(tr(changed ? "memory.sync.run.reviewChangedWould" : review ? "memory.sync.run.reviewWould" : "memory.sync.run.recheckWould", would))
    : !c.notes
      ? hint(tr(changed ? "memory.sync.run.reviewChangedNone" : review ? "memory.sync.run.reviewNone" : "memory.sync.run.recheckNone"))
      : hint([
        tr("memory.sync.run.rechecked", {
          asked: c.asked, questions: c.questions, notes: c.notes, kept: c.kept, fixed: c.fixed,
          dropped: c.dropped, merged: c.merged, linked: c.linked,
        }),
        review ? [" · ", tr("memory.sync.run.reviewSettled", { confirmed: c.confirmed || 0, lifted: c.lifted || 0 })]
          : c.lifted || c.rounds ? [" · ", tr("memory.sync.run.recheckLifted", { n: c.lifted || 0 })] : null,
        c.rounds ? [" · ", tr("memory.sync.run.settleRounds", { n: c.rounds })] : null,
        c.open ? [" · ", tr("memory.sync.run.settleOpen", { n: c.open })] : null,
        c.held ? [" · ", tr("memory.sync.run.recheckHeld", { n: c.held })] : null,
        c.more ? [" · ", tr("memory.sync.run.recheckMore", { n: c.more })] : null,
        c.failed ? [" · ", tr("memory.sync.run.recheckFailed", { n: c.failed })] : null,
        c.refused ? [" · ", tr("memory.sync.run.recheckRefused", { n: c.refused })] : null]);
  node.dataset.mcheck = r.id;
  return node;
}

/** A docs run's read and cards (SPEC §memory.docs): the documents read and what changed, then
 *  the cards saved and the code names found, and the cards re-linked to the code as it is
 *  now. A dry one says what it would read and ask. */
function docsNode(r            )                     {
  const d = r.scope && r.scope.docs;
  if (r.trigger !== "docs" || !d) return null;
  const left = Math.max(0, d.waiting - d.carded);
  const read = tr("memory.sync.run.docsRead", {
    files: d.files, sources: d.sources, added: d.added, changed: d.changed, unchanged: d.unchanged, removed: d.removed,
  });
  const node = r.dryRun
    ? hint([read, " · ", tr("memory.sync.run.docsWould", { waiting: d.waiting, questions: d.questions })])
    : hint([
      read,
      d.carded ? [" · ", tr("memory.sync.run.docsCarded", { carded: d.carded, asked: d.asked, questions: d.questions, symbols: d.symbols })] : null,
      d.relinked ? [" · ", tr("memory.sync.run.docsRelinked", { n: d.relinked })] : null,
      left ? [" · ", tr("memory.sync.run.docsWaiting", { n: left })] : null,
      d.more ? [" · ", tr("memory.sync.run.docsMore", { n: d.more })] : null,
      d.failed ? [" · ", tr("memory.sync.run.docsFailed", { n: d.failed })] : null,
      d.missing && d.missing.length ? [" · ", tr("memory.sync.run.docsMissing", { paths: d.missing.join(", ") })] : null]);
  node.dataset.mdocs = r.id;
  return node;
}

/** Whether a run's own lines say what its summary says, in the panel's language: a recheck's
 *  count, a docs run's read and cards, a full run's knowledge, wrap-up and recheck. The summary is the engine's English for
 *  the CLI and the log; such a run keeps it one hover away on its first line, with the first
 *  failure's reason it may carry, instead of saying everything twice. */
function saidByLines(r            )          {
  const s = r.scope;
  if (!s) return false;
  if (r.trigger === "recheck" || r.trigger === "review") return !!s.recheck;
  if (r.trigger === "docs") return !!s.docs;
  return r.trigger === "full" && !r.dryRun && !!s.knowledge;
}

/** The phase a running run is in, or the one a cancelled run stopped in; empty for none. */
function phaseWord(r            )         {
  const phase = r.scope && r.scope.phase;
  return phase && (r.state === "running" || r.state === "cancelled") ? tr(PHASE_KEY[phase] || phase) : "";
}

/** The phase as a line's tail: " · " and the phase, or nothing. */
export function phaseText(r            )         {
  const word = phaseWord(r);
  return word ? [" · ", word] : "";
}

/** A queued or running run's Stop, or the word that it is stopping once told to. */
function stopNode(r            , stopping          )         {
  if (r.state !== "queued" && r.state !== "running") return null;
  return stopping.includes(r.id)
    ? tag(tr("memory.sync.run.stopping"), { tone: "warn" })
    : actionBtn("stop", tr("memory.sync.run.stop"), { ghost: true, data: { mstop: r.id } });
}

/** How each worker of a pool went (§memory.settings), when the run asked more than one:
 *  the questions handed it - a retry on another worker counts there too - and the failures. */
function tallyNode(r            )                     {
  const ws = r.runner && r.runner.workers;
  if (!ws || !ws.length) return null;
  const node = hint([
    tr("memory.sync.run.workers"), " ",
    ws.map((w) => tr("memory.sync.run.workerTally", { name: w.label, asked: w.asked, failed: w.failed })).join(" · "),
  ]);
  node.dataset.mworkers = r.id;
  return node;
}

/** The agent runs a sync run started (SPEC §llm.history Live), as a flow inside its body: a
 *  running run's always, the ones still running first; a finished run's once the reader opens
 *  it. Each row is its state, what it was for, what it does now or how it went, and who ran
 *  it - the row opens its trace on the LLM history page. A queued run, or one that asked no
 *  agent, has none to show. */
export function agentRunsNode(r            , view           )         {
  if (r.state === "queued" || r.state === "empty") return null;
  const running = r.state === "running";
  if (!running && !view.shown.has(r.id)) return null;
  const got = view.rows.get(r.id);
  if (got == null) return running ? null : hint(tr("memory.sync.agents.loading"));
  if (typeof got === "string") return hint(tr("memory.sync.agents.failed", { why: got }), { bad: true });
  if (!got.length) return running ? null : hint(tr("memory.sync.agents.none"));
  const live = got.filter((a) => a.live);
  const shown = [...live, ...got.filter((a) => !a.live).slice(0, AGENT_RUNS_SHOWN)];
  const more = got.length - shown.length;
  return h("div", { data: { magentruns: r.id } },
    hint(live.length ? tr("memory.sync.agents.capLive", { n: got.length, live: live.length }) : tr("memory.sync.agents.cap", { n: got.length })),
    flow(shown.map((a)           => ({
      id: "a" + String(a.id), lead: runLead(a), title: runWhat(a), summary: runBrief(a),
      meta: a.model ? a.agent + " · " + a.model : a.agent, live: !!a.live,
      data: { magentrun: String(a.id), mllmrun: String(a.id) },
    }))),
    more > 0 ? hint(tr("memory.sync.agents.more", { n: more })) : null);
}

/** The marks after a run's name, in one quiet line: its state in words unless the dot says
 *  enough (done, or running with a phase to name), dry, redraw, the areas described, the phase. */
function runMarks(r            )         {
  const phase = phaseWord(r);
  const quiet = r.state === "ok" || (r.state === "running" && !!phase);
  return [
    quiet ? "" : tr(STATE_KEY[r.state] || STATE_KEY.failed),
    r.dryRun ? tr("memory.sync.run.dry") : "",
    r.scope && r.scope.redraw ? tr("memory.sync.run.redraw") : "",
    described(r),
    phase,
  ].filter(Boolean).join(" · ");
}

/** A stat line as the verdict under a run's row: the same words and hooks, inline. */
function verdictOf(line             , title        )              {
  const data                         = {};
  Object.keys(line.dataset).forEach((k) => { data[k] = line.dataset[k] || ""; });
  return h("span", { data, title: title || undefined }, ...Array.from(line.childNodes));
}

/** One run as a flow step, and its opened body. The verdict under the row is the run's first
 *  own line when its lines say what its summary says (saidByLines: the summary, the engine's
 *  English, one hover away), else a failed run's failure, else its summary. The body holds the
 *  rest - the other lines, the summary or failure the verdict did not take - and the agent runs. */
function runStep(r            , stopping          , agents           )                                     {
  const lines = [knowledgeNode(r), wrapupNode(r), tallyNode(r), recheckNode(r), docsNode(r)]
    .filter((l)                   => l != null);
  // A pool's tally says who answered, never what the run found: it never leads.
  const said = saidByLines(r) ? lines.find((l) => !l.dataset.mworkers) : undefined;
  const failure = !said && !!r.error && (r.state === "failed" || !r.summary);
  const detail         = said ? verdictOf(said, r.summary) : failure ? r.error : r.summary;
  const word = tr(STATE_KEY[r.state] || STATE_KEY.failed);
  return {
    step: {
      id: r.id,
      lead: dot(STATE_DOT[r.state] || "down", word),
      title: tr(changedReview(r) ? "memory.sync.run.reviewChanged" : TRIGGER_KEY[r.trigger] || TRIGGER_KEY.manual),
      summary: runMarks(r),
      detail,
      detailBad: failure,
      meta: ageText(r.finishedMs || r.startedMs || r.queuedMs),
      live: r.state === "running",
      tools: stopNode(r, stopping),
      data: { mrun: r.id },
    },
    body: [
      ...lines.filter((l) => l !== said),
      !said && !failure && r.summary && r.error ? hint(r.error, { bad: true }) : null,
      !said && failure && r.summary ? hint(r.summary) : null,
      said && r.error ? hint(r.error, { bad: true }) : null,
      agentRunsNode(r, agents),
    ].filter((n) => n != null),
  };
}

/** The last runs as a flow (ui/flow.ts): a state dot, what queued the run, its marks in one
 *  quiet line and its age, the verdict under it; opened, the run's counts and the agent runs it
 *  started. `open` holds the runs opened on the sheet, kept across the follow ticks' repaints;
 *  every body is painted up front, hidden until opened, so its words stay findable. */
function runsNode(runs              , stopping          , agents            = { rows: new Map(), shown: new Set() },
  open              = new Set())              {
  if (!runs.length) return h("div", { id: "memSyncRuns" }, card([tr("memory.sync.runsEmpty")]));
  const bodies = new Map                  ();
  const steps = runs.map((r) => {
    const made = runStep(r, stopping, agents);
    bodies.set(r.id, made.body);
    return made.step;
  });
  return h("div", { id: "memSyncRuns" }, flow(steps, {
    open, eager: true,
    body: (s) => {
      const b = bodies.get(s.id);
      return b && b.length ? b : null;
    },
  }));
}

/** The areas a map run would draw: the ones already on the map are kept, the rest asked about. */
function planNode(p            )              {
  const rows           = p.plan.map((u) => h("div", { data: { marea: u.path } },
    h("code", null, u.path), " · ", u.title, " · ", tr("memory.sync.map.files", { n: u.files }),
    u.kept ? [" ", tag(tr("memory.sync.map.kept"))] : null,
    hint(u.owns.join(", "))));
  // An empty plan over files the map owns is everything drawn already, not a missing index.
  const covered = p.plan.length === 0 && p.mappedFiles > 0;
  const removed = p.removed || [];
  return h("div", { id: "memSyncMapPlan" },
    removed.length ? hint(tr("memory.sync.map.removed", { n: removed.length, paths: removed.join(", ") })) : null,
    covered ? null : hint(tr("memory.sync.map.planN", { asks: p.asks, owned: p.ownedFiles, code: p.codeFiles })),
    p.review ? hint(tr("memory.sync.map.review")) : null,
    p.truncated ? hint(tr("memory.sync.map.truncated"), { bad: true }) : null,
    p.plan.length ? card(rows)
      : card([covered ? tr("memory.sync.map.planCovered", { n: p.mappedFiles }) : tr("memory.sync.map.planEmpty")]));
}

/** What a full run would do: the slices it reads and asks, the index, then the map's areas. */
function fullPlanNode(p             )              {
  const k = p.knowledge;
  const tone = p.embedding.state === "off" ? "off" : p.embedding.state === "error" ? "error" : "up";
  return h("div", { id: "memSyncInitPlan" },
    card([
      kvRow(tr("memory.sync.init.slices"), tr("memory.sync.init.slicesN", { files: k.files, slices: k.slices, asks: k.asks })),
      kvRow(tr("memory.sync.init.skipped"), tr("memory.sync.init.skippedN", { unchanged: k.unchanged, more: k.more })),
      kvRow(tr("memory.sync.init.embedding"), [dot(tone, p.embedding.state), " ", embeddingText(p.embedding)]),
    ]),
    k.unread ? hint(tr("memory.sync.init.unread", { n: k.unread })) : null,
    planNode(p));
}

/** The settings sheet: status and runs on top, the map and the block folded under. `openAuto`
 *  opens the project's auto-update sheet, where its daily review is set. */
export async function openSync(projectId        , o                            = {})                {
  const got = await req                 ("GET", "/sync?project=" + encodeURIComponent(projectId));
  if (!got.ok || !got.data) { toast(errMessage(got.err), true); return; }
  let over = got.data;
  // The agents to pick from (a db agent serves the Data page alone); the llm plugin off (or not
  // answering) leaves only "off" and says why.
  const llm = await loadStatus();
  const agents                = taskAgents(llm.data);

  // The agent runs of the sync runs on the sheet: a running run's read again on every follow
  // tick, and once more as it finishes; a finished one's when the reader opens it.
  const agentRuns            = { rows: new Map(), shown: new Set() };
  // The runs opened on the sheet. A running run opens once on its own, so its agent runs show
  // without a click; closed by the reader, it stays closed.
  const openRuns = new Set        ();
  const autoOpened = new Set        ();
  let wasRunning = new Set        ();
  const readAgents = async (ids          )                => {
    await Promise.all(ids.map(async (id) => {
      const r = await call                       ("GET", "/api/llm/history/runs?source=" + encodeURIComponent("memory:" + id));
      agentRuns.rows.set(id, r.data ? r.data.runs || [] : r.err);
      agentRuns.shown.add(id);
    }));
  };
  const readRunning = async ()                => {
    const running = new Set(over.runs.filter((r) => r.state === "running").map((r) => r.id));
    const ended = over.runs.filter((r) => wasRunning.has(r.id) && !running.has(r.id)).map((r) => r.id);
    wasRunning = running;
    running.forEach((id) => {
      if (!autoOpened.has(id)) { autoOpened.add(id); openRuns.add(id); }
    });
    await readAgents([...running, ...ended]);
  };
  await readRunning();

  const status = h("div", null, statusNode(over));
  const runs = h("div", null);
  const cancelPending = new Set        ();
  const cancelAccepted = new Set        ();
  const paintCancelButtons = ()       => {
    runs.querySelectorAll                   ("[data-mstop]").forEach((button) => {
      const id = button.dataset.mstop || "";
      const busy = cancelPending.has(id) || cancelAccepted.has(id);
      setActionBusy(button, "stop", { label: tr("memory.sync.run.stop") }, busy ? tr("memory.sync.run.stopping") : null);
    });
  };
  const paintRuns = ()       => {
    fill(runs, runsNode(over.runs, over.stopping || [], agentRuns, openRuns));
    paintCancelButtons();
  };
  paintRuns();
  const err = hint("", { id: "memSyncErr", bad: true, hidden: true, live: true });
  const say = (text        )       => { err.textContent = text; err.hidden = !text; };

  const block               = over.config || {};
  // The block's agent: inherit (absent), off, or one or more names - a pool (§memory.settings).
  const agentPick = agentsPick({ id: "memSyncAgent", value: block.agent, agents, namedLabel: tr("memory.sync.pick.named") });
  const manage = btn(tr("memory.sync.block.manageAgents"), { id: "memSyncManage", kind: "ghost" });
  const leadUi = leadPick({ id: "memSyncLead", value: block.lead, agents });
  const transcripts = h("select", { id: "memSyncTranscripts" },
    h("option", { value: "off", selected: block.transcripts === "off" }, tr("memory.sync.block.transcripts.off")),
    h("option", { value: "claude-code", selected: block.transcripts !== "off" }, tr("memory.sync.block.transcripts.claude")));
  const dirs = h("textarea", { id: "memSyncDirs", rows: 2, placeholder: tr("memory.sync.block.dirsPh") },
    (block.transcriptDirs || []).join("\n"));
  const auto = h("input", { id: "memSyncAuto", type: "checkbox", checked: block.auto !== false });
  const interval = h("input", { id: "memSyncInterval", value: block.minIntervalSec == null ? "" : String(block.minIntervalSec) });
  const saveSync = btn(tr("memory.sync.block.save"), { id: "memSyncBlockSave", kind: "primary" });

  const syncFields = h("div", { id: "memSyncBlockFields" });
  const paintSyncFields = ()       => {
    // Inherit still carries the block's transcripts and cadence - only off drops them.
    const on = agentPick.read().kind !== "off";
    fill(syncFields,
      on ? field({ label: tr("memory.sync.block.lead"), control: leadUi.node, hint: tr("memory.sync.block.leadWhy") }) : null,
      on ? field({ label: tr("memory.sync.block.transcripts"), control: transcripts, hint: tr("memory.sync.block.transcriptsWhy") }) : null,
      on && transcripts.value !== "off"
        ? field({ label: tr("memory.sync.block.dirs"), control: dirs, hint: tr("memory.sync.block.dirsWhy") })
        : null,
      on
        ? pair(
            checkField({ label: tr("memory.sync.block.auto"), control: auto, hint: tr("memory.sync.block.autoWhy") }),
            field({ label: tr("memory.sync.block.interval"), control: interval, hint: tr("memory.sync.block.intervalWhy") }))
        : null);
  };

  /** The block the form describes; empty optional fields dropped, a bad number refused.
   *  Absent agent and lead inherit (§memory.settings) - an untouched save writes them
   *  back absent, never silently off. */
  const readBlock = ()                        => {
    const picked = agentPick.read();
    if (picked.kind === "off") return { agent: "off" };
    if (picked.kind === "named" && !picked.names.length) return tr("memory.sync.pick.pickOne");
    const out               = {};
    if (picked.kind === "named") out.agent = picked.names.length === 1 ? picked.names[0] : picked.names;
    const lead = leadUi.read();
    if (lead === "off") out.lead = "off";
    else if (lead !== "inherit") out.lead = lead;
    out.transcripts = transcripts.value === "off" ? "off" : "claude-code";
    const dirLines = dirs.value.split("\n").map((s) => s.trim()).filter(Boolean);
    if (dirLines.length) out.transcriptDirs = dirLines;
    out.auto = auto.checked;
    if (interval.value.trim()) {
      if (!/^\d+$/.test(interval.value.trim())) return tr("memory.sync.block.wholeNumber", { field: tr("memory.sync.block.interval") });
      out.minIntervalSec = Number(interval.value.trim());
    }
    return out;
  };

  /** One CAS PUT of the row with the block replaced; the rest of the row rides back untouched. */
  const putBlock = async (value              )                         => {
    const fresh = await configReq                                                                ("GET");
    if (!fresh.data) return fresh.err;
    const put = await configReq                    ("PUT", { revision: fresh.data.revision, config: { ...(fresh.data.config || {}), sync: value } });
    if (!put.data) return put.err;
    return put.data.error || null;
  };

  let refreshSeq = 0;
  const refresh = async ()                => {
    const seq = ++refreshSeq;
    const r = await req                 ("GET", "/sync?project=" + encodeURIComponent(projectId));
    if (seq === refreshSeq && r.ok && r.data) {
      over = r.data;
      cancelAccepted.forEach((id) => {
        if (!over.runs.some((run) => run.id === id && (run.state === "queued" || run.state === "running"))) cancelAccepted.delete(id);
      });
      await readRunning();
      if (seq !== refreshSeq) return;
      fill(status, statusNode(over));
      paintRuns();
    }
  };
  // A queued or running run is followed while the sheet stays open; closing it ends the watch.
  let following = false;
  const follow = ()       => {
    if (following || !over.runs.some((r) => r.state === "queued" || r.state === "running")) return;
    following = true;
    setTimeout(() => {
      following = false;
      if (!runs.isConnected) return;
      void refresh().then(follow);
    }, FOLLOW_MS);
  };

  // The map: a language tag (the panel's own when it is not English), dry run, a preview.
  const lang = h("input", { id: "memSyncMapLang", placeholder: tr("memory.sync.map.langPh"),
    value: locale().startsWith("zh") ? "zh" : "" });
  const mapDry = h("input", { id: "memSyncMapDry", type: "checkbox" });
  const redraw = h("input", { id: "memSyncMapRedraw", type: "checkbox" });
  const preview = btn(tr("memory.sync.map.preview"), { id: "memSyncMapPreview" });
  const drawMap = btn(tr("memory.sync.map.draw"), { id: "memSyncMapDraw" });
  const maxAsks = h("input", { id: "memSyncInitMax", placeholder: String(MAX_ASKS) });
  const initPreview = btn(tr("memory.sync.init.preview"), { id: "memSyncInitPreview" });
  const initRun = btn(tr("memory.sync.init.run"), { id: "memSyncInit", kind: "primary" });
  const recheckRun = btn(tr("memory.sync.recheck.run"), { id: "memSyncRecheck" });
  const reviewRun = btn(tr("memory.sync.review.run"), { id: "memSyncReview" });
  const reviewChanged = btn(tr("memory.sync.review.changed"), { id: "memSyncReviewChanged" });
  const planBox = h("div", { id: "memSyncMapBox" });
  // This run's own pick (§memory.settings): left out, the layers below decide. It rides the
  // queued run's record and nowhere else.
  const runAgents = agentsPick({ id: "memSyncRunAgents", value: null, agents, namedLabel: tr("memory.sync.pick.named") });
  const runLead = leadPick({ id: "memSyncRunLead", value: null, agents });
  const mapFold = formFold({ summary: tr("memory.sync.map"), id: "memSyncMapFold" },
    hint(tr("memory.sync.map.intro")),
    field({ label: tr("memory.sync.run.pick"), control: runAgents.node, hint: tr("memory.sync.run.pickWhy") }),
    field({ label: tr("memory.sync.run.lead"), control: runLead.node, hint: tr("memory.sync.run.leadWhy") }),
    pair(
      field({ label: tr("memory.sync.map.lang"), control: lang, hint: tr("memory.sync.map.langWhy") }),
      checkField({ label: tr("memory.sync.map.dry"), control: mapDry, hint: tr("memory.sync.map.dryWhy") })),
    checkField({ label: tr("memory.sync.map.redraw"), control: redraw, hint: tr("memory.sync.map.redrawWhy") }),
    inlineForm(preview, drawMap),
    hint(tr("memory.sync.init.intro")),
    field({ label: tr("memory.sync.init.max"), control: maxAsks, hint: tr("memory.sync.init.maxWhy") }),
    inlineForm(initPreview, initRun),
    hint(tr("memory.sync.recheck.intro")),
    inlineForm(recheckRun),
    hint(tr("memory.sync.review.intro")),
    inlineForm(reviewRun, reviewChanged),
    planBox);

  const now = btn(tr("memory.sync.now"), { kind: "primary", id: "memSyncNow" });
  const dry = btn(tr("memory.sync.dry"), { id: "memSyncDry" });
  const close = btn(tr("memory.close"));
  const settings = formFold({ summary: tr("memory.sync.block"), id: "memSyncBlockFold" },
    field({
      label: tr("memory.sync.block.agent"), control: agentPick.node, action: manage,
      hint: llm.data
        ? (agents.length ? tr("memory.sync.block.agentWhy") : tr("memory.sync.block.noAgents"))
        : tr("memory.sync.block.agentsUnavailable", { error: llm.err }),
    }),
    syncFields,
    saveSync);
  // Nothing named here yet: the one setting that matters starts unfolded.
  if (block.agent == null) settings.setAttribute("open", "");

  // The project's own settings (§memory.settings): whom its runs ask, its lead and whether its
  // notes are embedded (§memory.embedding). Saved through the settings API; the fold repaints
  // from the saved answer. Never repainted by the follow-up reads - a refresh every few
  // seconds must not eat the edit in progress. The daily review is the auto-update sheet's:
  // the fold says how it stands and opens that sheet.
  const settingsBox = h("div", { id: "memSyncProjectFields" });
  // The global scope has no checkout: no file under its notes changes.
  const checkout = projectId !== "global";
                    
                                                                                                  
                             
    
  let projectUI                   = null;
  const paintProject = ()       => {
    const s = over.settings || { agents: null, lead: null, embedding: null };
    const pick = agentsPick({ id: "memSyncProjectAgents", value: s.agents, agents, namedLabel: tr("memory.sync.pick.named") });
    const lead = leadPick({ id: "memSyncProjectLead", value: s.lead, agents });
    const embed = h("select", { id: "memSyncProjectEmbedding" },
      h("option", { value: "inherit" }, tr("memory.sync.settings.embed.inherit")),
      h("option", { value: "on" }, tr("memory.sync.settings.embed.on")),
      h("option", { value: "off" }, tr("memory.sync.settings.embed.off")));
    embed.value = s.embedding == null ? "inherit" : s.embedding ? "on" : "off";
    projectUI = { agents: pick, lead, embed };
    const openAuto = o.openAuto;
    let autoGo                           = null;
    if (openAuto) {
      autoGo = btn(tr("memory.sync.settings.autoOpen"), { id: "memSyncAutoOpen", kind: "ghost", icon: "clock" });
      autoGo.onclick = ()       => { closeSheet(); openAuto(); };
    }
    fill(settingsBox,
      field({ label: tr("memory.sync.settings.agents"), control: pick.node, hint: tr("memory.sync.settings.agentsWhy") }),
      field({ label: tr("memory.sync.block.lead"), control: lead.node, hint: tr("memory.sync.settings.leadWhy") }),
      field({ label: tr("memory.sync.settings.embedding"), control: embed, hint: tr("memory.sync.settings.embeddingWhy") }),
      checkout
        ? field({
            label: tr("memory.sync.settings.autoReview"),
            control: h("span", { id: "memSyncAutoState" }, s.autoReview
              ? tr("memory.auto.daily.at", { time: s.autoReview })
              : tr("memory.auto.daily.off")),
            action: autoGo || undefined,
          })
        : null);
  };
  const saveSettings = btn(tr("memory.sync.settings.save"), { id: "memSyncSettingsSave", kind: "primary" });
  const projectFold = formFold({ summary: tr("memory.sync.settings"), id: "memSyncSettingsFold" },
    hint(tr("memory.sync.settings.intro")), settingsBox, saveSettings);
  paintProject();

  showSheet(sheet({
    title: tr("memory.sync.title"),
    body: [hint(tr("memory.sync.intro")), status, runs, err, mapFold, settings, projectFold],
    foot: [now, dry, h("span", { class: "grow" }), close],
  }));
  paintSyncFields();
  follow();

  agentPick.mode.addEventListener("change", paintSyncFields);
  transcripts.onchange = paintSyncFields;
  close.onclick = closeSheet;
  manage.onclick = ()       => {
    closeSheet();
    location.hash = "#llm-agents";
  };
  const onSaveSync = async ()                => {
    say("");
    const read = readBlock();
    if (typeof read === "string") { say(read); return; }
    saveSync.disabled = true;
    try {
      const failure = await putBlock(read);
      if (failure) { say(failure); return; }
      toast(tr("memory.sync.block.saved"));
      await refresh();
    } finally {
      saveSync.disabled = false;
    }
  };
  saveSync.onclick = ()       => { void onSaveSync(); };
  /** Save the project's own settings: every field goes - null inherits again (§memory.settings). */
  const onSaveSettings = async ()                => {
    say("");
    if (!projectUI) return;
    const picked = projectUI.agents.read();
    if (picked.kind === "named" && !picked.names.length) { say(tr("memory.sync.pick.pickOne")); return; }
    const lead = projectUI.lead.read();
    const body                          = {
      agents: picked.kind === "off" ? "off" : picked.kind === "inherit" ? null : picked.names.length === 1 ? picked.names[0] : picked.names,
      lead: lead === "inherit" ? null : lead,
      embedding: projectUI.embed.value === "inherit" ? null : projectUI.embed.value === "on",
    };
    saveSettings.disabled = true;
    try {
      const r = await req                       ("PATCH",
        "/projects/" + encodeURIComponent(projectId) + "/settings", body);
      if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
      toast(tr("memory.sync.settings.saved"));
      await refresh();
      paintProject();
    } finally {
      saveSettings.disabled = false;
    }
  };
  saveSettings.onclick = ()       => { void onSaveSettings(); };
  const queue = async (dryRun         )                => {
    say("");
    const r = await req                                      ("POST", "/sync", { project: projectId, dryRun });
    if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
    toast(r.data.merged ? tr("memory.sync.merged") : tr("memory.sync.queued", { id: r.data.run.id }));
    await refresh();
    follow();
  };
  /** Stop a run: a queued one is cancelled at once, a running one is told to stop. */
  const stopRun = async (id        , button                   )                => {
    if (cancelPending.has(id) || cancelAccepted.has(id)) return;
    const hadFocus = document.activeElement === button;
    cancelPending.add(id);
    say("");
    setActionBusy(button, "stop", { label: tr("memory.sync.run.stop") }, tr("memory.sync.run.stopping"));
    let failed = false;
    try {
      const r = await req                                        ("POST",
        "/sync/runs/" + encodeURIComponent(id) + "/cancel?project=" + encodeURIComponent(projectId));
      if (!r.ok || !r.data) { failed = true; say(errMessage(r.err)); return; }
      cancelAccepted.add(id);
      toast(r.data.stopping ? tr("memory.sync.run.stoppingToast", { id }) : tr("memory.sync.run.cancelledToast", { id }));
      await refresh();
      follow();
    } finally {
      cancelPending.delete(id);
      paintCancelButtons();
      if (failed && hadFocus && document.activeElement === document.body) {
        const restored = Array.from(runs.querySelectorAll                   ("[data-mstop]")).find((item) => item.dataset.mstop === id);
        if (restored && !restored.disabled) restored.focus();
      }
    }
  };
  /** Show a finished run's agent runs: the list says it is loading, then what the read found. */
  const showAgents = async (id        )                => {
    agentRuns.shown.add(id);
    paintRuns();
    await readAgents([id]);
    if (runs.isConnected) paintRuns();
  };
  runs.onclick = (ev)       => {
    const at = ev.target instanceof Element ? ev.target : null;
    const button = at ? at.closest                   ("[data-mstop]") : null;
    if (button && button.dataset.mstop && !button.disabled) { void stopRun(button.dataset.mstop, button); return; }
    // An agent run opens as its trace on the LLM history page.
    const link = at ? at.closest             ("[data-mllmrun]") : null;
    if (link && link.dataset.mllmrun) {
      closeSheet();
      showRun(Number(link.dataset.mllmrun));
      return;
    }
    // A run's row opens in place; a finished run reads its agent runs on its first opening.
    const sum = at ? at.closest(".fl-sum") : null;
    const step = sum ? sum.closest             ("[data-mrun]") : null;
    const id = step ? step.dataset.mrun : undefined;
    if (!id) return;
    const open = flowToggle(runs, id);
    if (open) openRuns.add(id);
    else openRuns.delete(id);
    const r = over.runs.find((x) => x.id === id);
    if (open && r && r.state !== "running" && r.state !== "queued" && r.state !== "empty" && !agentRuns.shown.has(id)) void showAgents(id);
  };
  now.onclick = ()       => { void queue(false); };
  dry.onclick = ()       => { void queue(true); };

  const onPreview = async ()                => {
    say("");
    preview.disabled = true;
    try {
      const r = await req            ("GET", "/sync/map?project=" + encodeURIComponent(projectId) + (redraw.checked ? "&redraw=1" : ""));
      if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
      fill(planBox, planNode(r.data));
    } finally {
      preview.disabled = false;
    }
  };
  /** The cap typed for a full run or a recheck: null for the default, the sentence for anything but a whole number from 1. */
  const readMax = ()                         => {
    const raw = maxAsks.value.trim();
    if (!raw) return null;
    return /^\d+$/.test(raw) && Number(raw) >= 1 ? Number(raw) : tr("memory.sync.init.maxBad");
  };
  /** Queue a map, a full run, a recheck or a review run with the fold's language and dry run;
   *  redraw and the lead go with a map or full run, the cap with any but a map. A review run
   *  `changed` judges only the notes whose code changed. */
  const onDraw = async (kind                                       , button                   , changed = false)                => {
    say("");
    const body                          = { project: projectId, [kind]: true, dryRun: mapDry.checked };
    if (changed) body.changed = true;
    // This run's own pick (§memory.settings): agents with any kind, a lead only beside a
    // map or full run.
    const pick = runAgents.read();
    if (pick.kind === "off") body.agents = "off";
    else if (pick.kind === "named") {
      if (!pick.names.length) { say(tr("memory.sync.pick.pickOne")); return; }
      body.agents = pick.names.length === 1 ? pick.names[0] : pick.names;
    }
    const draws = kind === "map" || kind === "full";
    if (draws) {
      const lead = runLead.read();
      if (lead === "off") body.lead = "off";
      else if (lead !== "inherit") body.lead = lead;
    }
    const language = lang.value.trim();
    if (language) body.language = language;
    if (redraw.checked && draws) body.redraw = true;
    if (kind !== "map") {
      const cap = readMax();
      if (typeof cap === "string") { say(cap); return; }
      if (cap !== null) body.maxAsks = cap;
    }
    button.disabled = true;
    try {
      const r = await req                                      ("POST", "/sync", body);
      if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
      const id = r.data.run.id;
      const said = {
        map: [tk("memory.sync.map.merged"), tk("memory.sync.map.queued")],
        full: [tk("memory.sync.init.merged"), tk("memory.sync.init.queued")],
        recheck: [tk("memory.sync.recheck.merged"), tk("memory.sync.recheck.queued")],
        review: [tk("memory.sync.review.merged"), tk("memory.sync.review.queued")],
      }[kind];
      toast(r.data.merged ? tr(said[0]) : tr(said[1], { id }));
      await refresh();
      follow();
    } finally {
      button.disabled = false;
    }
  };
  const onInitPreview = async ()                => {
    say("");
    const cap = readMax();
    if (typeof cap === "string") { say(cap); return; }
    let query = "/sync/full?project=" + encodeURIComponent(projectId);
    if (redraw.checked) query += "&redraw=1";
    if (cap !== null) query += "&maxAsks=" + cap;
    initPreview.disabled = true;
    try {
      const r = await req             ("GET", query);
      if (!r.ok || !r.data) { say(errMessage(r.err)); return; }
      fill(planBox, fullPlanNode(r.data));
    } finally {
      initPreview.disabled = false;
    }
  };
  preview.onclick = ()       => { void onPreview(); };
  drawMap.onclick = ()       => { void onDraw("map", drawMap); };
  initPreview.onclick = ()       => { void onInitPreview(); };
  initRun.onclick = ()       => { void onDraw("full", initRun); };
  recheckRun.onclick = ()       => { void onDraw("recheck", recheckRun); };
  reviewRun.onclick = ()       => { void onDraw("review", reviewRun); };
  reviewChanged.onclick = ()       => { void onDraw("review", reviewChanged, true); };
}

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
   LLM Agents - the llm plugin's third page (#llm-agents), SPEC §llm.agents and §llm.telemetry.

   A named agent is what a consumer runs (memory's sync names one): a runtime and what it
   needs. builtin is swiss's own tool loop over a provider, chat is one request to a provider,
   claude and pi spawn that CLI with an env. The name is the contract; a consumer keeps working
   when the agent behind it changes runtime. db is the one runtime no consumer names: the Data
   page's agent, its prompt and four tools fixed for that page, run by nothing else.

   Reporting: with a telemetry block, every run is OTLP logs in Claude Code's own event
   vocabulary (user_prompt, api_request, tool_result ...) - swiss reports builtin and chat runs
   itself; a claude child gets the OTEL env and reports itself; pi has no hook. A collector
   that already reads Claude Code (cc-otel) reads these unchanged.
   ================================================================================================ */
import { $, targetEl, toast } from "../util.js";
                                      
import { fill, h } from "../h.js";
import { tk, tr, trn } from "../i18n.js";
import {
  btn, card, chatQueue, checkField, closeSheet, dot, emptyNode, failNote, field, formCap, formFold, hint, kvRow, moreBtn, note, pair, paneBody,
  paneHead, popupMenu, proseText, relTime, row, section, sheet, showSheet, tag, transcript, transcriptTurn, valueBlock,
} from "../ui/index.js";
                                                                         
                                                                                                                                                               
import {
  NAME_RE, call, credentialLeak, isError, linesToMap, loadRow, loadStatus, mapToLines, modelsOf, putRow, runtimeWord, takeAgentPreset, testNode, toolSummary, wholeField,
} from "./llm-core.js";
                                                 
import { conversationNode, promptTurn } from "./llm-history.js";
                                                    
import { streamAsk } from "./llm-stream.js";
                                                                  

const AGENTS_MAX = 16;
const TURNS_MAX = 64;
const TOKENS_MIN = 256;
const TOKENS_MAX = 131_072;
const CONTEXT_KIB_MIN = 64;
const CONTEXT_KIB_MAX = 8192;
const CONCURRENCY_MAX = 16;
const TIMEOUT_MIN_S = 60;
const TIMEOUT_MAX_S = 7200;
/** The server's cap (swiss-llm agent.rs PROMPT_MAX): a spawned CLI takes the prompt on its
 *  command line, which Windows caps at 32767 UTF-16 units. */
const PROMPT_MAX = 20_000;
/** The stream route's cap on one ask (swiss-plugins llm_api.rs PROMPT_MAX): queued words
 *  that would join past it go back to the prompt box instead of being sent. */
const ASK_MAX = 2000;

let status = null                    ;
let painted = "";
let visit = 0; // bumped by mount and unmount: a late answer from an ended visit paints nothing

const RUNTIMES               = ["builtin", "chat", "claude", "pi", "db"];
const EFFORTS              = ["low", "medium", "high", "max"];
const EFFORT_LABELS                            = {
  low: tk("llm.agents.effort.low"), medium: tk("llm.agents.effort.medium"), high: tk("llm.agents.effort.high"), max: tk("llm.agents.effort.max"),
};
// The bare program names a child runtime looks up when its command is left empty.
const CLAUDE_COMMAND = "claude";
const PI_COMMAND = "pi";
const RUNTIME_LABELS                             = {
  builtin: tk("llm.agents.runtime.builtin"),
  chat: tk("llm.agents.runtime.chat"),
  claude: tk("llm.agents.runtime.claude"),
  pi: tk("llm.agents.runtime.pi"),
  db: tk("llm.agents.runtime.db"),
};
const RUNTIME_WHY                             = {
  builtin: tk("llm.agents.runtimeWhy.builtin"),
  chat: tk("llm.agents.runtimeWhy.chat"),
  claude: tk("llm.agents.runtimeWhy.claude"),
  pi: tk("llm.agents.runtimeWhy.pi"),
  db: tk("llm.agents.runtimeWhy.db"),
};
const TOOLS_WHY                             = {
  builtin: tk("llm.agents.toolsWhy.builtin"),
  chat: tk("llm.agents.toolsWhy.chat"),
  claude: tk("llm.agents.toolsWhy.claude"),
  pi: tk("llm.agents.toolsWhy.pi"),
  db: tk("llm.agents.toolsWhy.db"),
};
const REPORTS_WORDS                                         = {
  swiss: tk("llm.agents.reports.swiss"),
  child: tk("llm.agents.reports.child"),
  off: tk("llm.agents.reports.off"),
};
const PROTOCOL_LABELS                              = {
  grpc: tk("llm.telemetry.protocol.grpc"),
  "http/protobuf": tk("llm.telemetry.protocol.protobuf"),
  "http/json": tk("llm.telemetry.protocol.json"),
};
/** The Telemetry sheet's presets: cc-otel's two listeners on this machine. */
const COLLECTORS                                                                             = {
  custom: { protocol: "grpc", endpoint: "", label: tk("llm.telemetry.preset.custom") },
  grpc: { protocol: "grpc", endpoint: "http://127.0.0.1:4317", label: tk("llm.telemetry.preset.grpc") },
  http: { protocol: "http/protobuf", endpoint: "http://127.0.0.1:4318", label: tk("llm.telemetry.preset.http") },
};

function find(name                    )                          {
  return status ? status.agents.find((a) => a.name === name) : undefined;
}

function input(id        , value                             , placeholder = "")                   {
  return h("input", { id, autocomplete: "off", spellcheck: false, value: value == null ? "" : String(value), placeholder });
}

/** The runtimes that ask a provider themselves: a provider and a model, no command. */
function asksProvider(runtime            )          {
  return runtime === "builtin" || runtime === "chat" || runtime === "db";
}

function catalogOf(runtime            )                {
  return status ? status.catalog[runtime] : [];
}

/** The tools an agent takes, at a glance: the names in force, or none. */
function toolsCol(a             )         {
  if (!catalogOf(a.runtime).length || !a.toolsInForce.length) return { v: tr("llm.agents.toolsNone"), w: "l" };
  const names = a.toolsInForce.join(", ");
  return { v: names, w: "l", mono: true, title: names };
}

/** One entry of a builtin agent's `secrets`: a vault name, group:<group> or * (SPEC §llm.agents). */
function grantable(g        )          {
  return g === "*" || /^[a-z][a-z0-9-]{0,63}$/.test(g) || (g.startsWith("group:") && g.length > 6 && g.length <= 70);
}

/** What the agent runs on - the row's sub line, which an opened row hides, plus its limits. */
function setupView(a             )              {
  const asks = asksProvider(a.runtime);
  return valueBlock({ label: tr("llm.agents.setup"), data: { lablock: "setup" } },
    kvRow(tr("llm.agents.runtime"), runtimeWord(a.runtime)),
    asks
      ? kvRow(tr("llm.agents.provider"), a.provider || "-")
      : kvRow(tr("llm.agents.command"), h("code", null, a.program || a.command || "-")),
    a.model ? kvRow(tr("llm.agents.model"), h("code", null, a.model)) : null,
    a.maxTurns != null ? kvRow(tr("llm.agents.maxTurns"), String(a.maxTurns)) : null,
    a.secrets && a.secrets.length
      ? kvRow(tr("llm.agents.secrets"), a.secrets.join(", ") + " · " + tr("llm.agents.secretsReach", { count: (a.secretsInForce || []).length }),
        { title: (a.secretsInForce || []).join(", ") })
      : null,
    a.maxTokens != null ? kvRow(tr("llm.agents.maxTokens"), String(a.maxTokens)) : null,
    a.maxContextKiB != null ? kvRow(tr("llm.agents.maxContext"), String(a.maxContextKiB)) : null,
    asks ? kvRow(tr("llm.agents.effort"), a.effort ? tr(EFFORT_LABELS[a.effort] || a.effort) : tr("llm.agents.effortDefault")) : null,
    kvRow(tr("llm.agents.concurrency"), String(a.concurrency)),
    kvRow(tr("llm.agents.timeout"), String(Math.round(a.timeoutMs / 1000))));
}

/** The system prompt in force, as sent: a builtin, chat or db agent's runtime default (marked) and
 *  its own after it, a CLI's own with the agent's appended, or the CLI's own alone. */
function promptView(a             )              {
  const label = tr("llm.agents.systemPrompt");
  const data = { lablock: "prompt" };
  if (a.systemPromptDefault) {
    if (!a.systemPrompt) return valueBlock({ label, data, notes: [tr("llm.agents.promptDefaultTag")], text: a.systemPromptDefault });
    return valueBlock({ label, data, notes: [tr("llm.agents.promptDefaultThenOwn")], text: a.systemPromptDefault },
      proseText(a.systemPrompt, { data: { laown: "1" } }));
  }
  if (a.systemPrompt) return valueBlock({ label, data, notes: [tr("llm.agents.promptAppended")], text: a.systemPrompt });
  return valueBlock({ label, data, text: tr("llm.agents.promptCliOwn") });
}

/** Every tool the runtime knows, a row each: what it does, and whether this agent takes it. */
function toolsView(a             )              {
  const label = tr("llm.agents.tools");
  const data = { lablock: "tools" };
  const catalog = catalogOf(a.runtime);
  if (!catalog.length) return valueBlock({ label, data, text: tr("llm.agents.toolsChat") });
  return valueBlock({ label, data }, catalog.map((t) => kvRow(t.name, a.toolsInForce.includes(t.name)
    ? toolSummary(a.runtime, t)
    : [tag(tr("llm.agents.toolOff")), " ", toolSummary(a.runtime, t)])));
}

function agentRow(a             )              {
  const what         = asksProvider(a.runtime)
    ? [a.provider || "-", a.model ? [" · ", h("code", null, a.model)] : null]
    : [h("code", null, a.program || a.command || "-"), a.model ? [" · ", h("code", null, a.model)] : null];
  return row({
    name: a.name,
    sub: [runtimeWord(a.runtime), " · ", what, a.systemPrompt ? " · " + tr("llm.agents.ownPrompt") : null],
    err: a.programError,
    // What it runs on, the prompt and the tools are the agent's record: shown whole, a click away.
    detail: [setupView(a), promptView(a), toolsView(a)],
    cols: [toolsCol(a), { v: tr(REPORTS_WORDS[a.reports]), w: "m", title: tr("llm.agents.reportsWhy") }],
    // A db agent runs no Test: it is asked on the Data page, on a connection.
    primary: a.runtime === "db"
      ? btn(tr("llm.agents.openData"), { kind: "ghost", data: { ladata: a.name } })
      : btn(tr("llm.test"), { kind: "ghost", data: { latest: a.name } }),
    more: moreBtn(tr("llm.agents.actionsName", { name: a.name }), { data: { lamore: a.name } }),
    data: { larow: a.name },
  });
}

/** The exporter at a glance: on or off, where to, and what it has sent since the start. */
function telemetryNode(s           , block         )              {
  const t = s.telemetry;
  if (!block) return h("div", { id: "laTel" }, note(tr("llm.telemetry.none")));
  const state = t.enabled ? tr("llm.telemetry.on") : tr("llm.telemetry.off");
  return h("div", { id: "laTel" }, card(
    kvRow(tr("llm.telemetry.state"), [dot(t.enabled ? (t.lastError && (t.lastErrorMs || 0) > (t.lastExportMs || 0) ? "error" : "up") : "off", state), " ", state]),
    kvRow(tr("llm.telemetry.collector"), [t.protocol ? tr(PROTOCOL_LABELS[t.protocol]) : "-", " · ", h("code", null, t.endpoint || "-")]),
    kvRow(tr("llm.telemetry.counts"), [
      tr("llm.telemetry.sentN", { n: t.sent }),
      t.queued ? [" · ", tr("llm.telemetry.queuedN", { n: t.queued })] : null,
      t.dropped ? [" ", tag(tr("llm.telemetry.droppedN", { n: t.dropped }), { tone: "warn", title: tr("llm.telemetry.droppedWhy") })] : null,
      t.rejected ? [" ", tag(tr("llm.telemetry.rejectedN", { n: t.rejected }), { tone: "warn", title: tr("llm.telemetry.rejectedWhy") })] : null,
    ]),
    kvRow(tr("llm.telemetry.lastExport"), t.lastExportMs ? relTime(t.lastExportMs) : tr("llm.telemetry.never")),
    t.lastError ? kvRow(tr("llm.telemetry.lastError"), [t.lastError, t.lastErrorMs ? " · " + relTime(t.lastErrorMs) : null]) : null));
}

let hasBlock = false; // whether the row holds a telemetry block (the status cannot tell "absent" from "off")

function signature(s           )         {
  // The counters move with every export; the card repaints alone for them (poll).
  return JSON.stringify([s.serving, s.agents, s.providers.map((p) => p.name), hasBlock]);
}

function render()       {
  const s = status;
  if (!s) return;
  painted = signature(s);
  fill($("pane"), paneBody({ wide: true },
    paneHead({
      desc: tr("llm.agents.desc"),
      sub: s.serving ? null : tr("llm.notServing"),
      actions: [btn(tr("llm.agents.add"), { id: "laAdd", kind: "primary", disabled: s.agents.length >= AGENTS_MAX })],
    }),
    section({},
      s.agents.length
        ? card(s.agents.map(agentRow))
        : emptyNode({ icon: "sparkles", title: tr("llm.agents.emptyTitle"), hint: tr("llm.agents.emptyHint"), action: tr("llm.agents.add") })),
    section({
      cap: tr("llm.telemetry.cap"),
      note: tr("llm.telemetry.note"),
      tools: [
        btn(tr("llm.test"), { id: "laTelTest", kind: "ghost", disabled: !hasBlock }),
        btn(hasBlock ? tr("llm.edit") : tr("llm.telemetry.setUp"), { id: "laTelEdit", kind: "ghost" }),
      ],
    }, telemetryNode(s, hasBlock))));
  $("pane").onclick = onClick;
  $("countChip").textContent = countText();
}

/** The Add/Edit sheet over the stored entry. Only the chosen runtime's fields are written: the
 *  server refuses a field another runtime reads, so a runtime switch never leaves one behind.
 *  Each runtime's fields are built once and shown or hidden - never re-filled: a styled select
 *  moved into a fresh label would leave its face behind in the old one. A new agent may start
 *  from a preset another page asked for (its name only while free). */
async function openAgent(name               , preset                     = null)                {
  const got = await loadRow();
  if (!got.data || !status) { toast(got.err || tr("llm.loadFailed"), true); return; }
  const st = status;
  const stored = got.data.agents || {};
  const e                = (name && stored[name]) || {};
  const providers = st.providers;
  const storedRuntime             = e.runtime || (preset ? preset.runtime : "builtin");

  const nameIn = input("laName", name || (preset && !(preset.name in stored) ? preset.name : ""), tr("llm.agents.namePh"));
  nameIn.disabled = !!name;
  const runtime = h("select", { id: "laRuntime" }, RUNTIMES.map((r) => h("option", { value: r }, tr(RUNTIME_LABELS[r]))));
  runtime.value = storedRuntime;
  const runtimeWhy = hint("", { id: "laRuntimeWhy" });
  const provider = h("select", { id: "laProvider" },
    providers.map((p) => h("option", { value: p.name, selected: e.provider === p.name }, p.name)),
    e.provider && !providers.some((p) => p.name === e.provider) ? h("option", { value: e.provider, selected: true }, e.provider) : null);
  // builtin and chat pick one of the provider's chat models; a CLI takes its own model id.
  const pick = h("select", { id: "laModelPick" });
  const pickWhy = h("span", { id: "laModelPickWhy" });
  const model = input("laModel", e.model);
  const maxTokens = input("laTokens", e.maxTokens, "131072");
  const maxTurns = input("laTurns", e.maxTurns, "24");
  const maxContext = input("laContext", e.maxContextKiB, "1024");
  const effort = h("select", { id: "laEffort" },
    h("option", { value: "" }, tr("llm.agents.effortDefault")),
    EFFORTS.map((x) => h("option", { value: x }, tr(EFFORT_LABELS[x]))));
  effort.value = e.effort || "";
  const concurrency = input("laConcurrency", e.concurrency);
  const concurrencyWhy = h("span", { id: "laConcurrencyWhy" });
  const command = input("laCommand", e.command);
  const env = h("textarea", { id: "laEnv", rows: 3, spellcheck: false, placeholder: tr("llm.agents.envPh") }, mapToLines(e.env));
  const prompt = h("textarea", { id: "laPrompt", rows: 6, maxLength: PROMPT_MAX }, e.systemPrompt || "");
  const promptWhy = h("span", { id: "laPromptWhy" });
  // A builtin, chat or db agent always sends its runtime's default; the box adds to it.
  const promptBase = proseText("", { id: "laPromptBase" });
  const promptBaseField = field({ group: true, label: tr("llm.agents.promptBase"), control: promptBase });
  const tools = h("div", { id: "laTools" });
  const secrets = input("laSecrets", (e.secrets || []).join(", "), "api-key, group:deploy");
  const secretsField = field({ label: tr("llm.agents.secrets"), control: secrets, hint: tr("llm.agents.secretsWhy") });
  const toolsWhy = h("span", { id: "laToolsWhy" });
  // Seconds on screen, milliseconds in the row; an unchanged field keeps the stored number.
  const shownTimeout = e.timeoutMs == null ? "" : String(Math.round(e.timeoutMs / 1000));
  const timeout = input("laTimeout", shownTimeout, "1800");
  const report = h("input", { id: "laReport", type: "checkbox", checked: e.report !== false });
  const reportWhy = h("span", { id: "laReportWhy" });
  const err = hint("", { id: "laErr", bad: true, hidden: true, live: true });
  const say = (text        )       => { err.textContent = text; err.hidden = !text; };
  const cancel = btn(tr("llm.cancel"), { id: "laCancel" });
  const save = btn(name ? tr("llm.save") : tr("llm.add"), { id: "laSave", kind: "primary" });
  const rt = ()             => RUNTIMES.find((r) => r === runtime.value) || "builtin";
  const defaultPrompt = ()                => st.defaultPrompts[rt()];

  const turnsField = field({ label: tr("llm.agents.maxTurns"), control: maxTurns, hint: tr("llm.agents.maxTurnsWhy") });
  const askPart = h("div", { id: "laAsk" },
    pair(
      field({ label: tr("llm.agents.provider"), control: provider, required: true, hint: providers.length ? null : tr("llm.agents.noProvider") }),
      field({ label: tr("llm.agents.model"), control: pick, hint: pickWhy })),
    pair(
      field({ label: tr("llm.agents.maxTokens"), control: maxTokens, hint: tr("llm.wholeRange", { min: TOKENS_MIN, max: TOKENS_MAX }) }),
      turnsField),
    pair(
      field({ label: tr("llm.agents.maxContext"), control: maxContext, hint: tr("llm.agents.maxContextWhy", { min: CONTEXT_KIB_MIN, max: CONTEXT_KIB_MAX }) }),
      field({ label: tr("llm.agents.effort"), control: effort, hint: tr("llm.agents.effortWhy") })));
  const modelWhy = h("span", { id: "laModelWhy" });
  const cliPart = h("div", { id: "laCli" },
    pair(
      field({ label: tr("llm.agents.command"), control: command, hint: tr("llm.agents.commandWhy") }),
      field({ label: tr("llm.agents.model"), control: model, hint: modelWhy })),
    field({ label: tr("llm.agents.env"), control: env, hint: tr("llm.agents.envWhy") }));
  const toolsField = field({ group: true, label: tr("llm.agents.tools"), control: tools, hint: toolsWhy });

  // The model list follows the provider, one pick per provider: switching away and back keeps
  // it. "" follows the provider's first chat model, whatever that becomes.
  const picks = new Map                (e.provider && e.model ? [[e.provider, e.model]] : []);
  let shown = "";
  const paintModels = ()       => {
    if (shown) picks.set(shown, pick.value);
    shown = provider.value;
    const ids = modelsOf(st, shown, "chat");
    const want = picks.get(shown) || "";
    // A stored pick the provider no longer lists still shows, so the select never lies.
    const stale = want !== "" && !ids.includes(want);
    fill(pick,
      h("option", { value: "" }, ids.length ? tr("llm.agents.modelDefault", { model: ids[0] }) : tr("llm.agents.modelNone")),
      ids.map((id) => h("option", { value: id }, id)),
      stale ? h("option", { value: want }, tr("llm.agents.modelUnlisted", { model: want })) : null);
    pick.value = want;
    pick.disabled = !ids.length && !stale;
    pickWhy.textContent = !shown ? ""
      : ids.length || stale ? tr("llm.agents.modelWhy") : tr("llm.agents.noChatModel", { provider: shown });
  };

  // The checks are the runtime's catalog; a runtime switch starts from all of them, the stored
  // runtime from the agent's own list. A db agent's four are fixed: all taken, none to uncheck.
  let toolsFor                    = null;
  const paintTools = (r            )       => {
    if (toolsFor === r) return;
    toolsFor = r;
    const own = r === storedRuntime && e.tools ? e.tools : null;
    const fixed = r === "db";
    fill(tools, catalogOf(r).map((t) => checkField({
      label: h("code", null, t.name),
      control: h("input", { type: "checkbox", checked: fixed || (own ? own.includes(t.name) : true), disabled: fixed, data: { latool: t.name } }),
      hint: toolSummary(r, t),
    })));
    toolsWhy.textContent = tr(TOOLS_WHY[r]);
    toolsField.hidden = !catalogOf(r).length;
  };

  const paint = ()       => {
    const r = rt();
    runtimeWhy.textContent = tr(RUNTIME_WHY[r]);
    const asks = asksProvider(r);
    const isPi = r === "pi";
    askPart.hidden = !asks;
    cliPart.hidden = asks;
    turnsField.hidden = r !== "builtin" && r !== "db";
    secretsField.hidden = r !== "builtin";
    concurrency.placeholder = asks ? "4" : "1";
    concurrencyWhy.textContent = asks ? tr("llm.agents.concurrencyWhyAsk") : tr("llm.agents.concurrencyWhyCli");
    model.placeholder = isPi ? tr("llm.agents.piModelPh") : tr("llm.agents.claudeModelPh");
    modelWhy.textContent = isPi ? tr("llm.agents.piModelWhy") : tr("llm.agents.claudeModelWhy");
    command.placeholder = isPi ? PI_COMMAND : CLAUDE_COMMAND;
    const def = defaultPrompt();
    promptBase.textContent = def || "";
    promptBaseField.hidden = !def;
    prompt.placeholder = def ? tr("llm.agents.promptPhDefault") : tr("llm.agents.promptPhCli");
    promptWhy.textContent = def ? tr("llm.agents.promptWhyDefault") : tr("llm.agents.promptWhyCli");
    paintTools(r);
    report.disabled = isPi;
    if (isPi) report.checked = false;
    reportWhy.textContent = isPi ? tr("llm.agents.reportPi") : asks ? tr("llm.agents.reportSwiss") : tr("llm.agents.reportClaude");
  };

  /** The entry the form describes, or the sentence that refuses it. */
  const read = ()                         => {
    const r = rt();
    const next                = { runtime: r };
    if (asksProvider(r)) {
      if (!provider.value) return tr("llm.agents.needProvider");
      next.provider = provider.value;
      const ids = modelsOf(st, provider.value, "chat");
      if (!ids.length) return tr("llm.agents.noChatModel", { provider: provider.value });
      // A stored pick the provider no longer lists shows, so the select never lies, but the
      // server refuses it: say so here, in the panel's language.
      if (pick.value && !ids.includes(pick.value)) return tr("llm.agents.modelNotListed", { provider: provider.value, model: pick.value });
      if (pick.value) next.model = pick.value;
      const t = wholeField(maxTokens.value, tr("llm.agents.maxTokens"), TOKENS_MIN, TOKENS_MAX);
      if (isError(t)) return t.error;
      if (t != null) next.maxTokens = t;
      const k = wholeField(maxContext.value, tr("llm.agents.maxContext"), CONTEXT_KIB_MIN, CONTEXT_KIB_MAX);
      if (isError(k)) return k.error;
      if (k != null) next.maxContextKiB = k;
      if (r === "builtin" || r === "db") {
        const n = wholeField(maxTurns.value, tr("llm.agents.maxTurns"), 1, TURNS_MAX);
        if (isError(n)) return n.error;
        if (n != null) next.maxTurns = n;
      }
      if (r === "builtin") {
        const grant = secrets.value.split(",").map((s) => s.trim()).filter(Boolean);
        const bad = grant.find((g) => !grantable(g));
        if (bad) return tr("llm.agents.secretsBad", { entry: bad });
        if (grant.length) next.secrets = grant;
      }
      const picked = EFFORTS.find((x) => x === effort.value);
      if (picked) next.effort = picked;
    } else {
      const m = model.value.trim();
      if (r === "pi" && !m.includes("/")) return tr("llm.agents.piNeedsModel");
      if (m) next.model = m;
      const c = command.value.trim();
      if (c) next.command = c;
      const map = linesToMap(env.value);
      if (isError(map)) return map.error;
      const leak = credentialLeak(map, true);
      if (leak) return leak;
      if (Object.keys(map).length) next.env = map;
    }
    // A prompt equal to the default is no prompt of the agent's own: it keeps following it.
    const sp = prompt.value.trim();
    if (sp.length > PROMPT_MAX) return tr("llm.agents.promptLong", { max: PROMPT_MAX });
    if (sp && sp !== defaultPrompt()) next.systemPrompt = sp;
    // Every tool checked is no list at all, so a tool the runtime gains later is taken too.
    const boxes = Array.from(tools.querySelectorAll                  ("[data-latool]"));
    const taken = boxes.filter((b) => b.checked).map((b) => b.dataset.latool || "");
    if (boxes.length && taken.length < boxes.length) next.tools = taken;
    if (timeout.value.trim() === shownTimeout && e.timeoutMs != null) {
      next.timeoutMs = e.timeoutMs;
    } else {
      const s = wholeField(timeout.value, tr("llm.agents.timeout"), TIMEOUT_MIN_S, TIMEOUT_MAX_S);
      if (isError(s)) return s.error;
      if (s != null) next.timeoutMs = s * 1000;
    }
    const at = wholeField(concurrency.value, tr("llm.agents.concurrency"), 1, CONCURRENCY_MAX);
    if (isError(at)) return at.error;
    if (at != null) next.concurrency = at;
    // Reporting is on by default (off for pi, which cannot): only an opt-out is written.
    if (r !== "pi" && !report.checked) next.report = false;
    return next;
  };

  showSheet(sheet({
    title: name ? tr("llm.agents.editName", { name }) : tr("llm.agents.addTitle"),
    body: [
      pair(
        field({ label: tr("llm.name"), control: nameIn, required: true, hint: name ? tr("llm.nameKept") : tr("llm.nameWhy") }),
        field({ label: tr("llm.agents.runtime"), control: runtime })),
      runtimeWhy,
      formCap(tr("llm.agents.capModel")),
      askPart,
      cliPart,
      formCap(tr("llm.agents.capBehavior")),
      promptBaseField,
      field({ label: tr("llm.agents.systemPrompt"), control: prompt, hint: promptWhy }),
      toolsField,
      secretsField,
      formCap(tr("llm.agents.capRun")),
      pair(
        field({ label: tr("llm.agents.timeout"), control: timeout, hint: tr("llm.agents.timeoutWhy") }),
        field({ label: tr("llm.agents.concurrency"), control: concurrency, hint: concurrencyWhy })),
      checkField({ label: tr("llm.agents.report"), control: report, hint: reportWhy }),
      err,
    ],
    foot: [h("span", { class: "grow" }), cancel, save],
  }));
  paintModels();
  paint();
  runtime.onchange = paint;
  provider.onchange = paintModels;
  cancel.onclick = closeSheet;
  const onSave = async ()                => {
    say("");
    const n = name || nameIn.value.trim();
    if (!NAME_RE.test(n)) { say(tr("llm.badName")); return; }
    if (!name && n in stored) { say(tr("llm.agents.taken", { name: n })); return; }
    const next = read();
    if (typeof next === "string") { say(next); return; }
    save.disabled = true;
    try {
      const failure = await putRow((r) => { r.agents = { ...(r.agents || {}), [n]: next }; });
      if (failure) { say(failure); return; }
      closeSheet();
      toast(tr("llm.agents.saved", { name: n }));
      await refresh();
    } finally {
      save.disabled = false;
    }
  };
  save.onclick = ()       => { void onSave(); };
  (name ? runtime : nameIn).focus();
}

/** builtin runs the real loop - its tools, its turn cap, the dialogue filed as a run
 *  with turns; chat answers one prompt (it takes no tools by design); claude and pi prove
 *  the binary with --version and list the env it would get.
 *
 *  A provider-asking agent answers LIVE (SPEC §llm.api): server-sent events carry every
 *  happening as it happens, and the dialogue paints on the library's chat column - the
 *  typewriter. Reasoning streams into its dim turn, answer text into its own, tool asks
 *  land as chips, a tool's result is data, the closing answer renders as the markdown it
 *  is. The conversation outlives the sheet: the server holds it open in the llm history,
 *  the sheet restores the agent's open run on open, every ask names the run it continues,
 *  Stop cancels it (route and abort), and New closes it server-side before the column
 *  clears. A permanent refusal (a 409 carrying a code) locks the asks until New - a live
 *  conversation never silently forks onto a fresh run.
 *
 *  While a reply streams, Ask is Queue: the words wait above the prompt box in a queue
 *  (chatQueue), each to be edited, deleted, or steered now - ↑, or Ctrl/Cmd+Enter for all -
 *  into the live run's inbox, where the loop takes them after its tools answer, before it
 *  asks again (a `steer` frame paints them as the reader's turn). After a clean answer the
 *  first one goes as the next ask on its own, one per turn; Stop, a failure or a lock leaves
 *  the queue for the reader. */
function openTest(a             )       {
  const asks = a.runtime === "builtin" || a.runtime === "chat";
  const prompt = h("textarea", { id: "laTestPrompt", rows: 3, placeholder: tr("llm.test.promptPh") });
  const queued = chatQueue({
    id: "laTestQueued",
    max: ASK_MAX,
    onSteer: (q            )       => { steerNow(q); },
    onDelete: (q            )       => {
      // Steered and not read yet, it is withdrawn; read already, its steer frame paints it.
      if (q.steered && live && named && runId != null) void call("POST", "/api/llm/agents/test/steer/withdraw", { runId, id: q.id });
    },
    onEdited: ()       => {
      if (waiting && !live) { waiting = false; sendNext(); }
    },
  });
  const out = h("div", { id: "laTestOut" });
  const cwd = input("laTestCwd", "", tr("llm.test.cwdPh"));
  const toolPicks = catalogOf(a.runtime).map((t) => {
    const control = h("input", { type: "checkbox", value: t.name, checked: a.toolsInForce.includes(t.name), disabled: !a.toolsInForce.includes(t.name) });
    return { control, node: checkField({ label: t.name, control, hint: toolSummary(a.runtime, t) }) };
  });
  const pickedTools = h("span", null, a.toolsInForce.join(" · ") || tr("llm.agents.toolsNone"));
  const testTools = formFold({ summary: [tr("llm.test.tools"), " · ", pickedTools] },
    hint(tr("llm.test.toolsHint")),
    h("div", { id: "laTestTools" }, toolPicks.map((t) => t.node)),
    field({ label: tr("llm.test.cwd"), control: cwd, hint: tr("llm.test.cwdWhy") }));
  for (const pick of toolPicks) pick.control.onchange = ()       => {
    pickedTools.textContent = toolPicks.filter((p) => p.control.checked && !p.control.disabled).map((p) => p.control.value).join(" · ") || tr("llm.agents.toolsNone");
  };
  const close = btn(tr("llm.close"), { id: "laTestClose" });
  const fresh = btn(tr("llm.test.newChat"), { id: "laTestFresh", kind: "ghost", hidden: !asks });
  const run = btn(asks ? tr("llm.test.run") : tr("llm.agents.testVersion"), { id: "laTestRun", kind: "primary" });
  const stop = btn(tr("llm.test.stop"), { id: "laTestStop", kind: "ghost", hidden: true });
  showSheet(sheet({
    title: tr("llm.agents.testName", { name: a.name }),
    body: [
      hint(asks ? (a.reports === "swiss" ? tr("llm.agents.testReported") : tr("llm.agents.testUnreported")) : tr("llm.agents.testChild")),
      a.runtime === "builtin" ? testTools : null,
      out,
    ],
    compose: asks ? [queued.node, field({ label: tr("llm.test.prompt"), control: prompt })] : null,
    foot: [h("span", { class: "grow" }), fresh, close, run, stop],
    wide: asks,
  }));
  close.onclick = closeSheet;
  /** The conversation this sheet speaks in, as the server holds it: the run the last
   *  exchange left open (null: none yet). Every ask names it; New closes it. */
  let runId                = null;
  /** Bumped on Stop and New: a frame from an older loop paints nothing on a newer dialog. */
  let gen = 0;
  /** An ask is in flight: Ask and New are dead, Stop is armed. */
  let live = false;
  /** An ask has EVER painted on this column. Once one has, the opening restore is
   *  retired whole: a dialogue read that is still on the wire when the user asks
   *  would land its OLD turns over the NEW question and its streamed answer - and a
   *  discovery read landing late could re-commit a run id the live ask already
   *  replaced. The ask's own run frame is the sole authority from then on. */
  let asked = false;
  let controller                         = null;
  /** The live ask's run frame has named its run: the server holds its seat (and inbox)
   *  from that frame on, so steered words post only after it. */
  let named = false;
  /** A clean answer found the next queued message being edited: it goes when the edit ends. */
  let waiting = false;
  /** Ask while idle; Queue while a reply streams - Enter queues, Ctrl/Cmd+Enter steers all. */
  const label = ()       => {
    run.textContent = live ? tr("llm.test.queue") : tr("llm.test.run");
    if (live) run.title = tr("llm.test.queueTip");
    else run.removeAttribute("title");
    prompt.placeholder = live ? tr("ui.chat.queuePh") : tr("llm.test.promptPh");
  };
  const post = (q            )       => {
    // A refusal leaves it steered here: what the loop never read waits again when the ask ends.
    if (runId != null) void call("POST", "/api/llm/agents/test/steer", { runId, id: q.id, text: q.text });
  };
  /** ↑ on a queued row: into the live run's inbox (once its run frame named it); with nothing
   *  streaming it goes now, as the ask. */
  const steerNow = (q            )       => {
    if (!live) {
      if (run.disabled) return; // locked until New: it waits
      queued.remove(q.id);
      void ask(q.text);
      return;
    }
    queued.steer(q.id, true);
    if (named) post(q);
  };
  /** After a clean answer the first queued message goes as the next ask, by itself. */
  const sendNext = ()          => {
    const q = queued.items()[0]                          ;
    if (!q) return false;
    if (queued.editing() === q) { waiting = true; return false; }
    queued.remove(q.id);
    void ask(q.text);
    return true;
  };
  /** The ask ended: a steered message the loop never read waits again, first in line. */
  const unsteer = ()       => {
    for (const q of queued.items()) queued.steer(q.id, false);
  };
  /** The column the dialogue paints on, built on first paint. */
  const column = ()              => {
    if (!out.firstElementChild) fill(out, transcript([]));
    return out.firstElementChild               ;
  };
  /** One more turn on the column, kept in view - the typewriter's carriage. */
  const addTurn = (t                )              => {
    const node = transcriptTurn(t);
    column().appendChild(node);
    const scroller = out.closest(".sheet-body")                      ;
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    return node;
  };
  /** The server's open run for this agent, as the sheet opens. The id commits the
   *  moment discovery names it - BEFORE the dialogue loads - so New can close the run
   *  the sheet already knows is open even while its words are still on the wire; the
   *  dialogue then paints and the next ask continues the run. No open run is a clean
   *  column; a store that is not open says so. A run with a task is another surface's
   *  conversation (the Data agent's, one per connection), never the Test's. A dialogue that never arrives leaves
   *  the id DELIBERATELY held: an ask answers as a continuation of the run it names,
   *  never an unscoped fresh conversation, and New still closes it server-side. A New
   *  that lands while this restore is between awaits retires it whole: nothing it
   *  learned late may commit or paint (the run-history sig pattern). */
  const restoreOpen = async ()                => {
    const myGen = gen;
    try {
      const response = await fetch("/api/llm/history/runs?agent=" + encodeURIComponent(a.name) + "&open=true&page=0&pageSize=20");
      if (response.status === 503) {
        const why = await response.json().catch(()                     => ({}));
        if (gen !== myGen || asked) return; // a New reset the sheet mid-read, or an ask took the column: no late error paint
        fill(out, note(why.error || "", { err: true }));
        return;
      }
      if (!response.ok) return; // no open run for this agent - a clean column
      const found = await response.json().catch(()          => ({}))                                                          ;
      const open = (found.runs || []).find((r) => !r.task);
      if (!open) return;
      // Commit before the dialogue (discovery-time identity): a slow or missing
      // conversation must not stop New from closing what discovery says is open -
      // unless New or any Ask retired this restore. The Ask owns the identity even
      // before its first run frame, and keeps owning it after the stream finishes.
      if (gen !== myGen || asked) return;
      runId = open.id;
      const dialogue = await fetch("/api/llm/history/runs/" + open.id + "/dialogue");
      if (!dialogue.ok) return; // gone between the two reads: quiet, but the id stays held
      const d = await dialogue.json()               ;
      if (gen !== myGen || asked) return; // closed while the words loaded, or an ask is streaming: no resurrection, no overwrite
      if ((d.turns || []).length > 0) fill(out, conversationNode(d));
    } catch { /* offline: the column stays empty; an id already committed stays held */ }
  };
  if (asks) void restoreOpen();
  const onRun = async (steerAll = false)                => {
    if (!asks) {
      // A spawned runtime answers the blocking --version check, whole.
      run.disabled = true;
      fill(out, note(tr("llm.agents.testing"), { busy: true }));
      const r = await call              ("POST", "/api/llm/agents/test", { agent: a.name });
      run.disabled = false;
      fill(out, r.data ? testNode(r.data) : hint(r.err, { bad: true }));
      return;
    }
    if (run.disabled) return;
    // An empty prompt rides the agent's own default (the blocking Test's rule);
    // the column only carries the words the reader actually sent.
    const text = prompt.value.trim();
    if (live) {
      if (steerAll) {
        prompt.value = "";
        if (text) queued.add(text);
        for (const q of queued.items()) if (!q.steered && queued.editing() !== q) steerNow(q);
        return;
      }
      if (!text) return; // nothing to queue: an empty Queue is not the default prompt
      prompt.value = "";
      queued.add(text);
      return;
    }
    prompt.value = "";
    await ask(text);
  };
  const ask = async (text        )                => {
    asked = true;
    live = true;
    named = false;
    waiting = false;
    const myGen = gen;
    const mine = new AbortController();
    controller = mine;
    label();
    fresh.disabled = true;
    stop.hidden = false;
    stop.disabled = false;
    if (text) addTurn(promptTurn(text));
    const guarded = (t                )              => {
      // A stopped loop's late frame paints nothing (its gen moved on).
      return gen === myGen ? addTurn(t) : h("span", null);
    };
    const outcome = await streamTest(a.name, text, runId, {
      alive: ()          => gen === myGen,
      onRunId: (id        , continued         )       => {
        if (gen !== myGen) return;
        runId = id;
        if (!named) {
          // The seat is the server's from this frame: words steered before it post now.
          named = true;
          queued.items().filter((q) => q.steered).forEach(post);
        }
        if (continued) {
          const chip = addTurn({
            role: tr("llm.test.summary"),
            text: tr("llm.test.resume", { id: String(id) }),
            mono: true, dim: true,
          });
          chip.setAttribute("data-laresume", "");
        }
      },
      onSteer: (id        , text        )       => {
        if (gen !== myGen) return;
        queued.remove(id);
        const said = addTurn(promptTurn(text));
        said.setAttribute("data-lasteered", "");
      },
    }, guarded, mine.signal, {
      tools: toolPicks.map((t) => t.control).filter((t) => t.checked && !t.disabled).map((t) => t.value),
      cwd: cwd.value.trim(),
      defaults: a.toolsInForce,
    });
    if (gen === myGen) {
      live = false;
      label();
      stop.hidden = true;
      stop.disabled = true;
      fresh.disabled = false;
      // A permanent refusal (a code other than busy) locks the asks until New; the
      // run id STAYS - the conversation never silently forks onto a fresh one.
      run.disabled = outcome.locked === true;
      if (outcome.cancelled) {
        const said = addTurn({ role: tr("llm.history.role.assistant"), text: tr("llm.test.cancelled"), dim: true });
        said.setAttribute("data-lacancelled", "");
      } else if (outcome.error) {
        addTurn({ role: tr("llm.history.role.error"), text: outcome.error, mono: true });
      }
      if (outcome.ended) addTurn({ role: tr("llm.history.role.error"), text: tr("llm.test.streamEnded"), mono: true });
      // Every steer frame precedes done on the one stream, so a steered message still queued
      // the model never read. After a clean answer the next one goes; else the queue waits.
      unsteer();
      const clean = !outcome.cancelled && !outcome.error && !outcome.ended && !run.disabled;
      if (clean && sendNext()) return;
    }
    prompt.focus();
  };
  run.onclick = ()       => { void onRun(); };
  stop.onclick = ()       => {
    if (!live) return;
    // The loop stops here: its gen moves on (late frames paint nothing), its request
    // aborts, the server cancels by id, and the column says cancelled - never success.
    // The queue stays for the reader: none goes on its own, ↑ sends one.
    gen++;
    live = false;
    label();
    unsteer();
    stop.hidden = true;
    stop.disabled = true;
    run.disabled = false;
    fresh.disabled = false;
    controller?.abort();
    const id = runId;
    if (id != null) {
      void fetch("/api/llm/agents/test/cancel", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ runId: id }),
      }).catch(()       => { /* the history files what the loop managed */ });
    }
    // transcriptTurn() spreads its data record as raw props (transcript.ts), which
    // never reaches an attribute - markers ride on the returned node instead.
    const said = addTurn({ role: tr("llm.history.role.assistant"), text: tr("llm.test.cancelled"), dim: true });
    said.setAttribute("data-lacancelled", "");
  };
  const onFresh = async ()                => {
    if (live) return; // v1: New never abandons a live loop
    const id = runId;
    if (id != null) {
      try {
        const response = await fetch("/api/llm/history/runs/" + id + "/close", { method: "POST", headers: { "content-type": "application/json" } });
        if (!response.ok && response.status !== 404) {
          const why = await response.json().catch(()                     => ({}));
          toast(why.error || (response.status + " " + response.statusText), true);
          return; // an unknown failure keeps the column and says itself
        }
        if (response.status === 404) {
          // Gone on the server: clear anyway, with the server's word for why.
          const why = await response.json().catch(()                     => ({}));
          if (why.error) toast(why.error, true);
        }
      } catch { /* offline: the column still clears - the run is the server's */ }
      runId = null;
    }
    gen++;
    fill(out);
    run.disabled = false; // a locked sheet unlocks on New
    prompt.focus();
  };
  fresh.onclick = ()       => { void onFresh(); };
  prompt.addEventListener("keydown", (e               )       => {
    if (e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    if (!run.disabled) void onRun(e.ctrlKey || e.metaKey); // while live, Enter queues, Ctrl/Cmd+Enter steers all
  });
  run.focus();
}

/** The Test's ask on streamAsk: the agent, the words (an empty prompt rides the agent's default),
 *  the run it continues, and the tools and directory picked when they differ from the agent's. */
function streamTest(
  agent        ,
  prompt        ,
  runId               ,
  hooks             ,
  addTurn                                    ,
  signal             ,
  selection                                                      ,
)                         {
  const body                          = { agent };
  if (prompt) body.prompt = prompt;
  if (runId != null) body.runId = runId;
  if (selection.tools.join("\n") !== selection.defaults.join("\n")) body.tools = selection.tools;
  if (selection.cwd) body.cwd = selection.cwd;
  return streamAsk("/api/llm/agents/test/stream", body, hooks, addTurn, signal);
}

async function removeAgent(name        )                {
  if (!confirm(tr("llm.agents.deleteConfirm", { name }))) return;
  const failure = await putRow((r) => {
    r.agents = Object.fromEntries(Object.entries(r.agents || {}).filter(([k]) => k !== name));
  });
  if (failure) { toast(failure, true); return; }
  toast(tr("llm.agents.deleted", { name }));
  await refresh();
}

/** The telemetry block's sheet: the collector, the wire, the headers (credentials only as
 *  references) and whether prompt text rides along. Remove drops the block: nothing reports. */
async function openTelemetry()                {
  const got = await loadRow();
  if (!got.data) { toast(got.err, true); return; }
  const t                    = got.data.telemetry || { endpoint: "" };
  const had = !!got.data.telemetry;

  const preset = h("select", { id: "ltPreset" },
    Object.keys(COLLECTORS).map((k) => h("option", { value: k }, tr(COLLECTORS[k].label))));
  const enabled = h("input", { id: "ltEnabled", type: "checkbox", checked: t.enabled !== false });
  const protocol = h("select", { id: "ltProtocol" },
    (Object.keys(PROTOCOL_LABELS)                 ).map((p) => h("option", { value: p, selected: (t.protocol || "grpc") === p }, tr(PROTOCOL_LABELS[p]))));
  const endpoint = input("ltEndpoint", t.endpoint, tr("llm.telemetry.endpointPh"));
  const headers = h("textarea", { id: "ltHeaders", rows: 2, spellcheck: false, placeholder: tr("llm.telemetry.headersPh") }, mapToLines(t.headers));
  const service = input("ltService", t.serviceName, "swiss");
  const prompts = h("input", { id: "ltPrompts", type: "checkbox", checked: t.logPrompts === true });
  const endpointWhy = hint("", { id: "ltEndpointWhy" });
  const err = hint("", { id: "ltErr", bad: true, hidden: true, live: true });
  const say = (text        )       => { err.textContent = text; err.hidden = !text; };
  const removeBtn = btn(tr("llm.telemetry.remove"), { id: "ltRemove", kind: "danger", hidden: !had });
  const cancel = btn(tr("llm.cancel"), { id: "ltCancel" });
  const save = btn(tr("llm.save"), { id: "ltSave", kind: "primary" });
  const more = formFold({ summary: tr("llm.telemetry.more"), id: "ltMore" },
    field({ label: tr("llm.telemetry.headers"), control: headers, hint: tr("llm.telemetry.headersWhy") }),
    field({ label: tr("llm.telemetry.serviceName"), control: service, hint: tr("llm.telemetry.serviceNameWhy") }),
    checkField({ label: tr("llm.telemetry.logPrompts"), control: prompts, hint: tr("llm.telemetry.logPromptsWhy") }));
  if (t.headers || t.serviceName || t.logPrompts) more.setAttribute("open", "");
  const paint = ()       => {
    const grpc = protocol.value === "grpc";
    endpointWhy.textContent = grpc ? tr("llm.telemetry.endpointGrpc") : tr("llm.telemetry.endpointHttp");
  };
  showSheet(sheet({
    title: tr("llm.telemetry.title"),
    body: [
      hint(tr("llm.telemetry.intro")),
      had ? null : field({ label: tr("llm.telemetry.preset"), control: preset }),
      checkField({ label: tr("llm.telemetry.enabled"), control: enabled, hint: tr("llm.telemetry.enabledWhy") }),
      pair(
        field({ label: tr("llm.telemetry.protocol"), control: protocol }),
        field({ label: tr("llm.telemetry.endpoint"), control: endpoint, required: true })),
      endpointWhy,
      more,
      err,
    ],
    foot: [removeBtn, h("span", { class: "grow" }), cancel, save],
  }));
  paint();
  preset.onchange = ()       => {
    const c = preset.value in COLLECTORS ? COLLECTORS[preset.value] : COLLECTORS.custom;
    protocol.value = c.protocol;
    endpoint.value = c.endpoint;
    paint();
  };
  protocol.onchange = paint;
  cancel.onclick = closeSheet;
  const onSave = async ()                => {
    say("");
    if (!endpoint.value.trim()) { say(tr("llm.telemetry.needEndpoint")); return; }
    const next                    = { endpoint: endpoint.value.trim() };
    if (!enabled.checked) next.enabled = false;
    next.protocol = PROTOCOL_LABELS[protocol.value               ] ? protocol.value                : "grpc";
    const map = linesToMap(headers.value);
    if (isError(map)) { say(map.error); return; }
    const leak = credentialLeak(map, false);
    if (leak) { say(leak); return; }
    if (Object.keys(map).length) next.headers = map;
    if (service.value.trim()) next.serviceName = service.value.trim();
    if (prompts.checked) next.logPrompts = true;
    save.disabled = true;
    try {
      const failure = await putRow((r) => { r.telemetry = next; });
      if (failure) { say(failure); return; }
      closeSheet();
      toast(tr("llm.telemetry.saved"));
      await refresh();
    } finally {
      save.disabled = false;
    }
  };
  const onRemove = async ()                => {
    if (!confirm(tr("llm.telemetry.removeConfirm"))) return;
    const failure = await putRow((r) => { delete r.telemetry; });
    if (failure) { say(failure); return; }
    closeSheet();
    toast(tr("llm.telemetry.removed"));
    await refresh();
  };
  save.onclick = ()       => { void onSave(); };
  removeBtn.onclick = ()       => { void onRemove(); };
  (had ? endpoint : preset).focus();
}

/** One empty export to the saved collector: it stores nothing and proves the wire. */
async function testTelemetry()                {
  const b = $                   ("laTelTest");
  b.disabled = true;
  const r = await call              ("POST", "/api/llm/telemetry/test", {});
  b.disabled = false;
  if (!r.data) { toast(r.err, true); return; }
  if (r.data.ok) toast(tr("llm.telemetry.testOk", { endpoint: r.data.endpoint || "", ms: r.data.ms ?? 0 }));
  else toast(tr("llm.telemetry.testFailed", { error: r.data.error || "" }), true);
}

function onClick(event            )       {
  const t = targetEl(event);
  if (!t) return;
  if (t.closest("#laAdd") || t.closest("[data-empty-action]")) { void openAgent(null); return; }
  if (t.closest("#laTelEdit")) { void openTelemetry(); return; }
  if (t.closest("#laTelTest")) { void testTelemetry(); return; }
  const test = t.closest             ("[data-latest]");
  if (test) {
    const a = find(test.dataset.latest);
    if (a) openTest(a);
    return;
  }
  if (t.closest("[data-ladata]")) { location.hash = "#data"; return; }
  const more = t.closest             ("[data-lamore]");
  if (more) {
    // The opening click must not reach document (the menu closes on outside clicks).
    event.stopPropagation();
    const name = more.dataset.lamore || "";
    popupMenu(more.getBoundingClientRect(), [
      { label: tr("llm.edit"), fn: () => { void openAgent(name); } },
      { sep: true },
      { label: tr("llm.delete"), danger: true, fn: () => { void removeAgent(name); } },
    ]);
  }
}

/** Status and row together: the row says whether a telemetry block exists at all. Null when
 *  both answered for this visit; else why not ("" for an ended visit). */
async function load()                         {
  const at = visit;
  const [st, rw] = await Promise.all([loadStatus(), loadRow()]);
  if (at !== visit) return "";
  if (!st.data || !rw.data) return st.err || rw.err;
  status = st.data;
  hasBlock = !!rw.data.telemetry;
  return null;
}

export async function mount()                {
  visit++;
  painted = "";
  const at = visit;
  const failed = await load();
  if (at !== visit) return;
  if (failed !== null) {
    fill($("pane"), paneBody({ wide: true }, failNote({ text: tr("llm.loadFailed"), why: failed })));
    return;
  }
  render();
  // Sent here to make an agent (the Data page's "Make a DB agent"): its Add sheet, preset.
  const preset = takeAgentPreset();
  if (preset) void openAgent(null, preset);
}

export function unmount()       { visit++; }

export async function refresh()                {
  if (await load() !== null || !status) return; // keep the last paint; the next poll asks again
  if (signature(status) !== painted) render();
  else $("laTel").replaceWith(telemetryNode(status, hasBlock));
}

export async function poll()                { await refresh(); }

export function countText()         {
  const n = status ? status.agents.length : 0;
  return n ? trn(n, "llm.agents.n.one", "llm.agents.n.other") : "";
}

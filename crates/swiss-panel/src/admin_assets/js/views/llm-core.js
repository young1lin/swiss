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
   LLM - what the three pages (SPEC §llm.panel) share; not a page itself.

   One row holds every block. A page reads GET /api/llm/status to show what is in force (the
   resolved model, who reports a run, the exporter's counters) and edits the ROW through the
   host's config API - a fresh GET for the revision, the block changed, the CAS PUT - so a
   default the status filled in is never written back as if the operator had typed it.
   ================================================================================================ */
import { api } from "../util.js";
import { tk, tr } from "../i18n.js";
import { card, dot, kvRow } from "../ui/index.js";
                                                                                                                           

export const CONFIG_PATH = "/api/plugins/llm/config";

/** An agent's name, the server's rule: `[a-z0-9][a-z0-9-]{0,31}`. */
export const NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;

/** A provider's name, the server's rule: an agent's with capitals too (`GLM`, `DeepSeek`). */
export const PROVIDER_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9-]{0,31}$/;

/** A credential field's only legal shape: one `${secret://name}` or `${ENV_VAR}` reference. */
const REF_RE = /^\$\{(secret:\/\/[^}\s]+|[A-Za-z_][A-Za-z0-9_]*)\}$/;

                                                         

const RUNTIME_WORDS                             = {
  builtin: tk("llm.agents.word.builtin"),
  chat: tk("llm.agents.word.chat"),
  claude: tk("llm.agents.word.claude"),
  pi: tk("llm.agents.word.pi"),
  db: tk("llm.agents.word.db"),
};

/** A runtime in one word, for a row or a picker (the Agents page, memory's sync sheet); an
 *  unknown one (a newer server) is said as it came. */
export function runtimeWord(runtime        )         {
  const key = (RUNTIME_WORDS                                      )[runtime];
  return key ? tr(key) : runtime;
}

/** The catalog's tools in the panel's languages, by runtime and the runtime's own spelling. */
export const TOOL_SUMMARIES                         = {
  "builtin.read": tk("llm.tool.builtin.read"),
  "builtin.write": tk("llm.tool.builtin.write"),
  "builtin.edit": tk("llm.tool.builtin.edit"),
  "builtin.glob": tk("llm.tool.builtin.glob"),
  "builtin.grep": tk("llm.tool.builtin.grep"),
  "builtin.bash": tk("llm.tool.builtin.bash"),
  "builtin.sync": tk("llm.tool.builtin.sync"),
  "builtin.delegate": tk("llm.tool.builtin.delegate"),
  "builtin.job_output": tk("llm.tool.builtin.jobOutput"),
  "builtin.job_kill": tk("llm.tool.builtin.jobKill"),
  "claude.Read": tk("llm.tool.claude.read"),
  "claude.Grep": tk("llm.tool.claude.grep"),
  "claude.Glob": tk("llm.tool.claude.glob"),
  "pi.read": tk("llm.tool.pi.read"),
  "pi.grep": tk("llm.tool.pi.grep"),
  "pi.find": tk("llm.tool.pi.find"),
  "pi.ls": tk("llm.tool.pi.ls"),
  "db.list_tables": tk("llm.tool.db.listTables"),
  "db.describe_table": tk("llm.tool.db.describeTable"),
  "db.run_sql": tk("llm.tool.db.runSql"),
  "db.stage_sql": tk("llm.tool.db.stageSql"),
  "db.analysis": tk("llm.tool.db.analysis"),
  "db.list_collections": tk("llm.tool.db.listCollections"),
  "db.describe_collection": tk("llm.tool.db.describeCollection"),
  "db.run_mongo": tk("llm.tool.db.runMongo"),
  "db.stage_mongo": tk("llm.tool.db.stageMongo"),
  "db.scan_keys": tk("llm.tool.db.scanKeys"),
  "db.read_key": tk("llm.tool.db.readKey"),
  "db.read_stream": tk("llm.tool.db.readStream"),
  "db.list_channels": tk("llm.tool.db.listChannels"),
  "db.listen_channels": tk("llm.tool.db.listenChannels"),
  "db.stop_listen": tk("llm.tool.db.stopListen"),
  "db.run_redis": tk("llm.tool.db.runRedis"),
  "db.stage_redis": tk("llm.tool.db.stageRedis"),
  "db.open_console": tk("llm.tool.db.openConsole"),
  "db.list_console_tabs": tk("llm.tool.db.listConsoleTabs"),
  "db.read_console_tab": tk("llm.tool.db.readConsoleTab"),
  "db.edit_console_sql": tk("llm.tool.db.editConsoleSql"),
  "db.delete_console_sql": tk("llm.tool.db.deleteConsoleSql"),
  "db.read_memory": tk("llm.tool.db.readMemory"),
  "db.save_memory": tk("llm.tool.db.saveMemory"),
  "db.delete_memory": tk("llm.tool.db.deleteMemory"),
};

/** A tool's line in the panel's language when the panel knows the tool; a newer server's own
 *  words otherwise (the Agents page's catalog, the Data agent's tools line). */
export function toolSummary(runtime            , t             )         {
  const key = TOOL_SUMMARIES[runtime + "." + t.name];
  return key ? tr(key) : t.summary;
}

const KIND_WORDS                               = {
  chat: tk("llm.kind.chat"),
  embedding: tk("llm.kind.embedding"),
};

/** What a provider's model is for, in one word: a list caption, a picker's group. */
export function kindWord(kind              )         {
  return tr(KIND_WORDS[kind]);
}

/** One provider's models of one kind, in its order: the first is the default. */
export function modelsOf(status                  , provider        , kind              )           {
  const p = status ? status.providers.find((x) => x.name === provider) : undefined;
  return p ? p.models.filter((m) => m.kind === kind).map((m) => m.id) : [];
}

/** One request; a non-2xx is the server's `{error}` sentence, said where the form is (not a
 *  toast - a sheet stays open to fix it). */
export async function call   (method                        , path        , body          )                    {
  try {
    const r = await api(path, body === undefined ? { method } : { method, body: JSON.stringify(body) });
    const j          = await r.json().catch(()          => null);
    if (r.ok) return { data: j     , err: "" };
    const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : "";
    return { data: null, err: msg || tr("util.httpN", { n: r.status }) };
  } catch {
    return { data: null, err: tr("util.requestFailedGatewayRunning") };
  }
}

export async function loadStatus()                            {
  return call           ("GET", "/api/llm/status");
}

/** The agents a task may name (memory's sync, ask and review): every one but a db agent, which
 *  serves the Data page alone (SPEC §data.agent). None while the status is not read. */
export function taskAgents(s                  )                {
  return s ? s.agents.filter((a) => a.runtime !== "db") : [];
}

/** What the Agents page's next Add sheet starts as. */
;                                                                 

let agentPreset                     = null;

/** Set by a page that sends the reader to the Agents page to make an agent (the Data page's
 *  "Make a DB agent"); that page's next mount opens its Add sheet so, once. */
export function presetAgent(p             )       { agentPreset = p; }

export function takeAgentPreset()                     {
  const p = agentPreset;
  agentPreset = null;
  return p;
}

/** The row as stored: what an edit sheet prefills from. */
export async function loadRow()                         {
  const r = await call                            ("GET", CONFIG_PATH);
  return r.data ? { data: r.data.config || {}, err: "" } : { data: null, err: r.err };
}

/** Change the row and save it: a fresh GET right before the PUT (the CAS wants the newest
 *  revision, and every block this change does not touch rides back untouched). Null when it
 *  landed, else the sentence to show. */
export async function putRow(change                       )                         {
  const fresh = await call                                               ("GET", CONFIG_PATH);
  if (!fresh.data) return fresh.err;
  const row         = structuredClone(fresh.data.config || {});
  change(row);
  const put = await call                    ("PUT", CONFIG_PATH, { revision: fresh.data.revision, config: row });
  if (!put.data) return put.err;
  return put.data.error ? tr("llm.notApplied", { error: put.data.error }) : null;
}

/** An optional credential field: blank is none, a reference is kept, anything else is refused
 *  before it leaves the browser - a pasted key never reaches the wire, not even to be refused. */
export function refField(value        , label        )                                         {
  const v = value.trim();
  if (!v) return undefined;
  return REF_RE.test(v) ? v : { error: tr("llm.mustBeRef", { field: label }) };
}

/** An optional whole number in `min..=max`: blank is none. */
export function wholeField(value        , label        , min        , max        )                                         {
  const v = value.trim();
  if (!v) return undefined;
  const n = Number(v);
  if (!/^\d+$/.test(v) || n < min || n > max) return { error: tr("llm.wholeIn", { field: label, min, max }) };
  return n;
}

/** `NAME=value` lines into a map; a line without `=` is refused by its text. */
export function linesToMap(text        )                                             {
  const out                         = {};
  for (const line of text.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    const at = t.indexOf("=");
    if (at < 1) return { error: tr("llm.badLine", { line: t }) };
    out[t.slice(0, at).trim()] = t.slice(at + 1).trim();
  }
  return out;
}

/** The server's credential-name rule (swiss-llm names_credential): a variable or header so
 *  named takes a reference, whatever it holds. */
export function credentialNamed(name        )          {
  const upper = name.toUpperCase();
  return ["TOKEN", "KEY", "SECRET", "PASSWORD", "PASSWD", "AUTH", "CREDENTIAL"].some((w) => upper.includes(w));
}

/** A credential-named entry of an env or header map whose value is not a reference: the
 *  sentence that refuses it, said before the map leaves the browser. `whole`: the value must
 *  BE one reference (an env value); otherwise holding one is enough ("Bearer ${secret://x}"). */
export function credentialLeak(map                        , whole         )                {
  for (const [name, value] of Object.entries(map)) {
    if (!credentialNamed(name)) continue;
    const ok = whole ? REF_RE.test(value.trim()) : new RegExp(REF_RE.source.slice(1, -1)).test(value);
    if (!ok) return tr("llm.credentialNamed", { name });
  }
  return null;
}

export function mapToLines(map                                    )         {
  return Object.entries(map || {}).map(([k, v]) => k + "=" + v).join("\n");
}

export function isError(v         )                         {
  return typeof v === "object" && v !== null && "error" in v;
}

/** The host a URL sends text to, or null when it stays on this machine (the server's own rule:
 *  localhost, [::1] and 127.0.0.0/8 are loopback) or does not parse yet. */
export function remoteHost(url        )                {
  let host        ;
  try {
    host = new URL(url.trim()).hostname.toLowerCase();
  } catch {
    return null;
  }
  if (!host || host === "localhost" || host === "[::1]" || host.startsWith("127.")) return null;
  return host;
}

/** A Test's answer as one card: the outcome first, then whatever the route said. */
export function testNode(r              )              {
  const outcome = r.ok ? tr("llm.test.ok") : tr("llm.test.failed");
  return card(
    kvRow(tr("llm.test.outcome"), [dot(r.ok ? "up" : "error", outcome), " ", outcome]),
    r.model ? kvRow(tr("llm.test.model"), r.model, { mono: true }) : null,
    r.program ? kvRow(tr("llm.test.program"), r.program, { mono: true }) : null,
    r.protocol ? kvRow(tr("llm.test.collector"), r.protocol + " " + (r.endpoint || ""), { mono: true }) : null,
    r.dims ? kvRow(tr("llm.test.dims"), String(r.dims)) : null,
    r.turns != null ? kvRow(tr("llm.test.turns"), tr("llm.history.turnCount", { n: r.turns })) : null,
    r.tokensIn != null || r.tokensOut != null
      ? kvRow(tr("llm.test.tokens"), (r.tokensIn ?? 0) + " / " + (r.tokensOut ?? 0), { mono: true })
      : null,
    r.ms != null ? kvRow(tr("llm.test.took"), tr("llm.test.ms", { ms: r.ms })) : null,
    r.text ? kvRow(tr("llm.test.answer"), r.text) : null,
    r.env && r.env.length ? kvRow(tr("llm.test.env"), r.env.join(", "), { mono: true }) : null,
    r.session ? kvRow(tr("llm.test.session"), r.session, { mono: true, title: tr("llm.test.sessionWhy") }) : null,
    r.error ? kvRow(tr("llm.test.error"), r.error) : null);
}

/** A run another page asked to see (memory's sync sheet, SPEC §llm.history Live): navigation
 *  is hash-only, so the id rides here and the history page opens its sheet once it mounts. */
let wantedRun                = null;

/** Go to the LLM history page and open run `id` there. */
export function showRun(id        )       {
  wantedRun = id;
  location.hash = "#llm-history";
}

/** The run another page asked to see, once: null after the first take. */
export function takeWantedRun()                {
  const id = wantedRun;
  wantedRun = null;
  return id;
}

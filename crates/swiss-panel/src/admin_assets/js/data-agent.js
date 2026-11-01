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
   The Data agent (SPEC §data.agent): a db agent - the runtime made for this page and run by
   nothing else - docked at the Data page's end edge, working on the connection in front: a
   MySQL or PostgreSQL one, a MongoDB one or a Redis one, each kind with tools of its own. The
   reader asks in their own words; the agent reads how the data is laid out (tables, collections,
   keys), writes the statement or command in this connection's language and runs it read-only on
   the server (POST /api/llm/db/stream), and everything it runs lands on the page as the reader's
   own would - in its console, the answer in the results (data-sql.ts dbAgentStatement), its
   Agent tab unless it opened a console tab of its own under a name and named it. A write it only
   drafts there: the reader runs it - unless the reader set the connection's writes to auto (the
   Writes button beside the model, on a MySQL or PostgreSQL connection, kept per connection in
   this browser): then a narrow INSERT, REPLACE, UPDATE or DELETE runs and commits on its own
   while it changes at most the server's cap, and one the server holds lands in the console
   unrun, a toast saying why.

   One conversation per agent and connection, kept apart on the server (the run's task and
   subject) and here (one column per pair): switching the connection shows that connection's
   conversation, and a reply still streaming for another one never touches this page. The
   column outlives the page's repaints; the server's open run restores it on a fresh load.

   Around it: the tools the model may call, named above the column; words sent while a reply
   streams wait above the box in a queue (chatQueue) - edited, deleted, or steered into the
   reply now, else sent one per turn after it (the Agents Test's Queue);
   Under the box, beside Send, the model the conversation answers with and how hard it thinks
   (dsh's composer button): its menu's Model row opens every provider's chat models with a find
   box, its Effort row low to max. The pick is the reader's, kept per agent in this browser; it
   starts as the agent's own model, and its effort or else max. Every send, compaction and
   carry-on names it, and the server moves a conversation under way to it (SPEC §data.agent).
   History lists the connection's conversations and carries a closed one on; Memory lists the
   notes conversations on the connection saved about where its data lives and how to reach it
   (data-agent-memory.ts), the index of which a new conversation's opening carries; Compact (the head's
   button, or /compact [focus] in the box, which a "/" offers to complete) condenses the dialogue
   into a checkpoint the conversation carries on from. The box grows with its words. Under it the
   conversation's account (chatStats): its turns, steps and tokens, and how full the next request
   is against the agent's budget - each round's usage frame says it live, the context route
   (GET /api/llm/db/context) after a send and for a conversation restored or carried on.
   ================================================================================================ */
import { fill, h } from "./h.js";
import { tr } from "./i18n.js";
import { targetEl, toast } from "./util.js";
import {
  actionBtn, setActionBtn, btn, chatFollow, chatPane, chatQueue, chatStats, dot, emptyNode, factLine, flow, formActions, formCap, formFold, grip, gripFold, gripFolded, hint, iconBtn, iconNode, note,
  popupMenu, relTime, transcript, transcriptTurn,
} from "./ui/index.js";
                                                                                                                                  
                                                                          
import { dbConn, dbTab } from "./db-state.js";
import { dbSetGrep } from "./data-view.js";
import { dbSetTab } from "./data-structure.js";
import { dbAgentConsole, dbAgentConsoleEdit, dbAgentConsoleId, dbAgentStatement, dbConsoleTabs, dbRowLimit } from "./data-sql.js";
import { dbMemoryRefresh, dbMemoryView } from "./data-agent-memory.js";
import { dbOpenTab } from "./data-tabs.js";
import { dbAnalysisContext, dbAnalysisOpen } from "./data-analysis.js";
import { dbPubsubAgentRead, dbPubsubAgentStop } from "./data-pubsub.js";
import { mongoAgentSchema, mongoCurrentDb } from "./data-mongo.js";
import { call, loadStatus, presetAgent, toolSummary } from "./views/llm-core.js";
import { conversationNode, firstLine, runHow, sourceLabel } from "./views/llm-history.js";
                                                                                      
import { streamAsk } from "./views/llm-stream.js";

/** An element of the page by id, or null while the Data page is not on screen. */
function byId                                     (id        )           {
  return document.getElementById(id)            ;
}

const PICK_KEY = "swiss.dbAgent";
/** The model pick of an agent, under this key and its name. */
const MODEL_KEY = "swiss.dbAgent.model.";
/** How the agent's writes go on a connection (SPEC §data.agent Writes), under this key and its name. */
const WRITES_KEY = "swiss.dbAgent.writes.";
                               
const EFFORTS              = ["low", "medium", "high", "max"];
const ASK_MAX = 20_000;
/** The connection kinds the agent's tools speak, by dialect: each kind has its own. */
const AGENT_KINDS = new Map([["pg", "sql"], ["mysql", "sql"], ["mongo", "mongo"], ["redis", "redis"]]);
/** The box's one command: /compact, then what the checkpoint should keep in particular. */
const COMPACT_RE = /^\/compact(?:\s+([\s\S]*))?$/;

/** One conversation: an agent on a connection. */
                
                
               
                                                                      
                       
                                                                                   
              
                
                                                                                                
                      
                                                                      
                    
                                                                                         
                 
                     
                    
                                     
                                                                  
                   
                                                                                      
                 
                                                                                          
                                                                        
                   
                                                                                           
                   
                                                                                            
                           
                                                                                            
                   
                                                                                                     
                         
                                                                                              
                                                                                            
                      
 

const talks = new Map              ();
/** The db agents the status names (null: not read yet this visit); `why` when it failed. */
let agents                  = null;
let agentsWhy = "";
let agentsLoading = false;
/** The db runtime's tools, as the status names them: the line above every conversation. */
let catalog                = [];
let toolsOpen = false;

/** A model and how hard it thinks: what an agent answers with. */
                     
                   
                
                    
 

/** Each db agent's own provider, model and effort, as the status resolves them. */
const owns = new Map                                                                                     ();
/** Every provider's chat models in the status's order: what the Model menu offers, each with
 *  what its provider entry states (its name, window, whether it takes an effort). */
let offered                                                = [];

/** The offered model a pick names, when a provider still lists it. */
function offeredModel(provider                           , model                           )                          {
  return offered.find((p) => p.provider === provider)?.models.find((m) => m.id === model);
}

/** A model takes a thinking effort unless its entry says it takes none. */
function thinks(p           )          {
  return offeredModel(p.provider, p.model)?.thinking !== false;
}

/** What the pane calls a model: the name its entry gives it, else its id. */
function modelName(p           )         {
  return offeredModel(p.provider, p.model)?.name || p.model;
}

function talkOf(agent        , conn        )       {
  const key = agent + "\n" + conn;
  let t = talks.get(key);
  if (!t) {
    // The queue's hands reach the talk it is made for (built just below, before any click).
    const queue = chatQueue({
      max: ASK_MAX,
      onSteer: (q            )       => { steerNow(talk, q); },
      onDelete: (q            )       => {
        // Steered and not read yet, it is withdrawn; read already, its steer frame paints it.
        if (q.steered && talk.live && talk.named && talk.runId != null) void call("POST", "/api/llm/agents/test/steer/withdraw", { runId: talk.runId, id: q.id });
      },
      onEdited: ()       => {
        if (talk.waiting && !talk.live) { talk.waiting = false; sendNext(talk); }
      },
    });
    const talk       = {
      agent, conn, runId: null, gen: 0, live: false, compacting: false, stopping: false, asked: false, restoring: false, restored: false,
      controller: null, out: h("div"), named: false, queue, waiting: false, hist: null, usage: noUsage(), perByte: null,
      used: null,
    };
    t = talk;
    talks.set(key, t);
  }
  return t;
}

/** The connection in front when the agent can work on it - a MySQL, PostgreSQL, MongoDB or Redis
 *  one - with its kind (sql, mongo, redis). */
function agentConn()                                                       {
  const d = dbConn();
  const c = d.conns.find((x)          => x.name === d.conn);
  const kind = c ? AGENT_KINDS.get(c.dialect) : undefined;
  return c && kind ? { name: c.name, kind, label: c.label || c.name } : null;
}

/** The kind of a connection by name (sql, mongo, redis), in front or not: a queued send names
 *  its own. */
function kindOf(conn        )                     {
  const c = dbConn().conns.find((x)          => x.name === conn);
  return c ? AGENT_KINDS.get(c.dialect) : undefined;
}

/** A connection's writes: staged until the reader turns auto on for it. */
function readWrites(conn        )         {
  try { return localStorage.getItem(WRITES_KEY + conn) === "auto" ? "auto" : "stage"; } catch { return "stage"; }
}

function writeWrites(conn        , w        )       {
  try {
    if (w === "auto") localStorage.setItem(WRITES_KEY + conn, w);
    else localStorage.removeItem(WRITES_KEY + conn);
  } catch { /* the switch holds for this page */ }
}

function readPick()         {
  try { return localStorage.getItem(PICK_KEY) || ""; } catch { return ""; }
}

function writePick(name        )       {
  try { localStorage.setItem(PICK_KEY, name); } catch { /* the pick holds for this page */ }
}

/** The agent this pane speaks to: the reader's last pick, else one named db, else the first. */
function picked()                {
  if (!agents || !agents.length) return null;
  const mine = readPick();
  if (agents.includes(mine)) return mine;
  return agents.includes("db") ? "db" : agents[0];
}

function readModelPick(agent        )                     {
  try {
    const v          = JSON.parse(localStorage.getItem(MODEL_KEY + agent) || "null");
    return v && typeof v === "object" ? v                       : {};
  } catch { return {}; }
}

function writeModelPick(agent        , p           )       {
  try { localStorage.setItem(MODEL_KEY + agent, JSON.stringify(p)); } catch { /* the pick holds for this page */ }
}

/** What the agent answers with: the reader's pick while a provider still offers its model, else
 *  the agent's own; the effort the reader's, else the agent's, else max. Null while the status
 *  is not read or names no model for the agent. */
function modelOf(agent        )                   {
  const own = owns.get(agent);
  const mine = readModelPick(agent);
  const effort = EFFORTS.find((e) => e === mine.effort) || own?.effort || "max";
  const served = offeredModel(mine.provider, mine.model);
  if (served && mine.provider && mine.model) return { provider: mine.provider, model: mine.model, effort };
  return own?.provider && own.model ? { provider: own.provider, model: own.model, effort } : null;
}

/** The pick a request names: every route the pane calls moves the conversation to it. */
function modelBody(agent        )                          {
  const p = modelOf(agent);
  return p ? { provider: p.provider, model: p.model, effort: p.effort } : {};
}

/** An effort's word, on the button and in the menu. */
function effortWord(e           )         {
  return tr(e === "low" ? "dataAgent.effort.low" : e === "medium" ? "dataAgent.effort.medium" : e === "high" ? "dataAgent.effort.high" : "dataAgent.effort.max");
}

/** The footer's button as the agent's pick stands: its model and effort, hidden without one. */
function paintModel(agent               )       {
  const b = byId                   ("dbAgentModel");
  if (!b) return;
  const p = agent ? modelOf(agent) : null;
  b.hidden = !p;
  if (!p) return;
  // A model that takes no effort answers without one: the button names the model alone.
  fill(b, h("span", null, modelName(p) + (thinks(p) ? " " + effortWord(p.effort) : "")), iconNode("chevron-up"));
  b.title = p.model === modelName(p) ? tr("dataAgent.modelTip") : p.model + " - " + tr("dataAgent.modelTip");
}

/** The button's menu: the model and the effort, each row opening its own list over the button. */
function onModel(ev            )       {
  ev.stopPropagation();
  const b = byId                   ("dbAgentModel");
  const agent = picked();
  const p = agent ? modelOf(agent) : null;
  if (!b || !agent || !p) return;
  const choose = (next           )       => { writeModelPick(agent, next); paintModel(agent); };
  const items             = [
    {
      label: tr("dataAgent.modelRow", { model: modelName(p) }), title: p.model, affordance: "chevron-right",
      fn: ()       => {
        const rows             = [];
        for (const o of offered) {
          if (!o.models.length) continue;
          rows.push({ label: o.provider, heading: true, fn: ()       => { /* a heading is never chosen */ } });
          // A row reads the model's name, its id one hover away (and matched by the find box).
          for (const m of o.models) {
            rows.push({
              label: m.name || m.id, title: m.id, pick: true, on: o.provider === p.provider && m.id === p.model,
              fn: ()       => { choose({ ...p, provider: o.provider, model: m.id }); },
            });
          }
        }
        popupMenu(b.getBoundingClientRect(), rows, { filter: tr("dataAgent.modelFind") });
      },
    },
  ];
  // A model that takes no effort has none to pick: the row is left out, the pick kept for the
  // next model that does.
  if (thinks(p)) {
    items.push({
      label: tr("dataAgent.effortRow", { effort: effortWord(p.effort) }), affordance: "chevron-right",
      fn: ()       => {
        popupMenu(b.getBoundingClientRect(), EFFORTS.map((e)           => ({
          label: effortWord(e), pick: true, on: e === p.effort, fn: ()       => { choose({ ...p, effort: e }); },
        })));
      },
    });
  }
  popupMenu(b.getBoundingClientRect(), items);
}

/** The footer's Writes button as the connection's switch stands, on a SQL connection with an
 *  agent picked; hidden otherwise (MongoDB's and Redis's writes are always staged). */
function paintWrites(agent               , conn                                       )       {
  const b = byId                   ("dbAgentWrites");
  if (!b) return;
  b.hidden = !agent || !conn || conn.kind !== "sql";
  if (b.hidden || !conn) return;
  const w = readWrites(conn.name);
  fill(b, iconNode("pencil"), h("span", null, tr(w === "auto" ? "dataAgent.writes.auto" : "dataAgent.writes.stage")));
  b.setAttribute("data-writes", w);
  b.title = tr(w === "auto" ? "dataAgent.writes.autoTip" : "dataAgent.writes.stageTip");
}

/** The Writes menu: staged or auto, for the connection in front; the next send tells the model. */
function onWrites(ev            )       {
  ev.stopPropagation();
  const b = byId                   ("dbAgentWrites");
  const conn = agentConn();
  if (!b || !conn || conn.kind !== "sql") return;
  const now = readWrites(conn.name);
  const row = (w        )           => ({
    label: tr(w === "auto" ? "dataAgent.writes.autoRow" : "dataAgent.writes.stageRow"),
    title: tr(w === "auto" ? "dataAgent.writes.autoTip" : "dataAgent.writes.stageTip"),
    pick: true, on: w === now,
    fn: ()       => { writeWrites(conn.name, w); paintWrites(picked(), conn); },
  });
  popupMenu(b.getBoundingClientRect(), [row("stage"), row("auto")]);
}

/** A table the agent described (SPEC §data.agent): its tab in front on Structure, as the reader
 *  opening it and picking Structure would have it. A full strip that refused the tab leaves the
 *  tab in front alone. */
function dbAgentStructure(table        , schema               )       {
  dbOpenTab({ kind: "table", table, schema });
  const t = dbTab();
  if (t.kind === "table" && t.table === table && t.schema === schema) dbSetTab("structure");
}

/** What the page has in view for the tools' default: on SQL the open table's schema, else the
 *  sidebar's schema, else the database picked; on MongoDB the open collection's database, else
 *  the one the sidebar shows; on Redis nothing (the connection holds one logical database). */
function schemaInView(kind        )                     {
  const t = dbTab();
  if (kind === "redis") return undefined;
  if (kind === "mongo") return (t.kind === "coll" && t.db) || mongoCurrentDb() || undefined;
  if (t.kind === "table" && t.schema) return t.schema;
  const d = dbConn();
  return d.schemaFilter || d.database || undefined;
}

/** The edge and the pane, built once per page paint; the conversations live on in `talks`. */
let edge                     = null;
let stats                   = null;

function noUsage()            {
  return {
    turns: 0, steps: 0, tokensIn: 0, cacheRead: 0, cacheWrite: 0, tokensOut: 0,
    llmMs: 0, toolMs: 0, ttftMs: 0, ttftSteps: 0, decodeMs: 0, decodeTokens: 0, context: null,
  };
}

/** One round on the account: its tokens and time, and - when it said how long its first output
 *  took - that wait, and the output it wrote after it over the time that took. */
function addRound(u           , r                                                                                                             )       {
  u.steps++;
  u.tokensIn += r.tokensIn;
  u.cacheRead += r.cacheRead;
  u.cacheWrite += r.cacheWrite;
  u.tokensOut += r.tokensOut;
  u.llmMs += r.ms;
  if (r.ttftMs === undefined) return;
  u.ttftMs += r.ttftMs;
  u.ttftSteps++;
  u.decodeMs += Math.max(0, r.ms - r.ttftMs);
  u.decodeTokens += r.tokensOut;
}

/** The account a stored conversation gives: the reader's turns (a steer's among them), a step
 *  and its tokens and time per model call, and each tool's time - its answer is filed when it
 *  ends, the turn before it (the round's answer, or the tool before) when it began. */
function usageOfDialogue(d             )            {
  const u = noUsage();
  u.turns = d.turns.filter((t) => t.role === "user").length;
  for (const c of d.calls) {
    if (c.kind !== "chat") continue;
    addRound(u, {
      tokensIn: c.tokensIn || 0, cacheRead: c.tokensCacheRead || 0, cacheWrite: c.tokensCacheWrite || 0,
      tokensOut: c.tokensOut || 0, ms: c.ms || 0, ttftMs: typeof c.ttftMs === "number" ? c.ttftMs : undefined,
    });
  }
  d.turns.forEach((t, i) => {
    if (t.role !== "tool" || i === 0) return;
    const took = Date.parse(t.at) - Date.parse(d.turns[i - 1].at);
    if (took > 0) u.toolMs += took;
  });
  return u;
}

/** What a call's request counted, all of it: uncached, read from the cache, written to it. */
function promptTokens(c                                                                            )         {
  return (c.tokensIn || 0) + (c.tokensCacheRead || 0) + (c.tokensCacheWrite || 0);
}

/** The agent's budget (maxContextKiB) read as tokens: 250 a KiB, so the default 1024 KiB reads
 *  256K. The ring reads against it, or against the model's own context window where its
 *  provider entry states a smaller one - the window is where the provider refuses. */
const TOKENS_A_KIB = 250;

/** The tokens the ring reads against: the budget's, or the picked model's window when smaller. */
function contextLimit(agent        , cap        )         {
  const budget = Math.round(cap / 1024 * TOKENS_A_KIB);
  const p = modelOf(agent);
  const window = p ? offeredModel(p.provider, p.model)?.contextWindow : null;
  return window ? Math.min(window, budget) : budget;
}

/** The line under the box, for the conversation in front. */
function paintStats(talk      )       {
  if (!stats || !inFront(talk)) return;
  const u = talk.usage;
  const context                     = u.context && {
    ...u.context,
    limit: contextLimit(talk.agent, u.context.cap),
    used: talk.used ?? undefined,
    perByte: talk.perByte ?? undefined,
  };
  stats.set({ ...u, context });
}

/** One round's usage frame: a step and its tokens on the account, and the request it sent as
 *  the context - the provider's count over its bytes the tokens a byte from now on, its count
 *  with the answer where the next request starts. A compaction's round read the old dialogue,
 *  not the checkpoint the conversation carries on from (`carries` false): no count then. */
function tally(talk      , p                         , carries = true)       {
  const n = (key        )         => (typeof p[key] === "number" ? p[key]           : 0);
  const u = talk.usage;
  addRound(u, {
    tokensIn: n("tokensIn"), cacheRead: n("tokensCacheRead"), cacheWrite: n("tokensCacheWrite"),
    tokensOut: n("tokensOut"), ms: n("ms"), ttftMs: typeof p.ttftMs === "number" ? p.ttftMs : undefined,
  });
  const bytes = n("bytes");
  const prompt = n("tokensIn") + n("tokensCacheRead") + n("tokensCacheWrite");
  if (prompt > 0 && bytes > 0) talk.perByte = prompt / bytes;
  talk.used = carries && prompt > 0 ? prompt + n("tokensOut") : null;
  if (bytes > 0 && n("cap") > 0) u.context = { bytes, cap: n("cap"), system: n("system"), tools: n("tools") };
  paintStats(talk);
}

/** How full the conversation's next request starts, as the server weighs it (the context
 *  route): after a send, and for a conversation restored or carried on. `prompt`, the last
 *  call's count, gives the tokens a byte while no round of this visit has. A send that started
 *  meanwhile says it itself, frame by frame. */
async function weigh(talk      , prompt = 0)                {
  const id = talk.runId;
  if (id == null) return;
  const r = await call                      ("GET", "/api/llm/db/context?agent=" + encodeURIComponent(talk.agent) + "&conn=" + encodeURIComponent(talk.conn) + "&runId=" + id);
  const c = r.data;
  if (!c || typeof c.bytes !== "number" || typeof c.cap !== "number" || talk.runId !== id || talk.live) return;
  if (talk.perByte == null && prompt > 0 && c.bytes > 0) talk.perByte = prompt / c.bytes;
  talk.usage.context = { bytes: c.bytes, cap: c.cap, system: c.system || 0, tools: c.tools || 0 };
  paintStats(talk);
}

/** A stored conversation became the column: its account from its dialogue, then its weight. */
function account(talk      , d             )       {
  talk.usage = usageOfDialogue(d);
  const last = d.calls.filter((c) => c.kind === "chat").pop();
  // The last round's count is where the next request starts - unless the conversation was just
  // compacted: that round read the old dialogue, and nothing has counted the checkpoint yet.
  const asked = d.turns.filter((t) => t.role === "user" || t.role === "compact").pop();
  talk.used = last && asked?.role !== "compact" ? promptTokens(last) + (last.tokensOut || 0) : null;
  paintStats(talk);
  void weigh(talk, last && talk.used != null ? promptTokens(last) : 0);
}

/** The pane after the page's main column, its edge before it: folded until the reader opens it. */
function dbAgentPane()                {
  const ask = h("textarea", { id: "dbAgentAsk", rows: 2, maxLength: ASK_MAX, placeholder: tr("dataAgent.askPh"), aria: { label: tr("dataAgent.askPh") } });
  const pane = chatPane({
    id: "dbAgent",
    cap: tr("dataAgent.name"),
    tools: [
      h("select", { id: "dbAgentPick", aria: { label: tr("dataAgent.pick") } }),
      iconBtn("history", tr("dataAgent.history"), { id: "dbAgentHistory", ghost: true, pressed: false }),
      iconBtn("brain", tr("dataAgent.memory"), { id: "dbAgentMemory", ghost: true, pressed: false }),
      iconBtn("collapse", tr("dataAgent.compact"), { id: "dbAgentCompact", ghost: true }),
      iconBtn("plus", tr("dataAgent.new"), { id: "dbAgentNew", ghost: true }),
      iconBtn("x", tr("dataAgent.close"), { id: "dbAgentClose", ghost: true }),
    ],
    body: h("div", { id: "dbAgentBody" }),
    queued: h("div", { id: "dbAgentQueued" }),
    input: ask,
    // The box grows with its words; a "/" at its start offers the command (chatPane's own).
    commands: [{ name: "compact", arg: tr("dataAgent.cmdCompactArg"), hint: tr("dataAgent.cmdCompact") }],
    note: h("span", { id: "dbAgentNote" }),
    actions: [
      btn("", { id: "dbAgentWrites", kind: "ghost", icon: "pencil", hidden: true, title: tr("dataAgent.writes.stageTip") }),
      btn("", { id: "dbAgentModel", kind: "ghost", icon: "chevron-up", hidden: true, title: tr("dataAgent.modelTip") }),
      actionBtn("stop", tr("dataAgent.stop"), { id: "dbAgentStop", ghost: true, hidden: true }),
      btn(tr("dataAgent.send"), { id: "dbAgentSend", kind: "primary" }),
    ],
    stats: (stats = chatStats({ id: "dbAgentStats", note: tr("dataAgent.ctxNote") })).node,
  });
  edge = grip(pane, { axis: "x", key: "db-agent", min: 280, max: () => Math.max(280, Math.round(window.innerWidth * 0.6)), label: tr("dataAgent.name"), fold: true, folded: true, end: true });
  const pick = pane.querySelector                   ("#dbAgentPick");
  if (pick) pick.onchange = ()       => { writePick(pick.value); renderDbAgent(); };
  const model = pane.querySelector                   ("#dbAgentModel");
  if (model) {
    model.setAttribute("aria-haspopup", "menu");
    model.onclick = onModel;
  }
  const writes = pane.querySelector                   ("#dbAgentWrites");
  if (writes) {
    writes.setAttribute("aria-haspopup", "menu");
    writes.onclick = onWrites;
  }
  const send = pane.querySelector                   ("#dbAgentSend");
  if (send) send.onclick = ()       => { void onSend(); };
  const stop = pane.querySelector                   ("#dbAgentStop");
  if (stop) stop.onclick = onStop;
  const fresh = pane.querySelector                   ("#dbAgentNew");
  if (fresh) fresh.onclick = ()       => { void onNew(); };
  const history = pane.querySelector                   ("#dbAgentHistory");
  if (history) history.onclick = onHistory;
  const memory = pane.querySelector                   ("#dbAgentMemory");
  if (memory) memory.onclick = onMemory;
  const compactBtn = pane.querySelector                   ("#dbAgentCompact");
  if (compactBtn) compactBtn.onclick = ()       => { const talk = front(); if (talk) void compact(talk, ""); };
  const close = pane.querySelector                   ("#dbAgentClose");
  if (close) close.onclick = ()       => { if (edge) gripFold(edge, true); };
  ask.addEventListener("keydown", (e               )       => {
    if (e.key !== "Enter" || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    // While a reply streams, Enter queues and Ctrl/Cmd+Enter steers every queued message.
    void onSend(e.ctrlKey || e.metaKey);
  });
  return [edge, pane];
}

/** The head's button: opens the pane, or folds it away. */
function dbAgentButton()                    {
  const b = iconBtn("sparkles", tr("dataAgent.open"), { id: "dbAgentBtn", ghost: true });
  b.onclick = ()       => {
    if (!edge) return;
    const opening = gripFolded(edge);
    gripFold(edge, !opening);
    if (opening) byId                     ("dbAgentAsk")?.focus();
  };
  return b;
}

/** Paint the pane for the connection in front: its conversation with the picked agent (or its
 *  History, while the reader looks there), or why there is none. Called on every page paint and
 *  connection switch; the column, the History and the queue are the conversation's own, kept. */
function renderDbAgent()       {
  const pane = byId("dbAgent");
  const button = byId("dbAgentBtn");
  if (!pane) return;
  const conn = agentConn();
  // A connection the agent cannot work on takes the pane, its edge and its button away.
  pane.hidden = !conn;
  if (button) button.hidden = !conn;
  if (!conn) return;
  // One read per visit: a failed one stands (its reason shown) until the page is opened again.
  if (agents === null && !agentsLoading && !agentsWhy) { void loadAgents(); }
  const pick = byId                   ("dbAgentPick");
  const body = byId("dbAgentBody");
  const noteEl = byId("dbAgentNote");
  const queued = byId("dbAgentQueued");
  if (noteEl) { noteEl.textContent = conn.label; noteEl.title = conn.label; }
  const agent = picked();
  if (pick) {
    fill(pick, (agents || []).map((a) => h("option", { value: a, selected: a === agent }, a)));
    pick.hidden = !agents || agents.length < 2;
  }
  paintModel(agent);
  paintWrites(agent, conn);
  if (!body) return;
  if (!agent) {
    fill(body, agents === null
      ? (agentsWhy ? hint(tr("dataAgent.noLlm", { why: agentsWhy }), { bad: true }) : note(tr("dataAgent.loading"), { busy: true }))
      : noAgentNode());
    if (queued) fill(queued);
    stats?.set(null);
    controls(null);
    return;
  }
  const talk = talkOf(agent, conn.name);
  const changed = !body.contains(talk.out);
  if (talk.hist) fill(body, talk.hist);
  else fill(body, toolsNode(conn.kind), h("div", { id: "dbAgentEmpty", hidden: hasTurns(talk) }, hint(tr("dataAgent.empty"))), talk.out);
  if (queued) fill(queued, talk.queue.node);
  paintStats(talk);
  controls(talk);
  if (!talk.hist) follow(talk, changed);
  if (!talk.restored && !talk.restoring) void restore(talk);
}

/** The conversation's tools, above it: their names on the fold's line, what each does inside -
 *  what the model may call here, before the reader asks anything. The db catalog holds every
 *  kind's tools; this connection's kind picks its own (a tool naming no kind shows everywhere). */
function toolsNode(kind        )                     {
  const tools = catalog.filter((t) => !t.on || t.on === kind);
  if (!tools.length) return null;
  const fold = formFold({ id: "dbAgentTools", summary: [tr("dataAgent.tools"), " · ", tools.map((t) => t.name).join(" · ")] },
    hint(tr("dataAgent.toolsHint")),
    tools.map((t) => factLine([h("code", null, t.name), toolSummary("db", t)], { data: { dbtool: t.name } })))                      ;
  fold.open = toolsOpen;
  fold.ontoggle = ()       => { toolsOpen = fold.open; };
  return fold;
}

/** No db agent yet: what one is, and the way to make it - the Agents page's Add sheet, preset
 *  to the db runtime. A task agent never stands in: it serves its tasks, this page has its own. */
function noAgentNode()              {
  const node = emptyNode({ icon: "sparkles", title: tr("dataAgent.noAgentTitle"), hint: tr("dataAgent.noAgent"), action: tr("dataAgent.make") });
  node.id = "dbAgentNone";
  const make = node.querySelector                   ("[data-empty-action]");
  if (make) {
    make.onclick = ()       => {
      presetAgent({ runtime: "db", name: "db" });
      location.hash = "#llm-agents";
    };
  }
  return node;
}

/** Send (Queue while a reply streams), Stop, New, History, Memory and Compact as the
 *  conversation in front stands. A compaction takes no queued words: its round reads no inbox. */
function controls(talk             )       {
  const send = byId                   ("dbAgentSend");
  const stop = byId                   ("dbAgentStop");
  const fresh = byId                   ("dbAgentNew");
  const history = byId                   ("dbAgentHistory");
  const memory = byId                   ("dbAgentMemory");
  const compactBtn = byId                   ("dbAgentCompact");
  const ask = byId                     ("dbAgentAsk");
  const live = !!talk && talk.live;
  const queues = live && !talk.compacting;
  if (send) {
    send.disabled = !talk || talk.compacting;
    send.textContent = queues ? tr("llm.test.queue") : tr("dataAgent.send");
    if (queues) send.title = tr("llm.test.queueTip");
    else send.removeAttribute("title");
  }
  if (ask) ask.placeholder = queues ? tr("ui.chat.queuePh") : tr("dataAgent.askPh");
  if (stop) {
    stop.hidden = !live; stop.disabled = !live || talk.stopping;
    setActionBtn(stop, "stop", tr(talk?.stopping ? "memory.sync.run.stopping" : "dataAgent.stop"));
  }
  if (fresh) fresh.disabled = !talk || live;
  if (history) {
    history.disabled = !talk;
    history.setAttribute("aria-pressed", String(talk?.hist?.id === "dbAgentHist"));
  }
  if (memory) {
    memory.disabled = !talk;
    memory.setAttribute("aria-pressed", String(talk?.hist?.id === "dbAgentMem"));
  }
  if (compactBtn) compactBtn.disabled = !talk || live || talk.runId == null;
}

/** The conversation in front, when it is this one: what a late frame may paint on. */
function inFront(talk      )          {
  const conn = agentConn();
  return !!conn && conn.name === talk.conn && picked() === talk.agent && !!byId("dbAgent");
}

/** The conversation in front, or null while there is none (no connection the agent works on, no db agent). */
function front()              {
  const conn = agentConn();
  const agent = picked();
  return conn && agent ? talkOf(agent, conn.name) : null;
}

async function loadAgents()                {
  agentsLoading = true;
  const r = await loadStatus();
  agentsLoading = false;
  if (!r.data) { agentsWhy = r.err || tr("util.requestFailedGatewayRunning"); renderDbAgent(); return; }
  const all = Array.isArray(r.data.agents) ? r.data.agents : [];
  const mine = all.filter((a) => a.runtime === "db");
  agents = mine.map((a) => a.name);
  owns.clear();
  for (const a of mine) owns.set(a.name, { provider: a.provider, model: a.model, effort: a.effort ?? null });
  offered = (Array.isArray(r.data.providers) ? r.data.providers : [])
    .map((p) => ({ provider: p.name, models: p.models.filter((m) => m.kind !== "embedding") }));
  const tools = (r.data.catalog                                                      )?.db;
  catalog = Array.isArray(tools) ? tools : [];
  agentsWhy = "";
  renderDbAgent();
}

/** The note the server adds after the reader's words when the conversation moved to another
 *  model or its writes were switched (SPEC §data.agent): the model's to read, not the reader's. */
const MOVED_RE = /\n\n<environment>\nThe (?:model answering|write mode) changed since you were last told\.\n[\s\S]*<\/environment>$/;

/** The reader's question as they typed it: a pasted statement keeps its _ and its *, which
 *  markdown would take as escapes and emphasis. Its Copy gives the words back as sent. */
function askedTurn(text        )                 {
  const said = text.replace(MOVED_RE, "");
  return { side: "user", role: tr("llm.history.role.user"), text: said, copy: said };
}

/** The column a turn lands on, built on first paint. */
function column(talk      )              {
  if (!talk.out.firstElementChild) fill(talk.out, transcript([]));
  return talk.out.firstElementChild               ;
}

/** Whether this conversation has said anything yet — the one condition the empty-state
 *  guidance answers to. `talk.out.firstElementChild` is the transcript CONTAINER, which
 *  `column()` creates on the first paint whether or not it holds a turn, so testing it hid the
 *  guidance the moment the pane opened — and `follow`, called in the same tick, hid it again for
 *  good (2026-11-01). */
function hasTurns(talk      )          {
  const col = talk.out.firstElementChild;
  return !!col && col.childElementCount > 0;
}

/** Keep the end of the column in view while it is the one on screen. */
function follow(talk      , force = false)       {
  if (!inFront(talk) || talk.hist) return;
  const empty = byId("dbAgentEmpty");
  if (empty && hasTurns(talk)) empty.hidden = true;
  const pane = byId("dbAgent");
  if (pane) chatFollow(pane, force);
}

function addTurn(talk      , t                )              {
  const node = transcriptTurn(t);
  column(talk).appendChild(node);
  follow(talk);
  return node;
}

/** The connection's open conversation with this agent, as the server holds it. */
async function openRun(talk      )                                     {
  try {
    const r = await fetch("/api/llm/db/conversation?agent=" + encodeURIComponent(talk.agent) + "&conn=" + encodeURIComponent(talk.conn));
    if (!r.ok) return undefined;
    const j = await r.json()                             ;
    return typeof j.runId === "number" ? j.runId : null;
  } catch { return undefined; }
}

/** A run's dialogue as the history keeps it, or why it could not be read. */
async function dialogueOf(id        )                                                  {
  try {
    const r = await fetch("/api/llm/history/runs/" + id + "/dialogue");
    if (!r.ok) {
      const j = await r.json().catch(()                     => ({}))                      ;
      return { d: null, why: j.error || (r.status + " " + r.statusText) };
    }
    return { d: await r.json()               , why: "" };
  } catch (err) {
    return { d: null, why: err instanceof Error ? err.message : String(err) };
  }
}

/** The opening restore: the open run's id commits before its words load (so New can close it),
 *  and the words paint unless a send or New took the column meanwhile. */
async function restore(talk      )                {
  talk.restoring = true;
  const myGen = talk.gen;
  // Read at each step: a send or New may land while a read is on the wire.
  const retired = ()          => talk.gen !== myGen || talk.asked;
  try {
    const id = await openRun(talk);
    if (id === undefined) return; // unknown: the next look tries again
    talk.restored = true;
    if (id === null || retired()) return;
    talk.runId = id;
    const r = await fetch("/api/llm/history/runs/" + id + "/dialogue");
    if (!r.ok) return;
    const d = await r.json()               ;
    if (retired()) return;
    if (d.turns.length > 0) {
      fill(talk.out, conversationNode(d, undefined, askedTurn));
      if (inFront(talk)) renderDbAgent();
    }
    account(talk, d);
  } catch { /* offline: the column stays as it is */ } finally {
    talk.restoring = false;
  }
}

/** The box's words: a question (queued while a reply streams), or /compact [focus]. With
 *  `steerAll` (Ctrl/Cmd+Enter) while a reply streams, the words join the queue and every
 *  queued message is steered into the reply now. */
async function onSend(steerAll = false)                {
  const ask = byId                     ("dbAgentAsk");
  const talk = front();
  if (!ask || !talk || talk.compacting) return;
  const text = ask.value.trim();
  if (steerAll && talk.live) {
    if (COMPACT_RE.test(text)) { toast(tr("dataAgent.compactLive"), true); return; }
    ask.value = "";
    if (text) talk.queue.add(text);
    for (const q of talk.queue.items()) {
      if (!q.steered && talk.queue.editing() !== q) steerNow(talk, q);
    }
    return;
  }
  if (!text) return;
  const command = COMPACT_RE.exec(text);
  if (command) {
    // A command never waits in the queue: the model would read it as words.
    if (talk.live) { toast(tr("dataAgent.compactLive"), true); return; }
    if (talk.runId == null) { toast(tr("dataAgent.compactNone"), true); return; }
    ask.value = "";
    await compact(talk, (command[1] || "").trim());
    return;
  }
  ask.value = "";
  // While a reply streams the words wait in the queue, posted nowhere until steered.
  if (talk.live) { talk.queue.add(text); return; }
  await send(talk, text);
}

/** ↑ on a queued row (or Ctrl/Cmd+Enter for all): into the live reply's inbox once its run
 *  frame named the run - the loop takes it after its tools answer, before it asks again, and a
 *  steer frame paints it as the reader's turn. With nothing streaming it goes now, as the ask;
 *  during a compaction it waits first in line. */
function steerNow(talk      , q            )       {
  if (talk.compacting) return;
  if (!talk.live) {
    talk.queue.remove(q.id);
    void send(talk, q.text);
    return;
  }
  talk.queue.steer(q.id, true);
  if (talk.named) post(talk, q);
}

function post(talk      , q            )       {
  // A refusal leaves it steered here: what the loop never read waits again when the reply ends.
  if (talk.runId != null) void call("POST", "/api/llm/agents/test/steer", { runId: talk.runId, id: q.id, text: q.text });
}

/** After a clean answer the first queued message goes as the next ask, by itself - one per
 *  turn, in order. While the reader edits it, it goes when the edit ends. */
function sendNext(talk      )          {
  const q = talk.queue.items()[0]                          ;
  if (!q) return false;
  if (talk.queue.editing() === q) { talk.waiting = true; return false; }
  talk.queue.remove(q.id);
  void send(talk, q.text, false);
  return true;
}

/** The reply ended: a steered message the loop never read waits again, first in line. */
function unsteer(talk      )       {
  for (const q of talk.queue.items()) talk.queue.steer(q.id, false);
}

async function send(talk      , text        , reveal = true)                {
  talk.hist = null;
  talk.asked = true;
  talk.live = true;
  talk.named = false;
  talk.waiting = false;
  const myGen = ++talk.gen;
  const mine = new AbortController();
  talk.controller = mine;
  talk.usage.turns++;
  if (inFront(talk)) renderDbAgent();
  addTurn(talk, askedTurn(text));
  // A queued send respects a reader inspecting history; an explicit send reveals its words.
  if (reveal) follow(talk, true);
  const body                          = { agent: talk.agent, conn: talk.conn, prompt: text, rowLimit: dbRowLimit(), ...modelBody(talk.agent) };
  if (talk.runId != null) body.runId = talk.runId;
  // Every SQL send names the connection's writes: the server tells the model when they changed.
  if (kindOf(talk.conn) === "sql") body.writes = readWrites(talk.conn);
  // The page's view is the default only while this connection is in front: a queued ask that
  // goes on after the reader left it names none.
  const conn = agentConn();
  const schema = conn && conn.name === talk.conn ? schemaInView(conn.kind) : undefined;
  if (schema) body.schema = schema;
  // The console's tabs exist only here, so each send carries them (open and closed): the ids,
  // names and statements list_console_tabs, read_console_tab and open_console's tabId read.
  // None to send - nothing open, nothing in Recently closed - is the same as an empty list.
  const consoleTabs = conn && conn.name === talk.conn ? dbConsoleTabs() : [];
  if (consoleTabs.length) body.consoleTabs = consoleTabs;
  const analysisView = conn && conn.name === talk.conn ? dbAnalysisContext() : null;
  if (analysisView) body.analysisView = analysisView;
  // A send's first statement in a console tab starts its results over; the ones after it gather beside it.
  const ran = new Set        ();
  const outcome = await streamAsk("/api/llm/db/stream", body, {
    alive: ()          => talk.gen === myGen,
    onPaint: ()       => { follow(talk); },
    onRunId: (id        )       => {
      if (talk.gen !== myGen) return;
      talk.runId = id;
      if (!talk.named) {
        // The seat is the server's from this frame: words steered before it post now.
        talk.named = true;
        talk.queue.items().filter((q) => q.steered).forEach((q) => { post(talk, q); });
      }
    },
    onSteer: (id        , said        )       => {
      if (talk.gen !== myGen) return;
      talk.queue.remove(id);
      talk.usage.turns++;
      paintStats(talk);
      addTurn(talk, askedTurn(said)).setAttribute("data-dbsteered", "");
    },
    onToolResult: (payload                         )       => {
      if (talk.gen !== myGen || typeof payload.ms !== "number") return;
      talk.usage.toolMs += payload.ms;
      paintStats(talk);
    },
    onFrame: (name        , payload                         )       => {
      if (talk.gen !== myGen) return;
      if (name === "usage") { tally(talk, payload); return; }
      if (name !== "db") return;
      // The agent saved or deleted a note of the connection's memory: its Memory view, when
      // that is what the column shows, reads again.
      if (payload.kind === "memory") { dbMemoryRefresh(talk.hist); return; }
      // A statement for a connection the page has left stays in its own conversation.
      if (dbConn().conn !== talk.conn) return;
      if (payload.kind === "analysis" && typeof payload.taskId === "string") {
        void dbAnalysisOpen(payload.taskId, talk.conn);
        return;
      }
      // What the agent reads the reader sees (SPEC §data.agent): a key or stream it read opens
      // in its own tab, a channel listing or a listen opens the Pub/Sub tab on what it read.
      if (payload.kind === "key") {
        const key = typeof payload.key === "string" ? payload.key : "";
        if (key) dbOpenTab({ kind: "key", key, type: typeof payload.type === "string" ? payload.type : undefined });
        return;
      }
      // The page's own live feed, ended on the agent's ask (stop_listen): the reader's Stop, and
      // the only handle on a listen this conversation never held.
      if (payload.kind === "pubsubListen") {
        dbPubsubAgentStop();
        return;
      }
      if (payload.kind === "channels" || payload.kind === "listen") {
        dbOpenTab({ kind: "pubsub" });
        dbPubsubAgentRead(payload);
        return;
      }
      // A listing shows as the tree it found: the sidebar's search takes the agent's grep (a
      // MongoDB one while the tree shows that database). A table it described opens on its
      // Structure, a collection on its Schema.
      if (payload.kind === "list") {
        const grep = typeof payload.grep === "string" ? payload.grep : "";
        if (typeof payload.db !== "string" || payload.db === mongoCurrentDb()) dbSetGrep(grep);
        return;
      }
      if (payload.kind === "table") {
        const table = typeof payload.table === "string" ? payload.table : "";
        if (table) dbAgentStructure(table, typeof payload.schema === "string" && payload.schema ? payload.schema : null);
        return;
      }
      if (payload.kind === "coll") {
        if (typeof payload.db === "string" && typeof payload.coll === "string" && payload.coll) mongoAgentSchema(payload.db, payload.coll);
        return;
      }
      // A console tab of the agent's own, by the name it gave (SPEC §data.agent Tabs) - or by
      // the id that outlives a close and a reload - or one statement of one rewritten or
      // dropped. A run or stage naming it lands there, one naming none in the Agent tab.
      const tab = typeof payload.tab === "string" ? payload.tab : undefined;
      // The tab's id, when the server knows it: the address that tells two consoles of one name
      // apart and survives a rename (SPEC §data.agent Tabs).
      const tabId = typeof payload.tabId === "string" && payload.tabId ? payload.tabId : undefined;
      if (payload.kind === "console") {
        if (tabId) dbAgentConsoleId(tabId, tab);
        else if (tab) dbAgentConsole(tab);
        return;
      }
      if (payload.kind === "consoleEdit") { dbAgentConsoleEdit(payload); return; }
      // The statement, the MongoDB command document or the Redis line; a MongoDB one names its database.
      const sql = typeof payload.sql === "string" ? payload.sql : "";
      if (!sql) return;
      const db = typeof payload.db === "string" ? payload.db : undefined;
      if (payload.kind === "stage") {
        dbAgentStatement(sql, null, ran, db, tab, tabId);
        // An auto write the server held: it waits in the console, and the reader hears why.
        if (typeof payload.held === "string") toast(tr("dataAgent.writes.held", { why: payload.held }), true);
        return;
      }
      const reply = payload.reply && typeof payload.reply === "object" ? payload.reply                            : undefined;
      dbAgentStatement(sql, reply ? { reply } : { error: typeof payload.error === "string" ? payload.error : "" }, ran, db, tab, tabId);
    },
  }, (t                )              => (talk.gen === myGen ? addTurn(talk, t) : h("span")), mine.signal);
  if (talk.gen !== myGen) return; // Stop or New already settled the column
  talk.live = false;
  talk.controller = null;
  if (outcome.cancelled) addTurn(talk, { role: tr("llm.history.role.assistant"), text: tr("llm.test.cancelled"), dim: true });
  else if (outcome.error) addTurn(talk, { role: tr("llm.history.role.error"), text: outcome.error, mono: true });
  if (outcome.ended) addTurn(talk, { role: tr("llm.history.role.error"), text: tr("llm.test.streamEnded"), mono: true });
  // A refusal over the conversation (closed elsewhere, or one opened elsewhere): take the
  // server's word for which run is open, so the next send speaks in it.
  if (outcome.locked) {
    const id = await openRun(talk);
    if (id !== undefined) talk.runId = id;
  }
  // What the next send starts from: the answer and the tools' results now in the dialogue.
  void weigh(talk);
  // Every steer frame precedes done on the one stream, so a steered message still queued the
  // model never read. After a clean answer the next one goes; after a failure the queue waits
  // for the reader.
  unsteer(talk);
  const clean = !outcome.cancelled && !outcome.error && !outcome.ended;
  if (clean && sendNext(talk)) return;
  if (inFront(talk)) {
    controls(talk);
    byId                     ("dbAgentAsk")?.focus();
  }
}

/** Compact the conversation (SPEC §data.agent Compact): the model condenses it into a
 *  checkpoint, streamed under a quiet line where the next turns go, and the dialogue carries on
 *  from that checkpoint in a new run - the condensed turns stay on the column and in History.
 *  `focus` is what the reader typed after /compact. Stop asks the server to cancel and the
 *  stream is read to its end: the server reopens the old conversation first, then says so. */
async function compact(talk      , focus        )                {
  if (talk.live || talk.runId == null) return;
  const from = talk.runId;
  talk.hist = null;
  talk.asked = true;
  talk.live = true;
  talk.compacting = true;
  talk.stopping = false;
  talk.named = false;
  const myGen = ++talk.gen;
  if (inFront(talk)) renderDbAgent();
  const streamed = h("div");
  const box = h("div", { data: { dbcompact: String(from) } }, note(tr("dataAgent.compacting"), { busy: true }), streamed);
  addTurn(talk, { role: null, code: [box] });
  const body                          = { agent: talk.agent, conn: talk.conn, runId: from, ...modelBody(talk.agent) };
  if (focus) body.focus = focus;
  const outcome = await streamAsk("/api/llm/db/compact", body, {
    alive: ()          => talk.gen === myGen,
    onRunId: (id        )       => {
      if (talk.gen !== myGen) return;
      talk.runId = id;
      if (!talk.named) {
        talk.named = true;
        if (talk.stopping) cancelRun(id);
      }
    },
    onFrame: (name        , payload                         )       => {
      if (name === "usage" && talk.gen === myGen) tally(talk, payload, false);
    },
  }, (t                )              => {
    if (talk.gen !== myGen) return h("span");
    const node = transcriptTurn(t);
    streamed.appendChild(node);
    follow(talk);
    return node;
  }, new AbortController().signal);
  if (talk.gen !== myGen) return;
  talk.live = false;
  talk.compacting = false;
  talk.stopping = false;
  if (!outcome.cancelled && !outcome.error && !outcome.ended) {
    fill(box, factLine([tr("llm.history.compacted"), tr("llm.history.compactedFrom", { id: String(from) })]),
      formFold({ summary: tr("llm.history.checkpoint") }, ...Array.from(streamed.childNodes)));
  } else {
    // The old conversation reopened (done named it), or the server could not: its word decides.
    const id = await openRun(talk);
    if (id !== undefined) talk.runId = id;
    fill(box, outcome.cancelled
      ? note(tr("dataAgent.compactCancelled"))
      : note(outcome.error || tr("llm.test.streamEnded"), { err: true }));
  }
  // The checkpoint the conversation carries on from is what the next send starts with.
  void weigh(talk);
  if (inFront(talk)) {
    controls(talk);
    byId                     ("dbAgentAsk")?.focus();
  }
}

function cancelRun(id        )       {
  void fetch("/api/llm/agents/test/cancel", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ runId: id }),
  }).catch(()       => { /* the history files what the loop managed */ });
}

function onStop()       {
  const talk = front();
  if (!talk || !talk.live) return;
  if (talk.compacting) {
    // The server undoes a stopped compaction and says so in done: read on to it.
    if (talk.stopping) return;
    talk.stopping = true;
    if (talk.named && talk.runId != null) cancelRun(talk.runId);
    controls(talk);
    return;
  }
  // The send stops here: its gen moves on (late frames paint nothing), its request aborts, the
  // server cancels by id, and the column says cancelled. The queue stays for the reader: none
  // goes on its own, ↑ sends one.
  talk.gen++;
  talk.live = false;
  talk.controller?.abort();
  talk.controller = null;
  unsteer(talk);
  if (talk.runId != null) cancelRun(talk.runId);
  addTurn(talk, { role: tr("llm.history.role.assistant"), text: tr("llm.test.cancelled"), dim: true });
  controls(talk);
}

async function onNew()                {
  const talk = front();
  if (!talk || talk.live) return;
  const id = talk.runId;
  if (id != null) {
    try {
      const r = await fetch("/api/llm/history/runs/" + id + "/close", { method: "POST", headers: { "content-type": "application/json" } });
      if (!r.ok && r.status !== 404) {
        const why = await r.json().catch(()                     => ({}));
        toast(why.error || (r.status + " " + r.statusText), true);
        return;
      }
    } catch { /* offline: the column still clears - the run is the server's */ }
  }
  talk.runId = null;
  talk.gen++;
  talk.restored = true;
  talk.hist = null;
  talk.usage = noUsage();
  talk.used = null;
  fill(talk.out);
  if (inFront(talk)) {
    renderDbAgent();
    byId                     ("dbAgentAsk")?.focus();
  }
}

/** The head's History: this connection's conversations with the agent, or back to the one in
 *  front. */
function onHistory()       {
  const talk = front();
  if (!talk) return;
  if (talk.hist?.id === "dbAgentHist") talk.hist = null;
  else void showHistory(talk);
  renderDbAgent();
}

/** The head's Memory: the connection's notes in the column, or back to the conversation. */
function onMemory()       {
  const talk = front();
  if (!talk) return;
  talk.hist = talk.hist?.id === "dbAgentMem" ? null : dbMemoryView(talk.conn, ()       => {
    talk.hist = null;
    if (inFront(talk)) renderDbAgent();
  });
  renderDbAgent();
}

/** One conversation of the History in a quiet line: its state, what it was first asked (a
 *  compaction's run, the one it condensed), how it ran, when it opened. */
function historyStep(r           , open               )           {
  const state = r.live ? dot("starting", tr("llm.history.running"))
    : r.ok === false ? dot("down", tr("llm.history.failed"))
    : r.atEnd == null ? dot("up", tr("llm.history.runOpen"))
    : dot("off", tr("llm.history.runClosed"));
  const head = firstLine(r.head || "").slice(0, 120);
  const from = sourceLabel(r.source);
  return {
    id: String(r.id),
    lead: state,
    title: head || from || tr("llm.history.conversation"),
    summary: [head ? from : "", runHow(r), r.id === open ? tr("dataAgent.current") : ""].filter(Boolean).join(" · "),
    meta: relTime(Date.parse(r.at) || 0),
    data: { dbhist: String(r.id) },
  };
}

/** The History list, page by page (20 to a page, newest first); a line opens its dialogue. */
async function showHistory(talk      )                {
  const list = h("div", { data: { dbhistlist: "" } });
  const more = btn(tr("dataAgent.historyMore"), { kind: "ghost", hidden: true });
  const back = btn(tr("dataAgent.back"), { icon: "chevron-left", kind: "ghost", data: { dbhistback: "" } });
  const view = h("div", { id: "dbAgentHist" }, formActions(back, h("span", { class: "grow" }), formCap(tr("dataAgent.history"))), list, more);
  back.onclick = ()       => { talk.hist = null; if (inFront(talk)) renderDbAgent(); };
  talk.hist = view;
  let page = 0;
  const rows              = [];
  const load = async ()                => {
    more.hidden = true;
    const r = await call                                      ("GET", "/api/llm/db/conversations?agent=" + encodeURIComponent(talk.agent) + "&conn=" + encodeURIComponent(talk.conn) + "&page=" + page);
    if (talk.hist !== view) return;
    if (!r.data) { list.appendChild(hint(r.err, { bad: true })); return; }
    rows.push(...r.data.runs);
    fill(list, rows.length
      ? flow(rows.map((row) => historyStep(row, talk.runId)))
      : hint(tr("dataAgent.historyEmpty")));
    more.hidden = !r.data.more;
    page++;
  };
  more.onclick = ()       => { void load(); };
  list.onclick = (e)       => {
    const step = targetEl(e)?.closest             ("[data-dbhist]");
    const row = step && rows.find((x) => String(x.id) === step.dataset.dbhist);
    if (row) void showHistoryRun(talk, row);
  };
  fill(list, note(tr("dataAgent.loading"), { busy: true }));
  await load();
}

/** One conversation of the History, whole, with the way to carry it on here: Continue makes it
 *  the connection's open conversation (the one open now closes and stays in History). A failed
 *  one cannot go on; the one in front goes back to the column. */
async function showHistoryRun(talk      , r           )                {
  const list = talk.hist;
  if (!list) return;
  const back = btn(tr("dataAgent.back"), { icon: "chevron-left", kind: "ghost" });
  const cont = btn(tr("dataAgent.continue"), { kind: "primary", data: { dbcontinue: String(r.id) }, hidden: r.ok === false });
  const said = h("div", null, note(tr("dataAgent.loading"), { busy: true }));
  const view = h("div", { id: "dbAgentHist", data: { dbhistrun: String(r.id) } },
    formActions(back, h("span", { class: "grow" }), cont),
    r.ok === false ? hint(tr("dataAgent.historyFailed")) : null,
    said);
  back.onclick = ()       => { talk.hist = list; if (inFront(talk)) renderDbAgent(); };
  cont.onclick = ()       => { void carryOn(talk, r.id, cont); };
  talk.hist = view;
  if (inFront(talk)) renderDbAgent();
  const { d, why } = await dialogueOf(r.id);
  if (talk.hist !== view) return;
  fill(said, d ? conversationNode(d, undefined, askedTurn) : hint(why, { bad: true }));
}

/** Continue here: the run becomes the connection's open conversation and its dialogue the
 *  column. The one in front already is: back to it. */
async function carryOn(talk      , id        , button                   )                {
  if (talk.live) { toast(tr("dataAgent.compactLive"), true); return; }
  if (id !== talk.runId) {
    button.disabled = true;
    const r = await call                   ("POST", "/api/llm/db/reopen", { agent: talk.agent, conn: talk.conn, runId: id, ...modelBody(talk.agent) });
    button.disabled = false;
    if (!r.data) { toast(r.err, true); return; }
    const { d, why } = await dialogueOf(id);
    talk.gen++;
    talk.asked = true;
    talk.restored = true;
    talk.runId = id;
    if (d) { fill(talk.out, conversationNode(d, undefined, askedTurn)); account(talk, d); }
    else { fill(talk.out); talk.usage = noUsage(); talk.used = null; toast(why, true); }
  }
  talk.hist = null;
  if (inFront(talk)) {
    renderDbAgent();
    follow(talk);
    byId                     ("dbAgentAsk")?.focus();
  }
}

/** A fresh visit reads the agents again (one may have been added meanwhile). */
function dbAgentForget()       {
  agents = null;
  owns.clear();
  offered = [];
  agentsWhy = "";
  edge = null;
  stats = null;
}

export { dbAgentButton, dbAgentForget, dbAgentPane, renderDbAgent };

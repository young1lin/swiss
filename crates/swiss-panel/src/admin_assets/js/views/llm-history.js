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
   LLM History - the llm plugin fourth page (#llm-history), SPEC §llm.history.

   The local record of every agent run and every provider round trip (chat and embedding, from
   every consumer - a panel Test, a memory sync, the builtin loop), as the module keeps it in
   logs/llm.db on the shared host.history machinery. A run says three things before any of its
   dialogue: what it was for (the task and its subject), what started it (a memory run, the run
   whose tool call began it, a panel Test), and how it ran (one request, or a tool loop of N
   rounds and the tools it called). Its sheet reads as a dialogue: the user at the right, the assistant at the left, long
   context behind one fold, and the model rounds and tool calls inside a process disclosure
   above the answer. The flow (ui/flow.ts) keeps each step inspectable in place. A
   call answers "what did this one round trip cost and say". The bodies seal at rest and expand
   here on demand; nothing on this page mutates anything.
   ================================================================================================ */
import { $, api, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
                                      
import { locale, tr, trn } from "../i18n.js";
import { JV_LINES, actionBtn, btn, card, closeSheet, decodeStrings, dot, emptyNode, factLine, failNote, filterInput, flow, flowHead, flowIO, flowProse, flowToggle, fmtMs, formFold, jsonCodeNode, kvRow, markdownNode, note, pager, paneBody, popupMenu, refLink, relTime, row, seg, scrollBox, section, setActionBusy, sheet, showSheet, speaker, splitJsonBlock, stackSheet, tag, textNode, transcript, transcriptText, transcriptTurn, valueBlock } from "../ui/index.js";
                                                                       
import { call, takeWantedRun } from "./llm-core.js";

const PAGE = 20;

/** One call row of GET /api/llm/history/calls, field-for-field from history.rs. */
                             
             
             
                             
                   
                
                       
                                                                     
                                                                
                       
              
             
                        
                   
                    
                          
                           
                           
                  
                
                   
                                                                        
                       
                          
 

                                                                                             

/** One run row of GET /api/llm/history/runs, field-for-field from history.rs. */
                            
             
             
                       
                
                  
                          
                       
                     
             
                
                                                                            
                       
                                                                                           
                       
                          
                                                        
                         
                                            
                         
                                                                                 
                       
                                                       
                  
                    
                     
                           
                                              
                    
                                         
                                 
                                                               
                 
 

/** A running run's state (SPEC §llm.history Live): the server keeps it in memory while the
 *  run works and never files it, so a restart forgets it. */
                          
                    
                                                                                                
                
                         
                      
                                           
                 
                                                              
                   
 

                                                                                          

/** One turn of the dialogue read, its whole text inline - the trace renders from one
 * fetch, with no per-turn body round trip. */
                                                                                        

/** One provider call of a dialogue: the call row plus what the trace wants of the body -
 * the prompt, the loop turn it served, the answer texts, the tool calls it asked for and
 * the stop reason. The whole wire stays in the call sheet. */
                                                     
                
                  
                    
                                                                         
                      
                                                                   
                  
 

/** GET /api/llm/history/runs/{id}/dialogue: the run, every turn with its full text, every
 * provider call with its digest, the run that started it and the runs it started. */
                                                                            
                           
                           
                        
                         
 

/** What a run is named by in a list or a sheet head. */
                                                                                

                                      

let kind       = "";
let needle = "";
let callsPage = 0;
let runsPage = 0;
let calls = []                ;
let callsMore = false;
let runs = []               ;
let runsMore = false;
let failed = "";
let visit = 0; // mount and unmount bump it: a late answer paints nothing

async function loadCalls()                   {
  const at = visit;
  const q = "/api/llm/history/calls?page=" + callsPage + "&pageSize=" + PAGE +
    (kind ? "&kind=" + kind : "") +
    (needle ? "&q=" + encodeURIComponent(needle) : "");
  const r = await call              ("GET", q);
  if (at !== visit) return false;
  if (!r.data) { failed = r.err; return false; }
  failed = ""; // a good read clears the flag a failed one left
  calls = r.data.calls || [];
  callsMore = !!r.data.more;
  return true;
}

async function loadRuns()                   {
  const at = visit;
  const r = await call             ("GET", "/api/llm/history/runs?page=" + runsPage + "&pageSize=" + PAGE);
  if (at !== visit) return false;
  if (!r.data) { failed = r.err; return false; }
  failed = "";
  runs = r.data.runs || [];
  runsMore = !!r.data.more;
  for (const one of runs) runIndex.set(one.id, one);
  return true;
}

/* --- the words ----------------------------------------------------------------------------- */

/** The kind filter's label: a word, or "everything" for the empty kind. */
export function kindLabel(k      )         {
  return k === "chat" ? tr("llm.history.kindChat") : k === "embedding" ? tr("llm.history.kindEmbedding") : tr("llm.history.kindAll");
}

/** What a call row is called by: the agent it served, or "a panel test" when none did. */
export function callWho(c            )         {
  return c.agent || tr("llm.history.panelTest");
}

/** One line of the preview: the first line only, so the sub stays one line. */
export function firstLine(text        )         {
  const cut = text.indexOf("\n");
  return (cut < 0 ? text : text.slice(0, cut)).trim();
}

/** What a run or a call was for, in words; a key this page does not know says itself. */
export function taskLabel(task                           )         {
  switch (task) {
    case "memory.map": return tr("llm.history.task.map");
    case "memory.review": return tr("llm.history.task.review");
    case "memory.notes": return tr("llm.history.task.notes");
    case "memory.wrapup": return tr("llm.history.task.wrapup");
    case "memory.sync": return tr("llm.history.task.sync");
    case "memory.recheck": return tr("llm.history.task.recheck");
    case "memory.docs": return tr("llm.history.task.docs");
    case "memory.task": return tr("llm.history.task.delegated");
    default: return task || "";
  }
}

/** A task and its subject on one line. */
function taskLine(task                           , subject                           )         {
  const label = taskLabel(task);
  return label && subject ? label + " · " + subject : label;
}

/** A run's name: its task and subject; a run with no task is the panel Test conversation it
 *  was, or the first line it was asked, or at least its agent. */
export function runWhat(r          )         {
  const said = taskLine(r.task, r.subject);
  if (said) return said;
  if (r.kind === 1) return tr("llm.history.conversation");
  const head = firstLine(r.head || "");
  return head ? head.slice(0, 120) : r.agent;
}

/** The job that started a run, in words: a memory run by its id, anything else as kept. */
export function sourceLabel(source                           )         {
  if (!source) return "";
  const m = /^memory:(.+)$/.exec(source);
  if (m) return tr("llm.history.byMemory", { id: m[1] });
  const c = /^compact:(\d+)$/.exec(source);
  return c ? tr("llm.history.compactedFrom", { id: c[1] }) : source;
}

/** How a run ran: one request, a spawned runtime's process, or a tool loop of N rounds and
 *  the tools it called, the most used first. */
export function runHow(r                                                 )         {
  const rounds = r.rounds ?? 0;
  const tools = Object.entries(r.tools || {}).sort((a, b) => b[1] - a[1]);
  if (rounds === 0 && r.runtime && r.runtime !== "builtin") return tr("llm.history.howSpawned", { runtime: r.runtime });
  // A conversation asked N times with no tool in between is not a loop: every round was
  // its own request (the panel's own Test page ran this way before it said its task).
  if (!tools.length) return rounds <= 1 ? tr("llm.history.howOnce") : tr("llm.history.howAsks", { n: rounds });
  return [tr("llm.history.howLoop", { n: rounds }), ...tools.map(([name, n]) => name + " ×" + n)].join(" · ");
}

/** What a running run does now, in words: its round and step, how long the step has run,
 *  and what the model has streamed of the round so far. */
export function liveText(live         , runtime        )         {
  const s = span(live.stepMs);
  if (live.step === "tool") return tr("llm.history.live.tool", { n: live.round, tool: live.tool || "", s });
  if (live.round === 0) {
    return runtime && runtime !== "builtin" ? tr("llm.history.live.spawned", { runtime, s }) : tr("llm.history.live.first", { s });
  }
  const said = tr("llm.history.live.model", { n: live.round, s });
  return live.streamed ? said + " · " + tr("llm.history.live.streamed", { n: compact(live.streamed) }) : said;
}

/** A token count at a glance (1.2K, 52K), in the reader's locale. */
function compact(n        )         {
  return new Intl.NumberFormat(locale(), { notation: "compact", maximumFractionDigits: 1 }).format(n);
}

/** A run's or a call's tokens: in and out, and what of the input the cache served. */
function tokensText(o                                                                     )         {
  const said = tr("llm.history.tokens", { i: compact(o.tokensIn ?? 0), o: compact(o.tokensOut ?? 0) });
  return o.tokensCacheRead ? said + " · " + tr("llm.history.cached", { n: compact(o.tokensCacheRead) }) : said;
}

/** A span a reader can use: fmtMs's precision under a minute and a half, minutes past it. */
function span(ms        )         {
  if (ms < 90_000) return fmtMs(ms);
  const s = Math.round(ms / 1000);
  return tr("llm.history.minSec", { m: Math.floor(s / 60), s: s % 60 });
}

/** A text's size: bytes under a KiB, one decimal of KiB past it. */
function sizeOf(text        )         {
  const n = new TextEncoder().encode(text).length;
  return n < 1024 ? tr("llm.history.bytes", { n }) : tr("llm.history.kib", { n: (n / 1024).toFixed(1) });
}

/* --- the rows ------------------------------------------------------------------------------ */

function callRow(c            )              {
  const cols           = [
    { v: c.kind === "embedding" ? tr("llm.history.kindEmbedding") : tr("llm.history.kindChat"), mono: true, w: "s" },
    { v: c.kind === "embedding" ? tr("llm.history.ms", { ms: c.ms }) : c.tokensIn + " / " + c.tokensOut, mono: true, w: "s" },
    { v: c.ok ? "" : String(c.status ?? ""), mono: true, w: "s" },
    { v: relTime(Date.parse(c.at) || 0), w: "s" },
  ];
  // A call filed under a run says what the run was for; the body's preview is the wire,
  // which only a call outside any run has to be told by.
  const what = taskLine(c.task, c.subject);
  return row({
    name: h("span", { class: "mono" }, c.provider + " / " + c.model),
    sub: callWho(c) + (what ? " · " + what : c.preview ? " · " + firstLine(c.preview).slice(0, 160) : ""),
    lead: statusDot(c.ok),
    cols,
    data: { hcall: c.id },
    title: c.provider + " / " + c.model,
  });
}

/** A run's lead: the pulse while it runs, its outcome once it filed. ok NULL otherwise is
 *  not an outcome: the lead says the conversation's STATE instead. */
export function runLead(r                                          )              {
  return r.live ? liveDot() : r.ok == null ? runStateBadge(r.atEnd) : statusDot(r.ok);
}

/** A run in one line for another page (memory's sync sheet): what it does now and how long
 *  it has run, or how it ran, its tokens and how long it took. */
export function runBrief(r           )         {
  if (r.live) return liveText(r.live, r.runtime) + " · " + tr("llm.history.live.elapsed", { s: span(r.live.elapsedMs) });
  const tokens = (r.tokensIn ?? 0) + (r.tokensOut ?? 0) ? " · " + compact(r.tokensIn ?? 0) + " / " + compact(r.tokensOut ?? 0) : "";
  return runHow(r) + tokens + " · " + span(r.ms);
}

/** The memory run behind a run (its source memory:<id>), or "". */
function memoryRunOf(r                           )         {
  const m = /^memory:(.+)$/.exec(r.source || "");
  return m ? m[1] : "";
}

/** The memory runs told to stop from this page: their live rows say "stopping…" until they go. */
const stoppingMemory = new Set        ();

/** A live run a memory run asked carries Stop: it stops that memory run - every question it
 *  has in flight, these rows among them (SPEC §memory.sync Cancel). */
/** The words a memory run's Stop wears when it is not in flight (its name and its tooltip). */
function memoryStopIdle(id        )                                   {
  return { label: tr("memory.sync.run.stop"), title: tr("llm.history.stopMemoryTip", { id }) };
}

function stopMemoryNode(r           )                                {
  const id = r.live ? memoryRunOf(r) : "";
  if (!id) return undefined;
  // Built, then restated through the same setActionBusy the click uses, so a row that is already
  // stopping paints exactly what the click would have painted.
  const idle = memoryStopIdle(id);
  const b = actionBtn("stop", idle.label, { ghost: true, data: { hstop: id }, title: idle.title });
  setActionBusy(b, "stop", idle, stoppingMemory.has(id) ? tr("memory.sync.run.stopping") : null);
  return b;
}

/** Stop a memory run by its id alone - the run row knows no project. */
async function stopMemory(id        , button                   )                {
  if (stoppingMemory.has(id)) return;
  const hadFocus = document.activeElement === button;
  const rowId = button.closest             ("[data-hrun]")?.dataset.hrun;
  stoppingMemory.add(id);
  const idle = memoryStopIdle(id);
  const paint = (busy               )       => {
    setActionBusy(button, "stop", idle, busy);
    document.querySelectorAll                   ("[data-hstop]").forEach((other) => {
      if (other !== button && other.dataset.hstop === id) setActionBusy(other, "stop", idle, busy);
    });
  };
  paint(tr("memory.sync.run.stopping"));
  let said = "";
  try {
    const r = await api("/api/knowledge/sync/runs/" + encodeURIComponent(id) + "/cancel", { method: "POST" });
    const j          = await r.json().catch(()          => null);
    if (r.ok) {
      const stopping = typeof j === "object" && j !== null && "stopping" in j && j.stopping === true;
      toast(tr(stopping ? "memory.sync.run.stoppingToast" : "memory.sync.run.cancelledToast", { id }));
      return;
    }
    said = typeof j === "object" && j !== null && "message" in j && typeof j.message === "string" ? j.message : tr("util.httpN", { n: r.status });
  } catch {
    said = tr("util.requestFailedGatewayRunning");
  }
  stoppingMemory.delete(id);
  paint(null);
  if (hadFocus && document.activeElement === document.body) {
    const row = Array.from(document.querySelectorAll             ("[data-hrun]")).find((item) => item.dataset.hrun === rowId);
    const restored = row?.querySelector                   ("[data-hstop]");
    if (restored && restored.dataset.hstop === id && !restored.disabled) restored.focus();
  }
  toast(said, true);
}

function runRow(r           )              {
  const what = runWhat(r);
  return row({
    name: what,
    sub: h("span", null, r.agent + " · ", h("code", null, r.model || r.provider || "—"), " · " + (r.live ? liveText(r.live, r.runtime) : runHow(r))),
    lead: runLead(r),
    cols: [
      { v: (r.tokensIn ?? 0) + (r.tokensOut ?? 0) ? compact(r.tokensIn ?? 0) + " / " + compact(r.tokensOut ?? 0) : "—", mono: true, w: "m", title: tokensText(r) },
      { v: span(r.live ? r.live.elapsedMs : r.ms), mono: true, w: "s" },
      { v: relTime(Date.parse(r.at) || 0), w: "s", title: r.at },
    ],
    primary: stopMemoryNode(r),
    data: { hrun: r.id },
    title: what,
  });
}

function pagerRow(which                  , page        , more         )              {
  // The wire contract (remote-runs' pager): Newer dies at the top, Older dies at
  // the list's end - a click past the end never swaps a populated page for the
  // empty state.
  return pager({
    id: which === "calls" ? "lhCallsPager" : "lhRunsPager",
    label: tr(which === "calls" ? "llm.history.callsPager" : "llm.history.runsPager"),
    prev: btn(tr("llm.history.newer"), { kind: "ghost", data: { page: which + ":-1" }, disabled: page <= 0 }),
    next: btn(tr("llm.history.older"), { kind: "ghost", data: { page: which + ":1" }, disabled: !more }),
    status: tr("llm.history.pageN", { n: page + 1 }),
    statusId: which === "calls" ? "lhCallsStatus" : "lhRunsStatus",
  });
}

/* --- the sheets ---------------------------------------------------------------------------- */

/** Bumped every time a run sheet opens: a body fetch answered after ANOTHER run
 * took the stage paints nothing - the id alone could match a fresh container. */
let sheetSeq = 0;

/** The calls a run's sheet fetched, by id: their rows are clickable exactly like
 *  the page's own call rows, but they are not in the page's loaded array, so the
 *  click handler resolves them through here. */
const runCalls = new Map                    ();

/** Every run row this page has seen - the list's, a sheet's parent and children - so a
 *  sheet opened from any of them names its run before the dialogue answers. */
const runIndex = new Map                   ();

/** The open run sheet's step bodies, by step id: a step paints its body when opened. */
const stepBodies = new Map                      ();

function callSheet(c            , nested = false)       {
  // A conversation's inspector is a layer over its existing DOM: closing it must
  // preserve the reader's scroll, disclosures and focus without fetching the run again.
  const layer = nested ? stackSheet() : null;
  const dismiss = ()       => { if (layer) layer.close(); else closeSheet(); };
  const what = taskLine(c.task, c.subject);
  const meta = card(
    kvRow(tr("llm.history.f.at"), c.at, { mono: true }),
    kvRow(tr("llm.history.f.kind"), c.kind === "embedding" ? tr("llm.history.kindEmbedding") : tr("llm.history.kindChat")),
    kvRow(tr("llm.history.f.provider"), c.provider + " / " + c.model, { mono: true }),
    kvRow(tr("llm.history.f.agent"), callWho(c)),
    what ? kvRow(tr("llm.history.f.what"), what) : null,
    c.runId != null ? kvRow(tr("llm.history.f.run"), refLink(tr("llm.history.runN", { id: c.runId }), { data: { hrun: c.runId } })) : null,
    kvRow(tr("llm.history.f.ms"), tr("llm.history.ms", { ms: c.ms }), { mono: true }),
    kvRow(tr("llm.history.f.tokens"), c.tokensIn + " / " + c.tokensOut + " (" + tr("llm.history.f.cache", { r: c.tokensCacheRead, w: c.tokensCacheWrite }) + ")", { mono: true }),
    c.status != null ? kvRow(tr("llm.history.f.status"), String(c.status), { mono: true }) : null,
    c.requestId ? kvRow(tr("llm.history.f.requestId"), c.requestId, { mono: true }) : null,
  );
  const body = h("div", { id: "lhCallBody" }, note(tr("llm.history.loadingBody"), { busy: true }));
  // The inspector is also opened from Test, where History's page listener is absent.
  meta.onclick = (event)       => {
    const run = targetEl(event)?.closest             ("[data-hrun]");
    if (!run) return;
    event.stopPropagation();
    if (layer) {
      dismiss();
      if ($("lhRunBody")?.dataset.lhrun === run.dataset.hrun) return;
    }
    onClick(event);
  };
  // The foot is the detail viewer's head turned sideways (request-log's detail):
  // Copy carries the WHOLE stored JSON away, and the sides are one switch apart.
  let rawBody = "";
  const copy = btn(tr("llm.history.copyJson"), { kind: "ghost" });
  copy.onclick = ()       => {
    void navigator.clipboard.writeText(rawBody).then(() => {
      copy.textContent = tr("llm.history.copied");
      window.setTimeout(() => { copy.textContent = tr("llm.history.copyJson"); }, 1500);
    });
  };
  const close = btn(tr("llm.history.close"));
  close.onclick = dismiss;
  const spec = { title: tr("llm.history.callTitle", { id: c.id }), body: [meta, body], foot: [copy, close] };
  if (layer) layer.paint(spec);
  else showSheet(sheet(spec));
  void (async () => {
    const r = await call                                       ("GET", "/api/llm/history/calls/" + c.id + "/body");
    // A late answer belongs only to this layer, never to a newer inspector with the same id.
    if (!body.isConnected) return;
    if (!r.data) { fill(body, failNote({ text: tr("llm.history.bodyFailed"), why: r.err })); return; }
    rawBody = r.data.body || "";
    fill(body, r.data.bodyGone
      ? note(tr("llm.history.bodyGone"), { err: true })
      : sidedBlocks(rawBody));
  })();
}

/** The body as the detail shows it (request-log's detail page): a Request | Response
 * switch over the same two blocks when the body holds both sides, the blocks
 * themselves when it holds one, verbatim when it holds no JSON at all. */
function sidedBlocks(text        )           {
  const blocks = bodyBlocks(text);
  if (blocks.length !== 2) return blocks;
  const [requestBlock, responseBlock] = blocks                              ;
  const sides = h("div", null, responseBlock);
  const tabs = seg([
    { id: "request", label: tr("llm.history.request") },
    { id: "response", label: tr("llm.history.response") },
  ], "response", { key: "lhside" });
  tabs.addEventListener("click", (event) => {
    const hit = (event.target               ).closest             ("[data-lhside]");
    if (!hit) return;
    const which = hit.dataset.lhside || "response";
    for (const button of Array.from(tabs.querySelectorAll("[data-lhside]"))) {
      button.setAttribute("aria-selected", String(button === hit));
    }
    fill(sides, which === "request" ? requestBlock : responseBlock);
  });
  return [tabs, sides];
}

/** The row's outcome as the panel's state dot (ui/status.ts): green answered, red
 *  failed - the word rides as its title, so the colour is never the only carrier. */
function statusDot(ok         )              {
  return dot(ok ? "up" : "down", tr(ok ? "llm.history.ok" : "llm.history.failed"));
}

/** A run with no outcome yet: OPEN (no atEnd - a conversation the server still holds)
 *  or CLOSED without one (the reader closed it). The badge says which; a NULL-ok run
 *  never wears a dot and never renders a bare lead. Drawn by the library's tag()
 *  (ui/status.ts) - tone-less is its own register, a state word that is neither
 *  failure nor success - never a hand-written class literal (SPEC §panel.ui). */
function runStateBadge(atEnd               )              {
  const open = atEnd == null;
  const badge = tag(tr(open ? "llm.history.runOpen" : "llm.history.runClosed"));
  // The data hook rides on the returned node (the tree's idiom): tag() owns its classes.
  badge.setAttribute("data-lhstate", open ? "open" : "closed");
  return badge;
}

/** A run still working: the amber pulse, the word on its title. */
function liveDot()              {
  const pulse = dot("starting", tr("llm.history.running"));
  pulse.setAttribute("data-lhstate", "live");
  return pulse;
}

/** One JSON value as the panel's code block (the view every page draws, SPEC panel.ui),
 *  with the Show-all swap past the line cap: a 1024-wide vector block stays openable,
 *  not a wall. Shared with the live stream, which must highlight exactly as history does. */
export function jsonBlock(value         )         {
  const capped = jsonCodeNode(decodeStrings(value), false);
  if (capped.lines <= JV_LINES) return capped.node;
  const full = jsonCodeNode(decodeStrings(value), true);
  const more = btn(tr("logs.showAllLines", { n: capped.lines }));
  more.addEventListener("click", ()       => {
    capped.node.replaceWith(full.node);
    more.remove();
  });
  return h("div", null, capped.node, more);
}

/** Text that is not JSON as the panel's log block, with the same Show-all swap. The nodes
 *  come back loose so a value block styles the block as its own. */
function textBlock(text        )           {
  const capped = textNode(text, false, "logs");
  if (capped.lines <= JV_LINES) return [capped.node];
  const more = btn(tr("logs.showAllLines", { n: capped.lines }));
  more.addEventListener("click", ()       => {
    capped.node.replaceWith(textNode(text, true, "logs").node);
    more.remove();
  });
  return [capped.node, more];
}

/** The sealed body as the sheet shows it. A body that parses as JSON with a request and
 *  a response opens as TWO of the panel's code blocks - one per side of the round trip,
 *  the same view the Logs page draws - with the response's thinking, when the wire
 *  carried any, as grey prose above its block: reasoning reads as reasoning, not as JSON
 *  noise. Any other JSON renders as one block; a body that is not JSON is information,
 *  not a formatting failure, and rides verbatim. */
export function bodyBlocks(text        )           {
  const parsed = splitJsonBlock(text);
  if (!parsed || parsed.tail) return [textNode(text, true, "logs").node];
  const o = parsed.value && typeof parsed.value === "object" && !Array.isArray(parsed.value)
    ? parsed.value                           
    : null;
  if (!o || !("request" in o) || !("response" in o)) return [jsonBlock(parsed.value)];
  // The JSON stays whole - thinking included: the body is the wire's truth, and a
  // reader who copies it gets exactly what the provider said. The trace is where
  // reasoning reads as its own grey block; the response block never edits.
  const thought = Array.isArray((o.response                           ).thinking)
    ? ((o.response                           ).thinking             ).map(String).join("\n\n")
    : "";
  return [
    valueBlock({ label: tr("llm.history.request"), data: { lhbody: "request" } }, jsonBlock(o.request)),
    valueBlock(
      { label: tr("llm.history.response"), data: { lhbody: "response" } },
      thought ? textNode(thought, true, "logs jv-dim").node : null,
      jsonBlock(o.response),
    ),
  ];
}

/* --- the trace ----------------------------------------------------------------------------- */

/** One tool call of a round: what the model asked with, and what the tool answered. */
;                                                                                                                         

/** One model round: its reasoning, what it said, the tools it called, and the provider
 *  round trip that carried it. */
;                            
             
                     
               
                     
                               
                    
 

/** One ask of a run: the prompt, the rounds that worked it, the answer, any error. A
 *  one-shot run has one; a carried conversation one per message. A compaction's opening ask
 *  (`compact`) is its instruction, its answer the checkpoint the dialogue carries on from. */
;                                                                                                                             

;                                                                 

/** A tool call turn, as the loop notes it: "tool call {name} [{id}]: {arguments}" (the id
 *  absent in rows older than it). */
export function toolCallOf(text        )                                                         {
  const m = /^tool call (\S+)(?: \[([^\]]*)\])?: ([\s\S]*)$/.exec(text);
  return m ? { name: m[1], id: m[2] || "", arguments: m[3] } : null;
}

/** A tool result turn, as the loop notes it: "{name} [{id}]: {text}". */
export function toolResultOf(text        )                                                    {
  const m = /^(\S+) \[([^\]]*)\]: ([\s\S]*)$/.exec(text);
  return m ? { name: m[1], id: m[2], text: m[3] } : null;
}

/** The dialogue as rounds. The loop notes, per round, its thinking, what the model said
 *  and then each tool call followed by its result; the next model turn after a result
 *  begins the next round. Where the model said nothing between two rounds, the round
 *  trip a call's id names tells the rounds apart. */
export function traceOf(turns                   , roundTrips                   )        {
  const trace        = { system: null, asks: [] };
  const askedIn = new Map                         ();
  for (const c of roundTrips) for (const t of c.toolCalls || []) askedIn.set(t.callId, c);
  const at                                                                   = { ask: null, round: null, last: "" };
  const ask = ()           => {
    if (!at.ask) {
      at.ask = { prompt: "", rounds: [], answer: null, errors: [] };
      trace.asks.push(at.ask);
    }
    return at.ask;
  };
  const open = (when        )             => {
    const r             = { at: when, thinking: [], text: "", tools: [], call: null, answered: false };
    ask().rounds.push(r);
    at.round = r;
    return r;
  };
  const modelTurn = (when        )             => {
    return !at.round || at.last === "tool" || at.last === "final" ? open(when) : at.round;
  };
  for (const t of turns) {
    const when = Date.parse(t.at) || 0;
    const asked = t.role === "assistant" ? toolCallOf(t.text) : null;
    if (t.role === "system") {
      trace.system = t.text;
    } else if (t.role === "user" || t.role === "compact") {
      at.ask = { prompt: t.text, rounds: [], answer: null, errors: [], compact: t.role === "compact" || undefined };
      trace.asks.push(at.ask);
      at.round = null;
    } else if (t.role === "thinking") {
      modelTurn(when).thinking.push(t.text);
    } else if (t.role === "final") {
      modelTurn(when).answered = true;
      ask().answer = t.text;
    } else if (t.role === "error") {
      ask().errors.push(t.text);
    } else if (asked) {
      const via = askedIn.get(asked.id) || null;
      let r = at.round;
      if (!r || at.last === "final" || (at.last === "tool" && via != null && r.call != null && via !== r.call)) r = open(when);
      if (via && !r.call) r.call = via;
      r.tools.push({ name: asked.name, id: asked.id, args: asked.arguments, at: when, result: null, resultAt: 0 });
    } else if (t.role === "tool") {
      const told = toolResultOf(t.text);
      const step = told ? waiting(ask(), told.id) : null;
      if (told && step) {
        step.result = told.text;
        step.resultAt = when;
      } else {
        // A result whose call is not on record still shows, as a call with no arguments.
        (at.round || open(when)).tools.push({
          name: told ? told.name : "", id: told ? told.id : "", args: "", at: when, result: told ? told.text : t.text, resultAt: when,
        });
      }
    } else {
      // What the model said in a round (or a role this page does not know): its prose.
      const r = modelTurn(when);
      r.text = r.text && t.text ? r.text + "\n\n" + t.text : r.text || t.text;
    }
    at.last = asked ? "call" : t.role;
  }
  matchRoundTrips(trace, roundTrips);
  return trace;
}

/** The newest call of an ask still waiting on its result under that id. */
function waiting(ask          , id        )                   {
  for (let i = ask.rounds.length - 1; i >= 0; i--) {
    const tools = ask.rounds[i].tools;
    for (let k = tools.length - 1; k >= 0; k--) {
      if (tools[k].id === id && tools[k].result == null) return tools[k];
    }
  }
  return null;
}

/** Each round's provider round trip: the one its tool calls name, else the next answered
 *  chat call in order that asked for tools exactly when the round did. */
function matchRoundTrips(trace       , roundTrips                   )       {
  const chat = roundTrips.filter((c) => c.kind === "chat");
  let next = 0;
  for (const ask of trace.asks) {
    for (const r of ask.rounds) {
      if (r.call) {
        next = Math.max(next, chat.indexOf(r.call) + 1);
        continue;
      }
      let i = next;
      while (i < chat.length && !(chat[i].ok && ((chat[i].toolCalls || []).length > 0) === (r.tools.length > 0))) i++;
      if (i >= chat.length) i = next;
      if (i < chat.length) {
        r.call = chat[i];
        next = i + 1;
      }
    }
  }
}

/** A tool call's arguments on one line: the command of a bash, the pattern of a glob or a
 *  grep (and where it looks), the path and the window of a read, else the compact JSON. */
export function argLine(args        )         {
  let v         ;
  try {
    v = JSON.parse(args);
  } catch {
    return args.replace(/\s+/g, " ").slice(0, 200);
  }
  if (v && typeof v === "object" && !Array.isArray(v)) {
    const o = v                           ;
    if (typeof o.command === "string") return o.command;
    if (typeof o.pattern === "string") return o.pattern + (typeof o.path === "string" ? "  " + o.path : "");
    if (typeof o.path === "string") {
      const from = typeof o.from === "number" ? o.from : null;
      const lines = typeof o.lines === "number" ? o.lines : null;
      return from == null && lines == null ? o.path : o.path + " :" + (from ?? 1) + (lines == null ? "" : "+" + lines);
    }
  }
  return JSON.stringify(v).slice(0, 200);
}

/** A tool step's line: the call's own description when it gives one - prose, what the agent
 *  meant the call to do - else its arguments on one line. The opened step keeps the command. */
export function stepLine(args        )                                            {
  let said = "";
  try {
    const v          = JSON.parse(args);
    const d = v && typeof v === "object" && !Array.isArray(v) ? (v                           ).description : null;
    if (typeof d === "string") said = d.replace(/\s+/g, " ").trim();
  } catch {
    // Not JSON: the arguments read as text below.
  }
  return said ? { summary: said, summaryMono: false } : { summary: argLine(args) || undefined, summaryMono: true };
}

/** A round's thinking as its flow line, as dsh shows it: the brain, Think, and the thought's
 *  first line. The whole thought opens in place, dimmed. */
export function thinkLine(thinking        )                                               {
  return { lead: "brain", title: tr("llm.history.think"), summary: firstLine(thinking.trim()) || undefined };
}

/** A tool step's state, when it is not a plain answer: refused or failed, or never answered. */
function stepStatus(s           )                     {
  if (s.result == null) return { text: tr("llm.history.noResult"), tone: "warn" };
  let failed = /^(error|refused)\b/i.test(s.result);
  if (s.name === "bash" || s.name === "powershell" || s.name === "job_output") {
    try {
      const result          = JSON.parse(s.result);
      if (result && typeof result === "object" && "state" in result) {
        failed ||= ["failed", "timeout", "canceled"].includes(String(result.state));
      }
    } catch { /* Older tool results can be plain text. */ }
  }
  return failed ? { text: tr("llm.history.toolError"), tone: "bad" } : undefined;
}

/** A prompt's outline: the text before its first heading, then one part per level-1 or
 *  level-2 heading (### and deeper stay inside their part). A heading inside a code fence
 *  is code, not a heading. */
export function outline(text        )                                                                 {
  const lead           = [];
  const parts                                           = [];
  let fence = "";
  for (const line of text.split("\n")) {
    const bare = line.trim();
    if (fence) {
      if (bare.startsWith(fence) && /^[`~]+$/.test(bare)) fence = "";
    } else {
      const opens = /^(`{3,}|~{3,})/.exec(bare);
      const head = opens ? null : /^#{1,2}\s+(.+)$/.exec(line);
      if (opens) fence = opens[1];
      if (head) {
        parts.push({ head: head[1].replace(/`/g, "").trim(), lines: [] });
        continue;
      }
    }
    (parts.length ? parts[parts.length - 1].lines : lead).push(line);
  }
  return { lead: lead.join("\n").trim(), parts: parts.map((p) => ({ head: p.head, text: p.lines.join("\n").trim() })) };
}

/** A prompt short enough to read in place; past it, it folds under its size. */
const OPEN_PROMPT = 2000;

/** One folded part: its name and size on the summary, its markdown inside. */
function fold(head        , text        )              {
  return formFold({ summary: [head, tag(sizeOf(text))], scroll: true }, markdownNode(text, { class: "tc-md", copy: true }));
}

/** The prompt as an outline: the system prompt folded, the opening in place, every part
 *  folded under its heading and size - the reader opens the part they came for. */
function promptNodes(text        )           {
  const o = outline(text);
  if (text.length <= OPEN_PROMPT && o.parts.length <= 3) return [markdownNode(text, { class: "tc-md", copy: true })];
  const preview = (o.lead || firstLine(text)).slice(0, 320);
  return [
    transcriptText(preview + (preview.length < (o.lead || text).length ? "…" : "")),
    formFold({ summary: [tr("llm.history.promptWhole"), tag(sizeOf(text))], scroll: true },
      o.parts.length
        ? [o.lead ? markdownNode(o.lead, { class: "tc-md", copy: true }) : null, ...o.parts.map((p) => fold(p.head, p.text))]
        : markdownNode(text, { class: "tc-md", copy: true })),
  ];
}

/** A sent prompt has the same preview and context fold before and after restoring it; its Copy
 *  gives the whole prompt back. */
export function promptTurn(text        )                 {
  return { side: "user", role: speaker("user"), code: promptNodes(text), copy: text };
}

/** A tool's glyph by what its name says it does: a shell, a search, a write, a read, a store,
 *  the web; anything else is a plain tool. */
export function toolIcon(name        )         {
  const n = name.toLowerCase();
  if (/bash|shell|exec|command|terminal/.test(n)) return "terminal";
  if (/grep|search|find|glob|lookup/.test(n)) return "search";
  if (/write|edit|patch|apply|save|create|delete|remove|file_note|draw/.test(n)) return "pencil";
  if (/read|cat|view|open|list|ls|show|get/.test(n)) return "file";
  if (/sql|query|db|memory|note|hub|domain/.test(n)) return "database";
  if (/fetch|http|web|url|browse/.test(n)) return "globe";
  return "puzzle";
}

/** The ask's rounds and tool calls as the flow: a round that reasoned reads as its Think line
 *  (thinkLine), one that did not as its number and what it did; then whatever the model said
 *  as prose, then each tool call as its name and its arguments on one line. Each line opens
 *  in place. The
 *  ask a running run works on ends with the step in flight: the tool call still waiting on
 *  its result, or a line for the round the model is answering - shimmering, pulsing. */
function stepNodes(ask          , n        , run                    , bodies                           )                                {
  const items                                = [];
  let last                  = null;
  ask.rounds.forEach((r, i) => {
    const id = "r" + n + "-" + i;
    const thought = r.thinking.join("\n");
    const said = r.answered ? tr("llm.history.answered") : r.tools.length ? tr("llm.history.calledTools", { n: r.tools.length }) : firstLine(r.text);
    last = {
      id,
      ...(thought.trim()
        ? thinkLine(thought)
        : { lead: "sparkles", title: tr("llm.history.roundN", { n: i + 1 }), summary: said || undefined }),
      meta: r.call ? span(r.call.ms) : undefined,
      status: r.call && !r.call.ok ? { text: tr("llm.history.failed"), tone: "bad" } : undefined,
      data: { lhstep: "round" },
    };
    items.push(last);
    bodies.set(id, () => roundBody(r));
    // What the model said between its thinking and its tool calls reads as prose, in place.
    if (r.text.trim() && r.text.trim() !== ask.answer?.trim()) items.push(flowProse(markdownNode(r.text, { class: "tc-md", copy: true })));
    r.tools.forEach((s, k) => {
      const sid = id + "-" + k;
      last = {
        id: sid,
        lead: toolIcon(s.name),
        title: s.name || tr("llm.history.role.tool"),
        mono: true,
        ...stepLine(s.args),
        meta: s.result == null ? undefined : span(Math.max(0, s.resultAt - s.at)),
        status: stepStatus(s),
        data: { lhstep: "tool" },
      };
      items.push(last);
      bodies.set(sid, () => toolBody(s));
    });
  });
  const live = run && run.live;
  if (!live) return items;
  const lastRound = ask.rounds[ask.rounds.length - 1]                          ;
  const lastTool = lastRound ? lastRound.tools[lastRound.tools.length - 1] : undefined;
  const waitingOn = last                   ;
  if (live.step === "tool" && waitingOn && lastTool && lastTool.result == null && waitingOn.data?.lhstep === "tool") {
    waitingOn.status = undefined;
    waitingOn.live = true;
    waitingOn.meta = span(live.stepMs);
  } else {
    items.push({
      id: "live" + n,
      lead: live.step === "tool" ? toolIcon(live.tool || "") : "sparkles",
      title: live.step === "tool" ? live.tool || tr("llm.history.role.tool") : run.model || tr("llm.history.model"),
      mono: live.step === "tool",
      summary: liveText(live, run.runtime),
      live: true,
      meta: span(live.stepMs),
      data: { lhstep: "live" },
    });
  }
  return items;
}

/** An opened round: the round trip's facts, the whole thinking dimmed, and the way to the
 *  whole wire. What the model said already reads under the round's line. */
function roundBody(r            )         {
  const c = r.call;
  return [
    c ? factLine([h("code", null, c.provider + " / " + c.model), tokensText(c), c.stopReason ? tr("llm.history.stop", { why: c.stopReason }) : null]) : null,
    // A thought past the prose cap scrolls in place: one long reasoning round must not push
    // the answer, the next step and the box below it off the column.
    r.thinking.length ? scrollBox(transcriptText(r.thinking.join("\n\n"), { dim: true })) : null,
    c ? refLink(tr("llm.history.openCall", { id: c.id }), { data: { hcall: c.id } }) : null,
  ];
}

/** An opened tool call: the arguments it was asked with and what the tool answered, as one
 *  surface, each side scrolled past its cap. */
function toolBody(s           )         {
  const args = splitJsonBlock(s.args);
  return [
    s.args || s.result != null
      ? flowIO([
        s.args ? { label: tr("llm.history.arguments"), body: args && !args.tail ? jsonBlock(args.value) : textBlock(s.args), copy: s.args, copyLabel: tr("logs.copyArguments"), data: { lhio: "args" } } : null,
        s.result != null
          ? {
            label: tr("llm.history.result"),
            notes: [sizeOf(s.result), tr("llm.history.linesN", { n: s.result.split("\n").length })],
            body: textBlock(s.result),
            copy: s.result,
            copyLabel: tr("logs.copyResult"),
            data: { lhio: "result" },
          }
          : null,
      ])
      : null,
    s.result == null ? note(tr("llm.history.noResult"), { err: true }) : null,
  ];
}

/** The closing answer: the JSON blocks when it is JSON, the markdown the model wrote when not. */
function answerNodes(text        )           {
  const t = text.trim();
  return t.startsWith("{") || t.startsWith("[") ? bodyBlocks(t) : [markdownNode(text, { class: "tc-md", copy: true })];
}

/** One ask's work, as its fold's summary: rounds, tool calls (none said when none), tokens and
 *  time. The same line live (counted from the stream's usage and tool frames) and restored
 *  (summed from the ask's calls),
 *  so a reloaded conversation reads as it streamed (2026-11-04: the two said it differently). */
export function workSummary(o                                                                                            )         {
  return [
    trn(o.rounds, "ui.chat.steps.one", "ui.chat.steps.other"),
    o.tools ? trn(o.tools, "llm.work.tools.one", "llm.work.tools.other") : null,
    tr("llm.history.tokens", { i: compact(o.tokensIn), o: compact(o.tokensOut) }),
    o.ms != null && Number.isFinite(o.ms) && o.ms >= 0 ? span(o.ms) : null,
  ].filter(Boolean).join(" · ");
}

/** An ask's work out of its rounds' calls: tokens summed, time from the first call's start to
 *  the last one's end. */
function askWork(ask          , tools        )         {
  const calls = ask.rounds.map((r) => r.call).filter((c)                       => c != null);
  const first = calls[0], last = calls[calls.length - 1];
  const ms = first && last ? Date.parse(last.at) + last.ms - Date.parse(first.at) : null;
  return workSummary({
    rounds: ask.rounds.length, tools,
    tokensIn: calls.reduce((n, c) => n + (c.tokensIn || 0), 0),
    tokensOut: calls.reduce((n, c) => n + (c.tokensOut || 0), 0),
    ms,
  });
}

/** One ask on the sheet: its prompt, its steps, its answer and errors. A running run's
 *  `run` rides on the ask it works on now, its last. */
function askNodes(ask          , n        , system               , run                    , bodies                           , asTurn                                  , artifact                              )           {
  if (ask.compact) return compactNodes(ask, n, n === 0 ? system : null);
  const steps = stepNodes(ask, n, run, bodies);
  const tools = ask.rounds.reduce((sum, r) => sum + r.tools.length, 0);
  const asked = n === 0 ? system : null;
  const process = formFold({ summary: askWork(ask, tools) },
    h("div", { data: { lhsteps: String(n) } }, flow(steps)));
  process.setAttribute("data-lhprocess", String(n));
  if (run?.live) process.setAttribute("open", "");
  return [
    asked?.trim() ? fold(tr("llm.history.systemPrompt"), asked) : null,
    ask.prompt ? transcriptTurn(asTurn(ask.prompt)) : null,
    transcriptTurn({ side: "assistant", role: speaker("assistant"), code: [
      steps.length ? process : null,
      ...ask.rounds.flatMap(r => r.tools.map(tool => artifact?.(tool))),
      ask.answer != null ? h("div", { data: { lhanswer: String(n) } }, ...answerNodes(ask.answer)) : null,
      ...ask.errors.map((e) => note(e, { err: true })),
    ], copy: ask.answer?.trim() ? ask.answer : undefined }),
  ];
}

/** A compaction's opening ask: one quiet line where the condensed turns stood, the checkpoint
 *  the dialogue carries on from folded under it (its instruction is the call's), then any
 *  failure. */
function compactNodes(ask          , n        , system               )           {
  const checkpoint = ask.answer;
  return [
    system?.trim() ? fold(tr("llm.history.systemPrompt"), system) : null,
    transcriptTurn({ role: null, code: [h("div", { data: { lhcompact: String(n) } },
      factLine([tr("llm.history.compacted")]),
      checkpoint != null ? fold(tr("llm.history.checkpoint"), checkpoint) : null,
      ...ask.errors.map((e) => note(e, { err: true })))] }),
  ];
}

/** Recorded and restored conversations share one renderer, including their disclosure state.
 * Each column owns its step bodies and pointer routing, so restoring Test never relies on
 * History being mounted and never populates History's sheet state. */
export function conversationNode(d             , bodies = new Map                      (), asTurn                                   = promptTurn, artifact                              )              {
  const trace = traceOf(d.turns || [], d.calls || []);
  const of = trace.asks.length;
  const column = transcript([]);
  fill(column, trace.asks.map((ask, n) => askNodes(ask, n, trace.system, d.live && n === of - 1 ? d : null, bodies, asTurn, artifact)));
  column.onclick = (event)       => {
    const target = targetEl(event);
    const sum = target?.closest             (".fl-sum");
    if (sum) {
      const id = sum.closest             (".fl-step")?.dataset.flId || "";
      const list = sum.closest             (".fl");
      const paint = bodies.get(id);
      if (list && id) flowToggle(list, id, paint ? paint() : undefined);
      event.stopPropagation();
      return;
    }
    const call = target?.closest             ("[data-hcall]");
    const c = call && (d.calls || []).find((c) => c.id === Number(call.dataset.hcall));
    if (c) { event.stopPropagation(); callSheet(c, !!call?.closest(".sheet")); }
  };
  return column;
}

/** Who started a run: the run whose tool call did, the job, a panel Test, or nothing kept. */
function startedBy(d             )         {
  const source = sourceLabel(d.source);
  if (d.parentRun) {
    return h("span", null,
      refLink(tr("llm.history.byRun", { id: d.parentRun.id, what: runWhat(d.parentRun) }), { data: { hrun: d.parentRun.id } }),
      source ? " · " + source : null);
  }
  if (source) return source;
  return d.kind === 1 ? tr("llm.history.panelTest") : tr("llm.history.byUnknown");
}

/** The sheet's head, three lines of facts: how it stands (running and on what, or its
 *  outcome), how long and when; who ran it on what model and what it cost; how it ran and
 *  who started it. The sheet's title already says what it was for. */
function runHead(d             )              {
  const started = (d.children || []).length;
  const state         = d.live
    ? [liveDot(), tr("llm.history.running")]
    : d.ok == null ? runStateBadge(d.atEnd) : [statusDot(d.ok), d.ok ? tr("llm.history.ok") : tr("llm.history.failed")];
  return flowHead(
    factLine([
      state,
      d.live ? liveText(d.live, d.runtime) : null,
      span(d.live ? d.live.elapsedMs : d.ms),
      h("span", { title: d.at }, new Date(Date.parse(d.at) || 0).toLocaleString(locale())),
    ], { lead: true, data: { lhhead: "state" } }),
    factLine([
      d.agent + " · " + d.runtime,
      h("code", null, (d.provider || "—") + (d.model ? " / " + d.model : "")),
      tokensText(d),
    ], { data: { lhhead: "agent" } }),
    factLine([
      runHow(d),
      started ? tr("llm.history.startedN", { n: started }) : null,
      [tr("llm.history.f.startedBy") + " ", startedBy(d)],
    ], { data: { lhhead: "how" } }),
  );
}

function runSheet(id        )       {
  const known = runIndex.get(id);
  const body = h("div", { id: "lhRunBody", data: { lhrun: id } }, note(tr("llm.history.loadingBody"), { busy: true }));
  body.onclick = (event)       => {
    if (targetEl(event)?.closest("[data-hrun], [data-hcall]")) { event.stopPropagation(); onClick(event); }
  };
  const copy = btn(tr("llm.history.copyJson"), { kind: "ghost", id: "lhRunCopy" });
  const close = btn(tr("llm.history.close"));
  close.onclick = ()       => { closeSheet(); };
  showSheet(sheet({
    title: known ? runWhat(known) : tr("llm.history.runN", { id }),
    titleId: "lhRunTitle",
    sub: tr("llm.history.runN", { id }),
    body,
    foot: [copy, close],
    wide: true,
  }));
  void readRun(id, ++sheetSeq, visit, false);
}

/** How often an open sheet of a running run reads its dialogue again. */
const LIVE_MS = 3000;

/** Read a run's dialogue and paint its sheet. While the run is live it reads again every
 *  LIVE_MS, keeping the steps and folds the reader opened, until the run files, the sheet
 *  closes, or another sheet or page takes the stage. A failed read again keeps what is
 *  painted and tries at the next tick. */
async function readRun(id        , mine        , at        , painted         )                {
  // ONE fetch: the whole dialogue - every turn's full text, every provider round trip
  // with its ask and answer digested out of the body, the parent and the children.
  const res = await call             ("GET", "/api/llm/history/runs/" + id + "/dialogue");
  const into = $("lhRunBody");
  if (!into || mine !== sheetSeq || at !== visit) return;
  if (!res.data) {
    if (!painted) { fill(into, failNote({ text: tr("llm.history.bodyFailed"), why: res.err })); return; }
    window.setTimeout(() => { void readRun(id, mine, at, true); }, LIVE_MS);
    return;
  }
  const opened = painted ? openedIn(into) : null;
  paintRun(into, res.data);
  if (opened) reopen(into, opened);
  if (res.data.live) window.setTimeout(() => { void readRun(id, mine, at, true); }, LIVE_MS);
}

/** What the reader opened on a sheet: the steps by id, and each fold's state in order. */
;                                                     

function openedIn(into             )         {
  return {
    steps: Array.from(into.querySelectorAll             ("[data-lhsteps] .fl-step.open")).map((n) => n.dataset.flId || ""),
    folds: Array.from(into.querySelectorAll                    ("details")).map((n) => n.open),
  };
}

/** Open again on a repainted sheet what was open: the steps first, so the folds inside an
 *  opened step's body count in the same order they did. */
function reopen(into             , was        )       {
  const lists = Array.from(into.querySelectorAll             ("[data-lhsteps] .fl"));
  for (const id of was.steps) {
    const paint = stepBodies.get(id);
    for (const list of lists) if (flowToggle(list, id, paint ? paint() : undefined)) break;
  }
  const folds = into.querySelectorAll                    ("details");
  was.folds.forEach((open, i) => { if (folds[i]) folds[i].open = open; });
}

/** Paint one read of a run's dialogue into its sheet. */
function paintRun(into             , d             )       {
  const head = $("lhRunTitle");
  if (head) head.textContent = runWhat(d);
  // The run's calls and its kin register for click routing, replacing whatever the
  // previous run's sheet left: only one run's sheet is ever open.
  runCalls.clear();
  for (const c of d.calls || []) runCalls.set(c.id, c);
  if (d.parentRun) runIndex.set(d.parentRun.id, d.parentRun);
  for (const child of d.children || []) runIndex.set(child.id, child);
  stepBodies.clear();
  const trace = traceOf(d.turns || [], d.calls || []);
  const kids = d.children || [];
  fill(into, [
    runHead(d),
    trace.asks.length
      ? conversationNode(d, stepBodies)
      : emptyNode({ icon: "history", title: tr(d.live ? "llm.history.liveNoTurns" : "llm.history.noTurns") }),
    kids.length
      ? section({ cap: tr("llm.history.children", { n: kids.length }), note: tr("llm.history.childrenNote") }, card(...kids.map(runRow)))
      : null,
    (d.calls || []).length
      ? formFold({ summary: [tr("llm.history.callsOfRun"), tag(String(d.calls.length))] }, card(...d.calls.map(runCallRow)))
      : null,
  ]);
  // The whole conversation, carried away as the JSON it is stored as.
  const copyEl = $("lhRunCopy");
  if (copyEl) copyEl.onclick = ()       => {
    void navigator.clipboard.writeText(JSON.stringify(d, null, 2)).then(() => {
      copyEl.textContent = tr("llm.history.copied");
      window.setTimeout(() => { copyEl.textContent = tr("llm.history.copyJson"); }, 1500);
    });
  };
}

/** One provider round trip of the run: the loop turn it served wears a number badge, the
 * prompt rides as the row's sub, and the whole wire opens on click. */
function runCallRow(c                 )              {
  const cols           = [
    { v: c.turn == null ? "—" : "#" + c.turn, mono: true, w: "s", title: tr("llm.history.f.turn") },
    { v: c.tokensIn + " / " + c.tokensOut, mono: true, w: "m" },
    { v: tr("llm.history.ms", { ms: c.ms }), mono: true, w: "s" },
    { v: relTime(Date.parse(c.at) || 0), w: "s" },
  ];
  return row({
    name: h("span", { class: "mono" }, c.provider + " / " + c.model),
    sub: c.prompt ? firstLine(c.prompt).slice(0, 160) : callWho(c),
    lead: statusDot(c.ok),
    cols,
    data: { hcall: c.id },
    title: c.provider + " / " + c.model,
  });
}

/* --- the page ------------------------------------------------------------------------------ */

function callsNode()              {
  return section(
    {
      cap: tr("llm.history.calls"),
      note: tr("llm.history.callsNote"),
      tools: [
        btn(kindLabel(kind), { kind: "ghost", data: { lhkind: "1" }, id: "lhKind" }),
        filterInput({ id: "lhSearch", placeholder: tr("llm.history.search"), label: tr("llm.history.searchLabel"), value: needle }),
      ],
    },
    // The list holder is STABLE: render() paints it once, paintLists() swaps only
    // its children ever after - the search box keeps its text and its focus, and
    // the kind button its hover, through every poll and reload.
    h("div", { id: "lhCallsList" }),
    pagerRow("calls", callsPage, callsMore),
  );
}

function runsNode()              {
  return section(
    { cap: tr("llm.history.runs"), note: tr("llm.history.runsNote") },
    h("div", { id: "lhRunsList" }),
    pagerRow("runs", runsPage, runsMore),
  );
}

/** The pane holds the two sections. False while a load failure shows its note -
 * there is nothing to patch then, and a successful retry flips it back. */
let listed = false;

function render()       {
  const pane = $("pane");
  // The runs come first: a run says what was asked and why; its calls are the detail.
  fill(pane, paneBody({ wide: true },
    failed ? failNote({ text: tr("llm.history.loadFailed"), why: failed, action: btn(tr("llm.history.retry"), { data: { lhretry: "1" } }) }) : null,
    failed ? null : [runsNode(), callsNode()]));
  listed = !failed;
  paintLists();
}

/** Swap the list holders' children and patch the pagers in place - never the
 * sections around them. This is the poll path too: a repaint here would wipe
 * the search box mid-typing and steal focus from whatever had it. */
function paintLists()       {
  if (!listed) return;
  const callInto = $("lhCallsList");
  if (callInto) {
    fill(callInto, calls.length
      ? card(...calls.map(callRow))
      : emptyNode({ icon: "activity", title: tr("llm.history.noCalls"), hint: tr("llm.history.noCallsHint") }));
  }
  const runInto = $("lhRunsList");
  if (runInto) {
    fill(runInto, runs.length
      ? card(...runs.map(runRow))
      : emptyNode({ icon: "history", title: tr("llm.history.noRuns"), hint: tr("llm.history.noRunsHint") }));
  }
  pagerPaint("calls", callsPage, callsMore);
  pagerPaint("runs", runsPage, runsMore);
}

/** The pager patch: the status line and the two disabled flags. The buttons
 * never rebuild (remote-runs' wire contract: Newer dies at page 0, Older dies
 * when the page holds no more). */
function pagerPaint(which                  , page        , more         )       {
  const status = $(which === "calls" ? "lhCallsStatus" : "lhRunsStatus");
  if (status) status.textContent = tr("llm.history.pageN", { n: page + 1 });
  const box = $(which === "calls" ? "lhCallsPager" : "lhRunsPager");
  if (!box) return;
  const prev = box.querySelector("[data-page='" + which + ":-1']");
  const next = box.querySelector("[data-page='" + which + ":1']");
  if (prev) (prev                     ).disabled = page <= 0;
  if (next) (next                     ).disabled = !more;
}

function onClick(event            )       {
  const t = targetEl(event);
  if (!t) return;
  if (t.closest("[data-lhretry]")) { void reload(); return; }
  const page = t.closest             ("[data-page]");
  if (page) {
    const [which, dir] = (page.dataset.page || "").split(":");
    const delta = Number(dir) || 0;
    if (which === "calls" && (callsPage + delta >= 0)) { callsPage += delta; void reload(); }
    if (which === "runs" && (runsPage + delta >= 0)) { runsPage += delta; void reload(); }
    return;
  }
  const kindBtn = t.closest             ("[data-lhkind]");
  if (kindBtn) {
    popupMenu(kindBtn.getBoundingClientRect(), [
      { label: kindLabel(""), fn: () => { if (kind !== "") { kind = ""; callsPage = 0; void reload(); } } },
      { label: kindLabel("chat"), fn: () => { if (kind !== "chat") { kind = "chat"; callsPage = 0; void reload(); } } },
      { label: kindLabel("embedding"), fn: () => { if (kind !== "embedding") { kind = "embedding"; callsPage = 0; void reload(); } } },
    ]);
    return;
  }
  // A step of the trace opens in place, its body painted on open.
  const step = t.closest             ("[data-lhsteps] .fl-sum");
  if (step) {
    const item = step.closest             (".fl-step");
    const list = step.closest             (".fl");
    const id = item ? item.dataset.flId || "" : "";
    const paint = stepBodies.get(id);
    if (list && id) flowToggle(list, id, paint ? paint() : undefined);
    return;
  }
  const callRowEl = t.closest             ("[data-hcall]");
  if (callRowEl) {
    const id = Number(callRowEl.dataset.hcall);
    const c = calls.find((x) => { return x.id === id; }) || runCalls.get(id);
    if (c) callSheet(c, !!callRowEl.closest(".sheet"));
    return;
  }
  const stopEl = t.closest                   ("[data-hstop]");
  if (stopEl) {
    if (stopEl.dataset.hstop && !stopEl.disabled) void stopMemory(stopEl.dataset.hstop, stopEl);
    return;
  }
  const runRowEl = t.closest             ("[data-hrun]");
  if (runRowEl) {
    const id = Number(runRowEl.dataset.hrun);
    if (id) runSheet(id);
  }
}

function onKey(event               )       {
  if (event.key !== "Enter") return;
  const el = event.target               ;
  if (el.id === "lhSearch") {
    needle = (el                    ).value.trim();
    callsPage = 0;
    void reload();
  }
}

async function reload()                {
  const at = visit;
  failed = "";
  const [callsOk, runsOk] = await Promise.all([loadCalls(), loadRuns()]);
  if (at !== visit) return;
  if (!callsOk || !runsOk) { render(); return; } // the honest failure note
  if (!listed) { render(); return; }
  // A reload after a kind pick or a search: the label moves, the lists and
  // pagers patch - the input the user just typed in never rebuilds.
  const kindBtn = $("lhKind");
  if (kindBtn) kindBtn.textContent = kindLabel(kind);
  paintLists();
}

export async function mount()                {
  visit++;
  callsPage = 0;
  runsPage = 0;
  runIndex.clear();
  listed = false; // a fresh mount paints the sections; a patch without them is a no-op
  document.addEventListener("click", onClick);
  document.addEventListener("keydown", onKey);
  await reload();
  // Another page asked for one run (memory's sync sheet): its sheet opens over the lists.
  const wanted = takeWantedRun();
  if (wanted != null && $("lhRunsList")) runSheet(wanted);
}

export function unmount()       {
  visit++;
  document.removeEventListener("click", onClick);
  document.removeEventListener("keydown", onKey);
}

/** The poll: nothing here changes behind the page's back except new calls and
 * runs. It may PATCH the lists, never rebuild the page - a repaint would wipe
 * the search box mid-typing and steal focus. A failed poll keeps what is
 * painted; the next successful one patches (and clears `failed`). */
export async function poll()                {
  const at = visit;
  const [callsOk, runsOk] = await Promise.all([loadCalls(), loadRuns()]);
  if (at !== visit || !callsOk || !runsOk || !listed) return;
  paintLists();
}

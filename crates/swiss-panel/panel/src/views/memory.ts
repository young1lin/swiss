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
   Memory - the memory plugin page (#memory), SPEC §memory 11.2.

   The page is the human door onto the store the memory_* MCP tools and the offline CLI share. It
   is a workspace (the pane full-bleed, each column scrolling on its own), read left to right:

     projects  Global first, then every project with its live count and a review badge; New
               project, and the gear that sets up semantic search. A project no agent can reach
               yet (no directory, or none reachable) says so.
     head      the project's name and directories, Auto-update (memory-auto.ts: the daily
               review of the notes whose code changed, sync after commits) saying how it stands,
               its ⋯ (rename, add, change or remove a directory, export, import, delete), New
               note, and the five tabs.
     Domains   the project's map beside the domain picked from it (memory-domains.ts): the
               tree, Unfiled and Not covered; a domain's hub, owned code, sub-domains, notes.
     Notes     the list - keyword search or association over a passage, kind / status / domain /
               unconfirmed / history filters - and beside it the note picked from it, whole
               (memory-note.ts): its content, its verbs, its links and its own small graph.
     Review    the same two columns over the inbox: the reasons a note waits in the list, the
               verdict buttons in the detail; a verdict moves on to the next one. Over the
               list, the daily review: when it takes the notes whose code changed, or - off -
               that they stay as they are, one click from Auto-update.
     Graph     the project's graph and an inspector of the selected note (memory-graph.ts).
     Agent     the brief an agent gets, verbatim, beside the inputs that shape it.

   Module split: memory-core.ts holds the state, the request helper and the words; memory-note.ts
   the detail and the editor; memory-domains.ts the Domains tab; memory-graph.ts the Graph tab;
   memory-embed.ts the semantic search sheet; memory-sync.ts the sync sheet; memory-auto.ts the
   auto-update sheet; memory-transfer.ts export and import. This file is the entry the page registry loads (mount / refresh / unmount) and the one
   delegated listener on #pane.
   ================================================================================================ */
import { $, targetEl, toast } from "../util.js";
import type { HChild } from "../h.js";
import { fill, h } from "../h.js";
import { tr, trn } from "../i18n.js";
import {
  btn, card, checkField, closeMenu, closeSheet, dot, emptyNode, failNote, field, filterInput, form, formActions, hint,
  iconBtn, inlineForm, markdownNode, menuOpen, moreBtn, note, openFieldSheet, pickList, pickRow, pickSelect, popupMenu, row, seg,
  sheet, showSheet, sideRow, tag, textNode, valueBlock, workspace, wsBar, wsHead, wsScroll, wsSide, wsSplit,
} from "../ui/index.js";
import type { MemErrorBody, MemProject, MemProjectSettings, MemRelatedReply, MemSearchReply, MemStatusReply, MemSummary } from "../types/memory.js";
import type { MemTab } from "./memory-core.js";
import {
  KINDS, STATUSES, S, UNFILED, ageText, currentProject, domainList, domainSelect, errMessage, fullTime, hooks,
  isLive, kindChip, kindLabel, markerTags, qs, reasonsNode, req, scoreText, seg as idSeg, statusLabel, statusTag, tierLabel,
} from "./memory-core.js";
import { loadDetail, onDetailClick, openEditor, paintDetailEmpty } from "./memory-note.js";
import { domainKey, dropDomains, onDomainsClick, paintDomains, refreshDomains } from "./memory-domains.js";
import { dropGraph, paintGraph, selectNode } from "./memory-graph.js";
import { openEmbedding, semanticNode } from "./memory-embed.js";
import { openAsk } from "./memory-ask.js";
import { openSync } from "./memory-sync.js";
import { openAuto } from "./memory-auto.js";
import { openExport, openImport } from "./memory-transfer.js";

/** The ids the list column shows, in order: the keyboard walks them, a verdict moves on in them. */
let listIds: string[] = [];
/** The brief as last rendered: what Copy copies. */
let briefText = "";
/** Its size and root, said over it. */
let briefNotes: string[] = [];
/** The Markdown as the agent reads it instead of drawn. */
let briefSource = false;

/* ------------------------------------------------------------------ projects */

function projectName(p: MemProject): string {
  return p.global ? tr("memory.global") : p.name;
}

/** Whether an agent can reach the project: global always; a project through a reachable root. */
function reachable(p: MemProject): boolean {
  return p.global || p.roots.some((r) => r.reachable);
}

function reviewCount(p: MemProject): number {
  return p.counts.needsReview + p.counts.unconfirmed;
}

async function loadProjects(): Promise<string | null> {
  const [pr, st] = await Promise.all([
    req<{ projects: MemProject[] }>("GET", "/projects"),
    req<MemStatusReply>("GET", "/status"),
  ]);
  S.status = st.ok ? st.data : null;
  if (!pr.ok || !pr.data) return errMessage(pr.err);
  S.projects = pr.data.projects;
  if (!S.projects.some((p) => p.id === S.project)) {
    // First visit: the first bound project, never the shared scope by accident.
    const bound = S.projects.find((p) => !p.global);
    S.project = bound ? bound.id : S.projects.length ? S.projects[0].id : "global";
    S.sel = null;
  }
  // The map with the projects: the head counts its domains, Notes filters by them. The
  // settings beside it: the head says whether the daily review is on.
  await Promise.all([domainList(), loadAuto()]);
  return null;
}

/** The current project's own settings, for the head's Auto-update button and the Review bar.
 *  The global scope has no daily review: nothing to read. */
async function loadAuto(): Promise<void> {
  const p = currentProject();
  if (!p || p.global) { S.auto = null; return; }
  const r = await req<{ settings: MemProjectSettings }>("GET", "/projects/" + idSeg(p.id) + "/settings");
  S.auto = { of: p.id, settings: r.ok && r.data ? r.data.settings : null };
}

/** The settings S.auto holds for the project on screen, or null. */
function autoSettings(): MemProjectSettings | null {
  const p = currentProject();
  return p && S.auto && S.auto.of === p.id ? S.auto.settings : null;
}

/** The auto-update sheet of the project on screen; a save reads the page again, so the head
 *  and the Review bar say the new settings. */
async function openAutoSheet(): Promise<void> {
  const p = currentProject();
  if (!p || p.global) return;
  await openAuto(p.id, projectName(p), (settings) => {
    S.auto = { of: p.id, settings };
    void hooks.reload();
  }, () => { void openSync(p.id, { openAuto: () => { void openAutoSheet(); } }); });
}

/** The head's way into the auto-update sheet, saying how the daily review stands: its time, or
 *  off; the bare word while the settings are unread. */
function autoButton(p: MemProject): HTMLElement | null {
  if (p.global) return null;
  const s = autoSettings();
  const label = !s ? tr("memory.auto.open")
    : s.autoReview ? tr("memory.auto.openOn", { time: s.autoReview }) : tr("memory.auto.openOff");
  return btn(label, { id: "memAutoOpen", icon: "clock", data: { mauto: "" }, title: tr("memory.auto.openWhy") });
}

/** The Review bar's word on the daily review, under the inbox's own line: on, when it takes the
 *  entire inbox, including human notes; off with notes waiting, that they stay as they
 *  are; either way one click from the auto-update sheet. Nothing while the settings are unread. */
function paintReviewAuto(items: MemSummary[]): void {
  const slot = document.getElementById("memReviewAuto");
  if (!slot) return;
  const s = autoSettings();
  if (!s) { fill(slot); return; }
  const n = items.length;
  const time = s.autoReview;
  if (time) {
    fill(slot, hint([
      n ? tr("memory.review.autoOn", { time, n }) : tr("memory.review.autoOnNone", { time }), " ",
      btn(tr("memory.review.autoSet"), { kind: "ghost", data: { mauto: "" } }),
    ]));
    return;
  }
  fill(slot, hint([
    n ? [tag(tr("memory.review.autoOffTag"), { tone: "warn" }), " ", tr("memory.review.autoOff", { n })] : tr("memory.review.autoOffNone"), " ",
    btn(n ? tr("memory.review.autoTurnOn") : tr("memory.review.autoSet"), { kind: n ? undefined : "ghost", data: { mauto: "" } }),
  ]));
}

function railNode(): HTMLElement {
  return wsSide({
    cap: tr("memory.projects"), id: "memRail",
    tools: [
      iconBtn("history", tr("memory.sync.open"), { ghost: true, data: { msync: "" } }),
      iconBtn("gear", tr("memory.embed.open"), { ghost: true, data: { membed: "" } }),
      iconBtn("plus", tr("memory.newProject"), { ghost: true, data: { mnewproject: "" } }),
    ],
  }, S.projects.map((p) => {
    const n = reviewCount(p);
    const can = reachable(p);
    return sideRow({
      name: projectName(p),
      lead: can ? dot("up", tr("memory.reachable")) : dot("idle", tr("memory.unreachable")),
      badge: n ? tag(String(n), { tone: "warn", title: tr("memory.badgeTitle", { review: p.counts.needsReview, unconfirmed: p.counts.unconfirmed }) }) : null,
      tail: String(p.counts.live),
      selected: p.id === S.project,
      data: { mproject: p.id },
      title: p.global ? tr("memory.globalSub") : p.roots.map((r) => r.path).join("\n") || tr("memory.unreachable"),
    });
  }));
}

/** The directories a project is bound to, an unreachable one tagged. */
function rootsLine(p: MemProject): HChild {
  const out: HChild[] = [];
  p.roots.forEach((r, i) => {
    if (i) out.push(" · ");
    out.push(h("code", { title: r.identity }, r.path));
    if (!r.reachable) out.push(" ", tag(tr("memory.root.unreachable"), { tone: "warn" }));
  });
  return out;
}

function headNode(p: MemProject): HTMLElement {
  const sub: HChild = p.global
    ? tr("memory.globalSub")
    : reachable(p) ? rootsLine(p) : [h("span", { data: { munreach: "" } }, tr("memory.unreachable")), p.roots.length ? [" · ", rootsLine(p)] : null];
  const review = reviewCount(p);
  return wsHead({
    id: "memHead",
    title: projectName(p),
    sub,
    actions: [
      autoButton(p),
      // Global is the shared scope every project reads: it cannot be renamed or deleted.
      p.global ? null : moreBtn(tr("memory.proj.actions", { name: p.name }), { data: { mpmenu: "" } }),
      btn(tr("memory.newNote"), { kind: "primary", icon: "plus", data: { mnewnote: "" } }),
    ],
    nav: seg([
      { id: "domains", label: tr("memory.tab.domains"), n: S.domains && S.domainsOf === p.id ? S.domains.length || null : null, hidden: p.global },
      { id: "notes", label: tr("memory.tab.notes"), n: p.counts.live },
      { id: "review", label: tr("memory.tab.review"), n: review || null },
      { id: "graph", label: tr("memory.tab.graph"), n: p.counts.links || null },
      { id: "agent", label: tr("memory.tab.agent") },
    ], S.tab, { key: "mtab", label: tr("memory.tabs") }),
  });
}

/* ------------------------------------------------------------------ the frame */

function banner(): HChild {
  const st = S.status;
  if (!st || st.mcp.state !== "conflicted") return null;
  return wsBar({}, note(tr("memory.mcpConflict", { path: st.mcp.path }), { err: true, id: "memMcpBanner" }));
}

/** The pane as this page's workspace: full-bleed, the columns divide it (ui.css .pane.full). */
function framePane(): HTMLElement {
  const pane = $("pane");
  pane.classList.add("full");
  return pane;
}

function paint(): void {
  const pane = framePane();
  const p = currentProject();
  dropGraph();
  if (!p) {
    fill(pane, workspace(null, banner(), wsScroll({ pad: true },
      emptyNode({ icon: "brain", title: tr("memory.noProjects"), hint: tr("memory.noProjectsHint") }))));
    return;
  }
  // #memBody is a slot: each tab puts its own split in its place (paintBody).
  fill(pane, workspace(railNode(), banner(), headNode(p), h("div", { id: "memBody" })));
  void paintBody();
}

/** Repaint the rail and the head in place: counts move after every write. */
function paintChrome(): void {
  const p = currentProject();
  const rail = document.getElementById("memRail");
  const head = document.getElementById("memHead");
  if (!p || !rail || !head) { paint(); return; }
  rail.replaceWith(railNode());
  head.replaceWith(headNode(p));
}

function paintFailure(msg: string): void {
  fill(framePane(), workspace(null, wsScroll({ pad: true },
    failNote({ text: tr("memory.loadFailed"), why: msg, action: btn(tr("memory.retry"), { data: { mretry: "" } }) }))));
}

/** The tab's body in place of the one before it: Notes and Review a list beside the note picked
 *  from it, Graph the drawing beside its inspector, Agent the inputs beside the brief. */
async function paintBody(): Promise<void> {
  const slot = document.getElementById("memBody");
  if (!slot) return;
  const p = currentProject();
  // The global scope has no map: its notes hold across projects.
  if (S.tab === "domains" && (!p || p.global)) S.tab = "notes";
  if (S.tab === "graph") { dropDomains(); await paintGraph(slot); return; }
  dropGraph();
  if (S.tab === "domains") { await paintDomains(slot); return; }
  dropDomains();
  if (S.tab === "agent") { slot.replaceWith(agentNode()); await loadBrief(); return; }
  const review = S.tab === "review";
  // The domain filter lists the map: read it first (once; a write drops it).
  if (!review) await domainList();
  if (!slot.isConnected) return;
  slot.replaceWith(wsSplit({
    id: "memBody", fixed: "start",
    start: [
      review ? wsBar({ id: "memReviewBar" }, hint(tr("memory.review.note")), h("div", { id: "memReviewAuto" })) : notesToolbar(),
      wsScroll({ id: "memListScroll" }, pickList({ id: "memList", label: tr("memory.list") })),
      wsBar({ foot: true, id: "memListFoot" }),
    ],
    end: wsScroll({ pad: true, id: "memDetail" }),
    startLabel: review ? tr("memory.tab.review") : tr("memory.tab.notes"),
    endLabel: tr("memory.detail.label"),
  }));
  await loadList();
}

/** The body again on the same state - after a write: the list (where it was scrolled) and the
 *  note in the detail. */
async function refreshBody(): Promise<void> {
  if (S.tab === "agent") return;
  if (S.tab === "domains") {
    if (document.getElementById("memDomMap")) await refreshDomains();
    else await paintBody();
    return;
  }
  if (S.tab === "graph" || !document.getElementById("memList")) { await paintBody(); return; }
  await loadList();
}

/* ------------------------------------------------------------------ the list column */

function checkbox(id: string, on: boolean, label: string): HTMLElement {
  return checkField({ label, control: h("input", { type: "checkbox", id, checked: on }) as HTMLInputElement });
}

/** The list's tools, a line each: how to ask (keywords, or a passage to associate with), the
 *  ask itself, then the filters. */
function notesToolbar(): HTMLElement {
  const mode = seg([
    { id: "keyword", label: tr("memory.mode.keyword") },
    { id: "associate", label: tr("memory.mode.associate") },
  ], S.mode, { key: "mmode", label: tr("memory.mode") });
  const kind = h("select", { id: "memKind", aria: { label: tr("memory.filter.kind") } },
    h("option", { value: "", selected: !S.kind }, tr("memory.filter.anyKind")),
    KINDS.map((k) => h("option", { value: k, selected: S.kind === k }, kindLabel(k))));
  const status = h("select", { id: "memStatus", aria: { label: tr("memory.filter.status") } },
    h("option", { value: "", selected: !S.statusFilter }, tr("memory.filter.anyStatus")),
    STATUSES.map((s) => h("option", { value: s, selected: S.statusFilter === s }, statusLabel(s))));
  const p = currentProject();
  const domain = p && !p.global
    ? domainSelect({ id: "memDomain", label: tr("memory.filter.domain") },
        S.domains || [], S.domain, [["", tr("memory.dom.any")], ["none", tr("memory.dom.unfiled")]])
    : null;
  let ask: HChild[][];
  if (S.mode === "associate") {
    const text = h("textarea", { id: "memText", rows: 3, placeholder: tr("memory.assoc.placeholder"), aria: { label: tr("memory.assoc.label") }, title: tr("memory.assoc.hint") });
    text.value = S.text;
    ask = [[text], [hint(tr("memory.assoc.keys")), h("span", { class: "grow" }), btn(tr("memory.assoc.run"), { data: { massoc: "" } })]];
  } else {
    ask = [[filterInput({ id: "memQ", placeholder: tr("memory.searchHint"), label: tr("memory.search"), value: S.query })]];
  }
  // The memory question (SPEC §memory.rag): one ask answered by the project's agent.
  const askBtn = btn(tr("memory.ask"), { id: "memAskOpen" });
  askBtn.onclick = (): void => {
    if (p) void openAsk(p.id, projectName(p));
  };
  return wsBar({
    rows: [
      [mode, askBtn],
      ...ask,
      [kind, status],
      // A path reads only whole: the domain filter takes a line of its own.
      ...(domain ? [[domain]] : []),
      [checkbox("memUnconf", S.unconfirmed, tr("memory.filter.unconfirmed")), checkbox("memHist", S.history, tr("memory.history"))],
    ],
  });
}

/** The status, unconfirmed and kind filters over an answer the server could not filter itself
 *  (search takes kind and history only; association none of them). */
function filtered<T extends MemSummary>(items: T[]): T[] {
  return items.filter((x) => (!S.kind || x.kind === S.kind)
    && (!S.statusFilter || x.status === S.statusFilter)
    && (!S.unconfirmed || !x.confirmed)
    && inDomain(x.domain));
}

/** Whether a note filed at `path` passes the domain filter: a domain holds its subtree. */
function inDomain(path: string | undefined): boolean {
  if (!S.domain) return true;
  if (S.domain === "none") return !path;
  return !!path && (path === S.domain || path.startsWith(S.domain + "/"));
}

/** One note in the list: its kind's swatch, the title, when (or the score); under them its state
 *  and why it is here - an excerpt, the reasons it matched, what review wants of it. */
function item(s: MemSummary, o: { meta?: HChild; metaTitle?: string; sub?: HChild } = {}): HTMLElement {
  return pickRow({
    lead: kindChip(s.kind, { bare: true }),
    name: s.title,
    meta: o.meta ?? ageText(s.updatedMs),
    metaTitle: o.metaTitle ?? tr("memory.updatedAt", { when: fullTime(s.updatedMs) }),
    sub: [statusTag(s.status), markerTags(s.markers), h("span", null, o.sub ?? s.excerpt ?? "")],
    selected: s.id === S.sel,
    muted: !isLive(s.status),
    data: { mnote: s.id },
  });
}

function scoreNode(text: string): HTMLElement {
  return h("span", { data: { mscore: "" } }, text);
}

/** Paint the list and keep the detail on the selection - the first note when none is picked yet
 *  (or the picked one left the list, as a verdict does in Review). */
function showList(host: HTMLElement, items: MemSummary[], rows: HTMLElement[], foot: HChild): void {
  const prev = listIds.indexOf(S.sel || "");
  listIds = items.map((x) => x.id);
  const scroll = document.getElementById("memListScroll");
  const keep = scroll ? scroll.scrollTop : 0;
  fill(host, rows);
  if (scroll) scroll.scrollTop = keep;
  const f = document.getElementById("memListFoot");
  if (f) fill(f, foot);
  if (!S.sel || (S.tab === "review" && !listIds.includes(S.sel))) {
    // Review moves on: the item now where the decided one was. Notes opens the first.
    const next = listIds.length ? listIds[Math.min(Math.max(prev, 0), listIds.length - 1)] : null;
    S.sel = next;
    markSelected();
  }
  void showDetail();
}

function showEmpty(host: HTMLElement, node: HTMLElement): void {
  listIds = [];
  fill(host, node);
  const f = document.getElementById("memListFoot");
  if (f) fill(f);
  if (S.tab === "review") S.sel = null;
  void showDetail();
}

async function showDetail(): Promise<void> {
  const host = document.getElementById("memDetail");
  if (!host) return;
  if (!S.sel) { paintDetailEmpty(host); return; }
  await loadDetail(host, S.sel);
}

/** The selection's mark moves in place: no repaint, the list keeps its scroll. */
function markSelected(): HTMLElement | null {
  const list = document.getElementById("memList");
  return list ? pickSelect(list, (r) => r.dataset.mnote === S.sel) : null;
}

function countFoot(n: number, truncated = false): HChild {
  return truncated ? tr("memory.truncated", { n }) : trn(n, "memory.nNotes.one", "memory.nNotes.other");
}

async function loadList(): Promise<void> {
  const host = document.getElementById("memList");
  if (!host) return;
  const visit = S.visit;
  const project = S.project;
  const tab = S.tab;
  const stale = (): boolean => visit !== S.visit || project !== S.project || tab !== S.tab || !host.isConnected;
  if (!host.childElementCount) fill(host, note(tr("memory.loading"), { busy: true }));
  if (tab === "review") {
    const r = await req<{ items: MemSummary[] }>("GET", "/review" + qs({ project }));
    if (stale()) return;
    if (!r.ok || !r.data) { showEmpty(host, note(errMessage(r.err), { err: true })); return; }
    const items = r.data.items;
    paintReviewAuto(items);
    if (!items.length) { showEmpty(host, emptyNode({ icon: "check", title: tr("memory.review.empty"), hint: tr("memory.review.emptyHint") })); return; }
    showList(host, items, items.map((x) => item(x, { sub: x.confirmed ? "" : tr("memory.review.byAgent", { actor: x.actor }) })), countFoot(items.length));
    return;
  }
  if (S.mode === "associate") {
    if (!S.text.trim()) { showEmpty(host, emptyNode({ icon: "search", title: tr("memory.assoc.ask"), hint: tr("memory.assoc.empty") })); return; }
    const body: Record<string, unknown> = { project, text: S.text, limit: 50 };
    if (S.history) body.includeHistory = true;
    const r = await req<MemRelatedReply>("POST", "/related", body);
    if (stale()) return;
    if (!r.ok || !r.data) { showEmpty(host, note(errMessage(r.err), { err: true })); return; }
    const items = filtered(r.data.items);
    if (!items.length) { showEmpty(host, emptyNode({ icon: "search", title: tr("memory.assoc.none"), hint: tr("memory.assoc.noneHint") })); return; }
    showList(host, items, items.map((x) => item(x, {
      meta: scoreNode(tr("memory.scorePct", { n: Math.round(x.score) })), metaTitle: tr("memory.scoreTitle"),
      sub: reasonsNode(x.reasons),
    })), countFoot(items.length));
    return;
  }
  if (S.query) {
    const r = await req<MemSearchReply>("GET", "/search" + qs({ project, q: S.query, kind: S.kind, domain: S.domain, limit: 50, history: S.history }));
    if (stale()) return;
    if (!r.ok || !r.data) { showEmpty(host, note(errMessage(r.err), { err: true })); return; }
    const items = filtered(r.data.items);
    if (!items.length) { showEmpty(host, emptyNode({ icon: "search", title: tr("memory.search.none"), hint: tr("memory.search.noneHint") })); return; }
    // The foot says whether meaning was matched too (SPEC §memory.embedding): a provider that
    // failed this query leaves the words' ranking, and says so.
    const sem = semanticNode(r.data.semantic);
    showList(host, items, items.map((x) => item(x, {
      meta: scoreNode(scoreText(x.score)),
      metaTitle: tr("memory.scoreBy", { tier: tierLabel(x.tier) }) + (x.similarity != null ? " · " + tr("memory.similarity", { n: x.similarity.toFixed(2) }) : ""),
    })), sem ? [countFoot(items.length), " · ", sem] : countFoot(items.length));
    return;
  }
  // A history status is only ever in the history: asking for it turns history on.
  const history = S.history || S.statusFilter === "superseded" || S.statusFilter === "invalidated";
  const r = await req<{ items: MemSummary[]; truncated: boolean }>("GET", "/records" + qs({
    project, kind: S.kind, status: S.statusFilter, unconfirmed: S.unconfirmed, history, domain: S.domain, limit: 200,
  }));
  if (stale()) return;
  if (!r.ok || !r.data) { showEmpty(host, note(errMessage(r.err), { err: true })); return; }
  const items = r.data.items;
  if (!items.length) {
    const filtering = !!(S.kind || S.statusFilter || S.unconfirmed || S.domain);
    showEmpty(host, filtering
      ? emptyNode({ icon: "search", title: tr("memory.filter.none"), hint: tr("memory.filter.noneHint") })
      : emptyNode({ icon: "brain", title: tr("memory.empty"), hint: tr("memory.emptyHint"), action: tr("memory.newNote") }));
    return;
  }
  showList(host, items, items.map((x) => item(x)), countFoot(items.length, r.data.truncated));
}

/** Pick a note: the mark moves, the detail loads. `focus` keeps the keyboard on the row. */
function selectNote(id: string, focus = false): void {
  S.sel = id;
  const b = markSelected();
  if (b) {
    if (typeof b.scrollIntoView === "function") b.scrollIntoView({ block: "nearest" });
    if (focus) b.focus();
  }
  void showDetail();
}

/* ------------------------------------------------------------------ agent view */

/** The inputs an agent's brief is shaped by, beside the brief verbatim. */
function agentNode(): HTMLElement {
  const p = currentProject();
  const paths = h("textarea", { id: "memBriefPaths", rows: 4, placeholder: tr("memory.agent.pathsHint") });
  paths.value = S.briefPaths;
  const task = h("input", { id: "memBriefTask", autocomplete: "off", value: S.briefTask, placeholder: tr("memory.agent.taskHint") });
  const global = h("input", { type: "checkbox", id: "memBriefGlobal", checked: S.briefGlobal });
  return wsSplit({
    id: "memBody", fixed: "start",
    start: wsScroll({ pad: true },
      hint(tr("memory.agent.note")),
      form(
        field({ label: tr("memory.agent.paths"), control: paths, hint: tr("memory.agent.pathsWhy") }),
        field({ label: tr("memory.agent.task"), control: task }),
        p && !p.global ? checkField({ label: tr("memory.agent.includeGlobal"), control: global }) : null,
        formActions(btn(tr("memory.agent.render"), { kind: "primary", data: { mbrief: "" } })))),
    end: wsScroll({ pad: true, id: "memBrief" }),
    startLabel: tr("memory.agent.inputs"), endLabel: tr("memory.agent.brief"),
  });
}

async function loadBrief(): Promise<void> {
  const host = document.getElementById("memBrief");
  if (!host) return;
  const visit = S.visit;
  const project = S.project;
  const body: Record<string, unknown> = { project, includeGlobal: S.briefGlobal };
  const paths = S.briefPaths.split("\n").map((l) => l.trim()).filter(Boolean);
  if (paths.length) body.paths = paths;
  if (S.briefTask.trim()) body.task = S.briefTask.trim();
  fill(host, note(tr("memory.loading"), { busy: true }));
  const r = await req<{ markdown: string; chars: number; root?: string | null }>("POST", "/brief", body);
  if (visit !== S.visit || project !== S.project || !host.isConnected) return;
  if (!r.ok || !r.data) { fill(host, note(errMessage(r.err), { err: true })); return; }
  const b = r.data;
  briefText = b.markdown || "";
  briefNotes = [tr("memory.agent.chars", { n: b.chars })];
  if (b.root) briefNotes.push(tr("memory.agent.root", { root: b.root }));
  paintBrief(host);
}

/** The brief drawn as the Markdown it is, or - pressed - its source, which is what Copy copies. */
function paintBrief(host: HTMLElement): void {
  const body = !briefText
    ? textNode(tr("memory.agent.empty"), true, "logs").node
    : briefSource ? textNode(briefText, true, "logs").node : markdownNode(briefText, { class: "brief" });
  fill(host, valueBlock({
    label: tr("memory.agent.brief"), notes: briefNotes,
    tools: [
      iconBtn("braces", tr("memory.agent.source"), { ghost: true, pressed: briefSource, data: { msource: "" } }),
      iconBtn("copy", tr("memory.agent.copy"), { ghost: true, data: { mcopy: "" } }),
    ],
  }, body));
}

async function copyBrief(): Promise<void> {
  try {
    // A non-secure origin has no async clipboard: the call throws and the catch says so.
    await navigator.clipboard.writeText(briefText);
    toast(tr("memory.agent.copied"));
  } catch (e) {
    toast(tr("memory.agent.copyFailed"), true);
  }
}

/* ------------------------------------------------------------------ project sheets */

/** A 409 that names the project a directory already belongs to (or would merge into): offer to
 *  go there instead. True when it was one. */
function offerSwitch(e: MemErrorBody | null, err: HTMLElement, host: HTMLElement): boolean {
  if (!e) return false;
  const target = e.error === "memory_root_bound" ? e.projectId
    : e.error === "memory_project_ambiguous" && e.possibleMatch ? e.possibleMatch.projectId : undefined;
  if (!target) return false;
  const known = S.projects.find((p) => p.id === target);
  const name = known ? projectName(known) : e.possibleMatch ? e.possibleMatch.name : target;
  err.textContent = errMessage(e);
  err.hidden = false;
  const go = btn(tr("memory.proj.switchTo", { name }), { data: { mswitch: target } });
  go.onclick = () => { closeSheet(); void hooks.selectProject(target); };
  fill(host, inlineForm(go));
  return true;
}

/** A directory's two fields: the path, and whether to bind it exactly or at its Git root. */
function dirFields(required: boolean): { path: HTMLInputElement; exact: HTMLInputElement; nodes: HChild } {
  const path = h("input", { id: "memPPath", autocomplete: "off", placeholder: tr("memory.proj.pathHint") });
  const exact = h("input", { type: "checkbox", id: "memPExact" });
  return {
    path, exact,
    nodes: [
      field({ label: tr("memory.proj.path"), control: path, required, hint: tr("memory.proj.pathWhy") }),
      checkField({ label: tr("memory.proj.exact"), control: exact, hint: tr("memory.proj.exactWhy") }),
    ],
  };
}

function openNewProject(): void {
  const name = h("input", { id: "memPName", autocomplete: "off", placeholder: tr("memory.proj.nameHint") });
  const dir = dirFields(false);
  const err = hint("", { id: "memPErr", bad: true, hidden: true, live: true });
  const offer = h("div", { id: "memPOffer" });
  const cancel = btn(tr("ui.cancel"));
  const ok = btn(tr("memory.proj.create"), { kind: "primary", id: "memPSave" });
  showSheet(sheet({
    title: tr("memory.proj.newTitle"),
    body: [field({ label: tr("memory.proj.name"), control: name, required: true }), dir.nodes, err, offer],
    foot: [h("span", { class: "grow" }), cancel, ok],
  }));
  const fail = (text: string): void => { err.textContent = text; err.hidden = false; };
  const save = async (): Promise<void> => {
    err.hidden = true;
    fill(offer);
    const n = name.value.trim();
    if (!n) { fail(tr("memory.proj.nameRequired")); return; }
    const body: Record<string, unknown> = { name: n };
    const path = dir.path.value.trim();
    if (path) {
      body.path = path;
      if (dir.exact.checked) body.exact = true;
    }
    ok.disabled = true;
    const r = await req<{ project: MemProject }>("POST", "/projects", body);
    ok.disabled = false;
    if (r.ok && r.data) {
      closeSheet();
      toast(tr("memory.proj.created", { name: n }));
      await hooks.selectProject(r.data.project.id);
      return;
    }
    if (!offerSwitch(r.err, err, offer)) fail(errMessage(r.err));
  };
  cancel.onclick = closeSheet;
  ok.onclick = () => { void save(); };
  name.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); void save(); } };
  name.focus();
}

function openAddDir(p: MemProject): void {
  const dir = dirFields(true);
  const err = hint("", { id: "memPErr", bad: true, hidden: true, live: true });
  const offer = h("div", { id: "memPOffer" });
  const cancel = btn(tr("ui.cancel"));
  const ok = btn(tr("memory.proj.addDir"), { kind: "primary", id: "memPSave" });
  showSheet(sheet({ title: tr("memory.proj.addDirTitle"), sub: p.name, body: [dir.nodes, err, offer], foot: [h("span", { class: "grow" }), cancel, ok] }));
  const save = async (): Promise<void> => {
    err.hidden = true;
    fill(offer);
    const path = dir.path.value.trim();
    if (!path) { err.textContent = tr("memory.proj.pathRequired"); err.hidden = false; return; }
    const body: Record<string, unknown> = { path };
    if (dir.exact.checked) body.exact = true;
    ok.disabled = true;
    const r = await req<{ project: MemProject }>("POST", "/projects/" + idSeg(p.id) + "/roots", body);
    ok.disabled = false;
    if (r.ok) { closeSheet(); toast(tr("memory.proj.dirAdded")); await hooks.reload(); return; }
    if (!offerSwitch(r.err, err, offer)) { err.textContent = errMessage(r.err); err.hidden = false; }
  };
  cancel.onclick = closeSheet;
  ok.onclick = () => { void save(); };
  dir.path.focus();
}

/** Change a directory in place: the notes belong to the project's id, never to a path, so a
 *  checkout that moved keeps its project (SPEC §memory.scope). */
function openChangeDir(p: MemProject): void {
  const which = h("select", { id: "memPRoot" }, p.roots.map((r) => h("option", { value: r.identity }, r.path)));
  const dir = dirFields(true);
  const err = hint("", { id: "memPErr", bad: true, hidden: true, live: true });
  const offer = h("div", { id: "memPOffer" });
  const cancel = btn(tr("ui.cancel"));
  const ok = btn(tr("memory.proj.changeDir"), { kind: "primary", id: "memPSave" });
  showSheet(sheet({
    title: tr("memory.proj.changeDirTitle"), sub: p.name,
    body: [hint(tr("memory.proj.changeDirWhy")), field({ label: tr("memory.proj.changeDirFrom"), control: which }), dir.nodes, err, offer],
    foot: [h("span", { class: "grow" }), cancel, ok],
  }));
  const save = async (): Promise<void> => {
    err.hidden = true;
    fill(offer);
    const path = dir.path.value.trim();
    if (!path) { err.textContent = tr("memory.proj.pathRequired"); err.hidden = false; return; }
    ok.disabled = true;
    const r = await req<{ project: MemProject }>("PUT", "/projects/" + idSeg(p.id) + "/roots", { identity: which.value, path, exact: dir.exact.checked });
    ok.disabled = false;
    if (r.ok) { closeSheet(); toast(tr("memory.proj.dirChanged")); await hooks.reload(); return; }
    if (!offerSwitch(r.err, err, offer)) { err.textContent = errMessage(r.err); err.hidden = false; }
  };
  cancel.onclick = closeSheet;
  ok.onclick = () => { void save(); };
  dir.path.focus();
}

function openRemoveDir(p: MemProject): void {
  const err = hint("", { id: "memPErr", bad: true, hidden: true, live: true });
  const list = card(p.roots.map((r) => row({
    name: h("code", null, r.path),
    sub: [h("code", null, r.identity), r.reachable ? null : [" · ", tr("memory.root.unreachable")]],
    primary: btn(tr("memory.proj.removeDir"), { data: { mrootdel: r.identity } }),
  })));
  const close = btn(tr("memory.close"));
  const node = sheet({ title: tr("memory.proj.removeDirTitle"), sub: p.name, body: [hint(tr("memory.proj.removeDirWhy")), list, err], foot: [h("span", { class: "grow" }), close] });
  node.onclick = async (e) => {
    const t = e.target instanceof Element ? e.target.closest<HTMLElement>("[data-mrootdel]") : null;
    if (!t) return;
    const r = await req<{ project: MemProject }>("DELETE", "/projects/" + idSeg(p.id) + "/roots" + qs({ identity: t.dataset.mrootdel }));
    if (!r.ok) { err.textContent = errMessage(r.err); err.hidden = false; return; }
    closeSheet();
    toast(tr("memory.proj.dirRemoved"));
    await hooks.reload();
  };
  close.onclick = closeSheet;
  showSheet(node);
}

function openRename(p: MemProject): void {
  openFieldSheet({
    title: tr("memory.proj.renameTitle"), def: p.name, label: tr("memory.proj.name"),
    submit: async (name: string): Promise<boolean | string> => {
      const r = await req<{ project: MemProject }>("PATCH", "/projects/" + idSeg(p.id), { name });
      if (!r.ok) return errMessage(r.err);
      setTimeout(() => { void hooks.reload(); }, 0);
      return true;
    },
  });
}

function openDeleteProject(p: MemProject): void {
  openFieldSheet({
    title: tr("memory.proj.deleteTitle"), sub: p.name, label: tr("memory.proj.deleteLabel", { id: p.id }),
    placeholder: p.id, save: tr("memory.proj.delete"), exact: true,
    submit: async (typed: string): Promise<boolean | string> => {
      if (typed !== p.id) return tr("memory.proj.deleteMismatch");
      const r = await req<{ deleted: { records: number; links: number } }>("DELETE", "/projects/" + idSeg(p.id) + qs({ confirm: p.id }));
      if (!r.ok || !r.data) return errMessage(r.err);
      const d = r.data.deleted;
      toast(tr("memory.proj.deleted", { name: p.name, records: d.records, links: d.links }));
      S.project = "global";
      setTimeout(() => { void hooks.reload(); }, 0);
      return true;
    },
  });
}

function projectMenu(anchor: HTMLElement): void {
  const p = currentProject();
  if (!p || p.global) return;
  popupMenu(anchor.getBoundingClientRect(), [
    { label: tr("memory.proj.rename"), fn: () => { openRename(p); } },
    { label: tr("memory.proj.addDirAsk"), fn: () => { openAddDir(p); } },
    { label: tr("memory.proj.changeDirAsk"), fn: () => { openChangeDir(p); }, disabled: !p.roots.length },
    { label: tr("memory.proj.removeDirAsk"), fn: () => { openRemoveDir(p); }, disabled: !p.roots.length },
    { sep: true },
    { label: tr("memory.xfer.export"), fn: () => { void openExport(p); } },
    { label: tr("memory.xfer.import"), fn: () => { openImport(p); } },
    { sep: true },
    { label: tr("memory.proj.deleteAsk"), fn: () => { openDeleteProject(p); }, danger: true },
  ]);
}

/* ------------------------------------------------------------------ the delegated listeners */

async function onClick(ev: MouseEvent): Promise<void> {
  const t = targetEl(ev);
  if (!t) return;
  const at = (sel: string): HTMLElement | null => t.closest<HTMLElement>(sel);
  if (at("[data-mretry]")) { await mount(); return; }
  if (at("[data-mnewproject]")) { openNewProject(); return; }
  if (at("[data-membed]")) { await openEmbedding(); return; }
  if (at("[data-msync]") && S.project) {
    await openSync(S.project, { openAuto: () => { void openAutoSheet(); } });
    return;
  }
  if (at("[data-mauto]")) { await openAutoSheet(); return; }
  const pm = at("[data-mpmenu]");
  if (pm) {
    // The opening click must not reach document (the menu closes on outside clicks).
    ev.stopPropagation();
    if (menuOpen()) { closeMenu(); return; }
    projectMenu(pm);
    return;
  }
  if (S.tab === "domains" && at("#memBody") && await onDomainsClick(ev, t)) return;
  if (at("[data-mnewnote]") || at("[data-empty-action]")) { openEditor({ domain: newNoteDomain() }); return; }
  const proj = at("[data-mproject]");
  if (proj) { await hooks.selectProject(proj.dataset.mproject || ""); return; }
  const tab = at("[data-mtab]");
  if (tab) {
    // A tab picked by hand reads the counts first: its own number in the head is one of them.
    if (await reread()) hooks.showTab((tab.dataset.mtab || "notes") as MemTab);
    return;
  }
  const mode = at("[data-mmode]");
  if (mode) {
    S.mode = mode.dataset.mmode === "associate" ? "associate" : "keyword";
    await paintBody();
    return;
  }
  if (at("[data-massoc]")) {
    const text = document.getElementById("memText") as HTMLTextAreaElement | null;
    if (text) S.text = text.value;
    await loadList();
    return;
  }
  const scope = at("[data-mgscope]");
  if (scope) {
    if (scope.dataset.mgscope === "whole" && S.graph.center) { S.graph.center = null; await paintBody(); }
    return;
  }
  const depth = at("[data-mgdepth]");
  if (depth) { S.graph.depth = Number(depth.dataset.mgdepth) || 1; await paintBody(); return; }
  const center = at("[data-mgcenter]");
  if (center) {
    S.graph.center = center.dataset.mgcenter || null;
    S.graph.selected = S.graph.center;
    await paintBody();
    return;
  }
  const gopen = at("[data-mgopen]");
  if (gopen) { await hooks.openNote(gopen.dataset.mgopen || ""); return; }
  const gsel = at("[data-mgsel]");
  if (gsel) { selectNode(gsel.dataset.mgsel || null); return; }
  if (at("[data-mbrief]")) { readBriefInputs(); await loadBrief(); return; }
  if (at("[data-mcopy]")) { await copyBrief(); return; }
  if (at("[data-msource]")) {
    briefSource = !briefSource;
    const host = document.getElementById("memBrief");
    if (host) paintBrief(host);
    return;
  }
  if (at("#memDetail") && await onDetailClick(ev, t)) return;
  const open = at("[data-mopen]");
  if (open) { await hooks.openNote(open.dataset.mopen || ""); return; }
  const pick = at("[data-mnote]");
  if (pick) selectNote(pick.dataset.mnote || "");
}

/** Where a new note is filed by default: the domain the page is looking at, else by its anchors. */
function newNoteDomain(): string {
  if (S.tab === "domains") {
    const d = S.domains ? S.domains.find((x) => x.id === S.dom) : null;
    return d ? d.path : S.dom === UNFILED ? "none" : "";
  }
  return S.tab === "notes" ? S.domain : "";
}

function readBriefInputs(): void {
  const paths = document.getElementById("memBriefPaths") as HTMLTextAreaElement | null;
  const task = document.getElementById("memBriefTask") as HTMLInputElement | null;
  const global = document.getElementById("memBriefGlobal") as HTMLInputElement | null;
  if (paths) S.briefPaths = paths.value;
  if (task) S.briefTask = task.value;
  if (global) S.briefGlobal = global.checked;
}

function onChange(ev: Event): void {
  const t = targetEl(ev);
  if (!t) return;
  const value = (t as HTMLInputElement).value;
  const checked = !!(t as HTMLInputElement).checked;
  switch (t.id) {
    case "memKind": S.kind = value; break;
    case "memStatus": S.statusFilter = value; break;
    case "memDomain": S.domain = value; break;
    case "memUnconf": S.unconfirmed = checked; break;
    case "memHist": S.history = checked; break;
    case "memGraphHist": S.graph.history = checked; void paintBody(); return;
    case "memBriefGlobal": S.briefGlobal = checked; return;
    default: return;
  }
  void loadList();
}

function onInput(ev: Event): void {
  const t = targetEl(ev);
  if (!t) return;
  const value = (t as HTMLInputElement).value;
  if (t.id === "memQ" && !value.trim() && S.query) { S.query = ""; void loadList(); return; }
  if (t.id === "memText") S.text = value;
}

/** The list is a listbox: the arrows walk it (the detail follows), Home and End jump. */
function listKey(ev: KeyboardEvent, row: HTMLElement): boolean {
  const at = listIds.indexOf(row.dataset.mnote || "");
  if (at < 0) return false;
  const to = ev.key === "ArrowDown" ? at + 1 : ev.key === "ArrowUp" ? at - 1
    : ev.key === "Home" ? 0 : ev.key === "End" ? listIds.length - 1 : -1;
  if (to < 0 || to >= listIds.length || (to === at && ev.key !== "Home" && ev.key !== "End")) return ev.key.startsWith("Arrow");
  selectNote(listIds[to], true);
  return true;
}

function onKey(ev: KeyboardEvent): void {
  const t = targetEl(ev);
  if (!t) return;
  if (t.id === "memQ" && ev.key === "Enter") {
    ev.preventDefault();
    S.query = (t as HTMLInputElement).value.trim();
    void loadList();
    return;
  }
  if (t.id === "memText" && ev.key === "Enter" && (ev.ctrlKey || ev.metaKey)) {
    ev.preventDefault();
    S.text = (t as HTMLTextAreaElement).value;
    void loadList();
    return;
  }
  const row = t.closest<HTMLElement>("#memList [data-mnote]");
  if (row && listKey(ev, row)) { ev.preventDefault(); return; }
  const dom = t.closest<HTMLElement>("#memDomScroll [data-mdom]");
  if (dom && domainKey(ev, dom)) ev.preventDefault();
}

/* ------------------------------------------------------------------ hooks and the page contract */

/** Read the projects (their counts) and the map again. Notes move outside the panel too - an
 *  agent's save, the CLI, a sync - so a write here and a pick of a project or a tab both come
 *  through this. False when the page moved on or the read failed (the failure painted). */
async function reread(): Promise<boolean> {
  const visit = S.visit;
  // A write may have moved the map (a domain made, a note filed): read it again.
  S.domains = null;
  const err = await loadProjects();
  if (visit !== S.visit) return false;
  if (err) { paintFailure(err); return false; }
  return true;
}

hooks.reload = async (): Promise<void> => {
  if (!(await reread())) return;
  if (!document.getElementById("memBody")) { paint(); return; }
  paintChrome();
  await refreshBody();
};

hooks.showTab = (tab: MemTab): void => {
  S.tab = tab;
  if (!document.getElementById("memBody")) { paint(); return; }
  paintChrome();
  void paintBody();
};

hooks.selectProject = async (id: string): Promise<void> => {
  if (id !== S.project) {
    S.project = id;
    S.sel = null;
    S.graph.center = null;
    S.graph.selected = null;
    S.dom = null;
    S.domain = "";
    listIds = [];
  }
  // Every pick reads the counts again, and the list (one made a moment ago included).
  if (!(await reread())) return;
  paint();
};

hooks.changeDir = (): void => {
  const p = currentProject();
  if (!p || p.global) return;
  if (p.roots.length) openChangeDir(p);
  else openAddDir(p);
};

hooks.openDomain = (id: string): void => {
  if (!id) return;
  // An id or a path: the tab resolves it against the map it reads.
  S.dom = id;
  hooks.showTab("domains");
};

hooks.openNote = async (id: string): Promise<void> => {
  if (!id) return;
  if (S.tab === "notes" || S.tab === "review") { selectNote(id); return; }
  S.sel = id;
  hooks.showTab("notes");
};

export async function mount(): Promise<void> {
  const visit = ++S.visit;
  const pane = framePane();
  pane.onclick = (ev: MouseEvent): void => { void onClick(ev); };
  pane.onchange = onChange;
  pane.oninput = onInput;
  pane.onkeydown = onKey;
  fill(pane, workspace(null, wsScroll({ pad: true }, note(tr("memory.loading"), { busy: true }))));
  const err = await loadProjects();
  if (visit !== S.visit) return;
  if (err) { paintFailure(err); return; }
  paint();
}

export async function refresh(): Promise<void> {
  await hooks.reload();
}

export function countText(): string {
  const p = currentProject();
  return p ? trn(p.counts.live, "memory.nNotes.one", "memory.nNotes.other") : "";
}

export function unmount(): void {
  S.visit++;
  closeMenu();
  dropGraph();
  const pane = $("pane");
  pane.onclick = null;
  pane.onchange = null;
  pane.oninput = null;
  pane.onkeydown = null;
  // Take the workspace framing back off, so the next page starts from the pane's own padding.
  pane.classList.remove("full");
}

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
   Memory page - the Domains tab (SPEC §memory.domains, §memory.panel): the project's map beside
   the domain picked from it.

   The map is a tree, at most five levels: each domain with its live notes, the ones waiting on
   a person, an unconfirmed mark (an agent drew it) and drift - commits since the hub was last
   verified that touched the code it owns. Under the map two more rows: Unfiled (the notes no
   domain holds, each one click from a domain) and Not covered (the directories no domain owns,
   each one click from a new domain owning it). Drift and Not covered need Git; without it the
   rows say why in the reader's words, and offer to change the directory when that is the cure.

   A domain reads top to bottom: what it is (title, path, who drew it, when it was last true to
   the code), its summary, its hub - the Markdown an agent reads before working there - then the
   code it owns (each path opens its preview), its sub-domains, the notes filed in it, its
   history. The verbs: edit (a moved revision says so), verified, confirm, and under ⋯ a new
   sub-domain, move (the subtree moves with it), delete by typing the path (its notes go to the
   parent).
   ================================================================================================ */
import { toast } from "../util.js";
import type { HChild } from "../h.js";
import { fill, h } from "../h.js";
import { tk, tr, trn } from "../i18n.js";
import {
  btn, card, closeMenu, closeSheet, docHead, emptyNode, field, formFold, hint, iconBtn, inlineForm, kvRow, markdownNode,
  menuOpen, moreBtn, note, openFieldSheet, pickList, pickRow, pickSelect, popupMenu, refLink, relTime, row, section, sheet,
  showSheet, tag, timeline, timelineToggle, valueBlock, wsBar, wsScroll, wsSplit,
} from "../ui/index.js";
import type { MenuItem, TimelineItem } from "../ui/index.js";
import type {
  MemCoverage, MemDomain, MemDomainConfirmed, MemDomainDeleted, MemDomainDetail, MemDomainList, MemDomainRevision, MemDomainSnapshot, MemSummary,
} from "../types/memory.js";
import {
  S, UNCOVERED, UNFILED, ageText, errMessage, fullTime, gitIssue, hooks, isLive, keywordChips, keywordList, kindChip, lines, markerTags,
  noteButton,
  opLabel, originLabel, qs, req, seg, statusTag,
} from "./memory-core.js";
import type { Reply } from "./memory-core.js";
import { askFile, openEditor } from "./memory-note.js";
import { openCommit, openFile } from "./memory-preview.js";

/** The map and its coverage as last read: the rows, the drift tags and Not covered come from here. */
let map: MemDomainList | null = null;
let cov: MemCoverage | null = null;
/** Why the last coverage read failed; null with no coverage: it has not answered yet. */
let covErr: string | null = null;
/** Bumped by every coverage read: only the latest one's answer paints. */
let covAsk = 0;
/** The domain the detail shows, as last read: its verbs act on this. */
let cur: MemDomainDetail | null = null;
/** Bumped by every detail load: a late answer for a pick no longer current paints nothing. */
let seq = 0;

const DEPTH_MAX = 5;

/** The ids the two lists show, in order: the arrows walk them. */
function pickIds(): string[] {
  return map ? [...map.items.map((d) => d.id), UNFILED, UNCOVERED] : [];
}

function coverageOf(id: string): MemCoverage["domains"][number] | null {
  return cov ? cov.domains.find((c) => c.id === id) || null : null;
}

/** What a confirm of the domain whole would take on, from the map: the unconfirmed domains in
 *  its subtree and their unconfirmed live notes (some may need review and stay). */
function openInTree(d: MemDomain): { domains: number; notes: number } {
  const tree = map ? map.items.filter((x) => x.id === d.id || x.path.startsWith(d.path + "/")) : [d];
  return {
    domains: tree.filter((x) => !x.confirmed).length,
    notes: tree.reduce((n, x) => n + x.counts.unconfirmed, 0),
  };
}

/** The toast after a whole confirm: the notes it took on, and the ones still waiting. */
function confirmedWords(c: MemDomainConfirmed): string {
  const done = c.notes ? trn(c.notes, "memory.dom.confirmedNotes.one", "memory.dom.confirmedNotes.other") : tr("memory.dom.confirmDone");
  return c.held ? done + " · " + trn(c.held, "memory.dom.confirmHeld.one", "memory.dom.confirmHeld.other") : done;
}

/* ------------------------------------------------------------------ the map column */

/** Paint the tab in place of `slot`: the map beside the domain picked from it. */
export async function paintDomains(slot: HTMLElement): Promise<void> {
  const host = wsSplit({
    id: slot.id, fixed: "start",
    start: [
      wsBar({},
        btn(tr("memory.dom.new"), { icon: "plus", data: { mdnew: "" } }),
        h("span", { class: "grow" }),
        btn(tr("memory.dom.autofile"), { kind: "ghost", data: { mdauto: "" }, title: tr("memory.dom.autofileWhy") })),
      wsScroll({ id: "memDomScroll" }, h("div", { id: "memDomMap" }, note(tr("memory.loading"), { busy: true }))),
      wsBar({ foot: true, id: "memDomFoot" }),
    ],
    end: wsScroll({ pad: true, id: "memDomDetail" }),
    startLabel: tr("memory.tab.domains"), endLabel: tr("memory.dom.detail"),
  });
  slot.replaceWith(host);
  await refreshDomains();
}

/** Re-read the map and the pick - after a write, or on first paint. The map paints from the list
 *  alone; coverage (Git, and every file against every owns entry: the slow read) fills in its
 *  numbers when it answers, and a write's refresh never waits for it. */
export async function refreshDomains(): Promise<void> {
  const host = document.getElementById("memDomMap");
  if (!host) return;
  const visit = S.visit;
  const project = S.project;
  const ask = ++covAsk;
  const covered = req<MemCoverage>("GET", "/domains/coverage" + qs({ project }));
  const m = await req<MemDomainList>("GET", "/domains" + qs({ project }));
  if (visit !== S.visit || project !== S.project || !host.isConnected) return;
  if (!m.ok || !m.data) {
    map = null;
    fill(host, note(errMessage(m.err), { err: true }));
    return;
  }
  map = m.data;
  S.domains = map.items;
  S.domainsOf = project;
  // A pick may be a path (a note's domain line steps here by path) or gone (deleted, moved away).
  const hit = map.items.find((d) => d.id === S.dom || d.path === S.dom);
  if (hit) S.dom = hit.id;
  else if (S.dom !== UNFILED && S.dom !== UNCOVERED) S.dom = map.items.length ? map.items[0].id : null;
  paintMap(host, map);
  void coverageLanded(covered, ask, visit, project);
  await showDomain();
}

/** Coverage answered: its numbers fill in on the map, and in the detail where it shows them.
 *  An answer an ask after it overtook, or for a page left, paints nothing. */
async function coverageLanded(covered: Promise<Reply<MemCoverage>>, ask: number, visit: number, project: string): Promise<void> {
  const c = await covered;
  if (ask !== covAsk || visit !== S.visit || project !== S.project) return;
  const host = document.getElementById("memDomMap");
  if (!host || !map) return;
  cov = c.ok ? c.data : null;
  covErr = c.ok ? null : errMessage(c.err);
  paintMap(host, map);
  const detail = document.getElementById("memDomDetail");
  if (!detail) return;
  if (S.dom === UNCOVERED) {
    paintUncovered(detail);
  } else if (cur && cur.id === S.dom) {
    const keep = detail.scrollTop;
    fill(detail, domainDoc(cur));
    detail.scrollTop = keep;
  }
}

/** The map column and its foot, where they were scrolled and with the focused row still focused. */
function paintMap(host: HTMLElement, m: MemDomainList): void {
  const scroll = document.getElementById("memDomScroll");
  const keep = scroll ? scroll.scrollTop : 0;
  const active = document.activeElement;
  const focused = active instanceof HTMLElement && host.contains(active) ? active.dataset.mdom : undefined;
  fill(host, mapNode(m));
  if (scroll) scroll.scrollTop = keep;
  if (focused !== undefined) Array.from(host.querySelectorAll<HTMLElement>("[data-mdom]")).find((r) => r.dataset.mdom === focused)?.focus();
  const foot = document.getElementById("memDomFoot");
  if (foot) {
    fill(foot, trn(m.items.length, "memory.dom.n.one", "memory.dom.n.other"),
      cov && !cov.git ? [" · ", h("span", { title: gitIssue(cov.issue, cov.reason).why }, gitIssue(cov.issue, cov.reason).short)] : null);
  }
}

function mapNode(m: MemDomainList): HChild {
  const tree = m.items.length
    ? pickList({ label: tr("memory.dom.map"), id: "memDomList" }, m.items.map(domainRow))
    : hint(tr("memory.dom.mapEmpty"));
  const uncovered = cov && cov.git ? String(cov.uncovered.length) : "—";
  return [
    tree,
    pickList({ label: tr("memory.dom.more"), id: "memDomMore" },
      pickRow({
        name: tr("memory.dom.unfiled"), meta: String(m.unfiled.live), metaTitle: trn(m.unfiled.live, "memory.nNotes.one", "memory.nNotes.other"),
        sub: h("span", null, tr("memory.dom.unfiledSub")), selected: S.dom === UNFILED, muted: !m.unfiled.live, data: { mdom: UNFILED },
      }),
      pickRow({
        name: tr("memory.dom.uncovered"), meta: uncovered,
        sub: h("span", null, cov && !cov.git ? gitIssue(cov.issue, cov.reason).short : tr("memory.dom.uncoveredSub")),
        selected: S.dom === UNCOVERED, muted: !cov || !cov.git || !cov.uncovered.length, data: { mdom: UNCOVERED },
      })),
  ];
}

/** One domain on the map: indented under its parent; its note count; under it what waits on a
 *  person and how far the code moved since the hub was verified, then the summary. */
function domainRow(d: MemDomain): HTMLElement {
  const c = coverageOf(d.id);
  return pickRow({
    name: d.title,
    depth: d.depth - 1,
    meta: String(d.counts.live),
    metaTitle: trn(d.counts.live, "memory.nNotes.one", "memory.nNotes.other"),
    sub: [
      d.confirmed ? null : tag(tr("memory.marker.unconfirmed")),
      d.counts.needsReview ? tag(trn(d.counts.needsReview, "memory.dom.review.one", "memory.dom.review.other"), { tone: "warn" }) : null,
      c && c.drift ? tag(trn(c.drift, "memory.dom.drift.one", "memory.dom.drift.other"), { tone: "warn" }) : null,
      h("span", null, d.summary || d.path),
    ],
    selected: S.dom === d.id,
    data: { mdom: d.id },
  });
}

/** Pick a row: the mark moves in both lists, the detail follows. */
export function selectDomain(id: string, focus = false): void {
  S.dom = id;
  let hit: HTMLElement | null = null;
  for (const listId of ["memDomList", "memDomMore"]) {
    const list = document.getElementById(listId);
    const b = list ? pickSelect(list, (r) => r.dataset.mdom === id) : null;
    if (b) hit = b;
  }
  if (hit) {
    if (typeof hit.scrollIntoView === "function") hit.scrollIntoView({ block: "nearest" });
    if (focus) hit.focus();
  }
  void showDomain();
}

/** The two lists are one listbox to the keyboard: the arrows walk them, Home and End jump. */
export function domainKey(ev: KeyboardEvent, rowEl: HTMLElement): boolean {
  const ids = pickIds();
  const at = ids.indexOf(rowEl.dataset.mdom || "");
  if (at < 0) return false;
  const to = ev.key === "ArrowDown" ? at + 1 : ev.key === "ArrowUp" ? at - 1
    : ev.key === "Home" ? 0 : ev.key === "End" ? ids.length - 1 : -1;
  if (to < 0 || to >= ids.length || (to === at && ev.key !== "Home" && ev.key !== "End")) return ev.key.startsWith("Arrow");
  selectDomain(ids[to], true);
  return true;
}

/* ------------------------------------------------------------------ the detail */

async function showDomain(): Promise<void> {
  const host = document.getElementById("memDomDetail");
  if (!host) return;
  const mine = ++seq;
  const project = S.project;
  if (S.dom === UNFILED) { cur = null; await paintUnfiled(host, mine); return; }
  if (S.dom === UNCOVERED) { cur = null; paintUncovered(host); return; }
  if (!S.dom) {
    cur = null;
    fill(host, emptyNode({ icon: "brain", title: tr("memory.dom.none"), hint: tr("memory.dom.noneHint"), action: tr("memory.dom.new") }));
    return;
  }
  const same = !!cur && cur.id === S.dom;
  if (!same) fill(host, note(tr("memory.loading"), { busy: true }));
  const r = await req<{ domain: MemDomainDetail }>("GET", "/domains/" + seg(S.dom) + qs({ project, history: true }));
  if (mine !== seq || project !== S.project || !host.isConnected) return;
  if (!r.ok || !r.data) {
    cur = null;
    fill(host, note(errMessage(r.err), { err: true }));
    return;
  }
  const keep = same ? host.scrollTop : 0;
  cur = r.data.domain;
  fill(host, domainDoc(cur));
  host.scrollTop = keep;
}

/** Where the domain sits: each ancestor a step to it, then its own path. */
function crumbs(d: MemDomainDetail): HChild {
  const out: HChild[] = [];
  for (const a of d.ancestors) out.push(refLink(a.title, { data: { mdom: a.id }, title: a.path }), " / ");
  out.push(h("code", null, d.path));
  return h("span", { data: { mdcrumbs: "" } }, out);
}

function domainDoc(d: MemDomainDetail): HTMLElement {
  const c = coverageOf(d.id);
  // One click confirms the domain whole; the words say when that is more than its own mark.
  const open = openInTree(d);
  const whole = open.notes > 0 || open.domains > (d.confirmed ? 0 : 1);
  const confirm = !open.domains && !open.notes ? null
    : whole ? btn(tr("memory.dom.confirmAll"), { data: { mdact: "confirm" }, title: tr("memory.dom.confirmAllWhy", { notes: open.notes }) })
    : btn(tr("memory.confirm"), { data: { mdact: "confirm" }, title: tr("memory.dom.confirmWhy") });
  const acts: HChild[] = [
    btn(tr("memory.dom.edit"), { kind: "primary", data: { mdact: "edit" } }),
    btn(tr("memory.mark.verified"), { data: { mdact: "verify" }, title: tr("memory.dom.verifiedWhy") }),
    confirm,
    moreBtn(tr("memory.dom.moreOf", { title: d.title }), { data: { mdact: "more" } }),
  ];
  return h("article", { data: { mdomdoc: d.id } },
    docHead({
      tags: [
        d.confirmed ? null : tag(tr("memory.marker.unconfirmed")),
        c && c.drift ? [" ", tag(trn(c.drift, "memory.dom.drift.one", "memory.dom.drift.other"), { tone: "warn" })] : null,
      ],
      title: d.title,
      lede: d.summary,
      meta: [
        crumbs(d),
        tr("memory.originBy", { origin: originLabel(d.origin), actor: d.actor }),
        h("span", { title: tr("memory.detail.createdAt", { when: fullTime(d.createdMs) }) }, tr("memory.detail.updated", { when: relTime(d.updatedMs) })),
        h("span", { title: fullTime(d.verifiedMs), data: { mdverified: "" } }, tr("memory.detail.verified", { when: relTime(d.verifiedMs) })),
        tr("memory.revN", { n: d.revision }),
      ],
      actions: acts,
    }),
    h("div", { id: "memDomErr" }),
    c && c.drift ? driftNote(c.drift, c.latest || null) : null,
    d.body ? markdownNode(d.body, { class: "note-body" }) : note(tr("memory.dom.noHub"), { data: { mdnohub: "" } }),
    d.keywords && d.keywords.length ? section({ cap: tr("memory.f.keywords") }, keywordChips(d.keywords)) : null,
    section({ cap: tr("memory.dom.owns"), note: c && c.files != null ? trn(c.files, "memory.dom.files.one", "memory.dom.files.other") : undefined },
      d.owns.length
        ? card(d.owns.map((path) => row({
            name: refLink(path, { code: true, title: tr("memory.pv.openAnchor", { path }), data: { mpvfile: path } }),
            primary: iconBtn("history", tr("memory.pv.historyOf", { path }), { ghost: true, data: { mpvfile: path, mpvtab: "history" } }),
          })))
        : hint(tr("memory.dom.ownsNone"))),
    section({
      cap: tr("memory.dom.subDomains"),
      tools: d.depth < DEPTH_MAX ? [btn(tr("memory.dom.newSub"), { kind: "ghost", data: { mdact: "newsub" } })] : undefined,
    },
      d.children.length
        ? card(d.children.map((ch) => row({
            name: refLink(ch.title, { data: { mdom: ch.id }, title: ch.path }),
            sub: [h("code", null, ch.path), ch.summary ? " · " + ch.summary : null],
            cols: [{ v: String(ch.counts.live), w: "s", title: trn(ch.counts.live, "memory.nNotes.one", "memory.nNotes.other") }],
          })))
        : hint(tr("memory.dom.subNone"))),
    section({
      cap: tr("memory.dom.notes"), note: tr("memory.dom.notesNote"),
      tools: [
        btn(tr("memory.dom.newNote"), { kind: "ghost", data: { mdact: "newnote" } }),
        btn(tr("memory.dom.inNotes"), { kind: "ghost", data: { mdact: "notes" }, title: tr("memory.dom.inNotesWhy") }),
      ],
    },
      d.notes.length ? card(d.notes.map(noteRow)) : hint(tr("memory.dom.notesNone"))),
    d.history && d.history.length
      ? section({ cap: tr("memory.f.history") },
          formFold({ summary: trn(d.history.length, "memory.revisions.one", "memory.revisions.other"), id: "memDomHistFold" }, historyNode(d.history)))
      : null);
}

/** Commits since the hub was verified that touched owned code: read them, fix the hub, verify. */
function driftNote(n: number, latest: { hash: string; subject: string } | null): HTMLElement {
  return note([
    trn(n, "memory.dom.driftSince.one", "memory.dom.driftSince.other"),
    latest ? [" ", tr("memory.dom.driftLatest"), " ", refLink(latest.subject, { title: tr("memory.pv.openCommit", { rev: latest.hash.slice(0, 7) }), data: { mpvcommit: latest.hash } })] : null,
    " ", tr("memory.dom.driftAsk"),
  ], { data: { mddrift: String(n) } });
}

function noteRow(x: MemSummary): HTMLElement {
  return row({
    lead: kindChip(x.kind, { bare: true }),
    name: [noteButton(x), " ", statusTag(x.status), markerTags(x.markers)],
    sub: x.excerpt || undefined,
    cols: [{ v: ageText(x.updatedMs), w: "s", title: tr("memory.updatedAt", { when: fullTime(x.updatedMs) }) }],
    muted: !isLive(x.status),
  });
}

/* ------------------------------------------------------------------ history */

const yesNo = (v: boolean): string => (v ? tr("memory.yes") : tr("memory.no"));

const DIFF_FIELDS: Array<[string, (s: MemDomainSnapshot) => string]> = [
  [tk("memory.dom.f.path"), (s) => s.path],
  [tk("memory.f.title"), (s) => s.title],
  [tk("memory.dom.f.summary"), (s) => s.summary],
  [tk("memory.dom.owns"), (s) => s.owns.join(", ")],
  [tk("memory.f.keywords"), (s) => (s.keywords || []).join(", ")],
  [tk("memory.f.confirmed"), (s) => yesNo(s.confirmed)],
];

function diffBody(rv: MemDomainRevision, prev: MemDomainRevision | null): HChild {
  const a = prev ? prev.snapshot : null;
  const b = rv.snapshot;
  const rows: HTMLElement[] = [];
  const dash = "—";
  for (const [key, get] of DIFF_FIELDS) {
    const after = get(b);
    const before = a ? get(a) : null;
    if (before === after || (before === null && !after)) continue;
    rows.push(kvRow(tr(key), before === null ? after : [h("del", null, before || dash), " → ", h("ins", null, after || dash)]));
  }
  const out: HChild[] = [rows.length ? card(rows) : null];
  if ((!a && b.body) || (a && a.body !== b.body)) {
    if (a) out.push(valueBlock({ label: tr("memory.diff.before"), text: a.body }));
    out.push(valueBlock({ label: a ? tr("memory.diff.after") : tr("memory.dom.f.hub"), text: b.body }));
  }
  if (!rows.length && out.length === 1) out.push(hint(tr("memory.diff.none")));
  return out;
}

function historyNode(revisions: MemDomainRevision[]): HTMLElement {
  const sorted = revisions.slice().sort((x, y) => y.revision - x.revision);
  const prevOf = (rv: MemDomainRevision): MemDomainRevision | null => sorted.find((x) => x.revision < rv.revision) || null;
  const items: TimelineItem[] = sorted.map((rv) => ({
    id: "d" + rv.revision,
    at: rv.atMs,
    title: [tr("memory.revN", { n: rv.revision }), " ", opLabel(rv.op)],
    arg: rv.reason || undefined,
    who: rv.actor,
    data: { mdrev: rv.revision },
  }));
  const body = (it: TimelineItem): HChild => {
    const rv = sorted.find((x) => "d" + x.revision === it.id);
    return rv ? diffBody(rv, prevOf(rv)) : null;
  };
  const node = timeline(items, { open: new Set(items.length ? [items[0].id] : []), body, showWho: true });
  node.onclick = (e) => {
    const sum = e.target instanceof Element ? e.target.closest(".tl-sum") : null;
    const item = sum ? sum.closest<HTMLElement>(".tl-item") : null;
    if (!item) return;
    const id = item.dataset.tlId || "";
    const it = items.find((x) => x.id === id);
    timelineToggle(node, id, it ? body(it) : undefined);
  };
  return node;
}

/* ------------------------------------------------------------------ Unfiled and Not covered */

async function paintUnfiled(host: HTMLElement, mine: number): Promise<void> {
  const project = S.project;
  fill(host, note(tr("memory.loading"), { busy: true }));
  const r = await req<{ items: MemSummary[]; truncated: boolean }>("GET", "/records" + qs({ project, domain: "none", limit: 200 }));
  if (mine !== seq || project !== S.project || !host.isConnected) return;
  if (!r.ok || !r.data) { fill(host, note(errMessage(r.err), { err: true })); return; }
  const items = r.data.items;
  fill(host, h("article", { data: { mdomdoc: UNFILED } },
    docHead({
      title: tr("memory.dom.unfiled"),
      lede: tr("memory.dom.unfiledWhy"),
      meta: [r.data.truncated ? tr("memory.truncated", { n: items.length }) : trn(items.length, "memory.nNotes.one", "memory.nNotes.other")],
      actions: [btn(tr("memory.dom.autofile"), { kind: "primary", data: { mdauto: "" }, title: tr("memory.dom.autofileWhy") })],
    }),
    h("div", { id: "memDomErr" }),
    items.length
      ? card(items.map((x) => row({
          lead: kindChip(x.kind, { bare: true }),
          name: [noteButton(x), " ", statusTag(x.status), markerTags(x.markers)],
          sub: x.excerpt || undefined,
          primary: btn(tr("memory.dom.fileAsk"), { data: { mdfile: x.id, mdfiletitle: x.title } }),
        })))
      : emptyNode({ icon: "check", title: tr("memory.dom.unfiledNone") })));
}

function paintUncovered(host: HTMLElement): void {
  const head = docHead({ title: tr("memory.dom.uncovered") });
  if (!cov) {
    fill(host, head, covErr === null ? note(tr("memory.dom.coverageWait"), { busy: true }) : note(covErr || tr("memory.err.failed"), { err: true }));
    return;
  }
  if (!cov.git) {
    const g = gitIssue(cov.issue, cov.reason);
    const fix = g.fix ? btn(tr("memory.proj.changeDirAsk"), { data: { mdchangedir: "" } }) : null;
    if (fix) fix.onclick = () => { hooks.changeDir(); };
    fill(host, h("article", { data: { mdomdoc: UNCOVERED } },
      docHead({ title: tr("memory.dom.uncovered"), lede: tr("memory.dom.uncoveredWhy"), actions: fix ? [fix] : undefined }),
      note(g.why, { data: { mdnogit: "" } })));
    return;
  }
  const dirs = cov.uncovered;
  fill(host, h("article", { data: { mdomdoc: UNCOVERED } },
    docHead({ title: tr("memory.dom.uncovered"), lede: tr("memory.dom.uncoveredWhy"), meta: [trn(dirs.length, "memory.dom.dirs.one", "memory.dom.dirs.other")] }),
    cov.filesTruncated ? note(tr("memory.dom.filesTruncated")) : null,
    dirs.length
      ? card(dirs.map((u) => row({
          name: refLink(u.path + "/", { code: true, title: tr("memory.pv.openAnchor", { path: u.path }), data: { mpvfile: u.path } }),
          cols: [{ v: trn(u.files, "memory.dom.files.one", "memory.dom.files.other"), w: "m" }],
          primary: btn(tr("memory.dom.own"), { data: { mdown: u.path }, title: tr("memory.dom.ownWhy", { path: u.path }) }),
        })))
      : emptyNode({ icon: "check", title: tr("memory.dom.uncoveredNone") })));
}

/* ------------------------------------------------------------------ writes */

function domErr(text: string): void {
  const host = document.getElementById("memDomErr");
  if (host) fill(host, hint(text, { bad: true, live: true }));
}

/** A path segment from a directory's last name: what a new domain owning it is called. */
export function pathSuggestion(dir: string): string {
  const last = dir.split("/").filter(Boolean).pop() || "";
  const slug = last.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^[-_]+/, "").slice(0, 40).replace(/-+$/, "");
  return slug;
}

/** New domain, or edit one (a changed path moves it, its subtree with it). A new one may start
 *  under a parent, or owning a directory Not covered named. */
export function openDomainEditor(o: { domain?: MemDomainDetail; parent?: string; owns?: string[] }): void {
  const d = o.domain || null;
  const startPath = d ? d.path : o.parent ? o.parent + "/" : o.owns && o.owns.length ? pathSuggestion(o.owns[0]) : "";
  const path = h("input", { id: "memDomPath", autocomplete: "off", spellcheck: false, value: startPath, placeholder: tr("memory.dom.pathHint") });
  const title = h("input", { id: "memDomTitle", autocomplete: "off", value: d ? d.title : "", placeholder: tr("memory.dom.titleHint") });
  const summary = h("input", { id: "memDomSummary", autocomplete: "off", value: d ? d.summary : "", placeholder: tr("memory.dom.summaryHint") });
  const owns = h("textarea", { id: "memDomOwns", rows: 3, spellcheck: false, placeholder: tr("memory.dom.ownsHint") });
  owns.value = (d ? d.owns : o.owns || []).join("\n");
  const keywords = h("input", {
    id: "memDomKeywords", autocomplete: "off", value: d ? (d.keywords || []).join(", ") : "", placeholder: tr("memory.ed.keywordsHint"),
  });
  const body = h("textarea", { id: "memDomBody", rows: 14, placeholder: tr("memory.dom.hubTemplate") });
  body.value = d ? d.body || "" : "";
  const reason = h("input", { id: "memDomReason", autocomplete: "off", placeholder: tr("memory.ed.reasonHint") });
  const err = hint("", { id: "memDomEdErr", bad: true, hidden: true, live: true });
  const conflict = h("div", { id: "memDomEdConflict" });
  const cancel = btn(tr("ui.cancel"));
  const save = btn(d ? tr("memory.ed.save") : tr("memory.dom.create"), { kind: "primary", id: "memDomSave" });
  showSheet(sheet({
    title: d ? tr("memory.dom.editTitle") : tr("memory.dom.newTitle"),
    sub: d ? d.path : undefined,
    wide: true,
    body: [
      field({ label: tr("memory.dom.f.path"), control: path, required: true, hint: d ? tr("memory.dom.pathMoveWhy") : tr("memory.dom.pathWhy") }),
      field({ label: tr("memory.f.title"), control: title, required: true }),
      field({ label: tr("memory.dom.f.summary"), control: summary, hint: tr("memory.dom.summaryWhy") }),
      field({ label: tr("memory.dom.owns"), control: owns, hint: tr("memory.dom.ownsWhy") }),
      field({ label: tr("memory.f.keywords"), control: keywords, hint: tr("memory.ed.keywordsWhy") }),
      field({ label: tr("memory.dom.f.hub"), control: body, hint: tr("memory.dom.hubWhy") }),
      d ? field({ label: tr("memory.reason"), control: reason }) : null,
      err,
      conflict,
    ],
    foot: [h("span", { class: "grow" }), cancel, save],
  }));
  const fail = (text: string): void => { err.textContent = text; err.hidden = false; };
  const send = async (): Promise<void> => {
    err.hidden = true;
    fill(conflict);
    const p = path.value.trim().replace(/^\/+|\/+$/g, "");
    const t = title.value.trim();
    if (!p || !t) { fail(tr("memory.dom.required")); return; }
    save.disabled = true;
    try {
      const kw = keywordList(keywords.value);
      if (d) await sendEdit(d, { path: p, title: t, summary: summary.value.trim(), owns: lines(owns.value), keywords: kw, body: body.value, reason: reason.value.trim() }, fail, conflict);
      else await sendNew({ path: p, title: t, summary: summary.value.trim(), owns: lines(owns.value), keywords: kw, body: body.value }, fail);
    } finally {
      save.disabled = false;
    }
  };
  cancel.onclick = closeSheet;
  save.onclick = () => { void send(); };
  (d ? title : path).focus();
}

interface DomainDraft { path: string; title: string; summary: string; owns: string[]; keywords: string[]; body: string }

async function sendNew(dr: DomainDraft, fail: (t: string) => void): Promise<void> {
  const body: Record<string, unknown> = { project: S.project, path: dr.path, title: dr.title, owns: dr.owns };
  if (dr.summary) body.summary = dr.summary;
  if (dr.keywords.length) body.keywords = dr.keywords;
  if (dr.body.trim()) body.body = dr.body;
  const r = await req<{ domain: MemDomain }>("POST", "/domains", body);
  if (!r.ok || !r.data) { fail(errMessage(r.err)); return; }
  closeSheet();
  toast(tr("memory.dom.created", { path: r.data.domain.path }));
  S.dom = r.data.domain.id;
  if (S.tab !== "domains") { hooks.showTab("domains"); return; }
  void hooks.reload();
}

async function sendEdit(d: MemDomainDetail, dr: DomainDraft & { reason: string }, fail: (t: string) => void, conflict: HTMLElement): Promise<void> {
  const body: Record<string, unknown> = { project: S.project, expectedRevision: d.revision };
  // Only the fields that changed travel; when none did there is nothing to send.
  const fixed = Object.keys(body).length;
  if (dr.path !== d.path) body.path = dr.path;
  if (dr.title !== d.title) body.title = dr.title;
  if (dr.summary !== d.summary) body.summary = dr.summary;
  if (dr.owns.join("\n") !== d.owns.join("\n")) body.owns = dr.owns;
  if (dr.keywords.join("\n") !== (d.keywords || []).join("\n")) body.keywords = dr.keywords;
  if (dr.body !== (d.body || "")) body.body = dr.body;
  if (Object.keys(body).length === fixed) { closeSheet(); return; }
  if (dr.reason) body.reason = dr.reason;
  const r = await req<{ domain: MemDomain }>("PUT", "/domains/" + seg(d.id), body);
  if (r.ok) {
    closeSheet();
    toast(body.path ? tr("memory.dom.moved", { path: dr.path }) : tr("memory.ed.saved"));
    void hooks.reload();
    return;
  }
  const e = r.err;
  if (e && e.error === "memory_revision_conflict") {
    const reload = btn(tr("memory.ed.reload"), { data: { mdreload: "" } });
    reload.onclick = () => { void reopenEditor(d.id); };
    fill(conflict, hint(tr("memory.ed.conflict", { n: e.current ?? "?", mine: d.revision }), { bad: true, live: true }), inlineForm(reload));
    return;
  }
  fail(errMessage(e));
}

/** A moved revision's way back: the editor again, on what the store holds now. */
async function reopenEditor(id: string): Promise<void> {
  const r = await req<{ domain: MemDomainDetail }>("GET", "/domains/" + seg(id) + qs({ project: S.project }));
  if (!r.ok || !r.data) { toast(errMessage(r.err), true); return; }
  openDomainEditor({ domain: r.data.domain });
}

function askMove(d: MemDomainDetail): void {
  openFieldSheet({
    title: tr("memory.dom.moveTitle"), sub: d.path, label: tr("memory.dom.moveLabel"), def: d.path, save: tr("memory.dom.move"),
    submit: async (to: string): Promise<boolean | string> => {
      const path = to.replace(/^\/+|\/+$/g, "");
      if (path === d.path) return true;
      const r = await req<{ domain: MemDomain }>("PUT", "/domains/" + seg(d.id), { project: S.project, expectedRevision: d.revision, path });
      if (!r.ok) return errMessage(r.err);
      toast(tr("memory.dom.moved", { path }));
      setTimeout(() => { void hooks.reload(); }, 0);
      return true;
    },
  });
}

/** Delete by typing the path: the notes filed here go to the parent (or unfiled at the top). */
function askDelete(d: MemDomainDetail): void {
  openFieldSheet({
    title: tr("memory.dom.deleteTitle"), sub: d.path, label: tr("memory.dom.deleteLabel", { path: d.path }), placeholder: d.path,
    save: tr("memory.dom.delete"), exact: true,
    submit: async (typed: string): Promise<boolean | string> => {
      if (typed !== d.path) return tr("memory.dom.deleteMismatch");
      const r = await req<MemDomainDeleted>("DELETE", "/domains/" + seg(d.id) + qs({ project: S.project, confirm: d.path }));
      if (!r.ok || !r.data) return errMessage(r.err);
      const x = r.data;
      toast(x.to
        ? trn(x.refiled, "memory.dom.deletedTo.one", "memory.dom.deletedTo.other", { path: x.path, to: x.to })
        : trn(x.refiled, "memory.dom.deletedLoose.one", "memory.dom.deletedLoose.other", { path: x.path }));
      const parent = d.ancestors.length ? d.ancestors[d.ancestors.length - 1].id : null;
      S.dom = parent;
      setTimeout(() => { void hooks.reload(); }, 0);
      return true;
    },
  });
}

function domainMenu(anchor: HTMLElement, d: MemDomainDetail): void {
  const items: MenuItem[] = [
    { label: tr("memory.dom.newSubAsk"), fn: () => { openDomainEditor({ parent: d.path }); }, disabled: d.depth >= DEPTH_MAX },
    { label: tr("memory.dom.moveAsk"), fn: () => { askMove(d); } },
    { sep: true },
    { label: tr("memory.dom.deleteAsk"), fn: () => { askDelete(d); }, danger: true },
  ];
  popupMenu(anchor.getBoundingClientRect(), items);
}

async function autofile(): Promise<void> {
  const r = await req<{ filed: number }>("POST", "/domains/autofile", { project: S.project });
  if (!r.ok || !r.data) { domErr(errMessage(r.err)); toast(errMessage(r.err), true); return; }
  toast(r.data.filed ? trn(r.data.filed, "memory.dom.autofiled.one", "memory.dom.autofiled.other") : tr("memory.dom.autofiledNone"));
  await hooks.reload();
}

/* ------------------------------------------------------------------ the tab's clicks */

/** A click inside the tab's body; true when it was the tab's own. Opening a note ([data-mopen])
 *  is the page's: it walks to the note on Notes. */
export async function onDomainsClick(ev: MouseEvent, t: Element): Promise<boolean> {
  const at = (sel: string): HTMLElement | null => t.closest<HTMLElement>(sel);
  if (at("[data-mdnew]") || at("#memDomDetail [data-empty-action]")) { openDomainEditor({}); return true; }
  if (at("[data-mdauto]")) { await autofile(); return true; }
  const pick = at("[data-mdom]");
  if (pick) { selectDomain(pick.dataset.mdom || ""); return true; }
  const own = at("[data-mdown]");
  if (own) { openDomainEditor({ owns: [own.dataset.mdown || ""] }); return true; }
  const file = at("[data-mdfile]");
  if (file) { await askFile([file.dataset.mdfile || ""], file.dataset.mdfiletitle || ""); return true; }
  const pv = at("[data-mpvfile]");
  if (pv) {
    openFile({ path: pv.dataset.mpvfile || "", tab: pv.dataset.mpvtab === "history" ? "history" : "file" });
    return true;
  }
  const commit = at("[data-mpvcommit]");
  if (commit) { openCommit(commit.dataset.mpvcommit || ""); return true; }
  const a = at("[data-mdact]");
  const d = cur;
  if (!a || !d) return false;
  switch (a.dataset.mdact) {
    case "more":
      // The opening click must not reach document (the menu closes on outside clicks).
      ev.stopPropagation();
      if (menuOpen()) { closeMenu(); return true; }
      domainMenu(a, d);
      return true;
    case "edit": openDomainEditor({ domain: d }); return true;
    case "newsub": openDomainEditor({ parent: d.path }); return true;
    case "newnote": openEditor({ domain: d.path }); return true;
    case "notes":
      S.domain = d.path;
      S.sel = null;
      hooks.showTab("notes");
      return true;
    case "verify": {
      const r = await req<{ domain: MemDomain }>("POST", "/domains/" + seg(d.id) + "/verify", { project: S.project });
      if (!r.ok) { domErr(errMessage(r.err)); return true; }
      toast(tr("memory.dom.verifiedDone"));
      await hooks.reload();
      return true;
    }
    case "confirm": {
      const r = await req<MemDomainConfirmed>("POST", "/domains/" + seg(d.id) + "/confirm", { project: S.project, all: true });
      if (!r.ok || !r.data) { domErr(errMessage(r.err)); return true; }
      toast(confirmedWords(r.data));
      await hooks.reload();
      return true;
    }
    default:
      return false;
  }
}

/** The tab is leaving, or the project changed: forget what was read. */
export function dropDomains(): void {
  map = null;
  cov = null;
  covErr = null;
  cur = null;
  seq++;
}

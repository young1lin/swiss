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
   Memory page - one note (SPEC §memory 11.2 "Note detail"): the detail pane that shows a note
   whole beside the list it was picked from, the editor that writes one (new, edit, supersede),
   the link picker, and the feedback verbs (mark, confirm, restore, delete) Notes and Review share.

   The detail reads top to bottom as a person asks about a note: what it is (kind, state, title,
   who and when), what to do about it (Edit, Verified, Confirm, the rest under ⋯), what it says,
   then how it sits among the others - its own small graph, its links by kind (each one walks to
   the note at the other end, or comes off), the add-link picker, and the notes it relates to
   without a link yet, each linkable in a click. Anchors, refs and the revision history close it.

   Every write answers in place: a refused save keeps what was typed beside the reason, a similar
   title offers "supersede it" or "save anyway", a moved revision says so and offers a reload.
   Nothing is ever overwritten silently - an edit always carries the revision it was read at.
   ================================================================================================ */
import { toast } from "../util.js";
                                      
import { fill, h } from "../h.js";
import { tk, tr, trn } from "../i18n.js";
import {
  btn, card, closeMenu, closeSheet, docHead, emptyNode, field, formCap, formFold, graphView, hint, iconBtn, inlineForm,
  kvRow, markdownNode, menuOpen, moreBtn, note, openFieldSheet, popupMenu, refLink, relTime, row, section, sheet, showSheet, tag,
  timeline, timelineToggle, valueBlock,
} from "../ui/index.js";
                                                             
             
                                                                                                                                                 
                            
import { graphEngine } from "../graph-engine.js";
import {
  KINDS, LINKABLE, LINK_KINDS, S, anchorLabel, currentProject, domainFill, domainList, domainSelect, errMessage, fullTime, hooks, isLive,
  keywordChips, keywordList, kindChip, kindLabel, linkInLabel, linkLabel, lines, markerTags, noteButton, opLabel, producerLabel, qs, reasonsNode,
  relationLabel, req, seg, sourceLabel, statusLabel, statusTag, transportLabel,
} from "./memory-core.js";
                                              
                                                    
import { edgeOf, nodeOf } from "./memory-graph.js";
import { openCommit, openFile, refTarget } from "./memory-preview.js";

                                                        

/* ------------------------------------------------------------------ feedback verbs */

/** Mark a note; null when it took, else the sentence why not. */
export async function markNote(id        , verdict         , reason         )                         {
  const body                          = { project: S.project, verdict };
  if (reason) body.reason = reason;
  const r = await req                       ("POST", "/records/" + seg(id) + "/mark", body);
  return r.ok ? null : errMessage(r.err);
}

export async function confirmNote(id        )                         {
  const r = await req                       ("POST", "/records/" + seg(id) + "/confirm", { project: S.project });
  return r.ok ? null : errMessage(r.err);
}

/** A reason, asked on its own sheet: `run` sends it (a string back is the refusal, shown under the
 *  field); `after` runs once the sheet has closed - reopen the note, repaint the inbox. */
export function askReason(o                                                                                                                                       )       {
  openFieldSheet({
    title: o.title, sub: o.sub, label: tr("memory.reason"), placeholder: o.placeholder, save: o.save,
    submit: async (reason        )                            => {
      const err = await o.run(reason);
      if (err) return err;
      setTimeout(o.after, 0);
      return true;
    },
  });
}

/** Mark outdated or wrong: both carry a reason (the store refuses either without one). */
export function askMark(s                               , verdict                      , after            )       {
  const wrong = verdict === "wrong";
  askReason({
    title: wrong ? tr("memory.mark.wrongTitle") : tr("memory.mark.outdatedTitle"),
    sub: s.title,
    save: wrong ? tr("memory.mark.wrong") : tr("memory.mark.outdated"),
    placeholder: wrong ? tr("memory.mark.wrongHint") : tr("memory.mark.outdatedHint"),
    run: (reason) => markNote(s.id, verdict, reason),
    after: () => { toast(wrong ? tr("memory.mark.wrongDone") : tr("memory.mark.outdatedDone")); after(); },
  });
}

/** Delete for good: the note's own id, typed, is the confirmation (the server checks it too). */
export function askDelete(r                               )       {
  openFieldSheet({
    title: tr("memory.del.title"), sub: r.title, label: tr("memory.del.label", { id: r.id }), placeholder: r.id,
    save: tr("memory.del.save"), exact: true,
    submit: async (typed        )                            => {
      if (typed !== r.id) return tr("memory.del.mismatch");
      const x = await req                      ("DELETE", "/records/" + seg(r.id) + qs({ project: S.project, confirm: r.id }));
      if (!x.ok) return errMessage(x.err);
      toast(tr("memory.del.done", { id: r.id }));
      if (S.sel === r.id) S.sel = null;
      void hooks.reload();
      return true;
    },
  });
}

/** File notes by hand: a domain picked from the map (Unfiled takes them out). */
export async function askFile(ids          , label        , now = "")                {
  const items = await domainList();
  const pick = domainSelect({ id: "memFileTo" }, items, now || "none", [["none", tr("memory.dom.unfiled")]]);
  const err = hint("", { id: "memFileErr", bad: true, hidden: true, live: true });
  const cancel = btn(tr("ui.cancel"));
  const ok = btn(tr("memory.dom.file"), { kind: "primary", id: "memFileSave" });
  showSheet(sheet({
    title: tr("memory.dom.fileTitle"), sub: label,
    body: [field({ label: tr("memory.dom.fileTo"), control: pick }), err],
    foot: [h("span", { class: "grow" }), cancel, ok],
  }));
  cancel.onclick = closeSheet;
  ok.onclick = async () => {
    err.hidden = true;
    ok.disabled = true;
    const to = pick.value === "none" ? null : pick.value;
    const r = await req                ("POST", "/records/file", { project: S.project, ids, domain: to });
    ok.disabled = false;
    if (!r.ok || !r.data) { err.textContent = errMessage(r.err); err.hidden = false; return; }
    closeSheet();
    toast(r.data.domain ? tr("memory.dom.filed", { path: r.data.domain.path }) : tr("memory.dom.unfiledDone"));
    void hooks.reload();
  };
  pick.focus();
}

/** Whether the project has a map to file into: the global scope has none. */
function mapped()          {
  const p = currentProject();
  return !!p && !p.global;
}

/* ------------------------------------------------------------------ the link picker */

;                                                                              

/** Kind, then the target found by search, then an optional note. A pick calls `onPick`; the
 *  picker owns its own nodes and handlers (it lives in a sheet). */
function linkPicker(o                                                                  )              {
  const kindSel = h("select", { id: o.prefix + "Kind", aria: { label: tr("memory.linkPick.kind") } },
    LINKABLE.map((k) => h("option", { value: k }, linkLabel(k))));
  const q = h("input", {
    id: o.prefix + "Q", type: "search", autocomplete: "off",
    placeholder: tr("memory.linkPick.search"), aria: { label: tr("memory.linkPick.search") },
  });
  const noteIn = h("input", { id: o.prefix + "Note", autocomplete: "off", placeholder: tr("memory.linkPick.note"), aria: { label: tr("memory.linkPick.note") } });
  const results = h("div", { id: o.prefix + "Hits" });
  let seq = 0;
  let timer                                       = null;
  const search = async ()                => {
    const text = q.value.trim();
    const mine = ++seq;
    if (!text) { fill(results); return; }
    const r = await req                         ("GET", "/search" + qs({ project: S.project, q: text, limit: 8 }));
    if (mine !== seq) return;
    const items = r.ok && r.data ? r.data.items.filter((x) => x.id !== o.exclude) : [];
    if (!r.ok) { fill(results, hint(errMessage(r.err), { bad: true })); return; }
    if (!items.length) { fill(results, hint(tr("memory.linkPick.none"))); return; }
    fill(results, card(items.map((s) => row({
      name: h("button", { type: "button", data: { mpick: s.id }, title: s.title }, s.title),
      sub: [h("code", null, s.id), " · ", kindLabel(s.kind)],
      muted: !isLive(s.status),
    }))));
  };
  q.onkeydown = (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    if (timer) clearTimeout(timer);
    void search();
  };
  q.oninput = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => { void search(); }, 250);
  };
  const root = h("div", { data: { mpicker: o.prefix } },
    inlineForm(kindSel, q, noteIn), results);
  root.onclick = (e) => {
    const t = e.target instanceof Element ? e.target.closest             ("[data-mpick]") : null;
    if (!t) return;
    e.stopPropagation();
    const to = t.dataset.mpick || "";
    o.onPick({ to, title: t.title || to, kind: kindSel.value               , note: noteIn.value.trim() });
    q.value = "";
    noteIn.value = "";
    fill(results);
  };
  return root;
}

/* ------------------------------------------------------------------ revisions */

const yesNo = (v         )         => (v ? tr("memory.yes") : tr("memory.no"));

/* One snapshot field, as the diff compares it. */
const DIFF_FIELDS                                              = [
  [tk("memory.f.kind"), (s) => kindLabel(s.kind)],
  [tk("memory.f.title"), (s) => s.title],
  [tk("memory.f.status"), (s) => statusLabel(s.status)],
  [tk("memory.f.confirmed"), (s) => yesNo(s.confirmed)],
  [tk("memory.f.anchors"), (s) => s.anchors.join(", ")],
  [tk("memory.f.refs"), (s) => s.refs.join(", ")],
  [tk("memory.f.keywords"), (s) => (s.keywords || []).join(", ")],
];

/** What one revision changed against the one before it (or, for the first, what it wrote). */
function diffBody(rv             , prev                    )         {
  const a = prev ? prev.snapshot : null;
  const b = rv.snapshot;
  const rows                = [];
  const dash = "—";
  for (const [key, get] of DIFF_FIELDS) {
    const after = get(b);
    const before = a ? get(a) : null;
    if (before === after || (before === null && !after)) continue;
    rows.push(kvRow(tr(key), before === null
      ? after
      : [h("del", null, before || dash), " → ", h("ins", null, after || dash)]));
  }
  const out           = [rows.length ? card(rows) : null];
  if (!a || a.content !== b.content) {
    if (a) out.push(valueBlock({ label: tr("memory.diff.before"), text: a.content }));
    out.push(valueBlock({ label: a ? tr("memory.diff.after") : tr("memory.f.content"), text: b.content }));
  }
  if (!rows.length && a && a.content === b.content) out.push(hint(tr("memory.diff.none")));
  return out;
}

function historyNode(revisions               )              {
  const sorted = revisions.slice().sort((x, y) => y.revision - x.revision);
  const prevOf = (rv             )                     => sorted.find((x) => x.revision < rv.revision) || null;
  const items                 = sorted.map((rv) => ({
    id: "r" + rv.revision,
    at: rv.atMs,
    title: [tr("memory.revN", { n: rv.revision }), " ", opLabel(rv.op)],
    arg: rv.reason || undefined,
    who: rv.actor,
    data: { mrev: rv.revision },
  }));
  const body = (it              )         => {
    const rv = sorted.find((x) => "r" + x.revision === it.id);
    return rv ? diffBody(rv, prevOf(rv)) : null;
  };
  const node = timeline(items, { open: new Set(items.length ? [items[0].id] : []), body, showWho: true });
  node.onclick = (e) => {
    const sum = e.target instanceof Element ? e.target.closest(".tl-sum") : null;
    const item = sum ? sum.closest             (".tl-item") : null;
    if (!item) return;
    const id = item.dataset.tlId || "";
    const it = items.find((x) => x.id === id);
    timelineToggle(node, id, it ? body(it) : undefined);
  };
  return node;
}

/* ------------------------------------------------------------------ the note detail */

/** The note the detail pane shows, as last read: its menu and verbs act on this. */
let cur                   = null;
/** Bumped by every load: a late answer for a note no longer asked for paints nothing. */
let seq = 0;

/** The neighbourhood a note's own graph asks for: its direct links, history included. */
const EGO_LIMIT = 60;

export function currentNote()                   {
  return cur;
}

/** Nothing selected: what the pane is for. */
export function paintDetailEmpty(host             )       {
  cur = null;
  seq++;
  fill(host, emptyNode({ icon: "brain", title: tr("memory.detail.none"), hint: tr("memory.detail.pick") }));
}

/** Read a note whole - the record with its revisions, its neighbourhood, its associations - and
 *  show it in `host`. The note shows as soon as it and its neighbourhood are read; its
 *  associations, the slowest of the three, fill their section when they arrive. A reload of
 *  the note already shown waits for all three, so the pane neither flickers nor loses its
 *  scroll. */
export async function loadDetail(host             , id        )                {
  const mine = ++seq;
  const project = S.project;
  // The note on screen, not merely the last one read: a pane mounted afresh shows none yet.
  const same = !!cur && cur.id === id && host.querySelector             (":scope > article[data-mdoc]")?.dataset.mdoc === id;
  if (!same) fill(host, note(tr("memory.loading"), { busy: true }));
  const relating = req                 ("POST", "/related", { project, id, limit: 8 });
  const [one, near] = await Promise.all([
    req                                                  ("GET", "/records/" + seg(id) + qs({ project, history: true })),
    req            ("GET", "/graph" + qs({ project, center: id, depth: 1, history: true, limit: EGO_LIMIT })),
  ]);
  const rel = same ? await relating : null;
  const gone = ()          => mine !== seq || project !== S.project || !host.isConnected;
  if (gone()) return;
  if (!one.ok || !one.data) {
    cur = null;
    fill(host, note(errMessage(one.err), { err: true }));
    return;
  }
  const keep = same ? host.scrollTop : 0;
  const r = one.data.record;
  cur = r;
  fill(host, detailNode(r, one.data.revisions || [], rel, near.ok && near.data ? near.data : null));
  host.scrollTop = keep;
  if (rel) return;
  const late = await relating;
  const slot = host.querySelector             ("#memRelated");
  if (gone() || !slot) return;
  fill(slot, relatedBody(r, late));
}

/** The associations section's body: the rows, none, why not, or (`rel` null) still coming. */
function relatedBody(r           , rel                               )         {
  if (!rel) return note(tr("memory.related.loading"), { busy: true });
  if (!rel.ok || !rel.data) return hint(errMessage(rel.err), { bad: true });
  return rel.data.items.length ? relatedRows(r, rel.data.items) : hint(tr("memory.related.none"));
}

function anchorTag(state        )              {
  const warn = state === "changed" || state === "missing";
  return tag(anchorLabel(state), warn ? { tone: "warn" } : {});
}

/** Links grouped by kind and direction, each group a caption over its card - "→ supports 2" -
 *  read from this note's side; a symmetric kind is one group whichever end wrote it. A
 *  contradiction is a conflict to settle, so its caption is the red tag. */
function linkGroups(r           )           {
  const out           = [];
  for (const kind of LINK_KINDS) {
    const symmetric = kind === "contradicts" || kind === "related_to";
    const dirs                               = symmetric ? ["both"] : ["out", "in"];
    for (const dir of dirs) {
      const links = r.links.filter((l) => l.kind === kind && (dir === "both" || l.dir === dir));
      if (!links.length) continue;
      const word = dir === "in" ? linkInLabel(kind) : linkLabel(kind);
      const arrow = dir === "out" ? "→ " : dir === "in" ? "← " : "↔ ";
      const cap = formCap([arrow, kind === "contradicts" ? tag(word, { tone: "bad" }) : word, " · " + String(links.length)]);
      cap.dataset.mlgroup = kind + ":" + dir;
      out.push(cap, card(links.map((l) => linkRow(r.id, l))));
    }
  }
  return out;
}

function linkRow(self        , l               )              {
  const from = l.dir === "out" ? self : l.id;
  const to = l.dir === "out" ? l.id : self;
  // A supersede's link is the note's history: shown, never removed by hand.
  const removable = l.kind !== "supersedes";
  // A purged neighbour has no status and nothing to open; its link can still be removed.
  const purged = l.status === null;
  return row({
    name: purged ? h("span", null, tr("memory.link.purged")) : [noteButton({ id: l.id, title: l.title }), " ", l.status ? statusTag(l.status) : null],
    sub: [h("code", null, l.id), l.note ? [" · ", l.note] : null],
    muted: !l.status || !isLive(l.status),
    more: removable
      ? iconBtn("x", tr("memory.link.remove"), { ghost: true, data: { mlinkdel: l.kind, mlfrom: from, mlto: to } })
      : undefined,
  });
}

/** Associations: notes that share words, files or links with this one. One not linked yet can be
 *  linked from here in a click (as related to). */
function relatedRows(r           , items              )              {
  const linked = new Set(r.links.map((l) => l.id));
  const live = isLive(r.status);
  return card(items.map((x) => row({
    lead: kindChip(x.kind, { bare: true }),
    name: noteButton(x),
    sub: reasonsNode(x.reasons),
    cols: [{ v: tr("memory.scorePct", { n: Math.round(x.score) }), w: "s", title: tr("memory.scoreTitle") }],
    primary: live && !linked.has(x.id) && isLive(x.status)
      ? btn(tr("memory.rel.link"), { data: { mrellink: x.id }, title: tr("memory.rel.linkTitle") })
      : undefined,
    muted: !isLive(x.status),
  })));
}

/** The note's own graph: it in the middle, every note it links to around it, each link labelled
 *  with its kind. A click on a neighbour walks there. */
function egoGraph(r           , g            )                     {
  if (g.nodes.length < 2) return null;
  const v = graphView({
    nodes: g.nodes.map(nodeOf),
    edges: g.edges.map(edgeOf),
    center: r.id,
    selected: r.id,
    label: tr("memory.graph.labelAround", { id: r.id }),
    compact: true,
  }, {
    engine: graphEngine,
    onSelect: (id) => { if (id && id !== r.id) void hooks.openNote(id); },
  });
  v.el.id = "memEgo";
  return v.el;
}

/** The values under the title: the id, who wrote it, when it last moved, its revision. */
function metaLine(r           )           {
  return [
    h("code", null, r.id),
    mapped()
      ? r.domain
        ? refLink(r.domain, { code: true, title: tr("memory.dom.openTitle", { path: r.domain }), data: { mdomopen: r.domain } })
        : h("span", { data: { munfiled: "" } }, tr("memory.dom.unfiled"))
      : null,
    h("span", { title: r.actor, data: { mprov: r.producer + "/" + r.transport } },
      tr("memory.provenance", { producer: producerLabel(r.producer), transport: transportLabel(r.transport) })),
    h("span", { title: tr("memory.detail.createdAt", { when: fullTime(r.createdMs) }) }, tr("memory.detail.updated", { when: relTime(r.updatedMs) })),
    r.verifiedMs ? h("span", { title: fullTime(r.verifiedMs) }, tr("memory.detail.verified", { when: relTime(r.verifiedMs) })) : null,
    tr("memory.revN", { n: r.revision }),
  ];
}

function detailNode(r           , revisions               , related                               , near                   )              {
  const live = isLive(r.status);
  const acts           = live
    ? [
        btn(tr("memory.edit"), { kind: "primary", data: { mact: "edit" } }),
        btn(tr("memory.mark.verified"), { data: { mact: "verified" }, title: tr("memory.mark.verifiedWhy") }),
        r.confirmed ? null : btn(tr("memory.confirm"), { data: { mact: "confirm" }, title: tr("memory.confirmWhy") }),
      ]
    : [
        btn(tr("memory.restoreAsk"), { kind: "primary", data: { mact: "restore" } }),
        r.confirmed ? null : btn(tr("memory.confirm"), { data: { mact: "confirm" }, title: tr("memory.confirmWhy") }),
      ];
  acts.push(moreBtn(tr("memory.detail.more", { title: r.title }), { data: { mact: "more" } }));
  const ego = near ? egoGraph(r, near) : null;
  const links = linkGroups(r);
  const picker = live ? linkPicker({ prefix: "memLink", exclude: r.id, onPick: (p) => { void addLink(r, p); } }) : null;
  return h("article", { data: { mdoc: r.id } },
    docHead({
      tags: [kindChip(r.kind), statusTag(r.status), markerTags(r.markers),
        r.sourceVerified ? tag(tr("memory.sourceVerified"), { title: tr("memory.sourceVerifiedWhy") }) : null],
      title: r.title, meta: metaLine(r), actions: acts,
    }),
    h("div", { id: "memNoteErr" }),
    live ? null : note([tr("memory.detail.history", { status: statusLabel(r.status) }),
      r.supersededBy ? [" ", tr("memory.f.supersededBy"), " ", noteButton({ id: r.supersededBy, title: r.supersededBy })] : null]),
    markdownNode(r.content, { class: "note-body" }),
    section({
      cap: tr("memory.f.relations"),
      tools: [btn(tr("memory.showInGraph"), { kind: "ghost", data: { mact: "graph" } })],
    },
      ego ? card(ego) : null,
      links.length ? links : hint(tr("memory.links.none")),
      picker ? formFold({ summary: tr("memory.link.add"), id: "memLinkFold" }, picker) : null),
    section({ cap: tr("memory.f.related"), note: tr("memory.related.note") },
      h("div", { id: "memRelated" }, relatedBody(r, related))),
    r.anchors.length
      ? section({ cap: tr("memory.f.anchors") }, card(r.anchors.map((a) => row({
          name: refLink(a.path, {
            code: true, title: tr(a.glob ? "memory.pv.openGlob" : "memory.pv.openAnchor", { path: a.path }),
            data: { mpvfile: a.path, mpvglob: a.glob ? "" : null, mpvtab: a.state === "changed" ? "history" : null },
          }),
          cols: [a.glob ? tag(tr("memory.anchor.glob")) : null, anchorTag(a.state)],
          primary: a.glob ? undefined : iconBtn("history", tr("memory.pv.historyOf", { path: a.path }), {
            ghost: true, data: { mpvfile: a.path, mpvtab: "history" },
          }),
        }))))
      : null,
    r.refs.length ? section({ cap: tr("memory.f.refs") }, card(r.refs.map((ref) => row({ name: refNode(ref) })))) : null,
    r.keywords && r.keywords.length ? section({ cap: tr("memory.f.keywords") }, keywordChips(r.keywords)) : null,
    r.evidence && r.evidence.length
      ? section({ cap: tr("memory.f.evidence"), note: tr("memory.ev.note") }, card(r.evidence.map(evidenceRow)))
      : null,
    revisions.length
      ? section({ cap: tr("memory.f.history") },
          formFold({ summary: trn(revisions.length, "memory.revisions.one", "memory.revisions.other"), id: "memHistFold" }, historyNode(revisions)))
      : null);
}

/** One piece of evidence: what it is and how it bears on the note, its source opening the preview
 *  where it can; one attached to earlier text is kept but greyed. */
function evidenceRow(e             )              {
  const where = e.symbol ? "#" + e.symbol : "";
  const source = e.path
    ? refLink(e.path + where, { code: true, title: tr("memory.pv.openAnchor", { path: e.path }), data: { mpvfile: e.path } })
    : e.commitSha
      ? refLink(e.commitSha.slice(0, 12), { code: true, title: tr("memory.pv.openCommit", { rev: e.commitSha }), data: { mpvcommit: e.commitSha } })
      : e.sessionId || e.runId
        ? h("code", null, (e.sessionId || e.runId) + where)
        : null;
  return row({
    name: [sourceLabel(e.sourceType), source ? [" ", source] : null],
    sub: [relationLabel(e.relation), " · ", tr("memory.revN", { n: e.revision }), " · ", e.actor],
    cols: [e.current ? null : tag(tr("memory.ev.earlier"), { title: tr("memory.ev.earlierWhy") })],
    muted: !e.current,
    data: { mev: e.id },
  });
}

/** A ref as what it points at: a commit or a file opens its preview, a URL opens in a new tab,
 *  anything else is text. */
function refNode(ref        )              {
  const to = refTarget(ref);
  if (!to) return h("code", null, ref);
  if ("url" in to) return h("a", { href: to.url, target: "_blank", rel: "noopener noreferrer" }, ref);
  if ("commit" in to) return refLink(ref, { code: true, title: tr("memory.pv.openCommit", { rev: to.commit }), data: { mpvcommit: to.commit } });
  return refLink(ref, { code: true, title: tr("memory.pv.openAnchor", { path: to.path }), data: { mpvfile: to.path, mpvfind: to.find ?? null } });
}

/** When the note was last saved or verified: the commits after it are the ones to read. */
function baselineOf(r           )         {
  return Math.max(r.updatedMs, r.verifiedMs || 0);
}

function noteErr(text        )       {
  const host = document.getElementById("memNoteErr");
  if (host) fill(host, hint(text, { bad: true, live: true }));
}

/** The detail's ⋯: the verbs that ask for a reason, and the ones that end the note. */
function detailMenu(anchor             , r           )       {
  const live = isLive(r.status);
  const after = ()       => { void hooks.reload(); };
  const items             = live
    ? [
        { label: tr("memory.supersedeAsk"), fn: () => { openEditor({ replaces: r }); } },
        { label: tr("memory.mark.outdatedAsk"), fn: () => { askMark(r, "outdated", after); } },
        { label: tr("memory.mark.wrongAsk"), fn: () => { askMark(r, "wrong", after); } },
        { sep: true },
      ]
    : [];
  if (mapped()) items.push({ label: tr("memory.dom.moveNote"), fn: () => { void askFile([r.id], r.title, r.domain || ""); } });
  items.push(
    { label: tr("memory.showInGraph"), fn: () => { showInGraph(r.id); } },
    { sep: true },
    { label: tr("memory.del.ask"), fn: () => { askDelete(r); }, danger: true },
  );
  popupMenu(anchor.getBoundingClientRect(), items);
}

function showInGraph(id        )       {
  S.graph.center = id;
  S.graph.selected = id;
  hooks.showTab("graph");
}

/** A click inside the detail pane; true when it was the detail's own. Opening a note ([data-mopen])
 *  is the page's, so a link walks the same way a list row does. */
export async function onDetailClick(ev            , t         )                   {
  const r = cur;
  if (!r) return false;
  const del = t.closest             ("[data-mlinkdel]");
  if (del) {
    const x = await req                      ("DELETE", "/links" + qs({
      project: S.project, from: del.dataset.mlfrom, to: del.dataset.mlto, kind: del.dataset.mlinkdel,
    }));
    if (!x.ok) { noteErr(errMessage(x.err)); return true; }
    toast(tr("memory.link.removed"));
    await hooks.reload();
    return true;
  }
  const file = t.closest             ("[data-mpvfile]");
  if (file) {
    openFile({
      path: file.dataset.mpvfile || "",
      glob: file.dataset.mpvglob != null,
      tab: file.dataset.mpvtab === "history" ? "history" : "file",
      find: file.dataset.mpvfind || undefined,
      since: baselineOf(r),
    });
    return true;
  }
  const commit = t.closest             ("[data-mpvcommit]");
  if (commit) { openCommit(commit.dataset.mpvcommit || ""); return true; }
  const dom = t.closest             ("[data-mdomopen]");
  if (dom) { hooks.openDomain(dom.dataset.mdomopen || ""); return true; }
  const quick = t.closest             ("[data-mrellink]");
  if (quick) {
    await addLink(r, { to: quick.dataset.mrellink || "", title: "", kind: "related_to", note: "" });
    return true;
  }
  const a = t.closest             ("[data-mact]");
  if (!a) return false;
  switch (a.dataset.mact) {
    case "more":
      // The opening click must not reach document (the menu closes on outside clicks).
      ev.stopPropagation();
      if (menuOpen()) { closeMenu(); return true; }
      detailMenu(a, r);
      return true;
    case "edit": openEditor({ record: r }); return true;
    case "graph": showInGraph(r.id); return true;
    case "verified": {
      const err = await markNote(r.id, "verified");
      if (err) { noteErr(err); return true; }
      toast(tr("memory.mark.verifiedDone"));
      await hooks.reload();
      return true;
    }
    case "confirm": {
      const err = await confirmNote(r.id);
      if (err) { noteErr(err); return true; }
      toast(tr("memory.confirmDone"));
      await hooks.reload();
      return true;
    }
    case "restore":
      askReason({
        title: tr("memory.restoreTitle"), sub: r.title, save: tr("memory.restore"), placeholder: tr("memory.restoreHint"),
        run: async (reason) => {
          const x = await req                       ("POST", "/records/" + seg(r.id) + "/restore", { project: S.project, reason });
          return x.ok ? null : errMessage(x.err);
        },
        after: () => { toast(tr("memory.restoreDone")); void hooks.reload(); },
      });
      return true;
    default:
      return false;
  }
}

async function addLink(r           , p        )                {
  const body                          = { project: S.project, from: r.id, to: p.to, kind: p.kind };
  if (p.note) body.note = p.note;
  const x = await req                      ("POST", "/links", body);
  if (!x.ok || !x.data) { noteErr(errMessage(x.err)); return; }
  toast(x.data.changed ? tr("memory.link.added") : tr("memory.link.exists"));
  await hooks.reload();
}

/* ------------------------------------------------------------------ the editor */

;                                                                                     
;                    
                                                                                                                 
                                                                                                                                 
 

/** The formation hints a save answers with (SPEC ADR-032): advisory, the note is saved either way. */
const FORMATION_RULE                         = {
  sections: tk("memory.formation.sections"),
  long: tk("memory.formation.long"),
  call_chain: tk("memory.formation.call_chain"),
  code: tk("memory.formation.code"),
  global: tk("memory.formation.global"),
  restates_code: tk("memory.formation.restates_code"),
};

function formationText(f                    )         {
  const rules = f.map((x) => (FORMATION_RULE[x.rule] ? tr(FORMATION_RULE[x.rule]) : x.rule));
  return tr("memory.ed.savedFormation", { rules: rules.join(", ") });
}

/** What a save's code check did not find in the checkout (SPEC §memory.code). A panel save is a
 *  person's, so it is never held; "elsewhere" only says where a name lives and is left out. */
function missingCode(code                   )           {
  return code?.findings?.filter((f) => f.rule !== "elsewhere").map((f) => f.name) ?? [];
}

/** New note, edit, or supersede (a new note with `replaces`, prefilled from the old one). A new
 *  note is filed in `domain` when given, else by its anchors; an edit can re-file it. */
export function openEditor(o                                                               )       {
  const rec = o.record || null;
  const src = o.record || o.replaces || null;
  // "" on a new note: the store files it by its anchors (or the replaced note's domain).
  const lead                          = rec
    ? [["none", tr("memory.dom.unfiled")]]
    : [["", tr("memory.dom.byAnchors")], ["none", tr("memory.dom.unfiled")]];
  const startDomain = rec ? rec.domain || "none" : o.domain || "";
  const domSel = mapped() ? domainSelect({ id: "memEdDomain" }, S.domains || [], startDomain, lead) : null;
  if (domSel) {
    let touched = false;
    domSel.onchange = () => { touched = true; };
    void domainList().then((items) => { if (!touched) domainFill(domSel, items, startDomain, lead); });
  }
  const kindSel = h("select", { id: "memEdKind" },
    KINDS.map((k) => h("option", { value: k, selected: (src ? src.kind : "knowledge") === k }, kindLabel(k))));
  const title = h("input", { id: "memEdTitle", autocomplete: "off", value: src ? src.title : "", placeholder: tr("memory.ed.titleHint") });
  const content = h("textarea", { id: "memEdContent", rows: 7, placeholder: tr("memory.ed.contentHint") });
  content.value = src ? src.content : "";
  const anchors = h("textarea", { id: "memEdAnchors", rows: 2, placeholder: tr("memory.ed.anchorsHint") });
  anchors.value = src ? src.anchors.map((a) => a.path).join("\n") : "";
  const refs = h("textarea", { id: "memEdRefs", rows: 2, placeholder: tr("memory.ed.refsHint") });
  refs.value = src ? src.refs.join("\n") : "";
  const keywords = h("input", {
    id: "memEdKeywords", autocomplete: "off", value: src ? (src.keywords || []).join(", ") : "", placeholder: tr("memory.ed.keywordsHint"),
  });
  const reason = h("input", { id: "memEdReason", autocomplete: "off", placeholder: tr("memory.ed.reasonHint") });
  const err = hint("", { id: "memEdErr", bad: true, hidden: true, live: true });
  const conflict = h("div", { id: "memEdConflict" });
  const pending           = [];
  const chips = h("div", { id: "memEdLinks" });
  const paintChips = ()       => {
    fill(chips, pending.length ? card(pending.map((p, i) => row({
      name: [linkLabel(p.kind), " → ", p.title],
      sub: [h("code", null, p.to), p.note ? [" · ", p.note] : null],
      more: iconBtn("x", tr("memory.link.remove"), { ghost: true, data: { mlinkdrop: i } }),
    }))) : null);
  };
  chips.onclick = (e) => {
    const t = e.target instanceof Element ? e.target.closest             ("[data-mlinkdrop]") : null;
    if (!t) return;
    pending.splice(Number(t.dataset.mlinkdrop), 1);
    paintChips();
  };
  const picker = rec ? null : linkPicker({
    prefix: "memEdLink", exclude: o.replaces ? o.replaces.id : "",
    onPick: (p) => {
      if (!pending.some((x) => x.to === p.to && x.kind === p.kind)) pending.push(p);
      paintChips();
    },
  });
  const cancel = btn(tr("ui.cancel"));
  const save = btn(rec ? tr("memory.ed.save") : o.replaces ? tr("memory.ed.supersede") : tr("memory.ed.create"), { kind: "primary", id: "memEdSave" });
  const sheetTitle = rec ? tr("memory.ed.editTitle") : o.replaces ? tr("memory.ed.supersedeTitle") : tr("memory.ed.newTitle");
  showSheet(sheet({
    title: sheetTitle,
    sub: rec ? rec.id : o.replaces ? o.replaces.id : undefined,
    body: [
      field({ label: tr("memory.f.kind"), control: kindSel }),
      field({ label: tr("memory.f.title"), control: title, required: true }),
      field({ label: tr("memory.f.content"), control: content, required: true, hint: tr("memory.ed.contentWhy") }),
      field({ label: tr("memory.f.anchors"), control: anchors, hint: tr("memory.ed.anchorsWhy") }),
      field({ label: tr("memory.f.refs"), control: refs, hint: tr("memory.ed.refsWhy") }),
      field({ label: tr("memory.f.keywords"), control: keywords, hint: tr("memory.ed.keywordsWhy") }),
      domSel ? field({ label: tr("memory.dom.f.domain"), control: domSel, hint: rec ? undefined : tr("memory.dom.byAnchorsWhy") }) : null,
      picker ? [formCap(tr("memory.ed.links")), chips, picker] : null,
      rec ? field({ label: tr("memory.reason"), control: reason }) : null,
      err,
      conflict,
    ],
    foot: [h("span", { class: "grow" }), cancel, save],
  }));
  const fail = (text        )       => { err.textContent = text; err.hidden = false; };
  const send = async (extra                                               )                => {
    err.hidden = true;
    fill(conflict);
    const t = title.value.trim();
    const c = content.value.trim();
    if (!t || !c) { fail(tr("memory.ed.required")); return; }
    save.disabled = true;
    try {
      const domain = domSel ? domSel.value : "";
      const kw = keywordList(keywords.value);
      if (rec) await sendEdit(rec, { kind: kindSel.value           , title: t, content: content.value, anchors: lines(anchors.value), refs: lines(refs.value), keywords: kw, domain, reason: reason.value.trim() }, fail, conflict);
      else await sendNew({
        kind: kindSel.value           , title: t, content: content.value, anchors: lines(anchors.value), refs: lines(refs.value), keywords: kw, domain,
        links: pending, replaces: extra.replaces ?? (o.replaces ? o.replaces.id : undefined), allowSimilar: extra.allowSimilar,
      }, fail, conflict, send);
    } finally {
      save.disabled = false;
    }
  };
  cancel.onclick = closeSheet;
  save.onclick = () => { void send({}); };
  title.focus();
}

/** `domain`: a path, "none" unfiled, "" (a new note) by its anchors. */
;                                                                                                                                       

function sensitiveText(rule                    )         {
  return tr("memory.ed.sensitive", { rule: rule || "?" });
}

async function sendNew(
  d                                                                        ,
  fail                     , conflict             ,
  again                                                                         ,
)                {
  const body                          = { project: S.project, kind: d.kind, title: d.title, content: d.content };
  if (d.anchors.length) body.anchors = d.anchors;
  if (d.refs.length) body.refs = d.refs;
  if (d.keywords.length) body.keywords = d.keywords;
  if (d.links.length) body.links = d.links.map((l) => (l.note ? { to: l.to, kind: l.kind, note: l.note } : { to: l.to, kind: l.kind }));
  if (d.replaces) body.replaces = d.replaces;
  if (d.allowSimilar) body.allowSimilar = true;
  if (d.domain) body.domain = d.domain;
  const r = await req           ("POST", "/records", body);
  if (r.ok && r.data) {
    const saved = r.data;
    closeSheet();
    const similar = saved.similar ? saved.similar.length : 0;
    const formation = saved.formation || [];
    const missing = missingCode(saved.code);
    toast(saved.duplicate ? tr("memory.ed.duplicate")
      : missing.length ? tr("memory.ed.savedCode", { names: missing.join(", ") })
      : formation.length ? formationText(formation)
      : similar ? trn(similar, "memory.ed.savedSimilar.one", "memory.ed.savedSimilar.other") : tr("memory.ed.saved"));
    S.sel = saved.record.id;
    void hooks.reload();
    return;
  }
  const e = r.err;
  if (e && e.error === "memory_similar_exists" && e.similar) {
    const sim = e.similar;
    const sup = btn(tr("memory.ed.supersedeIt"), { data: { medsup: sim.id } });
    const anyway = btn(tr("memory.ed.saveAnyway"), { data: { medanyway: "" } });
    sup.onclick = () => { void again({ replaces: sim.id }); };
    anyway.onclick = () => { void again({ allowSimilar: true }); };
    fill(conflict, hint(tr("memory.ed.similar", { title: sim.title, id: sim.id }), { bad: true, live: true }), inlineForm(sup, anyway));
    return;
  }
  if (e && e.error === "memory_sensitive_content") { fail(sensitiveText(e.rule)); return; }
  fail(errMessage(e));
}

async function sendEdit(rec           , d                            , fail                     , conflict             )                {
  const body                          = { project: S.project, expectedRevision: rec.revision };
  // Only the fields that changed travel; when none did there is nothing to send.
  const fixed = Object.keys(body).length;
  if (d.kind !== rec.kind) body.kind = d.kind;
  if (d.title !== rec.title) body.title = d.title;
  if (d.content !== rec.content) body.content = d.content;
  if (d.anchors.join("\n") !== rec.anchors.map((a) => a.path).join("\n")) body.anchors = d.anchors;
  if (d.refs.join("\n") !== rec.refs.join("\n")) body.refs = d.refs;
  if (d.keywords.join("\n") !== (rec.keywords || []).join("\n")) body.keywords = d.keywords;
  if (d.domain && d.domain !== (rec.domain || "none")) body.domain = d.domain;
  if (Object.keys(body).length === fixed) { closeSheet(); return; }
  if (d.reason) body.reason = d.reason;
  const r = await req                       ("PUT", "/records/" + seg(rec.id), body);
  if (r.ok) {
    closeSheet();
    toast(tr("memory.ed.saved"));
    void hooks.reload();
    return;
  }
  const e = r.err;
  if (e && e.error === "memory_revision_conflict") {
    const reload = btn(tr("memory.ed.reload"), { data: { medreload: "" } });
    reload.onclick = () => { void reloadEditor(rec.id); };
    fill(conflict, hint(tr("memory.ed.conflict", { n: e.current ?? "?", mine: rec.revision }), { bad: true, live: true }), inlineForm(reload));
    return;
  }
  if (e && e.error === "memory_sensitive_content") { fail(sensitiveText(e.rule)); return; }
  fail(errMessage(e));
}

/** A moved revision's way back: the editor again, on what the store holds now. */
async function reloadEditor(id        )                {
  const r = await req                       ("GET", "/records/" + seg(id) + qs({ project: S.project }));
  if (!r.ok || !r.data) { toast(errMessage(r.err), true); return; }
  openEditor({ record: r.data.record });
}

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
   Memory page - the Graph tab (SPEC §memory 11.2): the project's notes and their links, or the
   neighbourhood of one note (center + depth 1-3), drawn by the library's graphView on the vendored
   engine. Kinds are tones, history is dimmed, an unconfirmed note is dashed; every link carries
   its kind as a label, the directed ones an arrowhead, a contradiction in red. The whole project
   draws each domain as a named box round its notes, a sub-domain inside its parent's, Unfiled
   round the rest: the first look reads the map, a note's name shows on hover or selection.

   The tab is a workspace: the toolbar on top, the drawing filling the rest, and an inspector at
   its right that reads the selected note out - its kind and state, and every link it has, each
   one a click away from the note at its other end. A click selects in place (no new layout); a
   double click, or Open, shows the note on the Notes tab. At most GRAPH_LIMIT nodes are asked for;
   a capped answer says so.
   ================================================================================================ */
import type { HChild } from "../h.js";
import { fill, h } from "../h.js";
import { tr, trn } from "../i18n.js";
import { btn, card, checkField, docHead, emptyNode, graphView, hint, note, refLink, row, section, seg, tag, wsBar, wsScroll, wsSplit } from "../ui/index.js";
import type { GraphEdge, GraphGroup, GraphHandle, GraphNode } from "../ui/index.js";
import type { MemDomain, MemEdge, MemNode } from "../types/memory.js";
import { graphEngine } from "../graph-engine.js";
import {
  S, UNFILED, domainList, errMessage, graphKeys, hooks, isLive, kindChip, kindLabel, kindTone, linkInLabel, linkLabel, linkStyle,
  markerTags, qs, req, statusLabel, statusTag,
} from "./memory-core.js";

export const GRAPH_LIMIT = 300;

export interface GraphReply { nodes: MemNode[]; edges: MemEdge[]; truncated: boolean }

/** The last answer and the drawing made of it: a selection repaints the inspector from here. */
let last: GraphReply | null = null;
let view: GraphHandle | null = null;

export function nodeOf(n: MemNode): GraphNode {
  return {
    id: n.id,
    label: n.title,
    tone: kindTone(n.kind),
    dim: !isLive(n.status),
    pending: !n.confirmed,
    title: tr("memory.graph.nodeTitle", { title: n.title, kind: kindLabel(n.kind), status: statusLabel(n.status) }),
    hop: n.hop ?? undefined,
  };
}

export function edgeOf(e: MemEdge): GraphEdge {
  return {
    from: e.from, to: e.to, style: linkStyle(e.kind), directed: e.directed, alert: e.kind === "contradicts",
    label: linkLabel(e.kind),
    title: e.note ? tr("memory.graph.edgeNote", { kind: linkLabel(e.kind), note: e.note }) : linkLabel(e.kind),
  };
}

/** The whole graph's boxes: each domain round its notes, a sub-domain inside its parent, Unfiled
 *  round the notes no domain holds. None when no drawn note is filed - one box of everything says
 *  nothing. `group` maps a node to its box. */
export function domainGroups(nodes: MemNode[], domains: MemDomain[]): { groups: GraphGroup[]; group: (n: MemNode) => string } {
  const byPath = new Map(domains.map((d) => [d.path, d]));
  const group = (n: MemNode): string => (n.domain && byPath.has(n.domain) ? n.domain : UNFILED);
  if (!nodes.some((n) => group(n) !== UNFILED)) return { groups: [], group };
  const groups: GraphGroup[] = domains.map((d) => ({
    id: d.path,
    label: d.title || d.path,
    title: d.summary ? d.path + " - " + d.summary : d.path,
    parent: d.parent && byPath.has(d.parent) ? d.parent : undefined,
  }));
  groups.push({ id: UNFILED, label: tr("memory.dom.unfiled") });
  return { groups, group };
}

/** The toolbar: whole project or around the centre, the depth, history. */
function controls(): HTMLElement {
  const g = S.graph;
  const around = g.center ? tr("memory.graph.around", { id: g.center }) : tr("memory.graph.aroundNone");
  const scope = seg([
    { id: "whole", label: tr("memory.graph.whole") },
    { id: "center", label: around, hidden: !g.center },
  ], g.center ? "center" : "whole", { key: "mgscope", label: tr("memory.graph.scope") });
  const depth = g.center
    ? seg([1, 2, 3].map((d) => ({ id: String(d), label: tr("memory.graph.depthN", { n: d }) })), String(g.depth),
        { key: "mgdepth", label: tr("memory.graph.depth") })
    : null;
  const hist = checkField({ label: tr("memory.history"), control: h("input", { type: "checkbox", id: "memGraphHist", checked: g.history }) as HTMLInputElement });
  return wsBar({}, scope, depth, hist, h("span", { class: "grow" }), h("span", { id: "memGraphCount" }));
}

/** One link of the selected note, read from its side: the note at the other end (a click selects
 *  it), and under it the kind - inverted when the link points in - with the link's own note. */
function linkLine(id: string, e: MemEdge, byId: Map<string, MemNode>): HTMLElement {
  const out = e.from === id;
  const other = out ? e.to : e.from;
  const n = byId.get(other);
  const word = out || !e.directed ? linkLabel(e.kind) : linkInLabel(e.kind);
  const arrow = e.directed ? (out ? "→ " : "← ") : "↔ ";
  return row({
    lead: n ? kindChip(n.kind, { bare: true }) : null,
    name: refLink(n ? n.title : other, { data: { mgsel: other } }),
    sub: [arrow, e.kind === "contradicts" ? tag(word, { tone: "bad" }) : word, e.note ? " · " + e.note : null],
    muted: !!n && !isLive(n.status),
  });
}

/** The inspector: what the selected note is, the two ways on, and every link it has. */
function inspector(): HChild {
  const id = S.graph.selected;
  const n = id && last ? last.nodes.find((x) => x.id === id) : null;
  if (!n || !last) return hint(tr("memory.graph.pick"));
  const byId = new Map(last.nodes.map((x) => [x.id, x]));
  const edges = last.edges.filter((e) => e.from === n.id || e.to === n.id);
  return [
    docHead({
      tags: [kindChip(n.kind), statusTag(n.status), n.confirmed ? null : markerTags(["unconfirmed"])],
      title: n.title,
      meta: [h("code", null, n.id)],
      actions: [
        btn(tr("memory.graph.open"), { kind: "primary", data: { mgopen: n.id } }),
        n.id === S.graph.center ? null : btn(tr("memory.graph.centerHere"), { data: { mgcenter: n.id } }),
      ],
    }),
    section({ cap: trn(edges.length, "memory.links.one", "memory.links.other") },
      edges.length ? card(edges.map((e) => linkLine(n.id, e, byId))) : hint(tr("memory.links.none"))),
  ];
}

export function paintInspector(): void {
  const host = document.getElementById("memGraphSel");
  if (host) fill(host, inspector());
}

/** Select a node in place: the drawing keeps its layout, only the highlight and the inspector move. */
export function selectNode(id: string | null): void {
  S.graph.selected = id;
  if (view) view.select(id);
  paintInspector();
}

/** Load and paint the tab in place of `slot` (the page's body): the toolbar over the drawing, the
 *  inspector beside it. The split takes the slot's id, so the next tab replaces it the same way. */
export async function paintGraph(slot: HTMLElement): Promise<void> {
  const visit = S.visit;
  const project = S.project;
  const g = S.graph;
  if (view) { view.destroy(); view = null; }
  const stage = wsScroll({ pad: true }, note(tr("memory.loading"), { busy: true }));
  const side = wsScroll({ pad: true, id: "memGraphSel" });
  side.setAttribute("aria-live", "polite");
  const host = wsSplit({
    id: slot.id, fixed: "end", start: [controls(), stage], end: side,
    startLabel: tr("memory.tab.graph"), endLabel: tr("memory.graph.inspector"),
  });
  slot.replaceWith(host);
  const [r, domains] = await Promise.all([
    req<GraphReply>("GET", "/graph" + qs({ project, center: g.center, depth: g.center ? g.depth : null, history: g.history, limit: GRAPH_LIMIT })),
    g.center ? Promise.resolve([]) : domainList(),
  ]);
  if (visit !== S.visit || project !== S.project || !host.isConnected) return;
  if (!r.ok || !r.data) {
    last = null;
    fill(stage, note(errMessage(r.err), { err: true }));
    return;
  }
  last = r.data;
  const data = r.data;
  if (S.graph.selected && !data.nodes.some((n) => n.id === S.graph.selected)) S.graph.selected = null;
  const count = document.getElementById("memGraphCount");
  if (count) {
    fill(count, data.truncated
      ? h("span", { data: { mgtrunc: "" } }, tr("memory.graph.truncated", { n: data.nodes.length }))
      : [trn(data.nodes.length, "memory.nNotes.one", "memory.nNotes.other"), " · ", trn(data.edges.length, "memory.links.one", "memory.links.other")]);
  }
  if (!data.nodes.length) {
    fill(stage, emptyNode({ icon: "brain", title: tr("memory.graph.empty"), hint: tr("memory.graph.emptyHint") }));
    paintInspector();
    return;
  }
  // Most connected first: the layout seeds them in the middle. Ties by id, so the order - and
  // with it the picture - is the same on every load.
  const nodes = data.nodes.slice().sort((a, b) => b.degree - a.degree || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const boxes = domainGroups(nodes, domains);
  view = graphView({
    nodes: nodes.map((n) => (boxes.groups.length ? { ...nodeOf(n), group: boxes.group(n) } : nodeOf(n))),
    edges: data.edges.map(edgeOf),
    groups: boxes.groups,
    center: g.center,
    selected: S.graph.selected,
    label: g.center ? tr("memory.graph.labelAround", { id: g.center }) : tr("memory.graph.labelWhole"),
    keys: graphKeys(),
    fill: true,
  }, {
    engine: graphEngine,
    onSelect: (id) => { S.graph.selected = id; paintInspector(); },
    onOpen: (id) => { void hooks.openNote(id); },
  });
  view.el.id = "memGraph";
  stage.replaceWith(view.el);
  paintInspector();
}

/** The tab is leaving: release the engine. */
export function dropGraph(): void {
  if (view) { view.destroy(); view = null; }
}

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

/* The node-link graph (SPEC §panel.ui; the memory page's Graph tab and a note's relations, SPEC
 * §memory 11.2). The drawing is an ENGINE's - the vendored Cytoscape.js (SPEC §panel.vendor) in
 * the panel, a fake in the tests: a canvas with real layouts, pan and zoom, labelled edges and
 * arrowheads, which the hand-rolled SVG this replaced did badly. ui/ imports nothing outside
 * itself (U2), so the engine arrives as an argument, typed here by the slice of it this file
 * calls; the vendor mirror types the same calls from upstream's side.
 *
 *   graphElements  the engine's elements, pure: one node per GraphNode (tone, history, pending
 *                  and center as classes, the hop it sits at), one edge per GraphEdge whose ends
 *                  both exist. A whole graph's nodes also carry their link count, a dot size
 *                  from it, and `hub` on the most linked; given groups, a box (a compound node)
 *                  round each group's members, nested by parent - a hairball of 300 dots says
 *                  nothing at a glance, a dozen named boxes do (cc-tutorial on 19998).
 *   graphStyle     the stylesheet, pure, over a palette of resolved colours: a canvas cannot read
 *                  CSS custom properties, so graphPalette reads base.css's off the document and a
 *                  theme switch reads them again. Rings around a center are labelled chips; a
 *                  whole graph is a map - dots by links, names under them at one size on screen
 *                  whatever the zoom, the hubs' always, the rest once zoomed in or in reach of
 *                  the pointer or the selection. Forty chips at fit read at 5 px (19998).
 *   graphLayoutOf  the layout, pure: with a center, rings by hop (concentric); with boxes, the
 *                  positions graphElements packed (preset); otherwise a force layout (cose)
 *                  started from a sunflower spiral in the caller's order - the most connected
 *                  first - so the same data lands in the same picture.
 *   packBoxes      a grouped graph's positions, pure: each box's members on a grid in the
 *                  caller's order - a dot and its name to the right, a row a note, like a list -
 *                  its sub-boxes after them on shelves; the top boxes dropped each to the
 *                  lowest free spot, to the pane's shape. The engine's force layout cannot do boxes: it
 *                  threw cc-tutorial's 34 dots 100 000 px apart, whatever its settings.
 *   graphView      the frame: the canvas, zoom and fit, a legend, and a line while the engine
 *                  loads or when it cannot. Returns a handle: select, fit, destroy.
 *
 * Identity is never colour alone (SPEC §panel.design): every node carries its words, every edge
 * its kind, and the legend names each tone and stroke. A selection lifts the node with its own
 * links and neighbours and quiets the rest - which notes a note touches is the question a graph
 * is opened to answer. */
import { h } from "../h.js";
import { tr } from "../i18n.js";
import { iconBtn } from "./button.js";

export type GraphTone = 1 | 2 | 3 | 4 | 5;
export type GraphEdgeStyle = "solid" | "heavy" | "dashed" | "dotted" | "faint";

export interface GraphNode {
  id: string;
  label: string;
  tone: GraphTone;
  /** History: drawn, but quiet. */
  dim?: boolean;
  /** Not yet confirmed: a dashed outline. */
  pending?: boolean;
  /** The tooltip; the label when absent. */
  title?: string;
  /** The caller's hop from the center, used when the edges do not reach a node. */
  hop?: number;
  /** The group (GraphSpec.groups) a whole graph draws this node inside. */
  group?: string;
}

/** A box a whole graph draws round its members - a domain on the memory page. */
export interface GraphGroup {
  id: string;
  label: string;
  /** The enclosing group's id: its box holds this one. */
  parent?: string;
  /** The tooltip; the label when absent. */
  title?: string;
}

export interface GraphEdge {
  from: string;
  to: string;
  style?: GraphEdgeStyle;
  /** An arrowhead at `to`. */
  directed?: boolean;
  /** A conflict: drawn in the error colour. */
  alert?: boolean;
  /** The words on the line. */
  label?: string;
  title?: string;
}

/** One legend entry: a node tone or an edge style. */
export interface GraphKey {
  label: string;
  tone?: GraphTone;
  style?: GraphEdgeStyle;
  directed?: boolean;
  alert?: boolean;
}

export interface GraphSpec {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** The node the rings are drawn around; absent, the whole graph is laid out by force. */
  center?: string | null;
  /** Boxes a whole graph draws round their members; a graph with a center ignores them. */
  groups?: GraphGroup[];
  selected?: string | null;
  /** The drawing's accessible name. */
  label: string;
  keys?: GraphKey[];
  /** A small inline graph (a note's neighbours): no zoom and no wheel, a short frame. */
  compact?: boolean;
  /** Fill the parent's height instead of the fixed frame (a workspace tab). */
  fill?: boolean;
}

/* --- the engine's slice ----------------------------------------------------------------------- */

export interface GraphElementDef {
  group: "nodes" | "edges";
  data: { id: string; source?: string; target?: string; [key: string]: unknown };
  classes?: string;
  position?: { x: number; y: number };
}

export interface GraphStyleBlock {
  selector: string;
  style: Record<string, unknown>;
}

export interface GraphLayoutOpts {
  name: string;
  [key: string]: unknown;
}

/** Elements and collections are one model in the engine. */
export interface GraphEls {
  readonly length: number;
  id(): string;
  data(key: string): unknown;
  addClass(classes: string): GraphEls;
  removeClass(classes: string): GraphEls;
  hasClass(cls: string): boolean;
  closedNeighborhood(): GraphEls;
  edges(): GraphEls;
  not(other: GraphEls): GraphEls;
  /** The elements' extent in model coordinates, labels included. */
  boundingBox(): { w: number; h: number };
  /** The first node's model position. */
  position(): { x: number; y: number };
  /** Move every node to where the function says. */
  positions(fn: (node: GraphEls) => { x: number; y: number }): GraphEls;
}

export interface GraphCore {
  on(events: string, selector: string, handler: (ev: { target: GraphEls }) => void): unknown;
  on(events: string, handler: (ev: { target: unknown }) => void): unknown;
  elements(): GraphEls;
  nodes(): GraphEls;
  getElementById(id: string): GraphEls;
  layout(options: GraphLayoutOpts): { run(): unknown };
  style(sheet: GraphStyleBlock[]): unknown;
  /** The stylesheet in place; `update` reapplies it, function values included. */
  style(): { update(): unknown };
  batch(fn: () => void): void;
  fit(eles?: GraphEls, padding?: number): unknown;
  zoom(): number;
  zoom(options: { level: number; renderedPosition: { x: number; y: number } }): unknown;
  width(): number;
  height(): number;
  resize(): unknown;
  destroy(): void;
}

export interface GraphEngineOptions {
  container: HTMLElement;
  elements: GraphElementDef[];
  style: GraphStyleBlock[];
  layout: GraphLayoutOpts;
  minZoom: number;
  maxZoom: number;
  wheelSensitivity: number;
  boxSelectionEnabled: boolean;
  autounselectify: boolean;
  autoungrabify: boolean;
  userZoomingEnabled: boolean;
  userPanningEnabled: boolean;
}

export type GraphEngine = (options: GraphEngineOptions) => GraphCore;
export type GraphEngineLoader = () => Promise<GraphEngine>;

/* --- elements --------------------------------------------------------------------------------- */

/** Up to this many characters a label is drawn whole; past it, cut with an ellipsis. */
const LABEL_MAX = 64;

function short(label: string): string {
  return label.length > LABEL_MAX ? label.slice(0, LABEL_MAX - 1) + "…" : label;
}

/** Hops from the center over the edges, either direction; a node the edges do not reach keeps
 *  the caller's hop, else sits one ring past the farthest reached. */
export function graphHops(spec: Pick<GraphSpec, "nodes" | "edges" | "center">): Map<string, number> {
  const out = new Map<string, number>();
  if (!spec.center || !spec.nodes.some((n) => n.id === spec.center)) return out;
  const adj = new Map<string, string[]>();
  for (const n of spec.nodes) adj.set(n.id, []);
  for (const e of spec.edges) {
    const a = adj.get(e.from);
    const b = adj.get(e.to);
    if (!a || !b || e.from === e.to) continue;
    a.push(e.to);
    b.push(e.from);
  }
  out.set(spec.center, 0);
  const queue = [spec.center];
  for (let i = 0; i < queue.length; i++) {
    const u = queue[i];
    for (const v of adj.get(u) || []) {
      if (out.has(v)) continue;
      out.set(v, (out.get(u) ?? 0) + 1);
      queue.push(v);
    }
  }
  let far = 0;
  out.forEach((x) => { if (x > far) far = x; });
  for (const n of spec.nodes) {
    if (out.has(n.id)) continue;
    out.set(n.id, n.hop != null && n.hop > 0 ? n.hop : far + 1);
  }
  return out;
}

/** The sunflower spiral a force layout starts from: the caller's first node in the middle. */
function spiral(i: number): { x: number; y: number } {
  const r = 90 * Math.sqrt(i);
  const a = i * 2.399963229728653;
  return { x: Math.round(r * Math.cos(a)), y: Math.round(r * Math.sin(a)) };
}

/** A whole graph names its most linked nodes at any zoom: this share of them, at most HUB_MAX,
 *  two links or more. */
const HUB_SHARE = 0.25;
const HUB_MAX = 12;

/** A whole graph's dot, in model pixels: wider with every link, slower as they add up. */
export function dotSize(links: number): number {
  return Math.round(14 + 6 * Math.sqrt(links));
}

/** A group's element id: its own namespace, so a group never takes a node's id. */
export function groupElementId(id: string): string {
  return "group:" + id;
}

/** The groups a whole graph draws, parents first: each with a member or a drawn sub-group. A
 *  parent that is not given, or a loop of parents, ends the chain. */
function drawnGroups(spec: GraphSpec): Array<{ g: GraphGroup; depth: number; parent?: string }> {
  if (spec.center || !spec.groups || !spec.groups.length) return [];
  const byId = new Map(spec.groups.map((g) => [g.id, g]));
  const chain = (id: string): string[] => {
    const out: string[] = [];
    for (let at: string | undefined = id; at && byId.has(at) && !out.includes(at); at = byId.get(at)?.parent) out.push(at);
    return out;
  };
  const used = new Set<string>();
  for (const n of spec.nodes) if (n.group) chain(n.group).forEach((id) => used.add(id));
  return spec.groups.filter((g) => used.has(g.id)).map((g) => {
    const up = chain(g.id);
    return { g, depth: up.length - 1, parent: up[1] };
  }).sort((a, b) => a.depth - b.depth);
}

/** A grouped graph's cell: a dot at its left, the name to its right, one line - a box reads as a
 *  list of its notes. Names under the dots needed rows too tall to pack (19998, rows ran into
 *  each other's names). */
const CELL_W = 230;
const CELL_H = 44;
/** The dot's centre, from its cell's left edge. */
const CELL_DOT = 18;
/** A boxed name's room, in model px: from past its dot to short of the next cell. */
const CELL_NAME = CELL_W - 50;
/** A box round its cells: a margin, room for its name over the top, a gap to the next box. The
 *  margin is the widest edge the engine draws (boxEdge), so a box - a box in a box too - never
 *  grows past the room packed for it. */
const BOX_PAD = 40;
const BOX_HEAD = 56;
const BOX_GAP = 40;
/** A box's edge past its dots, in model px at a zoom: the names under the bottom row are one size
 *  on screen, so further in the model as the zoom falls - held inside the box down to BOX_PAD. */
function boxEdge(zoom: number): number {
  return Math.round(Math.max(16, Math.min(BOX_PAD, 26 / zoom - 6)));
}

interface Block { w: number; h: number; place(x: number, y: number, out: Map<string, { x: number; y: number }>): void }

/** Cells in rows, about as tall as wide on screen. */
function gridBlock(ids: string[]): Block {
  const cols = Math.max(1, Math.ceil(Math.sqrt((ids.length * CELL_H) / CELL_W * 1.6)));
  return {
    w: cols * CELL_W,
    h: Math.ceil(ids.length / cols) * CELL_H,
    place(x, y, out) {
      ids.forEach((id, i) => { out.set(id, { x: x + (i % cols) * CELL_W + CELL_DOT, y: y + Math.floor(i / cols) * CELL_H + CELL_H / 2 }); });
    },
  };
}

/** Blocks on shelves, left to right, a new shelf past `aspect` times the side of their area. */
function shelves(blocks: Block[], aspect: number): Block {
  const area = blocks.reduce((a, b) => a + (b.w + BOX_GAP) * (b.h + BOX_GAP), 0);
  return shelvesAt(blocks, Math.sqrt(area * aspect));
}

/** The top level for a pane `aspect` wide to 1 high: of a range of widths, the skyline whose picture
 *  fits the pane at the largest zoom - a first guess from the area alone left swiss's boxes
 *  taller than the pane, and every name shorter for it. */
function skylineFit(blocks: Block[], aspect: number): Block {
  const area = blocks.reduce((a, b) => a + (b.w + BOX_GAP) * (b.h + BOX_GAP), 0);
  let best = skylineAt(blocks, Math.sqrt(area * aspect));
  const scale = (b: Block): number => Math.max(b.w / aspect, b.h);
  for (let f = 0.5; f <= 2.05; f += 0.1) {
    const next = skylineAt(blocks, Math.sqrt(area * aspect) * f);
    if (scale(next) < scale(best) - 0.5) best = next;
  }
  return best;
}

/** Blocks dropped in turn to the lowest spot no wider than `limit` (or the widest block), the
 *  leftmost of equals - shelves left a tall box's height blank beside the short ones on its shelf
 *  (a real project's map, a third of the picture). `sky`: from each step's x on, the first free y,
 *  a block's footprint taking the gap to its right and below. */
function skylineAt(blocks: Block[], limit: number): Block {
  const width = Math.max(Math.max(...blocks.map((b) => b.w)), limit) + BOX_GAP;
  let sky: Array<{ x: number; y: number }> = [{ x: 0, y: 0 }];
  const top = (x: number, end: number): number => {
    let y = 0;
    sky.forEach((s, i) => { if (s.x < end && (i + 1 === sky.length || sky[i + 1].x > x)) y = Math.max(y, s.y); });
    return y;
  };
  const at: Array<[Block, number, number]> = [];
  let w = 0, h = 0;
  for (const b of blocks) {
    const foot = b.w + BOX_GAP;
    let x = 0, y = top(0, foot);
    for (const s of sky) {
      if (s.x + foot > width) break;
      const sy = top(s.x, s.x + foot);
      if (sy < y) { x = s.x; y = sy; }
    }
    const end = x + foot;
    const resume = top(end, end + 1);
    const after = sky.filter((s) => s.x >= end);
    if (!after.length || after[0].x !== end) after.unshift({ x: end, y: resume });
    sky = [...sky.filter((s) => s.x < x), { x, y: y + b.h + BOX_GAP }, ...after];
    at.push([b, x, y]);
    w = Math.max(w, x + b.w);
    h = Math.max(h, y + b.h);
  }
  return { w, h, place(px, py, out) { for (const [b, bx, by] of at) b.place(px + bx, py + by, out); } };
}

/** Blocks on shelves no wider than `limit` (or the widest block). */
function shelvesAt(blocks: Block[], limit: number): Block {
  const width = Math.max(Math.max(...blocks.map((b) => b.w)), limit);
  const at: Array<[Block, number, number]> = [];
  let x = 0, y = 0, shelf = 0, w = 0;
  for (const b of blocks) {
    if (x > 0 && x + b.w > width) { x = 0; y += shelf + BOX_GAP; shelf = 0; }
    at.push([b, x, y]);
    w = Math.max(w, x + b.w);
    x += b.w + BOX_GAP;
    shelf = Math.max(shelf, b.h);
  }
  return { w, h: y + shelf, place(px, py, out) { for (const [b, bx, by] of at) b.place(px + bx, py + by, out); } };
}

/** A grouped graph's node positions (see the header): the biggest boxes first, the caller's order
 *  between equals; a node in no drawn box keeps a grid of its own, unboxed. `width`: each box's
 *  inside, so a box of one dot is still as wide as its name. */
export function packBoxes(spec: GraphSpec, aspect = 1.5): { at: Map<string, { x: number; y: number }>; width: Map<string, number> } {
  const groups = drawnGroups(spec);
  const drawn = new Set(groups.map((x) => x.g.id));
  const members = new Map<string, string[]>();
  const loose: string[] = [];
  for (const n of spec.nodes) {
    if (n.group && drawn.has(n.group)) members.set(n.group, (members.get(n.group) || []).concat(n.id));
    else loose.push(n.id);
  }
  const kids = new Map<string, string[]>();
  for (const x of groups) if (x.parent) kids.set(x.parent, (kids.get(x.parent) || []).concat(x.g.id));
  const width = new Map<string, number>();
  const total = (id: string): number => (members.get(id) || []).length + (kids.get(id) || []).reduce((a, k) => a + total(k), 0);
  const bigFirst = (ids: string[]): string[] => ids.map((id, i) => ({ id, i, n: total(id) })).sort((a, b) => b.n - a.n || a.i - b.i).map((x) => x.id);
  const box = (id: string): Block => {
    const inner: Block[] = [];
    const own = members.get(id) || [];
    if (own.length) inner.push(gridBlock(own));
    for (const k of bigFirst(kids.get(id) || [])) inner.push(box(k));
    const body = shelves(inner, 1.4);
    // From the first dot on: the engine holds a box round its dots, and widens it to the right.
    width.set(id, body.w - CELL_DOT);
    return { w: body.w + 2 * BOX_PAD, h: body.h + BOX_HEAD + BOX_PAD, place(x, y, out) { body.place(x + BOX_PAD, y + BOX_HEAD, out); } };
  };
  const top: Block[] = bigFirst(groups.filter((x) => !x.parent).map((x) => x.g.id)).map(box);
  if (loose.length) top.push(gridBlock(loose));
  const at = new Map<string, { x: number; y: number }>();
  if (top.length) skylineFit(top, aspect).place(0, 0, at);
  return { at, width };
}

/** `aspect`: the pane's width over its height, which a grouped graph's boxes follow. */
export function graphElements(spec: GraphSpec, o: { aspect?: number } = {}): GraphElementDef[] {
  const hops = graphHops(spec);
  const ids = new Set(spec.nodes.map((n) => n.id));
  const map = !spec.center;
  const groups = drawnGroups(spec);
  const drawn = new Set(groups.map((x) => x.g.id));
  const edges: GraphElementDef[] = [];
  const links = new Map<string, number>();
  const seen = new Set<string>();
  for (const e of spec.edges) {
    if (!ids.has(e.from) || !ids.has(e.to) || e.from === e.to) continue;
    const id = e.from + ">" + e.to + ">" + (e.label || e.style || "");
    if (seen.has(id)) continue;
    seen.add(id);
    links.set(e.from, (links.get(e.from) || 0) + 1);
    links.set(e.to, (links.get(e.to) || 0) + 1);
    const cls: string[] = [e.style || "solid"];
    if (e.directed) cls.push("directed");
    if (e.alert) cls.push("alert");
    edges.push({
      group: "edges",
      data: { id, source: e.from, target: e.to, label: e.label || "", title: e.title || e.label || "" },
      classes: cls.join(" "),
    });
  }
  // The hubs: most linked first, the caller's order between equals. Boxes are named instead: a
  // hub's name at fit is wider than its cell, and the names ran into each other.
  const hubs = new Set(map && !groups.length
    ? spec.nodes.map((n) => n.id).filter((id) => (links.get(id) || 0) >= 2)
      .sort((a, b) => (links.get(b) || 0) - (links.get(a) || 0))
      .slice(0, Math.min(HUB_MAX, Math.ceil(spec.nodes.length * HUB_SHARE)))
    : []);
  // Boxes first, parents before children; the engine sizes each round what it holds.
  const packed = groups.length ? packBoxes(spec, o.aspect ?? 1.5) : null;
  const out: GraphElementDef[] = groups.map(({ g, depth, parent }) => ({
    group: "nodes",
    data: {
      id: groupElementId(g.id), label: short(g.label), title: g.title || g.label, width: packed?.width.get(g.id) ?? 0,
      ...(parent ? { parent: groupElementId(parent) } : {}),
    },
    classes: depth ? "group sub" : "group",
  }));
  spec.nodes.forEach((n, i) => {
    const cls = ["t" + n.tone];
    if (n.dim) cls.push("dim");
    if (n.pending) cls.push("pending");
    if (n.id === spec.center) cls.push("center");
    if (hubs.has(n.id)) cls.push("hub");
    const hop = hops.get(n.id);
    const data: Record<string, unknown> = { id: n.id, label: short(n.label), title: n.title || n.label, hop: hop == null ? null : hop };
    if (map) {
      data.links = links.get(n.id) || 0;
      data.size = dotSize(links.get(n.id) || 0);
    }
    if (n.group && drawn.has(n.group)) data.parent = groupElementId(n.group);
    out.push({ group: "nodes", data: data as GraphElementDef["data"], classes: cls.join(" "), position: map ? (packed?.at.get(n.id) ?? spiral(i)) : undefined });
  });
  return out.concat(edges);
}

/* --- style ------------------------------------------------------------------------------------ */

export interface GraphPalette {
  /** --cat-1..5, in order. */
  tones: string[];
  text: string;
  text2: string;
  text3: string;
  card: string;
  accent: string;
  red: string;
  font: string;
}

export type GraphTokenRead = (name: string) => string;

/** The browser's reader: a token off the document element, where base.css defines them. */
function readToken(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** The palette from the theme's tokens; a missing one falls back to the light theme's value - a
 *  canvas never draws in nothing. */
export function graphPalette(read: GraphTokenRead = readToken): GraphPalette {
  const t = (name: string, fallback: string): string => read(name) || fallback;
  return {
    tones: [t("--cat-1", "#0c84fa"), t("--cat-2", "#109d7b"), t("--cat-3", "#b47825"), t("--cat-4", "#8b44c9"), t("--cat-5", "#c81e48")],
    text: t("--text", "#111114"),
    text2: t("--text-2", "#6b7280"),
    text3: t("--text-3", "#9ca3af"),
    card: t("--card", "#ffffff"),
    accent: t("--accent", "#2563eb"),
    red: t("--red", "#dc2626"),
    font: t("--sans", "system-ui, sans-serif"),
  };
}

function hexRgb(c: string): [number, number, number] | null {
  const m = /^#([0-9a-f]{6})$/i.exec(c.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** `a` laid over `b` at `t` (0-1), as an opaque hex - a node's tint must hide the edges under it,
 *  which a translucent fill would not. Non-hex input answers `b`. */
export function mixHex(a: string, b: string, t: number): string {
  const x = hexRgb(a), y = hexRgb(b);
  if (!x || !y) return b;
  const ch = (i: number): string => Math.round(x[i] * t + y[i] * (1 - t)).toString(16).padStart(2, "0");
  return "#" + ch(0) + ch(1) + ch(2);
}

/** A whole graph names every node from this zoom up: its layout gave each name its room at 1. */
export const LABEL_ZOOM = 0.9;
/** A whole graph's name, on screen: one line, this wide at most. */
const MAP_LABEL_W = 170;
/** A group's name, on screen: one line, this wide at most. */
const GROUP_LABEL_W = 260;
/** A boxed graph names every node whose cell, on screen, holds this much of a name. */
const CELL_NAME_MIN = 64;

export interface GraphStyleOpts {
  compact?: boolean;
  /** A whole graph: the map's dots and names (graphElements gave the nodes their size). */
  map?: boolean;
  /** The engine's zoom now, which a map's names are sized against; 1 when absent. */
  zoom?: () => number;
  /** A map in boxes: every node on a grid (packBoxes), named within its cell while one shows
   *  enough of a name - so a first look reads the notes too, not dots. */
  boxed?: boolean;
}

export function graphStyle(p: GraphPalette, o: GraphStyleOpts = {}): GraphStyleBlock[] {
  const size = o.compact ? 10.5 : 11.5;
  const out: GraphStyleBlock[] = [
    {
      selector: "node",
      style: {
        shape: "round-rectangle", width: "label", height: "label", padding: o.compact ? "6px" : "8px",
        label: "data(label)", "text-wrap": "wrap", "text-max-width": o.compact ? "120px" : "150px",
        "text-valign": "center", "text-halign": "center", "font-family": p.font, "font-size": size,
        "line-height": 1.25, color: p.text, "background-color": p.card, "border-width": 1.5, "border-color": p.text3,
      },
    },
  ];
  p.tones.forEach((tone, i) => {
    out.push({ selector: "node.t" + String(i + 1), style: { "border-color": tone, "background-color": mixHex(tone, p.card, 0.12) } });
  });
  // The map: a model size over the zoom is the same size on screen. Before the rules below, so a
  // selection's ring and history's quiet still apply to a dot.
  const z = (): number => Math.max(o.zoom ? o.zoom() : 1, 0.05);
  const lifted = (e: GraphEls): boolean => e.hasClass("sel") || e.hasClass("hover");
  const near = (e: GraphEls): boolean =>
    z() >= LABEL_ZOOM || (!!o.boxed && z() * CELL_NAME >= CELL_NAME_MIN) || e.hasClass("hub") || e.hasClass("near") || lifted(e);
  // A boxed name keeps to its cell unless it is the one pointed at or picked: its whole name then.
  const nameW = (e: GraphEls): number => (o.boxed && !lifted(e) ? Math.min(MAP_LABEL_W / z(), CELL_NAME) : MAP_LABEL_W / z());
  // A boxed dot stays inside its row.
  const dot = (n: GraphEls): number => {
    const d = Number(n.data("size") || 14) / Math.sqrt(Math.min(z(), 1));
    return o.boxed ? Math.min(d, CELL_H - 10) : d;
  };
  if (o.map) {
    out.push({
      selector: "node",
      style: {
        // Zoomed out, a dot shrinks at half the rate (the root of the zoom), or it ends a speck.
        shape: "ellipse", width: dot, height: dot, padding: "0px", "border-width": 2,
        "text-valign": "bottom", "text-wrap": "ellipsis", "line-height": 1.2,
        "font-size": (): number => size / z(),
        "text-max-width": (n: GraphEls): string => String(Math.round(nameW(n))) + "px",
        "text-margin-y": (): number => 4 / z(),
        "text-background-color": p.card, "text-background-opacity": 1, "text-background-shape": "round-rectangle",
        "text-background-padding": (): string => String(2 / z()) + "px",
        "text-opacity": (n: GraphEls): number => (near(n) ? 1 : 0),
      },
    });
    p.tones.forEach((tone, i) => {
      out.push({ selector: "node.t" + String(i + 1), style: { "background-color": mixHex(tone, p.card, 0.45) } });
    });
    out.push({ selector: "node.hub", style: { "font-weight": 600 } });
    if (o.boxed) {
      out.push(
        // A row: the name to the right of its dot. The notes over every line, so a line from a box
        // inside a box never runs through a name.
        {
          selector: "node:childless",
          style: {
            "text-valign": "center", "text-halign": "right", "text-margin-y": 0,
            "text-margin-x": (): number => 6 / z(), "z-compound-depth": "top",
          },
        },
        // A whole name runs over the next cell's: drawn over it.
        { selector: "node.hover:childless", style: { "z-index": 20 } },
      );
    }
  }
  out.push(
    { selector: "node.pending", style: { "border-style": "dashed" } },
    { selector: "node.dim", style: { opacity: 0.5, "border-style": "dotted", color: p.text2 } },
    { selector: "node.center", style: { "border-width": 3, "font-weight": 600 } },
    {
      selector: "edge",
      style: {
        "curve-style": "bezier", width: 1.5, "line-color": p.text3, "target-arrow-color": p.text3, "arrow-scale": 0.9,
        label: "data(label)", "font-family": p.font, "font-size": size - 1.5, color: p.text2, "text-rotation": "autorotate",
        "text-background-color": p.card, "text-background-opacity": 1, "text-background-padding": "2px",
        "text-background-shape": "round-rectangle", "min-zoomed-font-size": 7,
      },
    },
    { selector: "edge.directed", style: { "target-arrow-shape": "triangle" } },
    { selector: "edge.heavy", style: { width: 2.5, "line-color": p.text2, "target-arrow-color": p.text2 } },
    { selector: "edge.dashed", style: { "line-style": "dashed", "line-dash-pattern": [6, 4] } },
    { selector: "edge.dotted", style: { "line-style": "dotted" } },
    { selector: "edge.faint", style: { opacity: 0.55 } },
    { selector: "edge.alert", style: { "line-color": p.red, "target-arrow-color": p.red, color: p.red } },
    // The selection: its node ringed in the accent, its own links lifted, everything else quiet.
    {
      selector: "node.sel",
      style: { "border-width": 2.5, "underlay-color": p.accent, "underlay-opacity": 0.2, "underlay-padding": 6, "underlay-shape": "round-rectangle", "z-index": 10 },
    },
    { selector: "edge.hl", style: { width: 2.5, "line-color": p.text2, "target-arrow-color": p.text2, color: p.text, "min-zoomed-font-size": 0, "z-index": 9 } },
    { selector: "edge.hl.alert", style: { "line-color": p.red, "target-arrow-color": p.red, color: p.red } },
    { selector: ".faded", style: { opacity: 0.16 } },
  );
  if (o.map) {
    out.push(
      { selector: "node.sel", style: { "underlay-shape": "ellipse" } },
      // A link's words: the selection's always, the rest once zoomed in - at the names' size.
      {
        selector: "edge",
        style: { "font-size": (): number => (size - 1.5) / z(), "min-zoomed-font-size": 0, "text-opacity": (e: GraphEls): number => (z() >= LABEL_ZOOM || e.hasClass("hl") ? 1 : 0) },
      },
      // The lines are the detail, not the picture: thin and quiet until a selection lifts its own.
      { selector: "edge", style: { width: 1, opacity: 0.45 } },
      { selector: "edge.heavy", style: { width: 1.75 } },
      { selector: "edge.hl", style: { width: 2.5, opacity: 1 } },
      { selector: "edge.faded", style: { opacity: 0.12 } },
      // A group: a quiet box, its name over its top edge at one size on screen, always shown -
      // the boxes are what a first look reads. Its size leaves its members' names out: they grow
      // as the zoom falls (hidden or not), and a box sized round them grew with them - fit then
      // zoomed out further, and the boxes swelled again (cc-tutorial on 19998, boxes 3000 px wide
      // round nine dots).
      {
        selector: "node.group",
        style: {
          "compound-sizing-wrt-labels": "exclude", "min-width": "data(width)", "min-width-bias-left": "0%", "min-width-bias-right": "100%",
          shape: "round-rectangle", padding: (): string => String(boxEdge(z())) + "px", "background-color": mixHex(p.text3, p.card, 0.07),
          "border-width": 1, "border-style": "solid", "border-color": mixHex(p.text3, p.card, 0.6),
          label: "data(label)", "text-valign": "top", "text-halign": "center", "text-wrap": "ellipsis",
          "text-max-width": (): string => String(Math.round(GROUP_LABEL_W / z())) + "px",
          "font-size": (): number => (size + 1) / z(), "font-weight": 700, color: p.text,
          "text-margin-y": (): number => -4 / z(), "text-opacity": 1, "text-background-opacity": 0,
        },
      },
      // A sub-box's name a step quieter: one right under its parent's, in a box with no notes of
      // its own, read as the parent's second line (a real project's map).
      {
        selector: "node.group.sub",
        style: { "background-color": mixHex(p.text3, p.card, 0.14), "font-size": (): number => size / z(), "font-weight": 500, color: p.text2 },
      },
    );
  }
  return out;
}

/* --- layout ----------------------------------------------------------------------------------- */

export function graphLayoutOf(spec: GraphSpec): GraphLayoutOpts {
  const padding = spec.compact ? 14 : 30;
  if (spec.center && spec.nodes.some((n) => n.id === spec.center)) {
    let far = 0;
    graphHops(spec).forEach((x) => { if (x > far) far = x; });
    return {
      name: "concentric", animate: false, fit: true, padding, avoidOverlap: true, nodeDimensionsIncludeLabels: true,
      startAngle: (3 * Math.PI) / 2, clockwise: true, minNodeSpacing: spec.compact ? 10 : 24,
      // The center is the innermost ring; every hop one ring further out.
      concentric: (n: GraphEls): number => far + 1 - Number(n.data("hop") ?? far + 1),
      levelWidth: (): number => 1,
    };
  }
  // Boxes: where packBoxes put the nodes (graphElements gave each its position).
  if (drawnGroups(spec).length) return { name: "preset", animate: false, fit: true, padding };
  return {
    name: "cose", animate: false, randomize: false, fit: true, padding, nodeDimensionsIncludeLabels: true,
    idealEdgeLength: (): number => 70, edgeElasticity: (): number => 100, nodeRepulsion: (): number => 9000,
    nodeOverlap: 16, gravity: 0.9, componentSpacing: 50, numIter: 1500, initialTemp: 200, coolingFactor: 0.96, minTemp: 1,
  };
}

/** How lopsided a pane and a drawing must both be before [orient] turns the drawing. */
const ORIENT_RATIO = 1.2;

/** A force layout settles where it settles: a long chain comes out tall, and in a wide pane fit
 *  then shrinks it until no label reads (the Memory graph on 19998: a 300 px column in a 1000 px
 *  pane). Turned a quarter, its long side runs along the pane's - the same drawing, at up to twice
 *  the scale. Idempotent: a turned drawing already matches its pane. */
export function orient(cy: GraphCore, padding: number): void {
  const w = cy.width();
  const h = cy.height();
  const nodes = cy.nodes();
  if (nodes.length < 3 || !w || !h || Math.max(w, h) < ORIENT_RATIO * Math.min(w, h)) return;
  // A straight line has no width at all: as lopsided as a drawing gets.
  const box = nodes.boundingBox();
  if (Math.max(box.w, box.h) < ORIENT_RATIO * Math.min(box.w, box.h) || (box.h > box.w) === (h > w)) return;
  nodes.positions((n) => { const p = n.position(); return { x: p.y, y: -p.x }; });
  cy.fit(undefined, padding);
}

/* --- the view --------------------------------------------------------------------------------- */

export interface GraphEvents {
  /** The engine: the panel passes the vendored loader. */
  engine: GraphEngineLoader;
  /** A node clicked (its id), or the background (null). */
  onSelect?: (id: string | null) => void;
  /** A node double-clicked. */
  onOpen?: (id: string) => void;
}

export interface GraphHandle {
  el: HTMLElement;
  select(id: string | null): void;
  fit(): void;
  destroy(): void;
}

/** Live views, so a view whose frame left the page releases its engine on the next draw. */
const live = new Set<GraphHandle>();

/** A legend swatch: a tone's tinted chip, or a sample of an edge's stroke. */
function swatch(k: GraphKey): HTMLElement {
  if (k.tone) return h("span", { class: "gv-sw gv-t" + String(k.tone), aria: { hidden: "true" } });
  // A solid stroke is the line's own default: only the other styles add a class.
  const style = k.style && k.style !== "solid" ? " gv-" + k.style : "";
  const cls = "gv-sw gv-line" + style + (k.directed ? " gv-dir" : "") + (k.alert ? " gv-alert" : "");
  return h("span", { class: cls, aria: { hidden: "true" } });
}

export function graphLegend(keys: GraphKey[]): HTMLElement {
  return h("div", { class: "gv-legend" }, keys.map((k) => h("span", { class: "gv-key" }, swatch(k), k.label)));
}

/** The graph's frame. The engine loads (once, cached by its loader) and draws when the frame is
 *  in the document; until then a quiet line says so, and a load that fails says that instead. */
export function graphView(spec: GraphSpec, ev: GraphEvents): GraphHandle {
  for (const old of Array.from(live)) if (!old.el.isConnected) old.destroy();
  const canvas = h("div", { class: "gv-canvas" });
  const wait = h("div", { class: "gv-wait" }, tr("ui.graph.loading"));
  let cy: GraphCore | null = null;
  let selected = spec.selected || null;
  let gone = false;
  let themeWatch: MutationObserver | null = null;
  let sizeWatch: ResizeObserver | null = null;

  const boxes = drawnGroups(spec).map((x) => groupElementId(x.g.id));
  const paintSelection = (): void => {
    const core = cy;
    if (!core) return;
    core.batch(() => {
      core.elements().removeClass("sel hl near faded");
      if (!selected) return;
      const el = core.getElementById(selected);
      if (!el.length) return;
      el.addClass("sel");
      const near = el.closedNeighborhood();
      near.edges().addClass("hl");
      near.not(near.edges()).addClass("near");
      core.elements().not(near).addClass("faded");
      // A box's quiet would quiet everything inside it, the selection too.
      for (const id of boxes) core.getElementById(id).removeClass("faded");
    });
  };
  const zoomBy = (f: number): void => {
    if (!cy) return;
    cy.zoom({ level: cy.zoom() * f, renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 } });
  };
  const tools = spec.compact ? null : h("div", { class: "gv-tools" },
    iconBtn("plus", tr("ui.graph.zoomIn"), { ghost: true, data: { gvzoom: "in" } }),
    iconBtn("minus", tr("ui.graph.zoomOut"), { ghost: true, data: { gvzoom: "out" } }),
    iconBtn("expand", tr("ui.graph.fit"), { ghost: true, data: { gvzoom: "fit" } }));
  if (tools) {
    tools.onclick = (e: MouseEvent): void => {
      const b = e.target instanceof Element ? e.target.closest<HTMLElement>("[data-gvzoom]") : null;
      if (!b) return;
      e.stopPropagation();
      const how = b.dataset.gvzoom;
      if (how === "in") zoomBy(1.25);
      else if (how === "out") zoomBy(0.8);
      else handle.fit();
    };
  }
  const stage = h("div", { class: "gv-stage", role: "group", aria: { label: spec.label } }, canvas, wait, tools);
  const keys = spec.keys || [];
  const el = h("div", { class: "gv" + (spec.compact ? " compact" : "") + (spec.fill ? " fill" : "") },
    stage, keys.length ? graphLegend(keys) : null);

  const handle: GraphHandle = {
    el,
    select(id: string | null): void {
      selected = id;
      paintSelection();
    },
    fit(): void {
      if (cy) cy.fit(undefined, spec.compact ? 14 : 30);
    },
    destroy(): void {
      gone = true;
      live.delete(handle);
      if (themeWatch) themeWatch.disconnect();
      if (sizeWatch) sizeWatch.disconnect();
      themeWatch = null;
      sizeWatch = null;
      if (cy) cy.destroy();
      cy = null;
    },
  };
  live.add(handle);

  // A whole graph's names are sized against the zoom, read when the engine styles a node.
  const sheet = (): GraphStyleBlock[] =>
    graphStyle(graphPalette(), { compact: spec.compact, map: !spec.center, boxed: boxes.length > 0, zoom: () => (cy ? cy.zoom() : 1) });

  const draw = (engine: GraphEngine): void => {
    if (gone) return;
    // Boxes are packed to the pane's shape, measured now that the frame is in the document.
    const w = canvas.clientWidth, ht = canvas.clientHeight;
    cy = engine({
      container: canvas,
      elements: graphElements(spec, { aspect: w > 0 && ht > 0 ? w / ht : undefined }),
      style: sheet(),
      layout: graphLayoutOf(spec),
      minZoom: 0.15,
      maxZoom: 3,
      wheelSensitivity: 0.25,
      boxSelectionEnabled: false,
      autounselectify: true,
      autoungrabify: !!spec.compact,
      userZoomingEnabled: !spec.compact,
      userPanningEnabled: !spec.compact,
    });
    wait.hidden = true;
    const core = cy;
    // The whole-graph force layout only (a centered graph is rings, round already; boxes pack
    // round, and turning one would move its members twice). Whether the engine ran the layout
    // inside the constructor or runs it a tick later, one of these sees it.
    if (!spec.center) {
      const pad = spec.compact ? 14 : 30;
      if (!boxes.length) {
        orient(core, pad);
        core.on("layoutstop", () => { orient(core, pad); });
      }
      // Restyled once a frame at most while the zoom moves, and once now: the stylesheet was
      // built before the engine had a zoom to read.
      let queued = false;
      core.on("zoom", () => {
        if (queued) return;
        queued = true;
        requestAnimationFrame(() => { queued = false; if (cy) cy.style().update(); });
      });
      core.style().update();
      core.on("mouseover", "node", (e) => { e.target.addClass("hover"); });
      core.on("mouseout", "node", (e) => { e.target.removeClass("hover"); });
    }
    core.on("tap", "node", (e) => {
      // A box is the background it draws on.
      const id = e.target.hasClass("group") ? null : e.target.id();
      handle.select(id);
      if (ev.onSelect) ev.onSelect(id);
    });
    core.on("tap", (e) => {
      if (e.target !== core) return;
      handle.select(null);
      if (ev.onSelect) ev.onSelect(null);
    });
    core.on("dbltap", "node", (e) => { if (ev.onOpen && !e.target.hasClass("group")) ev.onOpen(e.target.id()); });
    core.on("mouseover", "node, edge", (e) => {
      stage.title = String(e.target.data("title") || "");
      stage.classList.add("gv-hover");
    });
    core.on("mouseout", "node, edge", () => {
      stage.title = "";
      stage.classList.remove("gv-hover");
    });
    paintSelection();
    if (typeof MutationObserver === "function") {
      themeWatch = new MutationObserver(() => { if (cy) cy.style(sheet()); });
      themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    }
    if (typeof ResizeObserver === "function") {
      sizeWatch = new ResizeObserver(() => { if (cy) cy.resize(); });
      sizeWatch.observe(canvas);
    }
  };

  /* The engine measures its container, so it draws only once the frame is in the document: the
     caller appends what this returns, a frame or two later at most. */
  let tries = 0;
  const whenAttached = (engine: GraphEngine): void => {
    if (gone) return;
    if (canvas.isConnected) { draw(engine); return; }
    if (++tries > 120) return;
    requestAnimationFrame(() => { whenAttached(engine); });
  };
  ev.engine().then(whenAttached, () => {
    if (gone) return;
    wait.textContent = tr("ui.graph.failed");
    wait.classList.add("bad");
  });
  return handle;
}

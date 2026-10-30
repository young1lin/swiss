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
   Memory page - the shared core (SPEC §memory 11): the page's state, the one request helper, the
   word maps and the small builders the note sheet, the graph tab and the page itself all draw.

   The request helper is not apiJson: the knowledge routes answer `{error: <code>, message, ...}`,
   and the page must branch on the code (a similar note, a bound root, a moved revision) and show
   the sentence - apiJson would toast the code and hand back null. A stopped plugin answers 503
   `{error: <message>}`, so the message falls back to `error` itself.

   Every closed set (kinds, statuses, markers, link kinds, tiers, revision ops, anchor states)
   resolves through a LITERAL key map, so the i18n completeness scan sees every key.
   ================================================================================================ */
import { api } from "../util.js";
import type { HChild } from "../h.js";
import { fill, h } from "../h.js";
import { locale, tk, tr, trn } from "../i18n.js";
import { catChip, refLink, relTime, tag } from "../ui/index.js";
import type { GraphEdgeStyle, GraphKey, GraphTone } from "../ui/index.js";
import type {
  MemDomain, MemDomainList, MemErrorBody, MemGitIssue, MemKind, MemLinkKind, MemProject, MemProjectSettings, MemReason, MemStatus,
  MemStatusReply,
} from "../types/memory.js";

/* ------------------------------------------------------------------ state (module-local) */

export type MemTab = "domains" | "notes" | "graph" | "agent" | "review";
export type NotesMode = "keyword" | "associate";

export interface GraphState {
  center: string | null;
  depth: number;
  history: boolean;
  selected: string | null;
}

export const S = {
  projects: [] as MemProject[],
  /** The selected project id - `p-…` or "global". "" until the first load picks one. */
  project: "",
  status: null as MemStatusReply | null,
  tab: "notes" as MemTab,
  mode: "keyword" as NotesMode,
  query: "",
  text: "",
  kind: "",
  statusFilter: "",
  unconfirmed: false,
  history: false,
  graph: { center: null, depth: 1, history: false, selected: null } as GraphState,
  /** Notes' domain filter: "" any, "none" the unfiled ones, else a domain's path (its subtree). */
  domain: "",
  /** What the Domains tab shows: a domain's id, or UNFILED / UNCOVERED. */
  dom: null as string | null,
  /** The project's map as last read, and the project it is of; a write drops it (hooks.reload). */
  domains: null as MemDomain[] | null,
  domainsOf: "",
  /** The note open in the detail pane (Notes and Review share it). */
  sel: null as string | null,
  /** The project's own settings as last read, for the head's Auto-update button and the Review
   *  bar (SPEC §memory.settings), and the project they are of; settings null when the read
   *  failed. Null for the global scope, which has no daily review. */
  auto: null as { of: string; settings: MemProjectSettings | null } | null,
  briefPaths: "",
  briefTask: "",
  briefGlobal: true,
  /** Bumped on every mount and unmount: a late answer from an ended visit paints nothing. */
  visit: 0,
};

/** The page's hooks into itself, set by memory.ts: the note sheet and the graph tab call back
 *  into the page without importing it (no module cycle). */
export const hooks = {
  /** Re-read the projects (counts) and repaint the current tab. */
  reload: async (): Promise<void> => { /* set by memory.ts */ },
  /** Switch tab and paint it. */
  showTab: (_tab: MemTab): void => { /* set by memory.ts */ },
  /** Select another project (a 409 memory_root_bound points at one). */
  selectProject: async (_id: string): Promise<void> => { /* set by memory.ts */ },
  /** Show a note in the detail pane - from the graph, a link, a save. */
  openNote: async (_id: string): Promise<void> => { /* set by memory.ts */ },
  /** Show a domain (an id or a path) on the Domains tab - from a note's domain line. */
  openDomain: (_id: string): void => { /* set by memory.ts */ },
  /** Point the project at another directory (or give it one) - the cure for a checkout Git cannot read. */
  changeDir: (): void => { /* set by memory.ts */ },
};

/** The Domains tab's two rows after the map: the unfiled notes, the code no domain owns. */
export const UNFILED = ":unfiled";
export const UNCOVERED = ":uncovered";

/** The project's map, read once and kept until a write drops it; the global scope has none.
 *  A failed read is an empty map (the tab says why when it reads the map itself). */
export async function domainList(): Promise<MemDomain[]> {
  const project = S.project;
  if (S.domains && S.domainsOf === project) return S.domains;
  const p = currentProject();
  if (!p || p.global) return [];
  const r = await req<MemDomainList>("GET", "/domains" + qs({ project }));
  const items = r.ok && r.data ? r.data.items : [];
  if (project === S.project) { S.domains = items; S.domainsOf = project; }
  return items;
}

/** Two no-break spaces: a select collapses plain ones, so a sub-domain would not sit under its parent. */
const INDENT = String.fromCharCode(0xa0, 0xa0);

/** Fill a domain picker: the caller's own options first (any domain, by its anchors, unfiled -
 *  each a [value, label]), then the map in tree order, each indented under its parent. A
 *  domain's value is its path. The pick is set on the filled select: an option marked selected
 *  before it is attached is not picked alike by every DOM. */
export function domainFill(sel: HTMLSelectElement, items: MemDomain[], cur: string, lead: Array<[string, string]>): void {
  fill(sel,
    lead.map(([value, label]) => h("option", { value }, label)),
    items.map((d) => h("option", { value: d.path }, INDENT.repeat(d.depth - 1) + d.title + " · " + d.path)));
  sel.value = cur;
}

export function domainSelect(o: { id: string; label?: string }, items: MemDomain[], cur: string, lead: Array<[string, string]>): HTMLSelectElement {
  const sel = h("select", { id: o.id, aria: o.label ? { label: o.label } : undefined });
  domainFill(sel, items, cur, lead);
  return sel;
}

export function currentProject(): MemProject | null {
  return S.projects.find((p) => p.id === S.project) || null;
}

/* ------------------------------------------------------------------ requests */

export interface Reply<T> {
  ok: boolean;
  status: number;
  data: T | null;
  err: MemErrorBody | null;
}

function isErrBody(j: unknown): j is MemErrorBody {
  return typeof j === "object" && j !== null && "error" in j && typeof (j as { error: unknown }).error === "string";
}

/** One call to /api/knowledge: the parsed body on 2xx, the error envelope otherwise. Never throws
 *  and never toasts - the caller decides where the sentence goes. */
export async function req<T>(method: string, path: string, body?: unknown): Promise<Reply<T>> {
  try {
    const opts: RequestInit = { method };
    if (body !== undefined) opts.body = JSON.stringify(body);
    const r = await api("/api/knowledge" + path, opts);
    const j: unknown = await r.json().catch((): unknown => null);
    if (r.ok) return { ok: true, status: r.status, data: j as T, err: null };
    return { ok: false, status: r.status, data: null, err: isErrBody(j) ? j : { error: "", message: tr("util.httpN", { n: r.status }) } };
  } catch (e) {
    return { ok: false, status: 0, data: null, err: { error: "", message: tr("util.requestFailedGatewayRunning") } };
  }
}

/** The sentence of a failure: the server's message, else the bare error (a stopped plugin's 503
 *  carries only that), else a generic one. */
export function errMessage(e: MemErrorBody | null): string {
  if (!e) return tr("memory.err.failed");
  if (e.issue) return gitIssue(e.issue, e.message).why;
  return e.message || e.error || tr("memory.err.failed");
}

/** Why a checkout gave no history, in the reader's words: `short` for a footer or a row, `why`
 *  for the page - git's own line only when nothing better is known. `fix`: pointing the project
 *  at its checkout is the cure. */
export function gitIssue(issue: MemGitIssue | undefined, reason?: string): { short: string; why: string; fix: boolean } {
  switch (issue) {
    case "no_directory": return { short: tr("memory.git.noDir"), why: tr("memory.git.noDirWhy"), fix: true };
    case "not_a_repository": return { short: tr("memory.git.notRepo"), why: tr("memory.git.notRepoWhy"), fix: true };
    case "git_missing": return { short: tr("memory.git.missing"), why: tr("memory.git.missingWhy"), fix: false };
    case "refused": return { short: tr("memory.git.refused"), why: tr("memory.git.refusedWhy"), fix: false };
    default: return { short: tr("memory.git.failed"), why: tr("memory.git.failedWhy", { reason: reason || "" }), fix: false };
  }
}

/** A query string from the set fields; empty, false and null ones are left out. */
export function qs(params: Record<string, string | number | boolean | null | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v == null || v === "" || v === false) continue;
    p.set(k, String(v));
  }
  const s = p.toString();
  return s ? "?" + s : "";
}

/** The path segment of a record or project id. */
export function seg(id: string): string {
  return encodeURIComponent(id);
}

/* ------------------------------------------------------------------ the closed sets */

export const KINDS: MemKind[] = ["decision", "constraint", "pitfall", "knowledge", "preference"];
export const STATUSES: MemStatus[] = ["active", "needs_review", "superseded", "invalidated"];
/** The link kinds a person may add; `supersedes` is written by a supersede only. */
export const LINKABLE: MemLinkKind[] = ["supports", "depends_on", "contradicts", "related_to"];
export const LINK_KINDS: MemLinkKind[] = ["supports", "depends_on", "contradicts", "related_to", "supersedes"];

const KIND_KEYS: Record<string, string> = {
  decision: tk("memory.kind.decision"),
  constraint: tk("memory.kind.constraint"),
  pitfall: tk("memory.kind.pitfall"),
  knowledge: tk("memory.kind.knowledge"),
  preference: tk("memory.kind.preference"),
};
const STATUS_KEYS: Record<string, string> = {
  active: tk("memory.status.active"),
  needs_review: tk("memory.status.needs_review"),
  superseded: tk("memory.status.superseded"),
  invalidated: tk("memory.status.invalidated"),
};
const MARKER_KEYS: Record<string, string> = {
  unconfirmed: tk("memory.marker.unconfirmed"),
  needs_review: tk("memory.marker.needs_review"),
  anchor_changed: tk("memory.marker.anchor_changed"),
  old: tk("memory.marker.old"),
  contradicted: tk("memory.marker.contradicted"),
  dependency_broken: tk("memory.marker.dependency_broken"),
};
const LINK_KEYS: Record<string, string> = {
  supports: tk("memory.link.supports"),
  depends_on: tk("memory.link.depends_on"),
  contradicts: tk("memory.link.contradicts"),
  related_to: tk("memory.link.related_to"),
  supersedes: tk("memory.link.supersedes"),
};
/** A link read from its other end: "supported by", "needed by". Symmetric kinds read the same. */
const LINK_IN_KEYS: Record<string, string> = {
  supports: tk("memory.linkIn.supports"),
  depends_on: tk("memory.linkIn.depends_on"),
  contradicts: tk("memory.link.contradicts"),
  related_to: tk("memory.link.related_to"),
  supersedes: tk("memory.linkIn.supersedes"),
};
const TIER_KEYS: Record<string, string> = {
  id: tk("memory.tier.id"),
  title: tk("memory.tier.title"),
  anchor: tk("memory.tier.anchor"),
  prefix: tk("memory.tier.prefix"),
  lexical: tk("memory.tier.lexical"),
  cjk: tk("memory.tier.cjk"),
  substring: tk("memory.tier.substring"),
  semantic: tk("memory.tier.semantic"),
};
const OP_KEYS: Record<string, string> = {
  create: tk("memory.op.create"),
  edit: tk("memory.op.edit"),
  mark_verified: tk("memory.op.mark_verified"),
  mark_outdated: tk("memory.op.mark_outdated"),
  invalidate: tk("memory.op.invalidate"),
  supersede: tk("memory.op.supersede"),
  restore: tk("memory.op.restore"),
  confirm: tk("memory.op.confirm"),
  import: tk("memory.op.import"),
  // A domain's own ops (§memory.domains): a moved path, a hub verified true to the code.
  move: tk("memory.op.move"),
  verify: tk("memory.op.verify"),
};
const ANCHOR_KEYS: Record<string, string> = {
  ok: tk("memory.anchor.ok"),
  changed: tk("memory.anchor.changed"),
  missing: tk("memory.anchor.missing"),
  unchecked: tk("memory.anchor.unchecked"),
  none: tk("memory.anchor.none"),
};
const ORIGIN_KEYS: Record<string, string> = {
  user: tk("memory.origin.user"),
  agent: tk("memory.origin.agent"),
};
const PRODUCER_KEYS: Record<string, string> = {
  human: tk("memory.producer.human"),
  agent: tk("memory.producer.agent"),
  bootstrap: tk("memory.producer.bootstrap"),
  import: tk("memory.producer.import"),
  sync: tk("memory.producer.sync"),
  unknown: tk("memory.producer.unknown"),
};
const TRANSPORT_KEYS: Record<string, string> = {
  cli: tk("memory.transport.cli"),
  mcp: tk("memory.transport.mcp"),
  panel: tk("memory.transport.panel"),
  internal: tk("memory.transport.internal"),
};
const SOURCE_KEYS: Record<string, string> = {
  human: tk("memory.ev.src.human"),
  session: tk("memory.ev.src.session"),
  code: tk("memory.ev.src.code"),
  commit: tk("memory.ev.src.commit"),
  test: tk("memory.ev.src.test"),
  doc: tk("memory.ev.src.doc"),
  bootstrap: tk("memory.ev.src.bootstrap"),
  import: tk("memory.ev.src.import"),
};
const RELATION_KEYS: Record<string, string> = {
  derived_from: tk("memory.ev.rel.derived_from"),
  supports: tk("memory.ev.rel.supports"),
  verified_by: tk("memory.ev.rel.verified_by"),
};

/** A word from a map, or the wire value itself when a newer server sends one this page lacks. */
function word(map: Record<string, string>, v: string): string {
  const key = map[v];
  return key ? tr(key) : v;
}

export const kindLabel = (k: string): string => word(KIND_KEYS, k);
export const statusLabel = (s: string): string => word(STATUS_KEYS, s);
export const markerLabel = (m: string): string => word(MARKER_KEYS, m);
export const linkLabel = (k: string): string => word(LINK_KEYS, k);
export const linkInLabel = (k: string): string => word(LINK_IN_KEYS, k);
export const tierLabel = (t: string): string => word(TIER_KEYS, t);
export const opLabel = (o: string): string => word(OP_KEYS, o);
export const anchorLabel = (a: string): string => word(ANCHOR_KEYS, a);
export const originLabel = (o: string): string => word(ORIGIN_KEYS, o);
export const producerLabel = (p: string): string => word(PRODUCER_KEYS, p);
export const transportLabel = (t: string): string => word(TRANSPORT_KEYS, t);
export const sourceLabel = (s: string): string => word(SOURCE_KEYS, s);
export const relationLabel = (r: string): string => word(RELATION_KEYS, r);

export function isLive(status: string): boolean {
  return status === "active" || status === "needs_review";
}

/* The graph's encodings: a kind is a tone (colour and shape), in the kinds' fixed order; a link
   kind is a stroke. Directed kinds carry an arrowhead. */
const KIND_TONE: Record<string, GraphTone> = { decision: 1, constraint: 2, pitfall: 3, knowledge: 4, preference: 5 };
const LINK_STYLE: Record<string, GraphEdgeStyle> = {
  supports: "solid", depends_on: "heavy", contradicts: "dashed", related_to: "dotted", supersedes: "faint",
};
const DIRECTED = new Set(["supports", "depends_on", "supersedes"]);

export function kindTone(k: string): GraphTone { return KIND_TONE[k] ?? 4; }
export function linkStyle(k: string): GraphEdgeStyle { return LINK_STYLE[k] ?? "solid"; }

export function isDirected(k: string): boolean { return DIRECTED.has(k); }

/** The graph legend: every kind's tone, every link kind's stroke - a contradiction in red. */
export function graphKeys(): GraphKey[] {
  return [
    ...KINDS.map((k): GraphKey => ({ label: kindLabel(k), tone: kindTone(k) })),
    ...LINK_KINDS.map((k): GraphKey => ({ label: linkLabel(k), style: linkStyle(k), directed: DIRECTED.has(k), alert: k === "contradicts" })),
  ];
}

/** A note's kind as the graph draws it: the tone's swatch, then the word (or the swatch alone,
 *  the word in its tooltip, where a list has no room for it). */
export function kindChip(kind: string, o: { bare?: boolean } = {}): HTMLElement {
  return catChip(kindTone(kind), kindLabel(kind), o);
}

/* ------------------------------------------------------------------ small builders */

/** Markers that are a state worth amber; the rest (unconfirmed, old) are plain words. */
const WARN_MARKERS = new Set(["needs_review", "anchor_changed", "contradicted", "dependency_broken"]);

export function markerTags(markers: string[]): HChild[] {
  return markers.map((m): HChild => [" ", tag(markerLabel(m), WARN_MARKERS.has(m) ? { tone: "warn" } : {})]);
}

/** A history note's status beside its title: superseded or invalidated, said aloud. */
export function statusTag(status: string): HChild {
  if (isLive(status)) return status === "needs_review" ? tag(statusLabel(status), { tone: "warn" }) : null;
  return tag(statusLabel(status));
}

/** An instant in the reader's locale - the tooltip beside a relative time. */
export function fullTime(ms: number): string {
  return new Date(ms).toLocaleString(locale());
}

export function ageText(ms: number): string {
  return relTime(ms);
}

/** A size a reader can take in: bytes under a KiB, then one decimal of KiB, MiB or GiB. */
export function sizeText(n: number): string {
  if (n < 1024) return tr("memory.pv.bytes", { n });
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KiB";
  if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MiB";
  return (n / (1024 * 1024 * 1024)).toFixed(1) + " GiB";
}

/** A keyword hit's score: whole numbers as they are, fractions to two places. */
export function scoreText(n: number): string {
  return Number.isInteger(n) ? String(n) : n.toFixed(2);
}

/** Why an association matched, one phrase per reason. */
export function reasonText(r: MemReason): string {
  if (r.type === "link") return trn(r.hops, "memory.reason.link.one", "memory.reason.link.other", { kind: linkLabel(r.kind), via: r.via });
  if (r.type === "text") return tr("memory.reason.text", { terms: r.terms.join(", ") });
  if (r.type === "similar") return tr("memory.reason.similar", { n: r.similarity.toFixed(2) });
  return tr("memory.reason.anchor", { paths: r.paths.join(", ") });
}

/** The reasons as one quiet line. */
export function reasonsNode(reasons: MemReason[]): HTMLElement {
  const parts: HChild[] = [];
  reasons.forEach((r, i) => {
    if (i) parts.push(" · ");
    parts.push(h("span", { data: { mreason: r.type } }, reasonText(r)));
  });
  return h("span", null, parts);
}

/** The title of a note as the link that opens it: a real button, so the keyboard reaches it. */
export function noteButton(s: { id: string; title: string }): HTMLButtonElement {
  return refLink(s.title, { data: { mopen: s.id } });
}

/** Lines of a textarea: trimmed, blanks dropped. */
export function lines(text: string): string[] {
  return text.split("\n").map((l) => l.trim()).filter(Boolean);
}

/** Search keywords as typed: split on commas (either width), 、 and line breaks; blanks dropped. */
export function keywordList(text: string): string[] {
  return text.split(/[,\uFF0C\u3001\n]/).map((k) => k.trim()).filter(Boolean);
}

/** A note's or a domain's search keywords as chips. */
export function keywordChips(keywords: string[]): HTMLElement {
  return h("div", { data: { mkeywords: "" } }, keywords.map((k) => [tag(k), " "]));
}

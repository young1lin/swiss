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

/* The UI library's gallery (SPEC §panel.ui): /admin/ui.html, reached by typing it - it is
 * not a user feature and has no seat in the panel. It shows every shape the library draws, in
 * every state, then the scenes (ui-scenes.ts): whole pages composed from those shapes, which is
 * where a new design is proposed before any page is built.
 *
 * It is a page of the panel in everything but navigation: the same base.css + ui.css (and not
 * views.css - what renders right here renders right on any page), the shell's context bar, the
 * one sprite (fetched from index.html rather than copied, so there is never a second list of
 * icons), the same tr() tables. Its three switches - theme, language, width - are the page's
 * own and live in the query string (?theme=dark&lang=zh&w=960), so a view is a link and
 * flipping one never touches the panel's stored preferences.
 *
 * Everything drawn here goes through ./ui/index.js; the gallery owns only its bar and the
 * few handlers that make the demos answer (a menu opens, a sheet opens, a call expands). */
import type { HChild } from "./h.js";
import { fill, h } from "./h.js";
import { install, langPref, tk, tr } from "./i18n.js";
import { SCENES } from "./ui-scenes.js";
import { graphEngine } from "./graph-engine.js";
import {
  actionBtn, anchoredMenu, btn, card, catChip, chatPane, chatQueue, chatStats, checkField, checkGrid, checkRow, closeMenu, closeSheet, decodeStrings, dock, docHead, dot, editDetail, editLine, editList, editSub,
  emptyNode, factLine, failNote, field, filterInput, flow, flowHead, flowIO, flowProse, flowToggle, form, formActions, formCap, formFold, graphView, groupNode, heldDot, hint, iconBtn, iconNode,
  initSelects, redacted, initSheet, inlineForm, jsonCodeNode, diffNode, kvRow, markdownNode, menuOpen, moreBtn, note, objTab, openFieldSheet,
  grip, pageFoot, pager, pair, pane, paneHead, pickList, pickRow, popupMenu, refLink, relTime, resHead, row, section,
  scrollBox, seg, sheet, sheetOpen, showSheet, sideRow, spinner, split, sw, tag, timeline, timelineMeta, timelineToggle, toTop,
  textNode, tokenCodeNode, transcript, transcriptText, transcriptTurn, toolChip, valueBlock, proseText, codeSchemeFor, workspace, wsBar, wsHead, wsScroll, wsSide, wsSplit,
} from "./ui/index.js";
import type { DotState, FlowStep, GraphEdge, GraphGroup, GraphKey, GraphNode, TimelineItem } from "./ui/index.js";

type Lang = "en" | "zh-CN";
/* code: the code colour scheme (SPEC §panel.code) - ?code= wins over the panel's own preference,
   and the gallery never writes it back. */
interface View { theme: "light" | "dark"; lang: Lang; width: 1440 | 960; code: string }

const MIN = 60 * 1000;

/* --- the view: theme, language, width --------------------------------------------------------- */

/** The query string wins; without one the gallery opens the way the panel is set, and never
 *  writes that setting back. */
function readView(): View {
  const q = new URLSearchParams(location.search);
  const theme = q.get("theme");
  const lang = q.get("lang");
  return {
    theme: theme === "dark" || theme === "light" ? theme : document.documentElement.dataset.theme === "dark" ? "dark" : "light",
    lang: lang === "zh" ? "zh-CN" : lang === "en" ? "en" : langPref(),
    width: q.get("w") === "960" ? 960 : 1440,
    code: q.get("code") || storedCode(),
  };
}

/** The panel's own code scheme preference (util.ts CODE_KEY, spelled out: the gallery imports
 *  nothing but the library, h and i18n). */
function storedCode(): string {
  try { return localStorage.getItem("swiss_code") || "auto"; } catch (e) { return "auto"; }
}

function writeView(v: View): void {
  const q = new URLSearchParams(location.search);
  q.set("theme", v.theme);
  q.set("lang", v.lang === "zh-CN" ? "zh" : "en");
  q.set("w", String(v.width));
  history.replaceState(null, "", location.pathname + "?" + q.toString() + location.hash);
}

let view: View = { theme: "light", lang: "en", width: 1440, code: "auto" };

async function applyView(v: View): Promise<void> {
  view = v;
  document.documentElement.setAttribute("data-theme", v.theme);
  document.documentElement.setAttribute("data-code", codeSchemeFor(v.code, v.theme));
  if (v.lang === "zh-CN") {
    const table: { default: Record<string, string> } = await import("./locales/zh.js");
    install("zh-CN", table.default);
  } else {
    install("en", null);
  }
  // The width is the viewport the page is judged at: the app column is held to it, and a
  // hairline marks its right edge when the window is wider.
  const app = appNode();
  app.style.width = v.width + "px";
  app.style.maxWidth = "100%";
  app.style.boxShadow = "1px 0 0 var(--sep)";
}

function appNode(): HTMLElement {
  const node = document.getElementById("app");
  if (!node) throw new Error("ui.html has no #app");
  return node;
}

/* --- the one sprite --------------------------------------------------------------------------- */

/** The icons live once, in index.html. Fetch it and move its sprite into this page - a second
 *  copy would drift, and a missing glyph here would pass unnoticed for the same reason. */
async function loadSprite(): Promise<string[]> {
  const res = await fetch("/admin/index.html");
  const doc = new DOMParser().parseFromString(await res.text(), "text/html");
  const sprite = doc.querySelector("svg[hidden]");
  if (!sprite) return [];
  document.body.prepend(document.importNode(sprite, true));
  return Array.from(sprite.querySelectorAll("symbol")).map((s) => s.id.replace(/^i-/, ""));
}

let icons: string[] = [];

/* --- the bar ---------------------------------------------------------------------------------- */

function route(): string {
  const id = location.hash.replace(/^#/, "");
  return SCENES.some((s) => "scene-" + s.id === id) ? id : "components";
}

function bar(current: string): HTMLElement {
  const tabs = [{ id: "components", key: tk("gallery.components") }]
    .concat(SCENES.map((s) => ({ id: "scene-" + s.id, key: s.titleKey })));
  return h("header", { class: "ctxbar" },
    h("span", { class: "ctx-title" }, iconNode("puzzle"), h("span", { class: "ctx-name" }, tr("gallery.title"))),
    h("nav", { class: "ctx-tabs", aria: { label: tr("gallery.views") } },
      tabs.map((t) => h("a", { class: "ctx-tab", href: "#" + t.id, aria: { current: t.id === current ? "page" : null } }, tr(t.key)))),
    h("span", { class: "chip" }, tr("gallery.widthChip", { w: view.width })),
    h("span", { class: "app-zone" },
      iconBtn(view.width === 1440 ? "collapse" : "expand", tr(view.width === 1440 ? "gallery.to960" : "gallery.to1440"),
        { ghost: true, data: { g: "width" } }),
      iconBtn(view.theme === "dark" ? "sun" : "moon", tr(view.theme === "dark" ? "gallery.toLight" : "gallery.toDark"),
        { ghost: true, data: { g: "theme" } }),
      iconBtn("languages", tr("gallery.toOtherLang"), { ghost: true, data: { g: "lang" } })));
}

/* --- the catalogue ---------------------------------------------------------------------------- */

/** One component's section: its words, and a row per state. `uses` is the coverage ledger the
 *  gallery test reads (G6): every library component must be claimed by a section or a scene. */
function entry(uses: string[], cap: string, note: string, rows: Array<[string, HChild]>, extra?: HChild): HTMLElement {
  const node = section({ cap: tr(cap), note: tr(note) },
    rows.length ? card(rows.map(([label, v]) => kvRow(label, v))) : null, extra);
  node.dataset.ui = uses.join(" ");
  return node;
}

/** Every dot state, with its word. */
const DOTS: Array<[DotState, string]> = [
  ["up", tk("gallery.dot.up")], ["starting", tk("gallery.dot.starting")], ["stopping", tk("gallery.dot.stopping")],
  ["idle", tk("gallery.dot.idle")], ["error", tk("gallery.dot.error")], ["down", tk("gallery.dot.down")],
  ["off", tk("gallery.dot.off")],
];
const CHOICES = ["default", "staging", "production"];
const MANY = ["* * * * *", "*/5 * * * *", "*/15 * * * *", "0 * * * *", "0 3 * * *", "0 3 * * 1", "0 0 1 * *", "@reboot"];

function sampleCalls(now: number): TimelineItem[] {
  return [
    // Still in flight: no duration yet, its state where the duration will go (live).
    { id: "g0", at: now - MIN, title: "export_frame", arg: '{"frame":"cover"}', live: { text: tr("gallery.d.running") } },
    { id: "g1", at: now - 3 * MIN, title: "query_orders", arg: '{"status":"open"}', ms: 42, same: "open" },
    { id: "g2", at: now - 4 * MIN, title: "query_orders", arg: '{"status":"open"}', ms: 40, same: "open" },
    { id: "g3", at: now - 12 * MIN, title: "list_tables", arg: "{}", ms: 1650 },
    { id: "g4", at: now - 27 * 60 * MIN, title: "export_frame", arg: '{"frame":"hero"}', ms: 88,
      status: { text: tr("gallery.d.error"), tone: "bad" } },
  ];
}

/* A search's results, the way Remote Runs draws them for the needle "running": each row carries
   the line its match is on, the match marked - in the case the output wrote it. */
function sampleHits(now: number): TimelineItem[] {
  return [
    { id: "h1", at: now - 6 * MIN, title: "exec", arg: "bash -s", ms: 985,
      hit: { before: "Active: active (", match: "running", after: ") since Sat 2026-10-10 16:02:51 CST; 2 days ago" } },
    { id: "h2", at: now - 41 * MIN, title: "exec", arg: "kubectl get pods -n shop", ms: 1430,
      hit: { before: "api-7d9f8c6b5-x2k4q   1/1     ", match: "Running", after: "   0          3h12m" } },
  ];
}

/* A MongoDB document as the shell printer (mongo-ejson.ts shellTokens) classes it - the tokens
   tokenCodeNode draws on the Data page's document cards, spelled out here because the gallery
   imports the library and nothing else. */
const SAMPLE_DOC_TOKENS: [string, string][] = [
  ["jv-p", "{"], ["", "\n  "], ["jv-k", "_id"], ["jv-p", ": "], ["jv-f", "ObjectId"], ["jv-p", "("],
  ["jv-s", "\"65f0c0ffee00000000000001\""], ["jv-p", ")"], ["jv-p", ","], ["", "\n  "],
  ["jv-k", "qty"], ["jv-p", ": "], ["jv-n", "5"], ["jv-p", ","], ["", "\n  "],
  ["jv-k", "at"], ["jv-p", ": "], ["jv-f", "ISODate"], ["jv-p", "("], ["jv-s", "\"2024-01-01T00:00:00.000Z\""], ["jv-p", ")"],
  ["", "\n"], ["jv-p", "}"],
];

/* A commit's patch as git prints it (the memory page's commit sheet): a changed file with two
   hunks and a new one, so every line kind and both numbering sides show. */
const SAMPLE_PATCH = [
  "diff --git a/src/orders.rs b/src/orders.rs",
  "index 3f2a1c4..9b8e7d6 100644",
  "--- a/src/orders.rs",
  "+++ b/src/orders.rs",
  "@@ -12,7 +12,8 @@ pub fn retry(order: &Order) -> Result<()> {",
  "     let key = order.idempotency_key();",
  "-    let attempts = 3;",
  "+    // The same key on every attempt: the payment provider dedupes on it.",
  "+    let attempts = config.retries();",
  "     for _ in 0..attempts {",
  "         submit(order, &key)?;",
  "     }",
  "@@ -40,3 +41,3 @@ fn submit(order: &Order, key: &str) -> Result<()> {",
  "-    client.post(order).send()",
  "+    client.post(order).header(\"Idempotency-Key\", key).send()",
  " }",
  "\\ No newline at end of file",
  "diff --git a/docs/retries.md b/docs/retries.md",
  "new file mode 100644",
  "index 0000000..1d2c3b4",
  "--- /dev/null",
  "+++ b/docs/retries.md",
  "@@ -0,0 +1,2 @@",
  "+# Retries",
  "+Every attempt carries the order's idempotency key.",
].join("\n") + "\n";

/* The SQL highlighter's classes (data-filters.ts dbSqlTokens), spelled out for the same reason:
   a DDL as SHOW CREATE TABLE prints it, numbered the way the Data page's DDL pane is. */
const SAMPLE_SQL_TOKENS: [string, string][] = [
  ["jv-c", "-- users, as SHOW CREATE TABLE prints it"], ["", "\n"],
  ["jv-w", "CREATE"], ["", " "], ["jv-w", "TABLE"], ["", " `users` (\n  "],
  ["jv-k", "`id`"], ["", " "], ["jv-w", "int"], ["", " "], ["jv-w", "NOT"], ["", " "], ["jv-l", "NULL"], ["", " "], ["jv-w", "AUTO_INCREMENT"], ["", ",\n  "],
  ["jv-k", "`name`"], ["", " "], ["jv-w", "varchar"], ["", "("], ["jv-n", "64"], ["", ") "], ["jv-w", "DEFAULT"], ["", " "], ["jv-l", "NULL"], ["", ",\n  "],
  ["jv-k", "`balance`"], ["", " "], ["jv-w", "decimal"], ["", "("], ["jv-n", "12"], ["", ","], ["jv-n", "2"], ["", ") "],
  ["jv-w", "DEFAULT"], ["", " "], ["jv-s", "'0.00'"], ["", ",\n  "],
  ["jv-w", "PRIMARY"], ["", " "], ["jv-w", "KEY"], ["", " ("], ["jv-k", "`id`"], ["", ")\n);\n"],
  ["jv-w", "SELECT"], ["", " "], ["jv-f", "count"], ["", "(*) "], ["jv-w", "FROM"], ["", " `users` "], ["jv-w", "WHERE"], ["", " `users`."],
  ["jv-k", "`balance`"], ["", " > "], ["jv-n", "0"], ["", ";"],
];

const SAMPLE_JSON = {
  id: 1042, status: "open", paid: false, discount: null,
  lines: [{ sku: "A-17", qty: 2 }, { sku: "B-03", qty: 1 }],
  meta: '{"source":"import","batch":7}',
};

/** An open call, the way Logs draws one: what the row left out, then the arguments. */
function callBody(it: TimelineItem): HChild {
  return [
    timelineMeta([tr("gallery.d.via", { host: "mcp" }), tr("gallery.d.replySize", { n: 96 })]),
    valueBlock({ label: tr("gallery.d.arguments"), tools: [iconBtn("copy", tr("gallery.d.copy"), { ghost: true }), moreBtn(tr("gallery.d.more"))] },
      jsonCodeNode(decodeStrings(JSON.parse(it.arg || "{}")), false, { oneLine: true }).node),
  ];
}

/** An opened tool call in the flow, the way the LLM history's run sheet draws one: what went in
 *  and what came back, on one code surface. */
function flowBody(s: FlowStep): HChild {
  // A round that reasoned opens on its thought: prose, scrolled past the prose cap (scrollBox),
  // the way the LLM history draws a long reasoning round.
  if (s.id === "f1") return scrollBox(transcriptText(tr("gallery.s.tThink"), { dim: true }));
  return flowIO([
    { label: tr("gallery.d.arguments"), body: jsonCodeNode({ path: "notes.md", offset: 1, limit: 40, step: s.id }, false, { oneLine: true }).node },
    { label: tr("gallery.d.result"), notes: [tr("gallery.d.replySize", { n: 64 })],
      body: textNode("# Retries\nEvery attempt carries the order's idempotency key.", true, "logs").node },
  ]);
}

/* An agent's run the way the LLM history's sheet draws it: a head of fact lines, a round that
   thought and then said a line, tool calls - one opened, one failed, one still in flight; then
   two sync runs the way the memory sheet draws them, a state dot leading each with its verdict
   under the row and a Stop beside the running one. */
function sampleFlow(): HChild[] {
  return [
    flowHead(
      factLine([[dot("up", tr("gallery.dot.up")), " ", tr("gallery.d.flowDone")], "12.4 s"], { lead: true }),
      factLine(["distiller · builtin", h("code", null, "glm / glm-5.3"), "1.2K / 80"])),
    flow([
      { id: "f1", lead: "brain", title: tr("gallery.d.flowRound"), summary: tr("gallery.s.tThink"), meta: "3.2 s" },
      flowProse(markdownNode(tr("gallery.s.tFinal"))),
      { id: "f2", lead: "file", title: "read_file", mono: true, summary: "notes.md :1+40", summaryMono: true, meta: "120 ms" },
      { id: "f3", lead: "search", title: "grep", mono: true, summary: "idempotency src/", summaryMono: true, meta: "40 ms",
        status: { text: tr("gallery.d.error"), tone: "bad" } },
      { id: "f4", lead: "terminal", title: "exec", mono: true, summary: "cargo test -p orders", summaryMono: true, live: true, meta: "3.0 s" },
    ], { open: new Set(["f1", "f2"]), body: flowBody }),
    flow([
      { id: "s1", lead: dot("starting", tr("gallery.dot.starting")), title: tr("gallery.d.flowSync"), summary: tr("gallery.d.flowPhase"),
        detail: tr("gallery.d.flowVerdict"), live: true, meta: "2 min", tools: btn(tr("gallery.d.cancel"), { kind: "ghost" }) },
      { id: "s2", lead: dot("down", tr("gallery.dot.down")), title: tr("gallery.d.flowSync"), summary: tr("gallery.d.error"),
        detail: tr("gallery.d.flowFailure"), detailBad: true, meta: "1 h" },
    ]),
  ];
}

/** A tool argument's name, its JSON type and a sample value - data, the same in every language. */
const SQL_ARG = "sql";
const SQL_TYPE = "string";
const SQL_SAMPLE = "SELECT 1";
const HOST_SAMPLE = "127.0.0.1";
const PORT_SAMPLE = "5432";
const KEY_SAMPLE = "~/.ssh/id_ed25519";
/** Model ids a provider serves, and more it lists - data, the same in every language. */
const MODEL_SAMPLES: Array<[string, "chat" | "embedding"]> = [["qwen3.5:4b", "chat"], ["qwen3:8b", "chat"], ["bge-m3", "embedding"]];
const MODEL_NAME_SAMPLE = "Qwen3 8B";
/** A helper that starts hidden (an editLine()'s sub-line, its details), shown for the page -
 *  with `text` as its content when given. */
function shownNode(node: HTMLElement, text?: string): HTMLElement {
  if (text != null) node.textContent = text;
  node.hidden = false;
  return node;
}
const FOUND_SAMPLES = ["llama3.2:3b", "mistral:7b", "phi4-mini", "gemma3:4b", "nomic-embed-text", "mxbai-embed-large"];
/** A documentation address (RFC 5737 TEST-NET-3): a remote host that is nobody's. */
const REMOTE_SAMPLE = "203.0.113.7";
/** An agent's name and the connection a chat pane talks about - data, the same in every language. */
const AGENT_SAMPLE = "db";
const CONN_SAMPLE = "orders · PostgreSQL";

/** A status line's words that are the same in every language: an HTTP status is a value. */
const HTTP_502 = "HTTP 502";

/* A project's notes and their links, the memory page's Graph tab in miniature. Titles are data -
   the same in every language; the tones follow the memory kinds' fixed order (decision 1,
   constraint 2, pitfall 3, knowledge 4, preference 5) and the strokes the link kinds'. */
const PROJECT_SAMPLE = "swiss";
const GRAPH_NODES: GraphNode[] = [
  { id: "m-1", label: "SQLite over redb", tone: 1 },
  { id: "m-2", label: "One writer per database file", tone: 2 },
  { id: "m-3", label: "sqlx deferred BEGIN cannot upgrade", tone: 3 },
  { id: "m-4", label: "WAL mode with busy_timeout", tone: 4 },
  { id: "m-5", label: "Prefer explicit transactions", tone: 5, pending: true },
  { id: "m-6", label: "redb as the store", tone: 1, dim: true },
  { id: "m-7", label: "FTS5 trigram for CJK search", tone: 4 },
];
const GRAPH_EDGES: GraphEdge[] = [
  { from: "m-2", to: "m-1", style: "solid", directed: true, label: tr("gallery.d.linkSupports") },
  { from: "m-1", to: "m-4", style: "heavy", directed: true, label: tr("gallery.d.linkDepends") },
  { from: "m-5", to: "m-3", style: "solid", directed: true, label: tr("gallery.d.linkSupports") },
  { from: "m-3", to: "m-2", style: "dashed", alert: true, label: tr("gallery.d.linkContradicts") },
  { from: "m-7", to: "m-4", style: "dotted", label: tr("gallery.d.linkRelated") },
  { from: "m-1", to: "m-6", style: "faint", directed: true, label: tr("gallery.d.linkSupersedes") },
];
/** The whole graph by domain: a box round each group, a sub-group inside its parent's. */
function boxedGraph(): { nodes: GraphNode[]; groups: GraphGroup[] } {
  const home: Record<string, string> = { "m-1": "store", "m-2": "store", "m-3": "store", "m-4": "store", "m-6": "store", "m-7": "store/search" };
  return {
    nodes: GRAPH_NODES.map((n) => (home[n.id] ? { ...n, group: home[n.id] } : n)),
    groups: [{ id: "store", label: tr("gallery.d.boxStore") }, { id: "store/search", label: tr("gallery.d.boxSearch"), parent: "store" }],
  };
}
function graphKeys(): GraphKey[] {
  return [
    { label: tr("gallery.d.kindDecision"), tone: 1 }, { label: tr("gallery.d.kindConstraint"), tone: 2 },
    { label: tr("gallery.d.kindPitfall"), tone: 3 }, { label: tr("gallery.d.kindKnowledge"), tone: 4 },
    { label: tr("gallery.d.kindPreference"), tone: 5 },
    { label: tr("gallery.d.linkSupports"), style: "solid", directed: true },
    { label: tr("gallery.d.linkDepends"), style: "heavy", directed: true },
    { label: tr("gallery.d.linkContradicts"), style: "dashed", alert: true },
    { label: tr("gallery.d.linkRelated"), style: "dotted" },
    { label: tr("gallery.d.linkSupersedes"), style: "faint", directed: true },
  ];
}

/** The memory page in miniature: the projects, the project's head and tabs, the notes beside the
 *  one picked from them. Every column at its natural height - a page gives the frame its own. */
function workspaceSample(now: number): HTMLElement {
  const HOUR = 60 * MIN;
  const kinds = [tr("gallery.d.kindDecision"), tr("gallery.d.kindConstraint"), tr("gallery.d.kindPitfall"), tr("gallery.d.kindKnowledge")];
  return workspace(
    wsSide({ cap: tr("gallery.d.projects"), tools: [iconBtn("plus", tr("gallery.d.newProject"), { ghost: true })] },
      sideRow({ name: tr("gallery.d.global"), lead: dot("up", tr("gallery.dot.up")), tail: "4" }),
      sideRow({ name: PROJECT_SAMPLE, lead: dot("up", tr("gallery.dot.up")), badge: tag("1", { tone: "warn", title: tr("gallery.d.waitingN", { n: 1 }) }), tail: "12", selected: true }),
      sideRow({ name: "orders", lead: dot("idle", tr("gallery.d.unreachable")), tail: "0" })),
    wsHead({
      title: PROJECT_SAMPLE,
      sub: h("code", null, "C:/src/swiss"),
      actions: [moreBtn(tr("gallery.d.more")), btn(tr("gallery.d.newNote"), { kind: "primary", icon: "plus" })],
      nav: seg([
        { id: "notes", label: tr("gallery.d.tabNotes"), n: 12 },
        { id: "graph", label: tr("gallery.d.tabGraph"), n: 6 },
      ], "notes", { key: "gws", label: tr("gallery.d.pages") }),
    }),
    wsSplit({
      fixed: "start",
      start: [
        wsBar({ rows: [[filterInput({ placeholder: tr("gallery.d.searchNotes"), label: tr("gallery.d.searchNotes") })]] }),
        wsScroll({}, pickList({ label: tr("gallery.d.tabNotes") },
          pickRow({ lead: catChip(1, kinds[0], { bare: true }), name: GRAPH_NODES[0].label, meta: relTime(now - 2 * HOUR), sub: tr("gallery.d.noteExcerpt"), selected: true }),
          pickRow({ lead: catChip(3, kinds[2], { bare: true }), name: GRAPH_NODES[2].label, meta: relTime(now - 26 * HOUR), sub: [tag(tr("gallery.d.needsReview"), { tone: "warn" }), h("span", null, tr("gallery.d.byActor", { actor: "codex" }))] }),
          pickRow({ lead: catChip(1, kinds[0], { bare: true }), name: GRAPH_NODES[5].label, meta: relTime(now - 30 * 24 * HOUR), sub: [tag(tr("gallery.d.superseded"))], muted: true }))),
        wsBar({ foot: true }, tr("gallery.d.threeNotes")),
      ],
      end: wsScroll({ pad: true },
        docHead({
          tags: [catChip(1, kinds[0])],
          title: GRAPH_NODES[0].label,
          meta: [h("code", null, "m-1"), tr("gallery.d.byActor", { actor: "codex" }), relTime(now - 2 * HOUR)],
          actions: [btn(tr("gallery.d.edit"), { kind: "primary" }), moreBtn(tr("gallery.d.more"))],
        }),
        markdownNode(tr("gallery.d.noteBody"), { class: "note-body" }),
        section({ cap: tr("gallery.d.links") },
          formCap("→ " + tr("gallery.d.linkDepends") + " · 1"),
          card(row({ lead: catChip(4, kinds[3], { bare: true }), name: refLink(GRAPH_NODES[3].label), sub: h("code", null, "m-4") })))),
    }));
}

/** An editor over its results, the results' bar the handle between them (a fixed column, so a
 *  drag has room to trade). */
function dockSample(): HTMLElement {
  const bar = seg([{ id: "0", label: "orders · 3" }, { id: "1", label: "SELECT · 1" }], "0", { key: "gdock", label: tr("gallery.d.dockResults") });
  return h("div", { style: "display: flex; flex-direction: column; height: 320px" },
    dock(tokenCodeNode([["", "SELECT id, total\nFROM orders\nWHERE total > 100"]], { all: true }).node, bar,
      card(row({ name: "1001", sub: "128.00" }), row({ name: "1002", sub: "240.50" }), row({ name: "1003", sub: "101.00" })),
      { key: "gallery-dock", size: 96, min: 48, restMin: 96, label: tr("gallery.d.dockEditor"), restLabel: tr("gallery.d.dockResults") }));
}

/** The sample answer: a code block, then the words. */
const chatAnswer = (): string => "```sql\nSELECT type, count(*) FROM posts GROUP BY type\n```\n" + tr("gallery.d.chatAnswer");

/** An assistant beside the work: a page column, the end edge, the pane - its picker and tools in
 *  the head, a short conversation, the words box with its note and Send; a "/" in the box lists
 *  its command, and two messages wait in the queue above it, the first steered; each turn closes
 *  on its Copy, the answer's code block carrying its own; centred under Send the conversation's
 *  account, its context ring a third full. Scrolling back exposes Back to bottom above the
 *  composer; it returns to the latest words and resumes following. */
function chatSample(): HTMLElement {
  const stats = chatStats({ id: "gallery-chat-stats", note: tr("gallery.d.chatCtxNote") });
  stats.set({
    turns: 2, steps: 5, tokensIn: 3_100, cacheRead: 38_400, cacheWrite: 2_600, tokensOut: 1_250,
    llmMs: 16_000, toolMs: 4_300, ttftMs: 4_200, ttftSteps: 5, decodeMs: 11_800, decodeTokens: 1_250,
    context: { bytes: 352_000, cap: 1_048_576, system: 30_000, tools: 6_400, limit: 256_000, used: 86_200, perByte: 0.24 },
  });
  const queued = chatQueue({ onSteer: (q) => { queued.steer(q.id, true); }, onDelete: () => undefined });
  queued.steer(queued.add(tr("gallery.d.chatQueued1")).id, true);
  queued.add(tr("gallery.d.chatQueued2"));
  const pane = chatPane({
    cap: tr("gallery.d.chatCap"),
    tools: [h("select", { aria: { label: tr("gallery.d.chatAgent") } }, h("option", null, AGENT_SAMPLE)),
      iconBtn("plus", tr("gallery.d.chatNew"), { ghost: true }), iconBtn("x", tr("gallery.d.chatClose"), { ghost: true })],
    body: transcript([
      { side: "user", role: tr("gallery.s.roleUser"), text: tr("gallery.d.chatAsk"), copy: tr("gallery.d.chatAsk") },
      { role: tr("gallery.s.roleAssistant"), text: chatAnswer(), markdown: true, copy: chatAnswer() },
    ]),
    input: h("textarea", { rows: 2, placeholder: tr("gallery.d.chatAskPh") }),
    queued: queued.node,
    commands: [{ name: "compact", arg: tr("gallery.d.chatCmdArg"), hint: tr("gallery.d.chatCmdHint") }],
    note: CONN_SAMPLE,
    actions: [btn(tr("gallery.d.chatSend"), { kind: "primary" })],
    stats: stats.node,
  });
  return h("div", { style: "display: flex; height: 360px" },
    h("div", { style: "flex: 1; min-width: 0" }, card(tokenCodeNode([["", "SELECT id, total\nFROM orders"]], { all: true }).node)),
    grip(pane, { axis: "x", key: "gallery-chat", min: 240, max: 520, label: tr("gallery.d.chatCap"), fold: true, end: true }),
    pane);
}

function catalogue(now: number): HTMLElement {
  const sections = [
    entry(["redacted"], tk("gallery.c.redact"), tk("gallery.c.redactNote"), [
      [tr("gallery.st.redactRemote"), redacted(REMOTE_SAMPLE, { suffix: ":22" })],
      [tr("gallery.st.redactLocal"), redacted(HOST_SAMPLE, { suffix: ":" + PORT_SAMPLE })],
    ]),
    entry(["btn", "iconBtn", "moreBtn"], tk("gallery.c.buttons"), tk("gallery.c.buttonsNote"), [
      [tr("gallery.st.push"), btn(tr("gallery.d.save"))],
      [tr("gallery.st.primary"), btn(tr("gallery.d.create"), { kind: "primary" })],
      [tr("gallery.st.primaryDisabled"), btn(tr("gallery.d.create"), { kind: "primary", disabled: true })],
      [tr("gallery.st.ghost"), btn(tr("gallery.d.cancel"), { kind: "ghost" })],
      [tr("gallery.st.danger"), btn(tr("gallery.d.delete"), { kind: "danger" })],
      [tr("gallery.st.withIcon"), btn(tr("gallery.d.run"), { icon: "play" })],
      [tr("gallery.st.disabled"), btn(tr("gallery.d.save"), { disabled: true })],
      [tr("gallery.st.iconBtn"), iconBtn("copy", tr("gallery.d.copy"))],
      [tr("gallery.st.iconGhost"), iconBtn("folder-plus", tr("gallery.d.newGroup"), { ghost: true })],
      [tr("gallery.st.iconPressed"), iconBtn("star", tr("gallery.d.pin"), { ghost: true, pressed: true })],
      [tr("gallery.st.more"), moreBtn(tr("gallery.d.more"))],
    ]),
    // The action table (SPEC §panel.design rule 7): one row per verb, so a reader can see that the
    // two halves of a pair wear different pictures and that the tooltip states the click's effect.
    entry(["actionBtn"], tk("gallery.c.actions"), tk("gallery.c.actionsNote"), [
      [tr("gallery.st.actStart"), [actionBtn("start", tr("gallery.d.run")), " ", actionBtn("startAll", tr("dataView.runAll")), " ", actionBtn("bookmark", tr("analysis.save")), " ", actionBtn("unbookmark", tr("analysis.unsave"))]],
      [tr("gallery.st.actStop"), actionBtn("stop", tr("gallery.d.stop"))],
      [tr("gallery.st.actEnable"), actionBtn("enable", tr("gallery.d.enable"))],
      [tr("gallery.st.actDisable"), actionBtn("disable", tr("gallery.d.disable"))],
      [tr("gallery.st.actRefresh"), actionBtn("refresh", tr("gallery.d.refresh"))],
      [tr("gallery.st.actRestart"), actionBtn("restart", tr("gallery.d.restart"))],
      [tr("gallery.st.actTest"), actionBtn("test", tr("gallery.d.test"))],
      [tr("gallery.st.actBusy"), actionBtn("stop", tr("gallery.d.actionBusy"), { disabled: true })],
      [tr("gallery.st.actPair"), [actionBtn("start", tr("gallery.d.run")), " ", actionBtn("stop", tr("gallery.d.stop")),
        " ", actionBtn("enable", tr("gallery.d.enable")), " ", actionBtn("disable", tr("gallery.d.disable"))]],
    ]),
    entry(["dot", "heldDot", "tag", "spinner"], tk("gallery.c.status"), tk("gallery.c.statusNote"),
      DOTS.map(([s, k]): [string, HChild] => [tr(k), dot(s, tr(k))]).concat([
        [tr("gallery.st.tagWord"), tag(tr("gallery.d.proxy"))],
        [tr("gallery.st.tagMono"), tag("npx", { mono: true })],
        [tr("gallery.st.tagBad"), tag(tr("gallery.d.error"), { tone: "bad" })],
        [tr("gallery.st.tagWarn"), tag(tr("gallery.d.slow"), { tone: "warn" })],
        [tr("gallery.st.spinner"), [spinner(), " ", tr("gallery.d.loading")]],
        [tr("gallery.st.held"), heldDot(tr("gallery.d.heldTwo"))],
      ])),
    entry(["sw"], tk("gallery.c.switch"), tk("gallery.c.switchNote"), [
      [tr("gallery.st.on"), sw(true, tr("gallery.d.enabled"))],
      [tr("gallery.st.off"), sw(false, tr("gallery.d.enabled"))],
      [tr("gallery.st.disabled"), sw(true, tr("gallery.d.enabled"), { disabled: true })],
    ]),
    entry(["styleSelect"], tk("gallery.c.select"), tk("gallery.c.selectNote"), [
      [tr("gallery.st.closed"), h("select", { aria: { label: tr("gallery.d.group") } }, CHOICES.map((c) => h("option", { value: c }, c)))],
      [tr("gallery.st.longList"), h("select", { aria: { label: tr("gallery.d.schedule") } }, MANY.map((c) => h("option", { value: c }, c)))],
      [tr("gallery.st.disabled"), h("select", { disabled: true, aria: { label: tr("gallery.d.group") } }, CHOICES.map((c) => h("option", { value: c }, c)))],
    ]),
    entry(["paneHead", "resHead", "anchoredMenu", "note", "failNote", "filterInput", "pager", "pageFoot", "inlineForm", "section", "card", "kvRow"], tk("gallery.c.page"), tk("gallery.c.pageNote"), [
      [tr("gallery.st.contentHead"), paneHead({ desc: tr("gallery.d.headDesc"), actions: [iconBtn("folder-plus", tr("gallery.d.newGroup")), btn(tr("gallery.d.newItem"), { kind: "primary", icon: "plus" })] })],
      // Out of a pane the two layers do not pin; the resource scene shows them stuck.
      [tr("gallery.st.resourceHead"), resHead({
        title: "orders-db", desc: tr("gallery.d.resourceDesc"),
        sub: [dot("up", tr("gallery.dot.up")), h("code", null, "/mcp/orders-db"), "·", tr("gallery.dot.up")],
        actions: [btn(tr("gallery.d.disable")), moreBtn(tr("gallery.d.more"), { data: { demo: "anchored" } })],
        nav: seg([{ id: "tools", label: tr("gallery.d.tools"), n: 4 }, { id: "logs", label: tr("gallery.d.logs") }], "logs"),
      })],
      [tr("gallery.st.note"), note(tr("gallery.d.noteQuiet"))],
      [tr("gallery.st.noteBusy"), note(tr("gallery.d.loading"), { busy: true })],
      [tr("gallery.st.noteErr"), note(tr("gallery.d.noteErr"), { err: true })],
      [tr("gallery.st.failNote"), failNote({ text: tr("gallery.d.failText"), why: HTTP_502, action: btn(tr("gallery.d.retry")) })],
      [tr("gallery.st.filter"), filterInput({ placeholder: tr("gallery.d.filter"), label: tr("gallery.d.filter") })],
      [tr("gallery.st.pager"), pager({ label: tr("gallery.d.pages"), status: tr("gallery.d.pageN", { n: 2 }), prev: btn(tr("gallery.d.newer")), next: btn(tr("gallery.d.older")) })],
      [tr("gallery.st.pagerBusy"), pager({
        label: tr("gallery.d.pages"), status: [tr("gallery.d.pageN", { n: 2 }), spinner(), tr("gallery.d.loading")],
        prev: btn(tr("gallery.d.newer"), { disabled: true }), next: btn(tr("gallery.d.older"), { disabled: true }),
      })],
      [tr("gallery.st.inlineForm"), inlineForm(h("input", { placeholder: tr("gallery.d.label"), aria: { label: tr("gallery.d.label") } }), btn(tr("gallery.d.create"), { kind: "primary" }))],
      [tr("gallery.st.foot"), pageFoot({ note: tr("gallery.d.footNote"), rev: "rev 14" })],
      [tr("gallery.st.kvSans"), tr("gallery.d.kvSentence")],
      [tr("gallery.st.kvMono"), h("code", null, "~/.swiss/gateway.json")],
    ]),
    entry(["row"], tk("gallery.c.rows"), tk("gallery.c.rowsNote"), [], card(
      row({ lead: dot("up", tr("gallery.dot.up")), name: "orders-db", sub: [h("code", null, "5432 → 127.0.0.1:15432"), " · ", tr("gallery.d.via", { host: "bastion-eu" })] }),
      row({ lead: dot("error", tr("gallery.dot.error")), name: "reports", err: tr("gallery.d.rowErr") }),
      row({
        name: "nightly-backup", sub: h("code", null, "pg_dump -Fc shop"),
        cols: [
          { v: tr("gallery.d.scheduleWords"), w: "m", title: "cron 0 3 * * *" },
          { v: relTime(now + 13 * 60 * MIN, now), w: "s", title: tr("gallery.d.schedule") },
          { v: [tag(tr("gallery.d.failed"), { tone: "bad" }), " ", relTime(now - 11 * 60 * MIN, now)], w: "l" },
        ],
        primary: btn(tr("gallery.d.runNow")), more: moreBtn(tr("gallery.d.more")),
      }),
      row({ lead: dot("up", tr("gallery.dot.up")), name: "grafana", sub: tr("gallery.d.rowSub"), toggle: sw(true, tr("gallery.d.enabled")), primary: btn(tr("gallery.d.copy"), { kind: "ghost", icon: "copy" }), more: moreBtn(tr("gallery.d.more")) }),
      row({ lead: dot("off", tr("gallery.dot.off")), name: "load-test", sub: tr("gallery.d.rowMuted"), muted: true, cols: [tag(tr("gallery.d.revoked"))] }),
      // A record behind the row (a tool): the name and sub open in place; the controls stay out.
      row({
        name: h("code", null, "query_orders"), sub: tr("gallery.d.toolDesc"), title: tr("gallery.d.toolDesc"),
        detail: [
          valueBlock({ label: tr("gallery.d.fullDescription"), text: tr("gallery.d.toolDesc") }),
          valueBlock({ label: tr("gallery.d.inputSchema") }, jsonCodeNode({ type: "object", required: ["status"], properties: { status: { type: "string" } } }, true).node),
        ],
        primary: btn(tr("gallery.d.try")), toggle: sw(true, tr("gallery.d.enabled")),
      }))),
    entry(["sideRow"], tk("gallery.c.sideRows"), tk("gallery.c.sideRowsNote"), [], card(
      groupNode({ name: "default", count: 4, density: "side", addTitle: tr("gallery.d.newIn", { g: "default" }), moreTitle: tr("gallery.d.groupActions") },
        sideRow({ name: "orders-db", lead: dot("up", tr("gallery.dot.up")), tail: iconNode("pg", "pg"), selected: true }),
        sideRow({ name: "cache", lead: dot("idle", tr("gallery.dot.idle")), tail: iconNode("redis", "redis") }),
        sideRow({ name: "design-files", lead: dot("error", tr("gallery.dot.error")), tail: tag("http", { mono: true }) }),
        // A count of what waits on the user, before the tail (the memory rail's review badge).
        sideRow({ name: PROJECT_SAMPLE, lead: dot("up", tr("gallery.dot.up")), badge: tag("3", { tone: "warn", title: tr("gallery.d.waitingN", { n: 3 }) }), tail: "12" })).root)),
    entry(["workspace", "grip", "wsSide", "wsHead", "wsSplit", "wsBar", "wsScroll", "pickList", "pickRow", "catChip", "docHead", "markdownNode", "refLink"],
      tk("gallery.c.workspace"), tk("gallery.c.workspaceNote"), [], card(workspaceSample(now))),
    entry(["dock"], tk("gallery.c.dock"), tk("gallery.c.dockNote"), [], dockSample()),
    entry(["chatPane", "chatCommands", "chatQueue", "chatStats", "copyBtn"], tk("gallery.c.chat"), tk("gallery.c.chatNote"), [], chatSample()),
    entry(["graphView", "graphLegend"], tk("gallery.c.graph"), tk("gallery.c.graphNote"), [], [
      card(graphView({ nodes: GRAPH_NODES, edges: GRAPH_EDGES, selected: "m-1", label: tr("gallery.d.graphWhole"), keys: graphKeys() }, { engine: graphEngine }).el),
      card(graphView({ nodes: GRAPH_NODES, edges: GRAPH_EDGES, center: "m-1", label: tr("gallery.d.graphCentred"), compact: true }, { engine: graphEngine }).el),
      card(graphView({ ...boxedGraph(), edges: GRAPH_EDGES, label: tr("gallery.d.graphBoxed") }, { engine: graphEngine }).el),
    ]),
    entry(["groupNode"], tk("gallery.c.groups"), tk("gallery.c.groupsNote"), [], [
      groupNode({ name: "default", count: 2, density: "page", addTitle: tr("gallery.d.newIn", { g: "default" }), moreTitle: tr("gallery.d.groupActions") },
        row({ name: "ci-runner", sub: h("code", null, "swk_…3f9a") }), row({ name: "grafana", sub: h("code", null, "swk_…c07e") })).root,
      groupNode({ name: "archive", count: 5, density: "page", collapsed: true, addTitle: tr("gallery.d.newIn", { g: "archive" }), moreTitle: tr("gallery.d.groupActions") }).root,
      groupNode({ name: "staging", count: 0, density: "page", emptyText: tr("gallery.d.emptyGroup"), addTitle: tr("gallery.d.newIn", { g: "staging" }), moreTitle: tr("gallery.d.groupActions") }).root,
      groupNode({ name: "tables", label: tr("gallery.d.derived"), count: 2, density: "page", addTitle: null, moreTitle: null },
        row({ name: "orders" }), row({ name: "order_lines" })).root,
    ]),
    entry(["seg"], tk("gallery.c.seg"), tk("gallery.c.segNote"), [
      [tr("gallery.st.counts"), seg([{ id: "tools", label: tr("gallery.d.tools"), n: 4 }, { id: "resources", label: tr("gallery.d.resources"), n: 0 }, { id: "logs", label: tr("gallery.d.logs") }], "tools")],
      [tr("gallery.st.second"), seg([{ id: "all", label: tr("gallery.d.all") }, { id: "errors", label: tr("gallery.d.errors"), n: 1 }], "errors")],
      [tr("gallery.st.fill"), seg([{ id: "daily", label: tr("jobs.modeDaily") }, { id: "weekly", label: tr("jobs.modeWeekly") },
        { id: "cron", label: tr("jobs.modeCron") }], "daily", { fill: true })],
    ]),
    entry(["objTab"], tk("gallery.c.tabs"), tk("gallery.c.tabsNote"), [
      [tr("gallery.st.tabOpen"), objTab({ name: "orders", selected: true, icon: "table", data: {}, close: { label: tr("gallery.d.closeTab"), data: {} } })],
      [tr("gallery.st.tabHeld"), objTab({ name: "customers", selected: false, icon: "table", data: {}, mark: heldDot(tr("gallery.d.heldTwo")), close: { label: tr("gallery.d.closeTab"), data: {} } })],
      [tr("gallery.st.tabCount"), objTab({ name: "audit_log", selected: false, icon: "table", data: {}, count: { n: 2, title: tr("gallery.d.twoFilters") }, close: { label: tr("gallery.d.closeTab"), data: {} } })],
    ]),
    entry(["transcript", "transcriptText", "transcriptTurn", "toolChip"], tk("gallery.c.transcript"), tk("gallery.c.transcriptNote"), [
      [tr("gallery.st.transcriptFull"), transcript([
        { role: tag(tr("gallery.s.roleSystem")), text: tr("gallery.s.tSystem"), dim: true },
        { side: "user", role: tr("gallery.s.roleUser"), text: tr("gallery.s.tUser"), markdown: true },
        { role: tag(tr("gallery.s.roleThinking")), text: tr("gallery.s.tThink"), dim: true },
        { role: tag(tr("gallery.s.roleAssistant")), code: [toolChip("read", '{ "path": "notes.md" }')] },
        { role: tag(tr("gallery.s.roleFinal")), text: tr("gallery.s.tFinal"), markdown: true },
      ])],
      [tr("gallery.st.transcriptTurn"), transcriptTurn({ role: tag(tr("gallery.s.roleTool")), text: "notes.md\t913 B\t2026-10-05 08:39", mono: true })],
      [tr("gallery.st.transcriptText"), transcriptText(tr("gallery.s.tThink"), { dim: true })],
    ]),
    entry(["timeline", "timelineMeta"], tk("gallery.c.timeline"), tk("gallery.c.timelineNote"), [], [
      timeline(sampleCalls(now), { now, open: new Set(["g3"]), body: callBody }),
      timeline(sampleHits(now), { now }),
    ]),
    entry(["flow", "flowProse", "flowIO", "flowHead", "factLine", "scrollBox"], tk("gallery.c.flow"), tk("gallery.c.flowNote"), [], sampleFlow()),
    entry(["jsonCodeNode", "tokenCodeNode", "valueBlock", "proseText"], tk("gallery.c.code"), tk("gallery.c.codeNote"), [
      [tr("gallery.st.codeOneLine"), valueBlock({ label: tr("gallery.d.arguments"), tools: [iconBtn("copy", tr("gallery.d.copy"), { ghost: true }), moreBtn(tr("gallery.d.more"))] },
        jsonCodeNode({ status: "open", limit: 20 }, false, { oneLine: true }).node)],
      [tr("gallery.st.codeBlock"), valueBlock({ label: tr("gallery.d.result"), notes: [tr("gallery.d.decodedNote")], tools: [iconBtn("copy", tr("gallery.d.copy"), { ghost: true }), moreBtn(tr("gallery.d.more"))] },
        jsonCodeNode(decodeStrings(SAMPLE_JSON), false, { oneLine: true }).node)],
      [tr("gallery.st.codeTokens"), valueBlock({ label: tr("gallery.d.document") }, tokenCodeNode(SAMPLE_DOC_TOKENS, { all: true }).node)],
      [tr("gallery.st.codeProse"), valueBlock({ label: tr("gallery.d.fullDescription"), text: tr("gallery.d.toolDesc") }, proseText(tr("gallery.d.prosePassage")))],
      [tr("gallery.st.codeLined"), tokenCodeNode(SAMPLE_SQL_TOKENS, { all: true, lined: true }).node],
      [tr("gallery.st.codeMarked"), tokenCodeNode(SAMPLE_SQL_TOKENS, { all: true, lined: true, mark: 4 }).node],
    ]),
    entry(["diffNode"], tk("gallery.c.diff"), tk("gallery.c.diffNote"), [], diffNode(SAMPLE_PATCH)),
    entry(["form", "field", "checkField", "pair", "formActions", "formCap", "formFold", "hint", "editList", "editLine", "editSub", "editDetail", "checkRow", "checkGrid"], tk("gallery.c.forms"), tk("gallery.c.formsNote"), [], card(form(
      pair(
        field({ label: tr("gallery.d.host"), control: h("input", { type: "text", value: HOST_SAMPLE }), hint: tr("gallery.d.hostHint") }),
        field({ label: tr("gallery.d.port"), control: h("input", { type: "text", value: PORT_SAMPLE }) })),
      field({ label: SQL_ARG, required: true, meta: SQL_TYPE, control: h("textarea", { placeholder: SQL_SAMPLE }) }),
      checkField({ label: tr("gallery.d.startNow"), control: h("input", { type: "checkbox", checked: true }) as HTMLInputElement, hint: tr("gallery.d.startNowHint") }),
      field({
        group: true, label: tr("gallery.d.retryOn"),
        control: h("div", null,
          checkField({ label: tr("gallery.d.onFailure"), control: h("input", { type: "checkbox", checked: true }) as HTMLInputElement }),
          checkField({ label: tr("gallery.d.onTimeout"), control: h("input", { type: "checkbox" }) as HTMLInputElement })),
      }),
      formFold({ summary: tr("gallery.d.advanced") },
        formCap(tr("gallery.d.sshKey")),
        field({ label: tr("gallery.d.keyPath"), control: h("input", { type: "text", placeholder: KEY_SAMPLE }), action: btn(tr("gallery.d.browse")) })),
      // An editable list (a provider's models): the id, its kind, the default's mark, Details, Move
      // up and Remove on every line - the mark on the second line is empty and the controls still
      // line up. The first line shows what its model states; the second is open on its details.
      field({
        group: true, label: tr("gallery.d.models"), hint: tr("gallery.d.modelsHint"),
        control: h("div", null,
          editList({ cols: 5 }, MODEL_SAMPLES.map(([id, kind], i) => editLine({},
            h("input", { type: "text", value: id, aria: { label: tr("gallery.d.modelId") } }),
            h("select", { aria: { label: tr("gallery.d.kind") } },
              h("option", { value: "chat", selected: kind === "chat" }, tr("gallery.d.chat")),
              h("option", { value: "embedding", selected: kind === "embedding" }, tr("gallery.d.embedding"))),
            h("span", null, i !== 1 ? tag(tr("gallery.d.default")) : null),
            iconBtn(i === 1 ? "chevron-up" : "chevron-down", tr("gallery.d.details"), { ghost: true }),
            iconBtn("arrow-up", tr("gallery.d.moveUp"), { ghost: true }),
            iconBtn("x", tr("gallery.d.remove"), { ghost: true }),
            i === 0 ? shownNode(editSub({}), tr("gallery.d.modelFacts")) : null,
            i === 1 ? shownNode(editDetail({},
              field({ label: tr("gallery.d.displayName"), control: h("input", { type: "text", value: MODEL_NAME_SAMPLE }) }),
              field({
                group: true, label: tr("gallery.d.inputTypes"),
                control: checkRow(
                  { label: tr("gallery.d.inputText"), control: h("input", { type: "checkbox", checked: true, disabled: true }) as HTMLInputElement },
                  { label: tr("gallery.d.inputImage"), control: h("input", { type: "checkbox" }) as HTMLInputElement }),
              }))) : null))),
          formActions(btn(tr("gallery.d.addModel"), { icon: "plus" }))),
      }),
      // Many checks to pick from (what a provider lists): a framed box that scrolls.
      field({
        group: true, label: tr("gallery.d.pickFound"),
        control: checkGrid({}, FOUND_SAMPLES.map((id, i) => checkField({ label: h("code", null, id), control: h("input", { type: "checkbox", checked: i < 2 }) as HTMLInputElement }))),
      }),
      hint(tr("gallery.d.refused"), { bad: true }),
      formActions(btn(tr("gallery.d.save"), { kind: "primary" }), btn(tr("gallery.d.cancel")))))),
    entry(["split"], tk("gallery.c.split"), tk("gallery.c.splitNote"), [], split(
      section({ cap: tr("gallery.d.settings") }, card(form(
        pair(
          field({ label: tr("gallery.d.host"), control: h("input", { type: "text", value: HOST_SAMPLE }) }),
          field({ label: tr("gallery.d.port"), control: h("input", { type: "text", value: PORT_SAMPLE }) })),
        formActions(btn(tr("gallery.d.save"), { kind: "primary" }))))),
      section({ cap: tr("gallery.d.inForce") }, card(kvRow(tr("gallery.d.host"), HOST_SAMPLE, { mono: true }), kvRow(tr("gallery.d.port"), PORT_SAMPLE, { mono: true }))))),
    entry(["popupMenu", "sheet", "openFieldSheet", "toTop"], tk("gallery.c.floating"), tk("gallery.c.floatingNote"), [
      [tr("gallery.st.menu"), btn(tr("gallery.d.openMenu"), { icon: "ellipsis", data: { demo: "menu" } })],
      [tr("gallery.st.menuFind"), btn(tr("gallery.d.openMenu"), { icon: "search", data: { demo: "menufind" } })],
      [tr("gallery.st.sheet"), btn(tr("gallery.d.openSheet"), { data: { demo: "sheet" } })],
      [tr("gallery.c.transcript"), btn(tr("gallery.d.openSheet"), { data: { demo: "chat" } })],
      [tr("gallery.st.fieldSheet"), btn(tr("gallery.d.openFieldSheet"), { data: { demo: "field" } })],
      // The real one is on this page already: it shows once the page is a screen down.
      [tr("gallery.st.toTop"), btn(tr("gallery.d.toEnd"), { data: { demo: "totop" } })],
    ]),
    entry(["emptyNode"], tk("gallery.c.empty"), tk("gallery.c.emptyNote"), [], card(emptyNode({ icon: "plug", title: tr("gallery.d.emptyTitle"), hint: tr("gallery.d.emptyHint"), action: tr("gallery.d.emptyAction") }))),
    entry(["iconNode"], tk("gallery.c.icons"), tk("gallery.c.iconsNote"), icons.map((id): [string, HChild] => [id, iconNode(id)])),
  ];
  return h("div", { class: "shell" },
    pane({ wide: true }, paneHead({ desc: tr("gallery.intro") }), sections));
}

/* --- the demos -------------------------------------------------------------------------------- */

/** A table name - data, the same in every language. */
const OPEN_TABLE = "orders";
/** A wide table's columns - data too. */
const DEMO_COLUMNS = ["order_id", "customer_id", "customer_name", "status", "total", "currency", "created_at", "updated_at", "shipped_at", "note"];

/** The menu with a find box (popupMenu's filter): a long list narrowed as it is typed into. */
function openDemoFind(anchor: HTMLElement): void {
  popupMenu(anchor.getBoundingClientRect(), DEMO_COLUMNS.map((label) => ({ label, fn: () => {} })), { filter: tr("gallery.d.findColumn") });
}

function openDemoMenu(anchor: HTMLElement): void {
  const r = anchor.getBoundingClientRect();
  popupMenu({ left: r.left, top: r.top, bottom: r.bottom, width: r.width }, [
    { label: tr("gallery.d.menuHeading"), fn: () => {}, heading: true },
    { label: OPEN_TABLE, fn: () => {}, icon: "table", dot: true },
    { label: tr("gallery.d.menuPicked"), fn: () => {}, pick: true, on: true },
    { label: tr("gallery.d.menuPick"), fn: () => {}, pick: true },
    { label: tr("gallery.d.menuMore"), fn: () => {}, affordance: "chevron-right" },
    { label: tr("gallery.d.menuRefused"), fn: () => {}, disabled: true, title: tr("gallery.d.menuRefusedWhy") },
    { sep: true },
    { label: tr("gallery.d.delete"), fn: () => {}, danger: true },
  ]);
}

/** The ⋯ of a resource head: the same rows, hung under the head's actions instead of floating. */
function openDemoAnchored(anchor: HTMLElement): void {
  const host = anchor.closest<HTMLElement>(".pane-actions");
  if (!host) return;
  anchoredMenu(host, [
    { label: tr("gallery.d.menuHeading"), fn: () => {}, heading: true },
    { label: tr("gallery.d.menuPicked"), fn: () => {}, pick: true, on: true },
    { label: tr("gallery.d.menuPick"), fn: () => {}, pick: true },
    { sep: true },
    { label: tr("gallery.d.delete"), fn: () => {}, danger: true },
  ]);
}

/** Scroll the gallery's own pane to its end, where its back-to-top button is showing. */
function scrollToEnd(): void {
  const p = appNode().querySelector<HTMLElement>("main.pane");
  if (p) p.scrollTop = p.scrollHeight;
}

function openDemoSheet(): void {
  const cancel = btn(tr("gallery.d.cancel"));
  const ok = btn(tr("gallery.d.save"), { kind: "primary" });
  showSheet(sheet({
    title: tr("gallery.d.sheetTitle"),
    sub: "ci-runner",
    body: card(kvRow(tr("gallery.d.label"), "ci-runner"), kvRow(tr("gallery.d.group"), "default"),
      kvRow(tr("gallery.d.key"), h("code", null, "swk_…3f9a"))),
    // .sheet-foot right-aligns its buttons; a .grow spacer is only for a leading secondary.
    foot: [cancel, ok],
  }));
  cancel.onclick = closeSheet;
  ok.onclick = closeSheet;
}

/** The chat variant: only the transcript scrolls as turns arrive; input and actions stay put. */
function openDemoChat(): void {
  const out = transcript([{ side: "user", role: tr("gallery.s.roleUser"), text: tr("gallery.s.tUser") },
    { role: tr("gallery.s.roleAssistant"), text: tr("gallery.s.tFinal"), markdown: true }]);
  const input = h("textarea", { rows: 3, aria: { label: tr("llm.test.prompt") } });
  const close = btn(tr("llm.close"));
  const send = btn(tr("llm.test.run"), { kind: "primary" });
  showSheet(sheet({ title: tr("gallery.c.transcript"), wide: true, body: out,
    compose: field({ label: tr("llm.test.prompt"), control: input }), foot: [close, send] }));
  close.onclick = closeSheet;
  send.onclick = (): void => {
    if (!input.value.trim()) return;
    out.appendChild(transcriptTurn({ side: "user", role: tr("gallery.s.roleUser"), text: input.value }));
    out.appendChild(transcriptTurn({ role: tr("gallery.s.roleAssistant"), text: tr("gallery.s.tFinal"), markdown: true }));
    input.value = "";
    input.focus();
  };
}

function openDemoFieldSheet(): void {
  openFieldSheet({
    title: tr("gallery.d.fieldTitle"),
    // "taken" shows the inline error a caller's refusal paints; anything else succeeds.
    submit: (v) => (v === "taken" ? tr("gallery.d.nameTaken") : true),
  });
}

/* --- render and wire -------------------------------------------------------------------------- */

/** The made-up data's "now": today at 14:00, whatever the clock says. Items sit minutes and days
 *  before it; measured from the REAL clock, a scene opened just after midnight put "30 minutes
 *  ago" on yesterday and drew a fourth day - the page moved with the time of day. */
function sceneNow(): number {
  const d = new Date();
  d.setHours(14, 0, 0, 0);
  return d.getTime();
}

function render(): void {
  closeMenu();
  if (sheetOpen()) closeSheet();
  const now = sceneNow();
  const current = route();
  const scene = SCENES.find((s) => "scene-" + s.id === current);
  fill(appNode(), h("div", { class: "workbench" }, bar(current), scene ? scene.build(now) : catalogue(now)));
  // Every render builds a new pane, so its back-to-top button is rebuilt with it (SPEC §panel.ui).
  document.querySelectorAll(".to-top").forEach((b) => { b.remove(); });
  const scroller = appNode().querySelector<HTMLElement>("main.pane");
  if (scroller) document.body.appendChild(toTop(scroller));
  document.title = tr("gallery.docTitle");
}

async function flip(what: string): Promise<void> {
  const next: View = { ...view };
  if (what === "theme") next.theme = view.theme === "dark" ? "light" : "dark";
  else if (what === "lang") next.lang = view.lang === "en" ? "zh-CN" : "en";
  else if (what === "width") next.width = view.width === 1440 ? 960 : 1440;
  await applyView(next);
  writeView(next);
  render();
}

function onClick(e: MouseEvent): void {
  const t = e.target instanceof Element ? e.target : null;
  if (!t) return;
  const g = t.closest<HTMLElement>("[data-g]");
  if (g && g.dataset.g) { void flip(g.dataset.g); return; }
  const demo = t.closest<HTMLElement>("[data-demo]");
  if (demo) {
    // The opening click must not reach the document closer below.
    e.stopPropagation();
    if (demo.dataset.demo === "menu") openDemoMenu(demo);
    else if (demo.dataset.demo === "menufind") openDemoFind(demo);
    else if (demo.dataset.demo === "sheet") openDemoSheet();
    else if (demo.dataset.demo === "field") openDemoFieldSheet();
    else if (demo.dataset.demo === "chat") openDemoChat();
    else if (demo.dataset.demo === "anchored") openDemoAnchored(demo);
    else if (demo.dataset.demo === "totop") scrollToEnd();
    return;
  }
  const sum = t.closest(".tl-sum");
  const item = sum && sum.closest<HTMLElement>(".tl-item");
  const list = item && item.closest<HTMLElement>(".tl");
  if (item && list && item.dataset.tlId) {
    const arg = item.querySelector(".tl-arg");
    timelineToggle(list, item.dataset.tlId, callBody({ id: "", at: 0, title: "", arg: arg ? arg.textContent || "{}" : "{}" }));
    return;
  }
  const step = t.closest(".fl-sum") ? t.closest<HTMLElement>(".fl-step") : null;
  const steps = step && step.closest<HTMLElement>(".fl");
  if (step && steps && step.dataset.flId) {
    const id = step.dataset.flId;
    // Only the agent's tool calls carry a body; a sync run's row opens on nothing in the demo.
    flowToggle(steps, id, step.classList.contains("open") || !id.startsWith("f") ? undefined : flowBody({ id, title: "" }));
  }
}

/** Boot: the sprite, the view, the shared mechanisms, the first paint. ui.html calls it; the
 *  gallery test calls it on happy-dom. */
export async function boot(): Promise<void> {
  icons = await loadSprite().catch(() => []);
  await applyView(readView());
  writeView(view);
  initSelects();
  initSheet();
  const app = appNode();
  app.addEventListener("click", onClick);
  // The panel's closers live in main.ts and connect.ts; the gallery has just these two.
  document.addEventListener("click", (e) => {
    const menu = document.getElementById("menu");
    if (menuOpen() && menu && !(e.target instanceof Node && menu.contains(e.target))) closeMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && sheetOpen()) closeSheet();
  });
  window.addEventListener("hashchange", render);
  render();
}

if (document.documentElement.dataset.page === "gallery") void boot();

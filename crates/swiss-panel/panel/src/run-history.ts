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

import type { ApiMcpCallRow, ApiMcpTool } from "./types/api.js";
import type { PgUrlParts } from "./types/dom.js";
import type { ApiMcpResourceRead, McpConfigLike, McpRunResult } from "./types/runs.js";
import type { McpDetail } from "./types/state.js";
import { $, api, apiJson, errText, iconNode, targetEl, toast } from "./util.js";
import { callsPageStep, callsRetry, cancelEdit, changeEditType, clearCalls, deleteRevision, loadCalls, loadPage, pageNext, pagePrev, restoreRevision, runConnTest, saveEdit, saveReplace, showFullResult, showTab, startEdit, startReplace } from "./detail.js";
import { TESTABLE_TYPES, TYPE_FIELDS, TYPE_LABELS, envToText, fieldsNode, parsePgUrl } from "./fields.js";
import { callBlockCopyText, copyLogText, fmtChars, logsBodyNode, readableBody, repaintCallBlock, toggleCall } from "./logs.js";
import { fill, frag, h } from "./h.js";
import type { HChild } from "./h.js";
import { renderPane } from "./pane.js";
import { readRunArgs } from "./run.js";
import { ago } from "./traffic.js";
import { mcpDetail, selectedMcp } from "./mcp-state.js";
import { locale, tr, trn } from "./i18n.js";
import { closeMenu, menuOpen, popupMenu } from "./ui/menu.js";
import { closeSheet } from "./ui/sheet.js";
import {
  btn, card, dot, field, form, formActions, hint, iconBtn, kvRow, note, row, section, sheet, showSheet, spinner, tag, valueBlock,
} from "./ui/index.js";
import type { DotState } from "./ui/index.js";

/* --- Run history: the refill control in the actions row ------------------------------------------ */
/** When an entry ran. Reuses ago() inside a day; past that, ago's time-of-day would be ambiguous,
 *  so the date comes back too. */
function histWhen(iso: string): string {
  return Date.now() - new Date(iso).getTime() < 86400000 ? ago(iso) : new Date(iso).toLocaleString(locale());
}

/** The control's closed label. "Past runs (12)" says what it opens and how much is in it, so
 *  the closed control explains itself without looking like a form value — the history glyph
 *  beside the words is the i-history sprite the paint sites carry (fix-plan #14). */
function histButtonLabel(d: McpDetail, tool: string): string {
  if (d.run.histTool !== tool) return tr("runHistory.pastRuns"); // never loaded, or a fetch in flight
  const n = (d.run.hist || []).length;
  return n ? tr("runHistory.pastRunsN", { n }) : tr("runHistory.pastRuns2");
}

/** The popover's left pane: one button per DISTINCT argument set, newest first, narrowed to the
 *  runs whose FULL recorded arguments contain the search box's query (the server does that matching —
 *  the 96-char row label would miss a keyword deeper in). The row label is the clipped one-line
 *  preview — the full text is what the right pane is for. */
function histRowsNode(d: McpDetail, tool: string): HChild {
  if (d.run.histTool !== tool) return h("div", { class: "hist-empty" }, tr("runHistory.loading"));
  const list = d.run.hist || [];
  if (!list.length) {
    return d.run.histQ
      ? h("div", { class: "hist-empty" }, tr("runHistory.runsWhoseArgumentsContain", { q: d.run.histQ }))
      : h("div", { class: "hist-empty" }, tr("runHistory.runsToolRecorded"));
  }
  return list.map((row) => {
    return h("button", { type: "button", class: "hist-row", data: { seq: row.seq }, title: row.args || tr("runHistory.arguments") },
      row.args || tr("runHistory.arguments"));
  });
}

/** One hovered entry rendered in full: the meta line, the COMPLETE arguments (the row label is
 *  clipped at 96 chars — this is the answer to "let me read the whole thing"), and the reply that
 *  run produced (the stored preview, so a 1 MB reply never loads on a hover). */
function histViewNode(c: ApiMcpCallRow): HChild {
  // The identity line: status dot (the panel's universal up/down mark), when, who, how long —
  // not prose glued with dots, so each fact keeps its own weight and nothing runs together.
  const outcome = c.ok ? tr("runHistory.result") : tr("runHistory.error");
  const head = h("div", { class: "hist-head" },
    dot(c.ok ? "up" : "down", outcome),
    h("span", null, histWhen(c.at)),
    h("span", null, "·"),
    h("span", null, c.via + (c.client ? " (" + c.client + ")" : "")),
    c.ms != null ? h("span", { class: "hist-ms" }, tr("runHistory.durationMs", { ms: c.ms })) : null);
  return frag(
    head,
    valueBlock({ label: tr("runHistory.arguments2") }, ...readableBody(c.args || tr("runHistory.none"), false)),
    valueBlock({ label: outcome }, ...readableBody(c.output || tr("runHistory.empty"), !c.ok)),
    c.preview
      ? h("div", { class: "hist-note" }, tr("runHistory.replyShownFirst"), fmtChars(c.chars),
          tr("runHistory.fullReplyLivesLogs"))
      : null);
}

/** Open/close the popover. Opening does not steal focus from the form — the rows are reachable
 *  with the mouse, or with Tab once the control itself has focus. */
function histToggle(): void {
  const d = mcpDetail();
  if (!d || d.tab !== "run" || !d.run.tool) return;
  d.run.histOpen ? histClose() : histOpen();
}
function histOpen(): void {
  const d = mcpDetail();
  if (!d || d.tab !== "run" || !d.run.tool) return;
  histClose(); // a pane rebuild can leave a stray popover behind under the same id
  const btn = $<HTMLButtonElement>("r-hist");
  if (!btn || btn.disabled) return;
  d.run.histOpen = true;
  btn.setAttribute("aria-expanded", "true");
  // Appended to <body> and position:fixed — inside the form it was clipped by the .group card's
  // overflow:hidden (the panel's existing pattern: .menu.float and the sheet both do this).
  const r = btn.getBoundingClientRect();
  const pop = document.createElement("div");
  pop.className = "hist-pop";
  pop.id = "r-hist-pop";
  pop.style.left = Math.max(8, Math.min(r.left, window.innerWidth - Math.min(780, window.innerWidth * .92) - 8)) + "px";
  // Drop below the button; flip above when that would run off the bottom of the viewport.
  const below = r.bottom + 6;
  const est = Math.min(340, window.innerHeight - 24);
  pop.style.top = (below + est > window.innerHeight ? Math.max(8, r.top - est - 6) : below) + "px";
  // Left pane: the filter box pinned above the rows it narrows. The input is NOT rebuilt while
  // typing — only #r-hist-list's children are swapped (renderHistoryOnly), so focus survives.
  fill(pop,
    h("div", { class: "hist-side" },
      h("div", { class: "hist-search" },
        h("input", { id: "r-hist-q", type: "search", placeholder: tr("runHistory.filterArguments"),
          aria: { label: tr("runHistory.filterPastRunsTheir") }, autocomplete: "off", spellcheck: false })),
      h("div", { class: "hist-list", id: "r-hist-list" }, histRowsNode(d, d.run.tool))),
    h("div", { class: "hist-view", id: "r-hist-view" },
      h("div", { class: "hist-empty" }, tr("runHistory.hoverRunSeeFull"))));
  document.body.appendChild(pop);
  // One delegated listener triplet on the popover (docs/37 R5) instead of three handlers
  // re-attached to every row on every filter repaint: rows come and go, the listener stays.
  pop.onclick = (ev: MouseEvent): void => {
    const row = targetEl(ev)?.closest<HTMLElement>(".hist-row");
    if (row) void applyRunHistory(Number(row.dataset.seq));
  };
  pop.onmouseover = (ev: MouseEvent): void => {
    const row = targetEl(ev)?.closest<HTMLElement>(".hist-row");
    if (row) void histPreview(Number(row.dataset.seq));
  };
  // focusin (bubbling focus) has no on* property - a listener, still one claim on the pop.
  pop.addEventListener("focusin", (ev: FocusEvent): void => {
    const row = targetEl(ev)?.closest<HTMLElement>(".hist-row");
    if (row) void histPreview(Number(row.dataset.seq));
  });
  const box = $<HTMLInputElement>("r-hist-q");
  if (box) {
    box.value = d.run.histQ || "";
    box.oninput = () => { queueHistSearch(d!); };
    // The box is where the hand already is — one ArrowDown lands on the first row, no mouse needed.
    box.onkeydown = (ev) => {
      if (ev.key === "ArrowDown") {
        const first = document.querySelector<HTMLElement>("#r-hist-pop .hist-row");
        if (first) { ev.preventDefault(); first.focus(); }
      }
    };
  }
}
function histClose(): void {
  window.clearTimeout(histSearchTimer!);
  histSearchTimer = null;
  const pop = $("r-hist-pop");
  if (pop) pop.remove();
  const d = mcpDetail();
  if (d && d.run) {
    d.run.histOpen = false;
    d.run.histSelSeq = null;
    // The query belongs to the popover, not the tool: drop it on close, and when it had narrowed
    // the loaded list, reload the unfiltered one so the closed label's count tells the whole truth.
    if (d.run.histQ) {
      d.run.histQ = "";
      if (d.run.histTool === d.run.tool && d.run.tool) {
        d.run.histTool = null;
        void loadRunHistory(d.name, d.run.tool);
      }
    }
  }
  const btn = $<HTMLButtonElement>("r-hist");
  if (btn) btn.setAttribute("aria-expanded", "false");
}

/** Typing in the filter re-asks the server for this tool's runs containing the text — the match
 *  happens there against the FULL stored arguments, so a hit past the 96-char preview still shows.
 *  Debounced: one scan per pause in typing, not one per keystroke. */
let histSearchTimer = null as number | null;
function queueHistSearch(d: McpDetail): void {
  const box = $<HTMLInputElement>("r-hist-q");
  d.run.histQ = box ? box.value.trim() : "";
  window.clearTimeout(histSearchTimer!);
  histSearchTimer = window.setTimeout(() => {
    if (mcpDetail() !== d || !d.run.histOpen) return;
    d.run.histTool = null; // the loaded list answers the old query — force the refetch
    void loadRunHistory(d.name, d.run.tool);
  }, 200);
}

/** Hover/focus a row: show that run in full in the right pane. Entries are cached by seq, and the
 *  seq guard drops a reply that lost the race to a newer hover. */
async function histPreview(seq: number): Promise<void> {
  const d = mcpDetail();
  const view = $("r-hist-view");
  if (!d || d.tab !== "run" || !d.run.histOpen || !view) return;
  d.run.histSelSeq = seq;
  let full = d.run.histFull[seq];
  if (!full) {
    fill(view, h("div", { class: "hist-empty" }, spinner(), " ", tr("runHistory.loading")));
    try {
      const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls/" + encodeURIComponent(seq));
      if (!r.ok) throw new Error(tr("runHistory.httpN", { n: r.status }));
      const j = await r.json();
      if (!j.call || mcpDetail() !== d) return;
      full = d.run.histFull[seq] = j.call;
    } catch (e) {
      if (d.run.histSelSeq === seq) fill(view, h("div", { class: "hist-empty" }, tr("runHistory.couldLoadRun")));
      return;
    }
  }
  if (d.run.histSelSeq !== seq) return; // a newer hover already won
  fill(view, histViewNode(full as ApiMcpCallRow));
}

/** Load the selected tool's newest DISTINCT runs — the full list when the search box is empty, the
 *  runs whose arguments contain d.run.histQ when it is not. Idempotent per tool: a repaint that
 *  re-wires the tab does not refetch. Set d.run.histTool = null first to force a refresh (right
 *  after a run, or on each debounced keystroke in the filter). */
async function loadRunHistory(name: string, tool: string | null): Promise<void> {
  const d = mcpDetail();
  if (!d || d.name !== name || d.run.histTool === tool || d.run.histLoading) return;
  d.run.histLoading = true;
  const q = d.run.histQ || "";
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(name) + "/tool-history?tool=" +
      encodeURIComponent(tool as string) + "&limit=300" + (q ? "&q=" + encodeURIComponent(q) : ""));
    if (r.ok) {
      const j = await r.json();
      // A revisit built a new detail object, or the query moved on (a newer keystroke, or the close
      // that reset it) — a stale reply would narrow the list to something nobody asked for. Drop it.
      if (mcpDetail() === d && d.run.histQ === q) {
        d.run.hist = j.entries || [];
        d.run.histTool = tool;
        renderHistoryOnly();
      }
    }
  } catch (e) { /* the control keeps its previous contents */ }
  finally {
    d.run.histLoading = false;
    // This reply lost the race to a newer query (a newer keystroke, or the reset a close performs),
    // and that query's own call was turned away while this one held the loading slot — run it now
    // rather than leave the list answering the old q. Not gated on the popover: a close-during-fetch
    // must still heal the closed label, which would otherwise sit at "Past runs…" forever.
    if (mcpDetail() === d && d.run.histQ !== q && !d.run.histTool) {
      void loadRunHistory(d.name, d.run.tool);
    }
  }
}

/** Repaint only the control's label and (if open) the popover's rows, never the form around it —
 *  the same discipline as renderRunResult, so the SQL being edited survives a post-run refresh. */
function renderHistoryOnly(): void {
  const d = mcpDetail();
  if (!d || d.tab !== "run" || !d.run.tool) return;
  const btn = $<HTMLButtonElement>("r-hist");
  if (btn) {
    fill(btn, iconNode("history"), " ", histButtonLabel(d, d.run.tool));
    // "No history at all" disables the control — but a filter that matched nothing must not: the
    // popover is mid-search, its empty state is the answer, and slamming it shut would eat the query.
    btn.disabled = !d.run.histQ && d.run.histTool === d.run.tool && !(d.run.hist || []).length;
    if (btn.disabled && d.run.histOpen) histClose();
  }
  const list = $("r-hist-list");
  if (list && d.run.histOpen) {
    fill(list, histRowsNode(d, d.run.tool));
    if (d.run.histSelSeq != null) void histPreview(d.run.histSelSeq); // keep what was being read
  }
}

/** A run was picked: fetch its full arguments by seq, then write them into the form. */
async function applyRunHistory(seq: number): Promise<void> {
  const d = mcpDetail();
  if (!d || !d.run.tool) return;
  const tools = d.tools.items || [];
  let toolDef = null as ApiMcpTool | null;
  for (let i = 0; i < tools.length; i++) if (tools[i].name === d.run.tool) toolDef = tools[i] as ApiMcpTool;
  if (!toolDef) return;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls/" + encodeURIComponent(seq));
    if (!r.ok) { toast(tr("runHistory.httpN", { n: r.status }), true); return; }
    const j = await r.json();
    if (!j.call) return;
    d.run.histFull[seq] = j.call; // the pick just fetched it — a hover later is free
    // Arguments over 4 KB were stored clipped (argsText's overflow marker), so they are no longer
    // parseable JSON — say that precisely instead of the generic read failure.
    if (j.call.args && j.call.args.includes("… +") && j.call.args.endsWith("more characters")) {
      toast(tr("runHistory.runsArgumentsWereToo"), true);
      return;
    }
    fillRunArgs(toolDef, JSON.parse(j.call.args || "{}") || {});
  } catch (e) { toast(tr("runHistory.runsArgumentsCouldRead"), true); }
  histClose();
}

/* wireHistRows retired with the delegated popover listener (docs/37 R5): the rows used to
   re-attach three handlers each on every filter repaint; the popover's own listener now
   answers click/hover/focus for whichever rows exist, and a repaint wires nothing. */

/** Write one call's arguments into the generated form fields — the inverse of readRunArgs. Fields
 *  the call does not mention are cleared, so nothing stale survives a refill; keys the schema no
 *  longer knows are dropped (the tool would reject them anyway). */
function fillRunArgs(tool: ApiMcpTool, args: Record<string, unknown>): void {
  const props = (tool.inputSchema && tool.inputSchema.properties) || {};
  Object.keys(props).forEach((k) => {
    const node = $<HTMLInputElement>("r-arg-" + k);
    if (!node) return;
    const kind = node.dataset.kind;
    if (kind === "boolean") { node.checked = args[k] === true; return; }
    const v = args[k];
    if (v === undefined || v === null) { node.value = ""; return; }
    if (kind === "number") { node.value = String(v); return; }
    if (kind === "array") {
      node.value = (Array.isArray(v) ? v : [v]).map((x) => {
        return x !== null && typeof x === "object" ? JSON.stringify(x) : String(x);
      }).join("\n");
      return;
    }
    if (kind === "object") { node.value = v !== null && typeof v === "object" ? JSON.stringify(v, null, 2) : String(v); return; }
    node.value = String(v); // free-text inputs, the SQL textarea, and the enum dropdown
  });
}

async function runTool(): Promise<void> {
  const d = mcpDetail();
  if (!d || d.run.running) return;
  const tools = d.tools.items || [];
  let toolDef = null as ApiMcpTool | null;
  for (let i = 0; i < tools.length; i++) if (tools[i].name === d.run.tool) toolDef = tools[i] as ApiMcpTool;
  if (!toolDef) return;
  let args;
  try {
    args = readRunArgs(toolDef);
  } catch (err) {
    d.run.result = { ok: false, text: errText(err), ms: 0 };
    renderRunResult();
    return;
  }
  d.run.running = true;
  const btn = $<HTMLButtonElement>("runBtn");
  if (btn) { btn.disabled = true; btn.textContent = tr("runHistory.running"); }
  const meta = $("runMeta");
  if (meta) fill(meta, spinner());
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/call", {
      method: "POST", body: JSON.stringify({ tool: d.run.tool, arguments: args }),
    });
    const j = await r.json().catch(() => { return {}; });
    d.run.result = {
      ok: !!j.ok && !j.isError,
      text: j.text != null && j.text !== "" ? j.text : (j.error || tr("runHistory.output")),
      ms: j.ms,
    };
  } catch (e) {
    d.run.result = { ok: false, text: tr("runHistory.requestFailed"), ms: 0 };
  }
  d.run.running = false;
  renderRunResult();
  // The run just landed in the log — make it the dropdown's newest entry. histTool=null forces the
  // refetch; renderHistoryOnly swaps the options without touching the form the args sit in.
  d.run.histTool = null;
  void loadRunHistory(d.name, d.run.tool);
}

/** Update only the output and the meta line — never the form, so the SQL you typed survives a run. */
function renderRunResult(): void {
  const d = mcpDetail();
  const running = !!(d && d.run.running);
  const btn = $<HTMLButtonElement>("runBtn");
  // Also called right after a pane rebuild, which may land mid-run — keep the button honest.
  if (btn) { btn.disabled = running; btn.textContent = running ? tr("runHistory.running") : tr("runHistory.run"); }
  const out = $("runOut"), meta = $("runMeta");
  if (!out || !d) return;
  const res = d.run.result as McpRunResult | null;
  if (!res) { fill(out); if (meta) meta.textContent = ""; return; }
  // The reply as Logs shows one (docs/46 §3.2); the size in the meta line is the real (compact)
  // reply, not the formatted text.
  fill(out, valueBlock({ label: res.ok ? tr("runHistory.result") : tr("runHistory.error") }, ...readableBody(res.text, !res.ok)));
  if (meta) {
    const bytes = new TextEncoder().encode(res.text).length;
    // ms reuses runHistory.durationMs; B/KB are unit format strings, not copy (docs/38
    // §2.1) - both hoisted so the write itself carries only translated text and the
    // "  ·  " separators.
    const ms = res.ms != null ? tr("runHistory.durationMs", { ms: res.ms }) : "";
    const size = bytes < 1024 ? bytes + " B" : (bytes / 1024).toFixed(1) + " KB";
    meta.textContent = (res.ok ? tr("runHistory.ok") : tr("runHistory.error2")) + "  ·  " + ms + "  ·  " + size;
  }
}

function configFieldLabel(type: string, key: string): string {
  if (key === "type") return tr("runHistory.type");
  if (key === "lazy") return tr("runHistory.startup");
  const fields = TYPE_FIELDS[type] || [];
  const found = fields.find((f) => { return f.k === key || (key === "lazy" && f.k === "autostart"); });
  // tk() keys resolve at paint time, never at table time (L7) - tr() them here.
  return found ? tr(found.label) : key;
}

function configValue(key: string, value: unknown): string {
  if (key === "lazy") return value ? tr("runHistory.demand") : tr("runHistory.boot");
  if (typeof value === "boolean") return value ? tr("runHistory.text") : tr("runHistory.off");
  if (typeof value === "object") return Array.isArray(value) ? JSON.stringify(value) : envToText(value as Record<string, string>);
  return String(value);
}

function configTarget(c: McpConfigLike): string {
  const type = c.type || "proc";
  if (type === "mysql" || type === "mariadb" || type === "redis") {
    const host = c.host || tr("runHistory.defaultHost");
    const target = host + (c.port != null ? ":" + c.port : "");
    const scope = type === "redis" ? c.db : c.database;
    return scope != null && scope !== "" ? target + " / " + scope : target;
  }
  if (type === "proc") return c.command || tr("runHistory.commandConfigured");
  if (type === "pg" || type === "http") return c.url || tr("runHistory.endpointConfigured");
  if (type === "rest") return c.baseUrl || tr("runHistory.baseUrlConfigured");
  if (type === "figma") return "https://mcp.figma.com/mcp";
  if (type === "zai-vision") return c.baseUrl || tr("runHistory.modeEndpoint", { mode: c.mode || "ZHIPU" });
  return c.url || c.baseUrl || c.host || tr("runHistory.connectionTargetConfigured");
}

function configBodyNode(d: McpDetail): HChild {
  if (d.editing) {
    const type = d.editType || (d.config && d.config.type) as string || "proc";
    // Stored values for the MCP's own type, nothing for a different one — then whatever the user has
    // already typed on top, so a type switch or a rejected save doesn't empty the form.
    const vals = Object.assign({}, type === ((d.config && d.config.type) || "proc") ? d.config : {}, d.editVals || {});
    // docs/30: a pg def stores one url; the form edits the pieces. Split it here (once, the same
    // parser the submit path mirrors) — an unparseable url falls back to a raw field and rides
    // through save untouched (__pgRaw), because old experience beats lost data.
    if (type === "pg" && vals.url !== undefined) {
      const pgParts = parsePgUrl(vals.url as string) as PgUrlParts & Record<string, string>;
      if (pgParts) {
        delete vals.url;
        Object.keys(pgParts).forEach((k) => { if (vals[k] === undefined) vals[k] = pgParts[k]; });
      } else {
        vals.__pgRaw = vals.url;
        delete vals.url;
      }
    }
    // The stored def carries `lazy`; the form shows its inverse as "Start automatically". A stored
    // def with no lazy at all gets the type's default (proc off, everything else on).
    if (vals.lazy !== undefined) vals.autostart = !vals.lazy;
    else if (vals.autostart === undefined) vals.autostart = type !== "proc";
    const replacing = d.editMode === "replace";
    const pgRaw = type === "pg" && vals.__pgRaw !== undefined
      ? field({
          label: tr("runHistory.connectionUrl"),
          control: h("textarea", { id: "e-pgraw", rows: 2 }, String(vals.__pgRaw)),
          hint: [tr("runHistory.wholeValueRefUrl"), tr("runHistory.fillFieldsAboveReplace")],
        })
      : null;
    return card(form(
      field({
        label: tr("runHistory.type"),
        control: h("select", { id: "e-type" },
          Object.keys(TYPE_FIELDS).map((t) => {
            return h("option", { value: t, selected: t === type }, tr(TYPE_LABELS[t] || t));
          })),
      }),
      fieldsNode(type, vals, "e-"),
      pgRaw,
      replacing
        ? field({ label: tr("runHistory.noteKeptParkedRevision"), control: h("input", { id: "e-note", placeholder: tr("runHistory.optionalEGProc") }) })
        : null,
      formActions(
        btn(replacing ? tr("runHistory.replaceDefinition") : tr("runHistory.saveRestart"), { kind: "primary", id: "e-save" }),
        TESTABLE_TYPES.includes(type) ? btn(tr("runHistory.testConnection"), { id: "e-test" }) : null,
        btn(tr("runHistory.cancel"), { id: "e-cancel" })),
      hint(null, { id: "e-test-out", hidden: true })));
  }
  const c = d.config;
  if (!c) return note(tr("runHistory.loading2"), { busy: true });
  const type = c.type as string || "proc";
  const settings = Object.keys(c).reduce((all, key) => {
    const value = c?.[key];
    if (value == null || value === "") return all;
    // Mono for a value you would copy (a host, a URL, a command); the description and the words
    // this panel translates (At boot, On / Off) stay sans (rule 1).
    const words = key === "description" || key === "lazy" || typeof value === "boolean";
    all.push({ key: key, label: configFieldLabel(type, key), text: configValue(key, value), mono: !words });
    return all;
  }, [] as { key: string; label: string; text: string; mono: boolean }[]);
  // Every stored setting as a label / value pair.
  const rows = settings.map((setting) => {
    return kvRow(setting.label, setting.text, { mono: setting.mono, title: setting.text.replace(/\r?\n/g, " · ") });
  });
  /* The adapter descriptors are "kind — detail" in English and "kind——detail" in Chinese;
   * split on whichever separator this locale's copy carries, so the detail tail reads
   * correctly in both (a label with no separator keeps the generic kind). */
  const label = tr(TYPE_LABELS[type] || type);
  const enDash = label.indexOf(" — ");
  const zhDash = label.indexOf("——");
  const kind = enDash >= 0 ? label.slice(enDash + 3)
    : zhDash >= 0 ? label.slice(zhDash + 2)
    : tr("runHistory.mcpAdapter");
  const target = configTarget(c as McpConfigLike);
  const badges: string[] = [];
  if (c.lazy !== undefined) badges.push(c.lazy ? tr("runHistory.startsDemand") : tr("runHistory.startsBoot"));
  if (c.exposeResources !== undefined) badges.push(c.exposeResources ? tr("runHistory.resources") : tr("runHistory.resourcesOff"));
  if (c.exposePrompts !== undefined) badges.push(c.exposePrompts ? tr("runHistory.prompts") : tr("runHistory.promptsOff"));
  if (c.auth === "oauth") badges.push(tr("runHistory.oauthManaged"));
  // The card holds the page's own summary layout (views.css .config-*); .config-summary is its hook.
  const summary = card(h("div", { class: "config-summary" },
    h("div", { class: "config-head" },
      h("div", { class: "config-identity" },
        h("div", { class: "config-kind" },
          tag(type, { mono: true }),
          h("span", null, kind)),
        h("div", { class: "config-target", title: target }, target)),
      btn(tr("runHistory.editConfiguration"), { id: "c-edit" })),
    badges.length ? h("div", { class: "config-badges" }, badges.map((badge) => { return tag(badge); })) : null,
    h("details", { class: "config-more" },
      h("summary", null,
        h("span", null, tr("runHistory.allSettings")),
        h("span", { class: "config-count" }, trn(settings.length, "runHistory.nValues.one", "runHistory.nValues.other")),
        h("span", { class: "config-chev" }, iconNode("chevron-right"))),
      h("div", { class: "config-rows" }, rows))));
  const fromFile = d.source === "config"
    ? note([tr("runHistory.definedGatewayConfigJson"), h("code", null, "${ENV}"), tr("runHistory.referencesKeptReferencesCredential")])
    : null;
  const revs = d.revisions || [];
  // Parked definition snapshots stay available without making an empty shelf a permanent section.
  // One row per parked definition: its adapter, its note and when, Restore - and the delete as
  // the row's trailing glyph, never a second red word button beside Restore (rule 4).
  const revRows: HChild = revs.length
    ? card(revs.map((r, i) => {
        const when = r.at ? new Date(r.at).toLocaleString(locale()) : "";
        return row({
          name: tr(TYPE_LABELS[r.type] || r.type || "?"),
          sub: (r.note || tr("runHistory.note")) + (when ? " · " + when : ""),
          primary: btn(tr("runHistory.restore"), { data: { restore: i } }),
          more: iconBtn("trash", tr("runHistory.delete"), { ghost: true, data: { revdel: i } }),
        });
      }))
    : note(tr("runHistory.noneReplacingDefinitionParks"));
  const revBlock = h("details", { class: "config-revisions" },
    h("summary", null,
      h("span", null, tr("runHistory.savedRevisionsN", { n: revs.length })),
      h("span", { class: "config-chev" }, iconNode("chevron-right"))),
    h("div", { class: "config-revision-body" },
      h("div", { class: "config-revision-intro" },
        h("span", null, tr("runHistory.swapAdapterDefinitionWhile")),
        btn(tr("runHistory.replaceDefinition2"), { id: "c-replace" })),
      revRows));
  // Its own section, so a Depends-on section above sits a full step away (.sec + .sec).
  return frag(tunnelDepsNode(d), section({}, summary, fromFile, revBlock));
}

/**
 * The tunnels this MCP's traffic goes through. When a health check fails, the answer to "is it the
 * tunnel or the database?" belongs on the same screen as the failure.
 *
 * The stale-pool line is information only, by explicit decision: a reconnect after this MCP started
 * means its pool may hold sockets of the old SSH session, and the fix is the Restart button that is
 * already there. Nothing restarts an MCP on its own.
 */
function tunnelDepsNode(d: McpDetail): HChild {
  const t = d.tunnels;
  if (!t || !t.length) return null;
  const rows = t.map((x) => {
    const state: DotState = x.state === "up" ? "up" : x.state === "error" ? "error" : x.state === "reconnecting" ? "starting" : "off";
    return row({
      lead: dot(state, x.state),
      name: x.name,
      // The forward is a value you would type; the state and its reason are words.
      sub: [h("code", null, String(x.localPort) + " → " + x.targetHost + ":" + x.targetPort), " · " + x.state + (x.reason ? " — " + x.reason : "")],
      // A pool that may hold sockets of the old SSH session is the one thing to act on here.
      err: x.stalePool ? tr("runHistory.reconnectedAfterMcpStarted") + tr("runHistory.useRestartAbove") : undefined,
    });
  });
  return section({ cap: tr("runHistory.depends") }, card(rows));
}

/** docs/33 C3: the one way a call row opens or closes, from a click or from Enter/Space. The
 *  operator opens a call to read its result, so a reply the page clipped to its 2 KB preview is
 *  fetched whole right away instead of waiting behind a Show full result click. */
function openOrCloseCall(seq: number): void {
  if (toggleCall(seq)) void showFullResult(seq);
}


/* --- pane-level delegation (docs/37 R5) --------------------------------------------------------
   wireTabBody used to walk the fresh tab body after every paint and assign ~34 handlers; each
   repaint re-attached them, and several handlers closed over the detail object the render
   happened under. The tab body's events now ride the ONE listener per event type that
   renderPane assigns on #pane (paneChromeClick handles the pane chrome; these four handle
   everything inside #tabbody). Dispatch happens at event time on the live state:

     - a click resolves mcpDetail() when it lands, so a poll that swapped the store between
       render and click can no longer fire a handler against an orphaned object;
     - the e-save verb follows the LIVE editMode, not the render-time one;
     - the calls-search debounce and its Escape clear resolve the detail at event time; the
       timer's name check still drops a late fire after the selection moved. */
function paneTabClick(ev: MouseEvent): void {
  const t = targetEl(ev);
  if (!t) return;
  // Config editing form
  if (t.id === "e-save") {
    const d = mcpDetail();
    if (d) void (d.editMode === "replace" ? saveReplace : saveEdit)();
    return;
  }
  if (t.id === "e-cancel") { cancelEdit(); return; }
  if (t.id === "e-test") { void runConnTest("e-"); return; }
  const restore = t.closest<HTMLElement>("[data-restore]");
  if (restore) { void restoreRevision(Number(restore.dataset.restore)); return; }
  const revdel = t.closest<HTMLElement>("[data-revdel]");
  if (revdel) { void deleteRevision(Number(revdel.dataset.revdel)); return; }
  // Config read view
  if (t.id === "c-edit") { startEdit(); return; }
  if (t.id === "c-replace") { startReplace(); return; }
  // Kind lists pager
  if (t.id === "pgPrev") { pagePrev(); return; }
  if (t.id === "pgNext") { pageNext(); return; }
  // Tools list -> Try, tool/resource toggles, resources -> Read
  const tryBtn = t.closest<HTMLElement>("[data-try]");
  if (tryBtn) { tryTool(String(tryBtn.dataset.try)); return; }
  const readBtn = t.closest<HTMLButtonElement>("[data-read]");
  if (readBtn) { void readResource(String(readBtn.dataset.read), readBtn); return; }
  const togBtn = t.closest<HTMLButtonElement>("[data-toggle]");
  if (togBtn) { void toggleTool(String(togBtn.dataset.toggle), togBtn.dataset.on === "1", togBtn); return; }
  const resTog = t.closest<HTMLButtonElement>("[data-restog]");
  if (resTog) { void toggleResources(resTog.dataset.restog === "1", resTog); return; }
  // Logs tab
  // docs/32 B4: Clear lives behind the toolbar's ellipsis menu — a destructive action does not
  // get a standing button in the filter row. The house popupMenu (menu.js) carries the item.
  const clMenuBtn = t.closest<HTMLElement>("#clMenu");
  if (clMenuBtn) {
    const clMenu = clMenuBtn;
    ev.stopPropagation();
    // The house toggle idiom (pane.js toggleMenu): a second click dismisses instead of reopening.
    if (menuOpen()) { closeMenu(); return; }
    popupMenu(clMenu.getBoundingClientRect(), [
      { label: tr("runHistory.clearLogs"), danger: true, fn: (): void => { void clearCalls(); } },
    ]);
    return;
  }
  // docs/32 B2: a keyboard activation (Enter/Space on a focused button) carries detail === 0 —
  // that action owes the user focus back on the equivalent button once the switch commits.
  if (t.id === "clPrev") { callsPageStep(-1, { fromKey: ev.detail === 0 }); return; }
  if (t.id === "clNext") { callsPageStep(1, { fromKey: ev.detail === 0 }); return; }
  if (t.id === "clRetry") { callsRetry({ fromKey: ev.detail === 0 }); return; }
  // docs/33 C3: a block's Copy (formatted: the decoded structure as valid JSON) and Copy raw
  // (the text exactly as stored). Both read the CALL ROW, not the painted DOM, so a block cut at
  // its line cap still copies everything. docs/46 §3.2: Copy is the block's one visible button;
  // Copy raw is the one item behind its ⋯.
  const copyBtn = t.closest<HTMLElement>("[data-copy]");
  if (copyBtn) {
    const d = mcpDetail();
    const text = d ? callBlockCopyText(d, String(copyBtn.dataset.copy), false) : null;
    if (text != null) void copyLogText(text);
    return;
  }
  const blkMore = t.closest<HTMLElement>("[data-blkmore]");
  if (blkMore) {
    ev.stopPropagation();
    if (menuOpen()) { closeMenu(); return; }
    const key = String(blkMore.dataset.blkmore);
    popupMenu(blkMore.getBoundingClientRect(), [{
      label: tr("logs.copyRaw"),
      title: key.startsWith("args:") ? tr("logs.copyArgumentsRaw") : tr("logs.copyResultRaw"),
      fn: (): void => {
        // Resolved at click time, like every other verb here: the log may have moved meanwhile.
        const d = mcpDetail();
        const text = d ? callBlockCopyText(d, key, true) : null;
        if (text != null) void copyLogText(text);
      },
    }]);
    return;
  }
  // docs/33 C3: Show all lifts one block's line cap; the choice is state, so a poll repaint of
  // the list keeps the block whole.
  const showAll = t.closest<HTMLElement>("[data-showall]");
  if (showAll) {
    const d = mcpDetail();
    if (!d) return;
    const key = String(showAll.dataset.showall);
    if (!d.callsAll) d.callsAll = {};
    d.callsAll[key] = true;
    repaintCallBlock(d, key);
    return;
  }
  // The full-result button sits inside an expandable row — it must not also toggle the row.
  const fullBtn = t.closest<HTMLElement>("[data-full]");
  if (fullBtn) { ev.stopPropagation(); void showFullResult(Number(fullBtn.dataset.full)); return; }
  // A call row's summary is a real <button> (ui/timeline.ts), so Enter and Space arrive here as
  // clicks too. Only the summary toggles: a click inside the open body is someone reading it.
  const callSum = t.closest<HTMLElement>(".tl-sum");
  const callRow = callSum ? callSum.closest<HTMLElement>("[data-callseq]") : null;
  if (callRow) { openOrCloseCall(Number(callRow.dataset.callseq)); return; }
  // Run tab
  if (t.id === "runBtn") { void runTool(); return; }
  if (t.id === "r-hist") { histToggle(); return; }
}

function paneTabChange(ev: Event): void {
  const t = targetEl(ev);
  if (!t) return;
  // Selects climb too (docs/37 R5): a change event's target is the select itself today, but
  // matching by closest keeps the dispatcher's one idiom instead of two.
  const eType = t.closest<HTMLSelectElement>("#e-type");
  if (eType) { changeEditType(eType.value); return; }
  const rTool = t.closest<HTMLSelectElement>("#r-tool");
  if (rTool) {
    const d = mcpDetail();
    if (!d) return;
    d.run.tool = rTool.value;
    d.run.result = null;
    renderPane();
  }
}

/** The calls search box (docs/31): debounced server-side reload. The input node itself carries
 *  no handler — this dispatcher sees every keystroke from #pane, and renderCallsOnly's swap-back
 *  of the live #callsQ node needs no re-wiring to keep working. */
function paneTabInput(ev: Event): void {
  const t = targetEl(ev);
  if (!t || t.id !== "callsQ") return;
  // One stable id owns exactly one element - the calls search input. Narrowed here the
  // same way every closest<HTMLInputElement> in the tree narrows; no Element classes
  // exist to instanceof against in the Node-boot environment this dispatcher is tested in.
  const q = t as HTMLInputElement;
  const d = mcpDetail();
  if (!d) return;
  clearTimeout(d.callsQTimer);
  d.callsQTimer = setTimeout(() => {
    const nd = mcpDetail();
    if (!nd || nd.name !== d.name) return;
    // docs/32 B3: a new needle supersedes any switch in flight — target page 0, the
    // pending switch cancelled, its error taken down.
    nd.callsQ = q.value;
    nd.callsPage = 0;
    nd.callsPendingPage = null;
    nd.callsSwitch = null;
    nd.callsError = "";
    nd.callsErrStatus = "";
    nd.calls = null; // the loading state, not the previous needle's rows
    void loadCalls(nd.name);
  }, 300);
}

function paneTabKeydown(ev: KeyboardEvent): void {
  const t = targetEl(ev);
  if (!t) return;
  if (t.id === "callsQ") {
    // Escape clears at once (docs/31): no debounce, no wait. #callsQ is the calls
    // search input - see paneTabInput for why the narrow is an id, not instanceof.
    const q = t as HTMLInputElement;
    if (ev.key !== "Escape" || !q.value) return;
    const d = mcpDetail();
    if (!d) return;
    ev.preventDefault();
    clearTimeout(d.callsQTimer);
    q.value = "";
    d.callsQ = "";
    d.callsPage = 0;
    d.callsPendingPage = null;
    d.callsSwitch = null;
    d.callsError = "";
    d.callsErrStatus = "";
    d.calls = null;
    void loadCalls(d.name);
    return;
  }
  // Ctrl/Cmd+Enter runs, so a SQL textarea can be submitted without reaching for the mouse.
  // #runBtn only exists on the Run tab, which is the scope the per-field listener had.
  // The field test is duck-typed (tagName/type) so stub targets keep answering too.
  if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter" && $("runBtn")
    && (t.tagName === "TEXTAREA" || (t as HTMLInputElement).type === "text")) {
    ev.preventDefault();
    void runTool();
  }
}

/** The non-wiring half of the retired wireTabBody: follow-ups a paint owes, in paint order.
 *  Called by renderPane after #tabbody is filled. */
function afterTabPaint(d: McpDetail): void {
  // A pane rebuild drops the popover with the old markup it lived in. If it was open, bring it
  // back over the fresh button; the previewed entry (histSelSeq) is re-shown by histPreview.
  if (d.run.histOpen) {
    d.run.histOpen = false; // histOpen() would no-op behind its own histClose guard
    histOpen();
  }
  // Loading here rather than in showTab: the tool the Run tab will show is only settled once the
  // form is built (runBodyNode falls back to the first tool when none was preselected via Try).
  if (d.tab === "run" && d.run.tool) void loadRunHistory(d.name, d.run.tool);
  if ($("runOut")) renderRunResult();
}

/** Jump from the Tools list into Run with that tool preselected. */
function tryTool(name: string): void {
  const d = mcpDetail();
  if (!d) return;
  d.run.tool = name;
  d.run.result = null;
  showTab("run");
}

/** Toggle all of an MCP's resources on/off. Live: the server answers an empty list while off and
 *  pushes notifications/resources/list_changed, so this just fires the change and reloads. */
async function toggleResources(currentlyOn: boolean, btn: HTMLButtonElement): Promise<void> {
  const sel = selectedMcp();
  if (!sel) return;
  btn.disabled = true;
  const j = await apiJson<{ unchanged?: boolean }>("/api/mcps/" + encodeURIComponent(sel) + "/resources-toggle", {
    method: "POST",
    body: JSON.stringify({ enabled: !currentlyOn }),
  });
  btn.disabled = false;
  if (!j) return;
  if (j.unchanged) { toast(currentlyOn ? tr("runHistory.already") : tr("runHistory.alreadyOff")); return; }
  const d = mcpDetail();
  if (d) { d.resources.loaded = false; d.resources.cursors = []; void loadPage(d.name, "resources"); }
}

/** Toggle a tool on/off for clients. Live: the server filters the next tools/list and pushes
 *  notifications/tools/list_changed, so this just fires the change and reloads the list. */
async function toggleTool(name: string, currentlyOn: boolean, btn: HTMLButtonElement): Promise<void> {
  const sel = selectedMcp();
  if (!sel) return;
  btn.disabled = true;
  const j = await apiJson<{ unchanged?: boolean }>("/api/mcps/" + encodeURIComponent(sel) + "/tools/" + encodeURIComponent(name), {
    method: "POST",
    body: JSON.stringify({ enabled: !currentlyOn }),
  });
  btn.disabled = false;
  if (!j) return;
  if (j.unchanged) { toast(currentlyOn ? tr("runHistory.already") : tr("runHistory.alreadyOff")); return; }
  // Reload the tools page so the row moves between the enabled list and the disabled group.
  const d = mcpDetail();
  if (d) { d.tools.loaded = false; d.tools.cursors = []; void loadPage(d.name, "tools"); }
}

/** Read one resource and show its contents. Read-only, so a sheet with no form is the whole UI. */
async function readResource(uri: string, trigger: HTMLButtonElement | null): Promise<void> {
  const sel = selectedMcp();
  if (!sel) return;
  const label = trigger ? trigger.textContent : "";
  if (trigger) { trigger.disabled = true; trigger.textContent = "…"; }
  const j = await apiJson<ApiMcpResourceRead>("/api/mcps/" + encodeURIComponent(sel) + "/resource", {
    method: "POST",
    body: JSON.stringify({ uri: uri }),
  });
  if (trigger) { trigger.disabled = false; trigger.textContent = label || tr("runHistory.read"); }
  if (!j) return;
  // hidden BEFORE the content (the house sheet idiom, panel-proof-of-life rule 1). The
  // resource text is a text node - a resource body is data, never markup.
  // The library sheet (showSheet unhides the host before it fills it) and the panel's one code
  // block: a resource is usually JSON (a schema overview, a table's columns) and reads like a reply.
  showSheet(sheet({
    title: uri, label: tr("runHistory.resourceContents"),
    body: [
      j.ok
        ? note(tr("runHistory.resourceMeta", { type: j.mimeType || "text/plain", ms: j.ms ?? 0 }))
        : note(tr("runHistory.readFailed"), { err: true }),
      h("div", { class: "rd-body" }, readableBody(j.text || "", !j.ok)),
    ],
    foot: btn(tr("runHistory.close"), { id: "rd-close" }),
  }));
  $("rd-close").onclick = closeSheet;
}

/** Repaint only the Logs tab body, so a poll doesn't rebuild the pane (or the header, or the menu).
 *  Expanded rows survive because `callsOpen` is state, not DOM. */
function renderCallsOnly(): void {
  const d = mcpDetail();
  if (!d || d.tab !== "logs") return;
  const body = $("tabbody");
  if (!body) return;
  // Repaint only when the log actually changed — otherwise a 6 s poll would scroll an open result
  // back to the top while it is being read. The needle counts too (docs/31): a cleared or changed
  // search must repaint even when the row count happens to stay the same.
  const calls = d.calls || [];
  // The page, a pending switch and the error state are part of the picture now (docs/32 B1):
  // a commit or a failure must repaint even when the row count happens to stay the same.
  const sig = (calls.length ? calls[0].seq : 0) + ":" + calls.length + ":" + d.stderr.length + ":" + (d.callsQ || "") +
    ":p" + d.callsPage + ":w" + (d.callsPendingPage == null ? "-" : d.callsPendingPage) + ":e" + (d.callsError || "");
  if (body.dataset.callsig === sig) return;
  // Keep the search box the user is typing into: the fresh markup carries a rebuilt input, and the
  // live node (focus, caret, IME state) is swapped back into its place. innerHTML detaching the
  // old node blurs it — focus and caret are restored explicitly after the swap, or the next
  // keystroke after a result repaint would land nowhere.
  const liveQ = document.getElementById("callsQ") as HTMLInputElement & { selectionStart: number };
  const hadFocus = !!(liveQ && document.activeElement === liveQ);
  const caret = hadFocus ? liveQ.selectionStart : null;
  fill(body, logsBodyNode(d));
  const freshQ = document.getElementById("callsQ");
  if (liveQ && freshQ && liveQ !== freshQ) freshQ.replaceWith(liveQ);
  if (hadFocus && liveQ) {
    liveQ.focus();
    try { liveQ.setSelectionRange(caret, caret); } catch (err) { /* type=search supports it; guard anyway */ }
  }
  body.dataset.callsig = sig;
  // No re-wiring here (docs/37 R5): the pager, the copy buttons and the search box answer
  // through #pane's delegated listeners, which a body repaint never disturbs. The swap-back
  // of the live #callsQ node needs no re-attached oninput for the same reason.
}

export { applyRunHistory, configBodyNode, fillRunArgs, histButtonLabel, histClose, histOpen, histPreview, histRowsNode, histSearchTimer, histToggle, histViewNode, histWhen, loadRunHistory, queueHistSearch, readResource, renderCallsOnly, renderHistoryOnly, renderRunResult, runTool, toggleResources, toggleTool, tryTool, tunnelDepsNode, paneTabClick, paneTabChange, paneTabInput, paneTabKeydown, afterTabPaint };
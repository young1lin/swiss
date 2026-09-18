/*
 * Copyright 2026 The swiss authors
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
import type { ApiMcpResourceRead, McpConfigLike, McpRevisionRow, McpRunResult, McpTunnelDepRow } from "./types/runs.js";
import type { McpDetail } from "./types/state.js";
import { $, api, apiJson, errText, esc, icon, state, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { callsPageStep, callsRetry, cancelEdit, changeEditType, clearCalls, deleteRevision, loadCalls, loadPage, pageNext, pagePrev, restoreRevision, runConnTest, saveEdit, saveReplace, showFullResult, showTab, startEdit, startReplace } from "./detail.js";
import { TESTABLE_TYPES, TYPE_FIELDS, TYPE_LABELS, envToText, fieldsHtml, parsePgUrl } from "./fields.js";
import { popupMenu } from "./menu.js";
import { closeMenu } from "./pane.js";
import { copyLogText, fmtChars, fmtJson, logsBody, mountJsonTrees, toggleCall } from "./logs.js";
import { renderPane } from "./pane.js";
import { readRunArgs } from "./run.js";
import { ago } from "./traffic.js";
import { menuIsOpen } from "./ui-state.js";

/* --- Run history: the refill control in the actions row ------------------------------------------ */
/** When an entry ran. Reuses ago() inside a day; past that, ago's time-of-day would be ambiguous,
 *  so the date comes back too. */
function histWhen(iso: string): string {
  return Date.now() - new Date(iso).getTime() < 86400000 ? ago(iso) : new Date(iso).toLocaleString();
}

/** The control's closed label. "↺ Past runs (12)" says what it opens and how much is in it, so the
 *  closed control explains itself without looking like a form value. */
function histButtonLabel(d: McpDetail, tool: string): string {
  if (d.run.histTool !== tool) return "↺ Past runs…"; // never loaded, or a fetch in flight
  const n = (d.run.hist || []).length;
  return n ? "↺ Past runs (" + n + ")" : "↺ No past runs";
}

/** The popover's left pane: one button per DISTINCT argument set, newest first, narrowed to the
 *  runs whose FULL recorded arguments contain the search box's query (the server does that matching —
 *  the 96-char row label would miss a keyword deeper in). The row label is the clipped one-line
 *  preview — the full text is what the right pane is for. */
function histRowsHtml(d: McpDetail, tool: string): string {
  if (d.run.histTool !== tool) return '<div class="hist-empty">loading…</div>';
  const list = d.run.hist as unknown as ApiMcpCallRow[] || [];
  if (!list.length) {
    return d.run.histQ
      ? '<div class="hist-empty">No runs whose arguments contain ' + esc('"' + d.run.histQ + '"') + ".</div>"
      : '<div class="hist-empty">No runs of this tool recorded yet.</div>';
  }
  return list.map((h) => {
    return '<button type="button" class="hist-row" data-seq="' + h.seq + '" title="' +
      esc(h.args || "(no arguments)") + '">' + esc(h.args || "(no arguments)") + "</button>";
  }).join("");
}

/** One hovered entry rendered in full: the meta line, the COMPLETE arguments (the row label is
 *  clipped at 96 chars — this is the answer to "let me read the whole thing"), and the reply that
 *  run produced (the stored preview, so a 1 MB reply never loads on a hover). */
function histViewHtml(c: ApiMcpCallRow): string {
  // The identity line: status dot (the panel's universal up/down mark), when, who, how long —
  // not prose glued with dots, so each fact keeps its own weight and nothing runs together.
  const head = '<div class="hist-head">' +
    '<span class="dot ' + (c.ok ? "up" : "down") + '"></span>' +
    "<span>" + esc(histWhen(c.at)) + "</span>" +
    "<span>·</span>" +
    "<span>" + esc(c.via + (c.client ? " (" + c.client + ")" : "")) + "</span>" +
    '<span class="grow"></span>' +
    (c.ms != null ? "<span>" + esc(c.ms + " ms") + "</span>" : "") +
    "</div>";
  let out = head +
    '<div class="call-lbl">Arguments</div><pre class="logs">' +
    esc(fmtJson(c.args) || "(none)") + "</pre>";
  out += '<div class="call-lbl">' + (c.ok ? "Result" : "Error") + "</div>" +
    '<pre class="logs' + (c.ok ? "" : " err") + '">' + esc(fmtJson(c.output) || "(empty)") + "</pre>";
  if (c.preview) {
    out += '<div class="hist-note">Reply shown is its first ' + esc(fmtChars(c.chars)) +
      ' — the full reply lives in the Logs tab.</div>';
  }
  return out;
}

/** Open/close the popover. Opening does not steal focus from the form — the rows are reachable
 *  with the mouse, or with Tab once the control itself has focus. */
function histToggle(): void {
  const d = state.detail;
  if (!d || d.tab !== "run" || !d.run.tool) return;
  d.run.histOpen ? histClose() : histOpen();
}
function histOpen(): void {
  const d = state.detail;
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
  // typing — only #r-hist-list's innerHTML is swapped (renderHistoryOnly), so focus survives.
  pop.innerHTML = '<div class="hist-side">' +
      '<div class="hist-search"><input id="r-hist-q" type="search" placeholder="Filter by arguments…" ' +
        'aria-label="Filter past runs by their arguments" autocomplete="off" spellcheck="false"></div>' +
      '<div class="hist-list" id="r-hist-list">' + histRowsHtml(d, d.run.tool) + "</div>" +
    "</div>" +
    '<div class="hist-view" id="r-hist-view">' +
    '<div class="hist-empty">Hover a run to see it in full — the arguments and the reply it produced.</div>' +
    "</div>";
  document.body.appendChild(pop);
  wireHistRows();
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
  const d = state.detail;
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
    if (state.detail !== d || !d.run.histOpen) return;
    d.run.histTool = null; // the loaded list answers the old query — force the refetch
    void loadRunHistory(d.name, d.run.tool);
  }, 200);
}

/** Hover/focus a row: show that run in full in the right pane. Entries are cached by seq, and the
 *  seq guard drops a reply that lost the race to a newer hover. */
async function histPreview(seq: number): Promise<void> {
  const d = state.detail;
  const view = $("r-hist-view");
  if (!d || d.tab !== "run" || !d.run.histOpen || !view) return;
  d.run.histSelSeq = seq;
  let full = d.run.histFull[seq];
  if (!full) {
    view.innerHTML = '<div class="hist-empty"><span class="spin"></span> loading…</div>';
    try {
      const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls/" + encodeURIComponent(seq));
      if (!r.ok) throw new Error("HTTP " + r.status);
      const j = await r.json();
      if (!j.call || state.detail !== d) return;
      full = d.run.histFull[seq] = j.call;
    } catch (e) {
      if (d.run.histSelSeq === seq) view.innerHTML = '<div class="hist-empty">could not load that run</div>';
      return;
    }
  }
  if (d.run.histSelSeq !== seq) return; // a newer hover already won
  view.innerHTML = histViewHtml(full as ApiMcpCallRow);
}

/** Load the selected tool's newest DISTINCT runs — the full list when the search box is empty, the
 *  runs whose arguments contain d.run.histQ when it is not. Idempotent per tool: a repaint that
 *  re-wires the tab does not refetch. Set d.run.histTool = null first to force a refresh (right
 *  after a run, or on each debounced keystroke in the filter). */
async function loadRunHistory(name: string, tool: string | null): Promise<void> {
  const d = state.detail;
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
      if (state.detail === d && d.run.histQ === q) {
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
    if (state.detail === d && d.run.histQ !== q && !d.run.histTool) {
      void loadRunHistory(d.name, d.run.tool);
    }
  }
}

/** Repaint only the control's label and (if open) the popover's rows, never the form around it —
 *  the same discipline as renderRunResult, so the SQL being edited survives a post-run refresh. */
function renderHistoryOnly(): void {
  const d = state.detail;
  if (!d || d.tab !== "run" || !d.run.tool) return;
  const btn = $<HTMLButtonElement>("r-hist");
  if (btn) {
    btn.textContent = histButtonLabel(d, d.run.tool);
    // "No history at all" disables the control — but a filter that matched nothing must not: the
    // popover is mid-search, its empty state is the answer, and slamming it shut would eat the query.
    btn.disabled = !d.run.histQ && d.run.histTool === d.run.tool && !(d.run.hist || []).length;
    if (btn.disabled && d.run.histOpen) histClose();
  }
  const list = $("r-hist-list");
  if (list && d.run.histOpen) {
    list.innerHTML = histRowsHtml(d, d.run.tool);
    wireHistRows();
    if (d.run.histSelSeq != null) void histPreview(d.run.histSelSeq); // keep what was being read
  }
}

/** A run was picked: fetch its full arguments by seq, then write them into the form. */
async function applyRunHistory(seq: number): Promise<void> {
  const d = state.detail;
  if (!d || !d.run.tool) return;
  const tools = d.tools.items || [];
  let toolDef = null as ApiMcpTool | null;
  for (let i = 0; i < tools.length; i++) if (tools[i].name === d.run.tool) toolDef = tools[i] as ApiMcpTool;
  if (!toolDef) return;
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/calls/" + encodeURIComponent(seq));
    if (!r.ok) { toast("HTTP " + r.status, true); return; }
    const j = await r.json();
    if (!j.call) return;
    d.run.histFull[seq] = j.call; // the pick just fetched it — a hover later is free
    // Arguments over 4 KB were stored clipped (argsText's overflow marker), so they are no longer
    // parseable JSON — say that precisely instead of the generic read failure.
    if (j.call.args && j.call.args.indexOf("… +") >= 0 && j.call.args.endsWith("more characters")) {
      toast("that run's arguments were too long to store in full — copy them from the Logs tab", true);
      return;
    }
    fillRunArgs(toolDef, JSON.parse(j.call.args || "{}") || {});
  } catch (e) { toast("that run's arguments could not be read", true); }
  histClose();
}

/** Bind the popover's rows: hover/focus previews in full, click refills the form and closes. */
function wireHistRows(): void {
  // NOTE: the popover lives on <body> (histOpen), not inside #tabbody — selecting from #tabbody
  // here is why hover never fired: the rows existed, the wiring found none of them.
  document.querySelectorAll<HTMLElement>("#r-hist-pop .hist-row").forEach((row) => {
    row.onclick = () => { void applyRunHistory(Number(row.dataset.seq)); };
    row.onmouseenter = () => { void histPreview(Number(row.dataset.seq)); };
    row.onfocus = () => { void histPreview(Number(row.dataset.seq)); };
  });
}

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
  const d = state.detail;
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
  if (btn) { btn.disabled = true; btn.textContent = "Running…"; }
  const meta = $("runMeta");
  if (meta) meta.innerHTML = '<span class="spin"></span>';
  try {
    const r = await api("/api/mcps/" + encodeURIComponent(d.name) + "/call", {
      method: "POST", body: JSON.stringify({ tool: d.run.tool, arguments: args }),
    });
    const j = await r.json().catch(() => { return {}; });
    d.run.result = {
      ok: !!j.ok && !j.isError,
      text: j.text != null && j.text !== "" ? j.text : (j.error || "(no output)"),
      ms: j.ms,
    };
  } catch (e) {
    d.run.result = { ok: false, text: "request failed", ms: 0 };
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
  const d = state.detail;
  const running = !!(d && d.run.running);
  const btn = $<HTMLButtonElement>("runBtn");
  // Also called right after a pane rebuild, which may land mid-run — keep the button honest.
  if (btn) { btn.disabled = running; btn.textContent = running ? "Running…" : "Run"; }
  const out = $("runOut"), meta = $("runMeta");
  if (!out || !d) return;
  const res = d.run.result as McpRunResult | null;
  if (!res) { out.textContent = ""; if (meta) meta.textContent = ""; return; }
  // Formatted for reading; the size in the meta line is the real (compact) reply.
  out.textContent = fmtJson(res.text);
  out.className = "logs" + (res.ok ? "" : " err");
  if (meta) {
    const bytes = new TextEncoder().encode(res.text).length;
    meta.textContent = (res.ok ? "ok" : "error") + "  ·  " + (res.ms != null ? res.ms + " ms" : "") +
      "  ·  " + (bytes < 1024 ? bytes + " B" : (bytes / 1024).toFixed(1) + " KB");
  }
}

function configFieldLabel(type: string, key: string): string {
  if (key === "type") return "Type";
  if (key === "lazy") return "Startup";
  const fields = TYPE_FIELDS[type] || [];
  const found = fields.find((f) => { return f.k === key || (key === "lazy" && f.k === "autostart"); });
  return found ? found.label : key;
}

function configValue(key: string, value: unknown): string {
  if (key === "lazy") return value ? "On demand" : "At boot";
  if (typeof value === "boolean") return value ? "On" : "Off";
  if (typeof value === "object") return Array.isArray(value) ? JSON.stringify(value) : envToText(value as Record<string, string>);
  return String(value);
}

function configTarget(c: McpConfigLike): string {
  const type = c.type || "proc";
  if (type === "mysql" || type === "mariadb" || type === "redis") {
    const host = c.host || "default host";
    const target = host + (c.port != null ? ":" + c.port : "");
    const scope = type === "redis" ? c.db : c.database;
    return scope != null && scope !== "" ? target + " / " + scope : target;
  }
  if (type === "proc") return c.command || "Command not configured";
  if (type === "pg" || type === "http") return c.url || "Endpoint not configured";
  if (type === "rest") return c.baseUrl || "Base URL not configured";
  if (type === "figma") return "https://mcp.figma.com/mcp";
  if (type === "zai-vision") return c.baseUrl || (c.mode || "ZHIPU") + " endpoint";
  return c.url || c.baseUrl || c.host || "Connection target not configured";
}

function configBody(d: McpDetail): string {
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
    const opts = Object.keys(TYPE_FIELDS).map((t) => {
      return '<option value="' + t + '"' + (t === type ? " selected" : "") + ">" + esc(TYPE_LABELS[t] || t) + "</option>";
    }).join("");
    const replacing = d.editMode === "replace";
    const pgRaw = type === "pg" && vals.__pgRaw !== undefined
      ? '<label class="field"><span>Connection URL</span>' +
        '<textarea id="e-pgraw" rows="2">' + esc(vals.__pgRaw) + "</textarea>" +
        '<div class="hint">A whole-value \${...} ref (or a url that did not decompose) — it is kept whole.' +
        " Fill the fields above to replace it with a decomposed url.</div></label>"
      : "";
    return '<div class="group"><div class="form">' +
      '<label class="field"><span>Type</span><select id="e-type">' + opts + "</select></label>" +
      fieldsHtml(type, vals, "e-") +
      pgRaw +
      (replacing
        ? '<label class="field"><span>Note (kept with the parked revision)</span>' +
          '<input id="e-note" placeholder="optional — e.g. proc version, before the swap"></label>'
        : "") +
      '<div class="form-actions"><button class="btn primary" id="e-save">' +
        (replacing ? "Replace definition" : "Save &amp; Restart") + "</button>" +
      (TESTABLE_TYPES.indexOf(type) >= 0
        ? '<button class="btn" id="e-test">Test connection</button>'
        : "") +
      '<button class="btn" id="e-cancel">Cancel</button></div>' +
      '<div class="hint" id="e-test-out" hidden></div>' +
      "</div></div>";
  }
  const c = d.config;
  if (!c) return '<div class="note"><span class="spin"></span> Loading…</div>';
  const type = c.type as string || "proc";
  const settings = Object.keys(c).reduce((all, key) => {
    const value = c?.[key];
    if (value == null || value === "") return all;
    all.push({ key: key, label: configFieldLabel(type, key), text: configValue(key, value) });
    return all;
  }, [] as { key: string; label: string; text: string }[]);
  const rows = settings.map((setting) => {
    const title = setting.text.replace(/\r?\n/g, " · ");
    return '<div class="config-row"><span class="config-label">' + esc(setting.label) +
      '</span><span class="config-value" title="' + esc(title) + '">' + esc(setting.text) + "</span></div>";
  }).join("");
  const label = TYPE_LABELS[type] || type;
  const split = label.indexOf(" — ");
  const kind = split >= 0 ? label.slice(split + 3) : "MCP adapter";
  const target = configTarget(c as McpConfigLike);
  const badges = [];
  if (c.lazy !== undefined) badges.push(c.lazy ? "Starts on demand" : "Starts at boot");
  if (c.exposeResources !== undefined) badges.push(c.exposeResources ? "Resources on" : "Resources off");
  if (c.exposePrompts !== undefined) badges.push(c.exposePrompts ? "Prompts on" : "Prompts off");
  if (c.auth === "oauth") badges.push("OAuth managed");
  const badgeHtml = badges.length ? '<div class="config-badges">' + badges.map((badge) => {
    return '<span class="tag">' + esc(badge) + "</span>";
  }).join("") + "</div>" : "";
  const summary = '<div class="group config-summary">' +
    '<div class="config-head"><div class="config-identity">' +
      '<div class="config-kind"><span class="tag">' + esc(type) + "</span><span>" + esc(kind) + "</span></div>" +
      '<div class="config-target" title="' + esc(target) + '">' + esc(target) + "</div>" +
    '</div><button class="btn" id="c-edit">Edit configuration…</button></div>' +
    badgeHtml +
    '<details class="config-more"><summary><span>All settings</span><span class="config-count">' + settings.length +
      (settings.length === 1 ? ' value' : ' values') + '</span><span class="config-chev">' + icon("chevron-right") + "</span></summary>" +
      '<div class="config-rows">' + rows + "</div></details></div>";
  const note = d.source === "config"
    ? '<div class="note">Defined in gateway.config.json. Edits are saved as an override in managed.json; ' +
      "<code>${ENV}</code> references are kept as references, so no credential is written to disk.</div>"
    : "";
  const revs = d.revisions as McpRevisionRow[] || [];
  // Parked definition snapshots stay available without making an empty shelf a permanent section.
  const revRows = revs.length
    ? revs.map((r, i) => {
        const when = r.at ? new Date(r.at).toLocaleString() : "";
        return '<div class="row"><span class="k">' + esc(TYPE_LABELS[r.type] || r.type || "?") + "</span>" +
          '<span class="v wrap">' + esc((r.note || "(no note)") + (when ? " · " + when : "")) +
          ' <button class="btn" data-restore="' + i + '">Restore</button>' +
          ' <button class="btn danger" data-revdel="' + i + '">Delete</button></span></div>';
      }).join("")
    : '<div class="row"><span class="rowmsg">None yet — replacing a definition parks the outgoing settings here.</span></div>';
  const revBlock = '<details class="config-revisions"><summary><span>Saved revisions (' + revs.length + ")</span>" +
    '<span class="config-chev">' + icon("chevron-right") + "</span></summary>" +
    '<div class="config-revision-body"><div class="config-revision-intro">' +
      '<span>Swap the adapter definition while keeping the current one available for restore.</span>' +
      '<button class="btn" id="c-replace">Replace definition…</button></div>' +
      '<div class="group">' + revRows + "</div></div></details>";
  return tunnelDepsHtml(d) + summary + note + revBlock;
}

/**
 * The tunnels this MCP's traffic goes through. When a health check fails, the answer to "is it the
 * tunnel or the database?" belongs on the same screen as the failure.
 *
 * The stale-pool line is information only, by explicit decision: a reconnect after this MCP started
 * means its pool may hold sockets of the old SSH session, and the fix is the Restart button that is
 * already there. Nothing restarts an MCP on its own.
 */
function tunnelDepsHtml(d: McpDetail): string {
  const t = d.tunnels as McpTunnelDepRow[] | null | undefined;
  if (!t || !t.length) return "";
  const rows = t.map((x) => {
    const dot = x.state === "up" ? "up" : x.state === "error" ? "error" : x.state === "reconnecting" ? "starting" : "";
    return '<div class="row"><span class="k">tunnel</span><span class="v">' +
      '<span class="dot ' + dot + '" style="display:inline-block;margin-right:6px"></span>' +
      esc(x.name) + " · " + esc(String(x.localPort)) + " &rarr; " + esc(x.targetHost + ":" + x.targetPort) +
      " · " + esc(x.state) + (x.reason ? " — " + esc(x.reason) : "") +
      (x.stalePool
        ? '<div class="tun-err">reconnected after this MCP started — its connection pool may hold dead sockets. ' +
          'Use Restart above.</div>'
        : "") +
      "</span></div>";
  }).join("");
  return '<div class="cap">Depends on</div><div class="group">' + rows + "</div><div style=\"height:var(--s5)\"></div>";
}

function wireTabBody(d: McpDetail): void {
  const prev = $("pgPrev"); if (prev) prev.onclick = pagePrev;
  const next = $("pgNext"); if (next) next.onclick = pageNext;
  const edit = $("c-edit"); if (edit) edit.onclick = startEdit;
  const replaceBtn = $("c-replace"); if (replaceBtn) replaceBtn.onclick = startReplace;
  const save = $("e-save"); if (save) save.onclick = d.editMode === "replace" ? saveReplace : saveEdit;
  document.querySelectorAll<HTMLElement>("#tabbody [data-restore]").forEach((b) => {
    b.onclick = () => { void restoreRevision(Number(b.dataset.restore)); };
  });
  document.querySelectorAll<HTMLElement>("#tabbody [data-revdel]").forEach((b) => {
    b.onclick = () => { void deleteRevision(Number(b.dataset.revdel)); };
  });
  const cancel = $("e-cancel"); if (cancel) cancel.onclick = cancelEdit;
  const testBtn = $("e-test"); if (testBtn) testBtn.onclick = () => { void runConnTest("e-"); };
  const type = $<HTMLSelectElement>("e-type"); if (type) type.onchange = () => { changeEditType(type.value); };

  // Tools list → Try
  document.querySelectorAll<HTMLElement>("#tabbody [data-try]").forEach((b) => {
    b.onclick = () => { tryTool(b.dataset.try as string); };
  });

  // Resources list → Read
  document.querySelectorAll<HTMLButtonElement>("#tabbody [data-read]").forEach((b) => {
    b.onclick = () => { void readResource(b.dataset.read as string, b); };
  });

  // Tools list → on/off toggle
  document.querySelectorAll<HTMLButtonElement>("#tabbody [data-toggle]").forEach((b) => {
    b.onclick = () => { void toggleTool(b.dataset.toggle as string, b.dataset.on === "1", b); };
  });

  // Resources → master on/off
  document.querySelectorAll<HTMLButtonElement>("#tabbody [data-restog]").forEach((b) => {
    b.onclick = () => { void toggleResources(b.dataset.restog === "1", b); };
  });

  // Logs tab
  // docs/32 B4: Clear lives behind the toolbar's ellipsis menu — a destructive action does not
  // get a standing button in the filter row. The house popupMenu (menu.js) carries the item.
  const clMenu = $("clMenu");
  if (clMenu) clMenu.onclick = (ev) => {
    ev.stopPropagation();
    // The house toggle idiom (pane.js toggleMenu): a second click dismisses instead of reopening.
    if (menuIsOpen()) { closeMenu(); return; }
    popupMenu(clMenu.getBoundingClientRect(), [
      { label: "Clear logs…", danger: true, fn: () => { void clearCalls(); } },
    ]);
  };
  // The search box (docs/31): debounced server-side reload; Escape clears at once. Property
  // assignment, not addEventListener — renderCallsOnly may re-wire the SAME live node.
  const q = $<HTMLInputElement>("callsQ");
  if (q) {
    q.oninput = () => {
      clearTimeout(d.callsQTimer);
      d.callsQTimer = setTimeout(() => {
        const nd = state.detail;
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
    };
    q.onkeydown = (ev) => {
      if (ev.key !== "Escape" || !q.value) return;
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
    };
  }
  // docs/32 B2: a keyboard activation (Enter/Space on a focused button) carries detail === 0 —
  // that action owes the user focus back on the equivalent button once the switch commits.
  const clPrev = $("clPrev"); if (clPrev) clPrev.onclick = (ev) => { callsPageStep(-1, { fromKey: !!ev && ev.detail === 0 }); };
  const clNext = $("clNext"); if (clNext) clNext.onclick = (ev) => { callsPageStep(1, { fromKey: !!ev && ev.detail === 0 }); };
  const clRetry = $("clRetry"); if (clRetry) clRetry.onclick = (ev) => { callsRetry({ fromKey: !!ev && ev.detail === 0 }); };
  // docs/33 C1: block copy buttons. The text comes from the CALL ROW, not the painted DOM —
  // a truncated preview or a highlighted render still copies the full pretty payload.
  document.querySelectorAll<HTMLElement>("#tabbody [data-copy]").forEach((b) => {
    b.onclick = () => {
      const nd = state.detail;
      if (!nd) return;
      const parts = String(b.dataset.copy || "").split(":");
      const seq = Number(parts[1]);
      const c = (nd.calls || []).find((r) => { return r.seq === seq; });
      if (!c) return;
      const text = parts[0] === "args" ? fmtJson(c.args || "")
        : fmtJson(nd.callsFull[seq] != null ? nd.callsFull[seq] : c.output);
      void copyLogText(text || "");
    };
  });
  document.querySelectorAll<HTMLElement>("#tabbody [data-callseq]").forEach((s) => {
    s.onclick = () => { toggleCall(s.dataset.callseq as unknown as number); };
    s.onkeydown = (ev) => {
      if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); toggleCall(s.dataset.callseq as unknown as number); }
    };
  });
  document.querySelectorAll<HTMLElement>("#tabbody [data-full]").forEach((b) => {
    b.onclick = (ev) => { ev.stopPropagation(); void showFullResult(b.dataset.full as unknown as number); };
  });

  // Run tab
  const toolSel = $<HTMLSelectElement>("r-tool");
  if (toolSel) {
    toolSel.onchange = () => { d.run.tool = toolSel.value; d.run.result = null; renderPane(); };
  }
  const histBtn = $<HTMLButtonElement>("r-hist");
  if (histBtn) {
    histBtn.onclick = () => { histToggle(); };
    // Same guard as renderHistoryOnly: a filter that matched nothing is a search in progress, not
    // an empty history — the control stays clickable so the popover (and its empty state) can open.
    histBtn.disabled = !d.run.histQ && d.run.histTool === d.run.tool && !(d.run.hist || []).length;
  }
  // A pane rebuild drops the popover with the old markup it lived in. If it was open, bring it
  // back over the fresh button; the previewed entry (histSelSeq) is re-shown by histPreview.
  if (d.run.histOpen) {
    d.run.histOpen = false; // histOpen() would no-op behind its own histClose guard
    histOpen();
  }
  // Loading here rather than in showTab: the tool the Run tab will show is only settled once the
  // form is built (runBody falls back to the first tool when none was preselected via Try).
  if (d.tab === "run" && d.run.tool) void loadRunHistory(d.name, d.run.tool);
  const runBtn = $("runBtn");
  if (runBtn) {
    runBtn.onclick = runTool;
    // Ctrl/Cmd+Enter runs, so a SQL textarea can be submitted without reaching for the mouse.
    document.querySelectorAll<HTMLElement>("#tabbody textarea, #tabbody input[type=text]").forEach((f) => {
      f.addEventListener("keydown", (ev) => {
        if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") { ev.preventDefault(); void runTool(); }
      });
    });
  }
  if ($("runOut")) renderRunResult();
}

/** Jump from the Tools list into Run with that tool preselected. */
function tryTool(name: string): void {
  const d = state.detail;
  if (!d) return;
  d.run.tool = name;
  d.run.result = null;
  showTab("run");
}

/** Toggle all of an MCP's resources on/off. Live: the server answers an empty list while off and
 *  pushes notifications/resources/list_changed, so this just fires the change and reloads. */
async function toggleResources(currentlyOn: boolean, btn: HTMLButtonElement): Promise<void> {
  if (!state.selected) return;
  btn.disabled = true;
  const j = await apiJson<{ unchanged?: boolean }>("/api/mcps/" + encodeURIComponent(state.selected) + "/resources-toggle", {
    method: "POST",
    body: JSON.stringify({ enabled: !currentlyOn }),
  });
  btn.disabled = false;
  if (!j) return;
  if (j.unchanged) { toast("Already " + (currentlyOn ? "on" : "off")); return; }
  const d = state.detail;
  if (d) { d.resources.loaded = false; d.resources.cursors = []; void loadPage(d.name, "resources"); }
}

/** Toggle a tool on/off for clients. Live: the server filters the next tools/list and pushes
 *  notifications/tools/list_changed, so this just fires the change and reloads the list. */
async function toggleTool(name: string, currentlyOn: boolean, btn: HTMLButtonElement): Promise<void> {
  if (!state.selected) return;
  btn.disabled = true;
  const j = await apiJson<{ unchanged?: boolean }>("/api/mcps/" + encodeURIComponent(state.selected) + "/tools/" + encodeURIComponent(name), {
    method: "POST",
    body: JSON.stringify({ enabled: !currentlyOn }),
  });
  btn.disabled = false;
  if (!j) return;
  if (j.unchanged) { toast("Already " + (currentlyOn ? "on" : "off")); return; }
  // Reload the tools page so the row moves between the enabled list and the disabled group.
  const d = state.detail;
  if (d) { d.tools.loaded = false; d.tools.cursors = []; void loadPage(d.name, "tools"); }
}

/** Read one resource and show its contents. Read-only, so a sheet with no form is the whole UI. */
async function readResource(uri: string, btn: HTMLButtonElement | null): Promise<void> {
  if (!state.selected) return;
  const label = btn ? btn.textContent : "";
  if (btn) { btn.disabled = true; btn.textContent = "…"; }
  const j = await apiJson<ApiMcpResourceRead>("/api/mcps/" + encodeURIComponent(state.selected) + "/resource", {
    method: "POST",
    body: JSON.stringify({ uri: uri }),
  });
  if (btn) { btn.disabled = false; btn.textContent = label || "Read"; }
  if (!j) return;
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Resource contents">' +
      '<div class="sheet-head"><h2>' + esc(uri) + "</h2></div>" +
      '<div class="sheet-body"><div class="note">' +
        (j.ok ? esc((j.mimeType || "text/plain") + " · " + j.ms + " ms") : '<span style="color:var(--red)">read failed</span>') +
      "</div>" +
      '<pre class="logs" style="max-height:60vh">' + esc(j.text || "") + "</pre></div>" +
      '<div class="sheet-foot"><button class="btn" id="rd-close">Close</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  $("rd-close").onclick = closeSheet;
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeSheet(); };
}

/** Repaint only the Logs tab body, so a poll doesn't rebuild the pane (or the header, or the menu).
 *  Expanded rows survive because `callsOpen` is state, not DOM. */
function renderCallsOnly(): void {
  const d = state.detail;
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
  body.innerHTML = logsBody(d);
  const freshQ = document.getElementById("callsQ");
  if (liveQ && freshQ && liveQ !== freshQ) freshQ.replaceWith(liveQ);
  if (hadFocus && liveQ) {
    liveQ.focus();
    try { liveQ.setSelectionRange(caret, caret); } catch (err) { /* type=search supports it; guard anyway */ }
  }
  body.dataset.callsig = sig;
  wireTabBody(d);
  mountJsonTrees(d); // docs/33 C2: open rows get their trees back, expansion restored
}

export { applyRunHistory, configBody, fillRunArgs, histButtonLabel, histClose, histOpen, histPreview, histRowsHtml, histSearchTimer, histToggle, histViewHtml, histWhen, loadRunHistory, queueHistSearch, readResource, renderCallsOnly, renderHistoryOnly, renderRunResult, runTool, toggleResources, toggleTool, tryTool, tunnelDepsHtml, wireHistRows, wireTabBody };

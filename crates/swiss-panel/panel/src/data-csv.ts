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

import type { ApiDbColumn, ApiDbConnectionRow } from "./types/api.js";
import { $, apiJson, el, errText, targetEl, toast } from "./util.js";
import { fill, h } from "./h.js";
import { dbLoadData, renderDbGrid, renderDbToolbar } from "./data-grid.js";
import { dbApplyFilters, renderDbFilters } from "./data-filters.js";
import { dbOpenValueSheet } from "./data-value.js";
import { dbDropEdits, dbOkToDrop, dbPending, dbPkKey, dbResultKey } from "./data-view.js";
import { dbConn, dbSqlTab, dbTab } from "./db-state.js";
import { locale, tr, trn } from "./i18n.js";
import { clampMenuPos, setMenuOpen } from "./ui/menu.js";
import { closeSheet } from "./ui/sheet.js";

/* --- CSV import wizard -------------------------------------------------------------------------- */
/* Paste or upload CSV, map its columns to table columns, preview the first rows, then commit.
   The batch runs server-side in ONE transaction; a failure rolls the whole file back. The
   Insert/Upsert segmented control picks the statement form (docs/22 W4.5): Insert keeps the
   plain INSERT (a duplicate key aborts the file), Upsert maps conflicts onto existing rows. */
function dbParseCsvLine(line: string): string[] {
  const out: string[] = []; let cur = "", inQ = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (inQ) {
      if (c === String.fromCharCode(34) && line[i + 1] === String.fromCharCode(34)) { cur += c; i++; }
      else if (c === String.fromCharCode(34)) inQ = false;
      else cur += c;
    } else {
      if (c === ",") { out.push(cur); cur = ""; }
      else if (c === String.fromCharCode(34) && cur === "") inQ = true;
      else cur += c;
    }
  }
  out.push(cur);
  return out;
}

/* The mapping row's left label is punctuation around a raw CSV header name (CSV "id" →):
   the CSV token, the quotes and the arrow carry no translatable words in any language, so
   the label stays out of the dictionary and its literals live here as consts. */
const CSV_MAP_LABEL = "CSV ";
const CSV_MAP_ARROW = " \u2192 ";

function dbOpenImport(): void {
  const c = dbConn();
  const t0 = dbTab();
  if (t0.kind !== "table") { toast(tr("dataCsv.openTableFirst"), true); return; }
  const d = t0;
  if (!c.conn || !d.table || !d.data) { toast(tr("dataCsv.openTableFirst"), true); return; }
  if (!d.data.editable) { toast(tr("dataCsv.tableNotEditable", { reason: d.data.editNote || tr("dataCsv.noPrimaryKey") }), true); return; }
  if (dbPending() && !dbOkToDrop()) return;
  let header: string[] = [], lines: string[] = [], mapping: (string | null)[] = [];
  let mode = "insert"; // docs/22 W4.5: "insert" | "upsert" — the statement form the commit uses
  // docs/37 R5: node sheet, painted AFTER the host is unhidden; the per-open wiring below
  // stays (the sheet idiom — parse/paint/setMode close over the mapping state).
  $("sheet").hidden = false;
  // docs/43 M2 fixup: sheet-head/body/foot are SIBLINGS here. A missing paren on the h2
  // line used to nest body and foot INSIDE the head band, so the whole dialog was laid out
  // by .sheet-head's rules on every dialect (mysql and pg alike) — the "broken import
  // styling" report. The sheet idiom (add-sheet.ts) is the reference shape.
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label: tr("dataCsv.importCsv") } },
      h("div", { class: "sheet-head" },
        h("h2", null, tr("dataCsv.importCsvIntoT", { t: (d.schema ? d.schema + "." : "") + d.table! }))),
      h("div", { class: "sheet-body" },
        h("div", { class: "db-console-row", style: "margin-bottom:var(--s2)" },
          h("div", { class: "seg", role: "tablist", id: "dbImpMode", style: "margin-bottom:0" },
            h("button", { type: "button", role: "tab", data: { mode: "insert" }, aria: { selected: "true" } }, tr("dataCsv.insert")),
            h("button", { type: "button", role: "tab", data: { mode: "upsert" }, aria: { selected: "false" } }, tr("dataCsv.upsert"))),
          h("span", { class: "hint", id: "dbImpModeSay" }, tr("dataCsv.everyRowInsertsDuplicate"))),
        h("div", { class: "db-console-row", style: "margin-bottom:var(--s2)" },
          h("input", { type: "file", id: "dbImpFile", accept: ".csv,text/csv", style: "width:auto" }),
          h("span", { class: "hint" }, tr("dataCsv.pasteBelowFirstRow"))),
        h("textarea", { id: "dbImpText", placeholder: tr("dataCsv.idNameN1Alice"), style: "min-height:120px" }),
        h("div", { id: "dbImpMap", style: "margin-top:var(--s3)" }),
        h("div", { id: "dbImpPreview", style: "margin-top:var(--s3)" })),
      h("div", { class: "sheet-foot" }, h("span", { class: "grow" }),
        h("button", { class: "btn", id: "dbImpCancel" }, tr("dataCsv.cancel")),
        h("button", { class: "btn primary", id: "dbImpRun" }, tr("dataCsv.importOneTransaction")))));

  function parse(): void {
    const text = $<HTMLInputElement>("dbImpText").value.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
    const rows = text.split("\n").filter((l: string): boolean => { return l.trim() !== ""; });
    if (!rows.length) { header = []; lines = []; mapping = []; paint(); return; }
    header = dbParseCsvLine(rows[0]);
    lines = rows.slice(1, 10001); // cap mirrors IMPORT_ROW_CAP
    // default mapping: match by name, skip otherwise
    const names = (d.data?.columns.map((c: ApiDbColumn): string => { return c.name; }) ?? []);
    mapping = header.map((h: string): string | null => { return names.includes(h) ? h : null; });
    paint();
  }

  function paint(): void {
    const mapBox = $("dbImpMap");
    mapBox.textContent = "";
    if (!header.length) return;
    const names = (d.data?.columns.map((c: ApiDbColumn): string => { return c.name; }) ?? []);
    header.forEach((h: string, i: number): void => {
      const row = el("div", "db-console-row");
      row.style.marginBottom = "2px";
      row.appendChild(el("span", "db-filter-hint", CSV_MAP_LABEL + String.fromCharCode(34) + h + String.fromCharCode(34) + CSV_MAP_ARROW));
      const sel = el("select");
      sel.style.width = "auto";
      const skip = el("option", "", tr("dataCsv.skipColumn")) as HTMLOptionElement;
      skip.value = "";
      sel.appendChild(skip);
      names.forEach((n: string): void => {
        const o = el("option", "", n) as HTMLOptionElement;
        o.value = n;
        o.selected = n === mapping[i];
        sel.appendChild(o);
      });
      sel.onchange = (e) => { mapping[i] = (e.currentTarget as HTMLSelectElement).value || null; preview(); };
      row.appendChild(sel);
      mapBox.appendChild(row);
    });
    preview();
  }

  function preview(): void {
    const box = $("dbImpPreview");
    box.textContent = "";
    if (!header.length) return;
    const mapped: number = mapping.filter(Boolean).length;
    if (!mapped) { box.appendChild(el("div", "hint", tr("dataCsv.noColumnsMapped"))); return; }
    const sample = lines.slice(0, 3).map((l: string): Record<string, unknown> => {
      const cells = dbParseCsvLine(l);
      const o: Record<string, unknown> = {};
      mapping.forEach((m: string | null, i: number): void => { if (m) o[m] = cells[i] === "" ? null : cells[i]; });
      return o;
    });
    box.appendChild(el("div", "hint", trn(lines.length, "dataCsv.previewSummary.one", "dataCsv.previewSummary.other",
      { n: lines.length.toLocaleString(locale()), m: mapped, k: header.length })));
    const pre = el("pre", "db-ddl");
    pre.style.position = "static";
    pre.style.margin = "var(--s1) 0 0";
    pre.textContent = sample.map((o: Record<string, unknown>): string => { return JSON.stringify(o); }).join("\n");
    box.appendChild(pre);
  }

  $<HTMLInputElement>("dbImpText").oninput = parse;
  function setMode(m: string): void {
    mode = m;
    // Hoisted: the i18n gate's ternary scan counts the comparison literal in the condition.
    const isUpsert = m === "upsert";
    $("dbImpMode").querySelectorAll("button").forEach((b) => {
      b.setAttribute("aria-selected", String(b.dataset.mode === m));
    });
    // One sentence beside the control names the cost of the picked mode (docs/22 W4.5).
    $("dbImpModeSay").textContent = isUpsert
      ? tr("dataCsv.rowsMatchExistingKey")
      : tr("dataCsv.everyRowInsertsDuplicate");
  }
  $("dbImpMode").onclick = (e: MouseEvent): void => {
    const b: HTMLElement | null = targetEl(e)?.closest<HTMLElement>("button[data-mode]") ?? null;
    if (b) setMode(b.dataset.mode!);
  };
  $<HTMLInputElement>("dbImpFile").onchange = (e) => {
    const f = (e.currentTarget as HTMLInputElement).files;
    if (!f || !f[0]) return;
    const rd = new FileReader();
    rd.onload = (): void => { $<HTMLInputElement>("dbImpText").value = String(rd.result); parse(); };
    rd.readAsText(f[0]);
  };
  $("dbImpCancel").onclick = closeSheet;
  $<HTMLButtonElement>("dbImpRun").onclick = async (e): Promise<void> => {
    if (!header.length || !lines.length) { toast(tr("dataCsv.pasteUploadCsvFirst"), true); return; }
    if (!mapping.some(Boolean)) { toast(tr("dataCsv.mapLeastOneColumn"), true); return; }
    const upsert = mode === "upsert";
    if (!confirm(tr(upsert ? "dataCsv.upsertConfirm" : "dataCsv.insertConfirm",
        { n: lines.length.toLocaleString(locale()), t: (d.schema ? d.schema + "." : "") + d.table! }))) return;
    const t = e.currentTarget as HTMLButtonElement;
    t.disabled = true;
    t.textContent = tr("dataCsv.importing");
    const j = await apiJson<{ inserted: number; note?: string }>("/api/db/" + encodeURIComponent(c.conn!) + "/import", {
      method: "POST",
      // mode rides the payload only when upsert — a default import stays byte-identical to
      // what a pre-W4.5 panel sent (docs/22 W4.5).
      body: JSON.stringify(Object.assign(
        { table: d.table, schema: d.schema, header: header, lines: lines, mapping: mapping },
        upsert ? { mode: "upsert" } : {}
      )),
    });
    const btn = $<HTMLButtonElement>("dbImpRun");
    if (btn) { btn.disabled = false; btn.textContent = tr("dataCsv.importOneTransaction"); }
    if (!j) return; // server rolled back; the sheet stays for fixing
    // j.note is the server's degrade explanation (a Postgres table with no primary key); the
    // count is still the truth, the sentence beside it says what actually ran.
    toast(tr(j.note ? "dataCsv.importedRowsNote" : "dataCsv.importedRows", { n: j.inserted, note: j.note ?? "" }));
    closeSheet();
    dbDropEdits();
    void dbLoadData(true);
  };
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) closeSheet(); };
}
/* --- copy to clipboard -------------------------------------------------------------------------- */
/* Right-click a cell: copy the value, or the whole row as JSON / CSV / INSERT. Values are taken
   from the CURRENT BUFFER when the cell has a pending edit, so what you copy is what you see. */
function dbCopyText(text: string): void {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then((): void => { toast(tr("dataCsv.copied")); }, (): void => { dbCopyFallback(text); });
  } else dbCopyFallback(text);
}

function dbCopyFallback(text: string): void {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); toast(tr("dataCsv.copied")); } catch (e) { toast(tr("dataCsv.copyFailed"), true); }
  ta.remove();
}

function dbRowForCopy(row: Record<string, unknown>, key: string): Record<string, unknown> {
  const d = dbTab();
  if (d.kind !== "table") return {};
  const upd = d.updates[key];
  const out: Record<string, unknown> = {};
  d.data?.columns.forEach((c: ApiDbColumn): void => {
    out[c.name] = upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name) ? upd.changes[c.name] : row[c.name];
  });
  return out;
}

function dbCopyCsvCell(v: unknown): string {
  if (v === null || v === undefined) return "";
  const s = typeof v === "object" ? JSON.stringify(v) : String(v);
  return String.fromCharCode(34) + s.split(String.fromCharCode(34)).join(String.fromCharCode(34) + String.fromCharCode(34)) + String.fromCharCode(34);
}

/** docs/22 W1.5: one filter straight from a cell value — push it, paint the row, apply.
 * docs/22 closeout B6: a REFUSED discard takes the pushed row back — a filter the user just
 * said no to must not stay on screen as if it had been accepted. */
function dbPushCellFilter(column: string, op: string, value: unknown): void {
  const d = dbTab();
  if (d.kind !== "table") return;
  d.filters.push({ column: column, op: op, value: value as string | number | boolean | null });
  renderDbFilters();
  if (!dbApplyFilters()) { d.filters.pop(); renderDbFilters(); }
}

function dbCellMenu(e: MouseEvent, row: Record<string, unknown> | null, key: string | null, column: string, editInDialog: null | (() => void)): void {
  e.preventDefault();
  const c = dbConn();
  const t0 = dbTab();
  if (t0.kind !== "table") return;
  const d = t0;
  if (!d.data) return;
  // An insert row's menu carries no row key; a data row's key and row always arrive together.
  const upd = key == null ? null : d.updates[key];
  const pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, column);
  const value = pending ? upd.changes[column] : row ? row[column] : undefined;
  const full = row && key != null ? dbRowForCopy(row, key) : null;
  const names = d.data?.columns.map((c: ApiDbColumn): string => { return c.name; });
  const dialect = (c.conns.find((x: ApiDbConnectionRow): boolean => { return x.name === c.conn; }) || {} as { dialect?: string }).dialect || "mysql";

  const menu = el("div", "ctx-menu");
  function item(label: string, fn: () => void): void {
    const b = el("button", "", label) as HTMLButtonElement;
    b.onclick = (): void => { closeMenu2(); fn(); };
    menu.appendChild(b);
  }
  item(tr("logs.copyValue"), (): void => { dbCopyText(value === null || value === undefined ? "NULL" : String(value)); });
  // docs/22 W5.3: the read-only viewer — full text, a JSON tree, hex or the link. NULL has
  // no content to view, so it gets no item (same rule as the filter items below).
  if (value !== null && value !== undefined) {
    item(tr("dataCsv.viewValue"), (): void => {
      dbOpenValueSheet(column, value, (d.schema ? d.schema + "." : "") + d.table!);
    });
  }
  if (editInDialog) {
    item(tr("dataCsv.editInDialog"), editInDialog); // long text / JSON: the user-chosen dialog path
  }
  // docs/22 W1.5: filter-by-value straight off a cell. NULL cells show none of these (there is
  // no value to equal); the pushed filter lands in the standing filter row like a typed one.
  if (row && value !== null && value !== undefined) {
    item(tr("dataCsv.filterEqValue"), (): void => { dbPushCellFilter(column, "eq", value); });
    item(tr("dataCsv.filterNeValue"), (): void => { dbPushCellFilter(column, "ne", value); });
    item(tr("dataCsv.filterContains"), (): void => { dbPushCellFilter(column, "like", value); });
  }
  // Checked-row copies live in the SAME menu — one right-click reaches every format. No
  // sqlResult guard left (docs/42 T2): this is the TABLE grid's cell menu, and a query reply
  // now paints in its own tab with dbResultCellMenu below — the two can no longer overlap.
  const hint = dbAppendSelItems(item, dbSelectedForCopy());
  if (hint) menu.appendChild(hint);
  item(tr("dataCsv.selectAllOnPage"), (): void => { dbSelAll(true); });
  if (Object.keys(d.sel).length) item(tr("dataCsv.clearSelection"), (): void => { dbSelAll(false); });
  if (full) {
    item(tr("dataCsv.copyRowAsJson"), (): void => { dbCopyText(JSON.stringify(full, null, 2)); });
    item(tr("dataCsv.copyRowAsCsv"), (): void => { dbCopyText(names.map((n: string): string => { return dbCopyCsvCell(full?.[n]); }).join(",")); });
    item(tr("dataCsv.copyRowAsInsert"), (): void => {
      try {
        const q = (n: string): string => { return dialect === "mysql" ? "`" + n + "`" : String.fromCharCode(34) + n + String.fromCharCode(34); };
        const cols = names.filter((n: string): boolean => { return full![n] !== undefined; });
        const lit = (v: unknown): string => {
          if (v === null || v === undefined) return "NULL";
          if (typeof v === "number" || typeof v === "boolean") return String(v);
          const qq = String.fromCharCode(39);
          return qq + String(v).split(qq).join(qq + qq) + qq;
        };
        const table = (d.schema ? q(d.schema) + "." : "") + q(d.table!);
        dbCopyText("INSERT INTO " + table + " (" + cols.map(q).join(", ") + ") VALUES (" +
          cols.map((n: string): string => { return lit(full?.[n]); }).join(", ") + ");");
      } catch (err) { toast(errText(err), true); }
    });
  }
  document.body.appendChild(menu);
  // The cursor point is the anchor, clamped to the viewport (docs/22 closeout audit): a
  // right-click at the right or bottom edge used to strand the menu off-screen. Measured
  // AFTER the append — offsetWidth is zero until the menu is in the document.
  const box1 = menu.getBoundingClientRect();
  const pos1 = clampMenuPos({ left: e.clientX, top: e.clientY, bottom: e.clientY }, box1.width, box1.height, window.innerWidth, window.innerHeight);
  menu.style.left = pos1.left + "px";
  menu.style.top = pos1.top + "px";
  setMenuOpen(true);
  function closeMenu2() { menu.remove(); setMenuOpen(false); }
  setTimeout(() => {
    document.addEventListener("mousedown", function h(ev) {
      if (!menu.contains(ev.target as Node)) { menu.remove(); setMenuOpen(false); document.removeEventListener("mousedown", h); }
    });
  }, 0);
}

/* --- multi-row copy ------------------------------------------------------------------------------ */
/* Checked rows (the rowctl checkboxes) copied to the clipboard in a paste-anywhere format.
   The row list mirrors what the grid SHOWS: pending buffered edits ride along, deleted-buffered
   rows keep their original values. For a query-result grid the keys are dbResultKey(tab, index) —
   the ACTIVE tab's namespace (docs/22 W4.3), so a copy never crosses tabs. */
function dbSelectedForCopy(): { cols: string[]; rows: Record<string, unknown>[] } {
  const t = dbTab();
  const st = dbSqlTab();
  if (st && st.sqlResult) {
    const res = st.sqlResult;
    const tab = st.resultTab || 0;
    const qrows: Record<string, unknown>[] = [];
    res.rows.forEach((r: Record<string, unknown>, i: number): void => { if (t.sel[dbResultKey(tab, i)]) qrows.push(r); });
    return { cols: res.columns, rows: qrows };
  }
  if (t.kind !== "table") return { cols: [], rows: [] };
  const d = t;
  if (!d.data) return { cols: [], rows: [] };
  const pkCols = d.data.primaryKey || [];
  const cols = d.data.columns.map((c: ApiDbColumn): string => { return c.name; });
  const rows: Record<string, unknown>[] = [];
  d.data.rows.forEach((row: Record<string, unknown>, i: number): void => {
    const key = pkCols.length ? dbPkKey(pkCols, row) : String(i);
    if (d.sel[key]) rows.push(dbRowForCopy(row, key));
  });
  return { cols: cols, rows: rows };
}

function dbFlatCell(v: unknown): string {
  if (v === null || v === undefined) return "";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

function dbRowsCsv(sel: { cols: string[]; rows: Record<string, unknown>[] }): string {
  const lines = [sel.cols.map(dbCopyCsvCell).join(",")];
  sel.rows.forEach((r: Record<string, unknown>): void => {
    lines.push(sel.cols.map((c: string): string => { return dbCopyCsvCell(r[c]); }).join(","));
  });
  return lines.join("\n");
}

function dbRowsTsv(sel: { cols: string[]; rows: Record<string, unknown>[] }): string {
  const cell = (v: unknown): string => { return dbFlatCell(v).replace(/\t/g, " ").replace(/\r?\n/g, " "); };
  const lines = [sel.cols.join("\t")];
  sel.rows.forEach((r: Record<string, unknown>): void => {
    lines.push(sel.cols.map((c: string): string => { return cell(r[c]); }).join("\t"));
  });
  return lines.join("\n");
}

function dbRowsMarkdown(sel: { cols: string[]; rows: Record<string, unknown>[] }): string {
  const cell = (v: unknown): string => { return dbFlatCell(v).replace(/\|/g, "\\|").replace(/\r?\n/g, " "); };
  const lines = ["| " + sel.cols.join(" | ") + " |"];
  lines.push("|" + sel.cols.map((): string => { return " --- |"; }).join(""));
  sel.rows.forEach((r: Record<string, unknown>): void => {
    lines.push("| " + sel.cols.map((c: string): string => { return cell(r[c]); }).join(" | ") + " |");
  });
  return lines.join("\n");
}

function dbRowsJson(sel: { cols: string[]; rows: Record<string, unknown>[] }): string { return JSON.stringify(sel.rows, null, 2); }

/** Tick or clear every row on screen (the header checkbox). Keys match dbSelectedForCopy. */
function dbSelAll(on: boolean): void {
  const t = dbTab();
  const st = dbSqlTab();
  if (st && st.sqlResult) {
    const tab = st.resultTab || 0;
    st.sqlResult.rows.forEach((r: Record<string, unknown>, i: number): void => {
      const k = dbResultKey(tab, i);
      if (on) t.sel[k] = true; else delete t.sel[k];
    });
  } else if (t.kind === "table") {
    const d = t;
    if (!d.data) { renderDbToolbar(); renderDbGrid(); return; }
    const pkCols = d.data?.primaryKey || [];
    d.data?.rows.forEach((row: Record<string, unknown>, i: number): void => {
      const key = pkCols.length ? dbPkKey(pkCols, row) : String(i);
      if (on) d.sel[key] = true; else delete d.sel[key];
    });
  }
  t.selAnchor = -1;
  renderDbToolbar(); renderDbGrid();
}

function dbAppendSelItems(item: (label: string, fn: () => void) => void, sel: { cols: string[]; rows: Record<string, unknown>[] }): HTMLElement | null {
  const n = sel.rows.length;
  if (!n) {
    const hint = el("button", "ctx-hint", tr("dataCsv.noRowsChecked")) as HTMLButtonElement;
    hint.disabled = true;
    // item() only makes enabled buttons; the hint is appended by the caller's menu directly
    return hint;
  }
  // The noun is plural-aware (trn) and rides the four format labels as a {noun} var — the
  // same nested-trn shape dataBrowsers.commitNKOne uses.
  const noun = trn(n, "dataCsv.checkedRows.one", "dataCsv.checkedRows.other");
  item(tr("dataCsv.copySelAsCsv", { noun }), (): void => { dbCopyText(dbRowsCsv(sel)); });
  item(tr("dataCsv.copySelAsTsv", { noun }), (): void => { dbCopyText(dbRowsTsv(sel)); });
  item(tr("dataCsv.copySelAsMarkdownTable", { noun }), (): void => { dbCopyText(dbRowsMarkdown(sel)); });
  item(tr("dataCsv.copySelAsJson", { noun }), (): void => { dbCopyText(dbRowsJson(sel)); });
  return null;
}

/* Right-click a QUERY-RESULT cell: the value copy plus the same checked-row section.
   The row's checkbox can also be toggled from here — right-click needs no mouse travel. */
function dbResultCellMenu(e: MouseEvent, row: Record<string, unknown> | null, column: string): void {
  e.preventDefault();
  const d = dbTab();
  const st = dbSqlTab();
  const i = st && st.sqlResult ? st.sqlResult.rows.indexOf(row!) : -1;
  const menu = el("div", "ctx-menu");
  function item(label: string, fn: () => void): void {
    const b = el("button", "", label) as HTMLButtonElement;
    b.onclick = (): void => { closeMenu(); fn(); };
    menu.appendChild(b);
  }
  const v = row ? row[column] : undefined;
  item(tr("logs.copyValue"), (): void => { dbCopyText(v === null || v === undefined ? "NULL" : String(v)); });
  // docs/22 W5.3: the same read-only viewer on a console-result cell (no column types there —
  // the value alone picks the presentation, and the \\x wire form still says hex).
  if (v !== null && v !== undefined) {
    item(tr("dataCsv.viewValue"), (): void => { dbOpenValueSheet(column, v, tr("dataCsv.sqlResult")); });
  }
  if (i >= 0) {
    const rk = dbResultKey(st ? st.resultTab || 0 : 0, i);
    const on = !!d.sel[rk];
    item(on ? tr("dataCsv.uncheckThisRow") : tr("dataCsv.checkThisRow"), (): void => {
      if (on) delete d.sel[rk]; else d.sel[rk] = true;
      d.selAnchor = i;
      renderDbToolbar(); renderDbGrid();
    });
  }
  const hint = dbAppendSelItems(item, dbSelectedForCopy());
  if (hint) menu.appendChild(hint);
  item(tr("dataCsv.selectAllOnPage"), (): void => { dbSelAll(true); });
  if (Object.keys(d.sel).length) item(tr("dataCsv.clearSelection"), (): void => { dbSelAll(false); });
  document.body.appendChild(menu);
  // Same clamp as dbCellMenu (docs/22 closeout audit) — measured after the append.
  const box2 = menu.getBoundingClientRect();
  const pos2 = clampMenuPos({ left: e.clientX, top: e.clientY, bottom: e.clientY }, box2.width, box2.height, window.innerWidth, window.innerHeight);
  menu.style.left = pos2.left + "px";
  menu.style.top = pos2.top + "px";
  setMenuOpen(true);
  function closeMenu() { menu.remove(); setMenuOpen(false); }
  setTimeout(() => {
    document.addEventListener("mousedown", function h(ev) {
      if (!menu.contains(ev.target as Node)) { menu.remove(); setMenuOpen(false); document.removeEventListener("mousedown", h); }
    });
  }, 0);
}

/* --- CSV export of whatever the grid is showing --------------------------------------------------- */

function dbExportCsv(): void {
  const t = dbTab();
  const st = dbSqlTab();
  let cols: string[], rows: Record<string, unknown>[], name: string;
  if (st && st.sqlResult) {
    cols = st.sqlResult.columns;
    rows = st.sqlResult.rows;
    name = "query.csv";
  } else if (t.kind === "table") {
    const d = t;
    if (!d.data) return;
    cols = d.data.columns.map((c: ApiDbColumn): string => { return c.name; });
    rows = d.data.rows;
    name = d.data.table + ".csv";
  } else return;
  function cell(v: unknown): string {
    if (v === null || v === undefined) return "";
    const s = typeof v === "object" ? JSON.stringify(v) : String(v);
    return '"' + s.replace(/"/g, '""') + '"';
  }
  const lines = [cols.map((c: string): string => { return '"' + String(c).replace(/"/g, '""') + '"'; }).join(",")];
  rows.forEach((r: Record<string, unknown>): void => {
    lines.push(cols.map((c: string): string => { return cell(r[c]); }).join(","));
  });
  const blob = new Blob(["\uFEFF" + lines.join("\r\n")], { type: "text/csv;charset=utf-8" });
  const a = document.createElement("a") as HTMLAnchorElement;
  a.href = URL.createObjectURL(blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => { URL.revokeObjectURL(a.href); }, 1000);
}

export { dbCellMenu, dbCopyCsvCell, dbCopyFallback, dbCopyText, dbExportCsv, dbOpenImport, dbParseCsvLine, dbPushCellFilter, dbResultCellMenu, dbRowForCopy, dbSelAll, dbSelectedForCopy };

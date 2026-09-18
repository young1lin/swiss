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

import { $, esc, state, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";

/* --- cell editor dialog ------------------------------------------------------------------------- */
/* Editing happens in a sheet, never inline: an inline input grows its row and reshuffles the
   whole grid mid-edit, and a 4 KB JSON cell has no business in a 30px row anyway. The dialog
   knows what it is editing (table, column, PK address), offers NULL / one-line / text / JSON,
   and Save only writes the LOCAL buffer — Commit is still the only door to the database. */
var dbCellEdit: { kind: "update" | "insert"; key: string; i: number; column: string; meta: DbCellMeta } | null = null; // { kind: "update"|"insert", key, i, column, meta } while the sheet is open

function dbCellJsonLike(s: unknown): boolean {
  var t = String(s == null ? "" : s).trim();
  return t.startsWith("{") || t.startsWith("[");
}

/** Try pretty-printing; on invalid JSON return null (the editor stays in plain-text mode). */
function dbCellPretty(s: string): string | null {
  try { return JSON.stringify(JSON.parse(String(s)), null, 2); } catch (e) { return null; }
}

function dbOpenCellEditor(kind: "update" | "insert", key: string, i: number, column: string, meta: DbCellMeta): void {
  var d = state.db;
  if (!d!.data || !d!.data.editable) return;
  var isNull = false;
  var text: string | null = "";
  if (kind === "update") {
    var e = d!.updates[key];
    var pending = e && Object.prototype.hasOwnProperty.call(e.changes, column);
    var v = pending ? e!.changes[column] : meta.orig;
    isNull = pending ? v === null : meta.orig === null || meta.orig === undefined;
    text = isNull ? "" : dbCellText(v)!;
  } else {
    var ins = d!.inserts[i];
    var has = Object.prototype.hasOwnProperty.call(ins!.values, column);
    isNull = has ? ins!.values[column] === null : false;
    text = has && ins!.values[column] !== null ? dbCellText(ins!.values[column])! : "";
  }
  var pretty = dbCellJsonLike(text) ? dbCellPretty(text!) : null;
  var colMeta = d!.data!.columns.find(function (c: ApiDbColumn): boolean { return c.name === column; }) || {} as { dataType?: string };
  var isBool = /bool/i.test(colMeta.dataType || "");
  dbCellEdit = { kind: kind, key: key, i: i, column: column, meta: meta };

  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="Edit cell">' +
      '<div class="sheet-head"><div class="db-cell-head">' +
        "<h2>" + esc(column) + '</h2>' +
        '<span class="db-cell-where">' + esc((d!.schema ? d!.schema + "." : "") + d!.table +
          (kind === "update" ? " · PK " + JSON.stringify(meta.pk) : " · new row")) + "</span>" +
      "</div></div>" +
      '<div class="sheet-body">' +
        '<div class="db-console-row" style="margin-bottom:var(--s2)">' +
          (isBool ? '<button class="btn" id="dbCellBool"></button>' : "") +
          '<button class="btn" id="dbCellNull"></button>' +
          (pretty ? '<button class="btn" id="dbCellJson">Format JSON</button>' : "") +
          '<span class="hint">saves to the local buffer — Commit writes it in one transaction</span>' +
        "</div>" +
        '<textarea id="dbCellText" spellcheck="false"></textarea>' +
      "</div>" +
      '<div class="sheet-foot"><span class="grow"></span>' +
        '<button class="btn" id="dbCellCancel">Cancel</button>' +
        '<button class="btn primary" id="dbCellSave">Save to buffer</button></div>' +
    "</div>";
  $("sheet").hidden = false;
  var ta = $<HTMLTextAreaElement>("dbCellText");
  ta.value = pretty || text;
  var nullState = isNull;
  function paintNull(): void {
    // Never disable the textarea: a NULL cell once opened a dead editor ("cannot edit").
    // NULL is a visible overlay state that typing clears on its own.
    ta.disabled = false;
    ta.style.opacity = nullState ? ".5" : "1";
    ta.placeholder = nullState ? "NULL — type to replace, or toggle the button" : "";
    var b = $("dbCellNull");
    b.textContent = nullState ? "NULL (set)" : "Set NULL";
  }
  // Typing while NULL is set means replacing the NULL — clear the flag.
  ta.addEventListener("input", function () {
    if (nullState) { nullState = false; paintNull(); }
  });
  $("dbCellNull").onclick = function () {
    nullState = !nullState;
    paintNull();
    if (!nullState) ta.focus();
  };
  if (isBool) {
    var boolState = text === "1" || text!.toLowerCase() === "true";
    var bb = $("dbCellBool");
    function paintBool(): void { bb.textContent = boolState ? "TRUE" : "FALSE"; }
    bb.onclick = function () {
      boolState = !boolState;
      nullState = false;
      ta.value = boolState ? "1" : "0";
      paintBool(); paintNull();
    };
    paintBool();
  }
  if (pretty) {
    $("dbCellJson").onclick = function () {
      var p: string | null = dbCellPretty(ta.value);
      if (p != null) { ta.value = p; ta.classList.add("json"); }
      else toast("Not valid JSON — left as-is", true);
    };
  }
  paintNull();
  $("dbCellCancel").onclick = dbCloseCellEditor;
  $("dbCellSave").onclick = function (): void {
    var v: string | null | undefined;
    if (nullState) v = null;
    else {
      var raw = ta.value;
      // A JSON-shaped text in a JSON-ish column is stored as the string it is; the driver casts.
      v = raw === "" && kind === "update" && meta.orig !== "" ? null : raw;
      if (raw === "" && kind === "insert") v = undefined; // empty insert cell = use column default
    }
    dbSaveCellEdit(v);
  };
  $("sheet").onclick = function (e: MouseEvent): void { if (e.target === $("sheet")) dbCloseCellEditor(); };
  ta.focus();
  if (!pretty) ta.select();
}

function dbCloseCellEditor(): void {
  closeSheet();
  dbCellEdit = null;
}

/** Write the dialog result into the local buffer and repaint the grid + bar. */
function dbSaveCellEdit(v: string | null | undefined): void {
  var d = state.db;
  var ed = dbCellEdit;
  if (!ed || !d!.data) return;
  if (ed.kind === "insert") {
    var ins = d!.inserts[ed.i];
    if (v === undefined) delete ins!.values[ed.column];
    else ins!.values[ed.column] = v;
  } else {
    var e = d!.updates[ed.key] || (d!.updates[ed.key] = { pk: ed.meta.pk as Record<string, unknown>, changes: {} });
    var backToOriginal = v === ed.meta.orig || (v === null && (ed.meta.orig === null || ed.meta.orig === undefined));
    if (backToOriginal) {
      delete e.changes[ed.column];
      if (!Object.keys(e.changes).length) delete d!.updates[ed.key];
    } else e.changes[ed.column] = v;
  }
  dbCloseCellEditor();
  renderDbGrid();
  renderDbBar();
}

/** The text a cell shows and the editor starts from. null stays null. */
function dbCellText(v: unknown): string | null {
  if (v === null || v === undefined) return null;
  if (typeof v === "object") return JSON.stringify(v, null, 2);
  return String(v);
}

/* The fold width (docs/22 W2.3): JSON longer than this collapses to "(JSON)" with the full
   text one hover away. dbgate picked 100 for the same affordance — enough for a small object
   to stay readable in place, short enough that a document does not blow out the row. */
var DB_CELL_FOLD = 100;

/** docs/22 W2.3: the typed view of one cell value — {text, cls, title, href}. The grid painter
 *  is a thin wrapper over this; every decision here is value- and column-type-driven, never
 *  DOM-dependent, so the intents pin without a browser:
 *  - NULL hands back text null and the painter keeps its italic NULL span (the style it
 *    already had; the editor's NULL toggle rides on the same null-ness).
 *  - numbers right-align (cls db-num): a JS number by itself, or a numeric COLUMN even when
 *    the driver delivered its value as a string — BIGINT arrives as an exact string on
 *    purpose (a JS double silently rounds 18-digit ids; mysql.rs / pg.rs stringify int64),
 *    and an exact string still deserves to sit in the number column it belongs to.
 *  - booleans word themselves TRUE/FALSE (information_schema only says "boolean" for real
 *    boolean columns — MySQL's tinyint(1) keeps its 0/1, matching the edit dialog's rule).
 *  - an http(s) value becomes a link (href set) opened with the noopener guard.
 *  - JSON past the fold width collapses to "(JSON)" with the full text as the title.
 *  - a binary column shows a byte count instead of its lossy text rendering; the value
 *    sheet (docs/22 W5.3) owns the content itself.
 */
function dbCellView(value: unknown, colType: unknown): { text: string | null; cls: string | null; title: string | null; href: string | null } {
  if (value === null || value === undefined) return { text: null, cls: null, title: null, href: null };
  var type = String(colType == null ? "" : colType).toLowerCase();
  if (/binary|blob|bytea/.test(type)) {
    // The adapters decode BLOB bytes as UTF-8 (mysql.rs / pg.rs, mirroring the Node build's
    // Buffers-to-string pass), so the panel never sees the raw bytes — count what it did get,
    // in UTF-8 bytes (exact for ASCII payloads, honest about the delivery for the rest).
    var bytes = new TextEncoder().encode(String(value)).length;
    return { text: "binary, " + bytes + " bytes", cls: "db-fold", title: null, href: null };
  }
  if (/bool/.test(type) || typeof value === "boolean") {
    var on = value === true || value === 1 || String(value).toLowerCase() === "true" || value === "1";
    return { text: on ? "TRUE" : "FALSE", cls: null, title: null, href: null };
  }
  var text = dbCellText(value)!;
  if (/^https?:\/\//i.test(text)) return { text: text, cls: null, title: text, href: text };
  var jsonLike = typeof value === "object" || dbCellJsonLike(text);
  if (jsonLike && text.length > DB_CELL_FOLD) {
    return { text: "(JSON)", cls: "db-fold", title: text, href: null };
  }
  var numeric = typeof value === "number" ||
    /int|decimal|numeric|float|double|real|money|year|bit/.test(type);
  return { text: text, cls: numeric ? "db-num" : null, title: null, href: null };
}

export { DB_CELL_FOLD, dbCellEdit, dbCellJsonLike, dbCellPretty, dbCellText, dbCellView, dbCloseCellEditor, dbOpenCellEditor, dbSaveCellEdit };

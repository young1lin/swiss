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

import type { ApiDbColumn } from "./types/api.js";
import type { DbCellMeta } from "./types/state.js";
import { $, toast } from "./util.js";
import { h } from "./h.js";
import { renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
import { dbTab } from "./db-state.js";
import { tr } from "./i18n.js";
import { btn } from "./ui/button.js";
import { hint } from "./ui/form.js";
import { closeSheet, sheet, showSheet } from "./ui/sheet.js";

/* --- cell editor dialog ------------------------------------------------------------------------- */
/* Editing happens in a sheet, never inline: an inline input grows its row and reshuffles the
   whole grid mid-edit, and a 4 KB JSON cell has no business in a 30px row anyway. The dialog
   knows what it is editing (table, column, PK address), offers NULL / one-line / text / JSON,
   and Save only writes the LOCAL buffer — Commit is still the only door to the database. */
/* While the sheet is open: an update carries its row's key and meta, an insert neither
   (its row is d.inserts[i]) - key/meta are nullable and the update paths narrow. */
let dbCellEdit: { kind: "update" | "insert"; key: string | null; i: number; column: string; meta: DbCellMeta | null } | null = null;

/* The boolean toggle words itself with the same SQL tokens dbCellView uses for boolean
   values — value words like NULL, not copy — so they render untranslated in every language
   and live here as consts, out of the i18n gate's scanned positions. */
const DB_BOOL_TRUE = "TRUE";
const DB_BOOL_FALSE = "FALSE";

function dbCellJsonLike(s: unknown): boolean {
  const t = String(s == null ? "" : s).trim();
  return t.startsWith("{") || t.startsWith("[");
}

/** Try pretty-printing; on invalid JSON return null (the editor stays in plain-text mode). */
function dbCellPretty(s: string): string | null {
  try { return JSON.stringify(JSON.parse(String(s)), null, 2); } catch (e) { return null; }
}

function dbOpenCellEditor(kind: "update" | "insert", key: string | null, i: number, column: string, meta: DbCellMeta | null): void {
  const t = dbTab();
  if (t.kind !== "table") return;
  const d = t;
  if (!d.data || !d.data.editable) return;
  let isNull = false;
  let text: string | null = "";
  // The header line and the empty-string rule read the update's meta; capture both where the
  // narrowing lives instead of re-testing meta at each later use.
  let pkJson = "";
  let emptyWasNotNull = false;
  if (kind === "update") {
    // An update editor is only opened for a real row, which always carries its key and meta.
    if (key == null || !meta) return;
    const e = d.updates[key];
    const pending = e && Object.prototype.hasOwnProperty.call(e.changes, column);
    const v = pending ? e?.changes[column] : meta.orig;
    isNull = pending ? v === null : meta.orig === null || meta.orig === undefined;
    text = isNull ? "" : dbCellText(v)!;
    // The address carries every original value on purpose (docs/22 W4.2: the other columns
    // are the optimistic lock), but the head says "PK", so a keyed table shows its key columns
    // only; a keyless table is addressed by the whole row, and shows it (docs/46 P7-2 walk).
    const keyCols = d.data.primaryKey;
    pkJson = JSON.stringify(keyCols.length
      ? Object.fromEntries(keyCols.map((k: string): [string, unknown] => [k, (meta.pk as Record<string, unknown>)[k]]))
      : meta.pk);
    emptyWasNotNull = meta.orig !== "";
  } else {
    const ins = d.inserts[i]!;   // an insert editor is only opened for a buffered row that still exists
    const has = Object.prototype.hasOwnProperty.call(ins.values, column);
    isNull = has ? ins.values[column] === null : false;
    text = has && ins.values[column] !== null ? dbCellText(ins.values[column])! : "";
  }
  const pretty = dbCellJsonLike(text) ? dbCellPretty(text!) : null;
  const colMeta = d.data?.columns.find((c: ApiDbColumn): boolean => { return c.name === column; }) || {} as { dataType?: string };
  const isBool = /bool/i.test(colMeta.dataType || "");
  dbCellEdit = { kind: kind, key: key, i: i, column: column, meta: meta };

  // The library's sheet (docs/46 P7): showSheet unhides the host before it paints. The title is
  // the column; the sub is where it lives, a value you would copy. The per-open wiring below
  // stays (the sheet idiom - the buttons and their closure state live only while it is open).
  // The NULL and boolean buttons are worded by paintNull / paintBool.
  showSheet(sheet({
    title: column,
    sub: (d.schema ? d.schema + "." : "") + d.table + tr(kind === "update" ? "dataCell.pkPk" : "dataCell.newRow", { pk: pkJson }),
    label: tr("dataCell.editCell"),
    body: [
      h("div", { class: "db-console-row" },
        isBool ? btn("", { id: "dbCellBool" }) : null,
        btn("", { id: "dbCellNull" }),
        pretty ? btn(tr("dataCell.formatJson"), { id: "dbCellJson" }) : null,
        hint(tr("dataCell.savesLocalBufferCommit"))),
      h("textarea", { id: "dbCellText", spellcheck: false }),
    ],
    foot: [btn(tr("dataCell.cancel"), { id: "dbCellCancel" }), btn(tr("dataCell.saveBuffer"), { kind: "primary", id: "dbCellSave" })],
  }));
  const ta = $<HTMLTextAreaElement>("dbCellText");
  ta.value = pretty || text;
  let nullState = isNull;
  function paintNull(): void {
    // Never disable the textarea: a NULL cell once opened a dead editor ("cannot edit").
    // NULL is a visible overlay state that typing clears on its own.
    ta.disabled = false;
    ta.style.opacity = nullState ? ".5" : "1";
    ta.placeholder = nullState ? tr("dataCell.nullPlaceholder") : "";
    const b = $("dbCellNull");
    b.textContent = nullState ? tr("dataCell.nullSet") : tr("dataCell.setNull");
  }
  // Typing while NULL is set means replacing the NULL — clear the flag.
  ta.addEventListener("input", () => {
    if (nullState) { nullState = false; paintNull(); }
  });
  $("dbCellNull").onclick = () => {
    nullState = !nullState;
    paintNull();
    if (!nullState) ta.focus();
  };
  if (isBool) {
    let boolState = text === "1" || text?.toLowerCase() === "true";
    const bb = $("dbCellBool");
    function paintBool(): void { bb.textContent = boolState ? DB_BOOL_TRUE : DB_BOOL_FALSE; }
    bb.onclick = () => {
      boolState = !boolState;
      nullState = false;
      ta.value = boolState ? "1" : "0";
      paintBool(); paintNull();
    };
    paintBool();
  }
  if (pretty) {
    $("dbCellJson").onclick = () => {
      const p: string | null = dbCellPretty(ta.value);
      if (p != null) { ta.value = p; ta.classList.add("json"); }
      else toast(tr("dataCell.validJsonLeft"), true);
    };
  }
  paintNull();
  $("dbCellCancel").onclick = dbCloseCellEditor;
  $("dbCellSave").onclick = (): void => {
    let v: string | null | undefined;
    if (nullState) v = null;
    else {
      const raw = ta.value;
      // A JSON-shaped text in a JSON-ish column is stored as the string it is; the driver casts.
      v = raw === "" && kind === "update" && emptyWasNotNull ? null : raw;
      if (raw === "" && kind === "insert") v = undefined; // empty insert cell = use column default
    }
    dbSaveCellEdit(v);
  };
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) dbCloseCellEditor(); };
  ta.focus();
  if (!pretty) ta.select();
}

function dbCloseCellEditor(): void {
  closeSheet();
  dbCellEdit = null;
}

/** Write the dialog result into the local buffer and repaint the grid + bar. */
function dbSaveCellEdit(v: string | null | undefined): void {
  const t = dbTab();
  if (t.kind !== "table") return;
  const d = t;
  const ed = dbCellEdit;
  if (!ed || !d.data) return;
  if (ed.kind === "insert") {
    const ins = d.inserts[ed.i]!;   // an insert editor only opens for a row that still exists
    if (v === undefined) delete ins.values[ed.column];
    else ins.values[ed.column] = v;
  } else {
    // An update save always carries its row key and meta (the editor's own invariant).
    if (ed.key == null || !ed.meta) return;
    const e = d.updates[ed.key] || (d.updates[ed.key] = { pk: ed.meta.pk as Record<string, unknown>, changes: {} });
    const backToOriginal = v === ed.meta.orig || (v === null && (ed.meta.orig === null || ed.meta.orig === undefined));
    if (backToOriginal) {
      delete e.changes[ed.column];
      if (!Object.keys(e.changes).length) delete d.updates[ed.key];
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
const DB_CELL_FOLD = 100;

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
  const type = String(colType == null ? "" : colType).toLowerCase();
  if (/binary|blob|bytea/.test(type)) {
    // The adapters decode BLOB bytes as UTF-8 (mysql.rs / pg.rs, mirroring the Node build's
    // Buffers-to-string pass), so the panel never sees the raw bytes — count what it did get,
    // in UTF-8 bytes (exact for ASCII payloads, honest about the delivery for the rest).
    const bytes = new TextEncoder().encode(String(value)).length;
    return { text: tr("dataCell.binaryNBytes", { n: bytes }), cls: "db-fold", title: null, href: null };
  }
  if (/bool/.test(type) || typeof value === "boolean") {
    const on = value === true || value === 1 || String(value).toLowerCase() === "true" || value === "1";
    return { text: on ? "TRUE" : "FALSE", cls: null, title: null, href: null };
  }
  const text = dbCellText(value)!;
  if (/^https?:\/\//i.test(text)) return { text: text, cls: null, title: text, href: text };
  const jsonLike = typeof value === "object" || dbCellJsonLike(text);
  if (jsonLike && text.length > DB_CELL_FOLD) {
    return { text: tr("dataCell.jsonFold"), cls: "db-fold", title: text, href: null };
  }
  const numeric = typeof value === "number" ||
    /int|decimal|numeric|float|double|real|money|year|bit/.test(type);
  return { text: text, cls: numeric ? "db-num" : null, title: null, href: null };
}

export { DB_CELL_FOLD, dbCellEdit, dbCellJsonLike, dbCellPretty, dbCellText, dbCellView, dbCloseCellEditor, dbOpenCellEditor, dbSaveCellEdit };

import type { DbState } from "./db-state.js";
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

import type { ApiDbColumn } from "./types/api.js";
import type { DbBufferedUpdate, DbCellMeta, DbFormField } from "./types/state.js";
import { el, icon } from "./util.js";
import { DB_INLINE_MAX } from "./data-edit.js";
import { dbCellText, dbOpenCellEditor } from "./data-cell.js";
import { dbRowAddr, renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
import { dbPkKey } from "./data-view.js";
import { dbView } from "./db-state.js";

/* --- single-record form view (docs/22 W5.1) ------------------------------------------------------ */
/* dbgate's SqlFormView precedent: one record, one field per line, a stepper for the adjacent
   records. The form is the SAME editor the grid is, worn differently — dbFormRowFields decides
   what each field shows (buffer over original), dbFormWrite writes with the grid's exact
   collapse-back semantics, so the bar's pending count is the two views' shared truth and
   Commit stays the only door to the database. The controls are the W2 vocabulary: the NULL
   button and boolean toggle from the cell dialog, long values opening the edit sheet. */

/** One row's fields as the form paints them: the buffered value where a change is pending,
 *  the original where it is not. An insert's UNSET column is undefined (the column default),
 *  never null — the two mean different things on INSERT. Pure. */
function dbFormRowFields(columns: ApiDbColumn[], row: Record<string, unknown> | null, key: string | null, updates: Record<string, DbBufferedUpdate>, insert: { values: Record<string, unknown> } | null): DbFormField[] {
  const upd = insert ? null : (updates[key as string] || null);
  return (columns || []).map((c: ApiDbColumn): DbFormField => {
    if (insert) {
      const has = Object.prototype.hasOwnProperty.call(insert.values, c.name);
      return { name: c.name, type: c.dataType || "", orig: undefined,
        value: has ? insert.values[c.name] : undefined, pending: has };
    }
    const pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name);
    const orig = row ? row[c.name] : undefined;
    return { name: c.name, type: c.dataType || "", orig: orig,
      value: pending ? upd?.changes[c.name] : orig, pending: pending };
  });
}

/** The form's one write path — the inline editor's semantics, plus the dialog's NULL rule:
 *  a value equal to the original (by identity, or as its text form) collapses the buffered
 *  change and drops the emptied entry; a NULL collapses only against a NULL original. An
 *  insert's empty text is the dialog's "back to the column default". Mutates the given
 *  state object only. */
function dbFormWrite(d: DbState, kind: string, key: string, i: number, column: string, meta: DbCellMeta, v: unknown): void {
  if (kind === "insert") {
    const ins = d.inserts[i];
    if (!ins) return;
    if (v === undefined || v === "") delete ins.values[column];
    else ins.values[column] = v;
    return;
  }
  let e = d.updates[key];
  if (!e) e = d.updates[key] = { pk: meta.pk as Record<string, unknown>, changes: {} };
  const orig = meta.orig;
  const back = v === orig ||
    (v === null && (orig === null || orig === undefined)) ||
    (typeof v === "string" && v === (orig == null ? "" : String(orig)));
  if (back) {
    delete e.changes[column];
    if (!Object.keys(e.changes).length) delete d.updates[key];
  } else e.changes[column] = v;
}

/** One field's value cell: the control vocabulary is W2's — an input for short values, the
 *  boolean toggle and NULL button from the cell dialog, and long text opening the edit
 *  sheet (never an inline overlay: a form row is not a grid cell). */
function dbFormField(d: DbState, val: HTMLElement, f: DbFormField, ctx: { kind: "update" | "insert"; key: string | null; i: number; editable: boolean; pkAddr: Record<string, unknown> }): void {
  const meta = { pk: ctx.pkAddr, orig: f.orig };
  function write(v: unknown): void {
    dbFormWrite(d, ctx.kind, ctx.key as string, ctx.i, f.name, meta, v);
    renderDbGrid();
    renderDbBar();
  }

  if (!ctx.editable) {
    if (f.value === undefined) { val.appendChild(el("span", "db-fold", "default")); return; }
    if (f.value === null) { val.appendChild(el("span", "db-null", "NULL")); return; }
    val.appendChild(el("span", "db-form-text", dbCellText(f.value)!));
    return;
  }

  const isBool = /bool/i.test(f.type);
  if (isBool) {
    const on = f.value === true || f.value === 1 ||
      String(f.value).toLowerCase() === "true" || f.value === "1";
    const bb = el("button", "btn", on ? "TRUE" : "FALSE") as HTMLButtonElement;
    bb.type = "button";
    bb.title = "Toggle this boolean — buffered like any cell edit";
    bb.onclick = () => { write(on ? "0" : "1"); };
    val.appendChild(bb);
  } else {
    const text = f.value === null || f.value === undefined ? null : dbCellText(f.value);
    const long = text != null && (text.length > DB_INLINE_MAX || text.indexOf("\n") >= 0);
    if (long) {
      const box = el("div", "db-form-long");
      box.textContent = text;
      box.title = "Open the editor sheet";
      box.onclick = (): void => { dbOpenCellEditor(ctx.kind, ctx.key as string, ctx.i, f.name, meta); };
      val.appendChild(box);
    } else {
      const inp = el("input", "db-form-input");
      inp.type = "text";
      inp.value = text == null ? "" : text;
      inp.placeholder = f.value === undefined ? "default" : f.value === null ? "NULL — type to replace" : "";
      inp.spellcheck = false;
      inp.onchange = (e) => { write((e.currentTarget as HTMLInputElement).value); };
      inp.onkeydown = (ev) => {
        if (ev.key === "Escape") { const t = ev.currentTarget as HTMLInputElement; t.value = text == null ? "" : text; t.blur(); }
      };
      val.appendChild(inp);
    }
  }

  const nb = el("button", "btn", f.value === null ? "NULL (set)" : "Set NULL") as HTMLButtonElement;
  nb.type = "button";
  nb.title = "Buffer NULL for this column";
  nb.onclick = () => {
    if (f.value !== null) { write(null); return; }
    // Setting NULL off goes back to what was there: the original for an update, the default for an insert.
    if (ctx.kind === "insert") write(undefined);
    else write(meta.orig == null ? "" : typeof meta.orig === "string" ? meta.orig : String(meta.orig));
  };
  val.appendChild(nb);
}

/** The Form tab's body. Same entry guards and row order as the grid (buffered inserts in
 *  front of the page's rows); the chevron stepper in the head walks between them. */
function renderDbFormView(wrap: HTMLElement): void {
  const d = dbView();
  if (!d.conn) {
    wrap.appendChild(el("div", "db-hint", "No database MCP registered — add a mysql or pg MCP first."));
    return;
  }
  if (!d.table || !d.data) {
    wrap.appendChild(el("div", "db-hint", d.table
      ? "Loading " + d.table + "\u2026"
      : "Select a table to see one record as a form."));
    return;
  }
  if (d.loading) { wrap.appendChild(el("div", "db-hint", "Loading\u2026")); return; }
  const nIns = d.inserts.length;
  const total = nIns + d.data?.rows.length;
  if (!total) { wrap.appendChild(el("div", "db-hint", "No rows on this page.")); return; }
  const idx = Math.max(0, Math.min(total - 1, d.formIdx || 0));
  d.formIdx = idx;

  const head = el("div", "db-form-head");
  const prev = el("button", "btn icon") as HTMLButtonElement;
  prev.type = "button";
  prev.innerHTML = icon("chevron-left");
  prev.title = "Previous record";
  prev.setAttribute("aria-label", "Previous record");
  prev.disabled = idx === 0;
  prev.onclick = (): void => { d!.formIdx = idx - 1; renderDbGrid(); };
  const next = el("button", "btn icon") as HTMLButtonElement;
  next.type = "button";
  next.innerHTML = icon("chevron-right");
  next.title = "Next record";
  next.setAttribute("aria-label", "Next record");
  next.disabled = idx === total - 1;
  next.onclick = (): void => { d!.formIdx = idx + 1; renderDbGrid(); };
  const isIns = idx < nIns;
  head.appendChild(prev);
  head.appendChild(el("span", "db-form-pos", isIns
    ? "New row " + (idx + 1) + " of " + nIns + " \u00b7 buffered"
    : "Row " + (idx - nIns + 1) + " of " + d.data?.rows.length + " on this page"));
  head.appendChild(next);

  const pkCols = d.data?.primaryKey || [];
  const columns = d.data?.columns || [];
  const editable = !!d.data?.editable;
  const ins = isIns ? d.inserts[idx] : null;
  const ri = isIns ? -1 : idx - nIns;
  const row = isIns ? null : d.data?.rows[ri];
  const key = isIns ? null : (pkCols.length ? dbPkKey(pkCols, row!) : String(ri));
  const deleted = !isIns && !!d.deletes[key as string];

  // The record's own actions ride the head's right end — the grid rowctl vocabulary.
  if (editable) {
    const act = el("button", "db-act", isIns ? "\u2715" : deleted ? "\u21a9" : "\u2715") as HTMLButtonElement;
    act.type = "button";
    act.title = isIns ? "Remove this buffered insert"
      : deleted ? "Undo this buffered delete" : "Buffer a delete \u2014 applied only on Commit";
    act.onclick = (): void => {
      if (isIns) d.inserts.splice(idx, 1);
      else if (deleted) delete d.deletes[key as string];
      else {
        d.deletes[key as string] = dbRowAddr(pkCols, columns, row!);
        delete d.updates[key as string]; // a deleted row's cell edits are moot
      }
      renderDbGrid();
      renderDbBar();
    };
    head.appendChild(el("span", "grow"));
    head.appendChild(act);
  }
  wrap.appendChild(head);

  const fields = dbFormRowFields(columns, row, key, d.updates, ins);
  const form = el("div", "db-form" + (deleted ? " db-del" : "") + (isIns ? " db-ins" : ""));
  fields.forEach((f: DbFormField): void => {
    const fr = el("div", "db-form-row" + (f.pending ? " db-dirty" : ""));
    const lab = el("div", "db-form-label");
    lab.appendChild(el("div", "db-form-name", f.name));
    if (f.type) lab.appendChild(el("div", "db-col-type", f.type));
    const val = el("div", "db-form-val");
    dbFormField(d!, val, f, {
      kind: isIns ? "insert" : "update",
      key: key,
      i: isIns ? idx : -1,
      editable: editable && (isIns || !deleted),
      pkAddr: isIns ? {} : dbRowAddr(pkCols, columns, row!),
    });
    fr.appendChild(lab);
    fr.appendChild(val);
    form.appendChild(fr);
  });
  wrap.appendChild(form);
}

export { dbFormField, dbFormRowFields, dbFormWrite, renderDbFormView };

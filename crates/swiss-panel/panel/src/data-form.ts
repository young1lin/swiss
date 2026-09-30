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
import type { DbBufferedUpdate, DbCellMeta, DbFormField, DbTableTab } from "./types/state.js";
import { el, iconNode } from "./util.js";
import { h } from "./h.js";
import { DB_INLINE_MAX } from "./data-edit.js";
import { dbCellText, dbOpenCellEditor } from "./data-cell.js";
import { dbRowAddr, renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
import { dbPkKey } from "./data-view.js";
import { dbConn, dbTab } from "./db-state.js";
import { tr } from "./i18n.js";
import { btn, iconBtn } from "./ui/button.js";

/* --- single-record form view (SPEC §data.grid) ------------------------------------------------------ */
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
function dbFormWrite(d: DbTableTab, kind: string, key: string, i: number, column: string, meta: DbCellMeta, v: unknown): void {
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
 *  sheet (never an inline overlay: a form row is not a grid cell).
 *
 *  SPEC §panel.toolchain: the controls carry data-ff/data-col addresses and NO handlers — #pane's
 *  delegated click/change/keydown (dbFormClick and friends) resolve the field's ctx from
 *  LIVE state at event time, so a toggle writes what the record says now, not what the
 *  button was painted with. */
function dbFormField(val: HTMLElement, f: DbFormField, ctx: { kind: "update" | "insert"; key: string | null; i: number; editable: boolean; pkAddr: Record<string, unknown> }): void {
  if (!ctx.editable) {
    if (f.value === undefined) { val.appendChild(el("span", "db-fold", tr("dataForm.phDefault"))); return; }
    if (f.value === null) { val.appendChild(el("span", "db-null", tr("dataGrid.null"))); return; }
    val.appendChild(el("span", "db-form-text", dbCellText(f.value)!));
    return;
  }

  const isBool = /bool/i.test(f.type);
  if (isBool) {
    const on = f.value === true || f.value === 1 ||
      String(f.value).toLowerCase() === "true" || f.value === "1";
    val.appendChild(btn(on ? tr("dataForm.true") : tr("dataForm.false"), {
      title: tr("dataForm.toggleBooleanBufferedLike"), data: { ff: "bool", col: f.name, on: on ? "1" : "" },
    }));
  } else {
    const text = f.value === null || f.value === undefined ? null : dbCellText(f.value);
    const long = text != null && (text.length > DB_INLINE_MAX || text.includes("\n"));
    if (long) {
      val.appendChild(h("div", { class: "db-form-long", title: tr("dataForm.openEditorSheet"), data: { ff: "long", col: f.name } }, text));
    } else {
      val.appendChild(h("input", {
        class: "db-form-input", type: "text", value: text == null ? "" : text,
        placeholder: f.value === undefined ? tr("dataForm.phDefault")
          : f.value === null ? tr("dataForm.phNullReplace") : "",
        spellcheck: false, data: { ff: "input", col: f.name },
      }));
    }
  }

  val.appendChild(btn(f.value === null ? tr("dataForm.nullSet") : tr("dataForm.setNull"), {
    title: tr("dataForm.bufferNullColumn"), data: { ff: "null", col: f.name, has: f.value === null ? "1" : "" },
  }));
}

/** The Form tab's body. Same entry guards and row order as the grid (buffered inserts in
 *  front of the page's rows); the chevron stepper in the head walks between them. */
function renderDbFormView(wrap: HTMLElement): void {
  const c = dbConn();
  const d = dbTab();
  if (d.kind !== "table") return;
  if (!c.conn) {
    wrap.appendChild(el("div", "db-hint", tr("dataGrid.databaseMcpRegisteredAdd")));
    return;
  }
  if (!d.table || !d.data) {
    wrap.appendChild(el("div", "db-hint", d.table
      ? tr("dataBrowsers.loadingK", { k: d.table })
      : tr("dataForm.selectTableRecordForm")));
    return;
  }
  if (d.loading) { wrap.appendChild(el("div", "db-hint", tr("dataGrid.loading"))); return; }
  const nIns = d.inserts.length;
  const total = nIns + d.data?.rows.length;
  if (!total) { wrap.appendChild(el("div", "db-hint", tr("dataForm.noRowsPage"))); return; }
  const idx = Math.max(0, Math.min(total - 1, d.formIdx || 0));
  d.formIdx = idx;

  // The stepper answers through #pane's delegated click via data-fpg (SPEC §panel.toolchain).
  const head = el("div", "db-form-head");
  head.appendChild(iconBtn("chevron-left", tr("dataForm.previousRecord"), { disabled: idx === 0, data: { fpg: "prev" } }));
  const isIns = idx < nIns;

  const pkCols = d.data?.primaryKey || [];
  const columns = d.data?.columns || [];
  const editable = !!d.data?.editable;
  const ins = isIns ? d.inserts[idx] : null;
  const ri = isIns ? -1 : idx - nIns;
  const row = isIns ? null : d.data?.rows[ri];
  const key = isIns ? null : (pkCols.length ? dbPkKey(pkCols, row!) : String(ri));
  const deleted = !isIns && !!d.deletes[key as string];

  head.appendChild(el("span", "db-form-pos", isIns
    ? tr("dataForm.newRowIN", { i: idx + 1, n: nIns })
    : tr("dataForm.rowINPage", { i: idx - nIns + 1, n: d.data?.rows.length ?? 0 })));
  head.appendChild(iconBtn("chevron-right", tr("dataForm.nextRecord"), { disabled: idx === total - 1, data: { fpg: "next" } }));

  // The record's own action rides the head's right end — the grid rowctl vocabulary. The
  // data-fact click re-derives insert/delete from live state (SPEC §panel.toolchain).
  if (editable) {
    head.appendChild(el("span", "grow"));
    head.appendChild(h("button", {
      class: "db-act", type: "button",
      title: isIns ? tr("dataGrid.removeBufferedInsert")
        : deleted ? tr("dataGrid.undoBufferedDelete") : tr("dataGrid.bufferDeleteAppliedOnly"),
      data: { fact: "" },
      // The grid's row-control sprites (SPEC §panel.design); this head still drew the unicode glyphs
      // until SPEC §panel.pages.
    }, iconNode(deleted ? "undo" : "x")));
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
    dbFormField(val, f, {
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

/* --- #pane's delegated listeners for the form (SPEC §panel.toolchain) ----------------------------------------
   Behavior notes (SPEC §panel.toolchain): every control resolves its record and field from LIVE
   state at event time — the boolean toggle's on/off, the NULL button's back-to-original
   value and the stepper's bounds all re-read dbTab(), so a buffered write that lands
   between render and click is what the next click acts on. The old per-render closures
   captured the painted values instead. */

/** The live record context: the same clamp renderDbFormView applies, then the insert/row
 *  split, key, delete state and pk address — recomputed per event. */
function dbFormCtx(): {
  d: DbTableTab; idx: number; isIns: boolean; ins: { values: Record<string, unknown> } | null;
  ri: number; row: Record<string, unknown> | null; key: string | null; deleted: boolean;
  pkCols: string[]; columns: ApiDbColumn[]; editable: boolean; pkAddr: Record<string, unknown>;
} | null {
  const d = dbTab();
  if (d.kind !== "table" || !d.data) return null;
  const nIns = d.inserts.length;
  const total = nIns + d.data.rows.length;
  if (!total) return null;
  const idx = Math.max(0, Math.min(total - 1, d.formIdx || 0));
  const isIns = idx < nIns;
  const pkCols = d.data.primaryKey || [];
  const columns = d.data.columns || [];
  const ri = isIns ? -1 : idx - nIns;
  const row = isIns ? null : d.data.rows[ri];
  const key = isIns ? null : (pkCols.length ? dbPkKey(pkCols, row!) : String(ri));
  return {
    d: d, idx: idx, isIns: isIns, ins: isIns ? d.inserts[idx] : null, ri: ri, row: row,
    key: key, deleted: !isIns && !!d.deletes[key as string], pkCols: pkCols, columns: columns,
    editable: !!d.data.editable, pkAddr: isIns ? {} : dbRowAddr(pkCols, columns, row!),
  };
}

/** The live field behind a data-col address. */
function dbFormFieldAt(ctx: NonNullable<ReturnType<typeof dbFormCtx>>, column: string): DbFormField | null {
  const fields = dbFormRowFields(ctx.columns, ctx.row, ctx.key, ctx.d.updates, ctx.ins);
  return fields.find((f: DbFormField): boolean => { return f.name === column; }) || null;
}

function dbFormWriteField(ctx: NonNullable<ReturnType<typeof dbFormCtx>>, f: DbFormField, v: unknown): void {
  dbFormWrite(ctx.d, ctx.isIns ? "insert" : "update", ctx.key as string, ctx.isIns ? ctx.idx : -1,
    f.name, { pk: ctx.pkAddr, orig: f.orig }, v);
  renderDbGrid();
  renderDbBar();
}

function dbFormClick(t: Element): boolean {
  const ctx = dbFormCtx();
  if (!ctx) return false;
  const d = ctx.d;
  const pg = t.closest<HTMLElement>("[data-fpg]");
  if (pg) {
    // The rendered disabled state already bounds the walk; the live re-check is belt-only.
    if (pg.dataset.fpg === "prev" && ctx.idx > 0) d.formIdx = ctx.idx - 1;
    if (pg.dataset.fpg === "next" && ctx.idx < d.inserts.length + (d.data ? d.data.rows.length : 0) - 1) d.formIdx = ctx.idx + 1;
    renderDbGrid();
    return true;
  }
  if (t.closest("[data-fact]")) {
    if (ctx.isIns) d.inserts.splice(ctx.idx, 1);
    else if (ctx.deleted) delete d.deletes[ctx.key as string];
    else {
      d.deletes[ctx.key as string] = dbRowAddr(ctx.pkCols, ctx.columns, ctx.row!);
      delete d.updates[ctx.key as string]; // a deleted row's cell edits are moot
    }
    renderDbGrid();
    renderDbBar();
    return true;
  }
  const ctl = t.closest<HTMLElement>("[data-ff]");
  if (!ctl) return false;
  const f = dbFormFieldAt(ctx, ctl.dataset.col ?? "");
  if (!f) return true; // the field set changed under the click — nothing to act on
  const kind = ctx.isIns ? "insert" : "update";
  if (ctl.dataset.ff === "bool") {
    const on = f.value === true || f.value === 1 ||
      String(f.value).toLowerCase() === "true" || f.value === "1";
    dbFormWriteField(ctx, f, on ? "0" : "1");
    return true;
  }
  if (ctl.dataset.ff === "long") {
    dbOpenCellEditor(kind, ctx.key, ctx.isIns ? ctx.idx : -1, f.name, { pk: ctx.pkAddr, orig: f.orig });
    return true;
  }
  if (ctl.dataset.ff === "null") {
    if (f.value !== null) { dbFormWriteField(ctx, f, null); return true; }
    // Setting NULL off goes back to what was there: the original for an update, the default for an insert.
    if (ctx.isIns) dbFormWriteField(ctx, f, undefined);
    else dbFormWriteField(ctx, f, f.orig == null ? "" : typeof f.orig === "string" ? f.orig : String(f.orig));
    return true;
  }
  return false;
}

function dbFormChange(t: Element): boolean {
  const ctl = t.closest<HTMLInputElement>('[data-ff="input"]');
  if (!ctl) return false;
  const ctx = dbFormCtx();
  if (!ctx) return true;
  const f = dbFormFieldAt(ctx, ctl.dataset.col ?? "");
  if (f) dbFormWriteField(ctx, f, ctl.value);
  return true;
}

function dbFormKeydown(t: Element, ev: KeyboardEvent): boolean {
  if (ev.key !== "Escape") return false;
  const ctl = t.closest<HTMLInputElement>('[data-ff="input"]');
  if (!ctl) return false;
  const ctx = dbFormCtx();
  if (ctx) {
    const f = dbFormFieldAt(ctx, ctl.dataset.col ?? "");
    const text = !f || f.value === null || f.value === undefined ? "" : (dbCellText(f.value) ?? "");
    ctl.value = text;
  }
  if (ctl.blur) ctl.blur();
  return true;
}

export { dbFormChange, dbFormClick, dbFormCtx, dbFormField, dbFormKeydown, dbFormRowFields, dbFormWrite, renderDbFormView };

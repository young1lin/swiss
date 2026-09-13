import { el, icon, state } from "./util.js";
import { DB_INLINE_MAX } from "./data-edit.js";
import { dbCellText, dbOpenCellEditor } from "./data-cell.js";
import { dbRowAddr, renderDbGrid } from "./data-grid.js";
import { renderDbBar } from "./data-sql.js";
import { dbPkKey } from "./data-view.js";

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
function dbFormRowFields(columns, row, key, updates, insert) {
  var upd = insert ? null : (updates[key] || null);
  return (columns || []).map(function (c) {
    if (insert) {
      var has = Object.prototype.hasOwnProperty.call(insert.values, c.name);
      return { name: c.name, type: c.dataType || "", orig: undefined,
        value: has ? insert.values[c.name] : undefined, pending: has };
    }
    var pending = !!upd && Object.prototype.hasOwnProperty.call(upd.changes, c.name);
    var orig = row ? row[c.name] : undefined;
    return { name: c.name, type: c.dataType || "", orig: orig,
      value: pending ? upd.changes[c.name] : orig, pending: pending };
  });
}

/** The form's one write path — the inline editor's semantics, plus the dialog's NULL rule:
 *  a value equal to the original (by identity, or as its text form) collapses the buffered
 *  change and drops the emptied entry; a NULL collapses only against a NULL original. An
 *  insert's empty text is the dialog's "back to the column default". Mutates the given
 *  state object only. */
function dbFormWrite(d, kind, key, i, column, meta, v) {
  if (kind === "insert") {
    var ins = d.inserts[i];
    if (!ins) return;
    if (v === undefined || v === "") delete ins.values[column];
    else ins.values[column] = v;
    return;
  }
  var e = d.updates[key];
  if (!e) e = d.updates[key] = { pk: meta.pk, changes: {} };
  var orig = meta.orig;
  var back = v === orig ||
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
function dbFormField(d, val, f, ctx) {
  var meta = { pk: ctx.pkAddr, orig: f.orig };
  function write(v) {
    dbFormWrite(d, ctx.kind, ctx.key, ctx.i, f.name, meta, v);
    renderDbGrid();
    renderDbBar();
  }

  if (!ctx.editable) {
    if (f.value === undefined) { val.appendChild(el("span", "db-fold", "default")); return; }
    if (f.value === null) { val.appendChild(el("span", "db-null", "NULL")); return; }
    val.appendChild(el("span", "db-form-text", dbCellText(f.value)));
    return;
  }

  var isBool = /bool/i.test(f.type);
  if (isBool) {
    var on = f.value === true || f.value === 1 ||
      String(f.value).toLowerCase() === "true" || f.value === "1";
    var bb = el("button", "btn", on ? "TRUE" : "FALSE");
    bb.type = "button";
    bb.title = "Toggle this boolean — buffered like any cell edit";
    bb.onclick = function () { write(on ? "0" : "1"); };
    val.appendChild(bb);
  } else {
    var text = f.value === null || f.value === undefined ? null : dbCellText(f.value);
    var long = text != null && (text.length > DB_INLINE_MAX || text.indexOf("\n") >= 0);
    if (long) {
      var box = el("div", "db-form-long");
      box.textContent = text;
      box.title = "Open the editor sheet";
      box.onclick = function () { dbOpenCellEditor(ctx.kind, ctx.key, ctx.i, f.name, meta); };
      val.appendChild(box);
    } else {
      var inp = el("input", "db-form-input");
      inp.type = "text";
      inp.value = text == null ? "" : text;
      inp.placeholder = f.value === undefined ? "default" : f.value === null ? "NULL — type to replace" : "";
      inp.spellcheck = false;
      inp.onchange = function () { write(this.value); };
      inp.onkeydown = function (ev) {
        if (ev.key === "Escape") { this.value = text == null ? "" : text; this.blur(); }
      };
      val.appendChild(inp);
    }
  }

  var nb = el("button", "btn", f.value === null ? "NULL (set)" : "Set NULL");
  nb.type = "button";
  nb.title = "Buffer NULL for this column";
  nb.onclick = function () {
    if (f.value !== null) { write(null); return; }
    // Setting NULL off goes back to what was there: the original for an update, the default for an insert.
    if (ctx.kind === "insert") write(undefined);
    else write(meta.orig == null ? "" : typeof meta.orig === "string" ? meta.orig : String(meta.orig));
  };
  val.appendChild(nb);
}

/** The Form tab's body. Same entry guards and row order as the grid (buffered inserts in
 *  front of the page's rows); the chevron stepper in the head walks between them. */
function renderDbFormView(wrap) {
  var d = state.db;
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
  var nIns = d.inserts.length;
  var total = nIns + d.data.rows.length;
  if (!total) { wrap.appendChild(el("div", "db-hint", "No rows on this page.")); return; }
  var idx = Math.max(0, Math.min(total - 1, d.formIdx || 0));
  d.formIdx = idx;

  var head = el("div", "db-form-head");
  var prev = el("button", "btn icon");
  prev.type = "button";
  prev.innerHTML = icon("chevron-left");
  prev.title = "Previous record";
  prev.setAttribute("aria-label", "Previous record");
  prev.disabled = idx === 0;
  prev.onclick = function () { d.formIdx = idx - 1; renderDbGrid(); };
  var next = el("button", "btn icon");
  next.type = "button";
  next.innerHTML = icon("chevron-right");
  next.title = "Next record";
  next.setAttribute("aria-label", "Next record");
  next.disabled = idx === total - 1;
  next.onclick = function () { d.formIdx = idx + 1; renderDbGrid(); };
  var isIns = idx < nIns;
  head.appendChild(prev);
  head.appendChild(el("span", "db-form-pos", isIns
    ? "New row " + (idx + 1) + " of " + nIns + " \u00b7 buffered"
    : "Row " + (idx - nIns + 1) + " of " + d.data.rows.length + " on this page"));
  head.appendChild(next);

  var pkCols = d.data.primaryKey || [];
  var columns = d.data.columns || [];
  var editable = !!d.data.editable;
  var ins = isIns ? d.inserts[idx] : null;
  var ri = isIns ? -1 : idx - nIns;
  var row = isIns ? null : d.data.rows[ri];
  var key = isIns ? null : (pkCols.length ? dbPkKey(pkCols, row) : String(ri));
  var deleted = !isIns && !!d.deletes[key];

  // The record's own actions ride the head's right end — the grid rowctl vocabulary.
  if (editable) {
    var act = el("button", "db-act", isIns ? "\u2715" : deleted ? "\u21a9" : "\u2715");
    act.type = "button";
    act.title = isIns ? "Remove this buffered insert"
      : deleted ? "Undo this buffered delete" : "Buffer a delete \u2014 applied only on Commit";
    act.onclick = function () {
      if (isIns) d.inserts.splice(idx, 1);
      else if (deleted) delete d.deletes[key];
      else {
        d.deletes[key] = dbRowAddr(pkCols, columns, row);
        delete d.updates[key]; // a deleted row's cell edits are moot
      }
      renderDbGrid();
      renderDbBar();
    };
    head.appendChild(el("span", "grow"));
    head.appendChild(act);
  }
  wrap.appendChild(head);

  var fields = dbFormRowFields(columns, row, key, d.updates, ins);
  var form = el("div", "db-form" + (deleted ? " db-del" : "") + (isIns ? " db-ins" : ""));
  fields.forEach(function (f) {
    var fr = el("div", "db-form-row" + (f.pending ? " db-dirty" : ""));
    var lab = el("div", "db-form-label");
    lab.appendChild(el("div", "db-form-name", f.name));
    if (f.type) lab.appendChild(el("div", "db-col-type", f.type));
    var val = el("div", "db-form-val");
    dbFormField(d, val, f, {
      kind: isIns ? "insert" : "update",
      key: key,
      i: isIns ? idx : -1,
      editable: editable && (isIns || !deleted),
      pkAddr: isIns ? {} : dbRowAddr(pkCols, columns, row),
    });
    fr.appendChild(lab);
    fr.appendChild(val);
    form.appendChild(fr);
  });
  wrap.appendChild(form);
}

export { dbFormField, dbFormRowFields, dbFormWrite, renderDbFormView };

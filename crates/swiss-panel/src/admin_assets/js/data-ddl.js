import { $, api, apiJson, el, esc, icon, toast } from "./util.js";
import { dbHighlightSql } from "./data-filters.js";
import { dbLoadData } from "./data-grid.js";
import { dbLoadTables, dbOpenTable } from "./data-view.js";
import { dbLoadDetail } from "./data-structure.js";

/* ================================================================================================
   docs/22 W4.6 — the minimal DDL set: CREATE TABLE, ADD COLUMN, CREATE INDEX, one sheet per
   op. The SQL preview is fetched live from POST /api/db/:name/ddl-preview and Commit posts
   the SAME (op, payload) to /api/db/:name/ddl, which builds the statements with the very
   function the preview used (swiss-host's build_ddl_create) — what the sheet shows is what
   Commit runs, byte for byte (pgAdmin's msql flow: one SQL producer serves preview and save).
   Identifiers are vetted server-side against the whitelist; refusals arrive as the server's
   own message and are painted in the preview area, not paraphrased.
   ================================================================================================ */

/* Common types first, dialect extras after — the datalist is suggestion, not constraint: the
   type stays a free-text input because real type spells (`numeric(12,4)`, `enum(...)`) are
   open-ended. Pure so the list can be pinned without a DOM. */
var DB_DDL_TYPES_COMMON = [
  "int", "bigint", "varchar(255)", "text", "date", "datetime", "timestamp",
  "decimal(10,2)", "double", "float", "json", "blob",
];
var DB_DDL_TYPES_MYSQL = [
  "tinyint", "smallint", "mediumint", "int unsigned", "char(36)", "varbinary(16)",
  "tinytext", "mediumtext", "longtext", "time", "year", "enum('a','b')",
];
var DB_DDL_TYPES_PG = [
  "smallint", "serial", "character varying(255)", "char(1)", "boolean", "numeric(10,2)",
  "real", "double precision", "uuid", "jsonb", "bytea", "inet", "timestamptz", "interval",
  "int[]",
];

function dbDdlTypeOptions(dialect) {
  return DB_DDL_TYPES_COMMON.concat(dialect === "pg" ? DB_DDL_TYPES_PG : DB_DDL_TYPES_MYSQL);
}

/** The pgAdmin three-bucket diff (utils/__init__.py's collection diff): the form's rows
 *  against the columns the table already has — added (new names), changed (same name,
 *  different facts), removed (old names the form no longer lists). The W4.6 minimal set
 *  commits ONLY the added bucket; the other two are classified anyway so nothing can
 *  silently pretend it ran. Rows without a name are nothing yet. Pure. */
function dbDdlDiffColumns(oldCols, rows) {
  var oldByName = {};
  (oldCols || []).forEach(function (c) { oldByName[c.name] = c; });
  var names = {};
  var added = [];
  var changed = [];
  (rows || []).forEach(function (r) {
    var name = (r.name || "").trim();
    if (!name) return;
    names[name] = true;
    var o = oldByName[name];
    if (!o) { added.push(r); return; }
    var same = (r.type || "").trim() === (o.dataType || "").trim()
      && !!r.nullable === !!o.nullable
      && (r.default || "").trim() === (o.defaultValue == null ? "" : String(o.defaultValue)).trim()
      && (r.comment || "").trim() === (o.comment == null ? "" : String(o.comment)).trim();
    if (!same) changed.push(r);
  });
  var removed = (oldCols || []).filter(function (c) { return !names[c.name]; });
  return { added: added, changed: changed, removed: removed };
}

/** The (op, payload) body both W4.6 endpoints take, from the sheet's form state. Returns null
 *  when there is nothing to preview yet — the sheet shows its quiet hint instead of calling
 *  the server. Only rows that carry a name ride along; a half-filled row still goes, so the
 *  server's refusal (shown verbatim) names what is missing. Pure over the form object. */
function dbDdlFormPayload(kind, form) {
  var col = function (r) {
    var o = { name: r.name, type: r.type, nullable: !!r.nullable };
    if (r.default) o.default = r.default;
    if (r.comment) o.comment = r.comment;
    return o;
  };
  var base = {};
  if (form.schema) base.schema = form.schema;
  if (kind === "index") {
    var cols = (form.indexColumns || []).filter(Boolean);
    if (!form.table || !form.index || !cols.length) return null;
    base.table = form.table;
    base.index = form.index;
    base.columns = cols;
    if (form.unique) base.unique = true;
    return base;
  }
  if (!form.table) return null;
  base.table = form.table;
  var rows = (form.columns || []).filter(function (r) { return (r.name || "").trim(); });
  if (kind === "table") {
    if (!rows.length) return null;
    base.columns = rows.map(col);
    if (form.comment) base.comment = form.comment;
    return base;
  }
  // add_column: the added bucket of the diff IS the commit; changed/removed stay out of the
  // minimal set (docs/22 §9) and are only classified, never run.
  var added = dbDdlDiffColumns(form.oldColumns || [], rows).added;
  if (!added.length) return null;
  base.columns = added.map(col);
  return base;
}

/** A conventional index name (adminer's spelling): table, the column run, _idx — lower-cased
 *  and capped under the identifier whitelist's 64, so the suggestion itself always passes it.
 *  Pure. */
function dbDdlIndexSuggestion(table, cols) {
  var run = cols.length ? "_" + cols.join("_") : "";
  return (String(table) + run + "_idx").toLowerCase().slice(0, 63);
}

/* --- the sheet ---------------------------------------------------------------------------------- */

var DDL_OP = { table: "create_table", column: "add_column", index: "create_index" };
var DDL_TITLE = { table: "New table", column: "Add column", index: "New index" };
var DDL_QUIET = {
  table: "Name the table and at least one column to see the SQL.",
  column: "Add a column row below to see the SQL.",
  index: "Name the index and pick at least one column to see the SQL.",
};

/** One sheet's whole mutable context; rebuilt per open, nulled on close. */
var S = null;

/** Open one W4.6 sheet. ctx = { kind, dialect, conn, schema, schemas, table, columns } —
 *  kind "table" creates fresh (schema select for pg), "column"/"index" prefill the table's
 *  old state (docs/22 W4.6: same form, prefilled, commit diffs). */
function openDbDdlSheet(kind, ctx) {
  closeDbDdlSheet();
  var rows = [];
  if (kind === "column") {
    rows = (ctx.columns || []).map(function (c) {
      return {
        name: c.name, type: c.dataType, nullable: c.nullable,
        default: c.defaultValue == null ? "" : String(c.defaultValue),
        comment: c.comment == null ? "" : String(c.comment),
        isNew: false,
      };
    });
    rows.push({ name: "", type: "", nullable: true, default: "", comment: "", isNew: true });
  } else {
    rows = [{ name: "", type: "", nullable: true, default: "", comment: "", isNew: true }];
  }
  S = {
    kind: kind, dialect: ctx.dialect, conn: ctx.conn,
    schema: ctx.schema || "", schemas: ctx.schemas || [],
    table: ctx.table || "", oldColumns: ctx.columns || [],
    rows: rows,
    indexName: kind === "index" ? dbDdlIndexSuggestion(ctx.table || "t", []) : "",
    indexCols: [], unique: false, comment: "",
    lastSql: null, lastPayload: null, seq: 0, timer: null,
  };
  paintDbDdlSheet();
  void refreshDbDdlPreview();
}

function closeDbDdlSheet() {
  if (S && S.timer) clearTimeout(S.timer);
  S = null;
  var host = $("sheet");
  if (host) { host.hidden = true; host.innerHTML = ""; }
}

/** Rebuild the whole sheet DOM from S. Called on open and on row add/remove only — keystrokes
 *  write straight into S (and schedule a preview) so an input never loses focus to a re-render. */
function paintDbDdlSheet() {
  var kind = S.kind;
  var title = DDL_TITLE[kind] + (S.table ? " in " + S.table : "");
  var html =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + esc(DDL_TITLE[kind]) + '">' +
      '<div class="sheet-head"><h2 id="ddl-title">' + esc(title) + "</h2></div>" +
      '<div class="sheet-body">';
  if (kind === "table" && S.dialect === "pg") {
    // docs/22 W1.1: a Postgres catalog is many schemas, so the new table says where it goes
    // (swiss-ui-design rule 6) — a select, prefilled from the list's active schema filter.
    var schemas = S.schemas.slice();
    if (schemas.indexOf(S.schema) < 0) schemas.unshift(S.schema);
    html += '<label class="field"><span>Schema</span><select id="ddl-schema">' +
      schemas.map(function (s) {
        return '<option value="' + esc(s) + '"' + (s === S.schema ? " selected" : "") + ">" + esc(s) + "</option>";
      }).join("") + "</select></label>";
  }
  if (kind === "table") {
    html += '<div class="two">' +
        '<label class="field"><span>Table name</span><input id="ddl-table" autocomplete="off" spellcheck="false" placeholder="events"></label>' +
        '<label class="field"><span>Comment (optional)</span><input id="ddl-comment" autocomplete="off" placeholder="' + esc("what this table holds") + '"></label>' +
      "</div>";
  }
  if (kind === "index") {
    html += '<div class="two">' +
        '<label class="field"><span>Index name</span><input id="ddl-index" autocomplete="off" spellcheck="false" value="' + esc(S.indexName) + '"></label>' +
        '<label class="check" style="align-self:end;padding-bottom:6px"><input type="checkbox" id="ddl-unique">Unique</label>' +
      "</div>" +
      '<div class="field"><span>Columns</span><div id="ddl-cols" class="db-ddl-colpick"></div></div>';
  }
  if (kind !== "index") {
    html += '<div class="field"><span>Columns</span>' +
      '<table class="db-ddl-grid" id="ddl-grid"><colgroup>' +
        '<col style="width:22%"><col style="width:24%"><col style="width:56px"><col style="width:20%"><col style="width:28%"><col style="width:26px">' +
      "</colgroup><thead><tr>" +
        ["Name", "Type", "Null", "Default", "Comment", ""].map(function (h) {
          return "<th>" + esc(h) + "</th>";
        }).join("") +
      "</tr></thead><tbody></tbody></table>" +
      '<button class="btn ghost" id="ddl-add-row" type="button">Add column</button>' +
      '<datalist id="ddl-types">' + dbDdlTypeOptions(S.dialect).map(function (t) {
        return '<option value="' + esc(t) + '"></option>';
      }).join("") + "</datalist></div>";
  }
  html +=
        '<div class="hint" id="ddl-hint">SQL preview — Commit runs these statements verbatim.</div>' +
        '<pre class="db-ddl" id="ddl-pre"></pre>' +
      "</div>" +
      '<div class="sheet-foot"><span class="grow"></span>' +
        '<button class="btn" id="ddl-cancel" type="button">Cancel</button>' +
        '<button class="btn primary" id="ddl-commit" type="button" disabled>' +
          esc(kind === "table" ? "Create table" : kind === "column" ? "Add column" : "Create index") +
        "</button></div>" +
    "</div>";
  var host = $("sheet");
  host.innerHTML = html;
  host.hidden = false;
  wireDbDdlSheet();
  // The mini-grid belongs to the table and column kinds only — the index sheet has no
  // #ddl-grid, and rendering rows into it would throw before the picker and the quiet
  // hint ever paint (caught live: the index sheet opened blank).
  if (kind !== "index") renderDbDdlRows();
  if (kind === "index") renderDbDdlColPick();
  paintDbDdlPreviewQuiet();
}

/* Every input writes into S directly — S is the model; the DOM is a view of it. */
function wireDbDdlSheet() {
  var kind = S.kind;
  $("ddl-cancel").onclick = closeDbDdlSheet;
  $("sheet").onclick = function (e) { if (e.target === $("sheet")) closeDbDdlSheet(); };
  // Enter submits the primary from any text input, the way every one-input sheet does.
  $("sheet").onkeydown = function (e) {
    if (e.key === "Enter" && e.target && e.target.tagName === "INPUT" && e.target.type !== "checkbox") {
      e.preventDefault();
      void commitDbDdl();
    }
  };
  var retitle = function () {
    if (S.dialect === "pg" && kind === "table") {
      $("ddl-title").textContent = "New table in " + $("ddl-schema").value;
    }
  };
  if (kind === "table") {
    if (S.dialect === "pg") {
      $("ddl-schema").onchange = function () {
        S.schema = this.value;
        retitle();
        scheduleDbDdlPreview();
      };
      retitle(); // say where the table goes from the first paint, not only after a change
    }
    // The name and comment wire for EVERY dialect — mysql tables need the model write just
    // as much as pg ones (caught live: the handlers once sat in the pg-only branch and a
    // mysql sheet never left its quiet hint).
    $("ddl-table").oninput = function () { S.table = this.value; scheduleDbDdlPreview(); };
    $("ddl-table").value = S.table;
    $("ddl-comment").oninput = function () { S.comment = this.value; scheduleDbDdlPreview(); };
    $("ddl-table").focus();
  } else if (kind === "index") {
    // the name input's own handler lives with the column picker it tracks (below)
    $("ddl-unique").onchange = function () { S.unique = this.checked; scheduleDbDdlPreview(); };
    $("ddl-index").focus();
    $("ddl-index").select();
  }
  // Both grid kinds can grow a row — table and column alike (the wiring once sat in an
  // else branch the table kind never reached, caught live on 19998).
  if (kind !== "index") {
    $("ddl-add-row").onclick = function () {
      S.rows.push({ name: "", type: "", nullable: true, default: "", comment: "", isNew: true });
      renderDbDdlRows();
      var last = $("ddl-grid").querySelectorAll("tbody tr:last-child input[data-k=name]")[0];
      if (last) last.focus();
    };
  }
  $("ddl-commit").onclick = function () { void commitDbDdl(); };
}

/** The mini-grid's rows. Old (prefilled) rows render disabled — only ADD is in the W4.6
 *  minimal set — so the diff's added bucket is exactly the editable rows. */
function renderDbDdlRows() {
  var body = $("ddl-grid").querySelector("tbody");
  body.innerHTML = "";
  S.rows.forEach(function (r, i) {
    var tr = el("tr");
    var mk = function (k, value, ph) {
      var td = el("td");
      var input = document.createElement("input");
      input.type = "text";
      input.value = value == null ? "" : value;
      input.dataset.k = k;
      input.dataset.i = String(i);
      if (ph) input.placeholder = ph;
      if (k === "type") input.setAttribute("list", "ddl-types");
      if (!r.isNew) input.disabled = true;
      input.oninput = function () {
        r[k] = this.value;
        scheduleDbDdlPreview();
      };
      td.appendChild(input);
      return td;
    };
    tr.appendChild(mk("name", r.name, "id"));
    tr.appendChild(mk("type", r.type, "int"));
    var tdn = el("td");
    var cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = !!r.nullable;
    cb.dataset.i = String(i);
    cb.setAttribute("aria-label", "Nullable");
    if (!r.isNew) cb.disabled = true;
    cb.onchange = function () { r.nullable = this.checked; scheduleDbDdlPreview(); };
    tdn.appendChild(cb);
    tr.appendChild(tdn);
    tr.appendChild(mk("default", r.default, "0"));
    tr.appendChild(mk("comment", r.comment, ""));
    var tdx = el("td");
    var rm = el("button", "btn icon");
    rm.type = "button";
    rm.innerHTML = icon("x", "Remove column");
    rm.dataset.i = String(i);
    if (!r.isNew) { rm.disabled = true; rm.title = "Only new columns can be removed here"; }
    rm.onclick = function () {
      S.rows.splice(i, 1);
      renderDbDdlRows();
      scheduleDbDdlPreview();
    };
    tdx.appendChild(rm);
    tr.appendChild(tdx);
    body.appendChild(tr);
  });
}

/** The index sheet's column picker: one check per existing column, in the table's order. */
function renderDbDdlColPick() {
  var box = $("ddl-cols");
  box.innerHTML = "";
  S.oldColumns.forEach(function (c) {
    var label = document.createElement("label");
    label.className = "check";
    var cb = document.createElement("input");
    cb.type = "checkbox";
    cb.value = c.name;
    cb.onchange = function () {
      var at = S.indexCols.indexOf(c.name);
      if (this.checked && at < 0) S.indexCols.push(c.name);
      if (!this.checked && at >= 0) S.indexCols.splice(at, 1);
      // The suggestion follows the picks until the user edits it by hand.
      if (!S.indexTouched) {
        S.indexName = dbDdlIndexSuggestion(S.table, S.indexCols);
        var name = $("ddl-index");
        if (name) name.value = S.indexName;
      }
      scheduleDbDdlPreview();
    };
    label.appendChild(cb);
    label.appendChild(document.createTextNode(c.name));
    box.appendChild(label);
  });
  var nameInput = $("ddl-index");
  if (nameInput) {
    nameInput.oninput = function () { S.indexName = this.value; S.indexTouched = true; scheduleDbDdlPreview(); };
  }
}

/* --- the preview: live, server-built, error text verbatim ---------------------------------------- */

function paintDbDdlPreviewQuiet() {
  var pre = $("ddl-pre");
  if (!pre) return;
  pre.textContent = DDL_QUIET[S.kind];
  pre.classList.add("db-ddl-quiet");
  $("ddl-commit").disabled = true;
  S.lastSql = null;
  S.lastPayload = null;
}

function paintDbDdlPreviewSql(sql) {
  var pre = $("ddl-pre");
  pre.classList.remove("db-ddl-quiet");
  pre.innerHTML = dbHighlightSql(sql);
  $("ddl-commit").disabled = false;
}

function paintDbDdlPreviewError(message) {
  var pre = $("ddl-pre");
  pre.classList.remove("db-ddl-quiet");
  // The server's own refusal, unpainted: it names the field and the rule (kept verbatim from the error text).
  pre.textContent = message;
  $("ddl-commit").disabled = true;
  S.lastSql = null;
  S.lastPayload = null;
}

function scheduleDbDdlPreview() {
  if (!S) return;
  clearTimeout(S.timer);
  S.timer = setTimeout(function () { void refreshDbDdlPreview(); }, 250);
}

/** Read the form, ask the server for the statements, paint them. The payload read here is
 *  the very object Commit posts — one builder, one input, preview = commit. */
async function refreshDbDdlPreview() {
  // Escape closes the sheet through main.js's own closeSheet(), which this module cannot
  // see — a still-pending timer then finds its DOM gone. The guard turns that into a no-op.
  if (!S || !$("ddl-pre")) return;
  var form = {
    schema: S.schema, table: S.table, comment: S.comment,
    columns: S.rows, oldColumns: S.oldColumns,
    index: S.indexName, indexColumns: S.indexCols, unique: S.unique,
  };
  var payload = dbDdlFormPayload(S.kind, form);
  if (!payload) { paintDbDdlPreviewQuiet(); return; }
  var seq = ++S.seq;
  var r;
  try {
    r = await api("/api/db/" + encodeURIComponent(S.conn) + "/ddl-preview", {
      method: "POST",
      body: JSON.stringify({ op: DDL_OP[S.kind], payload: payload }),
    });
  } catch (e) {
    if (S && seq === S.seq) paintDbDdlPreviewError("request failed — is the gateway running?");
    return;
  }
  if (!S || seq !== S.seq) return; // a newer keystroke already superseded this answer
  var j = await r.json().catch(function () { return {}; });
  if (!r.ok) {
    paintDbDdlPreviewError(j.error || "HTTP " + r.status);
    return;
  }
  S.lastSql = j.sql || "";
  S.lastPayload = payload;
  if (!S.lastSql) { paintDbDdlPreviewQuiet(); return; }
  paintDbDdlPreviewSql(S.lastSql);
}

/** Commit posts the previewed (op, payload) to /ddl. Refreshed first so the text on screen
 *  and the payload posted can never be two different drafts (the 250 ms debounce window). */
async function commitDbDdl() {
  if (!S) return;
  await refreshDbDdlPreview();
  if (!S || !S.lastSql || !S.lastPayload) return;
  var btn = $("ddl-commit");
  btn.disabled = true;
  var j = await apiJson("/api/db/" + encodeURIComponent(S.conn) + "/ddl", {
    method: "POST",
    body: JSON.stringify({ op: DDL_OP[S.kind], payload: S.lastPayload }),
  });
  if (!S) return; // the sheet closed while the request was in flight
  if (!j) { btn.disabled = false; return; } // apiJson already toasted the refusal
  var kind = S.kind;
  var table = S.lastPayload.table;
  var schema = S.lastPayload.schema || "";
  closeDbDdlSheet();
  if (kind === "table") {
    toast("Table " + table + " created");
    await dbLoadTables();
    dbOpenTable({ name: table, schema: schema, type: "table" });
  } else if (kind === "column") {
    toast("Column added to " + table);
    dbLoadDetail();
    dbLoadData(true);
  } else {
    toast("Index created on " + table);
    dbLoadDetail();
  }
}

export {
  DB_DDL_TYPES_COMMON, DB_DDL_TYPES_MYSQL, DB_DDL_TYPES_PG,
  closeDbDdlSheet, dbDdlDiffColumns, dbDdlFormPayload, dbDdlIndexSuggestion,
  dbDdlTypeOptions, openDbDdlSheet,
};
